// dataset orchestrator tests — buildDataset (WeakMap memo) + the per-source
// reshape + the honesty invariants on the real 16-model spine. Plus the smaller
// pure folds (findingsToCoverage, prDagToScope, deriveInfo, threadsByPath).
import { describe, expect, it } from "vitest";
import { loadFixture, rawFixture } from "../../data/fixtures";
import { parseContext } from "../schema";
import type { ContextData, FindingPayload } from "../context-data";
import {
  availableScopeAxes, blockDiffToHunks, blockDiffToPatch, buildDataset, buildSeedRecords, dagToCte, findingsToCoverage,
  pickScopeAxis, pierreSide, prDagToScope, schemaOf, scopeToGraph, sourceToContextPatch, stateToChange, threadsByPath,
  type PrScope,
} from "./dataset";

const ctx440 = (): ContextData => loadFixture("context.440") as unknown as ContextData;

describe("buildDataset — WeakMap memoization + per-source reshape", () => {
  it("is memoized on the ContextData identity (same object ⇒ same dataset)", () => {
    const data = ctx440();
    expect(buildDataset(data)).toBe(buildDataset(data));
  });
  it("a DISTINCT object identity rebuilds (not the same reference)", () => {
    const a = ctx440();
    // a fresh parse ⇒ a NEW object identity ⇒ the WeakMap memo misses ⇒ a rebuild.
    const b = parseContext(rawFixture("context.440")) as unknown as ContextData;
    expect(a).not.toBe(b);
    expect(buildDataset(a)).not.toBe(buildDataset(b));
  });
  it("surfaces every model + a record per model", () => {
    const ds = buildDataset(ctx440());
    expect(ds.MODELS.length).toBeGreaterThanOrEqual(16);
    ds.MODELS.forEach((n) => expect(ds.D[n]).toBeDefined());
  });
});

describe("prSelectableModels — the MODEL-ONLY review scope (cute-dbt#495 fix)", () => {
  // prSelectable carries every non-connector/non-halo PR-scope node — INCLUDING
  // seeds + macros (the dogfood fixture has `raw_payments` seed + `cents_to_dollars`
  // macro). The Models review LOOP must walk MODELS only: a seed/macro id has no
  // record in D, so advancing onto it shows the WRONG model's diff while marking
  // the seed/macro reviewed (the never-a-false-claim violation). prSelectableModels
  // is prSelectable filtered to ids that ARE models (own keys of D).
  it("excludes the seed + macro that prSelectable carries (real fixture)", () => {
    const ds = buildDataset(ctx440());
    // the contaminated source still carries the non-models (we don't mutate it).
    expect(ds.prSelectable).toContain("raw_payments"); // a seed
    expect(ds.prSelectable).toContain("cents_to_dollars"); // a macro
    // the MODEL-ONLY scope drops them.
    expect(ds.prSelectableModels).not.toContain("raw_payments");
    expect(ds.prSelectableModels).not.toContain("cents_to_dollars");
    // exactly the prSelectable ids that have a model record survive.
    expect(ds.prSelectableModels).toEqual(ds.prSelectable.filter((id) => !!ds.D[id]));
    expect(ds.prSelectableModels.length).toBe(ds.prSelectable.length - 2);
  });
  it("every prSelectableModels id has a model record in D (the loop never lands recordless)", () => {
    const ds = buildDataset(ctx440());
    expect(ds.prSelectableModels.length).toBeGreaterThan(0);
    ds.prSelectableModels.forEach((id) => {
      expect(ds.D[id], `${id} must have a model record`).toBeDefined();
      expect(ds.MODELS).toContain(id); // …and be a real model name
    });
  });
  it("falls back to MODELS when the PR scope is empty (parity with prSelectable)", () => {
    const data = {
      baseline: "main",
      models: [{ name: "m1", path: "models/marts/m1.sql", state: "modified", raw_sql: "select 1" }],
      seed_cards: [], pr_comments: { by_model: [], unanchored: [] },
    } as unknown as ContextData;
    const ds = buildDataset(data);
    expect(ds.prSelectable).toEqual(["m1"]); // empty PR scope → MODELS fallback
    expect(ds.prSelectableModels).toEqual(["m1"]);
  });
});

