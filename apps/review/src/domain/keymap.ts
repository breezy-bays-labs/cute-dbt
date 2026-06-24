// The keyboard action registry — the SSOT every downstream interaction derives
// from. Ported faithfully from the cute-dbt-next prototype's keymap.js into
// typed TypeScript. The whole app's keyboard logic is written against CANONICAL
// keys (the defaults below). Rebinding doesn't rewrite that logic — it builds an
// alias table that translates whatever physical key the user pressed back to the
// canonical key the handlers already expect, and shadows the old default so it
// stops firing. So every context-dependent behavior ("[" = prev-hunk in a diff
// but prev-section on the PR page) keeps working through one rebind, and there's
// a single source of truth for the help overlay + the remap UI + the footer.
//
// LAYER: this file is PURE DOMAIN — no I/O, no zustand, no React. The keymap
// Zustand slice (src/data/keymap-slice.ts) and the Footer (src/view/Footer.tsx)
// derive from this module; they never re-declare a predicate or a binding.

// ── identifiers ──────────────────────────────────────────────────────────────

/** The six functional groups; each tints its keys one hue everywhere. */
export type GroupId = "app" | "goto" | "view" | "move" | "code" | "review";

/** Active entity (the number-row selectors). */
export type Entity = "pr" | "models" | "macros" | "seeds" | "else";

/**
 * The current page context, for `when` evaluation. `app.js` builds this; the
 * selectors below resolve it. `view` is the active view value, `codeMode` is set
 * when a code surface is open, `viewCount` is how many views the active entity
 * exposes (so the positional ⇧1..⇧4 view actions light up correctly), and
 * `inReview`/`hasUnreviewed`/`hasOpenThread` carry the review-FLOW signals the
 * flow actions gate on (degrading honestly to inactive until S2/V1 feed them).
 */
export interface KbContext {
  entity: Entity;
  view: string;
  codeMode?: "diff" | "file";
  viewCount?: number;
  /** display noun for the instance-cycle footer chip (model · macro · seed · file). */
  noun?: string;
  /** true once a reviewable surface is mounted (the flow actions gate on it). */
  inReview?: boolean;
  /** true while ≥1 in-scope model is still unreviewed (next/prev-unreviewed). */
  hasUnreviewed?: boolean;
  /** true when a comment thread is focused + resolvable (the keyboard-resolve handler signal). */
  hasOpenThread?: boolean;
}

/** A `when` predicate — where a binding is live (omit = always). */
export type WhenPredicate = (c: KbContext) => boolean;

/**
 * A directional-motion hint folded into typed registry data (the prototype's
 * hand-written `dirHint`, the P4#17 / P1#1 smell S1 fixes). Motion keys
 * (hjkl/arrows/⇧hjkl) are not single rebindable actions, so they ride along the
 * registry as static, when-gated chips rather than a hand-written footer string.
 */
export interface MotionHint {
  /** display key tokens already resolved (e.g. ["⇧hjkl"], ["j","k"], ["←→"]). */
  keys: string[];
  label: string;
  /** where the motion chip is live (same predicate vocabulary as actions). */
  when: WhenPredicate;
}

/** ONE mapped action — the single source of truth the handlers + hints resolve against. */
export interface Action {
  /** stable id the handlers + hints resolve against. */
  id: string;
  /** default key token ("d", "!", "Enter", " ", …). */
  def: string;
  /** human label (map · hints · palette). */
  label: string;
  /** where it's live (omit = always). */
  when?: WhenPredicate;
  /** true = not user-rebindable (motion/select keys). */
  fixed?: boolean;
}

export interface KeyGroup {
  id: GroupId;
  /** display name. */
  group: string;
  /** CSS var for the group's hue. */
  color: string;
  actions: Action[];
}

// ── `when` context predicates ────────────────────────────────────────────────
// EXPORTED ONCE, here. S2's dispatcher, the footer, and the future command
// palette all import these — fixing the prototype's P4#17 duplication where the
// same predicate logic was re-expressed in app.js's dirHint + footerHints.

// (Predicates are `function` declarations — not arrow consts — so the coverage
// instrumenter attributes the hits my exhaustive cube tests produce by name.)

