// Column-lineage reshaper unit tests — the confidence honesty axis + the
// cross-model blast radius + the inferred terminal + the 0x1F key delimiter.
import { describe, expect, it } from "vitest";
import type { ColumnLineage } from "../context-data";
import type { ColumnRef } from "../context-data";
import {
  buildColEdges, buildColGraph, buildColLineage, colTerminal, inferTerminal, keyOf, nodeOf, SEP, splitKey,
} from "./col-lineage";

const edge = (fn: string | null, fc: string, tn: string | null, tc: string, conf = "resolved", kind = "pass_through") => ({
  from_col: { scope: fn == null ? { intra: { node_id: "" } } : { intra: { node_id: fn } }, column: fc },
  to_col: { scope: tn == null ? { intra: { node_id: "" } } : { intra: { node_id: tn } }, column: tc },
  confidence: conf as "resolved" | "opaque" | "ambiguous", kind: kind as "pass_through",
});

describe("the 0x1F key delimiter (EXACT)", () => {
  it("SEP is the unit-separator U+001F", () => { expect(SEP).toBe(""); expect(SEP.charCodeAt(0)).toBe(0x1f); });
  it("keyOf joins node + column with SEP; a null node ⇒ ?", () => {
    expect(keyOf("orders", "status")).toBe("ordersstatus");
    expect(keyOf(null, "x")).toBe("?x");
  });
  it("splitKey round-trips a column name that contains any non-0x1F char", () => {
    const k = keyOf("model.a", "a.b/c-d");
    expect(splitKey(k)).toEqual({ node: "model.a", column: "a.b/c-d" });
  });
});

describe("buildColLineage — per-output-column source fold", () => {
  it("null on no edges (honest empty)", () => {
    expect(buildColLineage(null)).toBeNull();
    expect(buildColLineage({ edges: [] })).toBeNull();
  });
  it("folds edges by output column, defaulting kind + confidence", () => {
    const cl: ColumnLineage = { edges: [edge("stg", "id", "orders", "order_id"), edge("stg", "amt", "orders", "total")] };
    const out = buildColLineage(cl)!;
    expect(Object.keys(out).sort()).toEqual(["order_id", "total"]);
    expect(out["order_id"]).toEqual([{ node: "stg", column: "id", kind: "pass_through", confidence: "resolved" }]);
  });
  it("preserves the opaque/ambiguous confidence verbatim (never defaults over it)", () => {
    const cl: ColumnLineage = { edges: [edge("a", "x", "b", "y", "opaque")] };
    expect(buildColLineage(cl)!["y"]![0]!.confidence).toBe("opaque");
  });
  it("defaults an empty kind→pass_through, empty confidence→resolved, empty column→?", () => {
    const cl = { edges: [{
      from_col: { scope: { intra: { node_id: "a" } }, column: "" }, // empty source column ⇒ ?
      to_col: { scope: { intra: { node_id: "b" } }, column: "y" },
      kind: "" as never, confidence: "" as never, // empty ⇒ defaults
    }] } as ColumnLineage;
    const src = buildColLineage(cl)!["y"]![0]!;
    expect(src).toEqual({ node: "a", column: "?", kind: "pass_through", confidence: "resolved" });
  });
  it("skips an edge with no output column (to_col.column falsy)", () => {
    const cl = { edges: [
      { from_col: { scope: { intra: { node_id: "a" } }, column: "x" }, to_col: { scope: { intra: { node_id: "b" } }, column: "" }, kind: "derived", confidence: "resolved" },
      edge("a", "x", "b", "real"),
    ] } as ColumnLineage;
    const out = buildColLineage(cl)!;
    expect(Object.keys(out)).toEqual(["real"]); // the empty-output edge is dropped
  });
});

describe("buildColEdges — normalized per-edge list", () => {
  it("null on no edges", () => { expect(buildColEdges(null)).toBeNull(); expect(buildColEdges({ edges: [] })).toBeNull(); });
  it("normalizes from/to + carries confidence", () => {
    const out = buildColEdges({ edges: [edge("a", "x", "b", "y", "ambiguous", "derived")] })!;
    expect(out).toEqual([{ from: { node: "a", column: "x" }, to: { node: "b", column: "y" }, kind: "derived", confidence: "ambiguous" }]);
  });
  it("defaults empty kind→pass_through + empty confidence→resolved", () => {
    const out = buildColEdges({ edges: [{ from_col: { scope: { intra: { node_id: "a" } }, column: "x" }, to_col: { scope: { intra: { node_id: "b" } }, column: "y" }, kind: "" as never, confidence: "" as never }] } as ColumnLineage)!;
    expect(out[0]).toMatchObject({ kind: "pass_through", confidence: "resolved" });
  });
  it("skips an edge missing a from/to column (the !fc || !tc guard)", () => {
    const out = buildColEdges({ edges: [
      { from_col: { scope: { intra: { node_id: "a" } }, column: "" }, to_col: { scope: { intra: { node_id: "b" } }, column: "y" }, kind: "derived", confidence: "resolved" },
      edge("a", "x", "b", "keep"),
    ] } as ColumnLineage)!;
    expect(out).toHaveLength(1);
    expect(out[0]!.to.column).toBe("keep");
  });
});

