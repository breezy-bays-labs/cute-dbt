// The SyncMaps-builder tests (S6b). This is the CONSUMER-side adapter the S6a
// cursor-sync machine left as the pane's responsibility: it lifts a model's
// `code_map` spine into the `SyncMaps` the pure machine resolves over —
//   • nodeSpans      ← code_map.node_spans (compiled coords, straight through),
//   • rawNodeSpans   ← buildRawSpans (raw line-spans incl. the computed
//                      `(final select)` heuristic + `zone:N`),
//   • zones          ← the RawZone→ZoneSpan adapter (this slice owns it).
//
// The LOAD-BEARING unit here is the zone.nodeId MEMBERSHIP GUARD: a `for_loop`
// zone may template NO real DAG node (node_map.raw["zone:N"] === []), and
// `zone:N` is then absent from the rawNodeSpans table. Pointing a zone's nodeId
// at that phantom id would make the machine claim a sync to a node that does not
// exist (a never-a-false-claim violation) — so the adapter resolves `nodeId` ONLY
// when `zone:N` is a genuine member of the rawNodeSpans table, otherwise the body
// lines honestly select the ZONE itself (nodeId left undefined).

import { describe, it, expect } from "vitest";
import { buildSyncMaps, rawZonesToZoneSpans } from "./sync-maps";
import type { CodeMap, ModelPayload, RawZone } from "./context-data";

function pos(line: number, byte: number) {
  return { line, col: 1, byte };
}
function span(sLine: number, sByte: number, eLine: number, eByte: number) {
  return { start: pos(sLine, sByte), end: pos(eLine, eByte) };
}

describe("rawZonesToZoneSpans — RawZone → ZoneSpan adapter", () => {
  it("returns undefined when there are no zones", () => {
    expect(rawZonesToZoneSpans([], {})).toBeUndefined();
    expect(rawZonesToZoneSpans(undefined, {})).toBeUndefined();
  });

  it("adapts a for_loop zone that GENERATES a node to a ZoneSpan with nodeId `zone:<i>`", () => {
    const zones: RawZone[] = [
      { kind: "for_loop", start: pos(10, 100), end: pos(18, 200), presence: "compiled_out" },
    ];
    // node_map.raw["zone:0"] non-empty → the loop genuinely collapsed CTEs.
    const out = rawZonesToZoneSpans(zones, { "zone:0": span(11, 110, 17, 190) }, { "zone:0": ["status_orders"] });
    expect(out).toEqual([{ id: "z0", startLine: 10, endLine: 18, nodeId: "zone:0" }]);
  });

  it("MEMBERSHIP GUARD: omits nodeId when the loop GENERATES NO node (node_map.raw[zone] empty)", () => {
    // a {% for %} that generates no CTE → body selects the ZONE, never a phantom
    // node — even though buildRawSpans synthesizes a `zone:0` REGION span.
    const zones: RawZone[] = [
      { kind: "for_loop", start: pos(33, 1605), end: pos(35, 1738), presence: "compiled_out" },
    ];
    const out = rawZonesToZoneSpans(zones, { "zone:0": span(33, 1605, 35, 1738) }, { "zone:0": [] });
    expect(out).toEqual([{ id: "z0", startLine: 33, endLine: 35 }]);
    expect(out?.[0]).not.toHaveProperty("nodeId");
  });

  it("MEMBERSHIP GUARD: omits nodeId when `zone:<i>` is absent from the rawNodeSpans table", () => {
    // even with a generation claim, no span to resolve to → no false sync target.
    const zones: RawZone[] = [
      { kind: "for_loop", start: pos(33, 1605), end: pos(35, 1738), presence: "compiled_out" },
    ];
    const out = rawZonesToZoneSpans(zones, { events: span(1, 0, 3, 0) }, { "zone:0": ["x"] });
    expect(out).toEqual([{ id: "z0", startLine: 33, endLine: 35 }]);
    expect(out?.[0]).not.toHaveProperty("nodeId");
  });

  it("omits nodeId when no node_map oracle is supplied at all (honest default)", () => {
    const zones: RawZone[] = [
      { kind: "for_loop", start: pos(33, 1605), end: pos(35, 1738), presence: "compiled_out" },
    ];
    const out = rawZonesToZoneSpans(zones, { "zone:0": span(33, 1605, 35, 1738) });
    expect(out).toEqual([{ id: "z0", startLine: 33, endLine: 35 }]);
  });

  it("skips a zone missing its start/end boundary (never fabricates a region)", () => {
    const zones: RawZone[] = [
      { kind: "for_loop", presence: "compiled_out" } as RawZone,
      { kind: "for_loop", start: pos(5, 50), end: pos(9, 90), presence: "compiled_out" },
    ];
    const out = rawZonesToZoneSpans(zones, { "zone:1": span(6, 60, 8, 80) }, { "zone:1": ["x"] });
    // only the well-formed second zone (index 1) survives, keyed by its real index.
    expect(out).toEqual([{ id: "z1", startLine: 5, endLine: 9, nodeId: "zone:1" }]);
  });

  it("only for_loop zones become regions — an incremental_guard is not a ZoneSpan", () => {
    const zones: RawZone[] = [
      { kind: "incremental_guard", start: pos(41, 1806), end: pos(63, 2361), presence: "structural" },
    ];
    expect(rawZonesToZoneSpans(zones, {})).toBeUndefined();
  });

  it("preserves the original zone index in the `z<i>` id across mixed zone kinds", () => {
    const zones: RawZone[] = [
      { kind: "incremental_guard", start: pos(1, 0), end: pos(5, 50), presence: "structural" },
      { kind: "for_loop", start: pos(7, 70), end: pos(12, 120), presence: "compiled_out" },
    ];
    const out = rawZonesToZoneSpans(zones, { "zone:1": span(8, 80, 11, 110) }, { "zone:1": ["s"] });
    expect(out).toEqual([{ id: "z1", startLine: 7, endLine: 12, nodeId: "zone:1" }]);
  });
});

