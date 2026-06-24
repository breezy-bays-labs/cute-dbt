// The HIGHEST-COMPLEXITY reshapers, extracted as a separately-tested module (the
// council strict honesty-fold set): rawDagToGraph + buildRawSpans + ensureMainNode.
//
// These reconstruct the RAW-side DAG + line-spans from the §3a code_map spine
// (byte-span parsing). The byte-span fuzz target (raw-spans.fuzz.test.ts) treats
// rawDagToGraph as the TS analog of the Rust --pr-diff bolero target: random
// inputs must NEVER throw and NEVER fabricate a span outside the source bounds.
//
// VERBATIM PORT of prototype/context.js (rawDagToGraph / buildRawSpans /
// ensureMainNode) — same control flow, typed + honesty-preserving.
//
// stop-gap labels (greppable; each → a Track-2 issue under epic #486):
//   tracked: cute-dbt#510 — B3/B9: raw_node_spans omits `(final select)` + the
//     templated zone:N ids for some models; buildRawSpans computes the final span
//     by heuristic (first non-blank after the last span → EOF). Delete once the
//     spine emits raw_node_spans for EVERY dag.nodes[].id.
//   tracked: cute-dbt#511 — B4: ensureMainNode synthesizes a main-query node for
//     CTE-less models that ship an empty compiled `dag`. Delete once the spine
//     emits the terminal node in m.dag for main-query-only models.

import type { CodeMap, DagNode, ModelPayload, NodeRole, RawZone, SourcePos } from "../context-data";

const ROLE_TONE: Record<string, string> = { import: "base", transform: "cte", final: "final" };

/** A reshaped raw-DAG graph node. `id`/`label`/`sub`/`tone` are presentation;
 * the zone* markers + presence carry the §3a honesty facts. */
export interface RawGraphNode {
  id: string;
  label: string;
  sub: string;
  tone: string;
  templated?: boolean;
  genCount?: number;
  presence?: RawZone["presence"];
  zoneKind?: string;
  zonePresence?: RawZone["presence"];
  hasIncremental?: boolean;
}
export interface RawZoneRegion {
  id: string;
  zi: number;
  label: string;
  template: string | null;
  startLine?: number;
  endLine?: number;
  depth: number;
  nodeId: string | null;
  members: string[];
}
export interface RawGraph {
  nodes: RawGraphNode[];
  edges: [string, string][];
  zones: RawZoneRegion[];
}

type DagLike = { nodes?: DagNode[]; edges?: { from: string; to: string }[] } | null | undefined;

const tone = (role?: NodeRole): string => (role ? ROLE_TONE[role] ?? "cte" : "cte");

/**
 * Build the RAW DAG for a model. Structure comes from the compiled `dag` (so the
 * final/main node + edges are present — the spine's raw_dag omits the final
 * node); each raw Jinja zone becomes a MARKER on the node whose raw byte span
 * contains it, else the final/main node. A {% for %} fan-out collapses the N
 * compiled CTEs into ONE raw template node. Honest-null when there's no raw spine.
 */
