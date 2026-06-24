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

// ── mutation-hardening: pin the never-a-false-claim honesty folds (cute-dbt#514) ─
// A surviving mutant in this module = a presence/zone/span-boundary flip the tests
// don't catch. Each test below kills a SPECIFIC survivor; comments name line+mutator.

describe("rawDagToGraph — the incremental_guard honesty marker (kills L130)", () => {
  it("sets hasIncremental ONLY for an incremental_guard zone, NOT for another inline kind", () => {
    // an incremental_guard ⇒ host.hasIncremental = true (the honesty claim "this
    // node has an is_incremental() branch"). The `if (z.kind === "incremental_guard")`
    // → true mutant would set hasIncremental on a NON-incremental inline zone (a false
    // "has incremental" claim); the → false mutant would never set it (a missing claim).
    const cmGuard: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { mid: span(1, 0, 10, 100) },
      raw_zones: [{ kind: "incremental_guard", start: pos(2, 20), end: pos(3, 30), presence: "compiled_out" }],
    };
    const cmOther: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { mid: span(1, 0, 10, 100) },
      raw_zones: [{ kind: "if_block", start: pos(2, 20), end: pos(3, 30), presence: "structural" }],
    };
    const guardMid = rawDagToGraph(dag, cmGuard)!.nodes.find((n) => n.id === "mid")!;
    const otherMid = rawDagToGraph(dag, cmOther)!.nodes.find((n) => n.id === "mid")!;
    expect(guardMid.hasIncremental).toBe(true);   // incremental_guard ⇒ marker set
    expect(otherMid.hasIncremental).toBeUndefined(); // a non-guard inline zone ⇒ NO marker
    // both still carry the zone kind + presence (the honest zone facts).
    expect(guardMid.zoneKind).toBe("incremental_guard");
    expect(guardMid.zonePresence).toBe("compiled_out");
    expect(otherMid.zoneKind).toBe("if_block");
    expect(otherMid.zonePresence).toBe("structural");
  });
});

describe("rawDagToGraph — out-of-span zone falls to the FINAL node, not the last array node (kills L81)", () => {
  it("finds the node whose sub === 'final' (not merely nodes[length-1]) to host the marker", () => {
    // dag with the final node NOT last in the array; an out-of-span zone must land on
    // the role==='final' node, not the trailing array element. Kills:
    //   L81 LogicalOperator/`?? nodes[len-1]` + the `n.sub === "final"` predicate +
    //   the ArithmeticOperator(length+1) index mutant.
    const reordered: DagPayload = {
      nodes: [
        { id: "stg", role: "import" },
        { id: "(final select)", label: "final", role: "final" }, // final is MIDDLE
        { id: "trailing", role: "transform" },                    // a non-final last node
      ],
      edges: [{ from: "stg", to: "(final select)", edge_type: "from" }],
    };
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { stg: span(1, 0, 2, 20) },
      raw_zones: [{ kind: "incremental_guard", start: pos(8, 80), end: pos(9, 90), presence: "compiled_out" }],
    };
    const g = rawDagToGraph(reordered, cm)!;
    expect(g.nodes.find((n) => n.id === "(final select)")!.hasIncremental).toBe(true);
    expect(g.nodes.find((n) => n.id === "trailing")!.hasIncremental).toBeUndefined();
  });
});

