// The Pierre @pierre/diffs DiffViewer wrapper — one PatchDiff per changed file,
// with live comment threads mounted as line annotations.
//
// CONTRACT TRAPS (load-bearing, from the harness):
//   - `lineAnnotations` (NOT `annotations`).
//   - `disableWorkerPool` (top-level) — wasm-free + worker-free.
//   - theme passed BY NAME in `options.theme` (preloaded before render).
//   - LEFT→deletions / RIGHT→additions via pierreSide.
import React from "react";
import { PatchDiff } from "@pierre/diffs/react";
import type { RenderedThread } from "../domain/context-data";
import { pierreSide, isLiveThread, type CtxFile } from "../domain/reshape";
import { CommentThread } from "./CommentThread";

export function DiffViewer({ file, shiki }: { file: CtxFile; shiki: string }): React.ReactElement {
  const liveThreads = file.threads.filter(isLiveThread);
  const foldedThreads = file.threads.filter((t) => t.outdated || t.line == null);

  const lineAnnotations = liveThreads.map((t) => ({
    // a live thread without an explicit side anchors to the new (added) side.
    side: pierreSide(t.side ?? "Right"),
    lineNumber: t.line as number,
    metadata: t,
  }));

  return (
    <section data-testid="review-file" data-path={file.path} style={{ marginBottom: 28 }}>
      <div style={{ font: "12px ui-monospace, monospace", opacity: 0.85, marginBottom: 4, color: "#7aa2f7" }}>
        {file.path} <span style={{ opacity: 0.6 }}>· {file.lang}</span>
      </div>
      <div data-testid="diff-host">
        <PatchDiff<RenderedThread>
          patch={file.patch}
          options={{ theme: shiki, preferredHighlighter: "shiki-js" }}
          disableWorkerPool
          lineAnnotations={lineAnnotations}
          renderAnnotation={(a) => <CommentThread thread={a.metadata} />}
        />
      </div>
      {foldedThreads.length > 0 && (
        <div data-testid="folded-threads" style={{ marginTop: 8, font: "12px system-ui", opacity: 0.85 }}>
          {foldedThreads.map((t, i) => (
            <div key={i} data-testid="folded-thread" style={{ padding: "2px 0" }}>
              was on line {t.original_line ?? t.line ?? "?"}: {t.comments[0]?.body ?? "(no body)"}
              {t.resolved ? " (resolved)" : ""}
              {t.outdated ? " (outdated)" : ""}
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