/** Models·topology or PR·lineage — the shelf surface (graph + code + threads). */
export function isTopoShelf(c: KbContext): boolean {
  return (
    (c.entity === "models" && c.view === "topology") ||
    (c.entity === "pr" && c.view === "lineage")
  );
}

/** The Models code diff surface specifically (diff mode of the code view). */
export function isCodeDiff(c: KbContext): boolean {
  return c.entity === "models" && c.view === "code" && c.codeMode === "diff";
}

/** Any surface that exposes a diff pane (drives the diff/file/compiled chips). */
export function hasDiffPane(c: KbContext): boolean {
  return (
    isTopoShelf(c) ||
    c.view === "code" ||
    (c.view === "data" && (c.entity === "models" || c.entity === "seeds"))
  );
}

/** Any surface that hosts comment threads (drives thread-navigation chips). */
export function inThreads(c: KbContext): boolean {
  return isTopoShelf(c) || isCodeDiff(c);
}

// ── flow-action `when` predicates (Council MUST-FIX D) ───────────────────────
// The review FLOW the prototype LACKS, registered first-class so the footer
// shows them inactive/greyed until their surfaces land (S2/V1/S10). They degrade
// HONESTLY: greyed when the when-context isn't met (never a false claim that the
// motion works). Handlers wire in later slices; here they are REGISTERED.

/**
 * mark-reviewed-advance + next/prev-unreviewed live on a reviewable instance —
 * AND only once the review flow is actually mounted (`inReview`). Gating on the
 * surface family alone (entity/view) would mark these flow actions active before
 * any reviewable surface exists, contradicting the declared flow contract (the
 * handlers land in S2/V1 and rely on the flow being present). The flow signal
 * `inReview` degrades honestly: until a slice feeds it, these actions read
 * inactive (greyed in the footer) rather than falsely claiming the motion works.
 */
export function isReviewable(c: KbContext): boolean {
  return (c.entity !== "pr" || c.view === "files") && c.inReview === true;
}

/** next/prev-unreviewed additionally need an unreviewed instance to advance to. */
export function hasUnreviewedTarget(c: KbContext): boolean {
  return isReviewable(c) && c.hasUnreviewed === true;
}

/**
 * `resolve` (⇧R) is keyed whenever a thread surface is focused (`inThreads`) —
 * matching the prototype, so the footer chip + key are shown in-threads. Whether
 * a thread is actually OPEN/resolvable at the cursor is a HANDLER-time signal
 * (`c.hasOpenThread`), not a key-visibility gate: the binding stays visible and
 * the S2/V1 handler decides whether the keypress resolves anything. This keeps
 * exactly ONE action on the ⇧R chord (no subset-`when` twin to conflict with).
 */
export function canResolveThread(c: KbContext): boolean {
  return inThreads(c) && c.hasOpenThread === true;
}

// ── the ONE registry ─────────────────────────────────────────────────────────
// Every action belongs to exactly one functional group; the group's color tints
// the key everywhere (keyboard map · footer · button chips). A key may be reused
// across mutually-exclusive `when` contexts — that is NOT a conflict, it's how
// one keyboard serves every screen.

