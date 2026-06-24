// The COMPLETE TS mirror of cute-dbt's context payload (the `data` member of the
// S3a `ContextEnvelope` — `src/domain/context_envelope.rs`). This is the
// HAND-AUTHORED data contract; the Zod schema in ./schema.ts is the drift gate
// that validates real `--context-out` output against it.
//
// Field names + enum CASINGS verified against the real committed goldens
// (context.440.json = the 16-model PR-440 dogfood; context.sample.json =
// comments-showcase; context.440.since-review.json = the thin since-review).
//
// CONTRACT INVARIANTS (the never-a-false-claim spine — enforced by the ts-morph
// fitness function in domain/honesty-fitness.test.ts):
//   - The FOUR honesty axes are STRING-LITERAL UNIONS, never booleans:
//       PRESENCE     = "compiled_in" | "compiled_out" | "structural"
//       CONFIDENCE   = "resolved" | "opaque" | "ambiguous"
//       COVERAGE     = "covered" | "uncovered" | "unknown"
//       CELL key.t   = "absent" | "null" | "number" | "str"
//     A future downgrade of any of these to a `boolean` fails the fitness gate.
//   - Optional keys use serde `skip_serializing_if` on the Rust side (absent ⇒
//     undefined here). Per the S3a absent→honesty mapping: an absent KEY means
//     "honest-empty" — it never overrides an honesty enum, which (when its
//     carrier object is present) is ALWAYS present.
//
// Every field the prototype reads is typed here, PLUS fields this app does not
// consume (is_recursive / axes / config_attributions / config_file /
// var_references on models; bound_to_node / is_this / column_meta / ordinal on
// tests) so the Zod gate never false-rejects real `--context-out` output.

// ── The four honesty axes (string-literal unions, NEVER bool) ────────────────

/** PRESENCE (source-map) — `crate::domain::source_map::Presence`. A compiled
 * span that is honestly `None` this build is `compiled_out`, NOT an absent key. */
export type Presence = "compiled_in" | "compiled_out" | "structural";

/** CONFIDENCE (column-edge) — `crate::domain::cte::ColumnEdgeConfidence`. The
 * uncertain joins (opaque/ambiguous) are the reviewer-worthy ones. */
export type ColumnConfidence = "resolved" | "opaque" | "ambiguous";

/** COVERAGE (finding verdict) — `crate::domain::checks::Verdict.status`. */
export type CoverageStatus = "covered" | "uncovered" | "unknown";

/** CELL key.t (the NULL / absent / empty trichotomy + typed value) — a cell's
 * key discriminator. `absent` (no cell) ≠ `null` (a SQL NULL) ≠ a typed value
 * (`number`/`str`, whose empty string is the empty-but-present case). */
export type CellKeyType = "absent" | "null" | "number" | "str";

// ── Other closed wire enums ──────────────────────────────────────────────────

/** CTE-DAG edge join type — `crate::domain::cte::EdgeType` (+ the bare `union`
 * the schema gate caught on order_status_pivot). */
export type EdgeType =
  | "from" | "inner" | "left" | "right" | "full" | "cross"
  | "union" | "union_all" | "union_distinct";
/** CTE-DAG node role. `cte`/`zone`/`terminal` appear on raw_dag / column nodes
 * beyond the compiled-dag import/transform/final triad. */
export type NodeRole = "import" | "transform" | "final" | "cte" | "zone" | "terminal";
export type DiffLineKind = "context" | "removed" | "added";
/** In-payload per-model change chip (distinct from `removed_models`, the
 * node-less list). Verified `new`/`modified`/`added`/`deleted` in 440. */
export type ModelState = "new" | "modified" | "added" | "deleted";
export type ThreadSide = "Left" | "Right"; // Left = old/deleted side, Right = new/added side
export type PrDagState = "new" | "modified" | "deleted" | "added";
/** Finding tier — `crate::domain::checks::Tier` (verified total/high/direct;
 * the prototype's TIER map also normalizes medium/med/low defensively). */
export type FindingTier = "total" | "high" | "direct" | "medium" | "med" | "low";
/** Column-edge kind — a provenance discriminator, NOT an honesty axis. */
export type ColumnEdgeKind =
  | "source" | "pass_through" | "derived" | "renamed" | "context"
  | "removed" | "added" | "unchanged" | "modified";
