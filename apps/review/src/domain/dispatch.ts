// The keyboard PRECEDENCE LADDER as a PURE function. This is the heart of the
// single-dispatcher contract (discovery risk #5: ONE capture-phase listener,
// never N competing ones; the R-ref mirror DISSOLVED). The React hook
// (src/data/use-keydown.ts) is a thin shell: it reads `store.getState()`, builds
// a `DispatchInput`, calls `routeKey` here, and applies the returned
// `DispatchAction` to the store. ALL routing logic — the ladder, the modal gate,
// the positional ⇧digit derivation, the entity/view/app keys — lives HERE, where
// it is layer-pure and exhaustively testable without a DOM.
//
// LAYER: PURE DOMAIN — no I/O, no zustand, no React, no DOM types beyond a small
// DOM-independent `KeyEventLike` record (so tests construct events as plain
// objects, exactly the captureKey/CapturableKey pattern S1 established).
//
// THE LADDER (highest precedence first — the prototype's app.js onKey order):
//   1. input-guard      — target is INPUT/SELECT/TEXTAREA → ignore entirely.
//   2. bare-modifier     — a lone Shift/Ctrl/Alt/Meta/CapsLock never fires.
//   3. alt-arrow history — ⌥←/⌥→ → back/forward (BEFORE the modal gate, so
//                          history works even with an overlay open — prototype).
//   4. modal-gate        — any overlay open → the overlay OWNS the keyboard;
//                          the dispatcher emits nothing (the overlay's own
//                          handler, a later slice, takes over).
//   5. app keys          — palette / help / settings / sidebar / review / pr.
//   6. entity keys       — the number row (1..5) selects the entity.
//   7. view keys         — ⇧1..⇧4 select the entity's positional view.
//   8. context keys      — instance cycle (n/b), code-mode (d/f), panel (v), …
//                          everything whose `when` is surface-scoped.
//
// The canonicalization happens BEFORE this function (the hook applies S1's
// `makeCanonicalizer` to the physical key); `routeKey` compares the CANONICAL
// token directly (S1's note: the canonical token uniquely identifies the action
// — no separate `e.shiftKey` branch for shifted glyphs, since "N" already parses
// onto the shift layer).

import { AVAIL, type View } from "./matrix";
import type { Entity } from "./keymap";

/** A DOM-independent keyboard event (the subset the ladder reads). */
export interface KeyEventLike {
  /** the CANONICAL key token (already run through S1's canonicalizer). */
  key: string;
  /** the physical `e.code` (e.g. "Digit2") — the positional ⇧digit source of truth. */
  code?: string;
  /** the raw `e.key` BEFORE canonicalization — used only for the shifted-glyph fallback. */
  rawKey?: string;
  shiftKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
  ctrlKey?: boolean;
  /** the tagName of the event target (INPUT/SELECT/TEXTAREA → input-guard). */
  targetTag?: string;
  /** true if the target is an editable element (contenteditable) — also guarded. */
  targetEditable?: boolean;
}

/** The live nav/ui snapshot the ladder routes against (read from getState()). */
export interface DispatchInput {
  entity: Entity;
  view: View;
  /** true when ANY overlay is open (the modal gate). */
  modal: boolean;
}

/**
 * The typed dispatch outcome — a discriminated union the hook applies to the
 * store. `null` means "the dispatcher did not claim this key" (let it bubble /
 * do nothing); every non-null result is an intent the hook turns into a store
 * mutation. `preventDefault` rides along so the hook can call it on claimed keys.
 */
export type DispatchAction =
  | { kind: "history-back" }
  | { kind: "history-forward" }
  | { kind: "toggle-overlay"; overlay: OverlayName }
  | { kind: "open-overlay"; overlay: OverlayName }
  | { kind: "set-entity"; entity: Entity }
  | { kind: "goto-pr" }
  | { kind: "set-view"; view: View }
  | { kind: "cycle-instance"; dir: 1 | -1 }
  | { kind: "set-code-mode"; mode: "diff" | "file" }
  | { kind: "set-data-mode"; mode: "diff" | "file" }
  | { kind: "toggle-panel" }
  | { kind: "mark-reviewed-advance" }
  | { kind: "context"; action: string };

