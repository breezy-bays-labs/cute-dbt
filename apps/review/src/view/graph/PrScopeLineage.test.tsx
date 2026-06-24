// PrScopeLineage — the PR-scope proving harness: the 3-axis ToggleGroup markup
// + active flags, and the prNode-vs-sel.models NAV SPLIT + kind-based route-out
// (routePrSelect). The full click→store wiring is exercised by the Playwright
// e2e; here we pin the toggle render + the routing decision.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { PrScopeLineage, routePrSelect } from "./PrScopeLineage";
import type { PrScope } from "../../domain/data/dataset";

const node = (id: string, kind: "model" | "seed" | "macro") =>
  ({ id, label: id, sub: "", tone: "modified", kind, change: "modified", context: false, mat: null });

const byAxis: Record<string, PrScope | null> = {
  all: { data: { nodes: [node("a", "model")], edges: [] }, selectable: ["a"], counts: {} },
  body: { data: { nodes: [], edges: [] }, selectable: [], counts: {} },
  config: { data: { nodes: [], edges: [] }, selectable: [], counts: {} },
  unit_test: { data: { nodes: [], edges: [] }, selectable: [], counts: {} },
};

describe("PrScopeLineage — the 3-axis ToggleGroup", () => {
  function render(axis: "all" | "body" | "config" | "unit_test"): string {
    return renderToStaticMarkup(
      <PrScopeLineage byAxis={byAxis} axis={axis} onAxis={() => {}} prNode={null} onPrNode={() => {}} />,
    );
  }

  it("renders an option for every available axis (All/Body/Config/Tests)", () => {
    const html = render("all");
    for (const a of ["all", "body", "config", "unit_test"]) {
      expect(html).toContain(`data-axis="${a}"`);
    }
  });

  it("the active axis carries data-active=true; the others false (single-select)", () => {
    const html = render("config");
    expect(html).toMatch(/data-axis="config"[^>]*data-active="true"/);
    expect(html).toMatch(/data-axis="body"[^>]*data-active="false"/);
  });

  it("never offers an axis the spine didn't emit", () => {
    const html = renderToStaticMarkup(
      <PrScopeLineage byAxis={{ all: byAxis.all ?? null }} axis="all" onAxis={() => {}} prNode={null} onPrNode={() => {}} />,
    );
    expect(html).toContain('data-axis="all"');
    expect(html).not.toContain('data-axis="body"');
  });
});

describe("routePrSelect — the nav split + KIND-aware route-out", () => {
  it("a model click STAYS on the PR DAG (pr-node — sets prNode, never sel.models)", () => {
    expect(routePrSelect("customers", "model", true)).toEqual({ kind: "pr-node", id: "customers" });
  });
  it("an unknown-kind node defaults to the PR cursor (stay)", () => {
    expect(routePrSelect("x", undefined, true)).toEqual({ kind: "pr-node", id: "x" });
  });
  it("a SEED node routes OUT carrying kind=seed (→ Seeds entity, never Models)", () => {
    expect(routePrSelect("raw_payments", "seed", true)).toEqual({
      kind: "open-node",
      id: "raw_payments",
      nodeKind: "seed",
    });
  });
  it("a MACRO node routes OUT carrying kind=macro (→ Macros entity, never Models)", () => {
    expect(routePrSelect("cents_to_dollars", "macro", true)).toEqual({
      kind: "open-node",
      id: "cents_to_dollars",
      nodeKind: "macro",
    });
  });
  it("a seed/macro falls back to the PR cursor when no route-out sink", () => {
    expect(routePrSelect("raw_payments", "seed", false)).toEqual({ kind: "pr-node", id: "raw_payments" });
  });
  it("a DELETED node (any kind) STAYS on the PR cursor — no live destination", () => {
    // a deleted seed is `removed`/`deleted` change-state: it keeps the prNode
    // selection rather than routing onto a Seeds surface for a node the PR deleted.
    expect(routePrSelect("dropped_seed", "seed", true, "removed")).toEqual({ kind: "pr-node", id: "dropped_seed" });
    expect(routePrSelect("dropped_macro", "macro", true, "deleted")).toEqual({ kind: "pr-node", id: "dropped_macro" });
    expect(routePrSelect("dropped_model", "model", true, "removed")).toEqual({ kind: "pr-node", id: "dropped_model" });
  });
});