/** Jinja raw-zone kind. */
export type RawZoneKind = "for_loop" | "incremental_guard" | string;
/** data_diff row kind. */
export type DiffRowKind = "added" | "removed" | "modified" | "unchanged" | string;
/** data_diff column status. */
export type DiffColStatus = "present" | "added" | "removed" | string;

// ── Source-map coordinate POD ────────────────────────────────────────────────

/** A 1-based (line, col) + 0-based byte offset position. */
export interface SourcePos { line: number; col: number; byte: number; }
/** A span over the (raw or compiled) source. */
export interface SourceSpan { start: SourcePos; end: SourcePos; }

// ── Top-level context ────────────────────────────────────────────────────────

export interface ContextData {
  baseline: string;
  models: ModelPayload[];
  pr_comments?: CommentsView;
  pr_dag?: PrDagPayload;
  pr_ref?: PrRef;
  removed_models?: string[];
  seed_cards?: SeedCard[];
  // broad-surface sections present in the full dogfood — typed loosely (this app
  // does not consume them, but they must round-trip through the Zod gate).
  manifest_nodes?: Record<string, unknown>;
  check_specs?: Record<string, unknown>;
  macro_lens?: unknown;
  governance?: unknown;
  project_definition?: unknown;
  project_change_panel?: unknown;
}

export interface PrRef {
  number: number;
  title: string;
  url: string;
  // §B6: the spine emits only {number,title,url} today; these are the eventual
  // `gh pr view --json` fields (optional until T2 lands them).
  body?: string;
  author?: string;
  head?: string;
  base?: string;
  state?: string;
  draft?: boolean;
  mergeable_state?: string;
  created_at?: string;
  reviewers?: Array<{ login: string; state?: string }>;
  checks?: { passed?: number; failed?: number; pending?: number } | null;
}

// ── Per model ────────────────────────────────────────────────────────────────

export interface ModelPayload {
  name: string;
  path?: string; // project-relative models/…/x.sql — the comment JOIN KEY
  description?: string;
  dag: DagPayload;
  // COMPILED SQL split PER CTE: keyed by CTE-DAG node id (the same ids as
  // dag.nodes[].id). Join values in dag.nodes order for the whole model.
  compiled_sql: Record<string, string>;
  raw_sql?: string;
  sql_diff?: BlockDiff; // present only in --pr-diff mode
  model_yaml?: ModelYamlPayload | null;
  tests?: TestPayload[]; // omitted on a `deleted` model
  is_recursive?: boolean | null; // omitted/null on a `deleted` model
  is_incremental?: boolean;
  findings?: FindingPayload[];
  axes?: Axes; // per-model change attribution (this app reads via pr_dag.by_axis)
  state?: ModelState;
  config_file?: string;
  config_attributions?: ConfigAttribution[];
  var_references?: VarReference[];
  // §3a lineage spine (omit-when-empty → null in the reshaper, never raw
  // masquerading as compiled).
  code_map?: CodeMap | null;
  column_lineage?: ColumnLineage | null;
}

/** Per-model change attribution (body/config/unit_test). */
export interface Axes { body: boolean; config: boolean; unit_test: boolean; }

/** A resolved config attribution (which file/precedence set a config). Loose —
 * unconsumed by this app, must round-trip. */
export type ConfigAttribution = Record<string, unknown>;
/** A var() reference resolved in the model. Loose — unconsumed, must round-trip. */
export type VarReference = Record<string, unknown>;

export interface ModelYamlPayload {
  path?: string;
  raw?: string; // the authoring YAML block (verbatim)
  diff?: BlockDiff; // present if the model's own YAML block changed
  // Rust `Option<String>`: a truthful-degrade COPY naming what is absent
  // (verified in context.sample.json), NOT a boolean flag.
  missing?: string;
}

// ── Diffs ────────────────────────────────────────────────────────────────────

export interface BlockDiff { lines: DiffLine[]; }
export interface DiffLine {
  kind: DiffLineKind;
  text: string; // \r-trimmed, no sigil
  emphasis?: [number, number] | null; // intra-line word-diff span (codepoints), or null
}

// ── The source-map spine (code_map) ──────────────────────────────────────────

