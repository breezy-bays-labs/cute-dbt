// CommentThread — an inline comment thread mounted into Pierre's per-line
// annotation slot (or the first-party fallback's). Bodies render as SANITIZED
// markdown (CommentBody → react-markdown + rehype-sanitize; never
// dangerouslySetInnerHTML). Suggestion blocks render as first-party diffs.
//
// THE NONCE-COMMAND PATTERN: the host (DiffViewer's keyboard nav) drives reply /
// quote / edit imperatively by BUMPING a nonce prop rather than reaching into
// this component. Each nonce bump runs a one-shot effect that opens the relevant
// composer. This keeps the data-flow render-pure (the host owns focus + nonces;
// the thread owns its compose UI) — the exact prototype discipline.
//
// LAYER: view (imports domain + sibling view).
import React, { useEffect, useState } from "react";
import type { RenderedThread } from "../../domain/context-data";
import type { CodeLang } from "../../domain/code-highlighter";
import { CommentBody } from "./Markdown";
import { Composer } from "./Composer";

export interface CommentThreadProps {
  thread: RenderedThread;
  shiki: string;
  lang?: CodeLang;
  reviewers?: string[];
  currentUser?: string;
  /** the anchored RIGHT-side snippet a suggestion replaces (enables ± Suggest). */
  snippet?: string | null;
  /** the focused comment index within this thread (host-driven), or -1. */
  activeIdx?: number;
  /** bump to (re)open the reply composer. */
  replyNonce?: number;
  /** bump to quote-reply the focused comment. */
  quoteNonce?: number;
  /** { idx, n } — bump n to (re)open the editor on comment `idx`. */
  editTarget?: { idx: number; n: number } | null;
  /** report a rendered comment element to the host (for scroll-into-view). */
  registerRef?: (i: number, el: HTMLElement | null) => void;
  onReply?: (body: string) => void;
  onResolve?: (resolved: boolean) => void;
}

export function CommentThread(props: CommentThreadProps): React.ReactElement {
  const {
    thread,
    shiki,
    lang = "sql",
    reviewers = [],
    snippet,
    activeIdx = -1,
    replyNonce = 0,
    quoteNonce = 0,
    editTarget = null,
    registerRef,
    onReply,
    onResolve,
  } = props;

  const [replying, setReplying] = useState(false);
  const [replyInit, setReplyInit] = useState("");

  // ── nonce-command effects: a bumped nonce opens the relevant composer ──────
  // reply: open an empty reply box.
  useEffect(() => {
    if (replyNonce) {
      setReplyInit("");
      setReplying(true);
    }
  }, [replyNonce]);
  // quote: seed the reply box with the focused comment as a blockquote.
  useEffect(() => {
    if (quoteNonce && activeIdx >= 0 && thread.comments[activeIdx]) {
      const body = String(thread.comments[activeIdx].body ?? "");
      setReplyInit("> " + body.replace(/\n/g, "\n> ") + "\n\n");
      setReplying(true);
    }
  }, [quoteNonce]);
  // jumping away from this thread closes any open composer.
  useEffect(() => {
    if (activeIdx < 0) setReplying(false);
  }, [activeIdx]);

  const sideLabel = thread.side === "Left" ? "old / deletions" : "new / additions";

  if (thread.resolved) {
    return (
      <div
        data-testid="comment-thread"
        data-thread-side={thread.side}
        data-resolved="true"
        className="my-1 ml-8 mr-2 rounded border border-zinc-700 bg-zinc-800/50"
      >
        <div className="flex items-center gap-2 px-3 py-1.5">
          <svg className="h-3.5 w-3.5 text-emerald-400" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
            <path d="M3.5 8.5l3 3 6-7" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
          <span data-testid="thread-resolved-badge" className="font-mono text-[11px] text-zinc-400">
            Conversation resolved · {thread.comments.length} comment{thread.comments.length === 1 ? "" : "s"}
          </span>
          <span className="flex-1" />
          {onResolve && (
            <button data-testid="thread-unresolve" onClick={() => onResolve(false)} className="font-mono text-[11px] text-sky-400 hover:underline">
              Unresolve
            </button>
          )}
        </div>
      </div>
    );
  }

  return (
    <div
      data-testid="comment-thread"
      data-thread-side={thread.side}
      className="my-1 ml-8 mr-2 rounded-r border-l-2 border-sky-500 bg-sky-500/5"
    >
      <div className="flex items-center gap-2 px-3 pt-2">
        <span data-testid="thread-side-badge" className="font-mono text-[11px] text-zinc-400">
          line {thread.line ?? "?"} · {sideLabel}
        </span>
      </div>
      {thread.comments.map((c, i) => (
        <div
          key={i}
          data-testid="thread-comment"
          data-active-comment={activeIdx === i ? String(i) : undefined}
          ref={(el) => registerRef?.(i, el)}
          className={
            "border-b border-sky-500/10 px-3 py-2 last:border-0 " +
            (activeIdx === i ? "rounded ring-2 ring-sky-400 ring-offset-1 ring-offset-zinc-900" : "")
          }
        >
          <div className="mb-1 flex items-center gap-2">
            <span className={"font-mono text-[12px] font-semibold " + (c.author == null ? "text-zinc-500" : "text-zinc-200")}>
              {c.author ?? "ghost"}
            </span>
          </div>
          <div className="pl-1">
            <CommentBody text={c.body} snippet={snippet} lang={lang} shiki={shiki} />
          </div>
        </div>
      ))}

      {replying ? (
        <Composer
          key={"reply" + replyNonce + ":" + quoteNonce}
          initialValue={replyInit}
          placeholder="Reply…"
          submitLabel="Reply"
          reviewers={reviewers}
          suggestionSeed={snippet ?? undefined}
          snippet={snippet}
          lang={lang}
          shiki={shiki}
          onSubmit={(v) => {
            onReply?.(v);
            setReplying(false);
            setReplyInit("");
          }}
          onCancel={() => {
            setReplying(false);
            setReplyInit("");
          }}
        />
      ) : (
        <div className="flex items-center gap-2 px-3 py-1.5">
          <button
            data-testid="thread-reply"
            onClick={() => {
              setReplyInit("");
              setReplying(true);
            }}
            className="font-mono text-[11px] text-sky-400 hover:underline"
          >
            Reply
          </button>
          <span className="flex-1" />
          {onResolve && (
            <button data-testid="thread-resolve" onClick={() => onResolve(true)} className="font-mono text-[11px] text-zinc-400 hover:text-zinc-100">
              Resolve conversation
            </button>
          )}
        </div>
      )}
      {/* editTarget is consumed by the host's edit flow (own-pending drafts);
          the nonce is referenced so a bump is observable in the render tree. */}
      <span hidden data-edit-nonce={editTarget ? editTarget.n : 0} />
    </div>
  );
}
