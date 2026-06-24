// LineageGraph — the SHARED graph engine for every DAG (compiled CTE DAG, raw
// DAG, column-lineage DAG, PR-scope lineage). @xyflow/react custom nodes/edges,
// laid out by the bundled elkjs worker (useGraphLayout), with the first-party
// glue ported VERBATIM from the prototype graph.js: the MIN_K fit-floor (the
// anti-flip lower bound on zoom), the ResizeObserver refit, the grow-to-fit node
// width, the `nearestInDirection` keyboard cursor nav, the viewport-synced zone
// overlay, and the confidence-3-state legend.
//
// LAYER: view (renders domain graph-model facts; never recomputes them).
import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ReactFlow, ReactFlowProvider, Background, Controls, useReactFlow,
  type Node, type Edge,
} from "@xyflow/react";
import {
  confidenceLegend, edgeMeta, fitView, nearestInDirection, CONFIDENCE,
  type GraphData, type GraphNode as GraphNodeFacts, type NavDir, type PlacedNode,
} from "../../domain/graph-model";
import { GRAPH_NODE_TYPES, GRAPH_NODE_TYPE, type GraphNodeData } from "./GraphNode";
import { CONFIDENCE_EDGE_TYPES, CONFIDENCE_EDGE_TYPE } from "./ConfidenceEdge";
import { ZoneOverlay } from "./ZoneOverlay";
import { useGraphLayout } from "./useGraphLayout";

export interface LineageGraphProps {
  data: GraphData;
  /** the selected instance id (recenters when it changes, unless recenter=false). */
  selected?: string | null;
  /** the selected zone id (recenters on its members). */
  selectedZone?: string | null;
  /** the keyboard cursor node id (dashed ring; no recenter). */
  cursor?: string | null;
  /** restrict selectability to this set (others render dimmed). */
  selectableIds?: string[];
  /** the focus (subject) node — accent border, never selectable (NavLineage). */
  focusId?: string | null;
  /** layout direction (RIGHT default; LEFT for mirrored lineage). */
  direction?: "RIGHT" | "LEFT";
  onSelect?: (id: string) => void;
  onSelectZone?: (id: string) => void;
  onBackground?: () => void;
  /** notified with the laid-out nodes (for an external cursor-nav controller). */
  onReady?: (nodes: PlacedNode[]) => void;
  fitPad?: number;
  maxK?: number;
  /** recenter on the externally-controlled selection (opt-out for step-nav). */
  recenter?: boolean;
  /** test/measure hook: an explicit canvas size (bypasses getBoundingClientRect). */
  height?: number;
}

