// Entity-views aggregation unit tests (S8 / cute-dbt#500). Covers the pure
// macro / seed / manifest-node (sources + tests) reshapers + the honesty
// invariants (never-a-false-claim):
//   - MACROS read REAL facts from `macro_lens.macros` (signature line, package,
//     path, call-site/impacted-model usages); a macro carries an honest-empty
//     description because the real spine emits none; a context with no
//     `macro_lens` yields an empty list (NOT a fabricated macro);
//   - SEEDS read REAL facts from `seed_cards` (columns, types, row counts, change
//     state, downstream); a context with no `seed_cards` yields an empty list;
//   - the MANIFEST-NODE index reads REAL facts from `manifest_nodes`: a node's
//     materialization + tag set + the per-column TEST inventory (the only test
//     facts the spine carries) — and SOURCES are honestly empty because the
//     fixture carries no `source.*` nodes (never an invented table identity).
import { describe, it, expect } from "vitest";
import {
  buildMacroViews, buildSeedViews, buildManifestIndex, testInventory,
  type MacroView, type SeedView,
} from "./entity-views";
import type { ContextData } from "./context-data";
import { loadFixture } from "../data/fixtures";

const real = loadFixture("context.440") as unknown as ContextData;

function synthCtx(over: Partial<ContextData> = {}): ContextData {
  return { baseline: "main", models: [], ...over } as ContextData;
}

// ── MACROS ──────────────────────────────────────────────────────────────────
describe("buildMacroViews", () => {
  it("reads the REAL macros from macro_lens (4 in the 440 dogfood)", () => {
    const macros = buildMacroViews(real);
    expect(macros.length).toBe(4);
    const names = macros.map((m) => m.name);
    expect(names).toContain("cents_to_dollars");
    expect(names).toContain("incremental_high_water_mark");
  });

  it("carries the REAL signature line + package + path (never fabricated)", () => {
    const m = buildMacroViews(real).find((x) => x.name === "cents_to_dollars")!;
    expect(m.package).toBe("jaffle_shop");
    expect(m.path).toBe("macros/cents_to_dollars.sql");
    expect(m.signature).toContain("cents_to_dollars");
    expect(m.bodyLines.length).toBeGreaterThan(0);
  });

  it("derives the parameter signature from the REAL macro body, not a guess", () => {
    const m = buildMacroViews(real).find((x) => x.name === "incremental_high_water_mark")!;
    // body: `{% macro incremental_high_water_mark(column, floor='1900-01-01') -%}`
    expect(m.signature).toContain("column");
    expect(m.signature).toContain("floor");
  });

  it("surfaces the REAL call-site usages (impacted models) with their paths", () => {
    const m = buildMacroViews(real).find((x) => x.name === "incremental_high_water_mark")!;
    expect(m.impactedCount).toBe(3);
    expect(m.usages.length).toBe(3);
    const u = m.usages.find((x) => x.name === "customer_order_days")!;
    expect(u.path).toBe("models/marts/customer_order_days.sql");
  });

  it("HONEST description: the real spine emits no macro desc → empty string, never invented", () => {
    const m = buildMacroViews(real)[0]!;
    expect(m.description).toBe("");
  });

  it("a macro with no usages reports zero usages + an honest-empty list (no fake caller)", () => {
    const ctx = synthCtx({
      macro_lens: { macros: [{ name: "noop", package: "p", path: "macros/noop.sql", body_lines: [{ kind: "context", text: "{% macro noop() %}{% endmacro %}" }], impacted_count: 0, impacted_models: [] }] },
    } as unknown as Partial<ContextData>);
    const m = buildMacroViews(ctx)[0]!;
    expect(m.impactedCount).toBe(0);
    expect(m.usages).toEqual([]);
  });

  it("no macro_lens ⇒ empty list (honest-empty, not a fabricated macro)", () => {
    expect(buildMacroViews(synthCtx())).toEqual([]);
  });
});

