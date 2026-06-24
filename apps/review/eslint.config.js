// ESLint flat config (v9) — the layered-architecture fitness function (the TS
// analog of the Rust tests/domain_clean_arch.rs). eslint-plugin-boundaries
// enforces the inward-dependency rule: domain/data → view → chrome, with NO
// inward imports (chrome may import view/data/domain; view may import
// data/domain; domain/data may import neither view nor chrome).
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import boundaries from "eslint-plugin-boundaries";

export default tseslint.config(
  {
    ignores: [
      "dist/**",
      "node_modules/**",
      "playwright-report/**",
      "test-results/**",
      "coverage/**",
      ".stryker-tmp/**", // Stryker's per-mutant sandbox (injects @ts-nocheck) — never lint
      "reports/**", // Stryker JSON mutation report
      "*.cjs",
      "*.config.{js,ts}",
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    plugins: { boundaries },
    settings: {
      "boundaries/include": ["src/**/*"],
      "boundaries/elements": [
        // Order: most-specific first. Each src/ subtree is a layer. The `**`
        // recurses into NESTED dirs (e.g. src/domain/data/* — the S3b reshapers)
        // so the boundary FF covers sub-packages, not just direct children (a `*`
        // matched only one segment, silently exempting nested files).
        { type: "domain", pattern: "src/domain/**" },
        { type: "data", pattern: "src/data/**" },
        { type: "worker", pattern: "src/worker/**" },
        { type: "view", pattern: "src/view/**" },
        { type: "chrome", pattern: "src/chrome/**" },
        { type: "entry", pattern: "src/{main,styles,vite-env}.*", mode: "file" },
        { type: "stub", pattern: "src/stubs/**" },
        { type: "fixture", pattern: "src/fixtures/*", mode: "file" },
      ],
    },
    rules: {
      "boundaries/element-types": [
        "error",
        {
          default: "disallow",
          rules: [
            // domain: pure — may NOT import view/chrome/data (only itself + worker types).
            { from: "domain", allow: ["domain"] },
            // data: domain + the fixtures it loads.
            { from: "data", allow: ["data", "domain", "fixture"] },
            // worker: domain only (the elk worker is a leaf).
            { from: "worker", allow: ["worker", "domain"] },
            // view: domain + data + worker (no chrome).
            { from: "view", allow: ["view", "data", "domain", "worker"] },
            // chrome: everything inward.
            { from: "chrome", allow: ["chrome", "view", "data", "domain", "worker"] },
            // entry (main.tsx): composes the whole app.
            { from: "entry", allow: ["chrome", "view", "data", "domain", "entry"] },
            // stubs: build-time shims (no layer constraint among themselves).
            { from: "stub", allow: ["stub"] },
          ],
        },
      ],
    },
  },
  {
    // The worker file references DOM-worker globals; relax the no-explicit-any
    // there only minimally is not needed — keep strict everywhere.
    files: ["scripts/**/*.mjs"],
    languageOptions: { globals: { process: "readonly", console: "readonly" } },
    rules: { "@typescript-eslint/no-unused-vars": "off" },
  },
);