/** The overlay flags the ui slice owns (any open ⇒ modal gate). */
export type OverlayName =
  | "palette"
  | "settings"
  | "review"
  | "kbDrawer"
  | "sidebar"
  | "scope"
  | "shelf";

export interface DispatchResult {
  action: DispatchAction | null;
  /** true ⇒ the hook calls e.preventDefault() (a claimed key). */
  preventDefault: boolean;
}

const PASS: DispatchResult = { action: null, preventDefault: false };
function claim(action: DispatchAction): DispatchResult {
  return { action, preventDefault: true };
}

const BARE_MODIFIERS = new Set(["Shift", "Control", "Alt", "Meta", "CapsLock"]);

/** Shifted-glyph → 1-based digit fallback when `e.code` is absent (the prototype's map). */
const SHIFT_DIGIT: Record<string, number> = { "!": 1, "@": 2, "#": 3, "$": 4, "%": 5 };

/** The entity each number-row digit selects (1 PR · 2 Models · 3 Macros · 4 Seeds · 5 Else). */
const DIGIT_ENTITY: Record<string, Entity> = {
  "1": "pr",
  "2": "models",
  "3": "macros",
  "4": "seeds",
  "5": "else",
};

/**
 * Resolve the 1-based positional view digit from a ⇧digit press. Position is the
 * SSOT: prefer the physical `e.code` (Digit1..Digit9 — layout-stable), fall back
 * to the shifted glyph (`!`/`@`/…) when `code` is unavailable. Returns 0 for a
 * non-digit. (Mirrors app.js's `/^Digit([1-9])$/.test(e.code) ? … : {…}[e.key]`.)
 */
export function shiftDigit(ev: KeyEventLike): number {
  const m = /^Digit([1-9])$/.exec(ev.code ?? "");
  if (m) return Number(m[1]);
  return SHIFT_DIGIT[ev.rawKey ?? ev.key] ?? 0;
}

// Each rung is its own small function returning `DispatchResult | null` (null =
// "this rung does not claim the key — fall through to the next rung"). The ladder
// (`routeKey`) is then a short ordered fold over the rungs, keeping every unit's
// cyclomatic complexity well under the CRAP ceiling and making each rung
// independently testable. The ORDER in `routeKey` IS the precedence contract.

/** rung 1: a form-control / contenteditable target never produces a hotkey. */
function rungInputGuard(ev: KeyEventLike): DispatchResult | null {
  if (ev.targetTag && /^(INPUT|SELECT|TEXTAREA)$/.test(ev.targetTag)) return PASS;
  if (ev.targetEditable) return PASS;
  return null;
}

/** rung 2: a lone Shift/Ctrl/Alt/Meta/CapsLock is a chord prefix, never an action. */
function rungBareModifier(ev: KeyEventLike): DispatchResult | null {
  return BARE_MODIFIERS.has(ev.rawKey ?? ev.key) ? PASS : null;
}

/** rung 3: ⌥←/⌥→ history — BEFORE the modal gate (works with an overlay open). */
function rungAltArrow(ev: KeyEventLike): DispatchResult | null {
  if (ev.altKey && ev.key === "ArrowLeft") return claim({ kind: "history-back" });
  if (ev.altKey && ev.key === "ArrowRight") return claim({ kind: "history-forward" });
  return null;
}

