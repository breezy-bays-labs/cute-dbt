// The non-Models entity aggregation (S8 / cute-dbt#500) — PURE reshapers from the
// validated `ContextData` onto the Macros / Seeds / Sources+Tests review surfaces.
// The context already did every compile/diff/anchor pass, so these are pure
// reshapes with NO recompute.
//
// HONESTY (never-a-false-claim — load-bearing here): every fact is READ from the
// context, never fabricated. The cute-dbt spine emits:
//   - `macro_lens.macros` — a macro's body lines (its signature + source), its
//     package/path, and the impacted models (its call-site usages). It does NOT
//     emit a hand-authored description, so `MacroView.description` is "" (an
//     honest-empty), never an invented sentence.
//   - `seed_cards` — a seed's columns/types/rows/diff/downstream (reshaped once in
//     `data/dataset.ts::buildSeedRecords`; reused here, not re-derived).
//   - `manifest_nodes` — the model-node metadata index (materialization, tags, and
//     the per-column TEST inventory). It carries NO discrete `source.*` node, so
//     the SOURCES list is honestly empty (never a fabricated table identity), and
//     the TEST inventory is the REAL per-column tests the spine attached — the only
//     test facts it carries. A future real source/test spine (T2) lands here
//     additively; until then these surfaces degrade HONESTLY.
//
// LAYER: PURE DOMAIN — std + the context types + the existing seed reshaper only.
// No I/O, no React, no zustand.

import type { ContextData } from "./context-data";
import { buildSeedRecords, type SeedRecord } from "./data/dataset";

// ── Macros ───────────────────────────────────────────────────────────────────

/** A macro body line (the spine's `DiffLine`-shaped record on `macro_lens`). */
export interface MacroBodyLine { kind: string; text: string }

/** A macro's call-site usage — an impacted model with its project-relative path. */
export interface MacroUsage { modelId: string; name: string; path: string }

/** The reshaped Macros review surface (one per `macro_lens.macros` entry). */
export interface MacroView {
  name: string;
  package: string;
  path: string;
  /** The macro's REAL signature line (`{% macro name(args) %}`), read from the
   *  first body line — never a guessed parameter list. */
  signature: string;
  /** The macro's parenthesized arg list extracted from the signature, or "" when
   *  the body carries none (honest-empty). */
  args: string;
  /** The macro source body lines (verbatim from the spine). */
  bodyLines: MacroBodyLine[];
  /** HONEST-EMPTY: the spine emits no macro description today. */
  description: string;
  /** The REAL impacted-model count the spine computed. */
  impactedCount: number;
  /** The REAL call-site usages (impacted models). Empty ⇒ no callers in scope. */
  usages: MacroUsage[];
}

// The macro_lens wire shape this surface reads (a narrow local view of the
// `.passthrough()` `macro_lens` field — typed here so the reshaper stays honest).
interface MacroLensMacro {
  name?: unknown;
  package?: unknown;
  path?: unknown;
  body_lines?: unknown;
  impacted_count?: unknown;
  impacted_models?: unknown;
}
interface MacroLens { macros?: unknown }

function asString(v: unknown, fallback = ""): string {
  return typeof v === "string" ? v : fallback;
}

function asBodyLines(v: unknown): MacroBodyLine[] {
  if (!Array.isArray(v)) return [];
  return v.map((l) => ({
    kind: asString((l as { kind?: unknown })?.kind, "context"),
    text: asString((l as { text?: unknown })?.text),
  }));
}

/** Extract the parenthesized arg list from a `{% macro name(args) %}` signature
 *  line. Returns "" (honest-empty) when there is no paren group. */
export function macroArgs(signature: string): string {
  const m = signature.match(/\(([^)]*)\)/);
  return m ? m[1]!.trim() : "";
}

function asUsages(v: unknown): MacroUsage[] {
  if (!Array.isArray(v)) return [];
  return v.map((u) => {
    const o = u as { model_id?: unknown; name?: unknown; path?: unknown };
    return {
      modelId: asString(o?.model_id),
      name: asString(o?.name),
      path: asString(o?.path),
    };
  });
}

/** Reshape `macro_lens.macros` into the Macros review surface. No `macro_lens` (or
 *  no `.macros`) ⇒ [] (honest-empty, never a fabricated macro). */
export function buildMacroViews(context: ContextData): MacroView[] {
  const lens = context.macro_lens as MacroLens | undefined;
  const macros = lens?.macros;
  if (!Array.isArray(macros)) return [];
  return macros.map((raw) => {
    const m = raw as MacroLensMacro;
    const bodyLines = asBodyLines(m.body_lines);
    const signature = bodyLines.length > 0 ? bodyLines[0]!.text : "";
    const usages = asUsages(m.impacted_models);
    const impactedCount =
      typeof m.impacted_count === "number" ? m.impacted_count : usages.length;
    return {
      name: asString(m.name),
      package: asString(m.package),
      path: asString(m.path),
      signature,
      args: macroArgs(signature),
      bodyLines,
      description: "", // honest-empty — the spine emits no macro desc (tracked: T2)
      impactedCount,
      usages,
    };
  });
}

// ── Seeds ──────────────────────────────────────────────────────────────────

/** The reshaped Seeds review surface (one per `seed_cards` entry). Built on the
 *  existing `buildSeedRecords` reshaper so the seed columns/types/diff/downstream
 *  are computed ONCE (no second null-proto/diff pass). */
