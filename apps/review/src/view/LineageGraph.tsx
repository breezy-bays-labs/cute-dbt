// The CTE/PR DAG pane — @xyflow/react rendering laid out by the BUNDLED elkjs
// worker (useElkLayout). Custom node styled by role; edges colored by edge type
// via EDGE_COLOR (dashed for unions); a legend lists every edge type present (no
// silent gap — the EdgeType-completeness contract).
import React, { useMemo } from "react";
import {
  ReactFlow,
  Handle,
  Position,
  Background,
  Controls,
  type NodeProps,
  type Node,
  type Edge,
} from "@xyflow/react";
import { toFlow, EDGE_COLOR, EDGE_DASHED } from "../domain/reshape";
import type { DagPayload, EdgeType, NodeRole } from "../domain/context-data";
import { useElkLayout } from "./useElkLayout";

type CteData = { label: string; role: NodeRole };

const ROLE_STYLE: Record<NodeRole, React.CSSProperties> = {
  import: { background: "#1f4068", color: "#cdd6f4", border: "1px solid #7aa2f7", borderRadius: 999 },
  transform: { background: "#2a2b36", color: "#cdd6f4", border: "1px solid #6c7086", borderRadius: 4 },
  final: {
    background: "#5a4410", color: "#f9e2af", border: "1px solid #e69f00", borderRadius: 4,
    clipPath: "polygon(8% 0, 92% 0, 100% 50%, 92% 100%, 8% 100%, 0 50%)",
    paddingLeft: 18, paddingRight: 18,
  },
  // raw_dag / column-graph roles reuse the structural styles (the compiled CTE
  // DAG never emits these; they appear only in the raw/column lineage panes).
  cte: { background: "#2a2b36", color: "#cdd6f4", border: "1px solid #6c7086", borderRadius: 4 },
  zone: { background: "#2a2b36", color: "#cdd6f4", border: "1px dashed #e69f00", borderRadius: 4 },
  terminal: {
    background: "#5a4410", color: "#f9e2af", border: "1px solid #e69f00", borderRadius: 4,
    clipPath: "polygon(8% 0, 92% 0, 100% 50%, 92% 100%, 8% 100%, 0 50%)",
    paddingLeft: 18, paddingRight: 18,
  },
};

function CteNode({ data }: NodeProps): React.ReactElement {
  const d = data as CteData;
  return (
    <div
      data-testid="cte-node"
      data-role={d.role}
      style={{
        padding: "6px 12px",
        font: "12px system-ui, sans-serif",
        minWidth: 70,
        textAlign: "center",
        ...ROLE_STYLE[d.role],
      }}
    >
      <Handle type="target" position={Position.Left} style={{ opacity: 0 }} />
      {d.label}
      <Handle type="source" position={Position.Right} style={{ opacity: 0 }} />
    </div>
  );
}

const NODE_TYPES = { cteNode: CteNode };

const ROLE_LEGEND: { role: NodeRole; label: string }[] = [
  { role: "import", label: "import (CTE source)" },
  { role: "transform", label: "transform (CTE step)" },
  { role: "final", label: "final select" },
];

export function LineageGraph({ dag }: { dag: DagPayload }): React.ReactElement {
  const flow = useMemo(() => toFlow(dag), [dag]);
  const nodes = useElkLayout(flow.nodes, flow.edges);

  return (
    <div data-testid="lineage-graph">
      <div style={{ height: 360, border: "1px solid #2a2b36", borderRadius: 8, background: "#16161e" }}>
        <ReactFlow
          nodes={nodes as unknown as Node[]}
          edges={flow.edges as unknown as Edge[]}
          nodeTypes={NODE_TYPES}
          fitView
          proOptions={{ hideAttribution: true }}
          nodesDraggable
          minZoom={0.2}
        >
          <Background color="#2a2b36" gap={20} />
          <Controls showInteractive={false} />
        </ReactFlow>
      </div>
      <div
        data-testid="dag-legend"
        style={{ display: "flex", flexWrap: "wrap", gap: 14, marginTop: 8, font: "12px system-ui" }}
      >
        {ROLE_LEGEND.map((r) => (
          <span key={r.role} style={{ display: "inline-flex", alignItems: "center", gap: 5 }}>
            <span
              style={{
                width: 14, height: 14, display: "inline-block",
                ...ROLE_STYLE[r.role], borderRadius: r.role === "import" ? 999 : 3,
              }}
            />
            {r.label}
          </span>
        ))}
        <span style={{ opacity: 0.4 }}>|</span>
        {flow.legend.map((t: EdgeType) => (
          <span key={t} data-testid="legend-edge" style={{ display: "inline-flex", alignItems: "center", gap: 5 }}>
            <svg width="26" height="6" style={{ overflow: "visible" }}>
              <line
                x1="0" y1="3" x2="26" y2="3"
                stroke={EDGE_COLOR[t]} strokeWidth="2"
                strokeDasharray={EDGE_DASHED[t] ? "6 4" : undefined}
              />
            </svg>
            {t}
          </span>
        ))}
      </div>
    </div>
  );
}
