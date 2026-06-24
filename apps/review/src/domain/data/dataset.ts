// buildDataset(raw) — the WeakMap-memoized per-source reshape that turns a
// validated ContextData into the per-model records the app's views consume. The
// VERBATIM port of prototype/context.js `build(srcKey)`, with the module-global
// `_activeSrc` replaced by an explicit `activeSource` field on the dataset's
// owning slice (data-slice.ts), and the `_cache[srcKey]` object cache replaced by
// a WeakMap keyed on the raw ContextData identity (a parsed fixture is a stable
// object reference → the memo is sound + GC-friendly).
//
// NO RECOMPUTE of spine facts: every field is read, reshaped (renamed/grouped),
// and surfaced. The honesty-load-bearing folds live in their own strict modules
// (cell-diff.ts, col-lineage.ts, raw-spans.ts) and are imported here.

import type {
  BlockDiff, Cell, CellTable, ContextData, DiffTable, FindingPayload,
  GivenPayload, ModelPayload, ModelState, PrDagPayload, PrDagView, RenderedThread,
  SeedCard, TestPayload,
} from "../context-data";
import type { ChangeState, GraphData, GraphNode, Materialization, NodeKind } from "../graph-model";
import { adaptDiffTable, allAdded, cellSide, type NormDiffTable } from "./cell-diff";
import { buildColEdges, buildColLineage, colTerminal, type ColEdge, type ColSource } from "./col-lineage";
import { buildRawSpans, ensureMainNode, rawDagToGraph, type LineSpan, type RawGraph } from "./raw-spans";

// ── Pierre side translation (THE deletion-comment fix) ───────────────────────
export function pierreSide(side: "Left" | "Right"): "deletions" | "additions" {
  return side === "Left" ? "deletions" : "additions";
}

// ── BlockDiff → unified-patch serializer (for the Pierre engine) ─────────────
const SIGIL: Record<BlockDiff["lines"][number]["kind"], string> = { context: " ", removed: "-", added: "+" };
export function blockDiffToPatch(diff: BlockDiff, path: string): string {
  let oldCount = 0, newCount = 0;
  for (const l of diff.lines) { if (l.kind !== "added") oldCount++; if (l.kind !== "removed") newCount++; }
  const header = `diff --git a/${path} b/${path}\n--- a/${path}\n+++ b/${path}\n@@ -1,${oldCount} +1,${newCount} @@\n`;
  return header + diff.lines.map((l) => SIGIL[l.kind] + l.text).join("\n") + "\n";
}
export function sourceToContextPatch(src: string, path: string): string {
  const lines = String(src).replace(/\n$/, "").split("\n");
  return `diff --git a/${path} b/${path}\n--- a/${path}\n+++ b/${path}\n@@ -1,${lines.length} +1,${lines.length} @@\n`
    + lines.map((t) => " " + t).join("\n") + "\n";
}

// ── BlockDiff → our hand-rolled hunk shape ({oldStart,newStart,lines:[{t,s}]}) ─
const KIND_T: Record<BlockDiff["lines"][number]["kind"], string> = { context: "ctx", removed: "del", added: "add" };
export interface Hunk { oldStart: number; newStart: number; lines: { t: string; s: string }[]; }
export function blockDiffToHunks(diff: BlockDiff | null | undefined): Hunk[] {
  if (!diff || !diff.lines || !diff.lines.length) return [];
  return [{ oldStart: 1, newStart: 1, lines: diff.lines.map((l) => ({ t: KIND_T[l.kind], s: l.text })) }];
}

// schema from a project-relative path: models/<schema>/x.sql → <schema>; flat → "models"
export function schemaOf(path: string | null | undefined): string {
  if (!path) return "models";
  const segs = path.split("/").filter(Boolean);
  return segs.length >= 3 ? segs[segs.length - 2]! : "models";
}
export const stateToChange = (s: ModelState | undefined): "added" | "removed" | "modified" =>
  s === "new" ? "added" : s === "deleted" ? "removed" : "modified";