describe("rawDagToGraph — strictWraps boundary equality (kills L88/L89 byte-edge mutants)", () => {
  // depth counts STRICTLY-wrapping loops: outer must contain inner with at least one
  // non-shared edge. Three calibrated cases pin the <= / >= / strict-not-equal lines.
  const mk = (zones: CodeMap["raw_zones"]): RawZoneRegionDepths => {
    const dagF: DagPayload = { nodes: [{ id: "(final select)", role: "final" }], edges: [] };
    const g = rawDagToGraph(dagF, { raw_dag: { nodes: [] }, raw_node_spans: {}, raw_zones: zones })!;
    return Object.fromEntries(g.zones.map((z) => [z.label, z.depth]));
  };
  type RawZoneRegionDepths = Record<string, number>;

  it("a SHARED start-edge but a contained end DOES strict-wrap (kills L88 <= → <)", () => {
    // outer.start === inner.start (shared), outer.end > inner.end (contained) ⇒ strict
    // wrap (one non-shared edge). The `<=` → `<` mutant would reject the shared start
    // and undercount the depth to 0.
    const d = mk([
      { kind: "for_loop", loop: "outer", start: pos(1, 10), end: pos(20, 200), presence: "compiled_in" },
      { kind: "for_loop", loop: "inner", start: pos(1, 10), end: pos(10, 100), presence: "compiled_in" },
    ]);
    expect(d["inner"]).toBe(1); // outer wraps inner via the shared-start/contained-end
    expect(d["outer"]).toBe(0);
  });
  it("a SHARED end-edge but a contained start DOES strict-wrap (kills L88 >= → >)", () => {
    // outer.start < inner.start, outer.end === inner.end (shared end). Strict wrap.
    // The `>=` → `>` mutant would reject the shared end and undercount to 0.
    const d = mk([
      { kind: "for_loop", loop: "outer", start: pos(1, 10), end: pos(20, 200), presence: "compiled_in" },
      { kind: "for_loop", loop: "inner", start: pos(2, 50), end: pos(20, 200), presence: "compiled_in" },
    ]);
    expect(d["inner"]).toBe(1);
    expect(d["outer"]).toBe(0);
  });
  it("a fully-EQUAL span is NOT a strict wrap (kills L89 the !(===&&===) clause)", () => {
    // both edges shared ⇒ NOT strict ⇒ depth 0 for both. The L89 mutants (drop the
    // not-equal exclusion / OR it) would count an equal span as a wrap (depth 1).
    const d = mk([
      { kind: "for_loop", loop: "twinA", start: pos(1, 10), end: pos(10, 100), presence: "compiled_in" },
      { kind: "for_loop", loop: "twinB", start: pos(1, 10), end: pos(10, 100), presence: "compiled_in" },
    ]);
    expect(d["twinA"]).toBe(0);
    expect(d["twinB"]).toBe(0);
  });
});

describe("rawDagToGraph — for-loop collapse vs wrapper branch (kills L101/L108/L115/L116)", () => {
  it("a for_loop that GENERATES CTEs collapses + registers a zone byte span for membership (L101/L115)", () => {
    // node_map.raw has zone:0 → [a,b] ⇒ the generated branch fires (L101 &&). The
    // collapsed zone:0 node gets a rawByte entry (L115) so a CONTAINED node is a member.
    const dag2: DagPayload = {
      nodes: [
        { id: "base", role: "import" },
        { id: "a_o", role: "transform" }, { id: "b_o", role: "transform" },
        { id: "(final select)", role: "final" },
      ],
      edges: [
        { from: "base", to: "a_o", edge_type: "from" }, { from: "base", to: "b_o", edge_type: "from" },
        { from: "a_o", to: "(final select)", edge_type: "union_all" }, { from: "b_o", to: "(final select)", edge_type: "union_all" },
      ],
    };
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { base: span(1, 0, 2, 20) },
      raw_zones: [{ kind: "for_loop", template: "{{ s }}_o", loop: "for s", start: pos(3, 30), end: pos(8, 90), presence: "compiled_in" }],
      node_map: { raw: { "zone:0": ["a_o", "b_o"] } },
    };
    const g = rawDagToGraph(dag2, cm)!;
    // the generated CTEs are collapsed into the one zone node (kills the L101 mutants
    // that would skip the collapse).
    expect(g.nodes.some((n) => n.id === "a_o" || n.id === "b_o")).toBe(false);
    expect(g.nodes.find((n) => n.id === "zone:0")).toMatchObject({ templated: true, genCount: 2, presence: "compiled_in" });
    // the fan-out is deduped (kills the L108 dedup-filter mutants): base→zone:0 once.
    expect(g.edges.filter(([a, b]) => a === "base" && b === "zone:0")).toHaveLength(1);
    expect(g.edges.filter(([a, b]) => a === "zone:0" && b === "(final select)")).toHaveLength(1);
    // no self-loop survives the filter.
    expect(g.edges.some(([a, b]) => a === b)).toBe(false);
  });
  it("a WRAPPER for_loop (generates NO CTE) yields NO graph node + NO marker (kills L116 branch)", () => {
    // node_map.raw has zone:0 → [] (empty) ⇒ the WRAPPER branch (L116) runs: no zone
    // node, no host marker. It surfaces only as a REGION (tested elsewhere).
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { mid: span(1, 0, 10, 100) },
      raw_zones: [{ kind: "for_loop", loop: "wrap", start: pos(2, 20), end: pos(9, 90), presence: "compiled_in" }],
      node_map: { raw: { "zone:0": [] } },
    };
    const g = rawDagToGraph(dag, cm)!;
    expect(g.nodes.some((n) => n.id === "zone:0")).toBe(false); // no collapsed node
    // and NO node gained a zoneKind marker (a wrapper loop is not an inline marker).
    expect(g.nodes.every((n) => n.zoneKind === undefined)).toBe(true);
    // it DOES surface as a selectable region.
    expect(g.zones.map((z) => z.label)).toContain("wrap");
  });
});

