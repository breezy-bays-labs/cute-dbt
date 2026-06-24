// The running hunk-cursor — the S5 next/prev-hunk deferral V1 OWNS. S5's
// PierreDiff nav landed on the FIRST/LAST anchor (stateless); V1 wires a real
// STEPPING cursor over the ordered change-run anchors. This is the pure reducer:
// (anchors, cursor, dir) → next index, clamped/wrapping, with a "no anchors"
// honest no-op. The view (PierreDiff) is a thin shell that scrolls to the
// resolved anchor's data-line.
//
// LAYER: domain (pure; std-only).
import { describe, it, expect } from "vitest";
import { stepHunk, anchorAt, type HunkCursor } from "./hunk-cursor";
import type { NavStart } from "./patch-nav";

const ANCHORS: NavStart[] = [
  { no: 3, side: "additions" },
  { no: 8, side: "deletions" },
  { no: 14, side: "additions" },
];

describe("stepHunk — the running cursor over change-run anchors", () => {
  it("from the unset cursor (-1), the FIRST forward step lands on index 0", () => {
    expect(stepHunk(ANCHORS, -1, 1)).toBe(0);
  });
  it("from the unset cursor (-1), the FIRST backward step lands on the LAST index", () => {
    expect(stepHunk(ANCHORS, -1, -1)).toBe(ANCHORS.length - 1);
  });
  it("forward steps advance one anchor at a time", () => {
    expect(stepHunk(ANCHORS, 0, 1)).toBe(1);
    expect(stepHunk(ANCHORS, 1, 1)).toBe(2);
  });
  it("forward past the end WRAPS to the first", () => {
    expect(stepHunk(ANCHORS, 2, 1)).toBe(0);
  });
  it("backward steps retreat one anchor at a time + wrap", () => {
    expect(stepHunk(ANCHORS, 2, -1)).toBe(1);
    expect(stepHunk(ANCHORS, 0, -1)).toBe(2); // wrap
  });
  it("an EMPTY anchor list is an honest no-op (-1, no cursor to move)", () => {
    expect(stepHunk([], -1, 1)).toBe(-1);
    expect(stepHunk([], 0, -1)).toBe(-1);
  });
  it("a single-anchor list always lands on index 0 (forward AND backward)", () => {
    const one: NavStart[] = [{ no: 5, side: "additions" }];
    expect(stepHunk(one, -1, 1)).toBe(0);
    expect(stepHunk(one, 0, 1)).toBe(0); // wrap onto itself
    expect(stepHunk(one, 0, -1)).toBe(0);
  });
  it("an out-of-range stored cursor is treated as unset (defensive)", () => {
    // a stale cursor (e.g. anchors shrank) → start fresh from the front.
    expect(stepHunk(ANCHORS, 99, 1)).toBe(0);
    expect(stepHunk(ANCHORS, 99, -1)).toBe(ANCHORS.length - 1);
  });
});

describe("anchorAt — resolve the cursor index to its anchor", () => {
  it("returns the anchor at a valid index", () => {
    expect(anchorAt(ANCHORS, 1)).toEqual({ no: 8, side: "deletions" });
  });
  it("returns null for the unset cursor (-1) and out-of-range", () => {
    expect(anchorAt(ANCHORS, -1)).toBeNull();
    expect(anchorAt(ANCHORS, 99)).toBeNull();
    expect(anchorAt([], 0)).toBeNull();
  });
});

describe("HunkCursor — the store-facing state shape", () => {
  it("a cursor carries an index + a step nonce (bump forces a re-scroll)", () => {
    const c: HunkCursor = { index: 0, nonce: 1 };
    expect(c.index).toBe(0);
    expect(c.nonce).toBe(1);
  });
});
