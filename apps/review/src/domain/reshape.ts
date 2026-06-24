// The reshape layer — pure reshapers from cute-dbt's "context" dataset onto the
// app's render surfaces. The context already did the compile/diff/anchor work, so
// these are pure reshapes with NO recompute. (Verbatim port of the design
// harness's context.ts.)

import type {
  ContextData, BlockDiff, RenderedThread, ThreadSide, DagPayload, EdgeType,
  PrDagPayload, PrDagView,
} from "./context-data";

// ── Pierre side translation (THE deletion-comment fix) ───────────────────────
// cute-dbt RenderedThread.side: "Left"=old/deleted line, "Right"=new/added line.
// Pierre AnnotationSide: "deletions" | "additions".
export function pierreSide(side: ThreadSide): "deletions" | "additions" {
  return side === "Left" ? "deletions" : "additions";
}

// A thread is mountable inline only when it anchors to a live line.
export function isLiveThread(t: RenderedThread): boolean {
  return t.line != null && !t.outdated;
}

// ── One shared BlockDiff → unified-patch serializer ──────────────────────────
const SIGIL: Record<BlockDiff["lines"][number]["kind"], string> = {
  context: " ", removed: "-", added: "+",
};

export function blockDiffToPatch(diff: BlockDiff, path: string): string {
  let oldCount = 0, newCount = 0;
  for (const l of diff.lines) {
    if (l.kind !== "added") oldCount++;
    if (l.kind !== "removed") newCount++;
  }
  const header =
    `diff --git a/${path} b/${path}\n` +
    `--- a/${path}\n+++ b/${path}\n` +
    `@@ -1,${oldCount} +1,${newCount} @@\n`;
  const body = diff.lines.map((l) => SIGIL[l.kind] + l.text).join("\n");
  return header + body + "\n";
}

// A synthetic all-context patch for a file with no diff (renders the source unchanged).
export function sourceToContextPatch(src: string, path: string): string {
  const lines = src.replace(/\n$/, "").split("\n");
  const header =
    `diff --git a/${path} b/${path}\n--- a/${path}\n+++ b/${path}\n` +
    `@@ -1,${lines.length} +1,${lines.length} @@\n`;
  return header + lines.map((t) => " " + t).join("\n") + "\n";
}

// ── A ReviewContext = a node + every file that changed for it ────────────────
export type CtxLang = "sql" | "yaml" | "csv";
export interface CtxFile {
  path: string;
  lang: CtxLang;
  patch: string;
  threads: RenderedThread[];
}
export interface ReviewContext {
  id: string;
  kind: "model";
  name: string;
  path?: string;
  files: CtxFile[];
}

export function buildContexts(context: ContextData): ReviewContext[] {
  const threadsByPath = new Map<string, RenderedThread[]>();
  const add = (t: RenderedThread) => {
    if (t.path == null) return; // an unanchored/path-less thread isn't file-joined
    const arr = threadsByPath.get(t.path) ?? [];
    arr.push(t);
    threadsByPath.set(t.path, arr);
  };
  for (const b of context.pr_comments?.by_model ?? []) b.threads.forEach(add);
  (context.pr_comments?.unanchored ?? []).forEach(add);
  const take = (p?: string): RenderedThread[] => (p ? threadsByPath.get(p) ?? [] : []);

  return context.models.map((m) => {
    const files: CtxFile[] = [];

    const sqlPath = m.path ?? `${m.name}.sql`;
    if (m.sql_diff) {
      files.push({ path: sqlPath, lang: "sql", patch: blockDiffToPatch(m.sql_diff, sqlPath), threads: take(sqlPath) });
    } else if (m.raw_sql) {
      files.push({ path: sqlPath, lang: "sql", patch: sourceToContextPatch(m.raw_sql, sqlPath), threads: take(sqlPath) });
    }

    const yamlPath = m.model_yaml?.path;
    if (yamlPath) {
      if (m.model_yaml?.diff) {
        files.push({ path: yamlPath, lang: "yaml", patch: blockDiffToPatch(m.model_yaml.diff, yamlPath), threads: take(yamlPath) });
      } else if (m.model_yaml?.raw) {
        files.push({ path: yamlPath, lang: "yaml", patch: sourceToContextPatch(m.model_yaml.raw, yamlPath), threads: take(yamlPath) });
      }
    }

    for (const t of m.tests ?? []) {
      if (t.yaml_diff && t.defined_in) {
        files.push({ path: t.defined_in, lang: "yaml", patch: blockDiffToPatch(t.yaml_diff, t.defined_in), threads: take(t.defined_in) });
      }
    }

    return { id: `model.${m.name}`, kind: "model" as const, name: m.name, path: m.path, files };
  });
}

