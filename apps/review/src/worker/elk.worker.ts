// The elkjs layout worker — BUNDLED by Vite (a same-origin chunk), NEVER a CDN
// worker URL. This wraps elkjs's OWN worker entry (`elk-worker.js`), which
// installs `self.onmessage` and speaks ELK's native layout protocol. The ELK api
// class on the main thread (src/view/useElkLayout.ts) drives it via a
// `workerFactory` that constructs `new Worker(new URL("./elk.worker.ts",
// import.meta.url), { type: "module" })` — local-first by construction.
//
// We intentionally do NOT import `elk.bundled.js` (it both is a worker AND a
// class; importing it into a worker hijacks self.onmessage with broken CJS
// interop — `o is not a constructor`). `elk-worker.js` is the worker half only.
import "elkjs/lib/elk-worker.js";
