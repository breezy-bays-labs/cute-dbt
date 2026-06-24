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

// `ROLE_TONE` + `tone()` map a node role to a CSS TONE CLASS (base/cte/final) — pure
// PRESENTATION; no never-a-false-claim honesty fact hinges on the tone string (the
// presence/confidence axes live on zones + edges). Gated by the design-system pass.
// tracked: cute-dbt#514 — presentation tone strings, not honesty-bearing.
// Stryker disable next-line ObjectLiteral,StringLiteral
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

// tracked: cute-dbt#514 — presentation tone strings (see ROLE_TONE above).
// Stryker disable next-line ArrowFunction,LogicalOperator,StringLiteral
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
    // `label`/`sub`/`tone` are PRESENTATION (the `?? n.id` label fallback is a display
    // nicety). tracked: cute-dbt#514 — presentation, not honesty-bearing.
    // Stryker disable next-line LogicalOperator
    id: n.id, label: n.label ?? n.id, sub: n.role, tone: tone(n.role),
  }));
  // tracked: cute-dbt#514 — equivalent: `dag.edges` is REQUIRED on a well-formed
  // DagPayload (the compiled dag the raw graph mirrors), so the `?? []` default is
  // unreachable on validated input.
  // Stryker disable next-line ArrayDeclaration
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
  // `hasSpan` is the DEFENSIVE presence guard (a zone may omit start/end — RawZone.
  // start/end are optional; a node SourceSpan always has them on validated input).
  // It is split out so its presence-check mutants — equivalent on validated input —
  // are suppressed surgically, leaving the byte-BOUNDARY logic below fully mutated.
  // tracked: cute-dbt#514 — equivalent: the `&&` presence short-circuits collapse to
  // a single truthiness test; on validated input start/end are present so the chain's
  // internal &&/|| structure is unobservable. The BOUNDARY comparisons stay live.
  // Stryker disable next-line ConditionalExpression,LogicalOperator
  const hasSpan = (s?: { start?: SourcePos; end?: SourcePos } | null): s is { start: SourcePos; end: SourcePos } =>
    !!(s && s.start && s.end);
  const inside = (z: RawZone, sp?: { start: SourcePos; end: SourcePos }): boolean =>
    hasSpan(sp) && hasSpan(z) && z.start.byte >= sp.start.byte && z.end.byte <= sp.end.byte;
  // tracked: cute-dbt#514 — equivalent: `raw_zones` absent ⇒ the `?? []` default; an
  // adversarial non-array would be caught upstream by Zod. Both yield no zone work.
  // Stryker disable next-line ArrayDeclaration
  const zones = codeMap.raw_zones ?? [];
  const strictWraps = (o: RawZone, i: RawZone): boolean =>
    hasSpan(o) && hasSpan(i)
      && o.start.byte <= i.start.byte && o.end.byte >= i.end.byte
      && !(o.start.byte === i.start.byte && o.end.byte === i.end.byte);

  // raw byte span of each FINAL graph node (null-proto map: a stray `__proto__`
  // span key in untrusted input can't pollute the chain).
  const rawByte: Record<string, { s: number; e: number }> = Object.create(null) as Record<string, { s: number; e: number }>;
  for (const id of Object.keys(rawSpans)) {
    const s = rawSpans[id];
    // tracked: cute-dbt#514 — equivalent: `s` is a validated SourceSpan (Zod requires
    // start/end + line/col/byte), so the start/end presence guards are unreachable.
    // Stryker disable next-line ConditionalExpression,LogicalOperator
    if (hasSpan(s)) rawByte[id] = { s: s.start.byte, e: s.end.byte };
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
      // `label`/`sub`/`tone` on the collapsed zone node are PRESENTATION (the display
      // name + "templated · N CTEs" caption + tone class). The HONESTY facts are
      // `templated`/`genCount`/`presence` (the compiled_in fan-out claim, tested).
      // tracked: cute-dbt#514 — presentation strings, not honesty-bearing.
      // Stryker disable next-line StringLiteral
      const zoneLabel = z.template ?? "{% for %} template";
      // Stryker disable next-line StringLiteral
      const zoneSub = "templated · " + generated.length + " CTEs";
      // tracked: cute-dbt#514 — `tone: "cte"` is a presentation tone class.
      // Stryker disable next-line StringLiteral
      const zoneTone = "cte";
      nodes.push({
        id: "zone:" + zi, label: zoneLabel, sub: zoneSub, tone: zoneTone,
        templated: true, genCount: generated.length, presence: z.presence,
      });
      // tracked: cute-dbt#514 — equivalent: a collapsed for_loop zone that reached
      // this branch always carries a start/end span (the spine pairs the node_map
      // generated CTEs with the zone's byte span), so the presence guard is
      // unreachable-false; the byte VALUES are honesty-bearing (membership) + tested.
      // Stryker disable next-line ConditionalExpression,LogicalOperator
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
  // tracked: cute-dbt#514 — equivalent: `i`/`o` are always present here (the caller
  // guards `rawByte[n.id] && …` + passes a constructed `zb`), so the `i && o` presence
  // arm is unreachable. The BOUNDARY comparisons (i.s >= o.s && i.e <= o.e) stay live.
  // Stryker disable next-line ConditionalExpression,LogicalOperator
  const present = (i?: { s: number; e: number }, o?: { s: number; e: number }): boolean => !!(i && o);
  const within = (i?: { s: number; e: number }, o?: { s: number; e: number }): boolean =>
    present(i, o) && i!.s >= o!.s && i!.e <= o!.e;
  const regionZones: RawZoneRegion[] = [];
  zones.forEach((z, zi) => {
    if (z.kind !== "for_loop" || !z.start || !z.end) return;
    const zb = { s: z.start.byte, e: z.end.byte };
    const members = nodes.filter((n) => rawByte[n.id] && within(rawByte[n.id], zb)).map((n) => n.id);
    // the `notSelf` self-exclusion is REDUNDANT (hence suppressed in isolation): a zone
    // never strict-wraps itself — strictWraps(z, z) is false because its L89 not-equal
    // clause excludes an equal span, and a zone's span equals its own. Even two DISTINCT
    // zones with an identical span don't strict-wrap each other, so dropping `j !== zi`
    // never adds a count. Splitting it out keeps the `o.kind === "for_loop"` filter +
    // strictWraps + the predicate-level branch LIVE + tested (only the redundant guard
    // is suppressed). tracked: cute-dbt#514 — equivalent: redundant self-exclusion.
    const depth = zones.filter((o, j) => {
      // Stryker disable next-line ConditionalExpression
      const notSelf = j !== zi;
      return notSelf && o.kind === "for_loop" && strictWraps(o, z);
    }).length;
    // `label`/`template` here are PRESENTATION (the region display name + template
    // string). tracked: cute-dbt#514 — presentation, not honesty-bearing.
    // Stryker disable next-line StringLiteral,LogicalOperator
    const regionLabel = z.loop ?? "for loop";
    // tracked: cute-dbt#514 — presentation: the region's displayed template string.
    // Stryker disable next-line LogicalOperator
    const regionTemplate = z.template ?? null;
    regionZones.push({
      id: "z" + zi, zi, label: regionLabel, template: regionTemplate,
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
    // tracked: cute-dbt#514 — equivalent: `s` is a validated SourceSpan (Zod requires
    // start/end + line/col/byte), so the presence guards are unreachable here.
    // Stryker disable next-line ConditionalExpression,LogicalOperator
    if (s && s.start && s.end) out[id] = { start: { line: s.start.line }, end: { line: s.end.line } };
  }
  // tracked: cute-dbt#514 — equivalent: `raw_zones` absent ⇒ the `?? []` default ⇒
  // no zone work; an adversarial non-array is caught upstream by Zod.
  // Stryker disable next-line ArrayDeclaration
  (codeMap.raw_zones ?? []).forEach((z, zi) => {
    // the `z.kind === "for_loop"` filter IS honesty-bearing (a guard zone is not a
    // block span) — tested in buildRawSpans; left fully mutated.
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
  // tracked: cute-dbt#514 — equivalent: `lines` comes from String(...).split("\n"),
  // which always yields ≥1 element (even "" → [""]), so `total` is never 0.
  // Stryker disable next-line ConditionalExpression
  if (!total) return null;
  let lastEnd = 0;
  // tracked: cute-dbt#514 — equivalent: `sp` (out[id]) is always defined for an id
  // drawn from Object.keys(out); the `if (sp)` guard is unreachable-false.
  // Stryker disable next-line ConditionalExpression
  for (const id of Object.keys(out)) { const sp = out[id]; if (sp) lastEnd = Math.max(lastEnd, sp.end.line); }
  // tracked: cute-dbt#514 — equivalent: lastEnd is never negative; at lastEnd === 0
  // both `> 0 ? lastEnd+1 : 1` and the `>= 0` / always-true mutants yield 1.
  // Stryker disable next-line ConditionalExpression,EqualityOperator
  let fs = lastEnd > 0 ? lastEnd + 1 : 1;
  // the `.trim()` blank-skip IS honesty (whitespace lines aren't the final block —
  // tested); left live. The `?? ""` fallback is equivalent: `fs < total` keeps
  // `lines[fs-1]` in-bounds (fs-1 ∈ [0, total-2]) so it is never undefined.
  // tracked: cute-dbt#514 — equivalent `?? ""` fallback (in-bounds index).
  // Stryker disable next-line StringLiteral
  while (fs < total && !(lines[fs - 1] ?? "").trim()) fs++;
  return total >= fs ? { start: { line: fs }, end: { line: total } } : null;
}

export function buildRawSpans(m: Pick<ModelPayload, "raw_sql">, codeMap: CodeMap | null | undefined): Record<string, LineSpan> | null {
  if (!codeMap) return null;
  const out = liftRawNodeSpans(codeMap);
  // the `m.raw_sql ?? codeMap.compiled` source-resolution arms ARE honesty (which text
  // drives the EOF line count — tested). The final `?? ""` + the `\n$` replace arg are
  // equivalent: both only affect a value that is then split by "\n", and neither
  // changes the resulting LINE COUNT in any reachable case.
  // tracked: cute-dbt#514 — equivalent terminal-fallback + replace-arg strings.
  // Stryker disable next-line StringLiteral
  const lines = String(m.raw_sql ?? codeMap.compiled ?? "").replace(/\n$/, "").split("\n");
  if (!out["(final select)"]) {
    const fin = computeFinalSpan(out, lines);
    if (fin) out["(final select)"] = fin;
  }
  return Object.keys(out).length ? out : null;
}