describe("buildSyncMaps — assemble SyncMaps from a model's code_map", () => {
  const baseCodeMap: CodeMap = {
    compiled: "select 1\nselect 2\nselect 3",
    node_spans: { events: span(5, 50, 9, 90), "(final select)": span(11, 110, 14, 140) },
    raw_node_spans: { events: span(1, 0, 3, 30) },
  };
  const model = (cm: CodeMap | null | undefined): ModelPayload =>
    ({ name: "m", dag: { nodes: [], edges: [] }, compiled_sql: {}, raw_sql: "a\nb\nc\nd\ne\nf", code_map: cm }) as ModelPayload;

  it("returns null when the model has NO code_map (honest-empty)", () => {
    expect(buildSyncMaps(model(null))).toBeNull();
    expect(buildSyncMaps(model(undefined))).toBeNull();
  });

  it("passes node_spans straight through as the compiled nodeSpans table", () => {
    const maps = buildSyncMaps(model(baseCodeMap));
    expect(maps).not.toBeNull();
    expect(maps!.nodeSpans).toEqual(baseCodeMap.node_spans);
  });

  it("builds rawNodeSpans from buildRawSpans (incl. the computed final-select span), widened to SourceSpan", () => {
    const maps = buildSyncMaps(model(baseCodeMap));
    // `events` raw span lifts straight through; `(final select)` is the heuristic.
    // widened to SourceSpan so the machine's validLineSpan accepts it (col/byte inert).
    expect(maps!.rawNodeSpans).toBeDefined();
    expect(maps!.rawNodeSpans!.events).toEqual({ start: { line: 1, col: 1, byte: 0 }, end: { line: 3, col: 1, byte: 0 } });
  });

  it("adapts raw_zones into the zones table with the membership guard applied", () => {
    const cm: CodeMap = {
      ...baseCodeMap,
      raw_zones: [{ kind: "for_loop", start: pos(33, 1605), end: pos(35, 1738), presence: "compiled_out" }],
    };
    const maps = buildSyncMaps(model(cm));
    // the for_loop generates no node → guard omits nodeId → body selects the zone.
    expect(maps!.zones).toEqual([{ id: "z0", startLine: 33, endLine: 35 }]);
  });

  it("returns honest nodeSpans even when there is no raw side at all", () => {
    const maps = buildSyncMaps(model({ node_spans: baseCodeMap.node_spans }));
    expect(maps!.nodeSpans).toEqual(baseCodeMap.node_spans);
    // no raw_node_spans / no raw_sql → buildRawSpans returns null → rawNodeSpans undefined.
    expect(maps!.zones).toBeUndefined();
  });

  it("returns an empty nodeSpans table (never throws) when node_spans is absent", () => {
    const maps = buildSyncMaps(model({ raw_node_spans: { events: span(1, 0, 3, 30) } }));
    expect(maps!.nodeSpans).toEqual({});
    expect(maps!.rawNodeSpans!.events).toEqual({ start: { line: 1, col: 1, byte: 0 }, end: { line: 3, col: 1, byte: 0 } });
  });
});
