// PierreDiff — the PRIMARY diff surface, @pierre/diffs (Shiki highlighting,
// Shadow DOM, unified/split). The proven local-first recipe (the harness +
// domain/highlighter): register + PRELOAD the Shiki theme BY NAME before paint,
// pass `theme` inside `options`, mount comment threads on the correct
// deletions/additions annotation side, `disableWorkerPool` (wasm-free +
// worker-free). Throws on a load/preload error so the DiffViewer error boundary
// degrades to the first-party FallbackDiff (a Pierre breakage never blanks).
//
// NAV binds the DATA-LINE NUMBER (Pierre virtualizes rows, so array indices are
// unstable). openAnchor (a nonce) drives ring-flash-retry-until-mounted with a
// fold-fallback (when the exact line isn't rendered, land on the first visible
// line of the selected span).
//
// LAYER: view (imports domain + sibling view).
import React, { useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { PatchDiff } from "@pierre/diffs/react";
import { ensureHighlighter } from "../../domain/highlighter";
import { pierreSide, isLiveThread, type CtxFile } from "../../domain/reshape";
import { parsePatchNav, type NavStart } from "../../domain/diff/patch-nav";
import type { RenderedThread } from "../../domain/context-data";
import { CommentThread } from "./CommentThread";

export interface PierreNavApi {
  /**
   * jump to the next/prev change-run by data-line number. Until the keyboard
   * slice (S6) wires the running cursor, these land on the FIRST / LAST anchor
   * respectively (they expose the ordered anchors + a stateless jump, not a
   * stepping cursor — see jump()).
   */
  nextHunk: () => void;
  prevHunk: () => void;
  /** the ordered change-run anchors (data-line numbers, never array indices). */
  starts: NavStart[];
}

export interface OpenAnchor {
  line: number;
  /** bump to (re)trigger the ring-flash even on the same line. */
  nonce: number;
  /** the selected span — the fold-fallback lands on its first VISIBLE line. */
  span?: { start: number; end: number };
}

/** annotation metadata carries the full thread for the inline mount. */
interface AnnoMeta {
  thread: RenderedThread;
}

export interface PierreDiffProps {
  file: CtxFile;
  shiki: string;
  reviewers?: string[];
  /** a host request to scroll-to + ring-flash a line (retry-until-mounted). */
  openAnchor?: OpenAnchor | null;
  navRef?: React.Ref<PierreNavApi>;
}

export function PierreDiff({ file, shiki, reviewers = [], openAnchor, navRef }: PierreDiffProps): React.ReactElement {
  const [ready, setReady] = useState(false);
  const [err, setErr] = useState<Error | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const flashRef = useRef<{ el: Element | null; t: ReturnType<typeof setTimeout> | null }>({ el: null, t: null });

  const liveThreads = useMemo(() => file.threads.filter(isLiveThread), [file.threads]);
  const foldedThreads = useMemo(() => file.threads.filter((t) => t.outdated || t.line == null), [file.threads]);
  const nav = useMemo(() => parsePatchNav(file.patch), [file.patch]);

  // PRELOAD the theme BY NAME before paint — the half of the recipe that fixes
  // the silent github-dark fallback. Throws (sets err) on an unregistered theme.
  useEffect(() => {
    let on = true;
    setReady(false);
    setErr(null);
    ensureHighlighter([shiki])
      .then(() => {
        if (on) setReady(true);
      })
      .catch((e: unknown) => {
        if (on) setErr(e instanceof Error ? e : new Error(String(e)));
      });
    return () => {
      on = false;
    };
  }, [shiki]);

  // imperative nav (the keyboard layer drives these by DATA-LINE NUMBER).
  useImperativeHandle(
    navRef,
    (): PierreNavApi => {
      const lineEls = (): HTMLElement[] => {
        const host = rootRef.current?.querySelector("diffs-container");
        const sr = (host as Element & { shadowRoot?: ShadowRoot } | null)?.shadowRoot;
        if (!sr) return [];
        return [...sr.querySelectorAll<HTMLElement>("[data-line]")];
      };
      const scrollToLine = (no: number): void => {
        const el = lineEls().find((e) => Number(e.getAttribute("data-line")) === no);
        el?.scrollIntoView({ block: "center", behavior: "smooth" });
      };
      const jump = (dir: 1 | -1): void => {
        const s = nav.starts;
        if (!s.length) return;
        // the cursor is the first start past/behind the currently centered line;
        // without DOM state we step from the ends (the keyboard slice owns the
        // running cursor in S6 — here we expose the ordered anchors + jump).
        const target = dir > 0 ? s[0] : s[s.length - 1];
        if (target) scrollToLine(target.no);
      };
      return {
        nextHunk: () => jump(1),
        prevHunk: () => jump(-1),
        starts: nav.starts,
      };
    },
    [nav.starts],
  );

  // openAnchor: scroll-to + ring-flash, RETRYING until the row mounts (Shiki
  // highlights async + Pierre virtualizes). Fold-fallback: if the exact line is
  // not rendered, land on the first VISIBLE line of the selected span.
  useEffect(() => {
    if (!openAnchor || openAnchor.line == null) return;
    let done = false;
    // the currently-scheduled (re)try timeout — tracked so a pending RETRY (not
    // just the initial schedule) is cleared on unmount and never fires late.
    let pending: ReturnType<typeof setTimeout> | null = null;
    const t0 = Date.now();
    const findRow = (): HTMLElement | null => {
      const host = rootRef.current?.querySelector("diffs-container");
      const sr = (host as Element & { shadowRoot?: ShadowRoot } | null)?.shadowRoot;
      if (!sr) return null;
      const exact = sr.querySelector<HTMLElement>(`[data-line="${openAnchor.line}"]`);
      if (exact) return exact;
      // fold-fallback: the exact line is folded/out-of-range — use the span's
      // first rendered line so the reviewer still lands inside the block.
      if (openAnchor.span) {
        for (let L = openAnchor.span.start; L <= openAnchor.span.end; L++) {
          const row = sr.querySelector<HTMLElement>(`[data-line="${L}"]`);
          if (row) return row;
        }
      }
      return null;
    };
    const tryScroll = (): void => {
      if (done) return;
      const row = findRow();
      if (row) {
        done = true;
        row.scrollIntoView({ block: "center", behavior: "smooth" });
        if (flashRef.current.el) {
          flashRef.current.el.classList.remove("kbd-ring");
          if (flashRef.current.t) clearTimeout(flashRef.current.t);
        }
        row.classList.add("kbd-ring");
        flashRef.current = { el: row, t: setTimeout(() => row.classList.remove("kbd-ring"), 1500) };
        return;
      }
      if (Date.now() - t0 < 3500) pending = setTimeout(tryScroll, 80); // retry-until-mounted
    };
    pending = setTimeout(tryScroll, 60);
    return () => {
      done = true;
      if (pending) clearTimeout(pending);
    };
  }, [openAnchor?.nonce, openAnchor?.line]);

  if (err) {
    // throw so the DiffViewer error boundary catches → FallbackDiff. (A render
    // throw is the cleanest signal; the boundary owns the degrade.)
    throw err;
  }
  if (!ready) {
    return (
      <div data-testid="pierre-loading" className="p-6 font-mono text-[12px] text-zinc-500">
        loading @pierre/diffs · {shiki}…
      </div>
    );
  }

  const lineAnnotations = liveThreads.map((t) => ({
    // a live thread without an explicit side anchors to the new (added) side.
    side: pierreSide(t.side ?? "Right"),
    lineNumber: t.line as number,
    metadata: { thread: t } as AnnoMeta,
  }));

  return (
    <div ref={rootRef} data-testid="pierre-diff">
      <div className="mb-2 flex items-center gap-2 font-mono text-[11px] text-zinc-400">
        <span className="inline-flex items-center gap-1 rounded border border-zinc-700 px-1.5 py-0.5">⚗ Pierre engine</span>
        <span>
          @pierre/diffs · Shiki «{shiki}» · {file.path} · {file.lang}
        </span>
      </div>
      <div data-testid="diff-host">
        <PatchDiff<AnnoMeta>
          patch={file.patch}
          options={{ theme: shiki, preferredHighlighter: "shiki-js" }}
          disableWorkerPool
          lineAnnotations={lineAnnotations}
          renderAnnotation={(a) => (
            <CommentThread thread={a.metadata.thread} shiki={shiki} lang={file.lang === "yaml" ? "yaml" : "sql"} reviewers={reviewers} />
          )}
        />
      </div>
      {foldedThreads.length > 0 && (
        <div data-testid="folded-threads" className="mt-2 font-mono text-[12px] text-zinc-400">
          {foldedThreads.map((t, i) => (
            <div key={i} data-testid="folded-thread" className="py-0.5">
              was on line {t.original_line ?? t.line ?? "?"}: {t.comments[0]?.body ?? "(no body)"}
              {t.resolved ? " (resolved)" : ""}
              {t.outdated ? " (outdated)" : ""}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
