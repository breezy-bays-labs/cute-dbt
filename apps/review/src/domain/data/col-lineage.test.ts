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
  it("splitKey on a SEP-less key returns the whole string as the column (no last-char lop)", () => {
    // a malformed (SEP-less) key must NOT silently slice off the last char (the old
    // slice(0, -1) bug). The whole string is the column under an empty node.
    expect(splitKey("no_separator_here")).toEqual({ node: "", column: "no_separator_here" });
  });
  it("splitKey on a key with SEP at index 0 (empty node) splits normally (kills i<0 → i<=0)", () => {
    // keyOf("", "col") puts SEP at index 0. The `i < 0` guard must let it through to
    // the normal split (node "", column "col"). The `i <= 0` mutant would wrongly take
    // the SEP-less branch and return the whole "\x1fcol" as the column.
    const k = keyOf("", "col"); // SEP is at index 0
    expect(splitKey(k)).toEqual({ node: "", column: "col" });
  });
});

describe("buildColLineage — untrusted column-name keys can't pollute the prototype", () => {
  it("a __proto__ output column is a real own-key, not a prototype mutation", () => {
    const cl = { edges: [{
      from_col: { scope: { intra: { node_id: "a" } }, column: "x" },
      to_col: { scope: { intra: { node_id: "b" } }, column: "__proto__" },
      kind: "pass_through", confidence: "resolved",
    }] } as ColumnLineage;
    const out = buildColLineage(cl)!;
    expect(Object.keys(out)).toEqual(["__proto__"]); // an OWN key on a null-proto map
    expect(out["__proto__"]).toHaveLength(1);
    // the global Object prototype is untouched.
    expect(({} as Record<string, unknown>)["polluted"]).toBeUndefined();
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

// ── mutation-hardening: pin the never-a-false-claim honesty folds (cute-dbt#514) ─
// Each test below kills a SPECIFIC surviving mutant in a confidence/terminal/cone
// fold. A surviving mutant in this module = a presence/confidence flip the tests
// don't catch — the load-bearing honesty contract. Comments name the line + mutator.

describe("buildColEdges — the !f || !t guard (a missing from/to ref drops the edge, never a phantom)", () => {
  it("drops an edge whose from_col ref is entirely absent (L82 !f || !t — not !f && !t)", () => {
    // ONLY from_col is missing; to_col is a real column. The `!f || !t` (OR) guard
    // must drop it. The `!f && !t` (AND) mutant would KEEP it (since !t is false),
    // fabricating an edge from undefined.column — a false lineage claim.
    const out = buildColEdges({ edges: [
      { from_col: undefined as never, to_col: { scope: { intra: { node_id: "b" } }, column: "y" }, kind: "derived", confidence: "resolved" },
      edge("a", "x", "b", "keep"),
    ] } as ColumnLineage)!;
    expect(out).toHaveLength(1);
    expect(out[0]!.to.column).toBe("keep");
  });
  it("drops an edge whose to_col ref is entirely absent (symmetric OR arm)", () => {
    const out = buildColEdges({ edges: [
      { from_col: { scope: { intra: { node_id: "a" } }, column: "x" }, to_col: undefined as never, kind: "derived", confidence: "resolved" },
      edge("a", "x", "b", "keep"),
    ] } as ColumnLineage)!;
    expect(out).toHaveLength(1);
    expect(out[0]!.to.column).toBe("keep");
  });
  it("KEEPS an edge when BOTH refs are present (the guard fires on absence, not presence)", () => {
    // pins L82 against the ConditionalExpression→false mutant (which would drop every edge).
    const out = buildColEdges({ edges: [edge("a", "x", "b", "y")] })!;
    expect(out).toHaveLength(1);
  });
});

describe("colTerminal — the `(final select)` / `final` preference is LOAD-BEARING (kills L106 false/'' )", () => {
  it("among multiple sinks, returns the conventional name even when it is NOT sinks[0]", () => {
    // two sinks: "zzz_other" appears FIRST (from edge order), "(final select)" second.
    // The find(...) preference must pick "(final select)" — the `false` mutant would
    // make find() return undefined → fall to sinks[0] = "zzz_other" (a wrong terminal).
    const e = buildColEdges({ edges: [
      edge("a", "x", "zzz_other", "w"),
      edge("a", "x", "(final select)", "z"),
    ] })!;
    expect(colTerminal(e)).toBe("(final select)");
  });
  it("the preference STRING is exactly '(final select)' (kills the L106 '' StringLiteral mutant)", () => {
    // sole sink "(final select)"; the `n === ""` mutant would not match it, but find
    // would still return undefined → sinks[0]. With a SECOND non-conventional sink
    // ordered first, the '' mutant returns the wrong sink; the real code returns the
    // conventional one.
    const e = buildColEdges({ edges: [
      edge("a", "x", "aaa_first", "w"),
      edge("a", "x", "(final select)", "z"),
    ] })!;
    // real code → "(final select)"; both `false` and `""` mutants → "aaa_first".
    expect(colTerminal(e)).not.toBe("aaa_first");
    expect(colTerminal(e)).toBe("(final select)");
  });
});

describe("buildColGraph — root backfill + BFS seeds + terminal/consumer folds (kills L155/167/177/197/198)", () => {
  const edges = buildColEdges({ edges: [
    edge("a", "c1", "b", "c2", "opaque", "derived"),
    edge("b", "c2", "term", "out", "resolved", "pass_through"),
  ] })!;

  it("root backfill: a root ABSENT from the edge set is added as a singleton (L155 !info[rootK])", () => {
    // the `if (!info[rootK])` guard backfills the root so it appears. The `false`/
    // `info[rootK]` (truthy) mutants skip the backfill → the root node vanishes.
    const g = buildColGraph(edges, [], { node: "ghost_node", column: "ghost_col" }, "m", "term");
    expect(g.nodes.map((n) => n.id)).toEqual([keyOf("ghost_node", "ghost_col")]);
  });
  it("root backfill carries the EXACT {node,column} (kills the L155 {} ObjectLiteral mutant)", () => {
    // the backfilled info drives the node's sub label (c.node) + label (c.column).
    // The `{}` mutant backfills an empty object ⇒ sub/label fall to "?"/key, not the
    // real node/column.
    const g = buildColGraph(edges, [], { node: "ghost_node", column: "ghost_col" }, "m", "term");
    const n = g.nodes[0]!;
    expect(n.label).toBe("ghost_col"); // info.column drives the label
    expect(n.sub).toBe("ghost_node");  // info.node drives the sub (non-terminal)
  });

  it("backward cone seed is the ROOT (L167 [rootK] not []) — sources of the terminal are reached", () => {
    // rooting at the terminal, the backward BFS must walk a,b. The `[]` seed mutant
    // visits nothing backward ⇒ the cone loses its upstream sources.
    const g = buildColGraph(edges, [], { node: "term", column: "out" }, "m", "term");
    const ids = new Set(g.nodes.map((n) => n.id));
    expect(ids.has(keyOf("a", "c1"))).toBe(true); // only reachable BACKWARD from term
    expect(ids.has(keyOf("b", "c2"))).toBe(true);
  });
  it("forward cone seed is the ROOT (L177 [rootK] not []) — descendants of an import are reached", () => {
    // rooting at the import a, the forward BFS must walk b,term. The `[]` seed mutant
    // visits nothing forward ⇒ the cone loses its downstream descendants.
    const g = buildColGraph(edges, [], { node: "a", column: "c1" }, "m", "term");
    const ids = new Set(g.nodes.map((n) => n.id));
    expect(ids.has(keyOf("b", "c2"))).toBe(true);   // forward from a
    expect(ids.has(keyOf("term", "out"))).toBe(true);
  });

  it("the terminal-membership filter uses === term (L197 EqualityOperator) — consumers attach ONLY to the terminal", () => {
    // downstream consumers attach to nodes whose info.node === term. The `!== term`
    // mutant would attach them to the NON-terminal nodes (a false downstream claim
    // on an import/cte). Assert the consumer edge targets the terminal key.
    const g = buildColGraph(edges, ["audit_x"], { node: "term", column: "out" }, "m", "term");
    const consumerNode = g.nodes.find((n) => n.consumer)!;
    const edgesToConsumer = g.edges.filter((e) => e[1] === consumerNode.id);
    expect(edgesToConsumer).toHaveLength(1);
    expect(edgesToConsumer[0]![0]).toBe(keyOf("term", "out")); // attached to the TERMINAL only
  });
  it("consumers attach ONLY when finals.length AND downstream.length (L198 && not ||)", () => {
    // BOTH conditions true ⇒ attach.
    const both = buildColGraph(edges, ["d1"], { node: "term", column: "out" }, "m", "term");
    expect(both.nodes.some((n) => n.consumer)).toBe(true);
    // downstream EMPTY (finals present) ⇒ the `&&` short-circuits ⇒ NO consumers.
    // The `||` mutant would attach on finals.length alone (a false downstream claim
    // when there is no downstream model).
    const noDownstream = buildColGraph(edges, [], { node: "term", column: "out" }, "m", "term");
    expect(noDownstream.nodes.some((n) => n.consumer)).toBe(false);
    // finals EMPTY (root is a ghost not reaching any terminal) ⇒ no consumers even
    // with downstream present (the `finals.length` arm of the &&).
    const noFinals = buildColGraph(edges, ["d1"], { node: "ghost", column: "z" }, "m", "term");
    expect(noFinals.nodes.some((n) => n.consumer)).toBe(false);
  });
  it("a consumer is attached for EACH downstream model (L202 forEach body not no-op)", () => {
    const g = buildColGraph(edges, ["d1", "d2", "d3"], { node: "term", column: "out" }, "m", "term");
    const consumers = g.nodes.filter((n) => n.consumer).map((n) => n.label).sort();
    expect(consumers).toEqual(["d1", "d2", "d3"]);
    // and each is wired from the terminal.
    expect(g.edges.filter((e) => e.length === 2)).toHaveLength(3);
  });

  it("the dedup id uses the fk>tk separator so DISTINCT edges are NOT collapsed (L160 '>' not '')", () => {
    // two DISTINCT edges (a→b and b→term). The real `fk + ">" + tk` id keeps them
    // both; the `""` mutant makes every id "" ⇒ the seen-set drops the second edge
    // (a silent edge loss — a missing lineage claim).
    const g = buildColGraph(edges, [], { node: "term", column: "out" }, "m", "term");
    const coneEdges = g.edges.filter((e) => e.length === 3);
    expect(coneEdges).toHaveLength(2);
    const pairs = coneEdges.map((e) => e[0] + "→" + e[1]).sort();
    expect(pairs).toEqual([
      keyOf("a", "c1") + "→" + keyOf("b", "c2"),
      keyOf("b", "c2") + "→" + keyOf("term", "out"),
    ].sort());
  });
});
