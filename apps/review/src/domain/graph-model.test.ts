// graph-model — the pure graph-engine geometry + honesty logic. Tests the
// nearestInDirection cursor nav (the 1.6× off-axis penalty), the MIN_K fit-floor
// (anti-flip), the confidence 3-state resolution, and the grow-to-fit width.
import { describe, it, expect } from "vitest";
import {
  CONFIDENCE, MIN_K, NODE_W, NODE_W_MAX,
  confidenceCounts, confidenceLegend, confidenceStyle, edgeMeta, fitView, nearestInDirection, nodeWidth, recenterViewport,
  type GraphEdge, type PlacedNode,
} from "./graph-model";

const place = (id: string, x: number, y: number, w = NODE_W): PlacedNode => ({ id, label: id, x, y, w });

describe("nearestInDirection", () => {
  // a grid: center at origin, neighbors right/left/up/down + a skewed one.
  const nodes: PlacedNode[] = [
    place("c", 0, 0),
    place("right", 400, 0),
    place("left", -400, 0),
    place("up", 0, -400),
    place("down", 0, 400),
  ];

  it("picks the collinear neighbor in each direction", () => {
    expect(nearestInDirection(nodes, "c", "right")).toBe("right");
    expect(nearestInDirection(nodes, "c", "left")).toBe("left");
    expect(nearestInDirection(nodes, "c", "up")).toBe("up");
    expect(nearestInDirection(nodes, "c", "down")).toBe("down");
  });

  it("ignores nodes behind the cursor (primary <= 1)", () => {
    // looking right, only `right` is ahead; left/up/down are not to the right.
    expect(nearestInDirection(nodes, "c", "right")).toBe("right");
  });

  it("penalizes off-axis distance 1.6x: a closer-but-skewed node loses to a farther collinear one", () => {
    // `near` is closer in raw distance but heavily off-axis; `far` is collinear.
    const g: PlacedNode[] = [
      place("c", 0, 0),
      place("near", 120, 300), // primary=120, off=300 → score 120 + 480 = 600
      place("far", 500, 0), //   primary=500, off=0   → score 500
    ];
    expect(nearestInDirection(g, "c", "right")).toBe("far");
  });

  it("the 1.6x penalty is exactly what flips the choice (boundary)", () => {
    // primary 100 off 0 → 100 ; primary 0?... use two right candidates.
    const g: PlacedNode[] = [
      place("c", 0, 0),
      place("a", 200, 100), // 200 + 160 = 360
      place("b", 300, 0), //   300 + 0   = 300  ← wins (collinear)
    ];
    expect(nearestInDirection(g, "c", "right")).toBe("b");
    // shrink b's lead so the off-axis penalty on a no longer matters: a closer.
    const g2: PlacedNode[] = [place("c", 0, 0), place("a", 200, 10), place("b", 400, 0)];
    expect(nearestInDirection(g2, "c", "right")).toBe("a"); // 200+16=216 < 400
  });

  it("returns the first node when fromId is unknown; undefined when nothing ahead", () => {
    expect(nearestInDirection(nodes, "nope", "right")).toBe("c");
    expect(nearestInDirection([place("only", 0, 0)], "only", "right")).toBeUndefined();
  });
});

describe("fitView — the MIN_K anti-flip floor", () => {
  const ns = [place("a", 0, 0), place("b", 400, 200)];

  it("returns null when the canvas has no real size yet (refit later)", () => {
    expect(fitView(ns, { w: 0, h: 0 })).toBeNull();
    expect(fitView(ns, { w: 2, h: 800 })).toBeNull();
  });

  it("never produces a zero/negative (flipped) zoom — floored at MIN_K", () => {
    // a tiny canvas vs a huge graph would drive zoom negative without the floor.
    const huge = [place("a", 0, 0), place("b", 100000, 50000)];
    const vp = fitView(huge, { w: 40, h: 40 }, { pad: 56 })!;
    expect(vp.zoom).toBeGreaterThanOrEqual(MIN_K);
    expect(vp.zoom).toBeGreaterThan(0);
  });

  it("caps zoom at maxK and centers the graph", () => {
    const vp = fitView(ns, { w: 2000, h: 2000 }, { maxK: 1.15 })!;
    expect(vp.zoom).toBeLessThanOrEqual(1.15);
    expect(Number.isFinite(vp.x)).toBe(true);
    expect(Number.isFinite(vp.y)).toBe(true);
  });

  it("clamps padding so a short canvas can't flip (w - 2*pad stays >= 0)", () => {
    // pad 56 on a 100px canvas would leave -12 without the w/4 clamp.
    const vp = fitView(ns, { w: 100, h: 100 }, { pad: 56 })!;
    expect(vp.zoom).toBeGreaterThan(0);
  });

  it("zones add headroom (a zoned fit zooms out at least as much as an unzoned one)", () => {
    const plain = fitView(ns, { w: 800, h: 600 })!;
    const zoned = fitView(ns, { w: 800, h: 600 }, { hasZones: true })!;
    expect(zoned.zoom).toBeLessThanOrEqual(plain.zoom);
  });
});

