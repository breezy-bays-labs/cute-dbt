// The zone-presence 3-state classifier (S6c) — the never-a-false-claim core of
// the zone-region/detail-shelf slice.
//
// A {% for %} / incremental zone has a §3a `presence` honesty axis
// (compiled_in / compiled_out / structural). This module maps that axis + the
// `node_map.raw` generation oracle to a CLASSIFICATION the view renders as a
// distinct treatment — it NEVER fabricates a span or a body. The three states:
//
//   • compiled_in   — the loop genuinely generated CTEs this build (the fan-out
//                     collapse exists; the DAG `zone:N` node is real).
//   • compiled_out  — a build-state guard (is_incremental()) STRIPPED the loop
//                     this build, so it generated nothing. The honest
//                     "incremental-only" explainer — DISTINCT from a compiled_in
//                     TEMPLATE collapse (S6b's own treatment). Never a fake body.
//   • structural    — a wrapper loop that emits no CTE of its own (its body is a
//                     region templating other loops, not a generating template).
//
// LAYER: domain (pure; std + the data contract only). It carries the §3a
// `presence` axis VERBATIM — it is the strict honesty-fold set's classifier, so
// a downgrade that lets compiled_out masquerade as compiled_in is a false claim
// the mutation gate must kill.
import type { Presence, RawZone, RawZoneKind } from "../context-data";

/** The honest classification of ONE zone's presence treatment. */
export interface ZonePresenceTreatment {
  /** the zone's ORIGINAL index in raw_zones (parallels `zone:<zi>` / `z<zi>`). */
  zi: number;
  /** the selectable region id (`z<zi>`) — the same id zoneRects/buildSyncMaps use. */
  zoneId: string;
  /** the generated DAG node id (`zone:<zi>`) when this loop collapsed real CTEs. */
  nodeId: string;
  kind: RawZoneKind;
  /** the §3a honesty axis — carried VERBATIM (never inferred from generation). */
  presence: Presence;
  /** did the loop genuinely generate ≥1 CTE this build (the fan-out collapse)? */
  generated: boolean;
  /** how many CTEs the loop collapsed (the fan-out count); 0 when none. */
  genCount: number;
  /** the generated CTE ids the loop folded into the `zone:<zi>` node (verbatim). */
  genIds: string[];
  /** is this the INCREMENTAL-ONLY treatment — a `compiled_out` zone the build
   *  stripped this run? DISTINCT from a `compiled_in` TEMPLATE collapse. */
  incrementalOnly: boolean;
  /** the displayed loop header / template (presentation; null when unnamed). */
  template: string | null;
  /** the loop expression (`for status in statuses`) — presentation. */
  loop: string | null;
  /** the honest explainer copy for the treatment (never fabricates a body). */
  explainer: string;
}

/** Normalize a `node_map.raw` arm (string | string[] | absent) to the generated
 *  CTE id array (the fan-out). A scalar is wrapped; absent ⇒ empty. */
function genIdsOf(nodeMapRaw: Record<string, string | string[]> | undefined, zi: number): string[] {
  if (!nodeMapRaw) return [];
  const v = nodeMapRaw["zone:" + zi];
  if (v == null) return [];
  return Array.isArray(v) ? v : [v];
}

/** Coerce a wire `presence` to the closed axis — an absent/unknown value is the
 *  HONEST default `structural` (never silently a compiled_in/out claim). */
function normalizePresence(p: Presence | undefined): Presence {
  return p === "compiled_in" || p === "compiled_out" || p === "structural" ? p : "structural";
}

/** The honest explainer copy per treatment — names the loop, never invents a
 *  structure. The compiled_out case is the load-bearing one (the incremental-only
 *  explainer); the others describe what IS present. */
function explainerFor(presence: Presence, generated: boolean, kind: RawZoneKind): string {
  if (presence === "compiled_out") {
    return kind === "incremental_guard"
      ? "Guarded by {% if is_incremental() %} — it applies only when dbt runs the model incrementally, so a fresh dbt compile strips it and the compiled DAG never shows it."
      : "Declared inside {% if is_incremental() %} — this loop generates CTEs only when dbt runs the model incrementally, so a fresh dbt compile strips it and the compiled DAG never shows them.";
  }
  if (presence === "compiled_in" && generated) {
    return "This {% for %} loop expands into the templated CTEs below — the compiled DAG collapses the fan-out into one templated node.";
  }
  // structural / wrapper: present this build, but emits no CTE of its own. The
  // copy MUST be kind-faithful — an incremental_guard is an {% if is_incremental() %}
  // region, NOT a {% for %} wrapper (mirror the kind-awareness in compiled_out).
  if (kind === "incremental_guard") {
    return "An {% if is_incremental() %} guard region — present this build, it scopes the rows the model writes rather than generating a CTE of its own.";
  }
  return "A {% for %} wrapper region — it templates the loops inside it; it emits no CTE of its own.";
}

/**
 * classifyZonePresence — map ONE zone + its `node_map.raw` fan-out to its honest
 * presence treatment. The `presence` axis is carried VERBATIM (never inferred);
 * `incrementalOnly` is true ONLY for a `compiled_out` zone (the stripped-this-build
 * case) — a `compiled_in` collapse and a `structural` wrapper both render their
 * own treatments and must NEVER borrow the incremental-amber explainer.
 */
export function classifyZonePresence(
  zone: RawZone,
  zi: number,
  nodeMapRaw: Record<string, string | string[]> | undefined,
): ZonePresenceTreatment {
  const presence = normalizePresence(zone.presence);
  const genIds = genIdsOf(nodeMapRaw, zi);
  const generated = genIds.length > 0;
  const incrementalOnly = presence === "compiled_out";
  return {
    zi,
    zoneId: "z" + zi,
    nodeId: "zone:" + zi,
    kind: zone.kind,
    presence,
    generated,
    genCount: genIds.length,
    genIds,
    incrementalOnly,
    template: zone.template ?? null,
    loop: zone.loop ?? null,
    explainer: explainerFor(presence, generated, zone.kind),
  };
}

/**
 * zonePresenceTreatments — classify EVERY zone in `raw_zones` (parallel-index
 * aligned with `zone:<zi>` / `z<zi>`), so a consumer can render the honest
 * 3-state treatment per zone. Honest-empty for an absent/empty zone list.
 */
export function zonePresenceTreatments(
  zones: RawZone[] | undefined,
  nodeMapRaw: Record<string, string | string[]> | undefined,
): ZonePresenceTreatment[] {
  if (!zones || !zones.length) return [];
  return zones.map((z, zi) => classifyZonePresence(z, zi, nodeMapRaw));
}
