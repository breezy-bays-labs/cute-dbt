// Markdown — GitHub-flavoured comment rendering via react-markdown + remark-gfm
// + rehype-sanitize. rehype-sanitize is the load-bearing safety: comment bodies
// are untrusted (they round-trip from the PR), so raw HTML / scripts / unsafe
// link protocols are stripped. NO dangerouslySetInnerHTML anywhere on this path.
//
// CommentBody additionally splits ```suggestion blocks out of the body and
// renders each as a first-party SuggestionBlock diff against the anchored
// snippet (the suggestion-as-diff invariant).
//
// LAYER: view (imports domain only).
import React from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeSanitize from "rehype-sanitize";
import { hasSuggestion, splitSuggestions } from "../../domain/diff/suggestion";
import type { CodeLang } from "../../domain/code-highlighter";
import { SuggestionBlock } from "./SuggestionBlock";

// LOCAL-FIRST component overrides — the load-bearing zero-egress fix for rendered
// markdown. Comment bodies are untrusted PR content and routinely embed REMOTE
// assets (e.g. gemini/CodeRabbit badge images: `![](https://gstatic.com/…svg)`).
// A bare react-markdown would render those as a fetching `<img src>` / navigating
// `<a href>`, leaking a network request the moment the report is opened — exactly
// the invariant the network-denied gate exists to protect. So:
//   - `img` NEVER fetches: render an INERT placeholder (alt + the URL as text).
//     never-a-false-claim: we honestly show "an image was here" + its source,
//     without loading it.
//   - `a` does not navigate on click and carries rel="noopener noreferrer"; it
//     renders the link text but the href is shown inert (no auto-prefetch, no
//     egress). Keeping it a non-anchor element guarantees zero passive fetch.
// rehype-sanitize still strips scripts / event handlers / unsafe protocols.
const SAFE_COMPONENTS: Components = {
  img(props) {
    const alt = typeof props.alt === "string" && props.alt ? props.alt : "image";
    return (
      <span data-testid="md-image-placeholder" data-src={typeof props.src === "string" ? props.src : undefined} className="inline-flex items-center gap-1 rounded border border-zinc-700 bg-zinc-800/60 px-1.5 py-0.5 font-mono text-[11px] text-zinc-400" title={typeof props.src === "string" ? props.src : undefined}>
        🖼 {alt}
        <span className="text-zinc-500">(not loaded · local-first)</span>
      </span>
    );
  },
  a({ children, href }) {
    // render the text; expose the href inertly (no navigation, no prefetch).
    return (
      <span data-testid="md-link" data-href={typeof href === "string" ? href : undefined} className="text-sky-400 underline decoration-dotted" title={typeof href === "string" ? href : undefined}>
        {children}
      </span>
    );
  },
};

export function Markdown({ text, className }: { text: string | null | undefined; className?: string }): React.ReactElement {
  return (
    <div data-testid="md-body" className={"md-body text-[13px] text-zinc-200 " + (className ?? "")}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeSanitize]} components={SAFE_COMPONENTS}>
        {String(text ?? "")}
      </ReactMarkdown>
    </div>
  );
}

/**
 * A comment body that may contain ```suggestion blocks. `snippet` is the
 * anchored (RIGHT-side) source the suggestion replaces. Non-suggestion segments
 * render as sanitized markdown; suggestion segments render as a SuggestionBlock.
 */
export function CommentBody({
  text,
  snippet,
  lang = "sql",
  shiki,
  className,
}: {
  text: string | null | undefined;
  snippet?: string | null;
  lang?: CodeLang;
  shiki: string;
  className?: string;
}): React.ReactElement {
  if (!hasSuggestion(text)) return <Markdown text={text} className={className} />;
  const segs = splitSuggestions(text);
  return (
    <div data-testid="comment-body" className={"md-body text-[13px] text-zinc-200 " + (className ?? "")}>
      {segs.map((s, i) =>
        s.type === "suggestion" ? (
          <SuggestionBlock key={i} oldCode={snippet} newCode={s.code} lang={lang} shiki={shiki} />
        ) : (
          <Markdown key={i} text={s.text} />
        ),
      )}
    </div>
  );
}
