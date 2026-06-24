// The SyncMaps builder (S6b) — the CONSUMER-side adapter the pure S6a cursor-sync
// machine deliberately left to the pane. It lifts a model's §3a `code_map` spine
// into the `SyncMaps` shape the machine resolves over, owning the two pieces the
// machine could not own without reaching into the data contract:
//
//   (1) the RawZone → ZoneSpan adapter (a {% for %} byte-zone → a line-region the
//       reverse sync resolves over), AND
//   (2) the zone.nodeId MEMBERSHIP GUARD — the never-a-false-claim core of this
//       slice. A `for_loop` zone may template NO real DAG node (it generates no
//       CTE: node_map.raw["zone:N"] === []), in which case `zone:N` is absent from
//       the rawNodeSpans table. Pointing the zone's `nodeId` at that phantom id
//       would make the machine claim a forward/reverse sync to a node that does
//       not exist. So `nodeId` is resolved ONLY when `zone:N` is a genuine member
//       of the rawNodeSpans table; otherwise it is left undefined and the zone's
//       body lines honestly select the ZONE itself (the machine's documented
//       fallback in `rawTargetForLine`).
//
// LAYER: domain (pure; std + the data contract + the sibling raw-spans reshaper
// only). It must NOT import view/chrome, the store, or @xyflow/react. It is the
// honest bridge between context-data.ts and cursor-sync.ts — both pure-domain.

import type { CodeMap, ModelPayload, RawZone, SourceSpan } from "./context-data";
import type { LineSpan as SyncLineSpan, SyncMaps, ZoneSpan } from "./cursor-sync";
import { buildRawSpans, type LineSpan as RawLineSpan } from "./data/raw-spans";

/** Does a {% for %} zone GENUINELY template a DAG node? The honest oracle is
 *  `node_map.raw["zone:<i>"]` — the fan-out map of the generated CTE ids. A loop
 *  that produced no CTE has an empty array (or no entry); a loop that collapsed N
 *  CTEs into one templated node has a non-empty array. (`buildRawSpans` keys a
 *  REGION span `zone:<i>` for EVERY for_loop regardless, so the rawNodeSpans table
 *  alone is NOT the generation oracle — it tints the region either way.) */
function zoneGeneratesNode(nodeMapRaw: Record<string, string | string[]> | undefined, zi: number): boolean {
  if (!nodeMapRaw) return false;
  const v = nodeMapRaw["zone:" + zi];
  return Array.isArray(v) ? v.length > 0 : v != null;
}

/**
 * rawZonesToZoneSpans — adapt the §3a `raw_zones` into the machine's `ZoneSpan[]`.
 * Only `for_loop` zones become selectable regions (an `incremental_guard` is a
 * marker on a node, not a region — handled elsewhere). A zone missing its
 * start/end boundary is SKIPPED (never fabricated). The `z<i>` id and the
 * `zone:<i>` nodeId both pin to the zone's ORIGINAL index in `raw_zones`, so they
 * stay aligned with `buildRawSpans` (which keys generated-CTE spans `zone:<i>`).
 *
 * MEMBERSHIP GUARD (the never-a-false-claim core): `nodeId` is set to `zone:<i>`
 * ONLY when BOTH hold —
 *   (a) the loop genuinely collapsed real CTEs into a templated node
 *       (`node_map.raw["zone:<i>"]` is non-empty — the generation oracle), AND
 *   (b) that `zone:<i>` id is a genuine member of the rawNodeSpans table the
 *       machine will `spanForNode` against (so a forward sync can land).
 * Otherwise `nodeId` is omitted and the body lines honestly select the ZONE
 * itself — never a sync to a node that does not exist.
 */
export function rawZonesToZoneSpans(
  zones: RawZone[] | undefined,
  rawNodeSpans: Record<string, SyncLineSpan>,
  nodeMapRaw?: Record<string, string | string[]>,
): ZoneSpan[] | undefined {
  if (!zones || !zones.length) return undefined;
  const out: ZoneSpan[] = [];
  zones.forEach((z, zi) => {
    if (z.kind !== "for_loop") return; // only loops are selectable regions
    if (!z.start || !z.end) return; // honest skip — no fabricated boundary
    const genId = "zone:" + zi;
    const span: ZoneSpan = { id: "z" + zi, startLine: z.start.line, endLine: z.end.line };
    // membership guard — resolve nodeId only when the loop truly generated a node
    // AND the machine has a span to resolve it to (never a phantom sync target).
    if (zoneGeneratesNode(nodeMapRaw, zi) && Object.prototype.hasOwnProperty.call(rawNodeSpans, genId)) {
      span.nodeId = genId;
    }
    out.push(span);
  });
  return out.length ? out : undefined;
}

/** Widen a raw-spans `{start:{line}, end:{line}}` to the machine's `SourceSpan`
 *  (the machine reads only `.start.line`/`.end.line`; col/byte are inert padding
 *  so a consumer can pass the table straight through `validLineSpan`). */
function widenRawSpan(sp: RawLineSpan): SourceSpan {
  return {
    start: { line: sp.start.line, col: 1, byte: 0 },
    end: { line: sp.end.line, col: 1, byte: 0 },
  };
}

/**
 * buildSyncMaps — assemble the `SyncMaps` the S6a machine resolves over from a
 * model's `code_map`. Honest-empty: a model with NO code_map returns `null` — the
 * pane then renders its no-compiled-spans state and never runs a sync.
 *
 *   • nodeSpans    — `code_map.node_spans` straight through (compiled coords), or
 *                    an empty table when absent (the machine resolves to null —
 *                    honest no-op, never a throw).
 *   • rawNodeSpans — the unified raw line-spans from `buildRawSpans` (imports/CTEs
 *                    via raw_node_spans, the `(final select)` heuristic, and the
 *                    `zone:<i>` generated-CTE collapses), widened to SourceSpan.
 *                    Undefined when there is no raw side.
 *   • zones        — the RawZone→ZoneSpan adapter output, with the membership guard
 *                    keyed off the JUST-BUILT rawNodeSpans table.
 */
export function buildSyncMaps(model: Pick<ModelPayload, "raw_sql">, codeMapArg?: CodeMap | null): SyncMaps | null;
export function buildSyncMaps(model: ModelPayload): SyncMaps | null;
export function buildSyncMaps(
  model: Pick<ModelPayload, "raw_sql"> & { code_map?: CodeMap | null },
  codeMapArg?: CodeMap | null,
): SyncMaps | null {
  const codeMap = codeMapArg !== undefined ? codeMapArg : model.code_map;
  if (!codeMap) return null;

  const nodeSpans: Record<string, SyncLineSpan> = codeMap.node_spans ?? {};

  // the unified raw line-spans (null when there is no raw side at all).
  const rawTable = buildRawSpans(model, codeMap);
  let rawNodeSpans: Record<string, SyncLineSpan> | undefined;
  if (rawTable) {
    rawNodeSpans = {};
    for (const id of Object.keys(rawTable)) {
      const sp = rawTable[id];
      if (sp) rawNodeSpans[id] = widenRawSpan(sp);
    }
  }

  const zones = rawZonesToZoneSpans(codeMap.raw_zones, rawNodeSpans ?? {}, codeMap.node_map?.raw);

  return { nodeSpans, rawNodeSpans, zones };
}
