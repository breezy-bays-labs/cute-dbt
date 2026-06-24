// ModelReviewSurface — the Models code/diff REVIEWABLE surface (V1 / cute-dbt#495).
// This is the thin vertical that makes Models reviewable END-TO-END (council
// MUST-FIX D — "build the VERB"): the changed-file diff(s) + a keyboard-reachable
// draft composer + the REAL reviewed-state chip + a mark-reviewed affordance.
//
// The draft composer's submit produces a pending review draft anchored to the
// file's first change-run line (a real anchor from the patch — the line you'd
// comment on). The host (App) threads the draft into the review slice
// (`addReviewDraft`) and the mark-reviewed verb into `markReviewedAdvance`. The
// reviewed chip + draft count read the REAL store state — never fabricated.
//
// LAYER: view (imports domain + sibling view).
import React from "react";
import type { ReviewContext, CtxFile } from "../../domain/reshape";
import { parsePatchNav } from "../../domain/diff/patch-nav";
import { DiffViewer } from "../DiffViewer";
import { Composer } from "../diff/Composer";

export interface ModelReviewSurfaceProps {
  /** the active model name (the reviewable instance). */
  model: string;
  /** the review context — the changed files for this model. */
  ctx: ReviewContext;
  shiki: string;
  reviewers?: string[];
  /** the REAL reviewed state for this model (from the review slice). */
  reviewed: boolean;
  /** the REAL pending-draft count for this model (from the review slice). */
  draftCount: number;
  /** add a pending draft (path/line/side/body) — wired to the review slice. */
  onDraft: (draft: { path: string; line: number; side: "old" | "new"; body: string }) => void;
  /** mark this model reviewed + advance (the `x` verb) — wired to the slice. */
  onMarkReviewed: () => void;
  /** force the first-party fallback diff (Storybook / settings / tests). */
  forceFallback?: boolean;
}

/** The first change-run line of a file's patch — the natural comment anchor. */
function anchorOf(file: CtxFile): { line: number; side: "old" | "new" } {
  const nav = parsePatchNav(file.patch);
  const first = nav.starts[0];
  if (!first) return { line: 1, side: "new" };
  return { line: first.no, side: first.side === "deletions" ? "old" : "new" };
}

export function ModelReviewSurface(props: ModelReviewSurfaceProps): React.ReactElement {
  const { model, ctx, shiki, reviewers = [], reviewed, draftCount, onDraft, onMarkReviewed, forceFallback } = props;
  const primary = ctx.files[0];

  return (
    <div
      data-testid="model-review-surface"
      data-model={model}
      data-draft-count={draftCount}
      className="min-w-0 flex-1 space-y-5 overflow-auto p-6"
    >
      {/* the REAL reviewed-state chip + mark-reviewed affordance */}
      <div className="flex items-center gap-3">
        <h2 className="text-sm font-semibold text-zinc-200" data-testid="review-heading">
          {model}
          <span className="ml-2 font-normal text-zinc-500">({ctx.files.length} changed file{ctx.files.length === 1 ? "" : "s"})</span>
        </h2>
        <span
          data-testid="review-state-chip"
          data-reviewed={reviewed}
          className={
            "rounded px-2 py-0.5 font-mono text-[11px] " +
            (reviewed ? "bg-emerald-500/15 text-emerald-300" : "bg-zinc-800 text-zinc-400")
          }
        >
          {reviewed ? "✓ reviewed" : "unreviewed"}
        </span>
        <span className="flex-1" />
        <button
          data-testid="mark-reviewed-btn"
          onClick={onMarkReviewed}
          className="rounded-md border border-zinc-700 px-2.5 py-1 text-[12px] font-medium text-zinc-200 hover:bg-zinc-800"
          title="Mark reviewed and advance (x)"
        >
          <kbd className="rounded border border-zinc-600 bg-zinc-800 px-1 text-[10px]">x</kbd> Mark reviewed
        </button>
      </div>

      {/* the changed-file diff(s) */}
      <section data-testid="review-diff-section" className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
        <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">Diff (Pierre · first-party fallback)</div>
        {ctx.files.length > 0 ? (
          ctx.files.map((f) => (
            <DiffViewer key={f.path} file={f} shiki={shiki} reviewers={reviewers} forceFallback={forceFallback} />
          ))
        ) : (
          <p data-testid="review-no-files" className="text-sm text-zinc-500">
            No changed files for this model.
          </p>
        )}
      </section>

      {/* the keyboard-reachable draft composer (a comment becomes a pending draft) */}
      {primary && (
        <section data-testid="review-draft-composer" className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-2">
          <div className="px-2 pt-1 text-xs uppercase tracking-wide text-zinc-500">
            Leave a review comment ({draftCount} draft{draftCount === 1 ? "" : "s"} on {model})
          </div>
          <Composer
            shiki={shiki}
            reviewers={reviewers}
            lang={primary.lang === "yaml" ? "yaml" : "sql"}
            submitLabel="Add review comment"
            placeholder={`Comment on ${primary.path}…`}
            autoFocus={false}
            onSubmit={(body) => {
              const a = anchorOf(primary);
              onDraft({ path: primary.path, line: a.line, side: a.side, body });
              // blur the composer after submit so the keyboard review LOOP resumes
              // (the input-guard suppresses flow keys while the textarea is focused;
              // a draft-then-`x` flow must hand the keyboard back to the dispatcher).
              if (typeof document !== "undefined") {
                (document.activeElement as HTMLElement | null)?.blur?.();
              }
            }}
          />
        </section>
      )}
    </div>
  );
}
