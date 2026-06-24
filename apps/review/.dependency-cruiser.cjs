// dependency-cruiser — the SECOND enforcer of the layered architecture (belt +
// suspenders with eslint-plugin-boundaries; the TS analog of the Rust
// tests/domain_clean_arch.rs). Forbids inward imports: domain/data must never
// reach into view/chrome; view must never reach into chrome.
/** @type {import('dependency-cruiser').IConfiguration} */
module.exports = {
  forbidden: [
    {
      name: "domain-no-view-or-chrome",
      comment: "src/domain (pure) must not import view or chrome.",
      severity: "error",
      from: { path: "^src/domain" },
      to: { path: "^src/(view|chrome)" },
    },
    {
      name: "data-no-view-or-chrome",
      comment: "src/data must not import view or chrome.",
      severity: "error",
      from: { path: "^src/data" },
      to: { path: "^src/(view|chrome)" },
    },
    {
      name: "worker-no-view-or-chrome",
      comment: "src/worker must not import view or chrome.",
      severity: "error",
      from: { path: "^src/worker" },
      to: { path: "^src/(view|chrome)" },
    },
    {
      name: "view-no-chrome",
      comment: "src/view must not import chrome (the shell composes view, not vice-versa).",
      severity: "error",
      from: { path: "^src/view" },
      to: { path: "^src/chrome" },
    },
    {
      name: "no-circular",
      comment: "No circular dependencies.",
      severity: "error",
      from: {},
      to: { circular: true },
    },
  ],
  options: {
    doNotFollow: { path: "node_modules" },
    tsConfig: { fileName: "tsconfig.json" },
    tsPreCompilationDeps: true,
    enhancedResolveOptions: {
      extensions: [".ts", ".tsx", ".js", ".jsx", ".json"],
    },
    exclude: { path: "(node_modules|dist|\\.test\\.ts|\\.spec\\.ts)" },
  },
};
