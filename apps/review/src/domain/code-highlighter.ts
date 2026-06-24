// A standalone Shiki highlighter for the read-only CODE PANE — built on
// @shikijs/core with the PURE-JS regex engine + STATICALLY-imported sql/yaml
// langs + the 12 app themes. Fully local-first: no dynamic import, no wasm, no
// CDN. Independent of Pierre's highlighter singleton (Pierre owns the diff
// surface; this owns the plain code pane).
import { createHighlighterCore, type HighlighterCore } from "@shikijs/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";

import sqlLang from "@shikijs/langs/sql";
import yamlLang from "@shikijs/langs/yaml";

import tokyoNight from "@shikijs/themes/tokyo-night";
import dracula from "@shikijs/themes/dracula";
import catppuccinMocha from "@shikijs/themes/catppuccin-mocha";
import oneDarkPro from "@shikijs/themes/one-dark-pro";
import gruvboxDarkMedium from "@shikijs/themes/gruvbox-dark-medium";
import githubDark from "@shikijs/themes/github-dark";
import githubLight from "@shikijs/themes/github-light";
import catppuccinLatte from "@shikijs/themes/catppuccin-latte";
import oneLight from "@shikijs/themes/one-light";
import vitesseLight from "@shikijs/themes/vitesse-light";
import everforestLight from "@shikijs/themes/everforest-light";
import solarizedLight from "@shikijs/themes/solarized-light";

let corePromise: Promise<HighlighterCore> | null = null;

/**
 * The ONE Shiki core for the whole app (council §E "ONE Shiki singleton"). Both
 * `highlightCode` (the read-only code pane) AND `highlightTokens` (the inline
 * word-emphasis path in shiki-tokens.ts) resolve THIS singleton — there is
 * exactly one `createHighlighterCore` in the bundle, with the consolidated 12
 * themes + sql/yaml langs registered once. Pierre owns its own highlighter for
 * the diff surface; this owns every plain/token render.
 */
export function highlighterCore(): Promise<HighlighterCore> {
  if (!corePromise) {
    corePromise = createHighlighterCore({
      themes: [
        tokyoNight, dracula, catppuccinMocha, oneDarkPro, gruvboxDarkMedium, githubDark,
        githubLight, catppuccinLatte, oneLight, vitesseLight, everforestLight, solarizedLight,
      ] as never[],
      langs: [sqlLang, yamlLang] as never[],
      engine: createJavaScriptRegexEngine(),
    });
  }
  return corePromise;
}

export type CodeLang = "sql" | "yaml";

/**
 * Highlight `code` to a self-contained HTML string (inline styles) using the
 * given shiki theme name. Throws loudly if the theme isn't loaded — never a
 * silent fallback.
 */
export async function highlightCode(code: string, lang: CodeLang, shiki: string): Promise<string> {
  const hl = await highlighterCore();
  return hl.codeToHtml(code, { lang, theme: shiki });
}
