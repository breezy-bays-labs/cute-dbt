// The 2-stage diff selection state machine (pure). Stage 1 = a LINE cursor;
// stage 2 = a RANGE anchored from the cursor. `space` advances the stages:
//   - cursor null  → set cursor to the first commentable line (no range yet)
//   - cursor set, no anchor → anchor here (begin selecting)
//   - anchor set   → commit the [anchor..cursor] range (open the composer)
// Gutter range-select (click a line, shift-click another) and the openAnchor
// nonce (an external "jump here + flash" request) are modeled here so the React
// layer is a thin projection of this reducer.
import { describe, it, expect } from "vitest";
import fc from "fast-check";
import {
  emptySelection,
  moveCursor,
  pressSpace,
  gutterClick,
  commitRange,
  clearSelection,
  selectionRange,
  type Selection,
} from "./selection";

const LINES = [1, 2, 3, 4, 5];

describe("emptySelection", () => {
  it("starts with no cursor, no anchor, no range", () => {
    const s = emptySelection();
    expect(s.cursor).toBeNull();
    expect(s.anchor).toBeNull();
    expect(s.range).toBeNull();
  });
});

describe("moveCursor", () => {
  it("from null, down lands on the first commentable line", () => {
    const s = moveCursor(emptySelection(), "down", LINES);
    expect(s.cursor).toBe(1);
  });
  it("from null, up lands on the first commentable line too", () => {
    const s = moveCursor(emptySelection(), "up", LINES);
    expect(s.cursor).toBe(1);
  });
  it("clamps at the ends", () => {
    let s: Selection = { cursor: 5, anchor: null, range: null };
    s = moveCursor(s, "down", LINES);
    expect(s.cursor).toBe(5);
    s = { cursor: 1, anchor: null, range: null };
    s = moveCursor(s, "up", LINES);
    expect(s.cursor).toBe(1);
  });
  it("skips a non-commentable line number (folded gap) by using the ordered list", () => {
    // commentable lines are 1,4,5 (2,3 folded); from 1 down → 4, not 2.
    const folded = [1, 4, 5];
    const s = moveCursor({ cursor: 1, anchor: null, range: null }, "down", folded);
    expect(s.cursor).toBe(4);
  });
});

describe("pressSpace — the 2-stage advance", () => {
  it("stage 0 → stage 1: null cursor sets the cursor (no range yet)", () => {
    const s = pressSpace(emptySelection(), LINES);
    expect(s.cursor).toBe(1);
    expect(s.anchor).toBeNull();
    expect(s.range).toBeNull();
  });
  it("stage 1 → stage 2: a cursor without an anchor anchors here", () => {
    const s = pressSpace({ cursor: 3, anchor: null, range: null }, LINES);
    expect(s.anchor).toBe(3);
    expect(s.range).toBeNull();
  });
  it("stage 2 → commit: an anchored cursor commits the range (sorted)", () => {
    // anchor at 4, cursor moved up to 2 → range {start:2,end:4}
    const s = pressSpace({ cursor: 2, anchor: 4, range: null }, LINES);
    expect(s.range).toEqual({ start: 2, end: 4 });
    expect(s.anchor).toBeNull(); // anchor cleared on commit
  });
  it("a single-line selection commits start===end", () => {
    const s = pressSpace({ cursor: 3, anchor: 3, range: null }, LINES);
    expect(s.range).toEqual({ start: 3, end: 3 });
  });
});

describe("gutterClick + commitRange", () => {
  it("a plain click starts a 1-line range", () => {
    const s = gutterClick(emptySelection(), 2, false);
    expect(s.range).toEqual({ start: 2, end: 2 });
  });
  it("a shift-click extends the existing range's end", () => {
    let s = gutterClick(emptySelection(), 2, false);
    s = gutterClick(s, 5, true);
    expect(s.range).toEqual({ start: 2, end: 5 });
  });
  it("a shift-click with no prior range behaves like a plain click", () => {
    const s = gutterClick(emptySelection(), 4, true);
    expect(s.range).toEqual({ start: 4, end: 4 });
  });
  it("commitRange normalizes start/end ordering", () => {
    expect(commitRange(5, 2)).toEqual({ start: 2, end: 5 });
    expect(commitRange(2, 5)).toEqual({ start: 2, end: 5 });
  });
});

describe("selectionRange (the painted span)", () => {
  it("is the committed range when present", () => {
    expect(selectionRange({ cursor: null, anchor: null, range: { start: 2, end: 4 } })).toEqual({ start: 2, end: 4 });
  });
  it("is the live anchor..cursor span while selecting", () => {
    expect(selectionRange({ cursor: 2, anchor: 5, range: null })).toEqual({ start: 2, end: 5 });
  });
  it("is null with neither", () => {
    expect(selectionRange({ cursor: 3, anchor: null, range: null })).toBeNull();
  });
});

describe("clearSelection", () => {
  it("resets every field", () => {
    expect(clearSelection()).toEqual({ cursor: null, anchor: null, range: null });
  });
});

describe("PROPERTY: selectionRange is always sorted + in-bounds", () => {
  it("start <= end whenever a range exists", () => {
    fc.assert(
      fc.property(
        fc.option(fc.integer({ min: 1, max: 100 }), { nil: null }),
        fc.option(fc.integer({ min: 1, max: 100 }), { nil: null }),
        (cursor, anchor) => {
          const r = selectionRange({ cursor, anchor, range: null });
          if (r) expect(r.start).toBeLessThanOrEqual(r.end);
        },
      ),
    );
  });
});
