// The shared graph-engine DOMAIN model (pure; std + types only) — the
// VERBATIM-port of the prototype graph.js geometry/honesty logic, lifted out of
// the React component so it is unit-testable in node (no DOM, no @xyflow/react).
//
// This is the single source of truth for every DAG the app renders (compiled CTE
// DAG, raw DAG, column-lineage DAG, PR-scope lineage): the GraphData shape, the
// confidence 3-state edge resolution (the never-a-false-claim honesty axis), the
// grow-to-fit node width, the MIN_K fit-floor (the anti-flip lower bound), and
// the `nearestInDirection` keyboard cursor geometry (the 1.6× off-axis penalty).
//
// LAYER: domain (pure). The view layer (src/view/graph/*) renders these facts
// onto @xyflow/react; it NEVER recomputes them.

import type { ColumnConfidence } from "./context-data";

// ── the shared node/edge/graph shape every DAG feeds the engine ──────────────

/** A node's change-state stripe color (PR lineage) — the LEFT stripe vocabulary. */
export type ChangeState = "added" | "modified" | "removed" | "deleted" | "context";

/** A node's resource-type glyph (bottom-left) — same vocabulary as the file list. */
export type NodeKind = "model" | "seed" | "macro";

/** A node's materialization glyph (bottom-right). */
export type Materialization = "view" | "table" | "incremental" | null;

/** A structural-tone class (CTE/raw/column DAGs) — presentation, not honesty. */
export type NodeTone = "base" | "cte" | "final" | "modified" | "added" | "context" | "incremental" | string;

/** The shared graph node — the union of every DAG's per-node facts. Every field
 *  beyond `id`/`label` is optional so a CTE node, a column node, and a PR node
 *  all flow through the SAME engine. */
export interface GraphNode {
  id: string;
  label: string;
  sub?: string;
  tone?: NodeTone;
  /** PR-lineage carries `kind` → renders the change-stripe + type/mat glyph node. */
  kind?: NodeKind;
  /** change-state → the LEFT stripe color (PR lineage). */
  change?: ChangeState;
  /** the materialization glyph (PR lineage). */
  mat?: Materialization;
  /** a context/halo/connector node — dimmed, dashed, never selectable. */
  context?: boolean;
  /** an honest-provisional node (model-level cross-model consumer) — dashed. */
  provisional?: boolean;
  /** an honest-provisional cross-model consumer (column lineage). */
  consumer?: boolean;
  /** raw-DAG: this CTE exists ONLY in the raw graph (is_incremental stripped it). */
  incrementalOnly?: boolean;
  /** raw-DAG: this node carries an is_incremental() guard. */
  hasIncremental?: boolean;
}

/** An edge's optional honesty/structural metadata (the 3rd tuple element). */
export interface EdgeMeta {
  /** the honesty 3-state — resolved (quiet) · opaque (amber) · ambiguous (red). */
  confidence?: ColumnConfidence;
  kind?: string;
}

/** A graph edge: [source, target] with optional honesty metadata. */
export type GraphEdge = [string, string] | [string, string, EdgeMeta];

/** A selectable zone region (a {% for %} / incremental guard) — concentric ring. */
export interface GraphZone {
  id: string;
  label: string;
  /** nesting depth (outer = smaller; nested loops render as concentric rings). */
  depth: number;
  /** the member node ids the ring encloses. */
  members: string[];
}

/** The data every DAG feeds the engine. */
export interface GraphData {
  nodes: GraphNode[];
  edges: GraphEdge[];
  zones?: GraphZone[];
}

/** Read an edge's honesty/structural metadata (the optional 3rd tuple element). */
export function edgeMeta(e: GraphEdge): EdgeMeta | undefined {
  return e.length > 2 ? e[2] : undefined;
}

// ── the change-state stripe + confidence-edge vocabularies (honesty axes) ─────

/** change-state → node LEFT-stripe color: added=green · modified=amber ·
 *  removed/deleted=red · context=gray. (graph.js CHANGE_COLOR, verbatim.) */
