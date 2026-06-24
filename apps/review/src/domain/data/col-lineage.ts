// The column-lineage reshapers — a strict honesty-fold module (the confidence
// axis + the cross-model blast-radius + the inferred terminal). VERBATIM PORT of
// prototype/context.js (buildColLineage / buildColEdges / colTerminal) +
// collineage.js (inferTerminal / buildColGraph).
//
// The  (0x1F unit-separator) column-key delimiter is preserved EXACTLY: it
// joins a node id + a column name into a single graph-node id (a column name can
// contain any char EXCEPT 0x1F, so the split is unambiguous).
//
// stop-gap labels (greppable; each → a Track-2 issue under epic #486):
//   tracked: cute-dbt#508 — B1: column_lineage.edges are INTRA-model only; the
//     downstream blast radius is approximated from MODEL-level pr_dag edges and
//     rendered as honest-provisional dashed "downstream model" nodes. Delete once
//     the spine emits inter-model column edges + a downstream column index.
//   tracked: cute-dbt#509 — B2: the terminal column-node id is INFERRED (the
//     unique sink), because models name it inconsistently ("(final select)" vs
//     "final", and `customers` has BOTH). Delete once the spine emits a stable
//     role:"terminal" marker on the terminal column node.

import type { ColumnConfidence, ColumnEdge, ColumnEdgeKind, ColumnLineage, ColumnRef } from "../context-data";

// The 0x1F unit-separator column-key delimiter — EXACT.
export const SEP = "";
export const keyOf = (node: string | null | undefined, col: string): string =>
  (node == null ? "?" : node) + SEP + col;
export const splitKey = (k: string): { node: string; column: string } => {
  const i = k.indexOf(SEP);
  return { node: k.slice(0, i), column: k.slice(i + 1) };
};

/** Resolve a column ref's owning node id — intra-model first, then inter-model
 * (the cross-model arm). Honest-null when neither scope is present. */
export const nodeOf = (c: ColumnRef | undefined | null): string | null =>
  (c && c.scope && c.scope.intra && c.scope.intra.node_id)
  ?? (c && c.scope && c.scope.inter && c.scope.inter.node_id) ?? null;

/** A per-output-column source: which upstream (node.column) contributes to it. */
export interface ColSource {
  node: string | null;
  column: string;
  kind: ColumnEdgeKind;
  confidence: ColumnConfidence; // HONESTY AXIS — defaulted to "resolved", never bool
}

/**
 * column lineage → a per-output-column map of its upstream contributing columns.
 * Honest-null when the model ships none (no fold → the compact chart degrades).
 */
export function buildColLineage(cl: ColumnLineage | null | undefined): Record<string, ColSource[]> | null {
  if (!cl || !cl.edges || !cl.edges.length) return null;
  const byOut: Record<string, ColSource[]> = {};
  cl.edges.forEach((e) => {
    const out = e.to_col && e.to_col.column;
    if (!out) return;
    (byOut[out] = byOut[out] ?? []).push({
      node: nodeOf(e.from_col),
      column: (e.from_col && e.from_col.column) || "?",
      kind: e.kind || "pass_through",
      confidence: e.confidence || "resolved",
    });
  });
  return Object.keys(byOut).length ? byOut : null;
}

/** A normalized column edge (node.column → node.column). */
export interface ColEdge {
  from: { node: string | null; column: string };
  to: { node: string | null; column: string };
  kind: ColumnEdgeKind;
  confidence: ColumnConfidence; // HONESTY AXIS — defaulted to "resolved", never bool
}

/**
 * column lineage → the raw per-edge list for the field-level column-lineage DAG.
 * Distinct from buildColLineage (the per-output-column fold). Honest-null when none.
 */
export function buildColEdges(cl: ColumnLineage | null | undefined): ColEdge[] | null {
  if (!cl || !cl.edges || !cl.edges.length) return null;
  const out: ColEdge[] = [];
  cl.edges.forEach((e: ColumnEdge) => {
    const f = e.from_col, t = e.to_col;
    if (!f || !t) return;
    const fc = f.column, tc = t.column;
    if (!fc || !tc) return;
    out.push({
      from: { node: nodeOf(f), column: fc },
      to: { node: nodeOf(t), column: tc },
      kind: e.kind || "pass_through",
      confidence: e.confidence || "resolved",
    });
  });
  return out.length ? out : null;
}

/**
 * terminal (output) node id for a model's column edges — the sink the model's
 * columns surface from. Named inconsistently ("(final select)" vs "final");
 * detect it as the to-node never used as a from-node.
 *
 * tracked: cute-dbt#509 — B2: inferred; delete once the spine emits role:"terminal".
 */