describe("rawDagToGraph — region membership boundary (kills L137 within())", () => {
  it("a node whose raw byte span sits AT the loop's edges is a member; one byte past is NOT", () => {
    // inAt is exactly at the loop bounds [20..80]; outPast starts one byte past 80.
    const dag4: DagPayload = {
      nodes: [
        { id: "inAt", role: "transform" }, { id: "outPast", role: "transform" }, { id: "(final select)", role: "final" },
      ],
      edges: [
        { from: "inAt", to: "(final select)", edge_type: "from" },
        { from: "outPast", to: "(final select)", edge_type: "from" },
      ],
    };
    const cm: CodeMap = {
      raw_dag: { nodes: [] },
      // inAt byte [20..80] == loop [20..80] (inclusive within); outPast [81..90] is past.
      raw_node_spans: { inAt: span(2, 20, 8, 80), outPast: span(9, 81, 10, 90) },
      raw_zones: [{ kind: "for_loop", loop: "L", start: pos(2, 20), end: pos(8, 80), presence: "compiled_in" }],
    };
    const region = rawDagToGraph(dag4, cm)!.zones[0]!;
    expect(region.members).toContain("inAt");   // boundary-inclusive (kills i.s>o.s / i.e<o.e)
    expect(region.members).not.toContain("outPast");
  });

  it("region membership: a node STARTING before the loop is excluded (kills L137 i.s >= o.s direction)", () => {
    // `before` starts at byte 5, BEFORE the loop's 20 ⇒ NOT a member (i.s < o.s). The
    // `i.s > o.s` mutant would FLIP the comparison; the always-true mutant would let
    // it in. `inside` (fully contained) is the only member.
    const dag5: DagPayload = {
      nodes: [
        { id: "before", role: "transform" }, { id: "inside", role: "transform" }, { id: "(final select)", role: "final" },
      ],
      edges: [
        { from: "before", to: "(final select)", edge_type: "from" },
        { from: "inside", to: "(final select)", edge_type: "from" },
      ],
    };
    const cm: CodeMap = {
      raw_dag: { nodes: [] },
      // before [5..40] straddles the loop start; inside [25..75] is fully contained.
      raw_node_spans: { before: span(1, 5, 4, 40), inside: span(3, 25, 7, 75) },
      raw_zones: [{ kind: "for_loop", loop: "L", start: pos(2, 20), end: pos(8, 80), presence: "compiled_in" }],
    };
    const region = rawDagToGraph(dag5, cm)!.zones[0]!;
    expect(region.members).toEqual(["inside"]); // ONLY the fully-contained node
  });

  it("region membership: a node ENDING past the loop is excluded (kills L137 i.e <= o.e direction)", () => {
    // `past` ends at byte 95, PAST the loop's 80 ⇒ NOT a member (i.e > o.e).
    const dag6: DagPayload = {
      nodes: [
        { id: "inside", role: "transform" }, { id: "past", role: "transform" }, { id: "(final select)", role: "final" },
      ],
      edges: [
        { from: "inside", to: "(final select)", edge_type: "from" },
        { from: "past", to: "(final select)", edge_type: "from" },
      ],
    };
    const cm: CodeMap = {
      raw_dag: { nodes: [] },
      raw_node_spans: { inside: span(3, 25, 7, 75), past: span(6, 60, 9, 95) },
      raw_zones: [{ kind: "for_loop", loop: "L", start: pos(2, 20), end: pos(8, 80), presence: "compiled_in" }],
    };
    const region = rawDagToGraph(dag6, cm)!.zones[0]!;
    expect(region.members).toEqual(["inside"]);
  });

  it("a NON-for_loop zone emits NO region (kills L140 z.kind !== 'for_loop' guard)", () => {
    // an incremental_guard zone is a marker, not a region. The `false` mutant would
    // skip the early-return ⇒ fabricate a region for a non-loop zone.
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { mid: span(1, 0, 10, 100) },
      raw_zones: [{ kind: "incremental_guard", start: pos(2, 20), end: pos(3, 30), presence: "compiled_out" }],
    };
    const g = rawDagToGraph(dag, cm)!;
    expect(g.zones).toEqual([]); // NO region for a non-loop zone
  });
});

