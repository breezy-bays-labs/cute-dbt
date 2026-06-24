// Local-first replacement for `shiki/wasm`.
//
// PROBLEM: @pierre/diffs/dist/highlighter/shared_highlighter.js contains a
// literal `import("shiki/wasm")` inside a ternary:
//   engine: preferredHighlighter === "shiki-wasm"
//     ? createOnigurumaEngine(import("shiki/wasm"))
//     : createJavaScriptRegexEngine()
// Even though we ALWAYS use 'shiki-js' (the JS branch), the bundler statically
// parses the `import("shiki/wasm")` expression and emits the ~620 KB oniguruma
// WASM as a separately-FETCHABLE chunk, and the entry retains a live import()
// reference to it. That breaks local-first / zero-egress.
//
// FIX: a Vite `resolve.alias` maps `shiki/wasm` to this empty module. The
// shiki-wasm branch is never executed in our build, so the contents are inert;
// aliasing it away collapses the WASM chunk to nothing.
export default undefined;
