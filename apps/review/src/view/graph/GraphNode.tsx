// The shared custom @xyflow/react node — the VERBATIM port of graph.js's two
// node branches, rendered as a real DOM element (React Flow's DOM nodes give the
// SAME native hit-testing-under-transform the prototype hand-built in SVG, the
// bug class that breaks canvas renderers).
//
//   • PR-lineage node (carries `kind`): change-state LEFT stripe color, title-only
//     top row, resource-type glyph bottom-left (model=db-cylinder / seed=table /
//     macro=braces), materialization glyph bottom-right (view=eye / table=grid /
//     incremental=appended-rows). context = dimmed + dashed; provisional = dashed.
//   • structural node (CTE/raw/column DAG): tone stripe + name + sub.
//
// LAYER: view (renders domain graph-model facts; never recomputes them).
import React from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import {
  CHANGE_COLOR, NODE_H,
  type ChangeState, type GraphNode as GraphNodeFacts, type Materialization, type NodeKind, type NodeTone,
} from "../../domain/graph-model";

/** The per-node render data threaded through React Flow's `node.data`. */
export interface GraphNodeData extends Record<string, unknown> {
  facts: GraphNodeFacts;
  /** width (grow-to-fit, from elk/nodeWidth). */
  w: number;
  /** is this the selected instance? */
  selected?: boolean;
  /** is this the keyboard cursor node? */
  cursor?: boolean;
  /** is this the focus (subject) node — accent border, never selectable? */
  focus?: boolean;
  /** is this node dimmed (out of the selectable set)? */
  dimmed?: boolean;
}

const STRIPE: Record<string, string> = {
  modified: "var(--role-modified, #e69f00)", added: "var(--role-added, #1a7f37)",
  base: "var(--role-base, #6c7086)", cte: "var(--role-cte, #6c7086)",
  final: "var(--role-final, #e69f00)", source: "var(--role-base, #6c7086)",
  context: "var(--text-muted, #6c7086)", incremental: "var(--mat-incremental, #e69f00)",
};

// resource-type glyph (bottom-left): model = stacked DB cylinder · seed = data
// table · macro = braces. (graph.js typeGlyph, verbatim paths.)
function TypeGlyph({ kind }: { kind: NodeKind }): React.ReactElement {
  const color =
    kind === "macro" ? "var(--legend-6, #8250df)" : kind === "seed" ? "var(--legend-2, #1a7f37)" : "var(--legend-3, #0969da)";
  let g: React.ReactElement;
  if (kind === "macro") {
    g = (
      <g>
        <path d="M6.2 2.5c-1.3 0-1.8.7-1.8 1.9v1c0 .9-.4 1.6-1.4 1.6 1 0 1.4.7 1.4 1.6v1c0 1.2.5 1.9 1.8 1.9" />
        <path d="M9.8 2.5c1.3 0 1.8.7 1.8 1.9v1c0 .9.4 1.6 1.4 1.6-1 0-1.4.7-1.4 1.6v1c0 1.2-.5 1.9-1.8 1.9" />
      </g>
    );
  } else if (kind === "seed") {
    g = (
      <g>
        <rect x="1.5" y="4" width="13" height="8" rx="1" />
        <path d="M1.5 7h13M1.5 9.5h13M5.5 4v8M10 4v8" />
      </g>
    );
  } else {
    g = (
      <g>
        <ellipse cx="8" cy="3.5" rx="5" ry="1.8" />
        <path d="M3 3.5v9c0 1 2.2 1.8 5 1.8s5-.8 5-1.8v-9" />
        <path d="M3 8c0 1 2.2 1.8 5 1.8s5-.8 5-1.8" />
      </g>
    );
  }
  return (
    <svg data-testid="type-glyph" data-kind={kind} width="16" height="16" viewBox="0 0 16 16"
      style={{ position: "absolute", left: 13, bottom: 8 }} fill="none" stroke={color}
      strokeWidth="1.25" strokeLinecap="round" strokeLinejoin="round" aria-label={kind}>
      <title>{kind}</title>
      {g}
    </svg>
  );
}