describe("rawDagToGraph — zone-host boundary: inside() at the exact node-span edges (kills L84/L83)", () => {
  it("a zone whose start is BEFORE the node span is NOT hosted there (kills L84/start-arm direction)", () => {
    // zone [10..50] starts BEFORE mid [20..100] ⇒ NOT inside mid ⇒ host = final.
    // The always-inside mutants would wrongly mark mid.
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { mid: span(2, 20, 10, 100) },
      raw_zones: [{ kind: "incremental_guard", start: pos(1, 10), end: pos(5, 50), presence: "compiled_out" }],
    };
    const g = rawDagToGraph(dag, cm)!;
    expect(g.nodes.find((n) => n.id === "mid")!.zoneKind).toBeUndefined(); // start-before ⇒ not in mid
    expect(g.nodes.find((n) => n.id === "(final select)")!.hasIncremental).toBe(true);
  });
  it("a zone whose end is PAST the node span is NOT hosted there (kills L84 end <= mutant)", () => {
    // zone [20..120] ends PAST mid [20..100] ⇒ NOT inside mid ⇒ host = final.
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { mid: span(2, 20, 10, 100) },
      raw_zones: [{ kind: "incremental_guard", start: pos(2, 20), end: pos(12, 120), presence: "compiled_out" }],
    };
    const g = rawDagToGraph(dag, cm)!;
    expect(g.nodes.find((n) => n.id === "mid")!.zoneKind).toBeUndefined(); // end-past ⇒ not in mid
    expect(g.nodes.find((n) => n.id === "(final select)")!.hasIncremental).toBe(true);
  });
  it("a fully-contained zone IS hosted on the node (kills the inside() always-false direction)", () => {
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { mid: span(2, 20, 10, 100) },
      raw_zones: [{ kind: "incremental_guard", start: pos(3, 40), end: pos(8, 80), presence: "compiled_out" }],
    };
    const g = rawDagToGraph(dag, cm)!;
    expect(g.nodes.find((n) => n.id === "mid")!.zoneKind).toBe("incremental_guard");
    expect(g.nodes.find((n) => n.id === "(final select)")!.zoneKind).toBeUndefined();
  });
});

