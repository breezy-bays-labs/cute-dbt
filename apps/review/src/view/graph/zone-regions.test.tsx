// Zone-region geometry + fan-out fold (S6c) — the END-TO-END proof that a REAL
// looped model (order_status_pivot, 3 {% for %} loops incl. a nested loop)
// drives concentric rings + a fan-out collapse via node_map.raw. This pins the
// zones-in-zones path the council RISK#3 mitigation requires ("keep a real looped
// model to exercise zones-in-zones") through the full pipeline:
//   rawDagToGraph (fan-out collapse) → rawGraphToGraphData → zoneRects (rings).
//
// LIVES IN THE VIEW LAYER: it crosses domain (raw-spans / topology-graphs) AND
// the view geometry (ZoneOverlay.zoneRects / useGraphLayout.fallbackPlace), so by
// the boundaries rule it is a view-layer integration test. Fixtures load via the
// `data` loader (loadFixture) — never a direct `fixture` import (boundaries).
import { describe, it, expect } from "vitest";
import { loadFixture } from "../../data/fixtures";
import type { ContextData, ModelPayload } from "../../domain/context-data";
import { rawDagToGraph, ensureMainNode } from "../../domain/data/raw-spans";
import { rawGraphToGraphData } from "../../domain/topology-graphs";
import { ringOf, zoneRects } from "./ZoneOverlay";
import { fallbackPlace } from "./useGraphLayout";
import type { PlacedNode } from "../../domain/graph-model";

const data = loadFixture("context.440") as unknown as ContextData;
const model = (name: string): ModelPayload => {
  const m = data.models.find((x) => x.name === name);
  if (!m) throw new Error("fixture model missing: " + name);
  return m;
};

function placeById(g: ReturnType<typeof rawGraphToGraphData>): Record<string, PlacedNode> {
  const byId: Record<string, PlacedNode> = Object.create(null);
  fallbackPlace(g).forEach((n) => { byId[n.id] = n; });
  return byId;
}

describe("order_status_pivot — the REAL looped model (fan-out collapse + concentric rings)", () => {
  const m = model("order_status_pivot");
  const raw = ensureMainNode(rawDagToGraph(m.dag, m.code_map), "order_status_pivot.sql");
  const g = rawGraphToGraphData(raw);

  it("collapses the {% for %} fan-out: the 6 generated CTEs fold into 2 templated zone:N nodes", () => {
    const ids = g.nodes.map((n) => n.id);
    expect(ids).toContain("zone:0"); // for status in statuses → completed/pending_orders
    expect(ids).toContain("zone:2"); // nested for status → the 4 region×status summaries
    // the generated CTE leaves are GONE (collapsed into the zone nodes).
    expect(ids).not.toContain("completed_orders");
    expect(ids).not.toContain("us_completed_summary");
    // both collapse nodes carry `templated` (their OWN flag) — NEVER incrementalOnly
    // (a template collapse is not an is_incremental strip; cute-dbt#497 finding 3).
    for (const id of ["zone:0", "zone:2"]) {
      const n = g.nodes.find((x) => x.id === id)!;
      expect(n.templated).toBe(true);
      expect(n.incrementalOnly).toBeUndefined();
    }
  });

  it("the zone regions nest: the `for region` wrapper (z1) strict-wraps the inner `for status` (z2)", () => {
    const zones = g.zones ?? [];
    const z1 = zones.find((z) => z.id === "z1");
    const z2 = zones.find((z) => z.id === "z2");
    expect(z1).toBeDefined();
    expect(z2).toBeDefined();
    // z2 (the inner generating loop) is DEEPER than z1 (the wrapper) — depth from
    // the byte-span strict-wrap in rawDagToGraph.
    expect(z2!.depth).toBeGreaterThan(z1!.depth);
    // both enclose the same templated node zone:2 (shared member ⇒ concentric).
    expect(z1!.members).toContain("zone:2");
    expect(z2!.members).toContain("zone:2");
  });

  it("zoneRects renders the nested loops as CONCENTRIC rings (outer wrapper a higher ring, larger pad)", () => {
    const byId = placeById(g);
    const rects = zoneRects(g.zones, byId, null);
    const r1 = rects.find((r) => r.id === "z1");
    const r2 = rects.find((r) => r.id === "z2");
    expect(r1).toBeDefined();
    expect(r2).toBeDefined();
    // the wrapper (z1) is the OUTER ring (higher ring index → larger pad → smaller ry).
    expect(r1!.ring).toBeGreaterThan(r2!.ring);
    expect(r1!.ry).toBeLessThan(r2!.ry);
    // the outer ring fully encloses the inner (concentric, not overlapping).
    expect(r1!.rx).toBeLessThanOrEqual(r2!.rx);
    expect(r1!.rx + r1!.rw).toBeGreaterThanOrEqual(r2!.rx + r2!.rw);
  });

  it("ringOf assigns the deepest overlapping loop a lower ring than the wrapper", () => {
    const zones = g.zones ?? [];
    const z1 = zones.find((z) => z.id === "z1")!;
    const z2 = zones.find((z) => z.id === "z2")!;
    expect(ringOf(zones, z2)).toBeLessThan(ringOf(zones, z1));
  });
});

describe("order_events_enriched_incremental — a compiled_out for_loop has NOTHING to draw (honest-empty ring)", () => {
  const m = model("order_events_enriched_incremental");
  const raw = ensureMainNode(rawDagToGraph(m.dag, m.code_map), "order_events_enriched_incremental.sql");
  const g = rawGraphToGraphData(raw);

  it("the compiled_out loop generated no CTE → no templated node + an empty-member region", () => {
    // it produced no CTE this build (is_incremental stripped it), so no zone:0 node.
    expect(g.nodes.map((n) => n.id)).not.toContain("zone:0");
    const z0 = (g.zones ?? []).find((z) => z.id === "z0");
    // the region (if present) has no members → zoneRects DROPS it (honest — no ring).
    if (z0) expect(z0.members).toEqual([]);
  });

  it("zoneRects draws NO ring for the member-less compiled_out region (never a fabricated ring)", () => {
    const byId = placeById(g);
    const rects = zoneRects(g.zones, byId, null);
    // the compiled_out region has no laid-out members → no rect (honest-empty). Its
    // honest treatment is the ZonePresence incremental-only explainer in the shelf.
    expect(rects.find((r) => r.id === "z0")).toBeUndefined();
  });
});
