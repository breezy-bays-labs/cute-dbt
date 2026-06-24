// The Zod parity / drift gate for the cute-dbt context payload (S3b).
//
// This is the TS-side enforcer of the context contract: it parses the embedded
// payload at load and FAILS LOUDLY on a shape that drifts from what the app
// consumes. It pins the load-bearing spine + EVERY honesty enum as a
// z.enum([...]) string union, while TOLERATING thin shapes (the since-review +
// comments-showcase fixtures omit most spine fields) via optional/passthrough.
//
// HONESTY CONTRACT (the never-a-false-claim drift gate): the FOUR honesty axes
// are z.enum string unions. A future wire downgrade to a `bool` where an enum
// belongs FAILS validation loudly — never silently coerced:
//   PRESENCE   = compiled_in | compiled_out | structural   (RawZone.presence)
//   CONFIDENCE = resolved | opaque | ambiguous             (ColumnEdge.confidence)
//   COVERAGE   = covered | uncovered | unknown             (Finding.verdict.status)
//   CELL key.t = absent | null | number | str              (Cell.key.t)
//
// `import * as z` (the flat namespace: z.enum / z.object / …) is the form that
// resolves identically under both the production Vite build and vitest's
// resolver — zod@4's `{ z }` named export is dropped by vitest's `@zod/source`
// source-condition resolution (see vitest.config.ts), but the flat namespace is
// stable in both.
import * as z from "zod";

// ── The four honesty axes (z.enum string unions — never bool) ────────────────

/** PRESENCE — `crate::domain::source_map::Presence`. */
export const PresenceSchema = z.enum(["compiled_in", "compiled_out", "structural"]);
/** CONFIDENCE — `crate::domain::cte::ColumnEdgeConfidence`. */
export const ColumnConfidenceSchema = z.enum(["resolved", "opaque", "ambiguous"]);
/** COVERAGE — `crate::domain::checks::Verdict.status`. */
export const CoverageStatusSchema = z.enum(["covered", "uncovered", "unknown"]);
/** CELL key.t — the NULL/absent/empty trichotomy + typed value. */
export const CellKeyTypeSchema = z.enum(["absent", "null", "number", "str"]);

// ── Other closed wire enums ──────────────────────────────────────────────────

// Verified against context.440.json — the gate caught a 9th variant `union`
// (a bare UNION) on order_status_pivot. Closed string-literal union; never bool.
export const EdgeTypeSchema = z.enum([
  "from", "inner", "left", "right", "full", "cross", "union", "union_all", "union_distinct",
]);
// raw_dag / column nodes carry cte/zone/terminal beyond the compiled triad.
export const NodeRoleSchema = z.enum([
  "import", "transform", "final", "cte", "zone", "terminal",
]);
export const ModelStateSchema = z.enum(["new", "modified", "added", "deleted"]);
export const ThreadSideSchema = z.enum(["Left", "Right"]);
export const PrDagStateSchema = z.enum(["new", "modified", "deleted", "added"]);
export const DiffLineKindSchema = z.enum(["context", "removed", "added"]);
export const FindingTierSchema = z.enum(["total", "high", "direct", "medium", "med", "low"]);

// ── Source-map coordinates ───────────────────────────────────────────────────

const SourcePosSchema = z.object({ line: z.number(), col: z.number(), byte: z.number() });
const SourceSpanSchema = z.object({ start: SourcePosSchema, end: SourcePosSchema });

// ── DAGs ─────────────────────────────────────────────────────────────────────

const NodePayloadSchema = z.object({
  id: z.string(),
  label: z.string().optional(),
  role: NodeRoleSchema,
});
const EdgePayloadSchema = z.object({
  from: z.string(),
  to: z.string(),
  edge_type: EdgeTypeSchema,
});
const DagPayloadSchema = z.object({
  nodes: z.array(NodePayloadSchema),
  edges: z.array(EdgePayloadSchema),
});

// ── Diffs ────────────────────────────────────────────────────────────────────

const DiffLineSchema = z.object({
  kind: DiffLineKindSchema,
  text: z.string(),
  emphasis: z.tuple([z.number(), z.number()]).nullish(),
});
const BlockDiffSchema = z.object({ lines: z.array(DiffLineSchema) });

// ── Cells (the key.t honesty axis) ───────────────────────────────────────────

// A cell key is a discriminated union on `t`. The `absent`/`null` arms carry NO
// value; the typed arms carry the raw `v`. A bool where `t` belongs FAILS here.
const CellKeySchema = z.union([
  z.object({ t: z.literal("absent") }).passthrough(),
  z.object({ t: z.literal("null") }).passthrough(),
  z.object({ t: z.literal("number"), v: z.string() }).passthrough(),
  z.object({ t: z.literal("str"), v: z.string() }).passthrough(),
]);
const CellSchema = z
  .object({ display: z.string().optional(), key: CellKeySchema.optional() })
  .passthrough();