describe("rawDagToGraph — out-of-span zone with NO final-role node falls to the LAST node (kills L81 length-1)", () => {
  it("with no role==='final' node, an out-of-span marker lands on nodes[length-1], not undefined", () => {
    // no node has sub/role 'final' ⇒ finalNode = nodes[nodes.length - 1] = "last".
    // The `length + 1` mutant indexes past the array ⇒ undefined ⇒ no host ⇒ the
    // marker is silently DROPPED (a missing honesty claim).
    const noFinal: DagPayload = {
      nodes: [
        { id: "first", role: "import" },
        { id: "last", role: "transform" }, // the trailing node — the fallback host
      ],
      edges: [{ from: "first", to: "last", edge_type: "from" }],
    };
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { first: span(1, 0, 2, 20) },
      raw_zones: [{ kind: "incremental_guard", start: pos(8, 80), end: pos(9, 90), presence: "compiled_out" }],
    };
    const g = rawDagToGraph(noFinal, cm)!;
    expect(g.nodes.find((n) => n.id === "last")!.hasIncremental).toBe(true); // hosted on the last node
    expect(g.nodes.find((n) => n.id === "first")!.hasIncremental).toBeUndefined();
  });
});

describe("rawDagToGraph — the collapse branch fires ONLY for a for_loop with generated CTEs (kills L101/L108/L109/L115/L143)", () => {
  it("an inline (non-for_loop) zone WITH a node_map entry is NOT collapsed — it stays an inline marker (kills L101 &&)", () => {
    // zone:0 is an incremental_guard but HAS a node_map.raw entry. The real code takes
    // the ELSE (inline-marker) branch (kind !== for_loop). The `true` mutant would
    // COLLAPSE it into a templated zone node — fabricating a {% for %} template node
    // for an is_incremental() guard (a false structural claim).
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { mid: span(1, 0, 10, 100) },
      raw_zones: [{ kind: "incremental_guard", start: pos(2, 20), end: pos(3, 30), presence: "compiled_out" }],
      node_map: { raw: { "zone:0": ["mid"] } }, // a node_map entry on a NON-loop zone
    };
    const g = rawDagToGraph(dag, cm)!;
    expect(g.nodes.some((n) => n.id === "zone:0")).toBe(false); // NOT collapsed
    expect(g.nodes.some((n) => n.id === "mid")).toBe(true);     // mid survives (not removed)
    expect(g.nodes.find((n) => n.id === "mid")!.zoneKind).toBe("incremental_guard"); // inline marker
  });

  it("the collapse keeps NON-generated nodes + the collapsed zone node is a region member (kills L109/L115)", () => {
    // base (NOT generated) must survive the `nodes.filter(n => !gen.has(n.id))` (L109);
    // the collapsed zone:0 node gets a rawByte span (L115) so it is a MEMBER of its own
    // loop region. The L109 `() => undefined` mutant would drop EVERY node.
    const dag2: DagPayload = {
      nodes: [
        { id: "base", role: "import" }, { id: "g1", role: "transform" }, { id: "g2", role: "transform" },
        { id: "(final select)", role: "final" },
      ],
      edges: [
        { from: "base", to: "g1", edge_type: "from" }, { from: "base", to: "g2", edge_type: "from" },
        { from: "g1", to: "(final select)", edge_type: "union_all" }, { from: "g2", to: "(final select)", edge_type: "union_all" },
      ],
    };
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { base: span(1, 0, 2, 20) },
      raw_zones: [{ kind: "for_loop", template: "{{ s }}", loop: "L", start: pos(3, 30), end: pos(8, 80), presence: "compiled_in" }],
      node_map: { raw: { "zone:0": ["g1", "g2"] } },
    };
    const g = rawDagToGraph(dag2, cm)!;
    expect(g.nodes.some((n) => n.id === "base")).toBe(true); // non-generated node SURVIVES (kills L109)
    expect(g.nodes.some((n) => n.id === "(final select)")).toBe(true);
    // the collapsed zone:0 node IS a member of its own loop region (its rawByte span
    // was registered at L115 ⇒ within() the loop bytes).
    expect(g.zones[0]!.members).toContain("zone:0");
  });

  it("an edge BETWEEN two generated CTEs collapses to a self-loop that is FILTERED OUT (kills the a !== b guard)", () => {
    // g1 → g2 (both generated) maps to zone:0 → zone:0, a self-loop. The `a !== b`
    // guard drops it; the always-true mutant would KEEP a fabricated self-edge on the
    // collapsed template node.
    const dagSelf: DagPayload = {
      nodes: [
        { id: "base", role: "import" }, { id: "g1", role: "transform" }, { id: "g2", role: "transform" },
        { id: "(final select)", role: "final" },
      ],
      edges: [
        { from: "base", to: "g1", edge_type: "from" },
        { from: "g1", to: "g2", edge_type: "from" },          // INTRA-loop edge ⇒ self-loop on collapse
        { from: "g2", to: "(final select)", edge_type: "from" },
      ],
    };
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: { base: span(1, 0, 2, 20) },
      raw_zones: [{ kind: "for_loop", template: "{{ s }}", loop: "L", start: pos(3, 30), end: pos(8, 80), presence: "compiled_in" }],
      node_map: { raw: { "zone:0": ["g1", "g2"] } },
    };
    const g = rawDagToGraph(dagSelf, cm)!;
    // NO self-loop on the collapsed node (the a !== b filter dropped zone:0→zone:0).
    expect(g.edges.some(([a, b]) => a === b)).toBe(false);
    expect(g.edges.some(([a, b]) => a === "zone:0" && b === "zone:0")).toBe(false);
  });

  it("a SOLE for_loop region has depth 0 — it does not count ITSELF (kills L143 j !== zi always-true)", () => {
    // exactly ONE for_loop zone ⇒ its region depth MUST be 0 (no OTHER loop wraps it).
    // The `ConditionalExpression → true` mutant makes depth = zones.length = 1 (it
    // wrongly counts itself), fabricating a nesting level for a top-level loop.
    const dagSolo: DagPayload = { nodes: [{ id: "(final select)", role: "final" }], edges: [] };
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: {},
      raw_zones: [{ kind: "for_loop", loop: "solo", start: pos(1, 10), end: pos(10, 100), presence: "compiled_in" }],
    };
    const g = rawDagToGraph(dagSolo, cm)!;
    expect(g.zones).toHaveLength(1);
    expect(g.zones[0]!.depth).toBe(0); // a lone loop is top-level (kills depth = zones.length)
  });

  it("the region depth filter EXCLUDES the loop itself + non-for_loop zones (kills L143 j !== zi / kind filter)", () => {
    // an inner for_loop wrapped by an outer for_loop, PLUS an incremental_guard that
    // also strictly wraps the inner by bytes. depth must count ONLY the outer for_loop
    // (j !== zi excludes self; kind === "for_loop" excludes the guard) ⇒ depth 1, not 2.
    const dagF: DagPayload = { nodes: [{ id: "(final select)", role: "final" }], edges: [] };
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: {},
      raw_zones: [
        { kind: "for_loop", loop: "outer", start: pos(1, 10), end: pos(30, 300), presence: "compiled_in" },
        { kind: "for_loop", loop: "inner", start: pos(5, 50), end: pos(10, 100), presence: "compiled_in" },
        // a guard that ALSO strictly wraps inner by bytes — must NOT add to depth.
        { kind: "incremental_guard", loop: "g", start: pos(2, 20), end: pos(20, 200), presence: "compiled_out" },
      ],
    };
    const g = rawDagToGraph(dagF, cm)!;
    const inner = g.zones.find((z) => z.label === "inner")!;
    expect(inner.depth).toBe(1); // ONLY the outer for_loop counts (guard + self excluded)
  });
});

