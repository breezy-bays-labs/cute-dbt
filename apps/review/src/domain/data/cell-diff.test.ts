// Cell-trichotomy unit tests — the 100%-mutation-kill target (the council strict
// tier: 100% Stryker kill on the cell `key.t` trichotomy). Every branch of
// diffSide / cellSide / adaptDiffTable / allAdded is pinned, plus the
// load-bearing DISTINCTNESS invariant: NULL ≠ absent ≠ "" (empty-but-present).
import { describe, expect, it } from "vitest";
import type { Cell, DiffTable } from "../context-data";
import { adaptDiffTable, allAdded, cellSide, diffSide } from "./cell-diff";

describe("diffSide — the NULL/absent/empty trichotomy", () => {
  it("absent cell (key.t=absent) ⇒ absent, not null, empty text", () => {
    expect(diffSide({ key: { t: "absent" } })).toEqual({ text: "", null: false, absent: true });
  });
  it("null cell (key.t=null) ⇒ null, not absent, text NULL", () => {
    expect(diffSide({ key: { t: "null" } })).toEqual({ text: "NULL", null: true, absent: false });
  });
  it("a number cell ⇒ present (not null, not absent), display text", () => {
    expect(diffSide({ display: "42", key: { t: "number", v: "42" } })).toEqual({ text: "42", null: false, absent: false });
  });
  it("a str cell ⇒ present, display text", () => {
    expect(diffSide({ display: "Ada", key: { t: "str", v: "Ada" } })).toEqual({ text: "Ada", null: false, absent: false });
  });
  it("a missing side (undefined) ⇒ absent", () => {
    expect(diffSide(undefined)).toEqual({ text: "", null: false, absent: true });
    expect(diffSide(null)).toEqual({ text: "", null: false, absent: true });
  });
  it("THE DISTINCTNESS INVARIANT: empty-string present ≠ NULL ≠ absent", () => {
    const emptyButPresent = diffSide({ display: "", key: { t: "str", v: "" } });
    const sqlNull = diffSide({ key: { t: "null" } });
    const missing = diffSide({ key: { t: "absent" } });
    // empty-but-present: empty text, but NOT null and NOT absent.
    expect(emptyButPresent).toEqual({ text: "", null: false, absent: false });
    // all three are mutually distinct (never collapse).
    expect(emptyButPresent).not.toEqual(sqlNull);
    expect(emptyButPresent).not.toEqual(missing);
    expect(sqlNull).not.toEqual(missing);
  });
  it("a cell with a display but no key ⇒ present (keyless typed value)", () => {
    expect(diffSide({ display: "x" } as Cell)).toEqual({ text: "x", null: false, absent: false });
  });
  it("the `!s` falsy guard: a truthy keyless cell is PRESENT, a nullish one is ABSENT (kills the !s flip)", () => {
    // a truthy object (no key) must NOT be treated as absent — only a nullish side is.
    expect(diffSide({ display: "kept" }).absent).toBe(false);
    expect(diffSide({}).absent).toBe(false); // truthy empty cell ⇒ present (empty), not absent
    expect(diffSide(undefined).absent).toBe(true);
    expect(diffSide(null).absent).toBe(true);
  });
  it("a cell with key but no display ⇒ present empty text (display null-coalesced)", () => {
    expect(diffSide({ key: { t: "str", v: "" } } as Cell)).toEqual({ text: "", null: false, absent: false });
  });
});

describe("cellSide — the display fold (seed/flat cells)", () => {
  it("absent ⇒ empty string", () => { expect(cellSide({ key: { t: "absent" } })).toBe(""); });
  it("missing ⇒ empty string", () => { expect(cellSide(undefined)).toBe(""); });
  it("null ⇒ the chosen null text (default (null))", () => {
    expect(cellSide({ key: { t: "null" } })).toBe("(null)");
    expect(cellSide({ key: { t: "null" } }, "")).toBe("");
    expect(cellSide({ key: { t: "null" } }, "NULL")).toBe("NULL");
  });
  it("present ⇒ the display string, NOT a NULL sentinel", () => {
    expect(cellSide({ display: "7", key: { t: "number", v: "7" } })).toBe("7");
    expect(cellSide({ display: "", key: { t: "str", v: "" } })).toBe(""); // empty-but-present
  });
});