describe("colTerminal / inferTerminal — the inferred sink", () => {
  it("null on no edges", () => { expect(colTerminal(null)).toBeNull(); expect(colTerminal([])).toBeNull(); });
  it("the sink is the to-node never used as a from-node", () => {
    const edges = buildColEdges({ edges: [edge("a", "x", "mid", "y"), edge("mid", "y", "final", "z")] })!;
    expect(colTerminal(edges)).toBe("final");
  });
  it('prefers the conventional "(final select)" / "final" name among multiple sinks', () => {
    const edges = buildColEdges({ edges: [edge("a", "x", "(final select)", "z"), edge("a", "x", "other_sink", "w")] })!;
    expect(colTerminal(edges)).toBe("(final select)");
  });
  it("inferTerminal === colTerminal", () => {
    const edges = buildColEdges({ edges: [edge("a", "x", "final", "z")] })!;
    expect(inferTerminal(edges)).toBe(colTerminal(edges));
  });
});

describe("buildColGraph — the rooted cone + provisional cross-model blast radius", () => {
  const edges = buildColEdges({ edges: [
    edge("stg_orders", "status", "mid", "status2"),
    edge("mid", "status2", "final", "status_variety"),
  ] })!;
  it("empty graph on no edges", () => {
    expect(buildColGraph(null, [], { node: "final", column: "x" }, "m")).toEqual({ nodes: [], edges: [] });
  });
  it("builds the backward + forward cone rooted at {node,column}", () => {
    const g = buildColGraph(edges, [], { node: "final", column: "status_variety" }, "orders_model", "final");
    const ids = g.nodes.map((n) => n.id).sort();
    expect(ids).toEqual([keyOf("final", "status_variety"), keyOf("mid", "status2"), keyOf("stg_orders", "status")].sort());
    // the terminal node carries the model label + final tone.
    const finalN = g.nodes.find((n) => n.id === keyOf("final", "status_variety"))!;
    expect(finalN.tone).toBe("final"); expect(finalN.sub).toBe("orders_model");
    // an import (no in-edge) gets the base tone.
    expect(g.nodes.find((n) => n.id === keyOf("stg_orders", "status"))!.tone).toBe("base");
    // edges carry the confidence + kind meta.
    expect(g.edges.every((e) => e.length === 3)).toBe(true);
  });
  it("attaches provisional model-level downstream consumers to the terminal (honest-provisional)", () => {
    const g = buildColGraph(edges, ["orders_audit"], { node: "final", column: "status_variety" }, "orders_model", "final");
    const consumer = g.nodes.find((n) => n.consumer)!;
    expect(consumer).toMatchObject({ label: "orders_audit", provisional: true, consumer: true, sub: "downstream model" });
    expect(consumer.id).toBe("modelorders_audit");
  });
  it("no consumers attached when downstream is empty", () => {
    const g = buildColGraph(edges, [], { node: "final", column: "status_variety" }, "m", "final");
    expect(g.nodes.some((n) => n.consumer)).toBe(false);
  });
});

// ── mutation-hardening: precise identity/branch assertions on the folds ───────

describe("nodeOf — intra-first, inter-fallback, honest-null", () => {
  it("prefers the intra scope node_id", () => {
    expect(nodeOf({ scope: { intra: { node_id: "a" }, inter: { node_id: "b" } }, column: "x" })).toBe("a");
  });
  it("falls back to inter when intra is absent (the cross-model arm)", () => {
    expect(nodeOf({ scope: { inter: { node_id: "b" } }, column: "x" } as ColumnRef)).toBe("b");
  });
  it("null when neither scope is present / the ref is null", () => {
    expect(nodeOf({ scope: {}, column: "x" } as ColumnRef)).toBeNull();
    expect(nodeOf(null)).toBeNull();
    expect(nodeOf(undefined)).toBeNull();
  });
});

