import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "node:path";
import { defineConfig } from "vite";

// Local-first review-app build config. Carries the SAME 3 resolve aliases proven
// by the design harness (strip shiki/wasm, @pierre/theming/themes, and the bare
// `shiki` full-bundle dynamic-import maps) + base:'./' + codeSplitting:false.
//
// The register/preload path in src/domain/highlighter.ts supplies the 12 themes +
// sql/yaml via statically-imported loaders, so the stripped bundle maps are dead
// weight. elkjs runs in a BUNDLED worker (src/worker/elk.worker.ts) — Vite emits
// it as a same-origin asset via `new Worker(new URL(...), { type: "module" })`,
// NEVER a CDN worker URL. check-dist.mjs allow-lists exactly that one worker
// chunk while still rejecting every theme/lang/wasm/CDN loader.
export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss()],
  worker: { format: "es" },
  resolve: {
    // ORDER + exact-match anchors matter: more specific specifiers first.
    alias: [
      {
        find: /^shiki\/wasm$/,
        replacement: resolve(import.meta.dirname, "src/stubs/shiki-wasm-stub.js"),
      },
      {
        find: /^@pierre\/theming\/themes$/,
        replacement: resolve(import.meta.dirname, "src/stubs/pierre-theming-themes-stub.js"),
      },
      {
        find: /^shiki$/,
        replacement: resolve(import.meta.dirname, "src/stubs/shiki-local-stub.js"),
      },
    ],
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      output: {
        codeSplitting: false,
      },
    },
  },
});
