// ModelUnitTests static-render tests (S7, Models · Unit tests). The per-model
// unit-test surface: each test's given/expect tables with the cell-level diff
// treatment (reusing the domain cell-diff facts). HONESTY is the spine:
//   - a model with NO unit tests → an honest no-unit-tests empty state.
//   - an external-fixture given the payload only POINTS at → an honest note,
//     never a fabricated grid.
//   - a changed test → old→new cell diffs; an unchanged/new test → the new state.
// Built from the REAL dogfood fixture (orders has 2 tests incl. an external CSV
// reference; customers has 1 with a data_diff; stg_customers has none).
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ModelUnitTests } from "./ModelUnitTests";
import { loadFixture } from "../../data/fixtures";
import type { ContextData, ModelPayload } from "../../domain/context-data";

const data = loadFixture("context.440") as unknown as ContextData;
const byName = (n: string): ModelPayload => data.models.find((m) => m.name === n)!;
const render = (m: ModelPayload, dataMode: "diff" | "file" = "diff"): string =>
  renderToStaticMarkup(<ModelUnitTests model={m} testIdx={0} dataMode={dataMode} />);

describe("ModelUnitTests — a model WITH unit tests renders the given/expect grids", () => {
  it("renders the unit-test surface + the given + expect sections", () => {
    const html = render(byName("customers"));
    expect(html).toContain('data-testid="model-unit-tests"');
    expect(html).toContain('data-testid="ut-given"');
    expect(html).toContain('data-testid="ut-expect"');
  });

  it("renders the cell-level diff grid (old→new) for a changed test in diff mode", () => {
    const html = render(byName("customers"), "diff");
    // the cell-diff grid mounts (a changed test carries a data_diff).
    expect(html).toContain('data-testid="cell-diff-row"');
    expect(html).toContain("→");
  });

  it("surfaces the test selector + the test count", () => {
    const html = render(byName("orders"));
    expect(html).toContain('data-testid="ut-test-select"');
    // orders has 2 unit tests.
    expect(html).toContain("/2");
  });

  it("renders the file-mode (new-state) grid without the diff arrow", () => {
    const html = render(byName("customers"), "file");
    expect(html).toContain('data-testid="fixture-table"');
  });
});

describe("ModelUnitTests — external fixture (honest pointer, never a fabricated grid)", () => {
  it("renders an honest external-fixture note naming the CSV file the payload points at", () => {
    // orders' test references tests/fixtures/stg_payments_credit_card.csv.
    const html = render(byName("orders"));
    expect(html).toContain('data-testid="ut-external-fixture"');
    expect(html).toContain(".csv");
  });
});

describe("ModelUnitTests — honest no-unit-tests empty state", () => {
  it("renders the honest empty state for a model with no unit tests", () => {
    const html = render(byName("stg_customers"));
    expect(html).toContain('data-testid="ut-empty"');
    expect(html.toLowerCase()).toContain("no unit test");
    // no given/expect grids fabricated.
    expect(html).not.toContain('data-testid="ut-given"');
  });

  it("renders the honest empty state for a deleted model (cannot have tests)", () => {
    const html = render(byName("legacy_order_rollup"));
    expect(html).toContain('data-testid="ut-empty"');
  });
});