describe("buildColGraph — exact cone membership + tones + edge meta (kills traversal mutants)", () => {
  // a > b > c (terminal), plus a side branch d > c, so the backward cone of c is {a,b,d}.
  const edges = buildColEdges({ edges: [
    edge("a", "c1", "b", "c2", "opaque", "derived"),
    edge("b", "c2", "term", "out", "resolved", "pass_through"),
    edge("d", "c4", "term", "out", "ambiguous", "renamed"),
  ] })!;

  it("the backward+forward cone rooted at the terminal contains EXACTLY a,b,d,term", () => {
    const g = buildColGraph(edges, [], { node: "term", column: "out" }, "model_x", "term");
    const ids = new Set(g.nodes.map((n) => n.id));
    expect(ids).toEqual(new Set([keyOf("a", "c1"), keyOf("b", "c2"), keyOf("d", "c4"), keyOf("term", "out")]));
  });
  it("the terminal node has tone=final + sub=model; imports tone=base; mid tone=cte", () => {
    const g = buildColGraph(edges, [], { node: "term", column: "out" }, "model_x", "term");
    const byId = Object.fromEntries(g.nodes.map((n) => [n.id, n]));
    expect(byId[keyOf("term", "out")]).toMatchObject({ tone: "final", sub: "model_x", label: "out" });
    expect(byId[keyOf("a", "c1")]!.tone).toBe("base"); // no in-edge ⇒ import
    expect(byId[keyOf("d", "c4")]!.tone).toBe("base");
    expect(byId[keyOf("b", "c2")]!.tone).toBe("cte"); // has an in-edge ⇒ intermediate
  });
  it("every cone edge carries the verbatim confidence + kind meta (3-tuple)", () => {
    const g = buildColGraph(edges, [], { node: "term", column: "out" }, "model_x", "term");
    const meta = g.edges.map((e) => e[2]).filter(Boolean);
    expect(meta).toContainEqual({ confidence: "opaque", kind: "derived" });
    expect(meta).toContainEqual({ confidence: "ambiguous", kind: "renamed" });
    expect(meta).toContainEqual({ confidence: "resolved", kind: "pass_through" });
  });
  it("rooting on an IMPORT yields only its forward cone (a → b → term)", () => {
    const g = buildColGraph(edges, [], { node: "a", column: "c1" }, "model_x", "term");
    const ids = new Set(g.nodes.map((n) => n.id));
    // d is NOT reachable forward from a (it's a sibling source of term) — but term IS.
    expect(ids.has(keyOf("a", "c1"))).toBe(true);
    expect(ids.has(keyOf("b", "c2"))).toBe(true);
    expect(ids.has(keyOf("term", "out"))).toBe(true);
    expect(ids.has(keyOf("d", "c4"))).toBe(false);
  });
  it("a root not present in the edge set yields a singleton node (info backfilled)", () => {
    const g = buildColGraph(edges, [], { node: "ghost", column: "z" }, "m", "term");
    expect(g.nodes.map((n) => n.id)).toEqual([keyOf("ghost", "z")]);
    expect(g.edges).toEqual([]);
  });
  it("deduplicates a repeated edge (the seen-set guard)", () => {
    const dup = buildColEdges({ edges: [edge("a", "x", "term", "y"), edge("a", "x", "term", "y")] })!;
    const g = buildColGraph(dup, [], { node: "term", column: "y" }, "m", "term");
    expect(g.edges.filter((e) => e[0] === keyOf("a", "x") && e[1] === keyOf("term", "y"))).toHaveLength(1);
  });
});

describe("buildColGraph — fallback + consumer-attach conditions (kills remaining logic mutants)", () => {
  const edges = buildColEdges({ edges: [edge("a", "x", "term", "out")] })!;
  it("infers the terminal when none is passed (terminal ?? inferTerminal)", () => {
    const g = buildColGraph(edges, [], { node: "term", column: "out" }, "m"); // no terminal arg
    // the term node is still tagged final via the inferred terminal.
    expect(g.nodes.find((n) => n.id === keyOf("term", "out"))!.tone).toBe("final");
  });
  it("attaches consumers ONLY when BOTH finals exist AND downstream is non-empty", () => {
    // downstream present but the cone has a terminal ⇒ consumers attach.
    const withDown = buildColGraph(edges, ["d1", "d2"], { node: "term", column: "out" }, "m", "term");
    expect(withDown.nodes.filter((n) => n.consumer).map((n) => n.label).sort()).toEqual(["d1", "d2"]);
    // downstream present but NO terminal in cone (root is the import, term not reached
    // backward) — still reaches term forward, so finals exist; flip to empty downstream:
    const noDown = buildColGraph(edges, [], { node: "term", column: "out" }, "m", "term");
    expect(noDown.nodes.some((n) => n.consumer)).toBe(false);
  });
  it("a `?` source column (missing) still produces a node labeled with the column key", () => {
    const e = buildColEdges({ edges: [{ from_col: { scope: {} as never, column: "src" }, to_col: { scope: { intra: { node_id: "term" } }, column: "out" }, kind: "derived", confidence: "resolved" }] } as ColumnLineage)!;
    const g = buildColGraph(e, [], { node: "term", column: "out" }, "m", "term");
    // the from node resolves to null ⇒ keyOf(null,...) ⇒ "?"-prefixed id present.
    expect(g.nodes.some((n) => n.id.startsWith("?"))).toBe(true);
  });
});

describe("colTerminal — the conventional-name preference + equality (kills === mutants)", () => {
  it("returns null when there are no sinks (every node is also a source — a cycle-ish set)", () => {
    // a→b, b→a: both are froms ⇒ no pure sink.
    const cyc = buildColEdges({ edges: [edge("a", "x", "b", "y"), edge("b", "y", "a", "x")] })!;
    expect(colTerminal(cyc)).toBeNull();
  });
  it("falls back to the first sink when neither (final select) nor final exists", () => {
    const e = buildColEdges({ edges: [edge("a", "x", "sink_z", "y")] })!;
    expect(colTerminal(e)).toBe("sink_z");
  });
  it("prefers `final` when present alongside another sink", () => {
    const e = buildColEdges({ edges: [edge("a", "x", "other", "w"), edge("a", "x", "final", "z")] })!;
    expect(colTerminal(e)).toBe("final");
  });
});
