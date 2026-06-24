// Per-LINE Shiki tokenization for the topology code panes (S6b). The read-only
// CodePane (S0) highlights a whole block to one `<pre class="shiki">` string —
// fine for a static block, but the topology panes need LINE-LEVEL rows so each
// line can carry the span tint, the keyboard cursor marker, and the scroll/flash
// target the bidirectional CTE⇄code sync drives. So this wraps the SAME Shiki
// singleton (domain/code-highlighter) `codeToTokens` to return one colored-token
// array PER line — the renderer maps each to a `<tr>`.
//
// LAYER: domain. The fetch touches the singleton core (async); no DOM, no view.
import { highlighterCore, type CodeLang } from "./code-highlighter";
import type { ShikiToken } from "./shiki-tokens";

/** A highlighted source line: its colored token runs (concatenating `.text`
 *  losslessly reproduces the original line). */
export type CodeLineTokens = ShikiToken[];

/**
 * Tokenize EVERY line of `code` via the singleton highlighter, returning one
 * `{text,color}[]` row per source line (in source order). Loud-fail: a
 * highlighter error rejects (the pane renders its visible error banner — never a
 * silent plain-text pass-off). An empty string yields a single empty row, so the
 * row count always equals `code.split("\n").length` (the pane's line gutter
 * count stays in lock-step with the source-map spine's 1-based line numbers).
 */
export async function highlightLines(code: string, lang: CodeLang, shiki: string): Promise<CodeLineTokens[]> {
  const hl = await highlighterCore();
  const res = hl.codeToTokens(String(code ?? ""), { lang, theme: shiki });
  return res.tokens.map((row) => row.map((t) => ({ text: t.content, color: t.color })));
}
