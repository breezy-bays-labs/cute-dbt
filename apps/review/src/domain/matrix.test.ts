// The Entity×View matrix unit tests — AVAIL routing (every (entity,view) pair
// resolves to the right discriminated target), the positional ⇧digit derivation
// (view key + action id come PURELY from AVAIL position), and the Models-`code`-
// outside-AVAIL invariant.
import { describe, it, expect } from "vitest";
import {
  AVAIL,
  ENTITY_ORDER,
  viewsFor,
  defaultViewFor,
  isAvailable,
  viewPos,
  viewKeyFor,
  viewActionFor,
  viewAtDigit,
  routeTarget,
  type View,
} from "./matrix";
import type { Entity } from "./keymap";

describe("AVAIL — the matrix shape", () => {
  it("covers exactly the five entities", () => {
    expect(Object.keys(AVAIL).sort()).toEqual([...ENTITY_ORDER].sort());
  });
  it("pins the prototype's tuples", () => {
    expect(AVAIL.pr).toEqual(["overview", "lineage", "files", "timeline"]);
    expect(AVAIL.models).toEqual(["topology", "node", "data"]);
    expect(AVAIL.macros).toEqual(["review"]);
    expect(AVAIL.seeds).toEqual(["review"]);
    expect(AVAIL.else).toEqual(["review"]);
  });
  it("Models `code` is NOT in AVAIL (reachable, not tab-selectable)", () => {
    expect(isAvailable("models", "code")).toBe(false);
    expect(viewPos("models", "code")).toBe(-1);
    expect((AVAIL.models as readonly View[]).includes("code")).toBe(false);
  });
});

describe("positional view keys (⇧digit) derive purely from AVAIL position", () => {
  it("Models: ⇧1 topology · ⇧2 node · ⇧3 data", () => {
    expect(viewKeyFor("models", "topology")).toBe("⇧1");
    expect(viewKeyFor("models", "node")).toBe("⇧2");
    expect(viewKeyFor("models", "data")).toBe("⇧3");
  });
  it("PR: ⇧1..⇧4 over its four views", () => {
    expect(AVAIL.pr.map((v) => viewKeyFor("pr", v))).toEqual(["⇧1", "⇧2", "⇧3", "⇧4"]);
  });
  it("the view-N action id mirrors the position", () => {
    expect(viewActionFor("models", "topology")).toBe("view-1");
    expect(viewActionFor("models", "data")).toBe("view-3");
    expect(viewActionFor("pr", "timeline")).toBe("view-4");
  });
  it("an off-matrix view (Models `code`) has no positional key or action", () => {
    expect(viewKeyFor("models", "code")).toBe("");
    expect(viewActionFor("models", "code")).toBeNull();
  });
  it("viewAtDigit is the inverse of the position", () => {
    expect(viewAtDigit("models", 1)).toBe("topology");
    expect(viewAtDigit("models", 3)).toBe("data");
    expect(viewAtDigit("models", 4)).toBeNull(); // out of range
    expect(viewAtDigit("macros", 2)).toBeNull();
  });
});

describe("viewsFor / defaultViewFor", () => {
  it("returns the entity's tuple", () => {
    expect(viewsFor("pr")).toEqual(AVAIL.pr);
  });
  it("the default view is the FIRST in the tuple", () => {
    (ENTITY_ORDER as readonly Entity[]).forEach((e) => {
      expect(defaultViewFor(e)).toBe(AVAIL[e][0]);
    });
  });
});

describe("routeTarget — the discriminated renderView dispatcher", () => {
  it("routes each PR view", () => {
    expect(routeTarget("pr", "overview")).toEqual({ kind: "pr-overview" });
    expect(routeTarget("pr", "lineage")).toEqual({ kind: "pr-lineage" });
    expect(routeTarget("pr", "files")).toEqual({ kind: "pr-files" });
    expect(routeTarget("pr", "timeline")).toEqual({ kind: "pr-timeline" });
  });
  it("routes each Models view + the off-matrix code surface", () => {
    expect(routeTarget("models", "topology")).toEqual({ kind: "models-topology" });
    expect(routeTarget("models", "node")).toEqual({ kind: "models-node" });
    expect(routeTarget("models", "data")).toEqual({ kind: "models-data" });
    expect(routeTarget("models", "code")).toEqual({ kind: "models-code" });
  });
  it("macros/seeds/else share the review surface (carrying the entity)", () => {
    expect(routeTarget("macros", "review")).toEqual({ kind: "entity-review", entity: "macros" });
    expect(routeTarget("seeds", "review")).toEqual({ kind: "entity-review", entity: "seeds" });
    expect(routeTarget("else", "review")).toEqual({ kind: "entity-review", entity: "else" });
  });
  it("an impossible pair routes to not-available (honest placeholder, never a crash)", () => {
    expect(routeTarget("pr", "data")).toEqual({ kind: "not-available", entity: "pr", view: "data" });
    expect(routeTarget("macros", "topology")).toEqual({ kind: "not-available", entity: "macros", view: "topology" });
  });
});