// ── React Flow adapters ──────────────────────────────────────────────────────
export const EDGE_TYPES: readonly EdgeType[] =
  ["from", "inner", "left", "right", "full", "cross", "union", "union_all", "union_distinct"] as const;

export const EDGE_COLOR: Record<EdgeType, string> = {
  from: "#1c1c1f",          // black — structural seed, not relational
  inner: "#009E73",         // green — strict row match
  left: "#0072B2",          // blue — keep-left
  right: "#56B4E9",         // light blue — keep-right
  full: "#982c61",          // magenta — keep-both
  cross: "#CC79A7",         // pink — cartesian
  union: "#E69F00",         // orange (dashed) — bare UNION (row concatenation)
  union_all: "#E69F00",     // orange (dashed) — row concatenation
  union_distinct: "#E69F00",
};
export const EDGE_DASHED: Record<EdgeType, boolean> = {
  from: false, inner: false, left: false, right: false, full: false, cross: false,
  union: true, union_all: true, union_distinct: true,
};

export interface FlowNode {
  id: string;
  type: string;
  position: { x: number; y: number };
  data: Record<string, unknown>;
}
export interface FlowEdge {
  id: string;
  source: string;
  target: string;
  data?: Record<string, unknown>;
  style?: Record<string, unknown>;
}

export function toFlow(dag: DagPayload): { nodes: FlowNode[]; edges: FlowEdge[]; legend: EdgeType[] } {
  return {
    nodes: dag.nodes.map((n) => ({
      id: n.id,
      type: "cteNode",
      position: { x: 0, y: 0 },
      data: { label: n.label ?? n.id, role: n.role },
    })),
    edges: dag.edges.map((e) => ({
      id: `${e.from}->${e.to}`,
      source: e.from,
      target: e.to,
      data: { edgeType: e.edge_type },
      style: { stroke: EDGE_COLOR[e.edge_type], strokeDasharray: EDGE_DASHED[e.edge_type] ? "6 4" : undefined },
    })),
    legend: EDGE_TYPES.filter((t) => dag.edges.some((e) => e.edge_type === t)),
  };
}

export function toPrFlow(
  pr: PrDagPayload,
  axis?: "body" | "config" | "unit_test",
): { counts: Record<string, number | boolean>; nodes: FlowNode[]; edges: FlowEdge[] } {
  const view: PrDagView = axis && pr.by_axis ? pr.by_axis[axis] : pr;
  return {
    counts: {
      modified: view.modified_count, connector: view.connector_count,
      halo: view.halo_count, deleted: view.deleted_count, collapsed: view.collapsed,
    },
    nodes: view.graph.nodes.map((n) => ({
      id: n.id,
      type: "prNode",
      position: { x: 0, y: 0 },
      data: {
        label: n.name, state: n.state, isConnector: n.is_connector, isHalo: !!n.is_halo,
        linesAdded: n.lines_added, linesRemoved: n.lines_removed,
      },
    })),
    edges: view.graph.edges.map((e) => ({ id: `${e.from}->${e.to}`, source: e.from, target: e.to })),
  };
}
