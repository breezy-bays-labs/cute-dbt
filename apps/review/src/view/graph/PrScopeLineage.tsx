// PrScopeLineage — proves the shared engine on the PR-scope lineage DAG (S4 §5).
//
//   • renders `prDagToScope` output through the shared LineageGraph engine;
//   • the All/Body/Config/Tests single-select 3-axis ToggleGroup drives
//     `pickScopeAxis(byAxis, axis)` → swaps the rendered subgraph;
//   • the prNode-vs-sel.models NAV SPLIT (load-bearing): clicking a PR node sets
//     `prNode` (the UNCONSTRAINED PR cursor) and NEVER touches `sel.models`;
//   • KIND-BASED route-out: clicking a MODEL stays on the PR DAG (sets prNode);
//     clicking a seed/macro node routes OUT via onOpenNode (the engine supports
//     the seed./macro. id prefixes; those nodes are hand-added in the fixture
//     today — T2 #508/A2/A3). The route-out carries the node KIND so the sink
//     lands on the MATCHING entity (seed → Seeds, macro → Macros) — never a
//     non-model id misrouted onto Models. A DELETED node (any kind) keeps the
//     PR cursor (treated as removed — no live destination surface).
//
// LAYER: view (reads the data-layer PrScope folds; never recomputes them).
import React, { useMemo } from "react";
import { scopeToGraph, pickScopeAxis, availableScopeAxes, type PrScope, type ScopeAxis } from "../../domain/data/dataset";
import type { NodeKind } from "../../domain/graph-model";
import { LineageGraph } from "./LineageGraph";

const AXIS_LABEL: Record<ScopeAxis, string> = { all: "All", body: "Body", config: "Config", unit_test: "Tests" };

/** A shared empty fallback so an absent `scope.selectable` keeps a STABLE
 *  reference across renders — a fresh `[]` each render would churn the
 *  LineageGraph memos (re-render + rfNodes recompute). Module-level + never
 *  mutated (LineageGraph only reads it); typed `string[]` to stay assignable to
 *  the `selectableIds?: string[]` prop. */
const EMPTY: string[] = [];

/** The nav-split + kind-route decision (pure → unit-testable):
 *   • a MODEL click STAYS on the PR DAG ("pr-node": sets prNode, never sel.models);
 *   • a DELETED node (any kind) STAYS on the PR DAG (treated as removed — there is
 *     no live destination surface for a node the PR deleted);
 *   • a seed/macro node ROUTES OUT ("open-node") carrying its KIND so the sink
 *     lands on the MATCHING entity — but only when a route-out sink exists; else
 *     it falls back to the PR cursor.
 *  `change` is the PR change-state ("added"/"modified"/"removed"/"deleted"/
 *  "context"); a removed/deleted node never routes out. */
export type PrRoute =
  | { kind: "pr-node"; id: string }
  | { kind: "open-node"; id: string; nodeKind: NodeKind };
export function routePrSelect(
  id: string,
  nodeKind: string | undefined,
  canRouteOut: boolean,
  change?: string,
): PrRoute {
  // a deleted/removed node has no live destination — keep it on the PR cursor.
  if (change === "removed" || change === "deleted") return { kind: "pr-node", id };
  // a model (and the unknown-kind fallback) stays on the PR DAG (the nav split).
  if (nodeKind === "model" || nodeKind === undefined) return { kind: "pr-node", id };
  // a seed/macro routes OUT to its matching entity, carrying the kind.
  if (canRouteOut && (nodeKind === "seed" || nodeKind === "macro"))
    return { kind: "open-node", id, nodeKind };
  return { kind: "pr-node", id };
}

export interface PrScopeLineageProps {
  /** the per-axis PR-scope map (dataset.prScopeByAxis). */
  byAxis: Record<string, PrScope | null>;
  /** the active axis (single-select). */
  axis: ScopeAxis;
  onAxis: (axis: ScopeAxis) => void;
  /** the UNCONSTRAINED PR-lineage cursor (split from sel.models). */
  prNode: string | null;
  /** set the PR cursor — NEVER sel.models (the nav split). */
  onPrNode: (id: string | null) => void;
  /** route OUT to a seed/macro node's own entity — carries the KIND so the sink
   *  lands on the MATCHING entity (seed → Seeds, macro → Macros), never Models. */
  onOpenNode?: (id: string, nodeKind: NodeKind) => void;
  height?: number;
}

export function PrScopeLineage(props: PrScopeLineageProps): React.ReactElement {
  const { byAxis, axis, onAxis, prNode, onPrNode, onOpenNode, height = 420 } = props;
  const axes = useMemo(() => availableScopeAxes(byAxis), [byAxis]);
  const scope = pickScopeAxis(byAxis, axis);
  const graph = useMemo(() => scopeToGraph(scope?.data ?? null), [scope]);
  // Memoize so the fallback (`?? EMPTY`) keeps a STABLE reference across renders
  // — a fresh `[]` each render would re-render LineageGraph + recompute rfNodes.
  const selectable = useMemo(() => scope?.selectable ?? EMPTY, [scope]);

  // KIND-BASED route-out: a model click stays (sets prNode); a seed/macro node
  // routes out (onOpenNode) to its MATCHING entity; a deleted node stays. Both
  // the node kind and change-state are carried on the graph node.
  const factsById = useMemo(() => {
    const m: Record<string, { kind?: string; change?: string }> = Object.create(null) as Record<
      string,
      { kind?: string; change?: string }
    >;
    graph.nodes.forEach((n) => { m[n.id] = { kind: n.kind, change: n.change }; });
    return m;
  }, [graph]);

  const onSelect = (id: string): void => {
    const facts = factsById[id];
    const route = routePrSelect(id, facts?.kind, !!onOpenNode, facts?.change);
    // seed/macro → route OUT to the matching entity (carries the kind).
    // model / deleted / unknown → stay on the PR DAG (prNode, NOT sel.models).
    if (route.kind === "open-node" && onOpenNode) onOpenNode(route.id, route.nodeKind);
    else onPrNode(route.id);
  };

  return (
    <div data-testid="pr-scope-lineage" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <div data-testid="axis-toggle" role="group" aria-label="change-axis filter"
        style={{ display: "inline-flex", gap: 0, alignSelf: "flex-start", borderRadius: 8, overflow: "hidden", border: "1px solid #2a2b36" }}>
        {axes.map((a) => (
          <button
            key={a}
            data-testid="axis-option"
            data-axis={a}
            data-active={a === axis ? "true" : "false"}
            aria-pressed={a === axis}
            onClick={() => onAxis(a)}
            style={{
              padding: "5px 12px", font: "12px system-ui, sans-serif", border: "none", cursor: "pointer",
              background: a === axis ? "var(--accent, #58a6ff)" : "transparent",
              color: a === axis ? "#0b0b10" : "var(--text-muted, #6c7086)",
            }}
          >
            {AXIS_LABEL[a]}
          </button>
        ))}
      </div>
      <LineageGraph
        data={graph}
        selected={prNode}
        selectableIds={selectable}
        recenter={false}
        height={height}
        onSelect={onSelect}
        onBackground={() => onPrNode(null)}
      />
    </div>
  );
}
