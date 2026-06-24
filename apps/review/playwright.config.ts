import { defineConfig } from "@playwright/test";

// Serves the PRODUCTION dist/ on loopback via `vite preview`, then runs the
// network-denied local-first gate against it. Single chromium project; no retries
// (a flake is a real failure for a determinism gate).
export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:4173",
    trace: "off",
  },
  webServer: {
    command: "bun --bun vite preview --port 4173 --strictPort --host 127.0.0.1",
    url: "http://127.0.0.1:4173/index.html",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
    // The cmux shell exports a node `--require` preload (NODE_OPTIONS) that
    // crashes any spawned node. vite preview runs under bun (`--bun`) but clear
    // it defensively so the webServer never inherits the broken preload.
    env: { NODE_OPTIONS: "" },
  },
});