// ── SEEDS ───────────────────────────────────────────────────────────────────
describe("buildSeedViews", () => {
  it("reads the REAL seed cards from seed_cards (1 in the 440 dogfood)", () => {
    const seeds = buildSeedViews(real);
    expect(seeds.length).toBe(1);
    expect(seeds[0]!.name).toBe("raw_payments");
  });

  it("carries the REAL columns, types, row count + downstream feeds (never fabricated)", () => {
    const s = buildSeedViews(real).find((x) => x.name === "raw_payments")!;
    expect(s.columns).toEqual(["id", "order_id", "payment_method", "amount"]);
    expect(s.colTypes.amount).toBe("integer");
    expect(s.file).toBe("seeds/raw_payments.csv");
    expect(s.downstream).toContain("stg_payments");
    expect(s.rowCount).toBeGreaterThan(0);
  });

  it("reports the REAL change state from the seed card diff (base | modified)", () => {
    const s = buildSeedViews(real).find((x) => x.name === "raw_payments")!;
    expect(["base", "modified"]).toContain(s.change);
  });

  it("no seed_cards ⇒ empty list (honest-empty, not a fake card)", () => {
    expect(buildSeedViews(synthCtx())).toEqual([]);
  });
});

// ── MANIFEST-NODE INDEX (sources + tests) ───────────────────────────────────
describe("buildManifestIndex", () => {
  it("reads the REAL manifest nodes (materialization + tags) from manifest_nodes", () => {
    const idx = buildManifestIndex(real);
    expect(idx.nodes.length).toBe(14);
    const cod = idx.nodes.find((n) => n.name === "customer_order_days")!;
    expect(cod.materialized).toBe("incremental");
    expect(cod.tags).toContain("marts");
  });

  it("SOURCES are honestly empty — the 440 context carries no source.* nodes", () => {
    const idx = buildManifestIndex(real);
    // never a fabricated table identity: the fixture has no discrete source node.
    expect(idx.sources).toEqual([]);
  });

  it("no manifest_nodes ⇒ empty index (honest-empty)", () => {
    const idx = buildManifestIndex(synthCtx());
    expect(idx.nodes).toEqual([]);
    expect(idx.sources).toEqual([]);
  });
});

describe("testInventory", () => {
  it("aggregates the REAL per-column tests from manifest_nodes (28 in the 440 dogfood)", () => {
    const inv = testInventory(real);
    expect(inv.total).toBe(28);
    // a real (model, column, test) triple is present — never invented.
    const notNull = inv.entries.find(
      (e) => e.model === "customers" && e.column === "customer_id" && e.test === "unique",
    );
    expect(notNull).toBeTruthy();
  });

  it("groups tests by kind with REAL counts (unique / not null …)", () => {
    const inv = testInventory(real);
    expect(inv.byKind["not null"]).toBeGreaterThan(0);
    expect(inv.byKind["unique"]).toBeGreaterThan(0);
  });

  it("no manifest_nodes ⇒ zero tests (honest-empty, never a fabricated test)", () => {
    const inv = testInventory(synthCtx());
    expect(inv.total).toBe(0);
    expect(inv.entries).toEqual([]);
  });
});

// ── never-a-false-claim type narrowing (the honest-empty contract) ──────────
describe("honest-empty contract", () => {
  it("every macro view is fully-typed REAL data (no undefined leaking as a fact)", () => {
    for (const m of buildMacroViews(real) as MacroView[]) {
      expect(typeof m.name).toBe("string");
      expect(typeof m.signature).toBe("string");
      expect(Array.isArray(m.usages)).toBe(true);
    }
  });
  it("every seed view is fully-typed REAL data", () => {
    for (const s of buildSeedViews(real) as SeedView[]) {
      expect(typeof s.name).toBe("string");
      expect(Array.isArray(s.columns)).toBe(true);
      expect(Array.isArray(s.downstream)).toBe(true);
    }
  });
});