// ── findings → coverage checklist ────────────────────────────────────────────
function humanizeCheck(c: string): string { return String(c || "check").replace(/\./g, " · ").replace(/[-_]/g, " "); }
const TIER: Record<string, string> = { total: "HIGH", high: "HIGH", medium: "MED", med: "MED", low: "LOW", direct: "HIGH" };
export interface CoverageCheck { name: string; status: "covered" | "uncovered" | "unknown"; tier: string; note?: string; sketch?: string; }
export interface Coverage { counts: { covered: number; uncovered: number; unknown: number }; checks: CoverageCheck[]; }
export function findingsToCoverage(findings: FindingPayload[] | null | undefined): Coverage {
  const counts = { covered: 0, uncovered: 0, unknown: 0 };
  const checks = (findings ?? []).map((f) => {
    const status = (f.verdict && f.verdict.status) || "unknown";
    const norm: "covered" | "uncovered" | "unknown" = status === "covered" ? "covered" : status === "uncovered" ? "uncovered" : "unknown";
    counts[norm]++;
    const ev = (f.evidence ?? []).map((e) => `${e.label}: ${e.value}`).join("; ");
    return {
      name: humanizeCheck(f.check), status: norm, tier: TIER[String(f.tier).toLowerCase()] ?? "MED",
      note: f.recommendation || ev || undefined,
      sketch: f.sketches && f.sketches[0] ? f.sketches[0] : undefined,
    };
  });
  return { counts, checks };
}

// ── compiled CTE DAG ─────────────────────────────────────────────────────────
const ROLE_TONE: Record<string, string> = { import: "base", transform: "cte", final: "final" };
export interface CteGraph { nodes: { id: string; label: string; sub: string; tone: string }[]; edges: [string, string][]; }
export function dagToCte(dag: ModelPayload["dag"] | null | undefined): CteGraph | null {
  if (!dag || !dag.nodes) return null;
  const nodes = dag.nodes.map((n) => ({ id: n.id, label: n.label || n.id, sub: n.role, tone: ROLE_TONE[n.role] ?? "cte" }));
  const edges: [string, string][] = (dag.edges ?? []).map((e) => [e.from, e.to]);
  return { nodes, edges };
}

// ── pr_dag → scope graph ─────────────────────────────────────────────────────
const PR_TONE: Record<string, string> = { new: "added", modified: "modified", deleted: "removed" };
function prKind(id: string | undefined): "seed" | "macro" | "model" {
  const p = String(id ?? "");
  return p.startsWith("seed.") || p.startsWith("source.") ? "seed" : p.startsWith("macro.") ? "macro" : "model";
}
export interface ScopeNode {
  id: string; label: string; sub: string; tone: string; kind: "seed" | "macro" | "model";
  change: string; context: boolean; mat: string | null;
}
export interface PrScope { data: { nodes: ScopeNode[]; edges: [string, string][] }; selectable: string[]; counts: Record<string, number> }

/** The 3-axis toggle vocabulary (single-select): the whole scope + the three
 *  per-axis subgraphs. `all` is always present; the axis arms are present only
 *  when the spine emitted `pr_dag.by_axis`. */
export type ScopeAxis = "all" | "body" | "config" | "unit_test";
export const SCOPE_AXES: ScopeAxis[] = ["all", "body", "config", "unit_test"];

/** Pick a PR-scope subgraph by axis from a per-axis map, degrading to `all` when
 *  the requested axis is absent (the spine emitted no `by_axis` for it). Pure +
 *  honest: never fabricates a subgraph, never returns a stale axis. */
export function pickScopeAxis(
  byAxis: Record<string, PrScope | null>,
  axis: ScopeAxis,
): PrScope | null {
  return byAxis[axis] ?? byAxis.all ?? null;
}

/** The axes actually present in a per-axis map (for a stable toggle — never
 *  offer an axis the spine didn't emit). `all` always leads when present. */
export function availableScopeAxes(byAxis: Record<string, PrScope | null>): ScopeAxis[] {
  return SCOPE_AXES.filter((a) => byAxis[a]);
}

/** Normalize a ScopeNode's `mat` string onto the engine's Materialization glyph
 *  vocabulary; an unrecognized/absent value is honest-null (no false glyph). */
function normMat(mat: string | null): Materialization {
  return mat === "view" || mat === "table" || mat === "incremental" ? mat : null;
}

/** Adapt the PR-scope subgraph (ScopeNode POD) onto the shared engine GraphData.
 *  Pure: the change/kind/context honesty facts are carried VERBATIM from the
 *  scope node (computed in prDagToScope), never recomputed here. */
