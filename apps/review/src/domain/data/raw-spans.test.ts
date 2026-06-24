// raw-spans byte-span unit tests — rawDagToGraph + buildRawSpans + ensureMainNode.
// The highest-complexity reshapers; the byte-span parsing is also fuzzed in
// raw-spans.fuzz.test.ts (the never-throws / never-fabricates invariant).
import { describe, expect, it } from "vitest";
import type { CodeMap, DagPayload, ModelPayload } from "../context-data";
import { buildRawSpans, ensureMainNode, rawDagToGraph } from "./raw-spans";

const pos = (line: number, byte: number) => ({ line, col: 1, byte });
const span = (sl: number, sb: number, el: number, eb: number) => ({ start: pos(sl, sb), end: pos(el, eb) });

const dag: DagPayload = {
  nodes: [
    { id: "stg", label: "stg", role: "import" },
    { id: "mid", label: "mid", role: "transform" },
    { id: "(final select)", label: "final", role: "final" },
  ],
  edges: [
    { from: "stg", to: "mid", edge_type: "from" },
    { from: "mid", to: "(final select)", edge_type: "inner" },
  ],
};

describe("rawDagToGraph", () => {
  it("null when there's no raw spine (honest empty — never raw masquerading)", () => {
    expect(rawDagToGraph(dag, null)).toBeNull();
    expect(rawDagToGraph(dag, { compiled: "x" } as CodeMap)).toBeNull(); // no raw_dag
    expect(rawDagToGraph(null, { raw_dag: { nodes: [] } } as CodeMap)).toBeNull();
  });

  it("mirrors the compiled dag structure when no zones apply", () => {
    const cm: CodeMap = { raw_dag: { nodes: [] }, raw_node_spans: {}, raw_zones: [] };
    const g = rawDagToGraph(dag, cm)!;
    expect(g.nodes.map((n) => n.id)).toEqual(["stg", "mid", "(final select)"]);
    expect(g.edges).toEqual([["stg", "mid"], ["mid", "(final select)"]]);
    expect(g.zones).toEqual([]);
  });

  it("an inline incremental_guard zone INSIDE a node's span ⇒ a marker on that node", () => {
    const cm: CodeMap = {
      raw_dag: { nodes: [] },
      raw_node_spans: { mid: span(1, 0, 10, 100) },
      raw_zones: [{ kind: "incremental_guard", start: pos(3, 30), end: pos(5, 50), presence: "compiled_out" }],
    };
    const g = rawDagToGraph(dag, cm)!;
    const mid = g.nodes.find((n) => n.id === "mid")!;
    expect(mid.zoneKind).toBe("incremental_guard");
    expect(mid.zonePresence).toBe("compiled_out");
    expect(mid.hasIncremental).toBe(true);
  });

  it("an inline zone OUTSIDE every span ⇒ a marker on the final node", () => {
    const cm: CodeMap = {
      raw_dag: { nodes: [] },
      raw_node_spans: { stg: span(1, 0, 2, 20) },
      raw_zones: [{ kind: "incremental_guard", start: pos(8, 80), end: pos(9, 90), presence: "compiled_out" }],
    };
    const g = rawDagToGraph(dag, cm)!;
    expect(g.nodes.find((n) => n.id === "(final select)")!.hasIncremental).toBe(true);
  });

  it("a {% for %} fan-out collapses the N generated CTEs into ONE zero-templated node + rewires the fan", () => {
    const dag2: DagPayload = {
      nodes: [
        { id: "base", role: "import" },
        { id: "a_orders", role: "transform" },
        { id: "b_orders", role: "transform" },
        { id: "(final select)", role: "final" },
      ],
      edges: [
        { from: "base", to: "a_orders", edge_type: "from" },
        { from: "base", to: "b_orders", edge_type: "from" },
        { from: "a_orders", to: "(final select)", edge_type: "union_all" },
        { from: "b_orders", to: "(final select)", edge_type: "union_all" },
      ],
    };
    const cm: CodeMap = {
      raw_dag: { nodes: [] },
      raw_node_spans: { base: span(1, 0, 2, 20) },
      raw_zones: [{ kind: "for_loop", template: "{{ status }}_orders", loop: "for status", start: pos(3, 30), end: pos(8, 90), presence: "compiled_in" }],
      node_map: { raw: { "zone:0": ["a_orders", "b_orders"] } },
    };
    const g = rawDagToGraph(dag2, cm)!;
    expect(g.nodes.some((n) => n.id === "a_orders" || n.id === "b_orders")).toBe(false);
    const zn = g.nodes.find((n) => n.id === "zone:0")!;
    expect(zn).toMatchObject({ templated: true, genCount: 2, presence: "compiled_in", label: "{{ status }}_orders" });
    // the 1→N fan-out is rewired through the single zone node + deduped.
    expect(g.edges).toContainEqual(["base", "zone:0"]);
    expect(g.edges).toContainEqual(["zone:0", "(final select)"]);
    expect(g.edges.filter(([a, b]) => a === "base" && b === "zone:0")).toHaveLength(1);
    // and a selectable region is emitted for the loop.
    expect(g.zones).toHaveLength(1);
    expect(g.zones[0]).toMatchObject({ id: "z0", nodeId: "zone:0", startLine: 3, endLine: 8 });
  });

  it("zone-host boundary: a zone exactly at a node's span edges IS inside (>= / <= are inclusive)", () => {
    // z spans [byte 0..100], node mid spans [byte 0..100] exactly ⇒ inside (inclusive).
    const cm: CodeMap = {
      raw_dag: { nodes: [] },
      raw_node_spans: { mid: span(1, 0, 10, 100) },
      raw_zones: [{ kind: "incremental_guard", start: pos(1, 0), end: pos(10, 100), presence: "compiled_out" }],
    };
    const g = rawDagToGraph(dag, cm)!;
    expect(g.nodes.find((n) => n.id === "mid")!.zoneKind).toBe("incremental_guard");
    expect(g.nodes.find((n) => n.id === "(final select)")!.zoneKind).toBeUndefined();
  });
  it("zone-host boundary: a zone one byte PAST a node's span is NOT inside it (falls to final)", () => {
    const cm: CodeMap = {
      raw_dag: { nodes: [] },
      raw_node_spans: { mid: span(1, 0, 10, 100) },
      // z.end.byte 101 > mid.end.byte 100 ⇒ NOT inside mid ⇒ marker on final.
      raw_zones: [{ kind: "incremental_guard", start: pos(1, 0), end: pos(11, 101), presence: "compiled_out" }],
    };
    const g = rawDagToGraph(dag, cm)!;
    expect(g.nodes.find((n) => n.id === "mid")!.zoneKind).toBeUndefined();
    expect(g.nodes.find((n) => n.id === "(final select)")!.hasIncremental).toBe(true);
  });
  it("a zone with a missing span is skipped (no host marker, no region)", () => {
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: {},
      raw_zones: [{ kind: "incremental_guard", presence: "compiled_out" }], // no start/end
    };
    const g = rawDagToGraph(dag, cm)!;
    // an inline zone with no span falls to the final node (inside() is false for all).
    expect(g.zones).toEqual([]);
  });
  it("nested for-loops: a region's depth counts STRICTLY-wrapping outer loops", () => {
    // outer loop [10..200] strictly wraps inner [20..100]; inner depth=1, outer depth=0.
    const dag3: DagPayload = { nodes: [{ id: "(final select)", role: "final" }], edges: [] };
    const cm: CodeMap = {
      raw_dag: { nodes: [] },
      raw_node_spans: {},
      raw_zones: [
        { kind: "for_loop", loop: "outer", start: pos(1, 10), end: pos(20, 200), presence: "compiled_in" },
        { kind: "for_loop", loop: "inner", start: pos(2, 20), end: pos(10, 100), presence: "compiled_in" },
      ],
    };
    const g = rawDagToGraph(dag3, cm)!;
    const inner = g.zones.find((z) => z.label === "inner")!;
    const outer = g.zones.find((z) => z.label === "outer")!;
    expect(inner.depth).toBe(1); // wrapped by outer
    expect(outer.depth).toBe(0); // wraps nothing (an EQUAL span is NOT a strict wrap)
  });
  it("strictWraps: a PARTIALLY-overlapping outer loop does NOT count as wrapping (depth 0)", () => {
    // outer [10..100] only partially overlaps inner [50..150] (outer.end < inner.end)
    // ⇒ NOT a strict wrap ⇒ inner depth stays 0.
    const dag5: DagPayload = { nodes: [{ id: "(final select)", role: "final" }], edges: [] };
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: {},
      raw_zones: [
        { kind: "for_loop", loop: "partial", start: pos(1, 10), end: pos(10, 100), presence: "compiled_in" },
        { kind: "for_loop", loop: "target", start: pos(5, 50), end: pos(15, 150), presence: "compiled_in" },
      ],
    };
    const g = rawDagToGraph(dag5, cm)!;
    expect(g.zones.find((z) => z.label === "target")!.depth).toBe(0);
  });
  it("strictWraps: an EQUAL-span outer loop is NOT a strict wrap (depth 0, not 1)", () => {
    const dag6: DagPayload = { nodes: [{ id: "(final select)", role: "final" }], edges: [] };
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: {},
      raw_zones: [
        { kind: "for_loop", loop: "twin_a", start: pos(1, 10), end: pos(10, 100), presence: "compiled_in" },
        { kind: "for_loop", loop: "twin_b", start: pos(1, 10), end: pos(10, 100), presence: "compiled_in" },
      ],
    };
    const g = rawDagToGraph(dag6, cm)!;
    // neither strictly wraps the other (equal spans) ⇒ both depth 0.
    expect(g.zones.every((z) => z.depth === 0)).toBe(true);
  });
  it("region membership: a node whose raw byte sits inside a loop is a member; outside is not", () => {
    const dag4: DagPayload = {
      nodes: [{ id: "inloop", role: "transform" }, { id: "outloop", role: "transform" }, { id: "(final select)", role: "final" }],
      edges: [{ from: "inloop", to: "(final select)", edge_type: "from" }, { from: "outloop", to: "(final select)", edge_type: "from" }],
    };
    const cm: CodeMap = {
      raw_dag: { nodes: [] },
      raw_node_spans: { inloop: span(3, 30, 5, 50), outloop: span(12, 120, 14, 140) },
      raw_zones: [{ kind: "for_loop", loop: "L", start: pos(2, 20), end: pos(8, 80), presence: "compiled_in" }],
    };
    const g = rawDagToGraph(dag4, cm)!;
    const region = g.zones[0]!;
    expect(region.members).toContain("inloop");
    expect(region.members).not.toContain("outloop");
  });
  it("a string-arm node_map.raw value (the asymmetric arm) is tolerated as a singleton", () => {
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: {},
      raw_zones: [{ kind: "for_loop", template: "t", start: pos(2, 10), end: pos(4, 40), presence: "compiled_in" }],
      node_map: { raw: { "zone:0": "mid" as unknown as string[] } },
    };
    const g = rawDagToGraph(dag, cm)!;
    expect(g.nodes.some((n) => n.id === "mid")).toBe(false);
    expect(g.nodes.find((n) => n.id === "zone:0")!.genCount).toBe(1);
  });
});

