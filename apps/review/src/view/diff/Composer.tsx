// Composer — the GitHub-style comment composer. Write/Preview tabs, a markdown
// formatting toolbar (bold · italic · quote · code · link · lists · mention ·
// suggestion), an @-mention picker sourced from the review participants, and a
// triple-backtick ```suggestion insert seeded from the anchored snippet. All
// output is plain GitHub-flavoured markdown so it round-trips to the PR
// verbatim. The TEXT TRANSFORMS are pure (domain/diff/composer-transforms);
// this component just applies them to the textarea + writes the result back.
//
// LAYER: view (imports domain only).
import React, { useMemo, useRef, useState } from "react";
import type { CodeLang } from "../../domain/code-highlighter";
import {
  wrapSelection,
  prefixLines,
  insertLink,
  insertSuggestion,
  matchMention,
  applyMention,
  type TextSel,
  type Transform,
  type MentionMatch,
} from "../../domain/diff/composer-transforms";
import { CommentBody } from "./Markdown";

export interface ComposerProps {
  placeholder?: string;
  initialValue?: string;
  submitLabel?: string;
  reviewers?: string[];
  /** the anchored RIGHT-side snippet a suggestion replaces (enables ± Suggest). */
  suggestionSeed?: string | null;
  snippet?: string | null;
  lang?: CodeLang;
  shiki: string;
  autoFocus?: boolean;
  onSubmit: (value: string) => void;
  onCancel?: () => void;
  onDelete?: () => void;
}