export const KEY_GROUPS: readonly KeyGroup[] = [
  {
    id: "app",
    group: "App",
    color: "var(--legend-3)",
    actions: [
      { id: "palette", def: "/", label: "command palette · jump to anything" },
      { id: "help", def: "?", label: "keyboard help" },
      { id: "settings", def: ",", label: "settings" },
      { id: "sidebar", def: "s", label: "review-checklist sidebar" },
      { id: "review", def: "w", label: "write review" },
      // Council MUST-FIX D: the `>` command-mode of the palette, registered
      // first-class. Always available app-wide; handler lands in S10.
      { id: "command-mode", def: ">", label: "command mode (>) · run an app action" },
    ],
  },
  {
    id: "goto",
    group: "Go to",
    color: "var(--legend-6)",
    actions: [
      { id: "entity-pr", def: "1", label: "PR" },
      { id: "entity-models", def: "2", label: "Models" },
      { id: "entity-macros", def: "3", label: "Macros" },
      { id: "entity-seeds", def: "4", label: "Seeds" },
      { id: "entity-else", def: "5", label: "Else (Project)" },
      { id: "pr", def: "p", label: "jump to the PR page" },
    ],
  },
  {
    id: "view",
    group: "View",
    color: "var(--legend-5)",
    actions: [
      { id: "view-1", def: "!", label: "1st view of the entity", when: (c) => (c.viewCount || 0) >= 1 },
      { id: "view-2", def: "@", label: "2nd view", when: (c) => (c.viewCount || 0) >= 2 },
      { id: "view-3", def: "#", label: "3rd view", when: (c) => (c.viewCount || 0) >= 3 },
      { id: "view-4", def: "$", label: "4th view", when: (c) => (c.viewCount || 0) >= 4 },
      { id: "inst-next", def: "n", label: "next model · macro · seed · file", when: (c) => c.entity !== "pr" },
      { id: "inst-prev", def: "b", label: "previous instance", when: (c) => c.entity !== "pr" },
    ],
  },
  {
    id: "move",
    group: "Move",
    color: "var(--legend-4)",
    actions: [
      { id: "prev-hunk", def: "[", label: "previous hunk · PR section", when: inThreads },
      { id: "next-hunk", def: "]", label: "next hunk · PR section", when: inThreads },
      { id: "prev-thread", def: "{", label: "previous comment thread", when: inThreads },
      { id: "next-thread", def: "}", label: "next comment thread", when: inThreads },
      { id: "next-file", def: "Tab", fixed: true, label: "next file · ⇧⇥ previous", when: isTopoShelf },
      {
        id: "select",
        def: "Enter",
        fixed: true,
        label: "select · commit cursor · open",
        when: (c) => isTopoShelf(c) || (c.entity === "pr" && c.view === "files"),
      },
      // Council MUST-FIX D: next/prev-unreviewed — keyboard review-flow motion.
      // Keys chosen to avoid conflict with the active set (see findConflicts /
      // the flow-action conflict test): N (⇧n) / B (⇧b) sit on the shift layer of
      // the instance-cycle keys, semantically "jump to the next UNREVIEWED one".
      { id: "next-unreviewed", def: "N", label: "next unreviewed (jump)", when: hasUnreviewedTarget },
      { id: "prev-unreviewed", def: "B", label: "previous unreviewed (jump)", when: hasUnreviewedTarget },
    ],
  },
  {
    id: "code",
    group: "Code",
    color: "var(--legend-2)",
    actions: [
      { id: "diff", def: "d", label: "diff view", when: hasDiffPane },
      { id: "file", def: "f", label: "file view", when: hasDiffPane },
      { id: "compiled", def: "g", label: "compiled SQL · CTE ⇄ code sync", when: isTopoShelf },
    ],
  },
  {
    id: "review",
    group: "Review",
    color: "var(--legend-7)",
    actions: [
      {
        id: "panel",
        def: "v",
        label: "toggle shelf · details · comments",
        when: (c) =>
          isTopoShelf(c) ||
          c.view === "node" ||
          c.view === "code" ||
          (c.entity === "models" && c.view === "data"),
      },
      { id: "comments-only", def: "c", label: "comments-only ⇄ full code", when: isTopoShelf },
      { id: "comments-hidden", def: "C", label: "hide all comments ⇄ show", when: (c) => isTopoShelf(c) || c.view === "code" },
      {
        id: "pin-info",
        def: "i",
        label: "pin the details tooltip",
        when: (c) =>
          isTopoShelf(c) || ((c.entity === "macros" || c.entity === "seeds") && c.view === "review"),
      },
      {
        id: "reply",
        def: " ",
        fixed: true,
        label: "reply · select line(s) · commit node",
        when: (c) => isTopoShelf(c) || isCodeDiff(c) || (c.entity === "pr" && c.view === "files"),
      },
      { id: "quote", def: "q", label: "quote-reply the focused comment", when: inThreads },
      { id: "edit", def: "e", label: "edit the focused comment", when: inThreads },
      // Council MUST-FIX D: `resolve` (⇧R) IS the first-class keyboard-resolve
      // flow action — the prototype's pre-existing `resolve` (keymap.js, R =
      // inThreads) already IS "resolve the focused thread from the keyboard", so
      // it is the council-D keyboard-resolve verb, not a separate one. It is kept
      // as the SINGLE keyboard-resolve action on ⇧R; there is no second action on
      // that chord (an earlier draft added a redundant `resolve-from-keyboard`
      // whose `when` — inThreads && hasOpenThread — was a STRICT SUBSET of this
      // one's `inThreads`, so both were co-active on ⇧R wherever it fired: a
      // registry conflict findConflicts reports. Collapsing to one removes it.)
      // The prototype's handler is mouse-only (FEATURE-GAP P2#6); the keyboard
      // handler wiring (and reading the focused/open-thread signal) lands in
      // S2/V1 — this registry entry is the SSOT it derives from.
      { id: "resolve", def: "R", label: "resolve / unresolve the focused thread", when: inThreads },
      { id: "next-open-thread", def: "o", label: "next open conversation (PR-wide)", when: (c) => c.entity === "models" || c.entity === "pr" },
      { id: "prev-open-thread", def: "O", label: "previous open conversation", when: (c) => c.entity === "models" || c.entity === "pr" },
      // The prototype's `mark` (x = mark-reviewed). Council MUST-FIX D reframes it
      // as the explicit flow verb mark-reviewed-AND-advance — same x chord, same
      // when, but the canonical flow-action id the V1 loop drives.
      { id: "mark-reviewed-advance", def: "x", label: "mark reviewed (and advance)", when: isReviewable },
    ],
  },
];

