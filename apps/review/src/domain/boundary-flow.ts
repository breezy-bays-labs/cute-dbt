// Cross-file boundary flow (S6b) — the PURE index-arithmetic the topology shelf's
// cross-file navigation runs when a cursor flows OFF the top/bottom edge of one
// file's diff into the next. Lifted out of the prototype topology.js `onBoundary`
// closure so the "skip empty files" rule is a unit-testable fold (the DOM
// focus/scroll the index drives stays in the view; the WHICH-file decision is
// pure here).
//
// The rule: a `block`/`hunk` boundary steps to the IMMEDIATE neighbor file; a
// `comment` boundary SKIPS files that have no comments (the empty-comment files),
// landing on the next file that does. Out-of-range returns null (no wrap — the
// edge is a hard stop, matching the prototype).
//
// LAYER: domain (pure; std only).

/** Which direction the cursor flowed off the current file's edge. */
export type FlowDir = "down" | "up";
/** What kind of boundary navigation triggered the flow. */
export type FlowKind = "block" | "hunk" | "comment";

/**
 * nextFileOnBoundary — the index of the file to flow into, or null at the edge.
 *
 *   from       the current file index
 *   dir        "down" (→ next, +1) or "up" (→ prev, −1)
 *   kind       "comment" skips empty-comment files; "block"/"hunk" step one
 *   total      the file count
 *   hasComments(i) → does file i carry at least one comment thread?
 *
 * For a `comment` flow, scan in `dir` from the immediate neighbor for the first
 * file with comments. For a `block`/`hunk` flow, the immediate neighbor is the
 * target (no skipping). Returns null when the scan runs off either end.
 */
export function nextFileOnBoundary(from: number, dir: FlowDir, kind: FlowKind, total: number, hasComments: (i: number) => boolean): number | null {
  const step = dir === "down" ? 1 : -1;
  let i = from + step;
  if (kind === "comment") {
    while (i >= 0 && i < total) {
      if (hasComments(i)) return i;
      i += step;
    }
    return null; // no later file has comments → hard stop
  }
  // block / hunk → the immediate neighbor (if in range).
  return i >= 0 && i < total ? i : null;
}