export interface CodeMap {
  /** The real compiled SQL (is_incremental()=false build). */
  compiled?: string;
  /** Compiled-pane node↔code spans, keyed by dag node id. */
  node_spans?: Record<string, SourceSpan>;
  /** Raw/diff node↔code spans (§B3: may omit `(final select)` → computed). */
  raw_node_spans?: Record<string, SourceSpan>;
  /** Per-column spans (compiled coordinates). */
  column_spans?: Record<string, SourceSpan>;
  /** Per-column spans (raw coordinates). */
  raw_column_spans?: Record<string, SourceSpan>;
  /** Jinja zones: for_loop → templated DAG node + region; incremental_guard →
   * a marker on the containing node. `presence` is the honesty axis. */
  raw_zones?: RawZone[];
  /** The raw-structure DAG (restores the terminal the compiled `dag` omits).
   * Distinct from `dag`: raw nodes carry the `presence` honesty axis + a role
   * incl. cte/zone/terminal; raw edges carry `confidence`, not a join `edge_type`. */
  raw_dag?: { nodes?: RawDagNode[]; edges?: RawDagEdge[] };
  /** The fan-out map: collapses the N compiled CTEs a {% for %} emits into one
   * raw `zone:N` node; carries selection across raw⇄compiled. ASYMMETRIC: `raw`
   * values are string[] (the generated CTE ids); `compiled` values are a string
   * (the single owning raw node). */
  node_map?: {
    raw?: Record<string, string | string[]>;
    compiled?: Record<string, string | string[]>;
  };
}

export interface RawZone {
  kind: RawZoneKind;
  template?: string | null; // displayed CTE name ({{ status }}_orders), verbatim; null when unnamed
  loop?: string | null;
  start?: SourcePos;
  end?: SourcePos;
  presence: Presence; // HONESTY AXIS — always present on a zone
}

// ── DAGs ─────────────────────────────────────────────────────────────────────

export interface DagPayload { nodes: DagNode[]; edges: DagEdge[]; }
export interface DagNode { id: string; label?: string; role: NodeRole; }
export interface DagEdge { from: string; to: string; edge_type: EdgeType; }

/** A raw-DAG node (the pre-compilation structure). Carries the `presence`
 * honesty axis (compiled_in/out/structural) + `is_zone`. */
export interface RawDagNode {
  id: string;
  role: NodeRole;
  label?: string;
  is_zone?: boolean;
  presence?: Presence; // HONESTY AXIS
}
/** A raw-DAG edge. Carries the `confidence` honesty axis, NOT a join `edge_type`. */
export interface RawDagEdge {
  from: string;
  to: string;
  confidence?: ColumnConfidence; // HONESTY AXIS
}

// ── Column lineage (field-level) ─────────────────────────────────────────────

export interface ColumnLineage {
  /** Per-column docs/tests for the schema table (resolved descriptions + the
   * COMPLETE test list — preferred over the YAML regex parse). */
  context?: Record<string, ColumnContextEntry>;
  /** The field-level edges (collineage.js). */
  edges?: ColumnEdge[];
}
export interface ColumnContextEntry {
  description?: string;
  data_type?: string;
  tests?: Array<ColumnTest | string>;
  documented?: boolean;
}
export interface ColumnTest { kind: string; kwargs?: Record<string, unknown>; node_id?: string; }
export interface ColumnRef {
  scope: { intra?: { node_id: string }; inter?: { node_id: string } };
  column: string;
}
export interface ColumnEdge {
  from_col: ColumnRef;
  to_col: ColumnRef;
  kind: ColumnEdgeKind;
  confidence: ColumnConfidence; // HONESTY AXIS — always present on an edge
}

// ── Tests & coverage ─────────────────────────────────────────────────────────

export interface TestPayload {
  id: string;
  name: string;
  target_model: string;
  changed: boolean;
  description?: string;
  tags?: string[];
  meta?: unknown;
  defined_in?: string; // original_file_path of the test's YAML
  authoring_yaml?: string;
  yaml_diff?: BlockDiff; // the test's own YAML block edit
  data_diff?: UnitTestDataDiff; // cell-level given/expect diff
  overrides?: unknown;
  is_incremental_mode?: boolean;
  given: GivenPayload[];
  expected: ExpectedPayload;
}

