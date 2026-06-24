// The ViewRouter (view layer) — the typed `renderView` dispatcher: a
// discriminated switch over `routeTarget(entity, view)` (src/domain/matrix.ts).
// S2 proves ROUTING, not the real surfaces — most routes render an honest
// placeholder body. The Models routes still mount the S0 walking-skeleton
// surfaces (Pierre DiffViewer + React Flow LineageGraph + Shiki CodePane) so the
// local-first behavioral gate keeps asserting the full stack renders.
//
// LAYER: view (may import domain + data; never chrome).
import React from "react";
import { routeTarget, type View } from "../domain/matrix";
import type { Entity } from "../domain/keymap";
import type { ReviewContext } from "../domain/reshape";
import type { ModelPayload } from "../domain/context-data";
import type { PrScope, ScopeAxis } from "../domain/data/dataset";
import type { NodeKind } from "../domain/graph-model";
import { DiffViewer } from "./DiffViewer";
import { LineageGraph } from "./LineageGraph";
import { CodePane } from "./CodePane";
import { PrScopeLineage } from "./graph/PrScopeLineage";

export interface ViewRouterProps {
  entity: Entity;
  view: View;
  /** the active model payload (for the Models surfaces). */
  model?: ModelPayload;
  /** the review context (changed files) for the active model. */
  ctx?: ReviewContext;
  /** the compiled SQL string for the Shiki pane. */
  compiledSql: string;
  shiki: string;
  /** the PR reviewers (for the comment composer's @-mention picker). */
  reviewers?: string[];
  /** the active instance id (for placeholder bodies). */
  sel: string | null;
  /** the per-axis PR-scope map (dataset.prScopeByAxis) — the PR Topology DAG. */
  prScopeByAxis?: Record<string, PrScope | null>;
  /** the active change-axis (single-select). */
  scopeAxis?: ScopeAxis;
  onScopeAxis?: (axis: ScopeAxis) => void;
  /** the UNCONSTRAINED PR-lineage cursor (split from sel.models). */
  prNode?: string | null;
  onPrNode?: (id: string | null) => void;
  /** route OUT to a seed/macro node's own entity — carries the KIND so the sink
   *  lands on the MATCHING entity (seed → Seeds, macro → Macros). */
  onOpenNode?: (id: string, nodeKind: NodeKind) => void;
}

/** An honest "this surface lands in a later slice" placeholder body. */
function Placeholder({ label, detail }: { label: string; detail?: string }): React.ReactElement {
  return (
    <div data-testid="view-placeholder" data-surface={label} className="p-6 text-sm text-zinc-500">
      <div className="font-semibold text-zinc-400">{label}</div>
      <p className="mt-1">{detail ?? "Surface lands in a later slice — S2 proves routing, keyboard, and persistence."}</p>
    </div>
  );
}

export function ViewRouter(p: ViewRouterProps): React.ReactElement {
  const target = routeTarget(p.entity, p.view);

  switch (target.kind) {
    case "models-topology":
      // The S0 walking-skeleton surfaces — kept live so the local-first gate
      // keeps asserting Pierre + React Flow + Shiki render end-to-end.
      return (
        <div data-testid="view-models-topology" className="min-w-0 flex-1 space-y-5 overflow-auto p-6">
          <h2 className="text-sm font-semibold" data-testid="model-heading">
            {p.model?.name}
            <span className="ml-2 font-normal text-zinc-500">({p.ctx?.files.length ?? 0} changed files)</span>
          </h2>
          <section data-testid="diff-section" className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
            <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">Diff (Pierre · first-party fallback)</div>
            {p.ctx && p.ctx.files.length > 0 ? (
              p.ctx.files.map((f) => <DiffViewer key={f.path} file={f} shiki={p.shiki} reviewers={p.reviewers} />)
            ) : (
              <p data-testid="no-diff" className="text-sm text-zinc-500">
                No changed files for this model.
              </p>
            )}
          </section>
          {p.model?.dag && (
            <section data-testid="lineage-section" className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
              <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">
                CTE lineage (React Flow + elkjs worker)
              </div>
              <LineageGraph dag={p.model.dag} />
            </section>
          )}
          {p.compiledSql && (
            <section data-testid="code-section" className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
              <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">Compiled SQL (Shiki)</div>
              <CodePane code={p.compiledSql} lang="sql" shiki={p.shiki} />
            </section>
          )}
        </div>
      );
    case "models-node":
      return <Placeholder label="Models · Details" detail={`Node details for ${p.sel ?? "—"}`} />;
    case "models-data":
      return <Placeholder label="Models · Unit tests" detail={`Unit-test data for ${p.sel ?? "—"}`} />;
    case "models-code":
      return <Placeholder label="Models · Code" detail={`Code diff for ${p.sel ?? "—"} (reachable, off-tab)`} />;
    case "pr-overview":
      return <Placeholder label="PR · Overview" />;
    case "pr-lineage":
      return p.prScopeByAxis ? (
        <div data-testid="view-pr-lineage" className="min-w-0 flex-1 space-y-4 overflow-auto p-6">
          <h2 className="text-sm font-semibold text-zinc-300">PR-scope lineage</h2>
          <PrScopeLineage
            byAxis={p.prScopeByAxis}
            axis={p.scopeAxis ?? "all"}
            onAxis={(a) => p.onScopeAxis?.(a)}
            prNode={p.prNode ?? null}
            onPrNode={(id) => p.onPrNode?.(id)}
            onOpenNode={p.onOpenNode}
          />
        </div>
      ) : (
        <Placeholder label="PR · Topology" detail="No PR-scope DAG in this context." />
      );
    case "pr-files":
      return <Placeholder label="PR · Files" />;
    case "pr-timeline":
      return <Placeholder label="PR · Timeline" />;
    case "entity-review":
      return <Placeholder label={`${target.entity} · Review`} detail={`Review pane for ${p.sel ?? "—"}`} />;
    case "not-available":
      return (
        <Placeholder
          label="View not available"
          detail={`${target.entity} has no “${target.view}” view.`}
        />
      );
  }
}
