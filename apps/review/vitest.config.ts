import { defineConfig } from "vitest/config";

// Unit/integration tests only (*.test.ts). The Playwright *.spec.ts files (the
// behavioral local-first gate) are excluded — they run under `playwright test`.
export default defineConfig({
  test: {
    include: ["src/**/*.test.ts", "src/**/*.test.tsx", "tests/**/*.test.ts"],
    exclude: ["**/node_modules/**", "**/dist/**", "tests/**/*.spec.ts"],
    environment: "node",
    coverage: {
      provider: "v8",
      include: ["src/domain/**", "src/data/**"],
      reporter: ["text-summary", "json"],
      reportsDirectory: "coverage",
    },
  },
});