describe("adaptDiffTable — cell-diff normalization", () => {
  const tbl: DiffTable = {
    columns: [{ name: "id", status: "present" }, { name: "name" }],
    rows: [
      { kind: "modified", cells: [
        { old: { display: "1", key: { t: "number", v: "1" } }, new: { display: "2", key: { t: "number", v: "2" } }, changed: true },
        { old: { key: { t: "absent" } }, new: { key: { t: "null" } }, changed: true },
      ] },
      { cells: [{ old: { display: "x", key: { t: "str", v: "x" } }, new: { display: "x", key: { t: "str", v: "x" } } }] },
    ],
  };
  it("returns null on an absent table (honest empty)", () => {
    expect(adaptDiffTable(null)).toBeNull();
    expect(adaptDiffTable(undefined)).toBeNull();
  });
  it("defaults a column status to present", () => {
    expect(adaptDiffTable(tbl)!.columns).toEqual([{ name: "id", status: "present" }, { name: "name", status: "present" }]);
  });
  it("defaults a row kind to modified", () => {
    expect(adaptDiffTable(tbl)!.rows[1]!.kind).toBe("modified");
  });
  it("runs each side through diffSide AND preserves `changed` verbatim", () => {
    const r0 = adaptDiffTable(tbl)!.rows[0]!;
    expect(r0.cells[0]).toEqual({ old: { text: "1", null: false, absent: false }, new: { text: "2", null: false, absent: false }, changed: true });
    expect(r0.cells[1]).toEqual({ old: { text: "", null: false, absent: true }, new: { text: "NULL", null: true, absent: false }, changed: true });
    // unchanged row: changed defaults to false.
    expect(adaptDiffTable(tbl)!.rows[1]!.cells[0]!.changed).toBe(false);
  });
  it("tolerates empty columns/rows", () => {
    expect(adaptDiffTable({ columns: [], rows: [] })).toEqual({ columns: [], rows: [] });
  });
  it("tolerates a table missing columns/rows arrays (the ?? [] fallback)", () => {
    // a malformed table with absent arrays must degrade to empty, not throw —
    // pins the `(tbl.columns ?? [])` / `(tbl.rows ?? [])` / `(r.cells ?? [])` guards.
    expect(adaptDiffTable({} as DiffTable)).toEqual({ columns: [], rows: [] });
    expect(adaptDiffTable({ columns: [{ name: "c" }] } as DiffTable)!.rows).toEqual([]);
    const noCells = adaptDiffTable({ columns: [], rows: [{ kind: "added" } as never] })!;
    expect(noCells.rows[0]!.cells).toEqual([]);
  });
});

describe("allAdded — the brand-new predicate", () => {
  const norm = (cols: string[], rowKinds: string[]) => ({
    columns: cols.map((name) => ({ name, status: "added" })),
    rows: rowKinds.map((kind) => ({ kind, cells: [] })),
  });
  it("true when every column added + every row added", () => {
    expect(allAdded(norm(["a", "b"], ["added", "added"]))).toBe(true);
  });
  it("false when ANY column isn't added (every, not some — 2 cols, one flipped)", () => {
    const t = norm(["a", "b"], ["added"]); t.columns[1]!.status = "present";
    // `some` would be true here (col a is added); `allAdded` must use `every` ⇒ false.
    expect(allAdded(t)).toBe(false);
  });
  it("false when ANY row isn't added (every, not some — 2 rows, one flipped)", () => {
    const t = norm(["a"], ["added", "added"]); t.rows[1]!.kind = "modified";
    expect(allAdded(t)).toBe(false);
  });
  it("false on an empty (no-columns) table — honest: nothing to assert", () => {
    expect(allAdded(norm([], []))).toBe(false);
    expect(allAdded(null)).toBe(false);
    expect(allAdded(undefined)).toBe(false);
  });
});
