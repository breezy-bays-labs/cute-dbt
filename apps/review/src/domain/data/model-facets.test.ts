// modelFacets — the per-model facet aggregation the Models · Details + Models ·
// Unit-tests views read. Pure fold over a built ModelRecord: it surfaces WHICH
// facets the payload actually carries (config / tests / external-fixture /
// coverage / downstream / columns), so each view can render an honest-empty
// state for any facet the model lacks (never-a-false-claim). The aggregation
// NEVER fabricates: a missing facet is `false`/empty, never a placeholder fact.
import { describe, it, expect } from "vitest";
import { buildDataset } from "./dataset";
import { loadFixture } from "../../data/fixtures";
import { modelFacets } from "./model-facets";
import type { ContextData } from "../context-data";

const ds = buildDataset(loadFixture("context.440") as unknown as ContextData);

describe("modelFacets — honest per-model facet aggregation", () => {
  it("reports unit tests present + the changed-test count for a model that has them", () => {
    const f = modelFacets(ds.D["orders"]!);
    expect(f.hasTests).toBe(true);
    expect(f.testCount).toBe(2);
    // orders' tests are both changed in the dogfood PR.
    expect(f.changedTestCount).toBeGreaterThan(0);
    expect(f.changedTestCount).toBeLessThanOrEqual(f.testCount);
  });

  it("flags an external-fixture reference when a given points at a CSV file (no fabricated grid)", () => {
    const f = modelFacets(ds.D["orders"]!);
    // the orders test references tests/fixtures/stg_payments_credit_card.csv.
    expect(f.hasExternalFixture).toBe(true);
  });

  it("reports honest-empty test facets for a model that ships NO unit test", () => {
    const f = modelFacets(ds.D["stg_customers"]!);
    expect(f.hasTests).toBe(false);
    expect(f.testCount).toBe(0);
    expect(f.changedTestCount).toBe(0);
    expect(f.hasExternalFixture).toBe(false);
  });

  it("surfaces the config facts the payload carries (materialization is always known)", () => {
    const f = modelFacets(ds.D["customers"]!);
    // every model has a materialization (derived: incremental flag → yaml → view).
    expect(f.materialized.length).toBeGreaterThan(0);
    // customers is non-incremental in the dogfood.
    expect(f.isIncremental).toBe(false);
  });

  it("flags the incremental facet for an incremental model", () => {
    const f = modelFacets(ds.D["customer_order_days"]!);
    expect(f.isIncremental).toBe(true);
    expect(f.materialized).toBe("incremental");
  });

  it("reports coverage-check presence honestly (counts mirror the record)", () => {
    const f = modelFacets(ds.D["orders"]!);
    const cov = ds.D["orders"]!.coverage;
    expect(f.coverageCount).toBe(cov.checks.length);
    expect(f.hasCoverage).toBe(cov.checks.length > 0);
  });

  it("reports the model change-state verbatim from the record (never recomputed)", () => {
    // the record's `change` is the spine's verbatim value: stateToChange maps the
    // dbt ModelState onto added/removed/modified (`new`→added, `deleted`→removed,
    // else→modified). We assert the RECORD's value, never a recomputed one.
    for (const name of ["order_status_pivot", "legacy_order_rollup", "customers"]) {
      expect(modelFacets(ds.D[name]!).change).toBe(ds.D[name]!.change);
    }
    expect(modelFacets(ds.D["legacy_order_rollup"]!).change).toBe("removed");
    expect(modelFacets(ds.D["customers"]!).change).toBe("modified");
  });
});
