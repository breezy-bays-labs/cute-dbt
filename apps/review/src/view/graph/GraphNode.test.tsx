// GraphNode — the shared custom node renders each change-state stripe + each
// type/mat glyph + the context/provisional dimming. Rendered to static markup
// (react-dom/server, no jsdom dep) — the same posture as Footer.test.tsx.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ReactFlowProvider } from "@xyflow/react";
import { GraphNodeView, type GraphNodeData } from "./GraphNode";
import { nodeWidth, type GraphNode as GraphNodeFacts } from "../../domain/graph-model";

function render(facts: GraphNodeFacts, extra: Partial<GraphNodeData> = {}): string {
  const data: GraphNodeData = { facts, w: nodeWidth(facts), ...extra };
  // NodeProps requires many fields; the node only reads `data`. Cast through unknown.
  // The node mounts @xyflow Handles, which need a ReactFlowProvider ancestor.
  return renderToStaticMarkup(
    <ReactFlowProvider>
      <GraphNodeView {...({ data } as unknown as Parameters<typeof GraphNodeView>[0])} />
    </ReactFlowProvider>,
  );
}

describe("GraphNodeView — PR-lineage node", () => {
  const pr = (change: GraphNodeFacts["change"], kind: GraphNodeFacts["kind"] = "model", mat: GraphNodeFacts["mat"] = null): GraphNodeFacts =>
    ({ id: "n", label: "n", kind, change, mat });

  it("renders the LEFT change-stripe color for each change-state", () => {
    for (const c of ["added", "modified", "removed", "deleted"] as const) {
      const html = render(pr(c));
      expect(html).toContain(`data-change="${c}"`);
      expect(html).toContain('data-testid="node-stripe"');
    }
  });

  it("renders each resource-type glyph (model / seed / macro)", () => {
    expect(render(pr("modified", "model"))).toContain('data-kind="model"');
    expect(render(pr("modified", "seed"))).toContain('data-kind="seed"');
    expect(render(pr("modified", "macro"))).toContain('data-kind="macro"');
  });

  it("renders each materialization glyph (view / table / incremental)", () => {
    expect(render(pr("modified", "model", "view"))).toContain('data-mat="view"');
    expect(render(pr("modified", "model", "table"))).toContain('data-mat="table"');
    expect(render(pr("modified", "model", "incremental"))).toContain('data-mat="incremental"');
  });

  it("a context node is dimmed + dashed (and shows its sub, not a mat glyph)", () => {
    const html = render({ id: "ctx", label: "ctx", kind: "model", change: "context", context: true, sub: "connector", mat: "table" });
    expect(html).toContain('data-context="true"');
    expect(html).toContain("connector");
    expect(html).not.toContain('data-mat='); // a context node suppresses the mat glyph
  });

  it("a selected PR node carries the selected flag", () => {
    expect(render(pr("modified"), { selected: true })).toContain('data-selected="true"');
  });
});

describe("GraphNodeView — structural (tone) node", () => {
  it("renders the tone stripe for a CTE/import/final node", () => {
    for (const tone of ["base", "cte", "final", "modified"]) {
      const html = render({ id: "t", label: "t", tone });
      expect(html).toContain(`data-tone="${tone}"`);
      expect(html).toContain('data-testid="node-stripe"');
    }
  });
  it("a provisional node is dashed (honest-provisional cross-model consumer)", () => {
    expect(render({ id: "p", label: "p", tone: "cte", provisional: true })).toContain('data-provisional="true"');
  });
  it("an incrementalOnly node shows the RAW ONLY marker", () => {
    expect(render({ id: "r", label: "r", tone: "cte", incrementalOnly: true })).toContain("RAW ONLY");
  });
  it("a templated ({% for %} collapse) node shows TEMPLATE — NOT the incremental RAW ONLY marker (cute-dbt#497 finding 3)", () => {
    const html = render({ id: "z", label: "z", tone: "cte", templated: true });
    expect(html).toContain("TEMPLATE");
    expect(html).toContain('data-templated="true"');
    // a {% for %} collapse is NOT an is_incremental strip — never the RAW ONLY badge.
    expect(html).not.toContain("RAW ONLY");
  });
  it("an incrementalOnly node is never mislabeled as templated", () => {
    const html = render({ id: "r", label: "r", tone: "cte", incrementalOnly: true });
    expect(html).toContain("RAW ONLY");
    expect(html).toContain('data-templated="false"');
    expect(html).not.toContain("TEMPLATE");
  });
  it("a dimmed node carries the dimmed flag; a cursor node draws the ring", () => {
    expect(render({ id: "d", label: "d", tone: "cte" }, { dimmed: true })).toContain('data-dimmed="true"');
    expect(render({ id: "c", label: "c", tone: "cte" }, { cursor: true })).toContain('data-testid="cursor-ring"');
  });
});
