// The shared custom @xyflow/react edge — reads the honesty 3-state confidence off
// the edge data and styles it: resolved = neutral solid (the quiet structural
// baseline) · opaque = amber dashed · ambiguous = red dashed. (graph.js's
// confidence-3rd-tuple-element edge renderer + collineage.js CONF, verbatim.)
//
// LAYER: view (renders the domain confidence facts; never recomputes them).
import React from "react";
import { BaseEdge, getBezierPath, type EdgeProps } from "@xyflow/react";
import { confidenceStyle } from "../../domain/graph-model";
import type { ColumnConfidence } from "../../domain/context-data";

export interface ConfidenceEdgeData extends Record<string, unknown> {
  confidence?: ColumnConfidence;
}

export function ConfidenceEdge(props: EdgeProps): React.ReactElement {
  const { id, sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition, markerEnd, data } = props;
  const [path] = getBezierPath({ sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition });
  const conf = (data as ConfidenceEdgeData | undefined)?.confidence;
  const style = confidenceStyle(conf);
  return (
    <g data-testid="confidence-edge" data-confidence={conf ?? "resolved"}>
      <BaseEdge
        id={id}
        path={path}
        markerEnd={markerEnd}
        style={{ stroke: style.color, strokeWidth: 1.5, strokeDasharray: style.dashed ? "5 4" : undefined }}
      />
    </g>
  );
}

export const CONFIDENCE_EDGE_TYPE = "confidenceEdge";
export const CONFIDENCE_EDGE_TYPES = { [CONFIDENCE_EDGE_TYPE]: ConfidenceEdge };