export const CHANGE_COLOR: Record<ChangeState, string> = {
  added: "var(--role-added, #1a7f37)",
  modified: "var(--role-modified, #e69f00)",
  removed: "var(--danger, #cf222e)",
  deleted: "var(--danger, #cf222e)",
  context: "var(--text-muted, #6c7086)",
};

/** the honesty 3-state edge resolution — resolved is the QUIET baseline (neutral,
 *  solid, like the structural edges); opaque + ambiguous POP (amber / red, dashed)
 *  so the uncertain joins — the ones worth a reviewer's eye — stand out.
 *  (collineage.js CONF, verbatim; the never-a-false-claim axis.) */
export interface ConfidenceStyle { color: string; label: string; dashed: boolean; }
export const CONFIDENCE: Record<ColumnConfidence, ConfidenceStyle> = {
  resolved: { color: "var(--border, #6c7086)", label: "resolved", dashed: false },
  opaque: { color: "var(--role-modified, #e69f00)", label: "opaque", dashed: true },
  ambiguous: { color: "var(--danger, #cf222e)", label: "ambiguous", dashed: true },
};

/** Resolve an edge's confidence style. A missing/unknown confidence is the quiet
 *  resolved baseline (never silently colored as a false uncertainty claim). */
export function confidenceStyle(c: ColumnConfidence | undefined): ConfidenceStyle {
  return (c && CONFIDENCE[c]) || CONFIDENCE.resolved;
}

/** Tally the confidence states present across a graph's edges (legend counts). */
export function confidenceCounts(edges: GraphEdge[]): Partial<Record<ColumnConfidence, number>> {
  const out: Partial<Record<ColumnConfidence, number>> = {};
  for (const e of edges) {
    const c = edgeMeta(e)?.confidence;
    if (c) out[c] = (out[c] ?? 0) + 1;
  }
  return out;
}

/** The ordered confidence states present in a graph (for a stable legend). */
export const CONFIDENCE_ORDER: ColumnConfidence[] = ["resolved", "opaque", "ambiguous"];
export function confidenceLegend(edges: GraphEdge[]): ColumnConfidence[] {
  const counts = confidenceCounts(edges);
  return CONFIDENCE_ORDER.filter((c) => counts[c]);
}

// ── geometry: the node box, the grow-to-fit width, the fit-floor ─────────────

/** the node box (graph.js NODE_W/NODE_H). NODE_W is the minimum/fallback width. */
export const NODE_W = 200;
export const NODE_H = 56;
/** the grow-to-fit ceiling — long templated names read cleanly, never unbounded. */
export const NODE_W_MAX = 680;
/** the scale floor — a fit must NEVER produce a zero/negative (flipped) scale. */
export const MIN_K = 0.08;

/** Per-node width: grow to fit the name at full font (no condensing), from a
 *  comfortable minimum up to a generous max, so long templated names read cleanly
 *  instead of being squished. (graph.js nodeWidth, verbatim.) */
export function nodeWidth(n: GraphNode): number {
  const label = n.label || n.id || "";
  const sub = n.sub || "";
  let right = 16;
  if (n.mat === "incremental") right = 54;
  else if (n.mat) right = 46;
  else if (n.incrementalOnly) right = 66;
  const nameW = 16 + label.length * 7.8 + right;
  const subW = 16 + sub.length * 6.6 + 64;
  return Math.max(NODE_W, Math.min(NODE_W_MAX, Math.round(Math.max(nameW, subW))));
}

/** A positioned node (elk feeds x/y/w; w defaults to the grow-to-fit width). */
export interface PlacedNode extends GraphNode { x: number; y: number; w: number; }

/**
 * nearestInDirection — the keyboard cursor geometry. From the current node,
 * pick the nearest node in `dir` (right/left/up/down), penalizing off-axis
 * distance 1.6× so a near-collinear neighbor wins over a closer-but-skewed one.
 * Nodes behind the cursor (primary ≤ 1) are ignored. (graph.js, verbatim.)
 */