// ── motion hints (the folded dirHint, now typed registry data) ───────────────
// The prototype's hand-written dirHint motions, folded into typed, when-gated
// registry data so the footer never re-expresses the predicate logic.

export const MOTION_HINTS: readonly MotionHint[] = [
  { keys: ["⇧hjkl"], label: "graph node", when: isTopoShelf },
  { keys: ["hjkl", "←↑↓→"], label: "code", when: isTopoShelf },
  { keys: ["j", "k"], label: "line/reply", when: isCodeDiff },
  { keys: ["←→"], label: "test", when: (c) => c.entity === "models" && c.view === "data" },
  { keys: ["↑↓"], label: "file", when: (c) => c.entity === "pr" && c.view === "files" },
  { keys: ["←→"], label: "name/check/badges", when: (c) => c.entity === "pr" && c.view === "files" },
];

// ── derived indices ──────────────────────────────────────────────────────────

/** Keys not bound to a single rebindable action (no dedicated key on the board). */
export const FIXED_KEYS: readonly { keys: string; label: string }[] = [
  { keys: "h j k l · ← ↑ ↓ →", label: "Move — code lines; ⇧ moves topology-graph nodes" },
  { keys: "esc", label: "close the shelf / any overlay" },
  { keys: "⌥ ← / ⌥ →", label: "back / forward through navigation history" },
];

export const ALL_ACTIONS: readonly Action[] = KEY_GROUPS.flatMap((g) => g.actions);

/** action id → { color, group, label, def } for the viz + footer + palette. */
export interface ActionMeta {
  color: string;
  group: string;
  label: string;
  def: string;
}
export const CATEGORIES: readonly { id: GroupId; label: string; color: string }[] = KEY_GROUPS.map(
  (g) => ({ id: g.id, label: g.group, color: g.color }),
);
const ACTION_META: Record<string, ActionMeta> = {};
KEY_GROUPS.forEach((g) =>
  g.actions.forEach((a) => {
    ACTION_META[a.id] = { color: g.color, group: g.group, label: a.label, def: a.def };
  }),
);
export function actionMeta(id: string): ActionMeta | null {
  return ACTION_META[id] ?? null;
}
export function actionLabel(id: string): string {
  const a = ALL_ACTIONS.find((x) => x.id === id);
  return a ? a.label : id;
}

// ── keymap (defaults + sparse-override merge) ────────────────────────────────

/** A keymap is action-id → key token; a sparse override merges over defaults. */
export type Keymap = Record<string, string>;

export function defaultKeymap(): Keymap {
  const m: Keymap = {};
  ALL_ACTIONS.forEach((a) => {
    m[a.id] = a.def;
  });
  return m;
}

/** Merge a sparse override (settings.keymap) over the registry defaults. */
export function mergeKeymap(override?: Keymap | null): Keymap {
  return { ...defaultKeymap(), ...(override || {}) };
}

// ── chords & layers ──────────────────────────────────────────────────────────
// A stored token is either a plain char ("d"), a SHIFTED glyph ("?" = Shift+/),
// or — once real chords land — a modifier-prefixed token ("meta+d").

