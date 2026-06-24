// The Zustand store (S2) — the 5-slice composition + versioned persist. Grows
// the S0 selection skeleton into the dispatch-state spine: nav (entity /
// per-entity viewMap / sel / prNode-split / history) + ui (overlays / codeAnchor)
// + settings (theme/style/density + experiment gates) + the S1 keymap slice. The
// S0 `selectedModel`/`theme` fields are SUBSUMED — selection now lives in
// `sel.models`, theme in `settings.theme`.
//
// Persistence (discovery risk #6): ONE namespace (`cute-dbt:review`), the EXACT
// prototype key NAMES as the persisted blob's fields (`entity` · `viewByEntity` ·
// `sel` · `settings` · `sidebar` · `keymapOverride`), a versioned `migrate()`
// that MERGES defaults (a new field appears for existing users — never a
// wholesale replace), and FAIL-CLOSED load (any exception → pristine defaults).
// Immer powers the nested-map slices.
import { create } from "zustand";
import { persist, createJSONStorage, type PersistStorage, type StorageValue } from "zustand/middleware";
import { createDataSlice, DATA_DEFAULTS, isDataSource, type DataSlice, type DataSource } from "./data-slice";
import { createKeymapSlice, type KeymapSlice } from "./keymap-slice";
import { createNavSlice, NAV_DEFAULTS, type NavSlice } from "./nav-slice";
import { createUiSlice, UI_DEFAULTS, type UiSlice } from "./ui-slice";
import {
  createSettingsSlice,
  mergeSettings,
  SETTINGS_DEFAULTS,
  type SettingsSlice,
  type Settings,
} from "./settings-slice";
import { createReviewSlice, REVIEW_DEFAULTS, sanitizeReviewState, type ReviewSlice } from "./review-slice";
import { DENY_REBIND_KEYS, type Keymap } from "../domain/keymap";
import type { Entity } from "../domain/keymap";
import type { View } from "../domain/matrix";
import type { ReviewState } from "../domain/review/review-machine";

/**
 * The I/O-boundary clock the review slice's publish stamps the checkpoint with.
 * Threaded in here (the store IS the I/O boundary) so the review DOMAIN never
 * calls `Date.now()` itself — the golden-determinism rule. Overridable for tests.
 */
export const nowIso = (): string => new Date().toISOString();

/** The persist namespace + version. Bump the version whenever a `migrate` is needed. */
export const PERSIST_KEY = "cute-dbt:review";
export const PERSIST_VERSION = 2;

/** The full app state — the 7-slice union (nav · ui · settings · keymap · data · review). */
export type AppState = NavSlice & UiSlice & SettingsSlice & KeymapSlice & DataSlice & ReviewSlice;

/**
 * Sanitize a persisted keymap override on hydration (unchanged from S1): drop any
 * binding onto a reserved/fixed token (the `DENY_REBIND_KEYS` guard `rebindAction`
 * enforces at write time). A stale or hand-edited blob can't reintroduce a
 * reserved binding the live path refuses. Non-object input → empty (fail-closed).
 */
export function sanitizeKeymapOverride(raw: unknown): Keymap {
  if (!raw || typeof raw !== "object") return {};
  const out: Keymap = {};
  for (const [id, token] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof token !== "string") continue;
    if (DENY_REBIND_KEYS.has(token)) continue;
    out[id] = token;
  }
  return out;
}

/**
 * The PERSISTED SHAPE — the prototype's exact key NAMES as fields. We persist
 * only the durable nav/ui/settings/keymap state (not history, not transient
 * overlays). `viewByEntity` keeps the prototype's localStorage key name.
 */
interface PersistedShape {
  entity: Entity;
  viewByEntity: Record<Entity, View>;
  sel: Record<Entity, string | null>;
  sidebar: boolean;
  settings: Settings;
  keymapOverride: Keymap;
  /** the active context source — the durable replacement for the module global. */
  activeSource: DataSource;
  /** the review-flow state (reviewed/pending/published/resolved/checkpoint) — V1. */
  review: ReviewState;
}

/** What we write out (`partialize`) — the durable subset, under the prototype's key names. */
function partialize(s: AppState): PersistedShape {
  return {
    entity: s.entity,
    viewByEntity: s.viewMap,
    sel: s.sel,
    sidebar: s.overlays.sidebar,
    settings: s.settings,
    keymapOverride: s.keymapOverride,
    activeSource: s.activeSource,
    review: s.review,
  };
}