describe("ensureMainNode", () => {
  it("synthesizes a main node for an empty graph (CTE-less model)", () => {
    const g = ensureMainNode({ nodes: [], edges: [] }, "order_metrics_pk.sql")!;
    expect(g.nodes).toEqual([{ id: "(final select)", label: "order_metrics_pk.sql", sub: "main query", tone: "final" }]);
    expect(g.edges).toEqual([]);
  });
  it("leaves a non-empty graph untouched", () => {
    const g = { nodes: [{ id: "x", label: "x", sub: "import", tone: "base" }], edges: [] as [string, string][] };
    expect(ensureMainNode(g, "f.sql")).toBe(g);
  });
  it("passes null/undefined through", () => { expect(ensureMainNode(null, "f")).toBeNull(); });
});

describe("buildRawSpans", () => {
  const m = (raw: string): Pick<ModelPayload, "raw_sql"> => ({ raw_sql: raw });

  it("null when there's no code_map", () => { expect(buildRawSpans(m("x"), null)).toBeNull(); });

  it("lifts raw_node_spans to line spans + zone:N from for_loop zones", () => {
    const cm: CodeMap = {
      raw_node_spans: { stg: span(1, 0, 3, 30) },
      raw_zones: [{ kind: "for_loop", start: pos(5, 50), end: pos(8, 90), presence: "compiled_in" }],
    };
    const out = buildRawSpans(m("a\nb\nc\nd\ne\nf\ng\nh\ni\nj"), cm)!;
    expect(out["stg"]).toEqual({ start: { line: 1 }, end: { line: 3 } });
    expect(out["zone:0"]).toEqual({ start: { line: 5 }, end: { line: 8 } });
  });

  it("computes the (final select) span: first non-blank after the last block → EOF (B3 heuristic)", () => {
    const cm: CodeMap = { raw_node_spans: { stg: span(1, 0, 2, 20) } };
    // lines 3 blank, 4-5 content ⇒ final span = 4..5.
    const out = buildRawSpans(m("l1\nl2\n\nl4\nl5"), cm)!;
    expect(out["(final select)"]).toEqual({ start: { line: 4 }, end: { line: 5 } });
  });

  it("CTE-less (no spans) ⇒ the whole file is the final span", () => {
    const out = buildRawSpans(m("select 1\nfrom orders"), { compiled: "" } as CodeMap)!;
    expect(out["(final select)"]).toEqual({ start: { line: 1 }, end: { line: 2 } });
  });

  it("does NOT override an explicit (final select) raw span", () => {
    const cm: CodeMap = { raw_node_spans: { "(final select)": span(10, 100, 20, 200) } };
    const out = buildRawSpans(m("x"), cm)!;
    expect(out["(final select)"]).toEqual({ start: { line: 10 }, end: { line: 20 } });
  });

  it("an empty source with an empty code_map ⇒ a whole-file (1-line) final span, not null", () => {
    // a present-but-empty source is the whole file (1 line) — honest: the model
    // exists; it is not the no-code_map case (which IS null, tested above).
    expect(buildRawSpans(m(""), {} as CodeMap)).toEqual({ "(final select)": { start: { line: 1 }, end: { line: 1 } } });
  });
});
