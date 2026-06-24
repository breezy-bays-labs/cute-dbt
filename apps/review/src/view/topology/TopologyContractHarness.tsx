// TopologyContractHarness — a TEST-ONLY surface mounting the REAL TopologyPanes
// for the looped model `order_status_pivot` (3 {% for %} loops incl. a nested
// loop → concentric rings + a fan-out collapse) so the network-denied Playwright
// gate can drive the zone-region OVERLAY click-through + the fan-out collapse
// against a real Chromium. Reachable ONLY via `?contract=topology` (main.tsx); it
// is NEVER on the production render path.
//
// WHY a harness: V1 (cute-dbt#495) narrowed the Models sidebar to the PR-DAG
// selectable set, and the only fixtures with a generating {% for %} (the ring +
// fan-out shape) — order_status_pivot / orders — are out of that PR-scope set, so
// they are unreachable via the sidebar (tracked: cute-dbt#523). The zone geometry
// + fold are unit/integration-covered (zone-regions.test.tsx), but the LIVE
// pointer-events click-through (the click-through fill routing a ring pick) needs
// a real laid-out @xyflow viewport — which only exists in a real browser. This
// harness mounts the looped model directly so that behavioral leg is mechanically
// asserted without depending on the sidebar's PR scope.
//
// LAYER: view (mounts the real TopologyPanes + loads the fixture via the data
// loader). Test affordance, query-param-gated.
import React from "react";
import { TopologyPanes } from "./TopologyPanes";
import { loadFixture } from "../../data/fixtures";
import type { ContextData, ModelPayload } from "../../domain/context-data";

/** The looped fixture model (order_status_pivot) — the real zones-in-zones shape. */
function loopedModel(): ModelPayload {
  const data = loadFixture("context.440") as unknown as ContextData;
  const m = data.models.find((x) => x.name === "order_status_pivot");
  if (!m) throw new Error("harness: order_status_pivot fixture missing");
  return m;
}

export function TopologyContractHarness({ shiki }: { shiki: string }): React.ReactElement {
  const model = loopedModel();
  return (
    <div
      data-testid="topology-contract-harness"
      style={{ display: "flex", flexDirection: "column", height: "100vh", padding: 16, background: "#1a1b26", color: "#a9b1d6" }}
    >
      <h1 style={{ font: "14px system-ui", marginBottom: 8 }}>
        Topology — zone rings + fan-out collapse (the real looped model: order_status_pivot)
      </h1>
      <div style={{ display: "flex", minHeight: 0, flex: 1 }}>
        <TopologyPanes model={model} shiki={shiki} />
      </div>
    </div>
  );
}