export function scopeToGraph(scope: PrScope["data"] | null | undefined): GraphData {
  if (!scope) return { nodes: [], edges: [] };
  const nodes: GraphNode[] = scope.nodes.map((n) => ({
    id: n.id, label: n.label, sub: n.sub, tone: n.tone,
    kind: n.kind as NodeKind, change: n.change as ChangeState,
    context: n.context, mat: normMat(n.mat),
  }));
  const edges: GraphData["edges"] = scope.edges.map(([s, t]) => [s, t]);
  return { nodes, edges };
}
export function prDagToScope(
  prDag: PrDagView | null | undefined,
  modelSchema: Record<string, string>,
  matByName: Record<string, { materialized: string }>,
  changeByName: Record<string, string>,
): PrScope | null {
  if (!prDag || !prDag.graph) return null;
  const g = prDag.graph;
  // null-proto map: keys are untrusted node ids (see parseSeedColumnTypes).
  const idToName: Record<string, string> = Object.create(null) as Record<string, string>;
  g.nodes.forEach((n) => { idToName[n.id] = n.name; });
  const mat = matByName ?? {};
  const cb = changeByName ?? {};
  const nodes: ScopeNode[] = g.nodes.map((n) => {
    const ctx = !!(n.is_connector || n.is_halo);
    const change = ctx ? "context" : (cb[n.name] || (n.state === "new" || n.state === "added" ? "added" : n.state === "deleted" ? "removed" : "modified"));
    return {
      id: n.name, label: n.name,
      sub: n.is_connector ? "connector" : n.is_halo ? "upstream" : (modelSchema[n.name] || change),
      tone: ctx ? "base" : (PR_TONE[n.state] ?? "modified"),
      kind: prKind(n.id), change, context: ctx,
      mat: !ctx && mat[n.name] ? mat[n.name]!.materialized : null,
    };
  });
  const edges: [string, string][] = g.edges.map((e) => [idToName[e.from] ?? e.from, idToName[e.to] ?? e.to]);
  const selectable = g.nodes.filter((n) => !n.is_connector && !n.is_halo).map((n) => n.name);
  const counts = { modified: prDag.modified_count, connector: prDag.connector_count, halo: prDag.halo_count, deleted: prDag.deleted_count };
  return { data: { nodes, edges }, selectable, counts };
}

// ── tests ────────────────────────────────────────────────────────────────────
export interface AdaptedGiven { input: string; columns: string[]; rows: (string | undefined)[][]; external: { file: string; format: string } | null }
export interface AdaptedExpect { columns: string[]; rows: { cells: { v: string | undefined }[] }[] }
export interface AdaptedDataDiff { given: { input?: string; ordinal?: number; table: NormDiffTable | null }[]; expect: NormDiffTable | null }
export interface AdaptedTest {
  name: string; badges: string[]; description: string; tags: string[];
  given: AdaptedGiven[]; expect: AdaptedExpect; changed: boolean; isNew: boolean;
  dataDiff: AdaptedDataDiff | null; yamlDiff: BlockDiff | null; overrides: unknown;
  incMode: boolean; external: { file: string; format: string } | null; definedIn: string | null;
}
export function adaptTest(t: TestPayload): AdaptedTest {
  const given: AdaptedGiven[] = (t.given ?? []).map((g: GivenPayload) => ({
    input: g.input,
    columns: (g.table && g.table.columns) ?? [],
    rows: ((g.table && g.table.rows) ?? []).map((r) => (r.cells ?? []).map((c: Cell) => c.display)),
    external: g.fixture ? { file: g.fixture, format: g.format ?? "csv" } : null,
  }));
  const ex: CellTable | undefined = t.expected && t.expected.table;
  const expect: AdaptedExpect = ex ? {
    columns: ex.columns ?? [],
    rows: (ex.rows ?? []).map((r) => ({ cells: (r.cells ?? []).map((c: Cell) => ({ v: c.display })) })),
  } : { columns: [], rows: [] };
  const dd = t.data_diff;
  const dataDiff: AdaptedDataDiff | null = dd ? {
    given: (dd.given ?? []).map((g) => ({ input: g.input, ordinal: g.ordinal, table: adaptDiffTable(g.diff) })),
    expect: adaptDiffTable(dd.expect),
  } : null;
  let external: { file: string; format: string } | null = null;
  (t.given ?? []).forEach((g) => { if (g.fixture) external = { file: g.fixture, format: g.format ?? "csv" }; });
  const isNew = !!dataDiff && (dataDiff.expect ? allAdded(dataDiff.expect) : false)
    && dataDiff.given.length > 0 && dataDiff.given.every((g) => allAdded(g.table));
  const badges = isNew ? ["added"] : t.changed ? ["modified"] : [];
  return {
    name: t.name, badges, description: t.description ?? "", tags: t.tags ?? [], given, expect,
    changed: !!t.changed, isNew, dataDiff, yamlDiff: t.yaml_diff ?? null, overrides: t.overrides ?? null,
    incMode: !!t.is_incremental_mode, external, definedIn: t.defined_in ?? null,
  };
}

