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

const STRUCTURAL: ZonePresenceTreatment = zonePresenceTreatments(
  [{ kind: "for_loop", presence: "structural", loop: "for region in regions", start: span(13), end: span(19) }],
  { "zone:0": [] },
)[0]!;

/** compiled_in — the loop genuinely expanded into the templated CTEs (the
 *  fan-out is named; NOT the incremental-amber treatment). */
export const CompiledIn = (): React.ReactElement => <ZonePresence treatment={COMPILED_IN} />;

/** compiled_out — the load-bearing INCREMENTAL-ONLY explainer: is_incremental()
 *  stripped the loop this build, so it generated nothing. An honest explainer,
 *  never a fabricated CTE list. DISTINCT from a compiled_in TEMPLATE collapse. */
export const CompiledOutIncrementalOnly = (): React.ReactElement => <ZonePresence treatment={COMPILED_OUT} />;

/** structural — a wrapper region that templates the loops inside it; it emits no
 *  CTE of its own. Present, but never incremental-only. */
export const StructuralWrapper = (): React.ReactElement => <ZonePresence treatment={STRUCTURAL} />;

/** The full list — all three states together (the shelf's Jinja-zones section). */
export const AllThreeStates = (): React.ReactElement => (
  <ZonePresenceList treatments={[COMPILED_IN, STRUCTURAL, COMPILED_OUT]} />
);