describe("raw NEVER masquerades as compiled (the honesty invariant)", () => {
  it("a model with no code_map has NULL compiledSql/nodeSpans (never raw_sql)", () => {
    const ds = buildDataset(loadFixture("context.sample") as unknown as ContextData);
    // the comments-showcase fixture ships no code_map.
    ds.MODELS.forEach((n) => {
      const r = ds.D[n]!;
      expect(r.compiledSql).toBeNull();
      expect(r.nodeSpans).toBeNull();
      // but the raw side is still surfaced honestly.
      if (r.rawSql) expect(typeof r.rawSql).toBe("string");
    });
  });
  it("a model WITH a code_map exposes the real compiled spine", () => {
    const ds = buildDataset(ctx440());
    const withMap = ds.MODELS.map((n) => ds.D[n]!).find((r) => r.compiledSql != null);
    expect(withMap).toBeDefined();
    expect(withMap!.compiledSql).not.toBe(withMap!.rawSql);
  });
});

describe("dataset maps — untrusted node/column/seed name keys can't pollute the prototype", () => {
  // The reshaper's maps are keyed by dbt node/column/model/seed names, all
  // attacker-influenceable. A name literally `__proto__` must land as a real OWN
  // key on a null-proto map — never mutate Object.prototype. (gemini-code-assist, #515)
  it("a model named __proto__ is a real own-key in D/CTE/MODELS, not a prototype mutation", () => {
    const data = {
      baseline: "main",
      models: [{ name: "__proto__", path: "models/marts/__proto__.sql", state: "modified", raw_sql: "select 1" }],
      seed_cards: [], pr_comments: { by_model: [], unanchored: [] },
    } as unknown as ContextData;
    const ds = buildDataset(data);
    expect(ds.MODELS).toContain("__proto__");
    expect(Object.prototype.hasOwnProperty.call(ds.D, "__proto__")).toBe(true); // an OWN key
    expect(ds.D["__proto__"]).toBeDefined();
    expect(Object.prototype.hasOwnProperty.call(ds.CTE, "__proto__")).toBe(true);
    // the global Object prototype is untouched (the entry did not climb the chain).
    expect(({} as Record<string, unknown>)["polluted"]).toBeUndefined();
    expect(Object.getPrototypeOf({})).toBe(Object.prototype);
  });
  it("a seed card + column literally named __proto__ are own-keys, not prototype mutations", () => {
    const data = {
      baseline: "main", models: [],
      seed_cards: [{
        name: "__proto__", original_file_path: "seeds/__proto__.csv",
        column_types: "__proto__:varchar", table: { columns: ["__proto__"], rows: [] },
      }],
      pr_comments: { by_model: [], unanchored: [] },
    } as unknown as ContextData;
    const { seeds, seedRecords } = buildSeedRecords(data);
    expect(seeds).toContain("__proto__");
    expect(Object.prototype.hasOwnProperty.call(seedRecords, "__proto__")).toBe(true);
    const rec = seedRecords["__proto__"]!;
    expect(rec).toBeDefined();
    // the colTypes map (parseSeedColumnTypes) is also null-proto + stores the own key.
    expect(Object.prototype.hasOwnProperty.call(rec.colTypes, "__proto__")).toBe(true);
    expect(rec.colTypes["__proto__"]).toBe("varchar");
    // the global Object prototype is untouched.
    expect(({} as Record<string, unknown>)["polluted"]).toBeUndefined();
    expect(Object.getPrototypeOf({})).toBe(Object.prototype);
  });
});

