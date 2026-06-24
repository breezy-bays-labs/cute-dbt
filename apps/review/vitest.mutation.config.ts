import { defineConfig } from "vitest/config";

// Mutation-only vitest config: runs ONLY the strict honesty-fold tests so Stryker
// (coverageAnalysis "all", deterministic) reruns a small, fast suite per mutant
// rather than the whole 300+ test app suite. Mirrors the Rust targeted
// cargo-mutants posture (kill-harness scoped to the load-bearing modules).
export default defineConfig({
  test: {
    include: [
      "src/domain/data/cell-diff.test.ts",
      "src/domain/data/col-lineage.test.ts",
      "src/domain/data/raw-spans.test.ts",
      "src/domain/data/raw-spans.fuzz.test.ts",
      "src/domain/data/honesty.property.test.ts",
      "src/domain/data/dataset.test.ts",
      "src/domain/cursor-sync.test.ts",
    ],
    environment: "node",
  },
});
