// ShikiLine — a single line of code highlighted by the ONE Shiki singleton,
// with optional inline word-emphasis (changed char-ranges tinted while keeping
// each token's syntax color). Falls back to the synchronous plain highlighter
// (domain/plain-highlight) until Shiki resolves AND on SSR, so there is never a
// flash of unstyled code. PlainSpans is the SSR/loud-fail-safe render path.
//
// LAYER: view (imports domain only).
import React, { useEffect, useState } from "react";
import type { CodeLang } from "../../domain/code-highlighter";
import { highlightTokens, overlayEmphasis, type EmphSpan } from "../../domain/shiki-tokens";
import { highlightPlain } from "../../domain/plain-highlight";
import type { Range } from "../../domain/diff/inline-emphasis";

/** The synchronous plain-token render (SSR + pre-Shiki fallback). */
export function PlainSpans({ line, lang, emph, emphCls }: { line: string; lang: CodeLang; emph?: Range[]; emphCls?: string }): React.ReactElement {
  const toks = highlightPlain(line, lang);
  if (!emph || !emph.length) {
    return (
      <>
        {toks.map((t, i) => (
          <span key={i} className={t.cls ? "tok-" + t.cls : undefined}>
            {t.text}
          </span>
        ))}
      </>
    );
  }
  const inEmph = (pos: number): boolean => emph.some(([a, b]) => pos >= a && pos < b);
  const nodes: React.ReactElement[] = [];
  let off = 0;
  let key = 0;
  for (const t of toks) {
    let buf = "";
    let cur = t.text.length ? inEmph(off) : false;
    const flush = (): void => {
      if (!buf) return;
      const cls = [t.cls ? "tok-" + t.cls : "", cur ? emphCls ?? "" : ""].filter(Boolean).join(" ");
      nodes.push(
        <span key={key++} className={cls || undefined}>
          {buf}
        </span>,
      );
      buf = "";
    };
    for (let p = 0; p < t.text.length; p++) {
      const e = inEmph(off + p);
      if (e !== cur) {
        flush();
        cur = e;
      }
      buf += t.text[p];
    }
    flush();
    off += t.text.length;
  }
  return <>{nodes}</>;
}

export function ShikiLine({
  line,
  lang,
  shiki,
  emph,
  emphCls = "diff-word-add",
}: {
  line: string;
  lang: CodeLang;
  shiki: string;
  emph?: Range[];
  emphCls?: string;
}): React.ReactElement {
  const [spans, setSpans] = useState<EmphSpan[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    highlightTokens(line, lang, shiki)
      .then((toks) => {
        if (!cancelled) setSpans(overlayEmphasis(toks, emph ?? []));
      })
      .catch(() => {
        // loud-fail-safe: keep the plain fallback (no silent unstyled flash).
        if (!cancelled) setSpans(null);
      });
    return () => {
      cancelled = true;
    };
  }, [line, lang, shiki, JSON.stringify(emph)]);

  if (spans == null) {
    return <PlainSpans line={line} lang={lang} emph={emph} emphCls={emphCls} />;
  }
  return (
    <>
      {spans.map((s, i) => (
        <span key={i} className={s.emph ? emphCls : undefined} style={{ color: s.color }}>
          {s.text}
        </span>
      ))}
    </>
  );
}
