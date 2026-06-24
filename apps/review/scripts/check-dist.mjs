// Local-first STRUCTURAL gate (the TS analog of the Rust resource-ref lint).
//
// Asserts the production dist/ ships ZERO runtime-fetchable highlighter chunks
// AND zero CDN/loader resource refs in the emitted HTML/JS/CSS. Every theme /
// lang / wasm asset must be bundled into the entry chunk via static imports +
// the register/preload path — never a separately-emitted fetchable chunk.
//
// ALLOW-LISTED exception: the BUNDLED elkjs worker chunk. It is a same-origin
// vendored asset Vite emits for `new Worker(new URL("../worker/elk.worker.ts",
// import.meta.url))` — local-first by construction (no CDN worker URL). The
// allow-list is pinned NARROWLY to the exact emitted filename shape
// (`assets/elk.worker-<hash>.js`) — a broad `/worker/i` would exempt any
// `*worker*.js` chunk from this LOAD-BEARING zero-egress gate. We additionally
// enforce that EXACTLY ONE worker chunk exists (0 or >1 fails). All
// theme/lang/wasm/oniguruma chunks are still rejected.
import { readdirSync, statSync, readFileSync } from "node:fs";
import { resolve, join } from "node:path";

const DIST = resolve(import.meta.dirname, "..", "dist");

// Forbidden file-name patterns (separately-emitted runtime-fetchable chunks).
const FORBIDDEN_NAMES = [
  // shiki theme names
  /tokyo-night/i, /dracula/i, /catppuccin/i, /mocha/i, /one-dark/i, /onedark/i,
  /gruvbox/i, /github-dark/i, /github-light/i, /latte/i, /one-light/i, /onelight/i,
  /vitesse/i, /everforest/i, /solarized/i, /monokai/i, /\bnord\b/i,
  // lang names as chunks (e.g. `sql-XXXX.js`)
  /(^|[/_-])sql[._-]/i, /(^|[/_-])yaml[._-]/i, /(^|[/_-])python[._-]/i,
  /(^|[/_-])javascript[._-]/i, /(^|[/_-])typescript[._-]/i,
  // wasm / oniguruma
  /wasm/i, /oniguruma/i, /\bonig\b/i,
];

// CDN / external-loader patterns in file CONTENT (HTML/JS/CSS sources).
const FORBIDDEN_CONTENT = [
  { re: /<script[^>]+\bsrc=["']https?:/i, why: "<script src> to remote" },
  { re: /<link[^>]+\bhref=["']https?:/i, why: "<link href> to remote" },
  { re: /<img[^>]+\bsrc=["']https?:/i, why: "<img src> to remote" },
  { re: /@import\s+(url\()?["']https?:/i, why: "CSS @import remote" },
  { re: /url\(\s*["']?https?:\/\//i, why: "CSS url() remote" },
  { re: /url\(\s*["']?\/\//i, why: "CSS url() protocol-relative" },
  { re: /\besm\.sh\b/i, why: "esm.sh CDN ref" },
  { re: /\bcdn\.(jsdelivr|skypack|unpkg)\b/i, why: "JS CDN ref" },
  { re: /\bunpkg\.com\b/i, why: "unpkg CDN ref" },
  { re: /(["'])\/\/[a-z0-9.-]+\.[a-z]{2,}\//i, why: "protocol-relative URL literal" },
];

// The ONE allow-listed local-first worker chunk — pinned to the exact emitted
// shape `assets/elk.worker-<hash>.js` (Vite hashes the basename). Anything
// matching `*worker*` more broadly is NOT exempt from the forbidden-name gate.
const WORKER_ALLOW = /(^|\/)(assets\/)?elk\.worker-[A-Za-z0-9_-]+\.js$/;

function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) out.push(...walk(p));
    else out.push(p);
  }
  return out;
}

const abs = walk(DIST);
const files = abs.map((p) => p.slice(DIST.length + 1));

console.log("dist/ files:");
for (const f of files) console.log("  " + f);

let failed = false;

// 1. Forbidden chunk names — workers are allow-listed.
const nameOffenders = files.filter(
  (f) => FORBIDDEN_NAMES.some((re) => re.test(f)) && !WORKER_ALLOW.test(f),
);
if (nameOffenders.length > 0) {
  console.error("\nFAIL: runtime-fetchable highlighter chunks found in dist/:");
  for (const o of nameOffenders) console.error("  " + o);
  failed = true;
}

// 2. Content scan for CDN / remote loaders (HTML/JS/CSS only).
for (let i = 0; i < files.length; i++) {
  const rel = files[i];
  if (!/\.(html|js|mjs|css)$/.test(rel)) continue;
  const text = readFileSync(abs[i], "utf8");
  for (const { re, why } of FORBIDDEN_CONTENT) {
    if (re.test(text)) {
      console.error(`\nFAIL: ${rel} contains a remote loader ref (${why}).`);
      failed = true;
    }
  }
}

// 3. Sanity: exactly one ENTRY JS chunk (workers excluded from the count).
const entryJs = files.filter((f) => /\.js$/.test(f) && !WORKER_ALLOW.test(f));
const workerJs = files.filter((f) => /\.js$/.test(f) && WORKER_ALLOW.test(f));
if (entryJs.length !== 1) {
  console.error(
    `\nFAIL: expected exactly 1 entry JS chunk, found ${entryJs.length}: ${entryJs.join(", ")}`,
  );
  failed = true;
}

// 4. Exactly ONE allow-listed worker chunk must exist. The allow-list narrowly
// exempts the elk worker from the forbidden-name gate; if Vite ever stops
// emitting it (0) or emits more than one (>1), the allow-list could be silently
// covering an unexpected chunk — fail loudly either way.
if (workerJs.length !== 1) {
  console.error(
    `\nFAIL: expected exactly 1 allow-listed worker chunk, found ${workerJs.length}: ${workerJs.join(", ") || "none"}`,
  );
  failed = true;
}

if (failed) process.exit(1);

console.log("\nPASS: dist/ has no runtime-fetchable theme/lang/wasm chunks and no remote loader refs.");
console.log(`  entry JS chunk: ${entryJs[0]}`);
console.log(`  bundled worker chunk(s) (allow-listed, local-first): ${workerJs.join(", ") || "none"}`);