export function rawDagToGraph(dag: DagLike, codeMap: CodeMap | null | undefined): RawGraph | null {
  if (!dag || !dag.nodes || !codeMap || !codeMap.raw_dag) return null;
  let nodes: RawGraphNode[] = dag.nodes.map((n) => ({
    id: n.id, label: n.label ?? n.id, sub: n.role, tone: tone(n.role),
  }));
  let edges: [string, string][] = (dag.edges ?? []).map((e) => [e.from, e.to]);
  const rawSpans = codeMap.raw_node_spans ?? {};
  // node_map.raw values are string[] (the generated CTE ids); normalize a stray
  // string arm (the asymmetric compiled side never reaches here) to an array.
  const nodeMapRawSrc = (codeMap.node_map && codeMap.node_map.raw) ?? {};
  const genOf = (id: string): string[] => {
    const v = nodeMapRawSrc[id];
    return v == null ? [] : Array.isArray(v) ? v : [v];
  };
  const finalNode = nodes.find((n) => n.sub === "final") ?? nodes[nodes.length - 1];
  const inside = (z: RawZone, sp?: { start: SourcePos; end: SourcePos }): boolean =>
    !!(sp && sp.start && sp.end && z.start && z.end
      && z.start.byte >= sp.start.byte && z.end.byte <= sp.end.byte);
  const zones = codeMap.raw_zones ?? [];
  const strictWraps = (o: RawZone, i: RawZone): boolean =>
    !!(o && i && o.start && o.end && i.start && i.end
      && o.start.byte <= i.start.byte && o.end.byte >= i.end.byte
      && !(o.start.byte === i.start.byte && o.end.byte === i.end.byte));

  // raw byte span of each FINAL graph node (null-proto map: a stray `__proto__`
  // span key in untrusted input can't pollute the chain).
  const rawByte: Record<string, { s: number; e: number }> = Object.create(null) as Record<string, { s: number; e: number }>;
  for (const id of Object.keys(rawSpans)) {
    const s = rawSpans[id];
    if (s && s.start && s.end) rawByte[id] = { s: s.start.byte, e: s.end.byte };
  }

  zones.forEach((z, zi) => {
    const generated = genOf("zone:" + zi);
    if (z.kind === "for_loop" && generated.length) {
      // a {% for %} that GENERATES CTEs → collapse the N compiled CTEs into ONE
      // raw template node + rewire the 1→N fan-out through it.
      const gen = new Set(generated);
      const seen = new Set<string>();
      edges = edges
        .map(([a, b]): [string, string] => [gen.has(a) ? "zone:" + zi : a, gen.has(b) ? "zone:" + zi : b])
        .filter(([a, b]) => a !== b && !seen.has(a + ">" + b) && (seen.add(a + ">" + b), true));
      nodes = nodes.filter((n) => !gen.has(n.id));
      nodes.push({
        id: "zone:" + zi, label: z.template ?? "{% for %} template",
        sub: "templated · " + generated.length + " CTEs", tone: "cte",
        templated: true, genCount: generated.length, presence: z.presence,
      });
      if (z.start && z.end) rawByte["zone:" + zi] = { s: z.start.byte, e: z.end.byte };
    } else if (z.kind === "for_loop") {
      // a WRAPPER {% for %} (generates no CTE of its own): no node, no marker —
      // it surfaces as a REGION below.
    } else {
      // inline non-loop zone ({% if is_incremental() %} on a WHERE, etc.) → a
      // MARKER on the node whose raw span contains it, else the final/main node.
      let host: RawGraphNode | undefined;
      for (const id of Object.keys(rawSpans)) {
        if (inside(z, rawSpans[id])) { host = nodes.find((n) => n.id === id); break; }
      }
      if (!host) host = finalNode;
      if (host) {
        host.zoneKind = z.kind;
        host.zonePresence = z.presence;
        if (z.kind === "incremental_guard") host.hasIncremental = true;
      }
    }
  });

  // zone regions: one selectable, shaded area per {% for %} loop.
  const within = (i?: { s: number; e: number }, o?: { s: number; e: number }): boolean =>
    !!(i && o && i.s >= o.s && i.e <= o.e);
  const regionZones: RawZoneRegion[] = [];
  zones.forEach((z, zi) => {
    if (z.kind !== "for_loop" || !z.start || !z.end) return;
    const zb = { s: z.start.byte, e: z.end.byte };
    const members = nodes.filter((n) => rawByte[n.id] && within(rawByte[n.id], zb)).map((n) => n.id);
    const depth = zones.filter((o, j) => j !== zi && o.kind === "for_loop" && strictWraps(o, z)).length;
    regionZones.push({
      id: "z" + zi, zi, label: z.loop ?? "for loop", template: z.template ?? null,
      startLine: z.start.line, endLine: z.end.line, depth,
      nodeId: genOf("zone:" + zi).length ? "zone:" + zi : null,
      members,
    });
  });

  return { nodes, edges, zones: regionZones };
}