/**
 * The fail-closed, migrate-MERGE hydration. Every field MERGES over its slice
 * defaults (a new default field appears for an existing user; persisted values
 * win where present). Any malformed sub-blob degrades to its default in place —
 * a corrupt/partial persisted state can never crash hydration or drop unrelated
 * settings (the wholesale-replace anti-pattern risk #6 names). Returns a partial
 * the persist `merge` lays over the live `current` state.
 */
export function hydrateMerge(persisted: unknown): Partial<AppState> {
  if (!persisted || typeof persisted !== "object") return {};
  const p = persisted as Partial<PersistedShape>;
  const out: Partial<AppState> = {};

  if (typeof p.entity === "string") out.entity = p.entity as Entity;
  // viewByEntity → viewMap, merged over the nav defaults so a new entity key
  // (or a missing one in an old blob) falls back to its default view.
  out.viewMap =
    p.viewByEntity && typeof p.viewByEntity === "object"
      ? { ...NAV_DEFAULTS.viewMap, ...p.viewByEntity }
      : { ...NAV_DEFAULTS.viewMap };
  out.sel =
    p.sel && typeof p.sel === "object"
      ? { ...NAV_DEFAULTS.sel, ...p.sel }
      : { ...NAV_DEFAULTS.sel };
  // settings: the migrate-MERGE — defaults + persisted, so a NEW settings field
  // appears for an existing user at its default (never dropped, never wholesale).
  out.settings = mergeSettings(p.settings);
  // the sidebar panel flag merges into the overlays defaults.
  out.overlays = {
    ...UI_DEFAULTS.overlays,
    sidebar: typeof p.sidebar === "boolean" ? p.sidebar : UI_DEFAULTS.overlays.sidebar,
  };
  // keymap override: sanitized through the deny-list (reserved tokens can't survive).
  out.keymapOverride = sanitizeKeymapOverride(p.keymapOverride);
  // active source: fail-closed to the default if the persisted value isn't a
  // known source (a renamed/removed fixture in an old blob degrades gracefully).
  out.activeSource = isDataSource(p.activeSource) ? p.activeSource : DATA_DEFAULTS.activeSource;
  // review-flow state: sanitized fail-closed (a corrupt/partial blob → a clean
  // empty review state; each sub-field degrades in place; maps rebuilt null-proto).
  out.review = sanitizeReviewState(p.review);

  return out;
}

/**
 * Map an OLDER persisted blob into the CURRENT persisted shape (`PersistedShape`)
 * before `hydrateMerge` runs. Zustand's `migrate` is called only when the stored
 * `version` is below `PERSIST_VERSION`.
 *
 * The single real prior on-disk shape under the `cute-dbt:review` namespace is
 * **v1** — the S0/S1 store persisted `{ selectedModel, theme, keymapOverride }`.
 * In S2 the selection moved to `sel.models` and the theme to `settings.theme`,
 * and `hydrateMerge` reads only the new keys; a v1 blob handed through unchanged
 * would silently lose `selectedModel` and `theme`. This maps both into the v2
 * shape (fail-closed per field — a wrong-typed legacy value degrades to its
 * default via the same `hydrateMerge`/`mergeSettings` validation downstream).
 * `keymapOverride` carries forward unchanged (it kept the same key + meaning).
 *
 * Any version ≥ 2 (or a non-object blob) passes straight through — `hydrateMerge`
 * owns the same-shape merge-over-defaults.
 */
export function migratePersisted(persisted: unknown, version: number): Partial<AppState> {
  if (version < 2 && persisted && typeof persisted === "object") {
    const p = persisted as Record<string, unknown>;
    const migrated: PersistedShape = {
      // v1 had no per-entity nav state — start from the nav defaults, then carry
      // the old single `selectedModel` into the models slot.
      entity: NAV_DEFAULTS.entity,
      viewByEntity: { ...NAV_DEFAULTS.viewMap },
      sel: {
        ...NAV_DEFAULTS.sel,
        models: typeof p.selectedModel === "string" ? p.selectedModel : NAV_DEFAULTS.sel.models,
      },
      sidebar: UI_DEFAULTS.overlays.sidebar,
      // theme moved from the top level into `settings.theme`; mergeSettings
      // backfills every other settings field at its default.
      settings: typeof p.theme === "string" ? mergeSettings({ theme: p.theme }) : { ...SETTINGS_DEFAULTS },
      // keymapOverride kept the same key/meaning across v1→v2 (sanitized in hydrate).
      keymapOverride: sanitizeKeymapOverride(p.keymapOverride),
      // activeSource is new in S3b — a v1 blob predates it; start at the default.
      activeSource: DATA_DEFAULTS.activeSource,
      // review is new in V1 — a v1 blob predates the review flow; start empty.
      review: REVIEW_DEFAULTS.review,
    };
    return migrated as unknown as Partial<AppState>;
  }
  return persisted as Partial<AppState>;
}

