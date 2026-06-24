// FixtureTable static-render tests (S7). The plain given/expect grid (file mode,
// no diff): a columns header + one row per fixture row. Honest-empty when a
// fixture carries no columns/rows. Used for the new-state ("file") view of a
// unit test's given inputs + the expect table.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { FixtureTable } from "./FixtureTable";

const render = (columns: string[], rows: (string | undefined)[][]): string =>
  renderToStaticMarkup(<FixtureTable columns={columns} rows={rows} />);

describe("FixtureTable", () => {
  it("renders a header per column + a row per fixture row", () => {
    const html = render(["id", "name"], [["1", "a"], ["2", "b"]]);
    expect(html).toContain('data-testid="fixture-table"');
    expect(html).toContain("id");
    expect(html).toContain("name");
    expect(html.match(/data-testid="fixture-row"/g)?.length).toBe(2);
  });

  it("renders an honest-empty note when there are no columns", () => {
    const html = render([], []);
    expect(html).toContain('data-testid="fixture-empty"');
    expect(html).not.toContain('data-testid="fixture-row"');
  });

  it("renders an absent cell as an explicit em-dash (never a silent blank)", () => {
    const html = render(["a"], [[undefined]]);
    expect(html).toContain("—");
  });
});
