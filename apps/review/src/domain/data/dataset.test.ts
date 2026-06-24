// dataset orchestrator tests — buildDataset (WeakMap memo) + the per-source
// reshape + the honesty invariants on the real 16-model spine. Plus the smaller
// pure folds (findingsToCoverage, prDagToScope, deriveInfo, threadsByPath).
import { describe, expect, it } from "vitest";
import { loadFixture, rawFixture } from "../../data/fixtures";
import { parseContext } from "../schema";
import type { ContextData, FindingPayload } from "../context-data";
import {
  blockDiffToHunks, blockDiffToPatch, buildDataset, dagToCte, findingsToCoverage,
  pierreSide, prDagToScope, schemaOf, sourceToContextPatch, stateToChange, threadsByPath,
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
