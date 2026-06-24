// DiffViewer — the load-bearing reviewer surface. Renders ONE changed file's
// diff through @pierre/diffs (PRIMARY), with a FIRST-PARTY fallback renderer
// (FallbackDiff) behind an error boundary: a Pierre breakage (load error, theme
// preload reject, shadow-DOM contract break, version skew) degrades to the
// first-party diff — NEVER a blank surface. Both paths are local-first (vendored
// Shiki, zero egress) and mount live CommentThreads on the correct side.
//
// The selection model, nav-by-data-line, ring-flash-retry, fold-fallback, and
// openAnchor nonce live in the engine components (PierreDiff / FallbackDiff),
// each a thin projection of the pure domain reducers (domain/diff/*).
//
// LAYER: view (imports domain + sibling view).
import React from "react";
import type { CtxFile } from "../domain/reshape";
import { PierreDiff, type OpenAnchor, type PierreNavApi } from "./diff/PierreDiff";
import { FallbackDiff } from "./diff/FallbackDiff";

/** Error boundary: any throw from the Pierre subtree → the first-party fallback. */
class PierreBoundary extends React.Component<
  { fallback: React.ReactNode; children: React.ReactNode },
  { failed: boolean }
> {
  constructor(props: { fallback: React.ReactNode; children: React.ReactNode }) {
    super(props);
    this.state = { failed: false };
  }
  static getDerivedStateFromError(): { failed: boolean } {
    return { failed: true };
  }
  componentDidCatch(error: unknown): void {
    // local-first: log to the console only (no telemetry, no network).
    console.warn("[DiffViewer] Pierre failed; using the first-party fallback renderer.", error);
  }
  render(): React.ReactNode {
    if (this.state.failed) return this.props.fallback;
    return this.props.children;
  }
}

export interface DiffViewerProps {
  file: CtxFile;
  shiki: string;
  reviewers?: string[];
  openAnchor?: OpenAnchor | null;
  navRef?: React.Ref<PierreNavApi>;
  /** force the first-party path (Storybook / a settings "engine: fallback"). */
  forceFallback?: boolean;
}

export function DiffViewer({ file, shiki, reviewers = [], openAnchor, navRef, forceFallback }: DiffViewerProps): React.ReactElement {
  const fallback = <FallbackDiff path={file.path} patch={file.patch} lang={file.lang} shiki={shiki} threads={file.threads} reviewers={reviewers} />;

  return (
    <section data-testid="review-file" data-path={file.path} className="mb-7">
      <div className="mb-1 font-mono text-[12px] text-sky-400">
        {file.path} <span className="text-zinc-500">· {file.lang}</span>
      </div>
      {forceFallback ? (
        fallback
      ) : (
        <PierreBoundary fallback={fallback}>
          <PierreDiff file={file} shiki={shiki} reviewers={reviewers} openAnchor={openAnchor} navRef={navRef} />
        </PierreBoundary>
      )}
    </section>
  );
}