export const SHIFT_MAP: Record<string, string> = {
  "`": "~", "1": "!", "2": "@", "3": "#", "4": "$", "5": "%", "6": "^",
  "7": "&", "8": "*", "9": "(", "0": ")", "-": "_", "=": "+", "[": "{",
  "]": "}", "\\": "|", ";": ":", "'": '"', ",": "<", ".": ">", "/": "?",
};
const UNSHIFT: Record<string, string> = {};
Object.keys(SHIFT_MAP).forEach((k) => {
  UNSHIFT[SHIFT_MAP[k] as string] = k;
});

const MOD_ORDER = ["ctrl", "alt", "meta", "shift"] as const;
export type ModKey = (typeof MOD_ORDER)[number];

export interface ParsedToken {
  mods: Set<ModKey>;
  /** the physical base key under the modifier layer. */
  base: string;
  /** the display glyph. */
  glyph: string;
}

/** token → { mods, base, glyph }. */
export function parseToken(tok: string): ParsedToken {
  const mods = new Set<ModKey>();
  let s = String(tok);
  // Strip modifier prefixes order-INDEPENDENTLY: loop until no `<mod>+` prefix
  // remains, so a token listing modifiers in any order ("ctrl+alt+d" as well as
  // "alt+ctrl+d") parses identically. A single fixed-order pass would leave a
  // trailing modifier glued to the base key when the orders disagree.
  for (let matched = true; matched; ) {
    matched = false;
    for (const m of ["meta", "alt", "ctrl"] as const) {
      if (s.startsWith(m + "+")) {
        mods.add(m);
        s = s.slice(m.length + 1);
        matched = true;
      }
    }
  }
  if (UNSHIFT[s] != null) {
    mods.add("shift");
    return { mods, base: UNSHIFT[s] as string, glyph: s };
  }
  if (/^[A-Z]$/.test(s)) {
    mods.add("shift");
    return { mods, base: s.toLowerCase(), glyph: s };
  }
  return { mods, base: s, glyph: s };
}

function layerKey(mods: Set<ModKey>): string {
  return MOD_ORDER.filter((m) => mods.has(m)).join("+");
}

// ── binding index + conflict detection ───────────────────────────────────────

export interface Binding {
  id: string;
  token: string;
  base: string;
  glyph: string;
  color: string;
  label: string;
  group: string;
  isCustom: boolean;
}

/** Build the binding index keyed by modifier-layer then physical base key. */
export function layerBindings(keymap?: Keymap | null): Record<string, Record<string, Binding[]>> {
  const km = mergeKeymap(keymap);
  const byLayer: Record<string, Record<string, Binding[]>> = {};
  Object.keys(km).forEach((id) => {
    const meta = ACTION_META[id];
    if (!meta) return;
    const tok = km[id] as string;
    const p = parseToken(tok);
    const lk = layerKey(p.mods);
    const slot = (byLayer[lk] = byLayer[lk] || {});
    (slot[p.base] = slot[p.base] || []).push({
      id,
      token: tok,
      base: p.base,
      glyph: p.glyph,
      color: meta.color,
      label: meta.label,
      group: meta.group,
      isCustom: tok !== meta.def,
    });
  });
  return byLayer;
}

export interface Conflict {
  layer: string;
  base: string;
  glyph: string;
  actions: Binding[];
}

/**
 * The CANONICAL key identity of a stored token: `layer|base`, where `base` is
 * the physical key under the modifier layer and `layer` is the shift/meta/alt/
 * ctrl chord. This is the identity the WHOLE keyboard model compares against —
 * `findConflicts` (two actions on the same canonical key in one active context)
 * AND `makeCanonicalizer` (a physical keystroke → the canonical key the handlers
 * expect) both derive from it, so the two can never disagree.
 *
 * The shift layer is part of the identity: `"n"` (base `n`, no mods) and `"N"`
 * (base `n`, shift) are DISTINCT canonical keys — exactly what the uppercase
 * shift-layer glyph tokens (`N`/`B`/`C`/`O`/`R`) rely on. A naive case-fold to
 * lowercase would collapse `N` onto `n`, silently colliding `next-unreviewed`
 * with `inst-next` (and `comments-hidden`/`prev-open-thread`/… with their
 * lowercase twins) in the alias table — a real runtime dispatch bug that a
 * raw-token compare cannot see. Comparing canonical keys is the SSOT soundness
 * guarantee the slice exists to provide.
 */