describe("findingsToCoverage", () => {
  const f = (status: string, tier = "high"): FindingPayload => ({ check: "grain.unique-key", tier: tier as "high", verdict: { status: status as "covered" } });
  it("counts covered/uncovered/unknown by verdict.status", () => {
    const c = findingsToCoverage([f("covered"), f("uncovered"), f("unknown"), f("weird")]);
    expect(c.counts).toEqual({ covered: 1, uncovered: 1, unknown: 2 }); // an unknown status normalizes to unknown
  });
  it("humanizes the check id + maps the tier", () => {
    const c = findingsToCoverage([f("covered", "total")]);
    expect(c.checks[0]!.name).toBe("grain · unique key");
    expect(c.checks[0]!.tier).toBe("HIGH");
  });
  it("empty findings ⇒ zero counts", () => {
    expect(findingsToCoverage(null).counts).toEqual({ covered: 0, uncovered: 0, unknown: 0 });
  });
});

describe("prDagToScope", () => {
  it("null on no pr_dag", () => { expect(prDagToScope(null, {}, {}, {})).toBeNull(); });
  it("builds nodes/edges/selectable + resolves names from ids", () => {
    const view = {
      graph: {
        nodes: [
          { id: "model.x", name: "x", state: "modified" as const, is_connector: false, lines_added: 1, lines_removed: 0 },
          { id: "seed.s", name: "s", state: "new" as const, is_connector: true, lines_added: 0, lines_removed: 0 },
        ],
        edges: [{ from: "seed.s", to: "model.x" }],
      },
      modified_count: 1, connector_count: 1, halo_count: 0, deleted_count: 0, collapsed: false,
    };
    const sc = prDagToScope(view, { x: "marts" }, { x: { materialized: "table" } }, { x: "modified" })!;
    expect(sc.selectable).toEqual(["x"]); // the connector is not selectable
    expect(sc.data.edges).toEqual([["s", "x"]]); // edges resolve to names
    expect(sc.data.nodes.find((n) => n.id === "s")!.context).toBe(true);
    expect(sc.data.nodes.find((n) => n.id === "x")!.kind).toBe("model");
    expect(sc.data.nodes.find((n) => n.id === "s")!.kind).toBe("seed");
  });
});

describe("the 3-axis scope selection (S4 engine harness)", () => {
  const mk = (n: number): PrScope => ({
    data: { nodes: Array.from({ length: n }, (_, i) => ({ id: "n" + i, label: "n" + i, sub: "", tone: "modified", kind: "model" as const, change: "modified", context: false, mat: null })), edges: [] },
    selectable: Array.from({ length: n }, (_, i) => "n" + i),
    counts: {},
  });

  it("pickScopeAxis returns the requested axis when present", () => {
    const byAxis = { all: mk(5), body: mk(3), config: mk(2), unit_test: mk(4) };
    expect(pickScopeAxis(byAxis, "body")!.data.nodes.length).toBe(3);
    expect(pickScopeAxis(byAxis, "unit_test")!.data.nodes.length).toBe(4);
    expect(pickScopeAxis(byAxis, "all")!.data.nodes.length).toBe(5);
  });

  it("pickScopeAxis degrades to `all` when the requested axis is absent (honest, no fabrication)", () => {
    const byAxis = { all: mk(5) };
    expect(pickScopeAxis(byAxis, "body")!.data.nodes.length).toBe(5);
    expect(pickScopeAxis(byAxis, "config")).toBe(byAxis.all);
  });

  it("pickScopeAxis is null when nothing is present", () => {
    expect(pickScopeAxis({}, "all")).toBeNull();
  });

  it("availableScopeAxes lists only emitted axes, `all` leading", () => {
    expect(availableScopeAxes({ all: mk(1), body: mk(1) })).toEqual(["all", "body"]);
    expect(availableScopeAxes({ all: mk(1) })).toEqual(["all"]);
    expect(availableScopeAxes({})).toEqual([]);
  });

  it("on the real fixture, the three axes select DISTINCT subgraphs (the toggle swaps the graph)", () => {
    const ds = buildDataset(ctx440());
    const all = pickScopeAxis(ds.prScopeByAxis, "all")!.data.nodes.length;
    const body = pickScopeAxis(ds.prScopeByAxis, "body")!.data.nodes.length;
    const unit = pickScopeAxis(ds.prScopeByAxis, "unit_test")!.data.nodes.length;
    expect(all).toBeGreaterThan(0);
    // the fixture's axes carry different node counts (a real subgraph swap).
    expect(new Set([all, body, unit]).size).toBeGreaterThan(1);
  });
});