// ── seeds ────────────────────────────────────────────────────────────────────
function parseSeedColumnTypes(str: string | undefined): Record<string, string> {
  // null-proto map: keys are untrusted seed column names; a stray `__proto__`/
  // `constructor` column can't pollute the chain (matches col-lineage/raw-spans).
  const out: Record<string, string> = Object.create(null) as Record<string, string>;
  String(str ?? "").split(",").forEach((p) => {
    const ix = p.indexOf(":"); if (ix < 0) return;
    const k = p.slice(0, ix).trim(); if (k) out[k] = p.slice(ix + 1).trim();
  });
  return out;
}
export interface SeedRecord {
  change: "modified" | "base"; file: string; desc: string; columns: string[];
  colTypes: Record<string, string>; colDesc: Record<string, string>;
  colTests: Record<string, string[]>; colTags: Record<string, string[]>;
  diffRows: { cells: { v: string; old?: string; changed?: boolean }[] }[];
  downstream: string[]; totalRows?: number; shownRows?: number; capped: boolean;
}
export function buildSeedRecords(data: ContextData): { seeds: string[]; seedRecords: Record<string, SeedRecord> } {
  const seeds: string[] = [];
  // null-proto map: keys are untrusted seed card names (see parseSeedColumnTypes).
  const seedRecords: Record<string, SeedRecord> = Object.create(null) as Record<string, SeedRecord>;
  (data.seed_cards ?? []).forEach((card: SeedCard) => {
    const cols = (card.table && card.table.columns) ?? [];
    let diffRows: SeedRecord["diffRows"] = [];
    let changed = false;
    const dd = card.diff as DiffTable | undefined;
    if (dd && dd.rows) {
      diffRows = dd.rows.map((r) => ({
        cells: (r.cells ?? []).map((c) => {
          const nv = cellSide(c.new, "");
          const cell: { v: string; old?: string; changed?: boolean } = { v: nv };
          if (c.changed) { changed = true; cell.old = cellSide(c.old, "(null)") || "(null)"; cell.changed = true; }
          return cell;
        }),
      }));
    } else if (card.table && card.table.rows) {
      diffRows = card.table.rows.map((r) => ({ cells: (r.cells ?? []).map((c) => ({ v: cellSide(c, "") })) }));
    }
    const colStatusChange = !!(dd && (dd.columns ?? []).some((c) => c.status && c.status !== "present"));
    seedRecords[card.name] = {
      change: (changed || colStatusChange) ? "modified" : "base",
      file: card.original_file_path ?? ("seeds/" + card.name + ".csv"),
      desc: card.description ?? "", columns: cols, colTypes: parseSeedColumnTypes(card.column_types),
      colDesc: {}, colTests: {}, colTags: {},
      diffRows, downstream: card.feeds_models ?? [],
      totalRows: card.total_rows, shownRows: card.shown_rows, capped: !!card.capped,
    };
    seeds.push(card.name);
  });
  return { seeds, seedRecords };
}

// ── threads-by-path join ─────────────────────────────────────────────────────
export function threadsByPath(data: ContextData): Map<string, RenderedThread[]> {
  const m = new Map<string, RenderedThread[]>();
  const add = (t: RenderedThread): void => {
    if (t.path == null) return;
    const a = m.get(t.path) ?? []; a.push(t); m.set(t.path, a);
  };
  (data.pr_comments && data.pr_comments.by_model ? data.pr_comments.by_model : []).forEach((b) => (b.threads ?? []).forEach(add));
  (data.pr_comments && data.pr_comments.unanchored ? data.pr_comments.unanchored : []).forEach(add);
  return m;
}

