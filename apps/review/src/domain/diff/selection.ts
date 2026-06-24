// The 2-stage diff selection state machine (pure). The React DiffViewer is a
// thin projection of this reducer, so the selection logic is unit/property/
// mutation-testable without a DOM.
//
// Stage space:
//   - cursor null            : nothing selected
//   - cursor set, anchor null : a single line cursor (stage 1)
//   - cursor + anchor set     : actively selecting a range (stage 2 — paint live)
//   - range set               : a committed range (open the composer here)
//
// `space` advances: null→cursor→anchor→commit. Gutter clicks bypass the cursor
// machine (click = 1-line range, shift-click = extend the end).
//
// LAYER: domain (pure; std-only).

export interface LineRange {
  start: number;
  end: number;
}

export interface Selection {
  /** the keyboard line cursor (a commentable line number) or null. */
  cursor: number | null;
  /** the range anchor — non-null means "actively selecting" (stage 2). */
  anchor: number | null;
  /** a committed range (the composer mounts on it) or null. */
  range: LineRange | null;
}

export function emptySelection(): Selection {
  return { cursor: null, anchor: null, range: null };
}

export function clearSelection(): Selection {
  return emptySelection();
}

/** Normalize two line numbers into a sorted range. */
export function commitRange(a: number, b: number): LineRange {
  return { start: Math.min(a, b), end: Math.max(a, b) };
}

/**
 * Move the line cursor up/down within the ORDERED list of commentable lines
 * (so folded gaps are skipped — the list is the source of adjacency, never
 * `cursor ± 1`). From a null cursor, both directions land on the first line.
 */
export function moveCursor(sel: Selection, dir: "up" | "down", lines: number[]): Selection {
  const first = lines[0];
  if (first == null) return sel;
  if (sel.cursor == null) return { ...sel, cursor: first };
  const idx = lines.indexOf(sel.cursor);
  if (idx < 0) return { ...sel, cursor: first };
  const next = Math.max(0, Math.min(lines.length - 1, idx + (dir === "down" ? 1 : -1)));
  return { ...sel, cursor: lines[next]! };
}

/**
 * The 2-stage `space` advance:
 *   - null cursor → set the cursor to the first commentable line (no range)
 *   - cursor, no anchor → anchor here (begin selecting)
 *   - anchor set → commit the [anchor..cursor] range, clear the anchor
 */
export function pressSpace(sel: Selection, lines: number[]): Selection {
  const first = lines[0];
  if (first == null) return sel;
  if (sel.cursor == null) return { cursor: first, anchor: null, range: null };
  if (sel.anchor == null) return { ...sel, anchor: sel.cursor };
  return { cursor: sel.cursor, anchor: null, range: commitRange(sel.anchor, sel.cursor) };
}

/**
 * Gutter click: a plain click starts a 1-line range; a shift-click extends the
 * existing range's end (or starts a 1-line range when there is none yet).
 */
export function gutterClick(sel: Selection, line: number, shift: boolean): Selection {
  if (shift && sel.range) {
    return { ...sel, range: { start: sel.range.start, end: line } };
  }
  return { ...sel, range: { start: line, end: line } };
}

/**
 * The currently-PAINTED span: the committed range if present, else the live
 * anchor..cursor span while selecting, else null. (Used by the view to tint
 * the selected rows.)
 */
export function selectionRange(sel: Selection): LineRange | null {
  if (sel.range) return commitRange(sel.range.start, sel.range.end);
  if (sel.anchor != null && sel.cursor != null) return commitRange(sel.anchor, sel.cursor);
  return null;
}
