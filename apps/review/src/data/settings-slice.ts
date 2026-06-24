// The settings Zustand slice (S2) — the app-wide presentation + behavior knobs
// the prototype kept in its `settings` object, applied to the document root as
// `data-*` attributes (theme / style / accent / density) so CSS drives the
// look. S2 owns the SHELL knobs (theme/style/accent/density + the experiment
// gates); the per-feature knobs (diffEngine, contextLines, …) ride along as the
// same MERGE-over-defaults blob so later slices read them without a new persist
// key.
//
// LAYER: data (may import domain; never view/chrome).
//
// migrate-MERGE (load-bearing — discovery risk #6): settings MERGE over the
// defaults via a versioned `migrate()`. A new field added in a new app version
// MUST appear (at its default) for an EXISTING user whose persisted blob predates
// it — never a wholesale replace that would drop their other settings. This is
// the `Object.assign({...defaults}, load("settings", {}))` rule from the
// prototype, hardened with a version + fail-closed load.

/** The persisted settings blob. New fields appended here appear for old users via merge. */
export interface Settings {
  /** color theme name (drives the Shiki theme + `data-theme`). */
  theme: string;
  /** style pack (`data-style`). */
  style: string;
  /** accent ("theme" = follow the theme; else `data-accent`). */
  accent: string;
  /** density (`data-density`: compact · cozy · roomy). */
  density: string;
  /** diff layout preference (auto · unified · split). */
  diffLayout: string;
  /** experiment gate — show the coverage view. */
  coverage: boolean;
  /** experiment gate — show the Else (Project) entity. */
  project: boolean;
  /** experiment gate — show the PR DAG. */
  prdag: boolean;
  /** diff fold expand step (lines). */
  expandStep: number;
  /** unified-diff context lines. */
  contextLines: number;
  /** diff engine ("hand-rolled" | "pierre"). */
  diffEngine: string;
  /** active data source id. */
  dataSource: string;
}

/** The settings defaults — exact prototype seeds. New fields ALWAYS get a default here. */
export const SETTINGS_DEFAULTS: Settings = {
  theme: "tokyo",
  style: "paper",
  accent: "theme",
  density: "cozy",
  diffLayout: "auto",
  coverage: true,
  project: true,
  prdag: true,
  expandStep: 20,
  contextLines: 3,
  diffEngine: "hand-rolled",
  dataSource: "pr440",
};

/**
 * Merge a persisted settings blob over the defaults — the migrate-MERGE rule,
 * with PER-FIELD type validation (fail-closed at the field grain). Every default
 * field is present even if the persisted blob omits it (a new field appears for
 * existing users); a persisted value wins ONLY when its runtime type matches the
 * default's type — otherwise that one field falls back to its default while the
 * rest of the valid blob still applies. Without this, a hand-edited / stale
 * localStorage blob like `{ project: "false", contextLines: "3" }` would spread
 * strings over the typed defaults and the chrome would render the wrong UI
 * (`App.tsx` reads these fields directly) instead of the fail-closed defaults
 * this layer promises. Drops non-object / array input wholesale to the defaults
 * (`typeof [] === "object"` is true, so the explicit `Array.isArray` rejection
 * keeps a persisted array's numeric keys out of the settings object). The
 * validation is data-driven off `SETTINGS_DEFAULTS` — `typeof default === typeof
 * persisted` per key — so a new field added to `Settings` is covered for free.
 */
export function mergeSettings(raw: unknown): Settings {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return { ...SETTINGS_DEFAULTS };
  const r = raw as Record<string, unknown>;
  const out = { ...SETTINGS_DEFAULTS };
  for (const key of Object.keys(SETTINGS_DEFAULTS) as (keyof Settings)[]) {
    const persisted = r[key];
    // accept the persisted value only when its runtime type matches the
    // default's type (string→string, boolean→boolean, number→number); anything
    // else (wrong type, missing) keeps the typed default for this field.
    if (key in r && typeof persisted === typeof SETTINGS_DEFAULTS[key]) {
      (out[key] as Settings[keyof Settings]) = persisted as Settings[keyof Settings];
    }
  }
  return out;
}

export interface SettingsSlice {
  settings: Settings;
  /** set one settings key (the prototype's `setSetting`). */
  setSetting: <K extends keyof Settings>(key: K, value: Settings[K]) => void;
}

export type SettingsSliceSet = (
  partial: Partial<SettingsSlice> | ((s: SettingsSlice) => Partial<SettingsSlice>),
) => void;

export function createSettingsSlice(set: SettingsSliceSet): SettingsSlice {
  return {
    settings: { ...SETTINGS_DEFAULTS },
    setSetting: (key, value) =>
      set((s) => ({ settings: { ...s.settings, [key]: value } })),
  };
}