export function canonicalKey(token: string): string {
  const p = parseToken(token);
  return layerKey(p.mods) + "|" + p.base;
}

/**
 * Every (layer, base) holding 2+ actions → a conflict — UNLESS scoped to an
 * active set, in which case cross-context reuse (one keyboard, many screens) is
 * fine and only two actions live in the SAME context count as a conflict.
 *
 * Conflict identity is the CANONICAL key (via `layerBindings`, which keys by
 * `parseToken`'s `(layer, base)`) — so a shift-layer binding (`N`) and its
 * lowercase twin (`n`) are correctly treated as distinct, while two RAW tokens
 * that fold to the SAME canonical key (e.g. a rebind onto `D` where `d` is also
 * bound, both base `d` no-mods) ARE reported even though the raw tokens differ.
 */
export function findConflicts(keymap?: Keymap | null, activeSet?: ReadonlySet<string>): Conflict[] {
  const byLayer = layerBindings(keymap);
  const out: Conflict[] = [];
  Object.keys(byLayer).forEach((lk) =>
    Object.keys(byLayer[lk] as Record<string, Binding[]>).forEach((base) => {
      let arr = (byLayer[lk] as Record<string, Binding[]>)[base] as Binding[];
      if (activeSet) arr = arr.filter((a) => activeSet.has(a.id));
      if (arr.length > 1) {
        out.push({ layer: lk, base, glyph: (arr[0] as Binding).glyph, actions: arr });
      }
    }),
  );
  return out;
}

// ── context selectors ────────────────────────────────────────────────────────

/** The active action-id set for a page context — the SSOT for highlighting + conflicts. */
export function activeActionIds(ctx: KbContext): Set<string> {
  const s = new Set<string>();
  ALL_ACTIONS.forEach((a) => {
    if (!a.when || a.when(ctx)) s.add(a.id);
  });
  return s;
}

// pretty label for a key token, for the help/remap UI + footer chips.
const PRETTY: Record<string, string> = {
  " ": "Space", Spacebar: "Space", ArrowLeft: "←", ArrowRight: "→",
  ArrowUp: "↑", ArrowDown: "↓", Escape: "Esc", Enter: "↵", Tab: "Tab",
};
const MOD_SYM: Record<ModKey, string> = { shift: "⇧", alt: "⌥", meta: "⌘", ctrl: "⌃" };
function modLabel(mod: ModKey): string {
  return MOD_SYM[mod] || mod;
}

export function displayKey(token: string | null | undefined): string {
  if (token == null || token === "") return "—";
  if (PRETTY[token]) return PRETTY[token] as string;
  if (/^(meta|alt|ctrl)\+/.test(token)) {
    const p = parseToken(token);
    return MOD_ORDER.filter((m) => p.mods.has(m)).map(modLabel).join("") + displayKey(p.glyph);
  }
  if (/^[A-Z]$/.test(token)) return modLabel("shift") + token; // shift+letter
  return token;
}

// ── footer hints (registry-derived chips) ────────────────────────────────────

export interface FooterChip {
  keys: string[];
  label: string;
  /** true when this chip's action is live in the context; false → render greyed. */
  active: boolean;
}

/**
 * Footer hint chips, derived from the registry so the status bar can never drift
 * from the real bindings: VISIBILITY/active comes from each action's `when`, KEYS
 * come from the live (possibly rebound) keymap. The flow actions appear as chips
 * that render greyed (active:false) until their when-context is met — degrading
 * honestly. Returns ordered { keys, label, active }.
 */