describe("rawDagToGraph — strictWraps requires BOTH spans present (kills L87 o||i / always-true)", () => {
  it("a loop with a MISSING span never wraps (depth stays 0 — kills L87 guard mutants)", () => {
    // the inner loop has start/end; the outer loop is MISSING its span ⇒ strictWraps
    // returns false (the `o && i && o.start && o.end…` guard). The always-true / `o||i`
    // mutants would let a span-less loop "wrap" ⇒ a fabricated nesting depth.
    const dagF: DagPayload = { nodes: [{ id: "(final select)", role: "final" }], edges: [] };
    const cm: CodeMap = {
      raw_dag: { nodes: [] }, raw_node_spans: {},
      raw_zones: [
        { kind: "for_loop", loop: "spanless", presence: "compiled_in" }, // NO start/end
        { kind: "for_loop", loop: "inner", start: pos(2, 20), end: pos(8, 80), presence: "compiled_in" },
      ],
    };
    const g = rawDagToGraph(dagF, cm)!;
    // a span-less loop produces NO region (L140 guard) AND cannot wrap anything.
    expect(g.zones.map((z) => z.label)).toEqual(["inner"]);
    expect(g.zones.find((z) => z.label === "inner")!.depth).toBe(0); // not wrapped by the span-less loop
  });
});