// materialization glyph (bottom-right): view = eye · table = boxed grid ·
// incremental = appended-rows. (graph.js matGlyph, verbatim paths.)
function MatGlyph({ mat }: { mat: Exclude<Materialization, null> }): React.ReactElement {
  const color = mat === "incremental" ? "var(--mat-incremental, #e69f00)" : "var(--text-muted, #6c7086)";
  let g: React.ReactElement;
  if (mat === "incremental") {
    g = (
      <g>
        <rect x="1" y="9.4" width="12" height="2.6" rx="0.7" fill="none" stroke="currentColor" strokeWidth="1.1" opacity="0.5" />
        <rect x="1" y="5.7" width="12" height="2.6" rx="0.7" fill="none" stroke="currentColor" strokeWidth="1.1" opacity="0.5" />
        <rect x="1" y="1.4" width="12" height="3.1" rx="0.7" fill="currentColor" stroke="none" />
      </g>
    );
  } else if (mat === "table") {
    g = (<g><rect x="1.5" y="2.5" width="11" height="9" rx="1" /><path d="M1.5 5.4h11M6 5.4v6.1" /></g>);
  } else {
    g = (<g><path d="M1 7c1.7-2.9 4-4.4 6-4.4S11.3 4.1 13 7c-1.7 2.9-4 4.4-6 4.4S2.7 9.9 1 7z" /><circle cx="7" cy="7" r="1.7" /></g>);
  }
  return (
    <svg data-testid="mat-glyph" data-mat={mat} width="16" height="16" viewBox="0 0 16 16"
      style={{ position: "absolute", right: 12, bottom: 8, color }} fill="none" stroke={color}
      strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" aria-label={mat}>
      <title>{mat}</title>
      {g}
    </svg>
  );
}

/** A PR-lineage node (change-stripe + type + mat glyphs). */
function PrNode({ facts, w, selected, focus, dimmed }: GraphNodeData): React.ReactElement {
  const change = (facts.change ?? "modified") as ChangeState;
  const ctx = change === "context" || facts.context;
  const stripe = ctx ? "var(--text-muted, #6c7086)" : CHANGE_COLOR[change] ?? "var(--border, #6c7086)";
  const border = selected ? "var(--selected, #e91e63)" : focus ? "var(--accent, #58a6ff)" : "var(--border, #6c7086)";
  return (
    <div
      data-testid="graph-node"
      data-kind={facts.kind}
      data-change={change}
      data-context={ctx ? "true" : "false"}
      data-selected={selected ? "true" : "false"}
      data-dimmed={dimmed ? "true" : "false"}
      style={{
        position: "relative", width: w, height: NODE_H,
        background: "var(--surface, #16161e)", borderRadius: 8,
        border: `${selected || focus ? 2.5 : 1.5}px ${ctx && !selected ? "dashed" : "solid"} ${border}`,
        opacity: ctx ? 0.5 : dimmed ? 0.34 : 1,
        font: "12px system-ui, sans-serif", color: "var(--text, #cdd6f4)",
        boxShadow: selected ? "0 1px 2px rgba(233,30,99,0.25)" : undefined,
      }}
    >
      <Handle type="target" position={Position.Left} style={{ opacity: 0 }} />
      <div data-testid="node-stripe" style={{ position: "absolute", left: 0, top: 0, width: 5, height: NODE_H, borderRadius: 2, background: stripe }} />
      <div className="node-name" style={{ position: "absolute", left: 16, top: 9, right: 12, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontWeight: 600 }}>
        {facts.label}
      </div>
      {facts.kind && <TypeGlyph kind={facts.kind} />}
      {ctx ? (
        <div className="node-sub" style={{ position: "absolute", left: 34, bottom: 6, fontSize: 10, color: "var(--text-muted, #6c7086)" }}>{facts.sub}</div>
      ) : facts.mat ? (
        <MatGlyph mat={facts.mat} />
      ) : null}
      <Handle type="source" position={Position.Right} style={{ opacity: 0 }} />
    </div>
  );
}

