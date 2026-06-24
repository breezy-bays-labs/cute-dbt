// The running hunk-cursor — the S5 next/prev-hunk deferral V1 OWNS.
//
// S5's PierreDiff exposed a STATELESS jump (next→first, prev→last); the keyboard
// slice (this slice, V1) owns the running CURSOR that steps one change-run anchor
// at a time, wrapping at the ends. This is the pure reducer; PierreDiff is a thin
// shell that resolves the cursor index to an anchor and scrolls to its data-line.
//
// LAYER: domain (pure; std-only). No DOM, no store.

import type { NavStart } from "./patch-nav";

/** The store-facing cursor: the current anchor index + a step nonce. A bump of
 *  the nonce forces a re-scroll even when the index repeats (the same re-anchor
 *  discipline as the codeAnchor nonce). `index === -1` is the unset cursor. */
export interface HunkCursor {
  index: number;
  nonce: number;
}

/**
 * Step the cursor over the ordered `anchors` by `dir` (+1 next / -1 prev),
 * wrapping at the ends. An UNSET cursor (-1) or an OUT-OF-RANGE stored index
 * (e.g. the anchor list shrank) starts fresh: forward → the first anchor (0),
 * backward → the last. An EMPTY anchor list is an honest no-op (returns -1 —
 * there is nothing to move to). Pure: (anchors, current, dir) → next index.
 */
export function stepHunk(anchors: readonly NavStart[], current: number, dir: 1 | -1): number {
  const n = anchors.length;
  if (n === 0) return -1;
  const valid = current >= 0 && current < n;
  if (!valid) return dir > 0 ? 0 : n - 1;
  return (((current + dir) % n) + n) % n;
}

/** Resolve a cursor index to its anchor, or null when unset/out-of-range/empty. */
export function anchorAt(anchors: readonly NavStart[], index: number): NavStart | null {
  if (index < 0 || index >= anchors.length) return null;
  return anchors[index] ?? null;
}