describe("ensureMainNode — the non-empty guard (kills L170)", () => {
  it("does NOT synthesize over a graph that already has nodes (no false main-query claim)", () => {
    // the `if (graph.nodes && graph.nodes.length) return graph` guard. The `false`
    // mutant would OVERWRITE a real one-node graph with a synthetic main node — a
    // fabricated structure. Pin that a present node survives untouched.
    const g = { nodes: [{ id: "real", label: "real", sub: "import", tone: "base" }], edges: [] as [string, string][] };
    const out = ensureMainNode(g, "model.sql");
    expect(out.nodes).toEqual([{ id: "real", label: "real", sub: "import", tone: "base" }]);
    expect(out.nodes.some((n) => n.id === "(final select)")).toBe(false);
  });
});

describe("buildRawSpans — liftRawNodeSpans for_loop guard + computeFinalSpan boundaries (kills L198/L210/L212/L213/L214/L222)", () => {
  const m2 = (raw: string): Pick<ModelPayload, "raw_sql"> => ({ raw_sql: raw });

  it("lifts a for_loop zone span but NOT a non-loop zone (kills L198 z.kind === 'for_loop')", () => {
    // only a for_loop zone becomes a zone:N line span; an incremental_guard zone does
    // NOT (it is a marker, not a block). The `true` mutant would lift a guard zone too.
    const cm: CodeMap = {
      raw_node_spans: { stg: span(1, 0, 2, 20) },
      raw_zones: [
        { kind: "for_loop", start: pos(4, 40), end: pos(6, 60), presence: "compiled_in" },
        { kind: "incremental_guard", start: pos(8, 80), end: pos(9, 90), presence: "compiled_out" },
      ],
    };
    const out = buildRawSpans(m2("a\nb\nc\nd\ne\nf\ng\nh\ni\nj"), cm)!;
    expect(out["zone:0"]).toEqual({ start: { line: 4 }, end: { line: 6 } }); // for_loop lifted
    expect(out["zone:1"]).toBeUndefined(); // incremental_guard NOT lifted as a block span
  });

  it("the (final select) span starts AFTER the last known block, not at it (kills L213 > → >= / lastEnd+1)", () => {
    // last block ends at line 2; the final span must start at 3 (lastEnd + 1), not 2.
    // lines: 1-2 = stg block, 3 content ⇒ final = 3..3.
    const cm: CodeMap = { raw_node_spans: { stg: span(1, 0, 2, 20) } };
    const out = buildRawSpans(m2("l1\nl2\nl3"), cm)!;
    expect(out["(final select)"]).toEqual({ start: { line: 3 }, end: { line: 3 } });
  });

  it("the (final select) span SKIPS blank lines after the last block (kills L214 trim/MethodExpression)", () => {
    // last block ends line 2; lines 3-4 blank; line 5 content ⇒ final span starts 5.
    const cm: CodeMap = { raw_node_spans: { stg: span(1, 0, 2, 20) } };
    const out = buildRawSpans(m2("l1\nl2\n\n\nl5"), cm)!;
    expect(out["(final select)"]).toEqual({ start: { line: 5 }, end: { line: 5 } });
  });

  it("with NO prior blocks the final span starts at line 1 (kills L212/L213 lastEnd seed)", () => {
    // no raw_node_spans, no zones ⇒ lastEnd stays 0 ⇒ fs = 1 ⇒ whole file.
    const out = buildRawSpans(m2("only\nlines\nhere"), { compiled: "" } as CodeMap)!;
    expect(out["(final select)"]).toEqual({ start: { line: 1 }, end: { line: 3 } });
  });

  it("does NOT recompute the final span when an explicit (final select) raw span exists (kills L222)", () => {
    // the `if (!out["(final select)"])` guard with an explicit span that is WITHIN the
    // line bounds, so a recompute WOULD produce a different (non-null) span. lines = 8;
    // explicit final = [3..5]. The real code keeps [3..5]; the `true` / `out[""]`
    // mutants recompute (lastEnd=5 ⇒ fs=6 ⇒ [6..8]) — overwriting the explicit span.
    const cm: CodeMap = { raw_node_spans: { "(final select)": span(3, 30, 5, 50) } };
    const out = buildRawSpans(m2("l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8"), cm)!;
    expect(out["(final select)"]).toEqual({ start: { line: 3 }, end: { line: 5 } }); // NOT [6..8]
  });

  it("the blank-skip TRIMS whitespace-only lines, not just empty ones (kills L214 .trim())", () => {
    // last block ends line 2; line 3 is whitespace-only ("   "); line 4 has content.
    // WITH .trim(): "   ".trim() === "" ⇒ blank ⇒ skipped ⇒ final starts at line 4.
    // WITHOUT .trim() (the MethodExpression mutant): "   " is truthy ⇒ NOT skipped ⇒
    // final wrongly starts at line 3 (a whitespace line claimed as the final block).
    const cm: CodeMap = { raw_node_spans: { stg: span(1, 0, 2, 20) } };
    const out = buildRawSpans(m2("l1\nl2\n   \nl4"), cm)!;
    expect(out["(final select)"]).toEqual({ start: { line: 4 }, end: { line: 4 } });
  });

  it("the SOURCE text drives the final-span EOF — raw_sql is used over compiled (kills L221 raw_sql arm)", () => {
    // raw_sql has 5 lines, compiled has 2. The final span end (EOF) must reflect
    // raw_sql (5), not compiled (2). The `m.raw_sql ?? …` "Stryker" mutant would
    // change the resolved text → wrong line count.
    const cm: CodeMap = { compiled: "c1\nc2" };
    const out = buildRawSpans(m2("r1\nr2\nr3\nr4\nr5"), cm)!;
    expect(out["(final select)"]).toEqual({ start: { line: 1 }, end: { line: 5 } }); // 5 lines from raw_sql
  });

  it("falls back to compiled text when raw_sql is absent (kills L221 compiled arm)", () => {
    // no raw_sql ⇒ resolve from codeMap.compiled (3 lines).
    const cm: CodeMap = { compiled: "c1\nc2\nc3" };
    const out = buildRawSpans({ raw_sql: undefined } as Pick<ModelPayload, "raw_sql">, cm)!;
    expect(out["(final select)"]).toEqual({ start: { line: 1 }, end: { line: 3 } });
  });
});