/** A structural node (CTE / raw / column DAG): tone stripe + name + sub. */
function ToneNode({ facts, w, selected, focus, dimmed }: GraphNodeData): React.ReactElement {
  const incOnly = facts.incrementalOnly;
  // a {% for %} loop COLLAPSE — its own treatment, DISTINCT from incremental-only
  // (an is_incremental strip). Never borrow the incremental-amber border/badge: a
  // template collapse is not an is_incremental claim (cute-dbt#497 finding 3).
  const templated = facts.templated && !incOnly;
  const border = selected
    ? "var(--selected, #e91e63)"
    : incOnly ? "var(--mat-incremental, #e69f00)"
    : templated ? "var(--legend-6, #8250df)"
    : focus ? "var(--accent, #58a6ff)"
    : facts.provisional ? "var(--text-muted, #6c7086)" : "var(--border, #6c7086)";
  const dashed = ((incOnly || templated) && !selected) || facts.provisional || dimmed;
  return (
    <div
      data-testid="graph-node"
      data-tone={facts.tone}
      data-templated={templated ? "true" : "false"}
      data-provisional={facts.provisional ? "true" : "false"}
      data-selected={selected ? "true" : "false"}
      data-dimmed={dimmed ? "true" : "false"}
      style={{
        position: "relative", width: w, height: NODE_H,
        background: focus ? "color-mix(in srgb, var(--accent, #58a6ff) 9%, var(--surface, #16161e))" : "var(--surface, #16161e)",
        borderRadius: 8,
        border: `${selected || focus ? 2.5 : 1.5}px ${dashed ? "dashed" : "solid"} ${border}`,
        opacity: dimmed ? 0.34 : facts.provisional ? 0.8 : 1,
        font: "12px system-ui, sans-serif", color: "var(--text, #cdd6f4)",
      }}
    >
      <Handle type="target" position={Position.Left} style={{ opacity: 0 }} />
      <div data-testid="node-stripe" style={{ position: "absolute", left: 0, top: 0, width: 5, height: NODE_H, borderRadius: 2, background: STRIPE[(facts.tone as NodeTone) ?? "base"] ?? STRIPE.base }} />
      <div className="node-name" style={{ position: "absolute", left: 16, top: 9, right: 12, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontWeight: 600 }}>
        {facts.label}
      </div>
      <div className="node-sub" style={{ position: "absolute", left: 16, bottom: 6, fontSize: 10, color: "var(--text-muted, #6c7086)" }}>
        {facts.sub}
        {(facts.tone === "modified" || facts.tone === "added") && (
          <span style={{ color: STRIPE[facts.tone] }}> · {facts.tone}</span>
        )}
      </div>
      {incOnly && (
        <div style={{ position: "absolute", right: 10, top: 8, fontSize: 8, fontWeight: 700, letterSpacing: "0.05em", color: "var(--mat-incremental, #e69f00)" }}>RAW ONLY</div>
      )}
      {templated && (
        <div style={{ position: "absolute", right: 10, top: 8, fontSize: 8, fontWeight: 700, letterSpacing: "0.05em", color: "var(--legend-6, #8250df)" }}>TEMPLATE</div>
      )}
      <Handle type="source" position={Position.Right} style={{ opacity: 0 }} />
    </div>
  );
}

/** The shared custom node — dispatches PR-kind vs structural by `facts.kind`. */
export function GraphNodeView({ data }: NodeProps): React.ReactElement {
  const d = data as GraphNodeData;
  const cursorRing = d.cursor ? (
    <div data-testid="cursor-ring" style={{
      position: "absolute", left: -4, top: -4, width: d.w + 8, height: NODE_H + 8,
      borderRadius: 10, border: "1.5px dashed var(--accent, #58a6ff)", pointerEvents: "none",
    }} />
  ) : null;
  return (
    <div style={{ position: "relative" }}>
      {cursorRing}
      {d.facts.kind ? <PrNode {...d} /> : <ToneNode {...d} />}
    </div>
  );
}

export const GRAPH_NODE_TYPE = "graphNode";
export const GRAPH_NODE_TYPES = { [GRAPH_NODE_TYPE]: GraphNodeView };