const CellTableSchema = z
  .object({ columns: z.array(z.string()), rows: z.array(z.object({ cells: z.array(CellSchema) }).passthrough()) })
  .passthrough();

const DiffCellSchema = z
  .object({ old: CellSchema.optional(), new: CellSchema.optional(), changed: z.boolean().optional() })
  .passthrough();
const DiffTableSchema = z
  .object({
    columns: z.array(z.object({ name: z.string(), status: z.string().optional() }).passthrough()),
    rows: z.array(z.object({ kind: z.string().optional(), cells: z.array(DiffCellSchema) }).passthrough()),
  })
  .passthrough();

// ── code_map (the source-map spine) ──────────────────────────────────────────

const RawZoneSchema = z
  .object({
    kind: z.string(),
    template: z.string().nullish(), // explicit null on a loop with no name template
    loop: z.string().nullish(),
    start: SourcePosSchema.optional(),
    end: SourcePosSchema.optional(),
    presence: PresenceSchema, // HONESTY AXIS — required when a zone is present
  })
  .passthrough();

// raw_dag has its OWN node/edge shapes (verified against context.440.json): a
// raw node carries the `presence` honesty axis + a `role` incl. cte/zone/terminal;
// a raw edge carries the `confidence` honesty axis, NOT a join `edge_type` (the
// raw structure is pre-compilation, so join-type vocabulary doesn't apply yet).
const RawDagNodeSchema = z
  .object({
    id: z.string(),
    role: NodeRoleSchema,
    label: z.string().optional(),
    is_zone: z.boolean().optional(),
    presence: PresenceSchema.optional(), // HONESTY AXIS — present on raw_dag nodes
  })
  .passthrough();
const RawDagEdgeSchema = z
  .object({
    from: z.string(),
    to: z.string(),
    confidence: ColumnConfidenceSchema.optional(), // HONESTY AXIS on raw edges
  })
  .passthrough();

// node_map values are ASYMMETRIC: `raw` maps id → string[] (the N collapsed CTEs
// a zone generates), `compiled` maps id → string (the single owning raw node).
// Tolerate string OR string[] so the gate accepts both arms without false-reject.
const NodeMapValueSchema = z.union([z.string(), z.array(z.string())]);