export type NavDir = "right" | "left" | "up" | "down";
export function nearestInDirection(
  nodes: PlacedNode[],
  fromId: string,
  dir: NavDir,
): string | undefined {
  const from = nodes.find((n) => n.id === fromId);
  if (!from) return nodes[0]?.id;
  const fx = from.x + (from.w || NODE_W) / 2;
  const fy = from.y + NODE_H / 2;
  let best: string | undefined;
  let bestScore = Infinity;
  for (const n of nodes) {
    if (n.id === fromId) continue;
    const dx = n.x + (n.w || NODE_W) / 2 - fx;
    const dy = n.y + NODE_H / 2 - fy;
    let primary: number;
    let off: number;
    if (dir === "right") { primary = dx; off = Math.abs(dy); }
    else if (dir === "left") { primary = -dx; off = Math.abs(dy); }
    else if (dir === "down") { primary = dy; off = Math.abs(dx); }
    else { primary = -dy; off = Math.abs(dx); }
    if (primary <= 1) continue;
    const score = primary + off * 1.6;
    if (score < bestScore) { bestScore = score; best = n.id; }
  }
  return best;
}

/** A canvas viewport transform (React Flow's {x,y,zoom} shape). */
export interface Viewport { x: number; y: number; zoom: number; }

/**
 * fitView — the MIN_K-floored fit transform. Computes the {x,y,zoom} that centers
 * the whole node set in a (w×h) canvas with `pad` margin, CLAMPING the padding so
 * `(w − 2·pad)` can never go negative on a small canvas (a negative term is what
 * used to make zoom negative and flip the whole graph 180° — the "upside-down on
 * first paint" bug). zoom is floored at MIN_K and capped at `maxK`. Zones add
 * top-weighted headroom for their rings + legends. (graph.js fitAll, verbatim.)
 *
 * Returns null when the canvas has no real size yet (refit once it does).
 */
export function fitView(
  nodes: PlacedNode[],
  canvas: { w: number; h: number },
  opts: { pad?: number; maxK?: number; hasZones?: boolean } = {},
): Viewport | null {
  if (!nodes.length) return null;
  const { w, h } = canvas;
  if (w < 4 || h < 4) return null; // not laid out yet — refit once it has a real size
  const pad = opts.pad ?? 56;
  const maxK = opts.maxK ?? 1.15;
  let a = Infinity, b = Infinity, c = -Infinity, d = -Infinity;
  for (const n of nodes) {
    a = Math.min(a, n.x);
    b = Math.min(b, n.y);
    c = Math.max(c, n.x + (n.w || NODE_W));
    d = Math.max(d, n.y + NODE_H);
  }
  if (opts.hasZones) { b -= 52; a -= 20; c += 20; d += 18; } // ring + legend headroom
  const gw = c - a, gh = d - b;
  // Clamp pad so (w − 2·pad) and (h − 2·pad) can never go negative → no flip.
  const p = Math.max(0, Math.min(pad, w / 4, h / 4));
  const zoom = Math.max(MIN_K, Math.min((w - 2 * p) / gw, (h - 2 * p) / gh, maxK));
  return { zoom, x: (w - gw * zoom) / 2 - a * zoom, y: (h - gh * zoom) / 2 - b * zoom };
}

/**
 * recenterViewport — the zoom-1 transform that centers a single selected node in
 * a (w×h) canvas (the externally-controlled selection recenter). Returns null on
 * a zero-/sub-pixel canvas: getBoundingClientRect() reports width/height 0 before
 * the container paints, and recentering against a 0-size canvas yields a flipped/
 * incorrect transform — the caller skips the viewport write until a real size
 * lands (the same null-on-zero-rect contract as fitView).
 */
export function recenterViewport(
  node: { x: number; w: number; y: number },
  canvas: { w: number; h: number },
): Viewport | null {
  const { w, h } = canvas;
  if (w <= 0 || h <= 0) return null; // not painted yet — skip until it has a real size
  return { zoom: 1, x: w / 2 - (node.x + node.w / 2), y: h / 2 - 28 - node.y };
}
