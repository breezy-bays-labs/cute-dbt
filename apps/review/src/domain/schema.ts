// The Zod parity / drift gate for the cute-dbt context payload.
//
// This is the TS-side enforcer of the context contract: it parses the embedded
// payload at load and FAILS LOUDLY on a shape that drifts from what the app
// consumes. It is deliberately MINIMAL + TOLERANT — it pins the load-bearing
// top-level spine (baseline, models[], the per-model dag) and the honesty-typed
// enums (EdgeType / NodeRole / state — string-literal unions, never bool), while
// tolerating thin shapes (e.g. the since-review fixture omits most spine fields)
// via optional/passthrough. Broad unconsumed sections (governance, macro_lens,
// findings, code_map, …) are accepted opaquely — tightening them is a later
// slice once the Rust `--context-out` contract (S3a) is the source of truth.
//
// Honesty contract: every enum here is a `z.enum([...])` string union. A future
// wire downgrade to a bool would fail validation loudly — never silently coerce.
//
// `import * as z` (the flat namespace: z.enum / z.object / …) is the form that
// resolves identically under both the production Vite build and vitest's
// resolver — zod@4's `{ z }` named export is dropped by vitest's `@zod/source`
// source-condition resolution (see vitest.config.ts), but the flat namespace is
// stable in both.
import * as z from "zod";

// Verified against context.440.json — the gate caught a 9th variant `union`
// (a bare UNION, beyond the documented union_all/union_distinct) on
// order_status_pivot. Closed string-literal union; never a bool.
export const EdgeTypeSchema = z.enum([
  "from", "inner", "left", "right", "full", "cross", "union", "union_all", "union_distinct",
]);
export const NodeRoleSchema = z.enum(["import", "transform", "final"]);
// The full wire vocabulary verified against context.440.json (the gate caught
// `added`/`deleted` beyond the hand-authored `new`/`modified` — exactly the
// drift this schema exists to surface). Still a CLOSED string-literal union.
export const ModelStateSchema = z.enum(["new", "modified", "added", "deleted"]);
export const ThreadSideSchema = z.enum(["Left", "Right"]);
export const PrDagStateSchema = z.enum(["new", "modified", "deleted"]);
export const DiffLineKindSchema = z.enum(["context", "removed", "added"]);

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

const DiffLineSchema = z.object({
  kind: DiffLineKindSchema,
  text: z.string(),
  emphasis: z.tuple([z.number(), z.number()]).nullable(),
});

const BlockDiffSchema = z.object({ lines: z.array(DiffLineSchema) });

const AxesSchema = z.object({
  body: z.boolean(),
  config: z.boolean(),
  unit_test: z.boolean(),
});

// The model spine the app consumes. Loose on everything else (.passthrough()
// keeps unconsumed fields like code_map/column_lineage/findings without pinning
// their shape — the honesty-fold slices tighten those at their birth).
const ModelPayloadSchema = z
  .object({
    name: z.string(),
    path: z.string().optional(),
    description: z.string().optional(),
    dag: DagPayloadSchema,
    compiled_sql: z.record(z.string(), z.string()),
    raw_sql: z.string().optional(),
    sql_diff: BlockDiffSchema.optional(),
    // omitted/null on a `deleted` model (no compiled structure to inspect) —
    // verified absent on legacy_order_rollup in context.440.json.
    is_recursive: z.boolean().nullish(),
    is_incremental: z.boolean().optional(),
    axes: AxesSchema.optional(),
    state: ModelStateSchema.optional(),
  })
  .passthrough();

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
    .object({
      body: PrDagViewSchema,
      config: PrDagViewSchema,
      unit_test: PrDagViewSchema,
    })
    .optional(),
});

const PrRefSchema = z.object({
  number: z.number(),
  title: z.string(),
  url: z.string(),
});

// The top-level context. Tolerant: only `baseline` + `models[]` are required;
// every broad section is optional + the object is open (.passthrough()).
export const ContextDataSchema = z
  .object({
    baseline: z.string(),
    models: z.array(ModelPayloadSchema),
    pr_dag: PrDagPayloadSchema.optional(),
    pr_ref: PrRefSchema.optional(),
    removed_models: z.array(z.string()).optional(),
  })
  .passthrough();

export type ParsedContextData = z.infer<typeof ContextDataSchema>;

/**
 * Parse + validate a raw context payload. Throws a loud ZodError on drift — the
 * fail-closed load (never render a shape we didn't validate). The caller narrows
 * the result to the hand-authored ContextData type (structurally compatible —
 * the schema is a subset pin, .passthrough() preserves the rest).
 */
export function parseContext(raw: unknown): ParsedContextData {
  return ContextDataSchema.parse(raw);
}