export function footerHints(ctx: KbContext, keymap?: Keymap | null): FooterChip[] {
  const km = mergeKeymap(keymap);
  const active = activeActionIds(ctx);
  const k = (id: string): string => displayKey(km[id]);
  const out: FooterChip[] = [];
  // A chip is emitted when the action is registered for this surface family; its
  // `active` flag reflects whether its when-context is currently met. We include
  // both always-relevant chips and the flow chips (which degrade honestly).
  const chip = (id: string, keys: string[], label: string): void => {
    out.push({ keys, label, active: active.has(id) });
  };
  // motion hints (folded dirHint) first — they prefix the registry chips.
  MOTION_HINTS.forEach((h) => {
    if (h.when(ctx)) out.push({ keys: h.keys, label: h.label, active: true });
  });

  if (active.has("inst-next")) out.push({ keys: [k("inst-next"), k("inst-prev")], label: ctx.noun || "instance", active: true });
  if (active.has("compiled")) out.push({ keys: [k("diff"), k("file"), k("compiled")], label: "diff/file/compiled", active: true });
  else if (active.has("diff")) out.push({ keys: [k("diff"), k("file")], label: "diff/file", active: true });
  if (active.has("next-hunk")) out.push({ keys: [k("prev-hunk"), k("next-hunk")], label: "hunk", active: true });
  if (active.has("next-thread")) out.push({ keys: [k("prev-thread"), k("next-thread")], label: "thread", active: true });
  if (active.has("next-file")) out.push({ keys: [k("next-file")], label: "file", active: true });
  if (active.has("select")) out.push({ keys: [k("select")], label: "select", active: true });
  if (active.has("reply")) out.push({ keys: [k("reply")], label: "reply", active: true });
  if (active.has("quote")) out.push({ keys: [k("quote")], label: "quote", active: true });
  if (active.has("edit")) out.push({ keys: [k("edit")], label: "edit", active: true });
  if (active.has("resolve")) out.push({ keys: [k("resolve")], label: "resolve", active: true });
  if (active.has("next-open-thread")) out.push({ keys: [k("next-open-thread")], label: "open conv", active: true });
  if (active.has("panel")) out.push({ keys: [k("panel")], label: "panel", active: true });
  if (active.has("comments-only")) out.push({ keys: [k("comments-only"), k("comments-hidden")], label: "comments", active: true });
  else if (active.has("comments-hidden")) out.push({ keys: [k("comments-hidden")], label: "hide comments", active: true });
  if (active.has("pin-info")) out.push({ keys: [k("pin-info")], label: "info", active: true });

  // ── flow chips (Council MUST-FIX D) — always emitted on a reviewable family,
  // greyed (active:false) until their when-context is met. They degrade HONESTLY:
  // the chip shows the key + label but reads inactive when the motion can't fire.
  if (ctx.entity !== "pr" || ctx.view === "files") {
    chip("mark-reviewed-advance", [k("mark-reviewed-advance")], "reviewed");
    chip("next-unreviewed", [k("next-unreviewed"), k("prev-unreviewed")], "unreviewed");
  }
  // The keyboard-resolve flow chip IS the `resolve` chip emitted above (⇧R,
  // inThreads) — council-D's keyboard-resolve verb, not a separate action. No
  // distinct "resolve thread" chip: that was the deduped `resolve-from-keyboard`.
  // command-mode is always available — a persistent app-level chip.
  chip("command-mode", [k("command-mode")], "command");

  return out;
}

// ── canonicalizer + capture (rebinding infra) ────────────────────────────────

const DEAD = "\u0000dead"; // a token nothing in the app compares against
export { DEAD };

/**
 * Normalize a key/token to its CANONICAL identity for alias + shadow lookup —
 * `canonicalKey` (`layer|base`). Case-SENSITIVE by construction: an uppercase
 * letter / shifted glyph parses onto the shift layer, so `"N"` and `"n"` map to
 * DISTINCT identities. A previous version case-folded single chars to lowercase,
 * which collapsed the shift-layer glyph tokens (`N`/`B`/`C`/`O`) onto their
 * lowercase twins (`inst-next`/`inst-prev`/`comments-only`/…) in the alias table
 * — a silent runtime collision the layer-keyed `findConflicts` couldn't see.
 * Sharing `canonicalKey` keeps the canonicalizer and the conflict check in
 * lockstep on ONE notion of key identity.
 */
function norm(key: string): string {
  return canonicalKey(key);
}

/**
 * Build the physical→canonical alias translator for a (possibly customized)
 * keymap. Pressing a bound key yields its action's canonical key; a default key
 * whose action has moved away is shadowed to DEAD so it stops firing.
 *
 * Identity is the CANONICAL key (`layer|base`, shift-aware), so a physical
 * `Shift+N` (`eventKey === "N"`) resolves through its own slot and never
 * collides with a bare `n`. The returned function takes the raw `eventKey` (the
 * DOM `KeyboardEvent.key`, already the uppercase/shift glyph for a shifted key)
 * and returns the action's canonical default TOKEN the handlers compare against
 * (`"N"`, `"d"`, `"?"`, …), or DEAD for a shadowed default, or the raw key
 * unchanged when nothing is bound there.
 */
