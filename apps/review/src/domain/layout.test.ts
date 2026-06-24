// layoutLR unit tests — the pure first-party LR fallback layout. Deterministic:
// depth = longest path from a root; x = depth*220, y = index-within-depth*90.
import { describe, it, expect } from "vitest";
import { layoutLR } from "./layout";

interface N { id: string }
interface E { source: string; target: string }

describe("layoutLR", () => {
  it("places a single root at the origin column", () => {
    const out = layoutLR<N, E>([{ id: "a" }], []);
    expect(out[0]!.position).toEqual({ x: 0, y: 0 });
  });

  it("assigns x by longest-path depth", () => {
    // a -> b -> c  (chain): depths 0,1,2
    const nodes: N[] = [{ id: "a" }, { id: "b" }, { id: "c" }];
    const edges: E[] = [
      { source: "a", target: "b" },
      { source: "b", target: "c" },
    ];
    const out = layoutLR(nodes, edges);
    const x = (id: string) => out.find((n) => n.id === id)!.position.x;
    expect(x("a")).toBe(0);
    expect(x("b")).toBe(220);
    expect(x("c")).toBe(440);
  });

  it("uses the LONGEST path for a diamond (b and c at depth 1, d at depth 2)", () => {
    // a -> b, a -> c, b -> d, c -> d
    const nodes: N[] = [{ id: "a" }, { id: "b" }, { id: "c" }, { id: "d" }];
    const edges: E[] = [
      { source: "a", target: "b" },
      { source: "a", target: "c" },
      { source: "b", target: "d" },
      { source: "c", target: "d" },
    ];
    const out = layoutLR(nodes, edges);
    const x = (id: string) => out.find((n) => n.id === id)!.position.x;
    expect(x("a")).toBe(0);
    expect(x("b")).toBe(220);
    expect(x("c")).toBe(220);
    expect(x("d")).toBe(440);
  });

  it("stacks same-depth nodes vertically (y = index*90)", () => {
    const nodes: N[] = [{ id: "a" }, { id: "b" }, { id: "c" }];
    const edges: E[] = [
      { source: "a", target: "b" },
      { source: "a", target: "c" },
    ];
    const out = layoutLR(nodes, edges);
    const y = (id: string) => out.find((n) => n.id === id)!.position.y;
    // b and c are both depth 1 — stacked at y 0 and 90 in input order.
    expect(y("b")).toBe(0);
    expect(y("c")).toBe(90);
  });

  it("ignores edges referencing unknown nodes (robust)", () => {
    const out = layoutLR<N, E>([{ id: "a" }], [{ source: "a", target: "ghost" }]);
    expect(out).toHaveLength(1);
    expect(out[0]!.position).toEqual({ x: 0, y: 0 });
  });

  it("does not hang on a cycle (cycle guard)", () => {
    const nodes: N[] = [{ id: "a" }, { id: "b" }];
    const edges: E[] = [
      { source: "a", target: "b" },
      { source: "b", target: "a" },
    ];
    const out = layoutLR(nodes, edges);
    expect(out).toHaveLength(2);
  });

  it("preserves input order for React-key stability", () => {
    const nodes: N[] = [{ id: "c" }, { id: "a" }, { id: "b" }];
    const out = layoutLR<N, E>(nodes, []);
    expect(out.map((n) => n.id)).toEqual(["c", "a", "b"]);
  });
});