export function Composer(props: ComposerProps): React.ReactElement {
  const {
    placeholder = "Leave a comment",
    initialValue = "",
    submitLabel = "Comment",
    reviewers = [],
    suggestionSeed,
    snippet,
    lang = "sql",
    shiki,
    autoFocus = true,
    onSubmit,
    onCancel,
    onDelete,
  } = props;

  const [txt, setTxt] = useState(initialValue);
  const [tab, setTab] = useState<"write" | "preview">("write");
  const [mention, setMention] = useState<MentionMatch | null>(null);
  const ref = useRef<HTMLTextAreaElement>(null);

  function submit(): void {
    const v = txt.trim();
    if (v) {
      onSubmit(v);
      setTxt("");
    }
  }

  /** Read the textarea selection, run a pure transform, write the result back. */
  function apply(fn: Transform): void {
    const el = ref.current;
    if (!el) return;
    const sel: TextSel = { text: txt, selStart: el.selectionStart, selEnd: el.selectionEnd };
    const r = fn(sel);
    setTxt(r.text);
    requestAnimationFrame(() => {
      if (el) {
        el.focus();
        el.setSelectionRange(r.selStart, r.selEnd);
      }
    });
  }

  const doSuggestion = (): void => apply((s) => insertSuggestion(s, suggestionSeed));

  // ── @-mention parsing on each keystroke ──────────────────────────────────
  function onInput(e: React.FormEvent<HTMLTextAreaElement>): void {
    const el = e.currentTarget;
    setTxt(el.value);
    setMention(matchMention(el.value, el.selectionStart));
  }
  const mentionHits = useMemo(() => {
    if (!mention) return [];
    return reviewers.filter((r) => r.toLowerCase().includes(mention.query)).slice(0, 6);
  }, [mention, reviewers]);
  function pickMention(name: string): void {
    const el = ref.current;
    if (!el || !mention) return;
    const r = applyMention(txt, el.selectionStart, mention, name);
    setTxt(r.text);
    setMention(null);
    requestAnimationFrame(() => {
      if (el) {
        el.focus();
        el.setSelectionRange(r.selStart, r.selEnd);
      }
    });
  }

  const tbBtn = "h-7 w-7 grid place-items-center rounded text-zinc-400 hover:text-zinc-100 hover:bg-zinc-800 text-[13px] font-mono";

  return (
    <div data-testid="composer" className="px-3 py-2">
      <div className="overflow-hidden rounded-md border border-zinc-700 bg-zinc-900">
        <div className="flex items-stretch border-b border-zinc-800 bg-zinc-800/40">
          <button
            type="button"
            data-testid="composer-tab-write"
            data-active={tab === "write"}
            onClick={() => setTab("write")}
            className={"-mb-px border-b-2 px-3 h-8 text-[12px] font-medium " + (tab === "write" ? "border-sky-400 text-zinc-100" : "border-transparent text-zinc-400")}
          >
            Write
          </button>
          <button
            type="button"
            data-testid="composer-tab-preview"
            data-active={tab === "preview"}
            onClick={() => setTab("preview")}
            className={"-mb-px border-b-2 px-3 h-8 text-[12px] font-medium " + (tab === "preview" ? "border-sky-400 text-zinc-100" : "border-transparent text-zinc-400")}
          >
            Preview
          </button>
          <span className="flex-1" />
          {tab === "write" && (
            <div data-testid="composer-toolbar" className="flex items-center gap-0.5 px-1.5">
              <button type="button" title="Heading" className={tbBtn} onClick={() => apply((s) => prefixLines(s, (l) => "### " + l))}>H</button>
              <button type="button" title="Bold" className={tbBtn} onClick={() => apply((s) => wrapSelection(s, "**", "**", "bold text"))}>B</button>
              <button type="button" title="Italic" className={tbBtn} onClick={() => apply((s) => wrapSelection(s, "_", "_", "italic text"))}>i</button>
              <button type="button" title="Quote" className={tbBtn} onClick={() => apply((s) => prefixLines(s, (l) => "> " + l))}>&gt;</button>
              <button type="button" title="Code" className={tbBtn} onClick={() => apply((s) => wrapSelection(s, "`", "`", "code"))}>{"<>"}</button>
              <button type="button" title="Link" className={tbBtn} onClick={() => apply(insertLink)}>🔗</button>
              <span className="mx-0.5 h-4 w-px bg-zinc-700" />
              <button type="button" title="Bulleted list" className={tbBtn} onClick={() => apply((s) => prefixLines(s, (l) => "- " + l))}>•</button>
              <button type="button" title="Numbered list" className={tbBtn} onClick={() => apply((s) => prefixLines(s, (l, i) => `${i + 1}. ` + l))}>1.</button>
              <button type="button" title="Task list" className={tbBtn} onClick={() => apply((s) => prefixLines(s, (l) => "- [ ] " + l))}>☑</button>
              <span className="mx-0.5 h-4 w-px bg-zinc-700" />
              <button type="button" title="Mention a reviewer" className={tbBtn} onClick={() => apply((s) => wrapSelection(s, "@", "", ""))}>@</button>
            </div>
          )}
        </div>

        {tab === "write" ? (
          <div className="relative">
            <textarea
              ref={ref}
              data-testid="composer-textarea"
              value={txt}
              autoFocus={autoFocus}
              onInput={onInput}
              placeholder={placeholder}
              onKeyDown={(e) => {
                if (mention && mentionHits[0] && (e.key === "Enter" || e.key === "Tab")) {
                  e.preventDefault();
                  pickMention(mentionHits[0]);
                  return;
                }
                if ((e.metaKey || e.ctrlKey) && (e.key === "g" || e.key === "G") && suggestionSeed != null) {
                  e.preventDefault();
                  doSuggestion();
                  return;
                }
                if (onDelete && (e.metaKey || e.ctrlKey) && (e.key === "Backspace" || e.key === "Delete")) {
                  e.preventDefault();
                  onDelete();
                  return;
                }
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                  e.preventDefault();
                  submit();
                }
                if (e.key === "Escape") {
                  if (mention) setMention(null);
                  else if (onCancel) onCancel();
                }
              }}
              className="block min-h-[5rem] w-full resize-y bg-zinc-900 px-2.5 py-2 font-sans text-[13px] text-zinc-200 focus:outline-none"
            />
            {mention && mentionHits.length > 0 && (
              <div data-testid="mention-picker" className="absolute bottom-2 left-2.5 z-20 w-56 overflow-hidden rounded-md border border-zinc-700 bg-zinc-900 shadow-xl">
                <div className="border-b border-zinc-800 px-2.5 py-1 text-[10px] uppercase tracking-wide text-zinc-500">Reviewers</div>
                {mentionHits.map((r, i) => (
                  <button
                    type="button"
                    key={r}
                    data-testid="mention-hit"
                    onClick={() => pickMention(r)}
                    className={"flex w-full items-center gap-2 px-2.5 py-1.5 text-left " + (i === 0 ? "bg-sky-500/10" : "hover:bg-zinc-800")}
                  >
                    <span className="grid h-4 w-4 place-items-center rounded-full bg-sky-500 font-mono text-[9px] font-bold text-white">{r[0]?.toUpperCase()}</span>
                    <span className="font-mono text-[12px] text-zinc-200">{r}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        ) : (
          <div data-testid="composer-preview" className="min-h-[5rem] px-3 py-2">
            {txt.trim() ? (
              <CommentBody text={txt} snippet={snippet} lang={lang} shiki={shiki} />
            ) : (
              <span className="font-mono text-[12px] text-zinc-500">Nothing to preview.</span>
            )}
          </div>
        )}

        <div className="flex h-9 items-center gap-2 border-t border-zinc-800 bg-zinc-800/30 px-2.5">
          <span className="flex-1" />
          <span className="font-mono text-[10px] text-zinc-500">⌘↵ to submit{onDelete ? " · ⌘⌫ delete" : ""}</span>
        </div>
      </div>

      <div className="mt-2 flex items-center gap-2">
        <button type="button" data-testid="composer-submit" onClick={submit} className="h-7 rounded-md bg-sky-500 px-2.5 text-[12px] font-medium text-white">
          {submitLabel}
        </button>
        {suggestionSeed != null && (
          <button type="button" data-testid="composer-suggest" title="Suggest a change (⌘G)" onClick={doSuggestion} className="h-7 rounded-md border border-zinc-700 px-2.5 text-[12px] font-medium text-zinc-300">
            ± Suggest a change
          </button>
        )}
        <span className="flex-1" />
        {onDelete && (
          <button type="button" data-testid="composer-delete" onClick={onDelete} className="h-7 rounded-md px-2.5 text-[12px] font-medium text-rose-400 hover:bg-rose-500/10">
            Delete
          </button>
        )}
        {onCancel && (
          <button type="button" data-testid="composer-cancel" onClick={onCancel} className="h-7 rounded-md px-2.5 text-[12px] font-medium text-zinc-400 hover:bg-zinc-800">
            Cancel
          </button>
        )}
      </div>
    </div>
  );
}