describe("scopeToGraph — ScopeNode POD → shared engine GraphData", () => {
  it("carries kind/change/context VERBATIM and normalizes the mat glyph", () => {
    const scope: PrScope["data"] = {
      nodes: [
        { id: "m", label: "m", sub: "marts", tone: "modified", kind: "model", change: "modified", context: false, mat: "incremental" },
        { id: "s", label: "s", sub: "connector", tone: "base", kind: "seed", change: "context", context: true, mat: null },
        { id: "x", label: "x", sub: "", tone: "added", kind: "model", change: "added", context: false, mat: "bogus" },
      ],
      edges: [["s", "m"], ["m", "x"]],
    };
    const g = scopeToGraph(scope);
    expect(g.nodes.map((n) => n.kind)).toEqual(["model", "seed", "model"]);
    expect(g.nodes.find((n) => n.id === "m")!.mat).toBe("incremental");
    expect(g.nodes.find((n) => n.id === "s")!.context).toBe(true);
    expect(g.nodes.find((n) => n.id === "x")!.mat).toBeNull(); // unrecognized mat → honest-null
    expect(g.edges).toEqual([["s", "m"], ["m", "x"]]);
  });
  it("null/undefined scope → empty graph", () => {
    expect(scopeToGraph(null)).toEqual({ nodes: [], edges: [] });
  });
});

describe("the small pure folds", () => {
  it("pierreSide: Left→deletions, Right→additions", () => {
    expect(pierreSide("Left")).toBe("deletions");
    expect(pierreSide("Right")).toBe("additions");
  });
  it("schemaOf: models/<schema>/x.sql → schema; flat → models", () => {
    expect(schemaOf("models/marts/orders.sql")).toBe("marts");
    expect(schemaOf("orders.sql")).toBe("models");
    expect(schemaOf(undefined)).toBe("models");
  });
  it("stateToChange: new→added, deleted→removed, else modified", () => {
    expect(stateToChange("new")).toBe("added");
    expect(stateToChange("deleted")).toBe("removed");
    expect(stateToChange("modified")).toBe("modified");
  });
  it("blockDiffToPatch counts old/new lines + emits a git header", () => {
    const p = blockDiffToPatch({ lines: [{ kind: "context", text: "a" }, { kind: "removed", text: "b" }, { kind: "added", text: "c" }] }, "f.sql");
    expect(p).toContain("@@ -1,2 +1,2 @@");
    expect(p).toContain("-b"); expect(p).toContain("+c"); expect(p).toContain(" a");
  });
  it("sourceToContextPatch makes an all-context patch", () => {
    expect(sourceToContextPatch("x\ny", "f")).toContain("@@ -1,2 +1,2 @@");
  });
  it("blockDiffToHunks: empty diff ⇒ []", () => {
    expect(blockDiffToHunks(null)).toEqual([]);
    expect(blockDiffToHunks({ lines: [] })).toEqual([]);
  });
  it("dagToCte: null on no dag", () => { expect(dagToCte(null)).toBeNull(); });
  it("threadsByPath groups by anchor path (a path-less thread is skipped)", () => {
    const data = { baseline: "main", models: [], pr_comments: { by_model: [{ path: "a.sql", threads: [{ path: "a.sql", comments: [] }] }], unanchored: [{ comments: [] }] } } as unknown as ContextData;
    const m = threadsByPath(data);
    expect(m.get("a.sql")).toHaveLength(1);
    expect([...m.keys()]).toEqual(["a.sql"]);
  });
});