describe("recenterViewport — the zero-rect guard (the #493/#516 fix)", () => {
  const n = { x: 100, w: 60, y: 40 };

  it("is a no-op (returns null) on a zero-size canvas — getBoundingClientRect 0×0 on initial mount", () => {
    expect(recenterViewport(n, { w: 0, h: 0 })).toBeNull();
  });

  it("is a no-op when EITHER width or height is 0 (no flipped transform)", () => {
    expect(recenterViewport(n, { w: 0, h: 600 })).toBeNull();
    expect(recenterViewport(n, { w: 900, h: 0 })).toBeNull();
  });

  it("is a no-op on a negative (degenerate) rect", () => {
    expect(recenterViewport(n, { w: -10, h: 600 })).toBeNull();
  });

  it("centers the node at zoom 1 once the canvas has a real size", () => {
    const vp = recenterViewport(n, { w: 900, h: 600 })!;
    expect(vp.zoom).toBe(1);
    // x centers the node's mid-x; y centers with the −28 header offset.
    expect(vp.x).toBe(900 / 2 - (100 + 60 / 2));
    expect(vp.y).toBe(600 / 2 - 28 - 40);
    expect(Number.isFinite(vp.x)).toBe(true);
    expect(Number.isFinite(vp.y)).toBe(true);
  });
});

describe("nodeWidth — grow-to-fit", () => {
  it("never below the minimum NODE_W", () => {
    expect(nodeWidth({ id: "x", label: "a" })).toBe(NODE_W);
  });
  it("grows for a long templated name, capped at NODE_W_MAX", () => {
    const long = "{{ region }}_{{ status }}_{{ segment }}_summary_with_a_really_long_tail_name";
    const w = nodeWidth({ id: "x", label: long });
    expect(w).toBeGreaterThan(NODE_W);
    expect(w).toBeLessThanOrEqual(NODE_W_MAX);
  });
  it("reserves right-pad for the incremental glyph", () => {
    const a = nodeWidth({ id: "x", label: "orders_inc", mat: "incremental" });
    const b = nodeWidth({ id: "x", label: "orders_inc" });
    expect(a).toBeGreaterThanOrEqual(b);
  });
});

describe("confidence — the honesty 3-state", () => {
  it("resolves each state to its style; resolved is the quiet (solid) baseline", () => {
    expect(confidenceStyle("resolved").dashed).toBe(false);
    expect(confidenceStyle("opaque").dashed).toBe(true);
    expect(confidenceStyle("ambiguous").dashed).toBe(true);
  });
  it("a missing/unknown confidence degrades to resolved (never a false uncertainty claim)", () => {
    expect(confidenceStyle(undefined)).toEqual(CONFIDENCE.resolved);
  });
  it("edgeMeta reads the optional 3rd tuple element", () => {
    const bare: GraphEdge = ["a", "b"];
    const meta: GraphEdge = ["a", "b", { confidence: "opaque", kind: "join" }];
    expect(edgeMeta(bare)).toBeUndefined();
    expect(edgeMeta(meta)?.confidence).toBe("opaque");
  });
  it("counts + legend reflect every confidence state present (no silent gap)", () => {
    const edges: GraphEdge[] = [
      ["a", "b", { confidence: "resolved" }],
      ["b", "c", { confidence: "opaque" }],
      ["c", "d", { confidence: "opaque" }],
      ["d", "e", { confidence: "ambiguous" }],
      ["e", "f"], // no meta — not counted
    ];
    expect(confidenceCounts(edges)).toEqual({ resolved: 1, opaque: 2, ambiguous: 1 });
    expect(confidenceLegend(edges)).toEqual(["resolved", "opaque", "ambiguous"]);
  });
});
