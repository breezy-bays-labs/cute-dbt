// The ONE Shiki singleton's token API + the pure inline word-emphasis overlay.
//
// `highlightTokens` runs on the SAME singleton highlighter as `highlightCode`
// (domain/code-highlighter) — there is exactly ONE Shiki core in the app. It
// returns per-line token arrays so the SuggestionBlock + ShikiLine renderers
// can tint changed regions (overlayEmphasis) while keeping each token's syntax
// color. The overlay fold is pure (no highlighter) and unit-tested directly.
//
// LAYER: domain. The token-fetch path touches the singleton core (async); the
// `overlayEmphasis` fold is std-only.
import { highlighterCore, type CodeLang } from "./code-highlighter";
import type { Range } from "./diff/inline-emphasis";

export interface ShikiToken {
  text: string;
  /** the Shiki-assigned hex color for this token (or undefined for default). */
  color?: string;
}

export interface EmphSpan {
  text: string;
  color?: string;
  /** true iff this span falls inside an inline word-emphasis range. */
  emph: boolean;
}

/**
 * Tokenize ONE line via the singleton highlighter, returning {text,color} runs.
 * Loud-fail: a highlighter error rejects (the caller renders a plain fallback).
 */
export async function highlightTokens(line: string, lang: CodeLang, shiki: string): Promise<ShikiToken[]> {
  const hl = await highlighterCore();
  const res = hl.codeToTokens(String(line ?? ""), { lang, theme: shiki });
  const row = res.tokens[0] ?? [];
  return row.map((t) => ({ text: t.content, color: t.color }));
}

/**
 * Overlay inline word-emphasis ranges onto colored tokens: split each token at
 * the emphasis boundaries, preserving its color, marking the inside spans
 * `emph: true`. Lossless — the concatenated span text equals the input.
 */
export function overlayEmphasis(tokens: ShikiToken[], ranges: Range[]): EmphSpan[] {
  const inEmph = (pos: number): boolean => ranges.some(([a, b]) => pos >= a && pos < b);
  const spans: EmphSpan[] = [];
  let off = 0;
  for (const t of tokens) {
    let buf = "";
    let cur = t.text.length ? inEmph(off) : false;
    const flush = (): void => {
      if (!buf) return;
      spans.push({ text: buf, color: t.color, emph: cur });
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
  return spans;
}
