// FallbackDiff — the FIRST-PARTY diff renderer (the Pierre escape hatch). When
// @pierre/diffs is unavailable (load error, version skew, a shadow-DOM contract
// break), the DiffViewer degrades to THIS — never a blank surface. GitHub-style
// unified rows: old/new gutters, per-line Shiki highlight + intra-line word
// emphasis (enrichLines → ShikiLine), gutter range-select wired to the same
// pure selection model, and inline CommentThread + Composer mounting.
//
// LAYER: view (imports domain + sibling view).
import React, { useMemo, useState } from "react";
import type { CtxLang } from "../../domain/reshape";
import type { RenderedThread } from "../../domain/context-data";
import type { CodeLang } from "../../domain/code-highlighter";
import { parsePatchHunks, type Hunk, type HunkLine } from "../../domain/diff/patch-hunks";
import { enrichLines, type DiffLine } from "../../domain/diff/inline-emphasis";
import {
  emptySelection,
  gutterClick,
  selectionRange,
  type Selection,
} from "../../domain/diff/selection";
import { ShikiLine } from "./ShikiLine";
import { CommentThread } from "./CommentThread";
import { Composer } from "./Composer";

const SIGN: Record<HunkLine["t"], string> = { add: "+", del: "−", ctx: " " };

function toCodeLang(lang: CtxLang): CodeLang {
  return lang === "yaml" ? "yaml" : "sql";
}

/** Enrich a hunk's lines with inline word-emphasis, preserving gutter numbers. */
function enrichedRows(h: Hunk): (HunkLine & { emph?: [number, number][] })[] {
  const diffLines: DiffLine[] = h.lines.map((l) => ({ t: l.t, s: l.s }));
  const enriched = enrichLines(diffLines);
  return h.lines.map((l, i) => ({ ...l, emph: enriched[i]?.emph }));
}

export function FallbackDiff({
  path,
  patch,
  lang,
  shiki,
  threads,
  reviewers = [],
}: {
  path: string;
  patch: string;
  lang: CtxLang;
  shiki: string;
  threads: RenderedThread[];
  reviewers?: string[];
}): React.ReactElement {
  const hunks = useMemo(() => parsePatchHunks(patch), [patch]);
  const codeLang = toCodeLang(lang);
  const [sel, setSel] = useState<Selection>(emptySelection());
  const range = selectionRange(sel);

  // threads keyed by their new-side line for inline mounting.
  const threadByLine = useMemo(() => {
    const m = new Map<number, RenderedThread>();
    for (const t of threads) if (t.line != null && !t.outdated) m.set(t.line, t);
    return m;
  }, [threads]);

  const inSel = (n: number | null): boolean => n != null && range != null && n >= range.start && n <= range.end;

  return (
    <div data-testid="fallback-diff" data-path={path} className="text-[12px]">
      <div className="mb-2 flex items-center gap-2 font-mono text-[11px] text-zinc-400">
        <span className="inline-flex items-center gap-1 rounded border border-zinc-700 px-1.5 py-0.5">⚠ fallback renderer</span>
        <span>Pierre unavailable — first-party diff (local-first, full fidelity)</span>
      </div>
      {hunks.length === 0 ? (
        <div data-testid="fallback-no-diff" className="p-4 font-mono text-zinc-500">
          no diff — this file is unchanged in the PR (context only).
        </div>
      ) : (
        hunks.map((h, hi) => (
          <div key={hi} className="border-b border-zinc-800 last:border-0">
            <div className="flex h-7 items-center gap-2 bg-zinc-800/60 px-3">
              <span className="font-mono text-[11px] text-sky-400">
                @@ -{h.oldStart} +{h.newStart} @@
              </span>
            </div>
            <table className="w-full border-collapse">
              <tbody className="align-top font-mono">
                {enrichedRows(h).map((r, ri) => {
                  const commentable = r.t !== "del" && r.newNo != null;
                  const line = r.newNo;
                  const thread = line != null ? threadByLine.get(line) : undefined;
                  const showComposer = range != null && line != null && range.end === line;
                  return (
                    <React.Fragment key={ri}>
                      <tr
                        data-testid="fallback-row"
                        data-line={r.newNo ?? undefined}
                        className={r.t === "add" ? "bg-[var(--diff-add-bg)]" : r.t === "del" ? "bg-[var(--diff-del-bg)]" : ""}
                      >
                        <td className="w-12 select-none pr-1 text-right text-[11px] text-zinc-500/70 tabular-nums">{r.oldNo ?? ""}</td>
                        <td
                          className={"group relative w-12 select-none pr-1 text-right text-[11px] tabular-nums " + (inSel(r.newNo) ? "bg-amber-400/40 text-zinc-200" : "text-zinc-500/70")}
                        >
                          {commentable && line != null && (
                            <button
                              data-testid="fallback-gutter-add"
                              title="Comment (shift-click to select a range)"
                              onClick={(e) => setSel((s) => gutterClick(s, line, e.shiftKey))}
                              className="absolute left-0.5 top-0 grid h-4 w-4 place-items-center rounded bg-sky-500 text-[11px] leading-none text-white opacity-0 transition-opacity group-hover:opacity-100"
                            >
                              +
                            </button>
                          )}
                          <span className="pr-1">{r.newNo ?? ""}</span>
                        </td>
                        <td className="w-5 select-none text-center text-zinc-500">{SIGN[r.t]}</td>
                        <td className="whitespace-pre-wrap break-words px-2">
                          <ShikiLine line={r.s} lang={codeLang} shiki={shiki} emph={r.emph} emphCls={r.t === "del" ? "diff-word-del" : "diff-word-add"} />
                        </td>
                      </tr>
                      {(thread || showComposer) && (
                        <tr>
                          <td colSpan={4}>
                            {thread && <CommentThread thread={thread} shiki={shiki} lang={codeLang} reviewers={reviewers} snippet={r.s} />}
                            {showComposer && (
                              <div className="my-1 ml-2 mr-2 rounded-r border-l-2 border-sky-500 bg-sky-500/5">
                                <div className="px-3 pt-2 font-mono text-[11px] text-zinc-400">
                                  Commenting on {range.start === range.end ? `line ${range.start}` : `lines ${range.start}–${range.end}`} · shift-click another line to extend
                                </div>
                                <Composer
                                  placeholder="Leave a comment (markdown · ```suggestion supported)…"
                                  reviewers={reviewers}
                                  suggestionSeed={r.s}
                                  snippet={r.s}
                                  lang={codeLang}
                                  shiki={shiki}
                                  onSubmit={() => setSel(emptySelection())}
                                  onCancel={() => setSel(emptySelection())}
                                />
                              </div>
                            )}
                          </td>
                        </tr>
                      )}
                    </React.Fragment>
                  );
                })}
              </tbody>
            </table>
          </div>
        ))
      )}
    </div>
  );
}
