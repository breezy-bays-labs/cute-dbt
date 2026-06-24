// Local-first replacement for the bare `shiki` specifier.
//
// PROBLEM: `shiki` (its `dist/index.mjs` "full bundle" entry) statically pulls
// in the 332-language `bundledLanguages` map, the 65-theme `bundledThemes`
// map, and a default engine of `() => createOnigurumaEngine(import("shiki/wasm"))`.
// Every entry in those maps is a `() => import(...)`, so under the bundler
// each becomes a SEPARATELY-EMITTED, runtime-FETCHABLE chunk — and the
// `import("shiki/wasm")` becomes a fetchable WASM chunk. That breaks the
// local-first / zero-egress invariant even though our register/preload path
// never reaches any of them at runtime (the bundler can't tree-shake individual
// properties out of a referenced object literal, so it emits them all).
//
// FIX: a Vite `resolve.alias` maps bare `shiki` to THIS module. It re-exports
// only what @pierre/diffs + @pierre/theming actually import from `shiki`, built
// on `@shikijs/core` (engine-agnostic, NO bundled maps) + the pure-JS regex
// engine. `bundledLanguages`/`bundledThemes` become EMPTY objects — Pierre's
// register/preload supplies the SQL/YAML grammars and app themes via
// statically-imported loaders instead.
import {
  createBundledHighlighter,
  createSingletonShorthands,
  guessEmbeddedLanguages,
  codeToHtml as coreCodeToHtml,
  createCssVariablesTheme,
  getTokenStyleObject,
  stringifyTokenStyle,
  normalizeTheme,
} from "@shikijs/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";

// Empty bundle maps. Pierre only ever loads "text" inline + register/preload
// langs/themes; it never resolves a NAME out of these maps in our flow.
export const bundledLanguages = {};
export const bundledLanguagesBase = {};
export const bundledLanguagesAlias = {};
export const bundledLanguagesInfo = [];
export const bundledThemes = {};
export const bundledThemesInfo = [];

// `createHighlighter` built with EMPTY bundle maps + the pure-JS engine as the
// default. Pierre always calls it as createHighlighter({ themes:[], langs:["text"],
// engine: createJavaScriptRegexEngine() }) so the empty maps + js engine default
// are never the load-bearing path — but keeping the JS engine here guarantees no
// WASM import can sneak in via the default.
export const createHighlighter = /* @__PURE__ */ createBundledHighlighter({
  langs: bundledLanguages,
  themes: bundledThemes,
  engine: () => createJavaScriptRegexEngine(),
});

export const {
  codeToHtml,
  codeToHast,
  codeToTokens,
  codeToTokensBase,
  codeToTokensWithThemes,
  getSingletonHighlighter,
  getLastGrammarState,
} = /* @__PURE__ */ createSingletonShorthands(createHighlighter, {
  guessEmbeddedLanguages,
});

// Pure-JS regex engine — re-exported so Pierre's `preferredHighlighter:'shiki-js'`
// branch resolves WITHOUT touching oniguruma/WASM.
export { createJavaScriptRegexEngine };

// `createOnigurumaEngine` is referenced by Pierre in a NEVER-TAKEN ternary
// branch (only when preferredHighlighter === 'shiki-wasm'). We must export the
// symbol, but it carries NO `import("shiki/wasm")` and throws loudly if anyone
// ever flips to the WASM engine in this local-first build.
export function createOnigurumaEngine() {
  throw new Error(
    "shiki-local-stub: createOnigurumaEngine (shiki-wasm) is disabled in the " +
      "local-first build. Use preferredHighlighter:'shiki-js'.",
  );
}

// Re-export the remaining value symbols Pierre / theming pull from bare shiki.
export {
  createCssVariablesTheme,
  getTokenStyleObject,
  stringifyTokenStyle,
  normalizeTheme,
  coreCodeToHtml,
};
