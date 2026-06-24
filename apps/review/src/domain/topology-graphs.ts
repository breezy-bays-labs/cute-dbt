// Topology DAG adapters (S6b) — the two pure converters that feed the shared
// graph engine (src/view/graph/LineageGraph → domain/graph-model GraphData) for
// the topology panes' compiled⇄raw DAG toggle:
//   • cteDagToGraph     — the COMPILED CTE DAG (DagPayload) → GraphData.
//   • rawGraphToGraphData — the RAW DAG (raw-spans.ts RawGraph) → GraphData,
//                           carrying the §3a presence/zone honesty markers.
//
// Neither recomputes a honesty fact: the compiled side is structural (role→tone
// is presentation), and the raw side carries `presence`/`templated` VERBATIM from
// rawDagToGraph (which the strict honesty-fold set already tests).
//
// LAYER: domain (pure; the data contract + graph-model + raw-spans only).
import type { DagPayload, NodeRole } from "./context-data";
import type { GraphData, GraphNode, GraphZone, NodeTone } from "./graph-model";
import type { RawGraph } from "./data/raw-spans";

/** Compiled CTE-DAG role → structural tone class (presentation only — the import/
 *  transform/final triad the shared ToneNode stripes). */
const ROLE_TONE: Record<NodeRole, NodeTone> = {
  import: "base",
  transform: "cte",
  final: "final",
  cte: "cte",
  zone: "cte",
  terminal: "final",
};

/**
 * cteDagToGraph — the COMPILED CTE DAG → the shared engine's GraphData. Node ids
 * are the dag node ids (the SAME keys `code_map.node_spans` uses), so a node click
 * forward-syncs straight through the cursor-sync machine. Edges drop the join
 * `edge_type` (the compiled-CTE pane is structural; confidence styling belongs to
 * the raw/column DAGs). Honest-empty graph for an absent/empty dag.
 */
export function cteDagToGraph(dag: DagPayload | null | undefined): GraphData {
  if (!dag || !dag.nodes) return { nodes: [], edges: [] };
  const nodes: GraphNode[] = dag.nodes.map((n) => ({
    id: n.id,
    label: n.label ?? n.id,
    sub: n.role,
    tone: ROLE_TONE[n.role] ?? "cte",
  }));
  const edges: GraphData["edges"] = (dag.edges ?? []).map((e) => [e.from, e.to]);
  return { nodes, edges };
}

/**
 * rawGraphToGraphData — the RAW DAG (rawDagToGraph output) → the shared engine's
 * GraphData. Carries the §3a honesty markers VERBATIM onto their OWN engine flags:
 * `templated` (a {% for %} collapse) → `templated`, and `hasIncremental` (an
 * is_incremental guard) → `hasIncremental` — the two are DISTINCT facts and are
 * NEVER conflated (a loop collapse is not an is_incremental strip; cute-dbt#497
 * finding 3). The zone regions become selectable concentric rings (GraphZone).
 * Node ids stay the raw ids (`zone:N` for collapsed loops) so the raw cursor-sync
 * resolves over the SAME keys buildSyncMaps emits.
 */
export function rawGraphToGraphData(raw: RawGraph | null | undefined): GraphData {
  if (!raw) return { nodes: [], edges: [] };
  const nodes: GraphNode[] = raw.nodes.map((n) => ({
    id: n.id,
    label: n.label,
    sub: n.sub,
    tone: n.tone as NodeTone,
    // `templated` (a {% for %} collapse) is carried as its OWN flag — NOT remapped
    // onto `incrementalOnly` (an is_incremental strip), which would render a loop
    // collapse with the incremental-amber treatment = a FALSE honesty claim
    // (cute-dbt#497 finding 3). is_incremental() is carried separately via
    // `hasIncremental`.
    templated: n.templated || undefined,
    hasIncremental: n.hasIncremental,
  }));
  const edges: GraphData["edges"] = raw.edges.map(([a, b]) => [a, b]);
  const zones: GraphZone[] | undefined = raw.zones.length
    ? raw.zones.map((z) => ({ id: z.id, label: z.label, depth: z.depth, members: z.members }))
    : undefined;
  return { nodes, edges, zones };
}
