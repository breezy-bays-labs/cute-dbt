// PrScopeLineage — proves the shared engine on the PR-scope lineage DAG (S4 §5).
//
//   • renders `prDagToScope` output through the shared LineageGraph engine;
//   • the All/Body/Config/Tests single-select 3-axis ToggleGroup drives
//     `pickScopeAxis(byAxis, axis)` → swaps the rendered subgraph;
//   • the prNode-vs-sel.models NAV SPLIT (load-bearing): clicking a PR node sets
//     `prNode` (the UNCONSTRAINED PR cursor) and NEVER touches `sel.models`;
//   • KIND-BASED route-out: clicking a MODEL stays on the PR DAG (sets prNode);
//     clicking a seed/macro/deleted node routes OUT via onOpenModel (the engine
//     supports the seed./macro. id prefixes; those nodes are hand-added in the
//     fixture today — T2 #508/A2/A3).
//
// LAYER: view (reads the data-layer PrScope folds; never recomputes them).
import React, { useMemo } from "react";
import { scopeToGraph, pickScopeAxis, availableScopeAxes, type PrScope, type ScopeAxis } from "../../domain/data/dataset";
import { LineageGraph } from "./LineageGraph";

const AXIS_LABEL: Record<ScopeAxis, string> = { all: "All", body: "Body", config: "Config", unit_test: "Tests" };

/** A shared empty fallback so an absent `scope.selectable` keeps a STABLE
 *  reference across renders — a fresh `[]` each render would churn the
 *  LineageGraph memos (re-render + rfNodes recompute). Module-level + never
 *  mutated (LineageGraph only reads it); typed `string[]` to stay assignable to
 *  the `selectableIds?: string[]` prop. */
const EMPTY: string[] = [];

/** The nav-split + kind-route decision (pure → unit-testable): a model click
 *  STAYS on the PR DAG ("pr-node": sets prNode, never sel.models); a seed/macro/
 *  deleted node ROUTES OUT ("open-model") when an onOpenModel sink exists, else
 *  it falls back to the PR cursor. */
export type PrRoute = { kind: "pr-node"; id: string } | { kind: "open-model"; id: string };
export function routePrSelect(id: string, nodeKind: string | undefined, canRouteOut: boolean): PrRoute {
  if (nodeKind === "model" || nodeKind === undefined) return { kind: "pr-node", id };
  return canRouteOut ? { kind: "open-model", id } : { kind: "pr-node", id };
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
  /** route OUT to a seed/macro/deleted node's own entity. */
  onOpenModel?: (id: string) => void;
  height?: number;
}

export function PrScopeLineage(props: PrScopeLineageProps): React.ReactElement {
  const { byAxis, axis, onAxis, prNode, onPrNode, onOpenModel, height = 420 } = props;
  const axes = useMemo(() => availableScopeAxes(byAxis), [byAxis]);
  const scope = pickScopeAxis(byAxis, axis);
  const graph = useMemo(() => scopeToGraph(scope?.data ?? null), [scope]);
  // Memoize so the fallback (`?? EMPTY`) keeps a STABLE reference across renders
  // — a fresh `[]` each render would re-render LineageGraph + recompute rfNodes.
  const selectable = useMemo(() => scope?.selectable ?? EMPTY, [scope]);

  // KIND-BASED route-out: a model click stays (sets prNode); a seed/macro/deleted
  // node routes out (onOpenModel). The node's kind is carried on the graph node.
  const kindById = useMemo(() => {
    const m: Record<string, string> = Object.create(null) as Record<string, string>;
    graph.nodes.forEach((n) => { if (n.kind) m[n.id] = n.kind; });
    return m;
  }, [graph]);

  const onSelect = (id: string): void => {
    const route = routePrSelect(id, kindById[id], !!onOpenModel);
    // model → stay on the PR DAG (the nav split: prNode, NOT sel.models).
    // seed/macro (and the deleted/hand-added prefixes) → route OUT.
    if (route.kind === "open-model" && onOpenModel) onOpenModel(route.id);
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
