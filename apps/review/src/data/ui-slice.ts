// The UI Zustand slice (S2) — overlay flags + the code anchor (line-anchored
// diff opens) + the derived `modal` gate. The fifth of the 5 store slices.
//
// LAYER: data (may import domain; never view/chrome). Composed into the app
// store; the dispatcher's modal-gate reads `anyOverlayOpen(ui)` to decide
// whether an overlay owns the keyboard.
//
// codeAnchor (load-bearing, from the prototype's app.js):
//   {id, line, side, nonce} — a request to scroll the diff to a comment line on
//   open. The `nonce` (Date.now() in the prototype; a monotonic counter here for
//   golden-determinism) forces a re-anchor even when (id,line,side) repeat, so
//   re-opening the SAME comment re-scrolls. The consuming pane does a DIRECT
//   scrollTop (NO smooth behavior — the prototype's explicit contract; smooth
//   would race the keyboard cursor).

import { produce } from "immer";
import { stepHunk, type HunkCursor } from "../domain/diff/hunk-cursor";
import type { NavStart } from "../domain/diff/patch-nav";

/** The overlay flags the ui slice owns. Any TRUE ⇒ the keyboard modal-gate fires. */
export interface OverlayFlags {
  /** the command palette (/). */
  palette: boolean;
  /** the settings modal (,). */
  settings: boolean;
  /** the write-review modal (w). */
  review: boolean;
  /** the keyboard-help drawer (?). */
  kbDrawer: boolean;
  /** the review-checklist sidebar (s) — a panel, NOT keyboard-owning by itself. */
  sidebar: boolean;
  /** the since-review scope panel. */
  scope: boolean;
  /** the topology detail shelf (v) — a panel, NOT keyboard-owning by itself. */
  shelf: boolean;
}

/** A line-anchored diff open request. The nonce forces a re-anchor on repeat. */
export interface CodeAnchor {
  id: string;
  line: number;
  side: "old" | "new";
  nonce: number;
}

/**
 * The overlays that OWN the keyboard when open (the modal gate). `sidebar` and
 * `shelf` are persistent PANELS that coexist with keyboard nav — they do NOT
 * hand the keyboard off — so they are excluded from the gate. This matches the
 * prototype's `modal = showPalette || showSettings || showReview || kbEdit`
 * (the panels were never in that disjunction).
 */
export const MODAL_OVERLAYS: readonly (keyof OverlayFlags)[] = [
  "palette",
  "settings",
  "review",
  "kbDrawer",
  "scope",
];

/** True when any KEYBOARD-OWNING overlay is open (the dispatcher's modal gate). */
export function anyOverlayOpen(flags: OverlayFlags): boolean {
  return MODAL_OVERLAYS.some((k) => flags[k]);
}

export interface UiSlice {
  overlays: OverlayFlags;
  /** the active line-anchor request (null = none). */
  codeAnchor: CodeAnchor | null;
  /** monotonic source for codeAnchor nonces (golden-deterministic, not Date.now). */
  anchorNonce: number;
  /** the active Models code-surface mode (diff/file) — promoted from App-local
   *  state to the store so the keyboard dispatcher can gate the diff/thread
   *  surface keys ([ ] / ⇧R / the hunk cursor) on it (V1). */
  codeMode: "diff" | "file";
  /** the running hunk cursor (V1 — the S5 next/prev-hunk deferral). `index===-1`
   *  is unset; the nonce forces a re-scroll even when the index repeats. */
  hunkCursor: HunkCursor;

  /** toggle one overlay flag. */
  toggleOverlay: (name: keyof OverlayFlags) => void;
  /** open one overlay flag (idempotent). */
  openOverlay: (name: keyof OverlayFlags) => void;
  /** close one overlay flag (idempotent). */
  closeOverlay: (name: keyof OverlayFlags) => void;
  /** close every overlay (the Escape-all path). */
  closeAllOverlays: () => void;
  /** request a line-anchored diff open (bumps the nonce so repeats re-anchor). */
  setCodeAnchor: (anchor: Omit<CodeAnchor, "nonce"> | null) => void;
  /** set the Models code-surface mode (diff/file). Resets the hunk cursor (a new
   *  mode is a fresh diff surface). */
  setCodeMode: (mode: "diff" | "file") => void;
  /** step the running hunk cursor over the given anchors (the [ / ] keys). */
  stepHunkCursor: (anchors: readonly NavStart[], dir: 1 | -1) => void;
  /** reset the hunk cursor (e.g. switching model / file). */
  resetHunkCursor: () => void;
}

export const UI_DEFAULTS: { overlays: OverlayFlags; codeMode: "diff" | "file"; hunkCursor: HunkCursor } = {
  overlays: {
    palette: false,
    settings: false,
    review: false,
    kbDrawer: false,
    sidebar: false,
    scope: false,
    shelf: false,
  },
  codeMode: "diff",
  hunkCursor: { index: -1, nonce: 0 },
};

export type UiSliceSet = (
  partial: UiSlice | Partial<UiSlice> | ((state: UiSlice) => UiSlice | Partial<UiSlice>),
) => void;
export type UiSliceGet = () => UiSlice;

export function createUiSlice(set: UiSliceSet, get: UiSliceGet): UiSlice {
  return {
    overlays: { ...UI_DEFAULTS.overlays },
    codeAnchor: null,
    anchorNonce: 0,
    codeMode: UI_DEFAULTS.codeMode,
    hunkCursor: { ...UI_DEFAULTS.hunkCursor },

    toggleOverlay: (name) =>
      set(
        produce((s: UiSlice) => {
          s.overlays[name] = !s.overlays[name];
        }),
      ),
    openOverlay: (name) =>
      set(
        produce((s: UiSlice) => {
          s.overlays[name] = true;
        }),
      ),
    closeOverlay: (name) =>
      set(
        produce((s: UiSlice) => {
          s.overlays[name] = false;
        }),
      ),
    closeAllOverlays: () =>
      set(
        produce((s: UiSlice) => {
          (Object.keys(s.overlays) as (keyof OverlayFlags)[]).forEach((k) => {
            s.overlays[k] = false;
          });
        }),
      ),
    setCodeAnchor: (anchor) => {
      if (anchor == null) {
        set({ codeAnchor: null });
        return;
      }
      const nonce = get().anchorNonce + 1;
      set({ codeAnchor: { ...anchor, nonce }, anchorNonce: nonce });
    },
    setCodeMode: (mode) =>
      // a new code-mode is a fresh diff surface — reset the running hunk cursor.
      set({ codeMode: mode, hunkCursor: { index: -1, nonce: 0 } }),
    stepHunkCursor: (anchors, dir) =>
      set((s) => {
        const index = stepHunk(anchors, s.hunkCursor.index, dir);
        // bump the nonce on EVERY step (even when the index repeats — e.g. a
        // single-anchor wrap onto itself — so the view re-scrolls deterministically).
        return { hunkCursor: { index, nonce: s.hunkCursor.nonce + 1 } };
      }),
    resetHunkCursor: () => set({ hunkCursor: { index: -1, nonce: 0 } }),
  };
}
