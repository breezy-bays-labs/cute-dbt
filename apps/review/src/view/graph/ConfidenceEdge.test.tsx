// ConfidenceEdge — the shared custom edge renders each honesty 3-state:
// resolved (neutral solid) · opaque (amber dashed) · ambiguous (red dashed).
// Rendered to static markup (react-dom/server).
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ConfidenceEdge } from "./ConfidenceEdge";
import { CONFIDENCE } from "../../domain/graph-model";
import type { ColumnConfidence } from "../../domain/context-data";

function render(confidence?: ColumnConfidence): string {
  const props = {
    id: "e0", source: "a", target: "b",
    sourceX: 0, sourceY: 0, targetX: 120, targetY: 60,
    sourcePosition: "right", targetPosition: "left",
    data: confidence ? { confidence } : undefined,
  };
  return renderToStaticMarkup(<ConfidenceEdge {...(props as unknown as Parameters<typeof ConfidenceEdge>[0])} />);
}

describe("ConfidenceEdge", () => {
  it("renders each confidence 3-state with the right dash + color", () => {
    for (const c of ["resolved", "opaque", "ambiguous"] as const) {
      const html = render(c);
      expect(html).toContain(`data-confidence="${c}"`);
      // React inlines the edge style → CSS form `stroke-dasharray:5 4`.
      if (CONFIDENCE[c].dashed) expect(html).toContain("stroke-dasharray:5 4");
      else expect(html).not.toContain("stroke-dasharray:");
    }
  });

  it("a missing confidence degrades to the quiet resolved baseline (never a false claim)", () => {
    const html = render(undefined);
    expect(html).toContain('data-confidence="resolved"');
    expect(html).not.toContain("stroke-dasharray");
  });
});
