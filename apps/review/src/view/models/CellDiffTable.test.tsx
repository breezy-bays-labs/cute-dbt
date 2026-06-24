// CellDiffTable static-render tests (S7). The cell-level given/expect diff grid:
// old→new per changed cell, +/− rows, and the NULL / absent / empty trichotomy
// rendered EXPLICITLY (never a blank that hides which state a cell is in). Node
// env → render to static markup and assert the synchronous structure. The
// trichotomy itself is the 100%-kill fold in domain/data/cell-diff.ts; this test
// pins the VIEW renders each state distinctly.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { CellDiffTable } from "./CellDiffTable";
import type { NormDiffTable } from "../../domain/data/cell-diff";

const render = (table: NormDiffTable | null, mode: "diff" | "file" = "diff"): string =>
  renderToStaticMarkup(<CellDiffTable table={table} mode={mode} />);

const tbl = (rows: NormDiffTable["rows"], cols = ["a", "b"]): NormDiffTable => ({
  columns: cols.map((name) => ({ name, status: "present" })),
  rows,
});

describe("CellDiffTable — honest-empty", () => {
  it("renders an honest empty-table note (never a fabricated row) for null", () => {
    const html = render(null);
    expect(html).toContain('data-testid="cell-diff-empty"');
    expect(html).not.toContain('data-testid="cell-diff-row"');
  });
  it("renders the empty note for a columnless table", () => {
    expect(render({ columns: [], rows: [] })).toContain('data-testid="cell-diff-empty"');
  });
});

describe("CellDiffTable — the cell trichotomy is rendered distinctly", () => {
  const row: NormDiffTable["rows"][number] = {
    kind: "modified",
    cells: [
      { old: { text: "1", null: false, absent: false }, new: { text: "2", null: false, absent: false }, changed: true },
      { old: { text: "", null: false, absent: true }, new: { text: "NULL", null: true, absent: false }, changed: true },
    ],
  };

  it("renders a changed cell as old→new (both sides visible)", () => {
    const html = render(tbl([row]));
    expect(html).toContain('data-testid="cell-diff-row"');
    expect(html).toContain("1"); // old
    expect(html).toContain("2"); // new
    expect(html).toContain("→");
  });

  it("renders a SQL NULL as an explicit NULL token (never a blank cell)", () => {
    expect(render(tbl([row]))).toContain("NULL");
  });

  it("renders an absent cell as the explicit absent glyph (·), distinct from NULL/empty", () => {
    expect(render(tbl([row]))).toContain("·");
  });

  it("renders an empty-but-present value as the ∅ glyph (distinct from absent)", () => {
    const r: NormDiffTable["rows"][number] = {
      kind: "added",
      cells: [{ old: { text: "", null: false, absent: true }, new: { text: "", null: false, absent: false }, changed: true }],
    };
    expect(render(tbl([r], ["a"]))).toContain("∅");
  });
});

describe("CellDiffTable — row marks", () => {
  it("marks added / removed rows with their sigils", () => {
    const added: NormDiffTable["rows"][number] = { kind: "added", cells: [{ old: { text: "", null: false, absent: true }, new: { text: "x", null: false, absent: false }, changed: true }] };
    const removed: NormDiffTable["rows"][number] = { kind: "removed", cells: [{ old: { text: "y", null: false, absent: false }, new: { text: "", null: false, absent: true }, changed: true }] };
    const html = render(tbl([added, removed], ["a"]));
    expect(html).toContain("+");
    expect(html).toContain("−");
  });
});

describe("CellDiffTable — file mode shows the new state only", () => {
  it("renders the new value without the old→new arrow in file mode", () => {
    const row: NormDiffTable["rows"][number] = {
      kind: "modified",
      cells: [{ old: { text: "1", null: false, absent: false }, new: { text: "2", null: false, absent: false }, changed: true }],
    };
    const html = render(tbl([row], ["a"]), "file");
    expect(html).toContain("2");
    expect(html).not.toContain("→");
  });
});