const CodeMapSchema = z
  .object({
    compiled: z.string().optional(),
    node_spans: z.record(z.string(), SourceSpanSchema).optional(),
    raw_node_spans: z.record(z.string(), SourceSpanSchema).optional(),
    column_spans: z.record(z.string(), SourceSpanSchema).optional(),
    raw_column_spans: z.record(z.string(), SourceSpanSchema).optional(),
    raw_zones: z.array(RawZoneSchema).optional(),
    // edges is OMITTED on an edge-less raw_dag (verified: 5 models in 440 ship a
    // node-only raw_dag) — optional, not required.
    raw_dag: z
      .object({ nodes: z.array(RawDagNodeSchema).optional(), edges: z.array(RawDagEdgeSchema).optional() })
      .passthrough()
      .optional(),
    node_map: z
      .object({
        raw: z.record(z.string(), NodeMapValueSchema).optional(),
        compiled: z.record(z.string(), NodeMapValueSchema).optional(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();

// ── column_lineage (the confidence honesty axis lives here) ──────────────────

const ColumnTestSchema = z.union([
  z.string(),
  z.object({ kind: z.string(), kwargs: z.record(z.string(), z.unknown()).optional(), node_id: z.string().optional() }).passthrough(),
]);
const ColumnContextEntrySchema = z
  .object({
    description: z.string().optional(),
    data_type: z.string().optional(),
    tests: z.array(ColumnTestSchema).optional(),
    documented: z.boolean().optional(),
  })
  .passthrough();
const ColumnRefSchema = z
  .object({
    scope: z
      .object({
        intra: z.object({ node_id: z.string() }).passthrough().optional(),
        inter: z.object({ node_id: z.string() }).passthrough().optional(),
      })
      .passthrough(),
    column: z.string(),
  })
  .passthrough();
const ColumnEdgeSchema = z
  .object({
    from_col: ColumnRefSchema,
    to_col: ColumnRefSchema,
    kind: z.string(),
    confidence: ColumnConfidenceSchema, // HONESTY AXIS — required on an edge
  })
  .passthrough();
const ColumnLineageSchema = z
  .object({
    context: z.record(z.string(), ColumnContextEntrySchema).optional(),
    edges: z.array(ColumnEdgeSchema).optional(),
  })
  .passthrough();

// ── findings (the coverage honesty axis lives here) ──────────────────────────

const FindingSchema = z
  .object({
    check: z.string(),
    tier: FindingTierSchema,
    verdict: z.object({ status: CoverageStatusSchema }).passthrough(), // HONESTY AXIS
    instrument: z.string().optional(),
    model_id: z.string().optional(),
    construct: z.string().optional(),
    evidence: z.array(z.object({ label: z.string(), value: z.string() }).passthrough()).optional(),
    recommendation: z.string().optional(),
    sketches: z.array(z.string()).optional(),
  })
  .passthrough();

// ── tests ────────────────────────────────────────────────────────────────────

const GivenSchema = z
  .object({
    input: z.string(),
    bound_to_node: z.string().optional(),
    is_this: z.boolean().optional(),
    ordinal: z.number().optional(),
    format: z.string().optional(),
    fixture: z.string().optional(),
    column_meta: z.record(z.string(), z.unknown()).optional(),
    table: CellTableSchema.optional(),
    rows: z.unknown().optional(),
  })
  .passthrough();
const UnitTestDataDiffSchema = z
  .object({
    given: z
      .array(z.object({ input: z.string().optional(), ordinal: z.number().optional(), diff: DiffTableSchema.nullish() }).passthrough())
      .optional(),
    // explicit `null` on a test whose expect side is honestly empty (verified
    // order_events_incremental in 440) — nullish, not just optional.
    expect: DiffTableSchema.nullish(),
  })
  .passthrough();
const TestPayloadSchema = z
  .object({
    id: z.string(),
    name: z.string(),
    target_model: z.string().optional(),
    changed: z.boolean().optional(),
    description: z.string().optional(),
    tags: z.array(z.string()).optional(),
    meta: z.unknown().optional(),
    defined_in: z.string().optional(),
    authoring_yaml: z.string().optional(),
    yaml_diff: BlockDiffSchema.optional(),
    data_diff: UnitTestDataDiffSchema.optional(),
    overrides: z.unknown().optional(),
    is_incremental_mode: z.boolean().optional(),
    given: z.array(GivenSchema).optional(),
    expected: z.object({ table: CellTableSchema.optional() }).passthrough().optional(),
  })
  .passthrough();

// ── model YAML ───────────────────────────────────────────────────────────────

const ModelYamlPayloadSchema = z
  .object({
    path: z.string().optional(),
    raw: z.string().optional(),
    diff: BlockDiffSchema.optional(),
    missing: z.string().optional(), // truthful-degrade COPY (Option<String>), NOT a bool
  })
  .passthrough();

const AxesSchema = z.object({ body: z.boolean(), config: z.boolean(), unit_test: z.boolean() });

// ── the model spine ──────────────────────────────────────────────────────────

const ModelPayloadSchema = z
  .object({
    name: z.string(),
    path: z.string().optional(),
    description: z.string().optional(),
    dag: DagPayloadSchema,
    compiled_sql: z.record(z.string(), z.string()),
    raw_sql: z.string().optional(),
    sql_diff: BlockDiffSchema.optional(),
    // nullish: the wire emits an explicit `null` on an `added` model (verified
    // order_status_pivot) as well as omitting it.
    model_yaml: ModelYamlPayloadSchema.nullish(),
    tests: z.array(TestPayloadSchema).optional(),
    findings: z.array(FindingSchema).optional(),
    code_map: CodeMapSchema.nullish(),
    column_lineage: ColumnLineageSchema.nullish(),
    is_recursive: z.boolean().nullish(),
    is_incremental: z.boolean().optional(),
    axes: AxesSchema.optional(),
    state: ModelStateSchema.optional(),
    config_file: z.string().optional(),
    config_attributions: z.array(z.unknown()).optional(),
    var_references: z.array(z.unknown()).optional(),
  })
  .passthrough();

// ── PR-scope DAG ─────────────────────────────────────────────────────────────

const PrDagNodeSchema = z.object({
  id: z.string(),
  name: z.string(),
  state: PrDagStateSchema,
  is_connector: z.boolean(),
  is_halo: z.boolean().optional(),
  lines_added: z.number(),
  lines_removed: z.number(),
});
const PrDagGraphSchema = z.object({
  nodes: z.array(PrDagNodeSchema),
  edges: z.array(z.object({ from: z.string(), to: z.string() })),
});
const PrDagViewSchema = z.object({
  graph: PrDagGraphSchema,
  modified_count: z.number(),
  connector_count: z.number(),
  halo_count: z.number(),
  deleted_count: z.number(),
  collapsed: z.boolean(),
});
const PrDagPayloadSchema = PrDagViewSchema.extend({
  by_axis: z
    .object({ body: PrDagViewSchema, config: PrDagViewSchema, unit_test: PrDagViewSchema })
    .optional(),
});

// ── PR ref ───────────────────────────────────────────────────────────────────

const PrRefSchema = z
  .object({
    number: z.number(),
    title: z.string(),
    url: z.string(),
    body: z.string().optional(),
    author: z.string().optional(),
    head: z.string().optional(),
    base: z.string().optional(),
    state: z.string().optional(),
    draft: z.boolean().optional(),
    mergeable_state: z.string().optional(),
    created_at: z.string().optional(),
    reviewers: z.array(z.object({ login: z.string(), state: z.string().optional() }).passthrough()).optional(),
    checks: z.object({ passed: z.number().optional(), failed: z.number().optional(), pending: z.number().optional() }).passthrough().nullish(),
  })
  .passthrough();

// ── PR comments ──────────────────────────────────────────────────────────────

const RenderedCommentSchema = z
  .object({ author: z.string().nullable(), body: z.string() })
  .passthrough();
const RenderedThreadSchema = z
  .object({
    model: z.string().optional(),
    path: z.string().optional(),
    line: z.number().nullish(),
    original_line: z.number().optional(),
    side: ThreadSideSchema.optional(),
    within_hunk: z.boolean().optional(),
    resolved: z.boolean().optional(),
    outdated: z.boolean().optional(),
    comments: z.array(RenderedCommentSchema),
  })
  .passthrough();
const ModelCommentBucketSchema = z
  .object({
    path: z.string().optional(),
    model: z.string().optional(),
    model_path: z.string().optional(),
    count: z.number().optional(),
    threads: z.array(RenderedThreadSchema),
  })
  .passthrough();
const CommentsViewSchema = z
  .object({
    by_model: z.array(ModelCommentBucketSchema),
    unanchored: z.array(RenderedThreadSchema).optional(),
    total: z.number().optional(),
  })
  .passthrough();

// ── seeds ────────────────────────────────────────────────────────────────────

const SeedCardSchema = z
  .object({
    name: z.string(),
    id: z.string().optional(),
    original_file_path: z.string().optional(),
    feeds_models: z.array(z.string()).optional(),
    // NOT an honesty axis (an unconsumed dbt config) — the wire emits the string
    // "true"/"false"; tolerate string OR bool so the gate doesn't false-reject.
    quote_columns: z.union([z.boolean(), z.string()]).optional(),
    column_types: z.string().optional(),
    table: CellTableSchema.optional(),
    total_rows: z.number().optional(),
    shown_rows: z.number().optional(),
    capped: z.boolean().optional(),
    description: z.string().optional(),
    diff: DiffTableSchema.optional(),
  })
  .passthrough();

// ── the top-level context ────────────────────────────────────────────────────

// Tolerant: only `baseline` + `models[]` are required; every broad section is
// optional + the object is open (.passthrough()).
export const ContextDataSchema = z
  .object({
    baseline: z.string(),
    models: z.array(ModelPayloadSchema),
    pr_comments: CommentsViewSchema.optional(),
    pr_dag: PrDagPayloadSchema.optional(),
    pr_ref: PrRefSchema.optional(),
    removed_models: z.array(z.string()).optional(),
    seed_cards: z.array(SeedCardSchema).optional(),
  })
  .passthrough();

export type ParsedContextData = z.infer<typeof ContextDataSchema>;

// ── the S3a envelope ({ metadata: { schema_version }, data }) ─────────────────

/** The standalone `--context-out` artifact wrapper. Matches
 * `crate::domain::context_envelope::ContextEnvelope`: a versioned header + the
 * bare payload. The fixtures are the BARE payload (`data`); the real
 * `--context-out` is the wrapped envelope. */
export const ContextEnvelopeSchema = z
  .object({
    metadata: z.object({ schema_version: z.number() }).passthrough(),
    data: ContextDataSchema,
  })
  .passthrough();

export type ParsedContextEnvelope = z.infer<typeof ContextEnvelopeSchema>;

/**
 * Parse + validate a raw context payload (the bare `data`). Throws a loud
 * ZodError on drift — the fail-closed load (never render a shape we didn't
 * validate).
 */
export function parseContext(raw: unknown): ParsedContextData {
  return ContextDataSchema.parse(raw);
}

/**
 * Parse + validate a wrapped `--context-out` envelope ({ metadata, data }),
 * returning the inner payload. The drift gate for the REAL artifact (S3a). A
 * thin context (legitimately sparse) passes; a downgraded one (an enum flipped
 * to a bool, a missing required spine field) fails loudly.
 */
export function parseContextEnvelope(raw: unknown): ParsedContextData {
  return ContextEnvelopeSchema.parse(raw).data;
}