/** A minimal graph shape ensureMainNode mutates in place (raw OR compiled). */
export interface MainNodeGraph {
  nodes: { id: string; label: string; sub: string; tone: string }[];
  edges: [string, string][];
}

/**
 * CTE-less models ship an EMPTY compiled `dag`; the structure lives only in
 * raw_dag's terminal. Synthesize one main-query node (titled with the filename)
 * so the graph shows the model instead of rendering blank.
 *
 * tracked: cute-dbt#511 — B4: delete once the spine emits the terminal node.
 */
export function ensureMainNode<G extends MainNodeGraph | RawGraph | null>(graph: G, fileName: string): G {
  if (!graph) return graph;
  if (graph.nodes && graph.nodes.length) return graph;
  graph.nodes = [{ id: "(final select)", label: fileName, sub: "main query", tone: "final" }];
  graph.edges = [];
  return graph;
}

/** A unified raw line-span: 1-based start/end line for a graph node's whole block. */
export interface LineSpan { start: { line: number }; end: { line: number }; }

/**
 * Unified RAW line-spans for every graph node, so File/Diff can tint a node's
 * whole block + place the cursor. Covers imports/CTEs (raw_node_spans), templated
 * loops (raw_zones → zone:N), and the final/main query (COMPUTED: after the last
 * block to EOF, or the whole file for a CTE-less model). Honest-null when empty.
 *
 * tracked: cute-dbt#510 — B3/B9: the `(final select)` computation is a heuristic;
 * delete once raw_node_spans is emitted for every dag node.
 */
/** Lift raw_node_spans + for_loop zones to a node→line-span map (null-proto so an
 * untrusted `__proto__` span key can't pollute the chain). */
function liftRawNodeSpans(codeMap: CodeMap): Record<string, LineSpan> {
  const out: Record<string, LineSpan> = Object.create(null) as Record<string, LineSpan>;
  const rns = codeMap.raw_node_spans ?? {};
  for (const id of Object.keys(rns)) {
    const s = rns[id];
    if (s && s.start && s.end) out[id] = { start: { line: s.start.line }, end: { line: s.end.line } };
  }
  (codeMap.raw_zones ?? []).forEach((z, zi) => {
    if (z.kind === "for_loop" && z.start && z.end) {
      out["zone:" + zi] = { start: { line: z.start.line }, end: { line: z.end.line } };
    }
  });
  return out;
}

/** The (final select) span heuristic: first non-blank line after the last known
 * block → EOF. Returns null when no final span should be added.
 * tracked: cute-dbt#510 — B3. */
function computeFinalSpan(out: Record<string, LineSpan>, lines: string[]): LineSpan | null {
  const total = lines.length;
  if (!total) return null;
  let lastEnd = 0;
  for (const id of Object.keys(out)) { const sp = out[id]; if (sp) lastEnd = Math.max(lastEnd, sp.end.line); }
  let fs = lastEnd > 0 ? lastEnd + 1 : 1;
  while (fs < total && !(lines[fs - 1] ?? "").trim()) fs++;
  return total >= fs ? { start: { line: fs }, end: { line: total } } : null;
}

export function buildRawSpans(m: Pick<ModelPayload, "raw_sql">, codeMap: CodeMap | null | undefined): Record<string, LineSpan> | null {
  if (!codeMap) return null;
  const out = liftRawNodeSpans(codeMap);
  const lines = String(m.raw_sql ?? codeMap.compiled ?? "").replace(/\n$/, "").split("\n");
  if (!out["(final select)"]) {
    const fin = computeFinalSpan(out, lines);
    if (fin) out["(final select)"] = fin;
  }
  return Object.keys(out).length ? out : null;
}