/** rung 5: the app-level keys (palette/help/settings/sidebar/review/pr). */
const APP_KEY: Record<string, DispatchAction> = {
  "?": { kind: "toggle-overlay", overlay: "kbDrawer" },
  "/": { kind: "open-overlay", overlay: "palette" },
  ",": { kind: "toggle-overlay", overlay: "settings" },
  s: { kind: "toggle-overlay", overlay: "sidebar" },
  w: { kind: "open-overlay", overlay: "review" },
  p: { kind: "goto-pr" },
};
// The canonicalizer (S1) normalizes only the bare KEY, not the chord modifiers,
// so a `⌘/⌃+W` / `⌘/⌃+P` / `⌘/⌃+S` press arrives here with `ev.key` === "w"/"p"/
// "s". Without a modifier guard `rungAppKeys` would claim those chords and call
// `preventDefault`, hijacking the browser's close-tab / print / save shortcuts.
// Guard with `noMods` for parity with the entity/view rungs — an app key fires
// only as a BARE press (`?`/`/`/`,` use Shift on most layouts, which `noMods`
// deliberately allows; it gates meta/ctrl/alt only).
function rungAppKeys(ev: KeyEventLike, k: string): DispatchResult | null {
  if (!noMods(ev)) return null;
  const a = APP_KEY[k];
  return a ? claim(a) : null;
}

/** True iff no chord modifier is held (entity/view rungs require this). */
function noMods(ev: KeyEventLike): boolean {
  return !ev.metaKey && !ev.altKey && !ev.ctrlKey;
}

/** rung 6: the number-row entity keys (no modifiers). */
function rungEntityKeys(ev: KeyEventLike, k: string): DispatchResult | null {
  if (!ev.shiftKey && noMods(ev) && DIGIT_ENTITY[k]) {
    return claim({ kind: "set-entity", entity: DIGIT_ENTITY[k] });
  }
  return null;
}

/** rung 7: the positional ⇧digit view keys (claimed-but-no-op when out of range). */
function rungViewKeys(ev: KeyEventLike, st: DispatchInput): DispatchResult | null {
  if (!ev.shiftKey || !noMods(ev)) return null;
  const digit = shiftDigit(ev);
  if (digit < 1) return null;
  const views = AVAIL[st.entity];
  const target = views[digit - 1];
  if (digit <= views.length && target) return claim({ kind: "set-view", view: target });
  // a ⇧digit beyond the entity's view count is CLAIMED (does not fall to context)
  // but a no-op — mirroring the prototype's `return` after its shift-digit block.
  return { action: null, preventDefault: false };
}

/** rung 8: the surface-scoped context keys (instance cycle, mode, panel, …). */
function rungContextKeys(k: string, st: DispatchInput): DispatchResult | null {
  const notPr = st.entity !== "pr";
  if (notPr && (k === "n" || k === "N")) return claim({ kind: "cycle-instance", dir: k === "n" ? 1 : -1 });
  if (notPr && (k === "b" || k === "B")) return claim({ kind: "cycle-instance", dir: k === "b" ? -1 : 1 });
  if (notPr && k === "x") return claim({ kind: "mark-reviewed-advance" });
  if (notPr && (k === "d" || k === "f")) {
    if (st.view === "code") return claim({ kind: "set-code-mode", mode: k === "d" ? "diff" : "file" });
    if (st.view === "data") return claim({ kind: "set-data-mode", mode: k === "d" ? "diff" : "file" });
  }
  if (k === "v") return claim({ kind: "toggle-panel" });
  return null;
}

/**
 * THE LADDER. Pure: (event, snapshot) → result. An ordered fold over the rungs;
 * the ORDER is the precedence contract. Rung 4 (the modal gate) is inline because
 * it short-circuits the WHOLE remainder (everything below it) rather than just
 * claiming a key. Every rung above the gate (input-guard, bare-modifier,
 * alt-arrow) runs regardless of `modal`.
 */
export function routeKey(ev: KeyEventLike, st: DispatchInput): DispatchResult {
  const k = ev.key;
  return (
    rungInputGuard(ev) ??
    rungBareModifier(ev) ??
    rungAltArrow(ev) ??
    // ── rung 4: modal-gate — an open overlay OWNS the keyboard. ──────────────
    (st.modal
      ? PASS
      : (rungAppKeys(ev, k) ??
        rungEntityKeys(ev, k) ??
        rungViewKeys(ev, st) ??
        rungContextKeys(k, st) ??
        PASS))
  );
}
