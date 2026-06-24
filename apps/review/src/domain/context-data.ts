// TS mirror of cute-dbt's embedded report payload (ReportPayload), i.e. the
// review app's view of cute-dbt's "context" dataset. Field names + enum CASINGS
// verified against the real committed goldens (comments-showcase →
// context.sample.json; the PR-440 dogfood → context.440.json).
//
// Optional keys use serde skip_serializing_if (absent ⇒ undefined). This is the
// HAND-AUTHORED contract; the Zod schema in ./schema.ts is the drift gate that
// validates real `--context-out` output against it. When the Rust S3a
// `--context-out` artifact lands, realign these against the emitted schema with a
// codegen drift gate (ts-rs/typeshare — a later nicety).

export type EdgeType =
  | "from" | "inner" | "left" | "right" | "full" | "cross"
  | "union" | "union_all" | "union_distinct";
export type NodeRole = "import" | "transform" | "final";
export type DiffLineKind = "context" | "removed" | "added";
// Verified against context.440.json (the Zod gate surfaced `added`/`deleted`
// beyond the original `new`/`modified`). `removed_models` is the node-less list;
// `deleted`/`added` here are the in-payload model-state chips.
export type ModelState = "new" | "modified" | "added" | "deleted";
export type ThreadSide = "Left" | "Right";            // Left = old/deleted side, Right = new/added side
export type PrDagState = "new" | "modified" | "deleted";

export interface ContextData {
  baseline: string;
  models: ModelPayload[];
  pr_comments?: CommentsView;
  pr_dag?: PrDagPayload;
  pr_ref?: { number: number; title: string; url: string };
  removed_models?: string[];
  // broad-surface sections (present in context.440.json / the full dogfood) — typed loosely:
  manifest_nodes?: Record<string, unknown>;
  check_specs?: Record<string, unknown>;
  macro_lens?: unknown;
  governance?: unknown;
  project_definition?: unknown;
  project_change_panel?: unknown;
  seed_cards?: unknown[];
}

export interface ModelPayload {
  name: string;
  path?: string;                 // project-relative models/…/x.sql — the comment JOIN KEY
  description?: string;
  dag: DagPayload;
  // COMPILED SQL split PER CTE: a map keyed by CTE-DAG node id (the SAME ids as
  // `dag.nodes[].id`). Join the values in `dag.nodes` order for the whole model.
  compiled_sql: Record<string, string>;
  // RAW SQL: the model's full Jinja source verbatim.
  raw_sql?: string;
  sql_diff?: BlockDiff;          // present only in --pr-diff mode
  model_yaml?: ModelYamlPayload;
  tests?: TestPayload[];          // omitted on a `deleted` model
  is_recursive?: boolean | null;  // omitted/null on a `deleted` model (no compiled structure)
  is_incremental?: boolean;
  findings?: FindingPayload[];
  axes?: Axes;                   // per-model change attribution
  state?: ModelState;
  config_file?: string;
  config_attributions?: unknown[];
  // §3a lineage spine (omit-when-empty) — typed loosely at S0; tightened in later slices.
  code_map?: unknown;
  column_lineage?: unknown;
}

export interface Axes { body: boolean; config: boolean; unit_test: boolean; }

export interface ModelYamlPayload {
  path?: string;
  raw?: string;                  // the authoring YAML block (verbatim)
  diff?: BlockDiff;              // present if the model's own YAML block changed
  missing?: boolean;
}

export interface BlockDiff { lines: DiffLine[]; }
export interface DiffLine {
  kind: DiffLineKind;
  text: string;                  // \r-trimmed, no sigil
  emphasis: [number, number] | null;  // intra-line word-diff span (codepoints), or null
}

export interface TestPayload {
  id: string;
  name: string;
  target_model: string;
  changed: boolean;
  description?: string;
  tags?: string[];
  meta?: unknown;
  defined_in?: string;           // original_file_path of the test's YAML
  authoring_yaml?: string;
  yaml_diff?: BlockDiff;         // the test's own YAML block edit
  data_diff?: UnitTestDataDiff;  // cell-level given/expect diff
  is_incremental_mode?: boolean;
  given: GivenPayload[];
  expected: ExpectedPayload;
}
export interface UnitTestDataDiff { given: unknown; expect: unknown; }
export type GivenPayload = Record<string, unknown>;
export type ExpectedPayload = Record<string, unknown>;
export type FindingPayload = Record<string, unknown>;

export interface DagPayload { nodes: NodePayload[]; edges: EdgePayload[]; }
export interface NodePayload { id: string; label?: string; role: NodeRole; }
export interface EdgePayload { from: string; to: string; edge_type: EdgeType; }

export interface CommentsView {
  by_model: ModelCommentBucket[];
  unanchored?: RenderedThread[];
  total: number;
}
export interface ModelCommentBucket {
  model: string; model_path: string; count: number; threads: RenderedThread[];
}
export interface RenderedThread {
  model?: string;
  path: string;                  // project-relative file the thread anchors to
  line?: number | null;          // live 1-based line on `side`; null/absent when outdated
  original_line?: number;        // "was on line N" for outdated
  side: ThreadSide;
  within_hunk: boolean;
  resolved: boolean;
  outdated: boolean;
  comments: RenderedComment[];
}
export interface RenderedComment { author: string | null; body: string; }  // author null ⇒ ghost

export interface PrDagPayload extends PrDagView {
  by_axis?: Record<"body" | "config" | "unit_test", PrDagView>;
}
export interface PrDagView {
  graph: PrDagGraph;
  modified_count: number; connector_count: number; halo_count: number; deleted_count: number;
  collapsed: boolean;
}
export interface PrDagGraph { nodes: PrDagNode[]; edges: { from: string; to: string }[]; }
export interface PrDagNode {
  id: string; name: string;
  state: PrDagState;
  is_connector: boolean; is_halo?: boolean;
  lines_added: number; lines_removed: number;
}