// ── model-detail INFO (materialized / grain / governance / config / meta / tags) ─
export interface DerivedInfo {
  materialized: string;
  grain: { value: string; source: string; known: boolean };
  gov: Record<string, string>;
  config: Record<string, string>;
  meta: { key: string; value: string }[];
  tags: string[];
}
const indentOf = (l: string): number => (l.match(/^\s*/) ?? [""])[0]!.length;
const grabIn = (txt: string, re: RegExp): string | null => { const x = txt.match(re); return x ? x[1]!.trim() : null; };

/** materialized: incremental flag → yaml override → default view. */
function deriveMaterialized(raw: string, isIncremental: boolean | undefined): string {
  const mm = raw.match(/materialized:\s*(\w+)/);
  if (mm) return mm[1]!;
  return isIncremental ? "incremental" : "view";
}

/** grain (unique_key) from the first unique_key/grain finding, else unknown. */
function deriveGrain(findings: FindingPayload[] | undefined): DerivedInfo["grain"] {
  for (const f of findings ?? []) {
    if (String(f.construct ?? "").includes("unique_key") || String(f.check ?? "").includes("grain")) {
      const ev = (f.evidence ?? []).find((e) => /unique_key|grain|key/i.test(e.label));
      if (ev) return { value: ev.value, source: "config.unique_key", known: true };
    }
  }
  return { value: "unknown", source: "unknown", known: false };
}