export function colTerminal(edges: ColEdge[] | null | undefined): string | null {
  if (!edges || !edges.length) return null;
  const froms = new Set(edges.map((e) => e.from.node));
  const sinks = [...new Set(edges.map((e) => e.to.node))].filter((n) => !froms.has(n));
  return sinks.find((n) => n === "(final select)" || n === "final") ?? sinks[0] ?? null;
}

// ── collineage.js port: inferTerminal + buildColGraph ────────────────────────

/** The terminal column-node id (collineage.js inferTerminal — same as colTerminal). */
export function inferTerminal(edges: ColEdge[] | null | undefined): string | null {
  return colTerminal(edges);
}

export interface ColGraphNode {
  id: string;
  label: string;
  sub: string;
  tone: string;
  provisional?: boolean;
  consumer?: boolean;
}
export type ColGraphEdge = [string, string] | [string, string, { confidence: ColumnConfidence; kind: ColumnEdgeKind }];
export interface ColGraph { nodes: ColGraphNode[]; edges: ColGraphEdge[]; }

/**
 * Build the cone rooted at {node,column}: backward (sources) + forward (intra
 * descendants), then attach cross-model consumers to every terminal node reached.
 * Never fabricates: every node/edge comes from `edges`. The downstream consumers
 * are honest-provisional (model-level), tagged `consumer: true`.
 *
 * tracked: cute-dbt#508 — B1: cross-model column edges pending; downstream is
 * model-level provisional.
 */
export function buildColGraph(
  edges: ColEdge[] | null | undefined,
  downstream: string[] | null | undefined,
  root: { node: string; column: string },
  model: string | null | undefined,
  terminal?: string | null,
): ColGraph {
  if (!edges || !edges.length) return { nodes: [], edges: [] };
  const term = terminal ?? inferTerminal(edges);
  const inAdj: Record<string, { k: string; e: ColEdge }[]> = {};
  const outAdj: Record<string, { k: string; e: ColEdge }[]> = {};
  const info: Record<string, { node: string | null; column: string }> = {};
  edges.forEach((e) => {
    const fk = keyOf(e.from.node, e.from.column), tk = keyOf(e.to.node, e.to.column);
    info[fk] = e.from; info[tk] = e.to;
    (outAdj[fk] = outAdj[fk] ?? []).push({ k: tk, e });
    (inAdj[tk] = inAdj[tk] ?? []).push({ k: fk, e });
  });
  const rootK = keyOf(root.node, root.column);
  if (!info[rootK]) info[rootK] = { node: root.node, column: root.column };
  const keep = new Set<string>([rootK]);
  const used: ColGraphEdge[] = [];
  const seen = new Set<string>();
  const addE = (fk: string, tk: string, e: ColEdge): void => {
    const id = fk + ">" + tk;
    if (seen.has(id)) return;
    seen.add(id);
    used.push([fk, tk, { confidence: e.confidence, kind: e.kind }]);
  };
  // backward cone — every field that contributes to the root.
  let fr = [rootK];
  const vb = new Set([rootK]);
  while (fr.length) {
    const k = fr.pop() as string;
    (inAdj[k] ?? []).forEach(({ k: fk, e }) => {
      addE(fk, k, e); keep.add(fk);
      if (!vb.has(fk)) { vb.add(fk); fr.push(fk); }
    });
  }
  // forward cone — intra-model descendants down to the terminal select.
  fr = [rootK];
  const vf = new Set([rootK]);
  while (fr.length) {
    const k = fr.pop() as string;
    (outAdj[k] ?? []).forEach(({ k: tk, e }) => {
      addE(k, tk, e); keep.add(tk);
      if (!vf.has(tk)) { vf.add(tk); fr.push(tk); }
    });
  }
  const isImport = (k: string): boolean => !(inAdj[k] && inAdj[k].length);
  const nodes: ColGraphNode[] = [...keep].map((k) => {
    const c = info[k] ?? { node: "?", column: k };
    const isFinal = c.node === term;
    return {
      id: k, label: c.column, sub: isFinal ? (model || "output") : (c.node ?? "?"),
      tone: isFinal ? "final" : isImport(k) ? "base" : "cte",
    };
  });
  // cross-model blast radius — model-level only (honest-provisional). Attached to
  // every terminal node present in the cone.
  // tracked: cute-dbt#508 — B1
  const finals = [...keep].filter((k) => (info[k] ?? {}).node === term);
  if (finals.length && downstream && downstream.length) {
    downstream.forEach((m) => {
      const ck = "model" + SEP + m;
      nodes.push({ id: ck, label: m, sub: "downstream model", tone: "cte", provisional: true, consumer: true });
      finals.forEach((fk) => used.push([fk, ck]));
    });
  }
  return { nodes, edges: used };
}
