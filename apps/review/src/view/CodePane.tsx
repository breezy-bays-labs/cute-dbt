// A read-only Shiki-highlighted code pane. Highlights the selected model's
// compiled SQL (joined in DAG order) with the active theme via the standalone
// shiki-core highlighter (domain/code-highlighter). Loud-fail: a highlight error
// renders a visible banner, never silent plain text passed off as highlighted.
import React, { useEffect, useState } from "react";
import { highlightCode, type CodeLang } from "../domain/code-highlighter";

export function CodePane({
  code,
  lang,
  shiki,
}: {
  code: string;
  lang: CodeLang;
  shiki: string;
}): React.ReactElement {
  const [html, setHtml] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setHtml(null);
    setError(null);
    highlightCode(code, lang, shiki)
      .then((h) => {
        if (!cancelled) setHtml(h);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [code, lang, shiki]);

  if (error) {
    return (
      <div
        data-testid="code-pane-error"
        style={{ color: "#f7768e", font: "13px system-ui", padding: 12, border: "1px solid #f7768e", borderRadius: 8 }}
      >
        Code highlight failed ({shiki}): {error}
      </div>
    );
  }

  if (html == null) {
    return (
      <div data-testid="code-pane-loading" style={{ opacity: 0.6, font: "13px ui-monospace", padding: 12 }}>
        highlighting…
      </div>
    );
  }

  return (
    <div
      data-testid="code-pane"
      data-lang={lang}
      style={{
        font: "12px ui-monospace, SFMono-Regular, Menlo, monospace",
        borderRadius: 8,
        overflow: "auto",
        maxHeight: 360,
        border: "1px solid #2a2b36",
      }}
      // Shiki output is self-contained, trusted (our highlighter over our own
      // compiled-SQL string — no user HTML). The only sink for shiki tokens.
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