export interface SeedView {
  name: string;
  file: string;
  change: SeedRecord["change"];
  description: string;
  columns: string[];
  colTypes: Record<string, string>;
  downstream: string[];
  /** REAL row count: the seed's diff-row count (the rows the spine carried). */
  rowCount: number;
  /** The spine's total/shown/capped row metadata (honest data-preview caps). */
  totalRows?: number;
  shownRows?: number;
  capped: boolean;
  /** The cell-level diff rows (the Data toggle's old→new preview). */
  diffRows: SeedRecord["diffRows"];
}

/** Reshape `seed_cards` into the Seeds review surface. No `seed_cards` ⇒ []
 *  (honest-empty, never a fake card). */
export function buildSeedViews(context: ContextData): SeedView[] {
  const { seeds, seedRecords } = buildSeedRecords(context);
  return seeds.map((name) => {
    const r = seedRecords[name]!;
    return {
      name,
      file: r.file,
      change: r.change,
      description: r.desc,
      columns: r.columns,
      colTypes: r.colTypes,
      downstream: r.downstream,
      rowCount: r.diffRows.length,
      totalRows: r.totalRows,
      shownRows: r.shownRows,
      capped: r.capped,
      diffRows: r.diffRows,
    };
  });
}

// ── Manifest-node index (Sources + Tests) ───────────────────────────────────

/** A per-column entry on a manifest node (the REAL docs + test list). */
export interface ManifestColumn { name: string; description: string; tests: string[] }

/** A manifest node — the model-node metadata the spine attached to a name. */
export interface ManifestNode {
  name: string;
  description: string;
  materialized: string;
  tags: string[];
  columns: ManifestColumn[];
}

/** A discrete source node (a `source.*` table identity). The 440 spine carries
 *  none, so this list is honestly empty today (tracked: T2 source spine). */
export interface SourceNode { id: string; name: string; sourceName: string; identifier: string }

/** The reshaped manifest-node index: the model-node metadata + the (currently
 *  empty) source list. */
export interface ManifestIndex { nodes: ManifestNode[]; sources: SourceNode[] }

// The manifest_nodes wire shape (a narrow local view of the `.passthrough()`
// `manifest_nodes` record — keyed by node name).
interface ManifestNodeWire {
  description?: unknown;
  materialized?: unknown;
  tags?: unknown;
  columns?: unknown;
  resource_type?: unknown;
  source_name?: unknown;
  identifier?: unknown;
}

function asTags(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

/** A column test name from the wire (`{ name: "unique" }` or a bare string). */
function asTestName(t: unknown): string {
  if (typeof t === "string") return t;
  const n = (t as { name?: unknown })?.name;
  return typeof n === "string" ? n : "";
}

function asColumns(v: unknown): ManifestColumn[] {
  if (!Array.isArray(v)) return [];
  return v.map((c) => {
    const o = c as { name?: unknown; description?: unknown; tests?: unknown };
    const tests = Array.isArray(o?.tests) ? o.tests.map(asTestName).filter((s) => s !== "") : [];
    return { name: asString(o?.name), description: asString(o?.description), tests };
  });
}

/** Reshape `manifest_nodes` into the node index. A `source.*` resource (or an
 *  explicit `resource_type: "source"`) lands in `sources`; everything else is a
 *  model node. No `manifest_nodes` ⇒ an empty index (honest-empty). */
export function buildManifestIndex(context: ContextData): ManifestIndex {
  const mn = context.manifest_nodes as Record<string, ManifestNodeWire> | undefined;
  const nodes: ManifestNode[] = [];
  const sources: SourceNode[] = [];
  if (!mn || typeof mn !== "object") return { nodes, sources };
  for (const [key, raw] of Object.entries(mn)) {
    if (!raw || typeof raw !== "object") continue;
    const rt = asString(raw.resource_type);
    if (rt === "source" || key.startsWith("source.")) {
      sources.push({
        id: key,
        name: asString(raw.identifier) || key,
        sourceName: asString(raw.source_name),
        identifier: asString(raw.identifier),
      });
      continue;
    }
    nodes.push({
      name: key,
      description: asString(raw.description),
      materialized: asString(raw.materialized),
      tags: asTags(raw.tags),
      columns: asColumns(raw.columns),
    });
  }
  return { nodes, sources };
}

/** One REAL (model, column, test) triple. */
export interface TestEntry { model: string; column: string; test: string }

/** The aggregated test inventory — the REAL per-column tests the spine carried on
 *  `manifest_nodes`, the only test facts the context exposes. */
export interface TestInventory {
  total: number;
  entries: TestEntry[];
  /** test-kind → count (e.g. `unique` → 4, `not null` → 9). */
  byKind: Record<string, number>;
}

/** Aggregate every per-column test across the manifest nodes. No `manifest_nodes`
 *  ⇒ a zero inventory (honest-empty, never a fabricated test). */
export function testInventory(context: ContextData): TestInventory {
  const { nodes } = buildManifestIndex(context);
  const entries: TestEntry[] = [];
  // null-proto map: keys are untrusted test kinds from the wire.
  const byKind: Record<string, number> = Object.create(null) as Record<string, number>;
  for (const node of nodes) {
    for (const col of node.columns) {
      for (const test of col.tests) {
        entries.push({ model: node.name, column: col.name, test });
        byKind[test] = (byKind[test] ?? 0) + 1;
      }
    }
  }
  return { total: entries.length, entries, byKind };
}
