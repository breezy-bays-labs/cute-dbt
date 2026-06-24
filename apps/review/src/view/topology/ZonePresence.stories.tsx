// ZonePresence stories (S6c) — the documented PRESENCE 3-STATE CONTRACT, the
// Storybook half of the council's "encode it as a Storybook story + a Playwright
// assertion." These stories are the canonical, type-checked specification of the
// never-a-false-claim zone-presence treatments (compiled_in / compiled_out /
// structural); the MECHANICAL enforcement is the network-denied Playwright gate
// (tests/topology-zones-shelf.spec.ts), which drives the real built app.
//
// (The project ships no Storybook RUNNER yet — same posture as
// DiffViewer.stories.tsx. These stories type-check under the normal `tsc` gate
// and document the contract; the Playwright spec is the always-green enforcer.)
//
// LAYER: view (story file; imports view + domain).
import React from "react";
import { ZonePresence, ZonePresenceList } from "./ZonePresence";
import { zonePresenceTreatments, type ZonePresenceTreatment } from "../../domain/data/zone-presence";
import type { RawZone } from "../../domain/context-data";

const meta = {
  title: "topology/ZonePresence",
  component: ZonePresence,
};
export default meta;

const span = (line: number): RawZone["start"] => ({ line, col: 1, byte: line * 10 });

// The three real shapes drawn straight from the order_status_pivot /
// order_events_enriched_incremental fixtures (the same wire the spine emits).
const COMPILED_IN: ZonePresenceTreatment = zonePresenceTreatments(
  [{ kind: "for_loop", presence: "compiled_in", template: "{{ status }}_orders", loop: "for status in statuses", start: span(8), end: span(12) }],
  { "zone:0": ["completed_orders", "pending_orders"] },
)[0]!;

const COMPILED_OUT: ZonePresenceTreatment = zonePresenceTreatments(
  [{ kind: "for_loop", presence: "compiled_out", loop: "for shard in shards", start: span(20), end: span(26) }],
  { "zone:0": [] },
)[0]!;

// order_status_pivot zone:1 — drawn straight from the real fixture: the outer
// `for region` loop is presence "compiled_in" (carried VERBATIM) yet generates NO
// CTE of its own (node_map.raw["zone:1"] = []) — the nested `for status` does. The
// honest treatment is a wrapper region; the chip MUST NOT claim "templated · 0
// CTEs" (the never-a-false-claim contradiction this slice prevents).
const STRUCTURAL: ZonePresenceTreatment = zonePresenceTreatments(
  [{ kind: "for_loop", presence: "compiled_in", loop: "for region in regions", template: null, start: span(13), end: span(19) }],
  { "zone:0": [] },
)[0]!;

// order_events_enriched_incremental zone:1 — a STRUCTURAL incremental_guard
// (an {% if is_incremental() %} region present THIS build). It must read as a
// guard region, never a "{% for %} wrapper region" (wrong-kind copy).
const STRUCTURAL_GUARD: ZonePresenceTreatment = zonePresenceTreatments(
  [{ kind: "incremental_guard", presence: "structural", start: span(41), end: span(63) }],
  { "zone:0": [] },
)[0]!;

/** compiled_in — the loop genuinely expanded into the templated CTEs (the
 *  fan-out is named; NOT the incremental-amber treatment). */
export const CompiledIn = (): React.ReactElement => <ZonePresence treatment={COMPILED_IN} />;

/** compiled_out — the load-bearing INCREMENTAL-ONLY explainer: is_incremental()
 *  stripped the loop this build, so it generated nothing. An honest explainer,
 *  never a fabricated CTE list. DISTINCT from a compiled_in TEMPLATE collapse. */
export const CompiledOutIncrementalOnly = (): React.ReactElement => <ZonePresence treatment={COMPILED_OUT} />;

/** wrapper region — a compiled_in loop that generated NO CTE of its own (a 0-CTE
 *  wrapper around a nested generating loop). Present, but never incremental-only,
 *  and NEVER a "templated · 0 CTEs" fan-out chip. */
export const StructuralWrapper = (): React.ReactElement => <ZonePresence treatment={STRUCTURAL} />;

/** structural incremental_guard — an {% if is_incremental() %} region present this
 *  build. Reads as a GUARD region, never a "{% for %} wrapper region". */
export const StructuralIncrementalGuard = (): React.ReactElement => <ZonePresence treatment={STRUCTURAL_GUARD} />;

/** The full list — every state together (the shelf's Jinja-zones section). */
export const AllStates = (): React.ReactElement => (
  <ZonePresenceList treatments={[COMPILED_IN, STRUCTURAL, STRUCTURAL_GUARD, COMPILED_OUT]} />
);
