// Topology DAG-adapter tests (S6b) — cteDagToGraph + rawGraphToGraphData. Asserts
// the structural mapping (ids preserved so sync resolves over the same keys), the
// role→tone presentation map, and the raw honesty markers carried verbatim.
import { describe, it, expect } from "vitest";
import { cteDagToGraph, rawGraphToGraphData } from "./topology-graphs";
import type { DagPayload } from "./context-data";
import type { RawGraph } from "./data/raw-spans";

describe("cteDagToGraph — compiled CTE DAG → GraphData", () => {
  it("returns an honest-empty graph for an absent/empty dag", () => {
    expect(cteDagToGraph(null)).toEqual({ nodes: [], edges: [] });
    expect(cteDagToGraph(undefined)).toEqual({ nodes: [], edges: [] });
    expect(cteDagToGraph({ nodes: [], edges: [] })).toEqual({ nodes: [], edges: [] });
  });

  it("preserves node ids (so a node click forward-syncs over the same keys)", () => {
    const dag: DagPayload = {
      nodes: [
        { id: "events", label: "events", role: "import" },
        { id: "agg", role: "transform" },
        { id: "(final select)", label: "final", role: "final" },
      ],
      edges: [
        { from: "events", to: "agg", edge_type: "from" },
        { from: "agg", to: "(final select)", edge_type: "inner" },
      ],
    };
    const g = cteDagToGraph(dag);
    expect(g.nodes.map((n) => n.id)).toEqual(["events", "agg", "(final select)"]);
    // edges drop the join type — structural [from,to] tuples.
    expect(g.edges).toEqual([
      ["events", "agg"],
      ["agg", "(final select)"],
    ]);
  });

  it("maps role → structural tone (import=base · transform=cte · final=final)", () => {
    const dag: DagPayload = {
      nodes: [
        { id: "a", role: "import" },
        { id: "b", role: "transform" },
        { id: "c", role: "final" },
      ],
      edges: [],
    };
    const g = cteDagToGraph(dag);
    expect(g.nodes.map((n) => n.tone)).toEqual(["base", "cte", "final"]);
  });

  it("falls back the label to the id when absent", () => {
    const g = cteDagToGraph({ nodes: [{ id: "x", role: "transform" }], edges: [] });
    expect(g.nodes[0]!.label).toBe("x");
  });
});

describe("rawGraphToGraphData — raw DAG → GraphData (honesty markers verbatim)", () => {
  it("returns honest-empty for a null raw graph", () => {
    expect(rawGraphToGraphData(null)).toEqual({ nodes: [], edges: [] });
  });

  it("carries the templated/incremental markers + zone regions verbatim", () => {
    const raw: RawGraph = {
      nodes: [
        { id: "events", label: "events", sub: "import", tone: "base" },
        { id: "zone:0", label: "{% for %}", sub: "templated · 2 CTEs", tone: "cte", templated: true, genCount: 2, presence: "compiled_out" },
        { id: "(final select)", label: "final", sub: "final", tone: "final", hasIncremental: true },
      ],
      edges: [
        ["events", "zone:0"],
        ["zone:0", "(final select)"],
      ],
      zones: [{ id: "z0", zi: 0, label: "for loop", template: null, depth: 0, nodeId: "zone:0", members: ["zone:0"] }],
    };
    const g = rawGraphToGraphData(raw);
    expect(g.nodes.map((n) => n.id)).toEqual(["events", "zone:0", "(final select)"]);
    // the templated zone node carries `templated` (its OWN flag); the final carries
    // hasIncremental. (cute-dbt#497 finding 3.)
    expect(g.nodes.find((n) => n.id === "zone:0")!.templated).toBe(true);
    expect(g.nodes.find((n) => n.id === "(final select)")!.hasIncremental).toBe(true);
    // a {% for %} collapse is NOT an is_incremental strip — it must NEVER be marked
    // incrementalOnly (that would render the incremental-amber treatment = a false
    // honesty claim). cute-dbt#497 finding 3.
    expect(g.nodes.find((n) => n.id === "zone:0")!.incrementalOnly).toBeUndefined();
    // un-templated import node has neither flag (no false claim).
    expect(g.nodes.find((n) => n.id === "events")!.templated).toBeUndefined();
    expect(g.nodes.find((n) => n.id === "events")!.incrementalOnly).toBeUndefined();
    // the zone region survives as a selectable ring.
    expect(g.zones).toEqual([{ id: "z0", label: "for loop", depth: 0, members: ["zone:0"] }]);
  });

  it("omits zones entirely when the raw graph has none", () => {
    const raw: RawGraph = {
      nodes: [{ id: "a", label: "a", sub: "import", tone: "base" }],
      edges: [],
      zones: [],
    };
    expect(rawGraphToGraphData(raw).zones).toBeUndefined();
  });
});