export function makeCanonicalizer(keymap?: Keymap | null): (eventKey: string) => string {
  const defs = defaultKeymap();
  const km = { ...defs, ...(keymap || {}) };
  const alias: Record<string, string> = {};
  const boundSet = new Set(
    Object.values(km)
      .filter((t) => !String(t).includes("+"))
      .map(norm),
  );
  for (const id in km) {
    const t = km[id] as string;
    if (String(t).includes("+")) continue;
    alias[norm(t)] = defs[id] as string;
  }
  // shadow defaults that are no longer bound to anything
  ALL_ACTIONS.forEach((a) => {
    const d = norm(a.def);
    if (!boundSet.has(d) && !(d in alias)) alias[d] = DEAD;
  });
  return (eventKey: string): string => {
    const n = norm(eventKey);
    return n in alias ? (alias[n] as string) : eventKey;
  };
}

/** A minimal KeyboardEvent-shaped record for captureKey (DOM-independent). */
export interface CapturableKey {
  key: string;
  metaKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
}

/**
 * Modifier-only keys — a bare press of one of these never produces a token.
 */
const MODIFIER_KEYS: readonly string[] = ["Shift", "Control", "Alt", "Meta", "CapsLock", "Dead", "Process"];

/**
 * Reserved keys that are never user-rebindable, regardless of how a rebind is
 * attempted (the remap UI's `captureKey`, OR a programmatic / persisted override
 * that bypasses the UI). This is the SINGLE source the `captureKey` deny path AND
 * the `DENY_REBIND_KEYS` set below both derive from, so they cannot drift:
 *   • whitespace/commit keys (`Space`/`Enter`/`Tab`) — the `fixed:true` actions;
 *   • overlay/edit-control keys (`Escape`/`Backspace`/`Delete`);
 *   • the directional cluster (`ArrowLeft/Right/Up/Down`) — motion stays fixed.
 * `captureKey` rejects them at the keyboard; `DENY_REBIND_KEYS` rejects them at
 * the store (write-time AND hydration sanitize), closing the bypass the narrower
 * fixed-only list left open.
 */
export const RESERVED_KEYS: readonly string[] = [
  " ",
  "Spacebar",
  "Enter",
  "Tab",
  "Escape",
  "Backspace",
  "Delete",
  "ArrowLeft",
  "ArrowRight",
  "ArrowUp",
  "ArrowDown",
];

/**
 * Capture a keydown into a storable token (the remap "press a key" UI). Returns
 * null for modifier-only / disallowed keys. The deny-list is the reserved keys
 * (Space/Enter/Tab/Esc/Backspace/Delete/arrows) + modifier chords (not bindable
 * yet). Letter case is PRESERVED so a user can rebind onto a shift-layer letter
 * (e.g. `Shift+N` → token `"N"`); case-folding here would make every shift-layer
 * action (`N`/`B`/`C`/`O`/`R`) un-rebindable by collapsing the shifted letter
 * onto its already-bound lowercase twin.
 */
export function captureKey(e: CapturableKey): string | null {
  const k = e.key;
  if (MODIFIER_KEYS.includes(k)) return null;
  if (RESERVED_KEYS.includes(k)) return null; // reserved (incl. arrows)
  if (e.metaKey || e.ctrlKey || e.altKey) return null; // ⌘/⌃/⌥ chords not bindable yet
  if (k.length !== 1) return null; // single printable char only
  return k; // case-preserving — shift-layer letters stay distinct
}

/**
 * The set of key TOKENS that are NOT user-rebindable — enforced at every write
 * path (the `rebindAction` store action AND the persisted-override hydration
 * sanitize). It unions the `fixed:true` action defaults with the reserved
 * non-action keys (`RESERVED_KEYS`), so a programmatic or stale-blob override
 * can never reintroduce a reserved binding (`Tab`, `Space`, an arrow, …) that
 * the `captureKey` UI would have refused.
 */
export const DENY_REBIND_KEYS: ReadonlySet<string> = new Set<string>([
  ...ALL_ACTIONS.filter((a) => a.fixed).map((a) => a.def),
  ...RESERVED_KEYS,
]);