/** The inner engine (must run inside a ReactFlowProvider for useReactFlow/Viewport). */
function LineageGraphInner(props: LineageGraphProps): React.ReactElement {
  const {
    data, selected, selectedZone, cursor, selectableIds, focusId, direction,
    onSelect, onSelectZone, onBackground, onReady, fitPad = 56, maxK = 1.15,
    recenter = true, height = 380,
  } = props;
  const placed = useGraphLayout(data, { direction });
  const { setViewport } = useReactFlow();
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const fittedRef = useRef(false);
  const selSet = selectableIds ? new Set(selectableIds) : null;
  const hasZones = !!(data.zones && data.zones.length);

  const byId = useMemo(() => {
    const m: Record<string, PlacedNode> = Object.create(null) as Record<string, PlacedNode>;
    placed.forEach((n) => { m[n.id] = n; });
    return m;
  }, [placed]);

  // measure the canvas (explicit height + measured width).
  const canvasSize = useCallback((): { w: number; h: number } => {
    const r = wrapRef.current?.getBoundingClientRect();
    return { w: r?.width ?? 900, h: r?.height ?? height };
  }, [height]);

  // the MIN_K-floored fit — computed in the domain (fitView), applied to RF.
  const doFit = useCallback((ns: PlacedNode[]): boolean => {
    const vp = fitView(ns, canvasSize(), { pad: fitPad, maxK, hasZones });
    if (!vp) return false;
    setViewport(vp);
    return true;
  }, [canvasSize, fitPad, maxK, hasZones, setViewport]);

  // re-fit whenever the layout lands; notify the ready hook.
  useEffect(() => {
    if (!placed.length) return;
    fittedRef.current = false;
    onReady?.(placed);
    const raf = requestAnimationFrame(() => { fittedRef.current = doFit(placed); });
    return () => cancelAnimationFrame(raf);
  }, [placed, doFit, onReady]);

  // ResizeObserver refit — the SVG/pane often mounts before its flex parent has a
  // height, so the first fit can measure a 0-height canvas. Refit when it first
  // reaches a real size (and on resizes until that first good fit lands).
  useEffect(() => {
    const el = wrapRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (!fittedRef.current && placed.length) fittedRef.current = doFit(placed);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [doFit, placed]);

  // recenter on the externally-controlled selection / zone (opt-out via recenter).
  useEffect(() => {
    if (!recenter || !selected) return;
    const n = byId[selected];
    if (!n) return;
    const { w, h } = canvasSize();
    setViewport({ zoom: 1, x: w / 2 - (n.x + n.w / 2), y: h / 2 - 28 - n.y });
  }, [selected, recenter, byId, canvasSize, setViewport]);

  const rfNodes: Node[] = useMemo(
    () =>
      placed.map((n) => {
        const isFocus = n.id === focusId;
        const selectable = isFocus ? false : !selSet || selSet.has(n.id);
        const facts: GraphNodeFacts = n;
        const data: GraphNodeData = {
          facts, w: n.w,
          selected: n.id === selected,
          cursor: n.id === cursor && n.id !== selected,
          focus: isFocus,
          dimmed: !selectable && !isFocus,
        };
        return {
          id: n.id, type: GRAPH_NODE_TYPE,
          position: { x: n.x, y: n.y },
          data: data as unknown as Record<string, unknown>,
          draggable: false, selectable,
        };
      }),
    // selSet is derived from selectableIds; depend on the stable inputs.
    [placed, focusId, selected, cursor, selectableIds],
  );

  const rfEdges: Edge[] = useMemo(
    () =>
      data.edges.map((e, i) => ({
        id: "e" + i, source: e[0], target: e[1], type: CONFIDENCE_EDGE_TYPE,
        data: { confidence: edgeMeta(e)?.confidence },
      })),
    [data.edges],
  );

  const legend = useMemo(() => confidenceLegend(data.edges), [data.edges]);

  const onNodeClick = useCallback(
    (_e: React.MouseEvent, node: Node) => {
      const selectable = (node as Node & { selectable?: boolean }).selectable;
      if (selectable !== false) onSelect?.(node.id);
    },
    [onSelect],
  );

  return (
    <div data-testid="lineage-graph" style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <div ref={wrapRef} style={{ position: "relative", height, border: "1px solid #2a2b36", borderRadius: 8, background: "#16161e" }}>
        <ReactFlow
          nodes={rfNodes}
          edges={rfEdges}
          nodeTypes={GRAPH_NODE_TYPES}
          edgeTypes={CONFIDENCE_EDGE_TYPES}
          proOptions={{ hideAttribution: true }}
          nodesDraggable={false}
          minZoom={0.08}
          maxZoom={2.4}
          onNodeClick={onNodeClick}
          onPaneClick={onBackground}
        >
          <Background color="#2a2b36" gap={20} />
          <Controls showInteractive={false} />
        </ReactFlow>
        <ZoneOverlay zones={data.zones} byId={byId} selectedZone={selectedZone} onSelectZone={onSelectZone} />
      </div>
      {!!legend.length && (
        <div data-testid="confidence-legend" style={{ display: "flex", flexWrap: "wrap", gap: 14, font: "11px ui-monospace, monospace", color: "var(--text-muted, #6c7086)" }}>
          {legend.map((c) => (
            <span key={c} data-testid="legend-confidence" data-confidence={c} style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
              <span style={{ display: "inline-block", width: 16, borderTop: `2px ${CONFIDENCE[c].dashed ? "dashed" : "solid"} ${CONFIDENCE[c].color}` }} />
              {CONFIDENCE[c].label}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

/** The shared graph engine — wrapped in its own ReactFlowProvider so multiple
 *  DAGs can mount independently on a page (each has its own viewport store). */
export function LineageGraph(props: LineageGraphProps): React.ReactElement {
  return (
    <ReactFlowProvider>
      <LineageGraphInner {...props} />
    </ReactFlowProvider>
  );
}

// ── NavLineage — the focus-but-inert keyboard-cursor variant ──────────────────

export interface NavCtrl {
  navigate: (dir: NavDir) => void;
  commit: () => void;
}

export interface NavLineageProps {
  data: GraphData;
  selectableIds?: string[];
  focusId?: string | null;
  /** receives the navigate()/commit() controller so the host keyboard drives it. */
  ctrlRef?: React.MutableRefObject<NavCtrl | null>;
  onCommit?: (id: string) => void;
  onCursor?: (id: string) => void;
  onReady?: (nodes: PlacedNode[]) => void;
  height?: number;
}

/**
 * NavLineage — wraps LineageGraph with a keyboard cursor over the SELECTABLE
 * nodes and registers navigate()/commit() into ctrlRef so the host keyboard can
 * drive it. `focusId` is the subject node (macro/seed), shown highlighted but
 * never selectable; non-selectable, non-focus nodes are dimmed connectors. The
 * cursor is FOCUS-but-INERT: stepping it never re-pans the canvas (recenter off).
 * (graph.js NavLineage, ported.)
 */
export function NavLineage(props: NavLineageProps): React.ReactElement {
  const { data, selectableIds, focusId, ctrlRef, onCommit, onCursor, onReady, height } = props;
  const selArr = useMemo(() => selectableIds ?? data.nodes.map((n) => n.id), [selectableIds, data.nodes]);
  const [cursor, setCursor] = useState<string | undefined>(() => selArr[0]);
  const nodesRef = useRef<PlacedNode[]>([]);
  const cursorRef = useRef<string | undefined>(cursor);
  cursorRef.current = cursor;

  useEffect(() => { if (cursor) onCursor?.(cursor); }, [cursor, onCursor]);

  const ready = useCallback((ns: PlacedNode[]) => { nodesRef.current = ns; onReady?.(ns); }, [onReady]);

  useEffect(() => {
    if (!ctrlRef) return;
    ctrlRef.current = {
      navigate(dir: NavDir) {
        const sel = nodesRef.current.filter((n) => selArr.includes(n.id));
        const next = nearestInDirection(sel, cursorRef.current ?? selArr[0]!, dir);
        if (next) setCursor(next);
      },
      commit() {
        if (cursorRef.current && selArr.includes(cursorRef.current)) onCommit?.(cursorRef.current);
      },
    };
  });

  return (
    <LineageGraph
      data={data}
      cursor={cursor}
      selectableIds={selectableIds}
      focusId={focusId}
      recenter={false}
      height={height}
      onReady={ready}
      onSelect={(id) => onCommit?.(id)}
    />
  );
}