/** A cell: `display` (the rendered text) + `key` (the honesty discriminator).
 * key.t is the CELL trichotomy axis; `v` is the typed raw value (absent on
 * `absent`/`null`). */
export interface Cell {
  display?: string;
  key?: CellKey;
}
export type CellKey =
  | { t: "absent" }
  | { t: "null" }
  | { t: "number"; v: string }
  | { t: "str"; v: string };

export interface CellTable { columns: string[]; rows: CellRow[]; }
export interface CellRow { cells: Cell[]; }

export interface GivenPayload {
  input: string;
  bound_to_node?: string; // unconsumed in detail by this app — must round-trip
  is_this?: boolean;
  ordinal?: number;
  format?: string;
  fixture?: string; // external CSV fixture reference
  column_meta?: Record<string, unknown>;
  table?: CellTable;
  rows?: unknown; // a `select …` raw form (string) on some givens
}
export interface ExpectedPayload { table?: CellTable; }

/** The cell-level given/expect diff. Each cell carries an `old`/`new` side
 * (each a Cell with its key.t honesty axis) + a `changed` flag. */
export interface UnitTestDataDiff {
  given?: Array<{ input?: string; ordinal?: number; diff?: DiffTable | null }>;
  expect?: DiffTable | null; // explicit null when the expect side is honestly empty
}
export interface DiffTable {
  columns: Array<{ name: string; status?: DiffColStatus }>;
  rows: DiffRow[];
}
export interface DiffRow { kind?: DiffRowKind; cells: DiffCell[]; }
export interface DiffCell { old?: Cell; new?: Cell; changed?: boolean; }

export interface FindingPayload {
  check: string;
  tier: FindingTier;
  instrument?: string;
  model_id?: string;
  construct?: string;
  verdict: { status: CoverageStatus }; // HONESTY AXIS — always present
  evidence?: Array<{ label: string; value: string }>;
  recommendation?: string;
  sketches?: string[];
}

// ── PR comments / threads ────────────────────────────────────────────────────

export interface CommentsView {
  by_model: ModelCommentBucket[];
  unanchored?: RenderedThread[];
  total?: number;
}
export interface ModelCommentBucket {
  model?: string;
  path?: string;
  model_path?: string;
  count?: number;
  threads: RenderedThread[];
}
export interface RenderedThread {
  model?: string;
  path?: string; // project-relative file the thread anchors to
  line?: number | null; // live 1-based line on `side`; null/absent when outdated
  original_line?: number; // "was on line N" for outdated
  side?: ThreadSide;
  within_hunk?: boolean;
  resolved?: boolean;
  outdated?: boolean;
  comments: RenderedComment[];
}
export interface RenderedComment { author: string | null; body: string; } // author null ⇒ ghost

// ── PR-scope DAG ─────────────────────────────────────────────────────────────

export interface PrDagPayload extends PrDagView {
  by_axis?: Record<"body" | "config" | "unit_test", PrDagView>;
}
export interface PrDagView {
  graph: PrDagGraph;
  modified_count: number;
  connector_count: number;
  halo_count: number;
  deleted_count: number;
  collapsed: boolean;
}
export interface PrDagGraph { nodes: PrDagNode[]; edges: { from: string; to: string }[]; }
export interface PrDagNode {
  id: string;
  name: string;
  state: PrDagState;
  is_connector: boolean;
  is_halo?: boolean;
  lines_added: number;
  lines_removed: number;
}

// ── Seeds ────────────────────────────────────────────────────────────────────

export interface SeedCard {
  id?: string;
  name: string;
  original_file_path?: string;
  feeds_models?: string[];
  quote_columns?: boolean | string; // unconsumed dbt config; wire emits "true"/"false"
  column_types?: string;
  table?: CellTable;
  total_rows?: number;
  shown_rows?: number;
  capped?: boolean;
  description?: string;
  diff?: DiffTable;
}

// ── The S3a envelope wrapper (metadata + data) ───────────────────────────────

/** The standalone `--context-out` artifact: a versioned header + the payload.
 * Mirrors `crate::domain::context_envelope::ContextEnvelope`. */
export interface ContextEnvelope {
  metadata: { schema_version: number };
  data: ContextData;
}