/**
 * A FAIL-CLOSED storage wrapper. If `JSON.parse` (or any read) throws on a
 * corrupt blob, hydration silently falls back to defaults rather than crashing
 * the app — the load-half of the risk-#6 contract (the `merge`/`migrate` hooks
 * only run on a SUCCESSFULLY-parsed value, so the parse itself must be guarded).
 */
function failClosedStorage(): PersistStorage<Partial<AppState>> | undefined {
  const inner = createJSONStorage<Partial<AppState>>(() => localStorage);
  if (!inner) return undefined;
  // Wrap explicitly (don't spread — the inner methods must stay bound to `inner`).
  return {
    getItem: (name): StorageValue<Partial<AppState>> | null => {
      try {
        const v = inner.getItem(name);
        // localStorage is synchronous; if a backend ever returns a Promise we
        // can't fail-closed around its rejection here, so treat it as defaults.
        return v instanceof Promise ? null : v;
      } catch {
        return null; // corrupt/unreadable blob → defaults (fail-closed)
      }
    },
    setItem: (name, value) => {
      try {
        inner.setItem(name, value);
      } catch {
        /* persistence is best-effort — a read-only/absent backend never crashes the app */
      }
    },
    removeItem: (name) => {
      try {
        inner.removeItem(name);
      } catch {
        /* same: removal is best-effort */
      }
    },
  };
}

export const useAppStore = create<AppState>()(
  persist(
    (set, get) => ({
      ...createNavSlice(
        (partial) => set(partial as AppState | Partial<AppState> | ((s: AppState) => AppState | Partial<AppState>)),
        get as () => NavSlice,
      ),
      ...createUiSlice(
        (partial) => set(partial as AppState | Partial<AppState> | ((s: AppState) => AppState | Partial<AppState>)),
        get as () => UiSlice,
      ),
      ...createSettingsSlice((partial) =>
        set(partial as Partial<AppState> | ((s: AppState) => Partial<AppState>)),
      ),
      ...createKeymapSlice((partial) =>
        set(partial as Partial<AppState> | ((s: AppState) => Partial<AppState>)),
      ),
      ...createDataSlice((partial) =>
        set(partial as Partial<AppState> | ((s: AppState) => Partial<AppState>)),
      ),
      ...createReviewSlice(
        (partial) => set(partial as Partial<AppState> | ((s: AppState) => Partial<AppState>)),
        get as () => ReviewSlice,
        nowIso,
      ),
    }),
    {
      name: PERSIST_KEY,
      version: PERSIST_VERSION,
      storage: failClosedStorage(),
      partialize: (s) => partialize(s) as unknown as Partial<AppState>,
      // migrate runs when the persisted version < PERSIST_VERSION. A v2+ blob is
      // already in the current persisted shape (`entity`/`viewByEntity`/`sel`/
      // `settings`/`sidebar`/`keymapOverride`) and routes through hydrateMerge
      // (below, in `merge`) unchanged — the merge-over-defaults IS the additive
      // migration for same-shape blobs.
      //
      // The ONE real prior on-disk shape under this namespace is v1 (the S0/S1
      // store: `{ selectedModel, theme, keymapOverride }`). Those keys moved in
      // S2 — selection → `sel.models`, theme → `settings.theme` — and hydrateMerge
      // reads only the NEW keys, so a v1 blob passed straight through would
      // SILENTLY drop the user's `selectedModel` and `theme`. Map the v1 shape
      // into the current partialized shape here so an upgrading user keeps both.
      migrate: (persisted, version) => migratePersisted(persisted, version),
      // fail-closed merge: defaults win for anything the persisted blob lacks,
      // each sub-blob merged over its slice defaults (new fields appear).
      merge: (persisted, current) => ({ ...current, ...hydrateMerge(persisted) }),
    },
  ),
);

// Re-export the defaults for tests + the chrome.
export { SETTINGS_DEFAULTS, NAV_DEFAULTS, UI_DEFAULTS, REVIEW_DEFAULTS };
