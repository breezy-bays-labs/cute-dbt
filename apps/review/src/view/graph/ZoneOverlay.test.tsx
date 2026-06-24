// ZoneOverlay — the pure zone-ring geometry (zoneRects / ringOf). The
// click-through SVG overlay itself (useViewport) is exercised by the Playwright
// e2e; here we pin the concentric-ring math: outer rings get a larger pad, nested
// loops nest, and a zone with no laid-out members is dropped (honest-empty).
import { describe, it, expect } from "vitest";
import { ringOf, zoneRects } from "./ZoneOverlay";
import { NODE_H, type GraphZone, type PlacedNode } from "../../domain/graph-model";

const place = (id: string, x: number, y: number, w = 200): PlacedNode => ({ id, label: id, x, y, w });

describe("ringOf", () => {
  it("the deepest overlapping zone is ring 0; an outer (shallower) zone is ring +1", () => {
    const outer: GraphZone = { id: "o", label: "for region", depth: 0, members: ["a", "b"] };
    const inner: GraphZone = { id: "i", label: "for status", depth: 1, members: ["b"] };
    const zs = [outer, inner];
    expect(ringOf(zs, inner)).toBe(0); // deepest
    expect(ringOf(zs, outer)).toBe(1); // one ring out
  });
});

describe("zoneRects", () => {
  const byId: Record<string, PlacedNode> = {
    a: place("a", 0, 0),
    b: place("b", 400, 0),
    c: place("c", 800, 0),
  };

  it("computes a ring rectangle enclosing the zone members", () => {
    const z: GraphZone = { id: "z", label: "for region", depth: 0, members: ["a", "b"] };
    const [rect] = zoneRects([z], byId, null);
    expect(rect).toBeDefined();
    // encloses x 0..600 and y 0..NODE_H, plus padding (so rx < 0, rw > 600).
    expect(rect!.rx).toBeLessThan(0);
    expect(rect!.rw).toBeGreaterThan(600);
    expect(rect!.rh).toBeGreaterThan(NODE_H);
    expect(rect!.on).toBe(false);
  });

  it("marks the selected zone on", () => {
    const z: GraphZone = { id: "z", label: "for region", depth: 0, members: ["a", "b"] };
    const [rect] = zoneRects([z], byId, "z");
    expect(rect!.on).toBe(true);
  });

  it("nested rings get progressively larger pad (outer ring wider than inner)", () => {
    const outer: GraphZone = { id: "o", label: "for region", depth: 0, members: ["a", "b", "c"] };
    const inner: GraphZone = { id: "i", label: "for status", depth: 1, members: ["b", "c"] };
    const rects = zoneRects([outer, inner], byId, null);
    const o = rects.find((r) => r.id === "o")!;
    const i = rects.find((r) => r.id === "i")!;
    expect(o.ring).toBeGreaterThan(i.ring); // outer is a higher ring index
    // a higher ring = larger pad → the outer ring's top edge is higher (smaller ry).
    expect(o.ry).toBeLessThan(i.ry);
  });

  it("drops a zone with no laid-out members (honest — never an empty ring)", () => {
    const z: GraphZone = { id: "ghost", label: "for none", depth: 0, members: ["missing"] };
    expect(zoneRects([z], byId, null)).toEqual([]);
  });

  it("no zones → no rects", () => {
    expect(zoneRects(undefined, byId, null)).toEqual([]);
    expect(zoneRects([], byId, null)).toEqual([]);
  });
});