/** the model-level `config:` block lines, comment-stripped + indent-bounded. */
function extractConfigBlock(raw: string): string[] {
  const rawLines = raw.split("\n").filter((l) => !/^\s*#/.test(l)).map((l) => l.replace(/\s+#.*$/, ""));
  const cfgStart = rawLines.findIndex((l) => /^\s*config:\s*$/.test(l));
  if (cfgStart < 0) return [];
  const cfgIndent = indentOf(rawLines[cfgStart]!);
  const out: string[] = [];
  for (let i = cfgStart + 1; i < rawLines.length; i++) {
    const l = rawLines[i]!; if (!l.trim()) continue;
    if (indentOf(l) <= cfgIndent) break;
    out.push(l);
  }
  return out;
}

/** governance (access/group/contract) + config (on_schema_change/inc_strategy/unique_key). */
function parseGovernanceConfig(cfg: string, isIncremental: boolean | undefined): { gov: Record<string, string>; config: Record<string, string> } {
  const gov: Record<string, string> = {}, config: Record<string, string> = {};
  const access = grabIn(cfg, /\baccess:\s*(\w+)/); if (access) gov.access = access;
  const group = grabIn(cfg, /\bgroup:\s*([\w.-]+)/); if (group) gov.group = group;
  const contractM = cfg.match(/contract:\s*\n\s*enforced:\s*(true|false)/);
  if (contractM) gov.contract = contractM[1] === "true" ? "enforced" : "off";
  const onSchema = grabIn(cfg, /\bon_schema_change:\s*([\w.-]+)/); if (onSchema) config["on_schema_change"] = onSchema;
  const strat = grabIn(cfg, /incremental_strategy:\s*(\w+)/);
  if (isIncremental && strat) config["incremental_strategy"] = strat;
  const uniqueKey = grabIn(cfg, /unique_key:\s*(\[[^\]]*\]|['"][^'"]*['"]|\w+)/);
  if (uniqueKey) config["unique_key"] = uniqueKey.replace(/['"]/g, "");
  return { gov, config };
}

/** the `meta:` sub-block kv pairs (excluding the inc_strategy/unique_key configs). */
function parseMeta(cfgLines: string[]): { key: string; value: string }[] {
  const meta: { key: string; value: string }[] = [];
  const metaStart = cfgLines.findIndex((l) => /^\s*meta:\s*$/.test(l));
  if (metaStart < 0) return meta;
  const metaIndent = indentOf(cfgLines[metaStart]!);
  for (let i = metaStart + 1; i < cfgLines.length; i++) {
    const l = cfgLines[i]!; if (!l.trim()) continue;
    if (indentOf(l) <= metaIndent) break;
    const kv = l.match(/^\s*([\w.-]+):\s*(.+?)\s*$/);
    if (!kv || /^[[{]/.test(kv[2]!)) continue;
    const k = kv[1]!, v = kv[2]!.replace(/['"]/g, "");
    if (k !== "incremental_strategy" && k !== "unique_key") meta.push({ key: k, value: v });
  }
  return meta;
}

/** inline `tags: [...]` from the config block. */
function parseTags(cfg: string): string[] {
  const inlineTags = cfg.match(/\btags:\s*\[([^\]]*)\]/);
  if (!inlineTags) return [];
  return inlineTags[1]!.split(",").map((s) => s.trim().replace(/['"]/g, "")).filter(Boolean);
}

export function deriveInfo(m: ModelPayload): DerivedInfo {
  const raw = (m.model_yaml && m.model_yaml.raw) || "";
  const cfgLines = extractConfigBlock(raw);
  const cfg = cfgLines.join("\n");
  const { gov, config } = parseGovernanceConfig(cfg, m.is_incremental);
  return {
    materialized: deriveMaterialized(raw, m.is_incremental),
    grain: deriveGrain(m.findings),
    gov, config,
    meta: parseMeta(cfgLines),
    tags: parseTags(cfg),
  };
}

// ── the per-model record ─────────────────────────────────────────────────────
export interface ModelRecord {
  change: "added" | "removed" | "modified"; schema: string; desc: string;
  compiled: string; diffFile: string; lang: "sql" | "yaml";
  compiledSql: string | null; nodeSpans: Record<string, unknown> | null;
  rawSql: string | null; rawCte: RawGraph | null; rawNodeSpans: Record<string, unknown> | null;
  rawSpansAll: Record<string, LineSpan> | null;
  columnSpans: Record<string, unknown> | null; rawColumnSpans: Record<string, unknown> | null;
  columnLineage: Record<string, ColSource[]> | null; columnEdges: ColEdge[] | null; columnTerminal: string | null;
  nodeMap: unknown; diff: Hunk[]; patch: string; comments: CommentEntry[];
  unitTests: AdaptedTest[]; coverage: Coverage; info: DerivedInfo; downstream?: string[];
}
export interface CommentEntry {
  line: number | null | undefined; endLine: number | null | undefined; side: "old" | "new";
  author: string; association: string; body: string; outdated: boolean; threadResolved: boolean; seqInThread: number;
}

/**
 * buildModelRecord — one model's full record. The compiled-pane spine is honest:
 * compiledSql/nodeSpans are NULL when a fixture has no code_map (the honest-empty
 * Compiled state), never `raw_sql` masquerading as compiled. `compiled` (the
 * fallback text for the legacy pane) prefers code_map.compiled, then raw_sql, then
 * the per-CTE compiled_sql join.
 */
/** The §3a source-map spine projected off a model's code_map. Every field is
 * honestly NULL when the code_map (or that field) is absent — NEVER raw_sql
 * masquerading as compiled. Extracted so buildModelRecord stays a flat assembly. */
interface CodeMapSpine {
  compiledSql: string | null; nodeSpans: Record<string, unknown> | null;
  rawNodeSpans: Record<string, unknown> | null; columnSpans: Record<string, unknown> | null;
  rawColumnSpans: Record<string, unknown> | null; nodeMap: unknown;
}
function codeMapSpine(codeMap: ModelPayload["code_map"]): CodeMapSpine {
  const cm = codeMap ?? null;
  return {
    compiledSql: (cm && cm.compiled) ?? null,
    nodeSpans: (cm && cm.node_spans) ?? null,
    rawNodeSpans: (cm && cm.raw_node_spans) ?? null,
    columnSpans: (cm && cm.column_spans) ?? null,
    rawColumnSpans: (cm && cm.raw_column_spans) ?? null,
    nodeMap: (cm && cm.node_map) ?? null,
  };
}

export function buildModelRecord(m: ModelPayload, liveCommentsFor: (p: string) => CommentEntry[]): ModelRecord {
  const path = m.path ?? `${m.name}.sql`;
  const lang: "sql" | "yaml" = path.endsWith(".yml") || path.endsWith(".yaml") ? "yaml" : "sql";
  const compiled = m.raw_sql || Object.values(m.compiled_sql ?? {}).join("\n\n") || "";
  const codeMap = m.code_map ?? null;
  const fileName = path.split("/").pop() || m.name;
  const colEdges = buildColEdges(m.column_lineage);
  const spine = codeMapSpine(codeMap);
  return {
    change: stateToChange(m.state), schema: schemaOf(path), desc: m.description ?? "",
    compiled, diffFile: path, lang,
    ...spine, // compiledSql/nodeSpans/rawNodeSpans/columnSpans/rawColumnSpans/nodeMap — honest-null
    rawSql: m.raw_sql ?? null,
    rawCte: ensureMainNode(rawDagToGraph(m.dag, codeMap), fileName),
    rawSpansAll: buildRawSpans(m, codeMap),
    columnLineage: buildColLineage(m.column_lineage),
    columnEdges: colEdges,
    columnTerminal: colTerminal(colEdges),
    diff: blockDiffToHunks(m.sql_diff),
    patch: m.sql_diff ? blockDiffToPatch(m.sql_diff, path) : sourceToContextPatch(compiled, path),
    comments: liveCommentsFor(path),
    unitTests: (m.tests ?? []).map(adaptTest),
    coverage: findingsToCoverage(m.findings),
    info: deriveInfo(m),
  };
}

// ── the dataset ──────────────────────────────────────────────────────────────
export interface Dataset {
  D: Record<string, ModelRecord>;
  CTE: Record<string, CteGraph | null>;
  MODELS: string[];
  SCOPE: Record<string, unknown>;
  seeds: string[];
  seedRecords: Record<string, SeedRecord>;
  prScope: PrScope["data"] | null;
  prSelectable: string[];
  /**
   * The MODEL-ONLY review scope (cute-dbt#495): `prSelectable` filtered to ids
   * that are actual MODELS (own keys of `D`). `prSelectable` carries every
   * non-connector/non-halo PR-scope node — INCLUDING seeds + macros, which belong
   * to their own entities, not the Models review LOOP. The keyboard `x`/`N` loop,
   * the progress chip, and `markReviewedAdvance` ALL walk THIS list so they never
   * advance onto a recordless seed/macro (which would show the wrong model's diff
   * while marking the seed/macro reviewed — the never-a-false-claim violation).
   * Falls back to `MODELS` when the PR scope is empty (parity with prSelectable).
   */
  prSelectableModels: string[];
  prScopeByAxis: Record<string, PrScope | null>;
}

const _cache = new WeakMap<ContextData, Dataset>();

/**
 * buildDataset(raw) — WeakMap-memoized. Keyed on the ContextData object identity
 * (a parsed fixture/`--context-out` payload is a stable reference), so repeat
 * builds for the same source are O(1) + GC-friendly (replaces the prototype's
 * `_cache[srcKey]` object-keyed-by-string global).
 */
/** Build the path→live-comment-entries lookup (a thread flattens to one entry
 * per comment; outdated/line-less threads are dropped). */
function makeLiveCommentsFor(data: ContextData): (path: string) => CommentEntry[] {
  const tbp = threadsByPath(data);
  const threadComments = (t: RenderedThread): CommentEntry[] => (t.comments ?? []).map((c, i) => ({
    line: t.line, endLine: t.line, side: t.side === "Left" ? "old" as const : "new" as const,
    author: c.author ?? "ghost", association: "CONTRIBUTOR",
    body: c.body, outdated: !!t.outdated, threadResolved: !!t.resolved, seqInThread: i,
  }));
  return (path: string) => (tbp.get(path) ?? []).filter((t) => t.line != null && !t.outdated).flatMap(threadComments);
}

/** Attach model-level downstream consumers (from the PR-scope DAG edges) to each
 * record — the best-available blast radius until cross-model column edges land.
 * tracked: cute-dbt#508 — B1 */
function attachDownstream(prScope: PrScope | null, models: string[], D: Record<string, ModelRecord>): void {
  if (!prScope || !prScope.data) return;
  // null-proto map: keys are untrusted model names from DAG edges (see parseSeedColumnTypes).
  const downByName: Record<string, string[]> = Object.create(null) as Record<string, string[]>;
  (prScope.data.edges ?? []).forEach(([from, to]) => {
    if (from && to && from !== to) (downByName[from] = downByName[from] ?? []).push(to);
  });
  models.forEach((n) => { if (D[n]) D[n]!.downstream = downByName[n] ? [...new Set(downByName[n])] : []; });
}

/** The all + per-axis (body/config/unit_test) PR-scope graphs. */
function buildScopeByAxis(
  data: ContextData, all: PrScope | null,
  schemaByName: Record<string, string>, matByName: Record<string, { materialized: string }>, changeByName: Record<string, string>,
): Record<string, PrScope | null> {
  const out: Record<string, PrScope | null> = { all };
  const byAxis = data.pr_dag && (data.pr_dag as PrDagPayload).by_axis;
  if (byAxis) (["body", "config", "unit_test"] as const).forEach((ax) => {
    if (byAxis[ax]) out[ax] = prDagToScope(byAxis[ax], schemaByName, matByName, changeByName);
  });
  return out;
}

/** The flattened PR + scope header the views read. */
function buildScope(data: ContextData, inScope: number): Record<string, unknown> {
  const pr = data.pr_ref ?? ({} as NonNullable<ContextData["pr_ref"]>);
  return {
    baseline: data.baseline || "main",
    prTitle: pr.title || "Pull request", prNumber: pr.number || 0, prUrl: pr.url || "",
    prAuthor: pr.author || "", prBody: pr.body || "", prState: pr.state || "open",
    prDraft: !!pr.draft, prMergeable: pr.mergeable_state || "", prHead: pr.head || "",
    prReviewers: pr.reviewers ?? [], prChecks: pr.checks ?? null, prCreatedAt: pr.created_at || "",
    inScope, removed: data.removed_models ?? [],
  };
}

export function buildDataset(data: ContextData): Dataset {
  const cached = _cache.get(data);
  if (cached) return cached;

  const { seeds, seedRecords } = buildSeedRecords(data);
  const liveCommentsFor = makeLiveCommentsFor(data);

  // null-proto maps: keys are untrusted model names (see parseSeedColumnTypes).
  const D: Record<string, ModelRecord> = Object.create(null) as Record<string, ModelRecord>;
  const CTE: Record<string, CteGraph | null> = Object.create(null) as Record<string, CteGraph | null>;
  const MODELS: string[] = [];
  const schemaByName: Record<string, string> = Object.create(null) as Record<string, string>;
  (data.models ?? []).forEach((m) => { schemaByName[m.name] = schemaOf(m.path ?? `${m.name}.sql`); });
  (data.models ?? []).forEach((m) => {
    const fileName = (m.path ?? `${m.name}.sql`).split("/").pop() || m.name;
    MODELS.push(m.name);
    D[m.name] = buildModelRecord(m, liveCommentsFor);
    CTE[m.name] = ensureMainNode(dagToCte(m.dag), fileName);
  });

  // null-proto maps: keys are untrusted model names (see parseSeedColumnTypes).
  const matByName: Record<string, { materialized: string }> = Object.create(null) as Record<string, { materialized: string }>;
  const changeByName: Record<string, string> = Object.create(null) as Record<string, string>;
  MODELS.forEach((n) => {
    matByName[n] = { materialized: D[n]?.info.materialized || "view" };
    if (D[n]) changeByName[n] = D[n]!.change;
  });
  const prScopeBuilt = prDagToScope(data.pr_dag, schemaByName, matByName, changeByName);
  attachDownstream(prScopeBuilt, MODELS, D);

  const prSelectable = prScopeBuilt ? prScopeBuilt.selectable : MODELS;
  // the MODEL-ONLY review scope: drop the seed/macro/non-model ids prSelectable
  // carries — they have no record in D and belong to their own entities, not the
  // Models review loop. (cute-dbt#495: the keyboard `x`/`N` loop + the progress
  // chip + markReviewedAdvance all walk THIS list, so they never advance onto a
  // recordless seed/macro.)
  const prSelectableModels = prSelectable.filter((id) => !!D[id]);

  const ds: Dataset = {
    D, CTE, MODELS, seeds, seedRecords,
    SCOPE: buildScope(data, MODELS.length),
    prScope: prScopeBuilt ? prScopeBuilt.data : null,
    prSelectable,
    prSelectableModels,
    prScopeByAxis: buildScopeByAxis(data, prScopeBuilt, schemaByName, matByName, changeByName),
  };
  _cache.set(data, ds);
  return ds;
}
