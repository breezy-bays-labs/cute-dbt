import { defineConfig } from "crap4ts";

// CRAP scorecard for the review app — wired as a RAW-exit CI gate on the
// skeleton (a council MUST-FIX: wire it before there's code to hide behind).
// Threshold 25 (the org default ceiling); the strict ≤15 set tightens per
// honesty-fold slice as those land (S3b/S6/S7). Consumes vitest's v8 coverage
// JSON (coverage/coverage-final.json).
//
// Scope = the LOGIC layers (domain/data) — the testable branching surface. The
// presentation/glue layers (chrome/view/main/worker/stubs) are React/framework
// wiring exercised by the Playwright behavioral gate, not unit-CRAP targets.
export default defineConfig({
  threshold: 25,
  // STRICT ≤15 on the honesty-fold set (council §E, gating from S3b): the
  // never-a-false-claim folds carry a tighter CRAP ceiling than the default 25.
  // Per-glob overrides; a function in these files exceeding 15 fails the gate.
  thresholds: {
    "src/domain/data/cell-diff.ts": 15, // the cell key.t trichotomy (diffSide/cellSide/adaptDiffTable)
    "src/domain/data/col-lineage.ts": 15, // buildColGraph / buildColEdges / confidence folds
    "src/domain/data/raw-spans.ts": 15, // rawDagToGraph byte-span parsing + buildRawSpans
  },
  coverageMetric: "line",
  src: ["src"],
  exclude: [
    "**/*.test.*",
    "**/*.spec.*",
    "**/*.d.ts",
    "src/chrome/**",
    "src/view/**",
    "src/main.tsx",
    "src/stubs/**",
    "src/worker/**",
    // adr: the highlighter modules are Pierre/Shiki + browser-bound (register +
    // preloadHighlighter resolve only in a real DOM); they are exercised by the
    // Playwright behavioral local-first gate (tests/local-first.spec.ts — the
    // theme-genuinely-applied + loud-fail asserts), not by unit-CRAP. Same
    // posture as the Rust render-layer modules covered by headless tests.
    "src/domain/highlighter.ts",
    "src/domain/code-highlighter.ts",
  ],
});
