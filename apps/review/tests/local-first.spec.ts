// The BEHAVIORAL local-first gate — the React analog of the Rust
// headless_zero_egress.rs. Opens the PRODUCTION dist/ on loopback with ALL
// non-localhost traffic DENIED and asserts:
//   1. ZERO external (non-localhost) http/https/ws/wss requests.
//   2. The full stack RENDERS: a Pierre diff (<diffs-container>) + a React Flow
//      DAG (a [data-testid=cte-node], laid out by the elkjs worker) + a Shiki
//      code pane (.shiki output).
//   3. The active theme is GENUINELY tokyo-night — NOT the github-dark fallback
//      (loud-fail-on-fallback is the trust property).
//   4. Zustand persist round-trips the selected model under a `cute-dbt:` key.
import { test, expect, type Page } from "@playwright/test";

// Theme identity signals (verified against @shikijs/themes@4.2.0):
//   tokyo-night editor.background = #1a1b26 -> rgb(26, 27, 38)
//   tokyo-night editor.foreground = #a9b1d6 -> rgb(169, 177, 214)
//   github-dark editor.background = #24292e -> rgb(36, 41, 46)
const TOKYO_NIGHT_BG = "rgb(26, 27, 38)";
const GITHUB_DARK_BG = "rgb(36, 41, 46)";

async function denyExternalNetwork(page: Page): Promise<string[]> {
  const external: string[] = [];
  await page.route("**", (route) => {
    const u = new URL(route.request().url());
    const isLocal = u.hostname === "127.0.0.1" || u.hostname === "localhost";
    if (!isLocal) {
      external.push(u.href);
      return route.abort();
    }
    return route.continue();
  });
  return external;
}

test("network-denied render: zero external, full stack renders, genuine tokyo-night", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  const consoleErrors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") consoleErrors.push(m.text());
  });
  page.on("pageerror", (e) => consoleErrors.push("PAGEERROR: " + e.message));

  await page.goto("/index.html", { waitUntil: "networkidle" });

  // ── The app shell mounted (Zod-validated context rendered). ────────────────
  await expect(page.getByTestId("app-title")).toBeVisible();
  await expect(page.getByTestId("entity-tabs")).toBeVisible();
  await expect(page.getByTestId("model-list")).toBeVisible();

  // ── The Pierre diff rendered (shadow-DOM diffs-container). ─────────────────
  await page.waitForSelector("diffs-container");

  // ── The React Flow DAG rendered + laid out (elkjs worker). ─────────────────
  await page.waitForSelector('[data-testid="cte-node"]');
  await expect(page.getByTestId("dag-legend")).toBeVisible();
  // elkjs worker produced non-trivial positions (not all stacked at x=0).
  const xs = await page.$$eval(".react-flow__node", (nodes) =>
    nodes.map((n) => {
      const t = getComputedStyle(n).transform;
      const m = /matrix.*?,\s*([-\d.]+),\s*[-\d.]+\)/.exec(t);
      return m && m[1] ? parseFloat(m[1]) : 0;
    }),
  );
  expect(xs.length, "expected React Flow nodes").toBeGreaterThan(0);
  expect(new Set(xs).size, "expected DAG nodes laid out at distinct x positions").toBeGreaterThan(1);

  // ── The Shiki code pane rendered (highlighted, not the loading/error state). ─
  await page.waitForSelector('[data-testid="code-pane"] .shiki');

  // ── Zero external requests, zero console/page errors. ──────────────────────
  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
  expect(consoleErrors, `console/page errors: ${consoleErrors.join(" | ")}`).toEqual([]);

  // ── Theme is GENUINELY tokyo-night (NOT github-dark fallback). ─────────────
  const themeProbe = await page.evaluate(
    ({ tnBg }) => {
      const containers = [...document.querySelectorAll("diffs-container")];
      let bgMatch = false;
      let containerBg = "";
      for (const c of containers) {
        const cbg = getComputedStyle(c).backgroundColor;
        if (!containerBg) containerBg = cbg;
        if (cbg === tnBg) bgMatch = true;
        const scope = c.shadowRoot ?? c;
        for (const el of scope.querySelectorAll<HTMLElement>("*")) {
          if (getComputedStyle(el).backgroundColor === tnBg) bgMatch = true;
        }
      }
      // Also check the Shiki pane carries the tokyo-night bg.
      const shiki = document.querySelector<HTMLElement>('[data-testid="code-pane"] .shiki');
      const shikiBg = shiki ? getComputedStyle(shiki).backgroundColor : "";
      return { bgMatch, containerBg, shikiBg };
    },
    { tnBg: TOKYO_NIGHT_BG },
  );
  expect(themeProbe.bgMatch || themeProbe.shikiBg === TOKYO_NIGHT_BG, "tokyo-night bg not found in diff or code pane").toBe(true);
  expect(themeProbe.containerBg).not.toBe(GITHUB_DARK_BG);
  expect(themeProbe.shikiBg, "Shiki code pane fell back to github-dark").not.toBe(GITHUB_DARK_BG);

  // No theme-error banner (no silent or loud fallback).
  await expect(page.getByTestId("theme-error-banner")).toHaveCount(0);
  await expect(page.getByTestId("code-pane-error")).toHaveCount(0);
});

test("Zustand persist round-trips the selected model under a cute-dbt: key", async ({ page }) => {
  await denyExternalNetwork(page);
  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="model-list-item"]');

  // Click the SECOND model (not the default first).
  const items = page.getByTestId("model-list-item");
  const count = await items.count();
  expect(count).toBeGreaterThan(1);
  const second = items.nth(1);
  const name = await second.getAttribute("data-model");
  await second.click();
  await expect(second).toHaveAttribute("data-selected", "true");

  // The selection persisted under the `cute-dbt:` localStorage key.
  const persisted = await page.evaluate(() => localStorage.getItem("cute-dbt:review"));
  expect(persisted, "expected cute-dbt:review key in localStorage").toBeTruthy();
  expect(persisted!).toContain(name!);

  // Reload — the persisted model is still selected (hydration).
  await page.reload({ waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="model-list-item"]');
  const stillSelected = page.locator(`[data-testid="model-list-item"][data-model="${name}"]`);
  await expect(stillSelected).toHaveAttribute("data-selected", "true");
});

test("the SINGLE keydown dispatcher routes entity/view/overlay keys + persists nav", async ({ page }) => {
  // Proves the S2 dispatch spine end-to-end against the real bundle: one
  // capture-phase listener routes the number-row entity keys, the ⇧digit
  // positional view keys, and an app overlay key — and the resulting nav state
  // round-trips through the `cute-dbt:review` persist namespace (network-denied).
  await denyExternalNetwork(page);
  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');

  // default entity = Models (the entity tab is active).
  await expect(page.locator('[data-testid="tab-models"]')).toHaveAttribute("data-active", "true");

  // press "3" → Macros entity (number-row entity key, routed by the dispatcher).
  await page.keyboard.press("3");
  await expect(page.locator('[data-testid="tab-macros"]')).toHaveAttribute("data-active", "true");

  // press "2" → back to Models.
  await page.keyboard.press("2");
  await expect(page.locator('[data-testid="tab-models"]')).toHaveAttribute("data-active", "true");

  // press ⇧3 → the THIRD Models view (data / "Unit tests"), positional over AVAIL.
  await page.keyboard.press("Shift+Digit3");
  await expect(page.locator('[data-testid="tab-data"]')).toHaveAttribute("data-active", "true");

  // press "s" → toggle the sidebar overlay (an app-level key).
  await page.keyboard.press("s");
  await expect(page.locator('[data-testid="btn-sidebar"]')).toHaveAttribute("data-active", "true");

  // The nav state (entity + per-entity view) persisted under the cute-dbt: key.
  const persisted = await page.evaluate(() => localStorage.getItem("cute-dbt:review"));
  expect(persisted, "expected cute-dbt:review persist blob").toBeTruthy();
  const blob = JSON.parse(persisted!) as { state: { entity: string; viewByEntity: Record<string, string>; sidebar: boolean } };
  expect(blob.state.entity).toBe("models");
  expect(blob.state.viewByEntity.models).toBe("data");
  expect(blob.state.sidebar).toBe(true);

  // Reload → the nav position is restored (the dispatcher routed it, persist hydrated it).
  await page.reload({ waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');
  await expect(page.locator('[data-testid="tab-data"]')).toHaveAttribute("data-active", "true");
  await expect(page.locator('[data-testid="btn-sidebar"]')).toHaveAttribute("data-active", "true");
});

test("an open overlay OWNS the keyboard (modal gate suppresses entity keys)", async ({ page }) => {
  await denyExternalNetwork(page);
  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');

  // open the settings overlay flag via the keyboard (",").
  await page.keyboard.press(",");
  // the settings button reflects the open flag.
  await expect(page.locator('[data-testid="btn-settings"]')).toBeVisible();

  // while the modal flag is set, a number-row entity key must be SUPPRESSED
  // (the modal gate hands keyboard ownership to the overlay). Models stays active.
  const before = await page.locator('[data-testid="tab-models"]').getAttribute("data-active");
  await page.keyboard.press("3");
  await expect(page.locator('[data-testid="tab-models"]')).toHaveAttribute("data-active", before ?? "true");
  await expect(page.locator('[data-testid="tab-macros"]')).toHaveAttribute("data-active", "false");
});

test("S4 DAG engine: PR-scope lineage renders via the elkjs worker (real layout, local-first) + the 3-axis toggle swaps the subgraph + the prNode nav split holds", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  const consoleErrors: string[] = [];
  page.on("pageerror", (e) => consoleErrors.push("PAGEERROR: " + e.message));

  // Instrument the Worker constructor BEFORE any script runs so we can prove the
  // elk worker is kept ALIVE across re-layouts (axis toggle). Each `new Worker`
  // bumps a page-global elk-worker counter (matched by the vite-emitted "elk"
  // filename); a re-layout that respawned the worker would increment it.
  await page.addInitScript(() => {
    const w = window as unknown as { __elkWorkerCount?: number };
    w.__elkWorkerCount = 0;
    const NativeWorker = window.Worker;
    class CountingWorker extends NativeWorker {
      constructor(url: string | URL, opts?: WorkerOptions) {
        const href = typeof url === "string" ? url : url.href;
        if (/elk/i.test(href)) w.__elkWorkerCount = (w.__elkWorkerCount ?? 0) + 1;
        super(url, opts);
      }
    }
    window.Worker = CountingWorker as unknown as typeof Worker;
  });

  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');

  // The Models default model is selected — capture it to prove the nav split later.
  await page.waitForSelector('[data-testid="model-list-item"]');
  const selectedBefore = await page
    .locator('[data-testid="model-list-item"][data-selected="true"]')
    .getAttribute("data-model");

  // Navigate to PR → Topology (entity key "1", then the Topology view tab).
  await page.keyboard.press("1");
  await page.locator('[data-testid="tab-lineage"]').click();
  await page.waitForSelector('[data-testid="pr-scope-lineage"]');
  await page.waitForSelector('[data-testid="graph-node"]');

  // ── the engine rendered through the custom node + custom edge pipeline ─────
  const nodeCount = await page.locator('[data-testid="graph-node"]').count();
  expect(nodeCount, "PR-scope nodes rendered").toBeGreaterThan(1);
  await expect(page.locator('[data-testid="confidence-edge"]').first()).toBeAttached();

  // ── the elkjs worker produced DISTINCT geometry (real layered layout) ──────
  // Wait until the layered worker result lands (nodes spread across >1 x AND >1 y).
  await expect
    .poll(async () => {
      const pts = await page.$$eval(".react-flow__node", (nodes) =>
        nodes.map((n) => {
          const t = getComputedStyle(n).transform;
          const m = /matrix\(([^)]+)\)/.exec(t);
          if (!m || !m[1]) return [0, 0] as [number, number];
          const p = m[1].split(",").map((s) => parseFloat(s.trim()));
          return [p[4] ?? 0, p[5] ?? 0] as [number, number];
        }),
      );
      const xs = new Set(pts.map((p) => Math.round(p[0])));
      const ys = new Set(pts.map((p) => Math.round(p[1])));
      return Math.min(xs.size, ys.size);
    }, { timeout: 8000 })
    .toBeGreaterThan(1);

  // ── the elk worker is kept ALIVE across re-layouts (the #493/#516 fix) ──────
  // The worker laid out the geometry above, so at least one was spawned. Capture
  // that count; toggling the axis re-runs elk.layout() on the SAME long-lived
  // worker (no terminate+respawn), so the count must NOT increase.
  const workersBefore = await page.evaluate(
    () => (window as unknown as { __elkWorkerCount?: number }).__elkWorkerCount ?? 0,
  );
  expect(workersBefore, "the elk worker laid out the first paint").toBeGreaterThan(0);

  // ── the 3-axis toggle swaps the rendered subgraph ─────────────────────────
  await expect(page.locator('[data-testid="axis-option"][data-axis="all"]')).toHaveAttribute("data-active", "true");
  const allNodes = await page.locator('[data-testid="graph-node"]').count();
  await page.locator('[data-testid="axis-option"][data-axis="unit_test"]').click();
  await expect(page.locator('[data-testid="axis-option"][data-axis="unit_test"]')).toHaveAttribute("data-active", "true");
  // the fixture's unit_test axis carries a different node count than `all`.
  await expect.poll(async () => page.locator('[data-testid="graph-node"]').count(), { timeout: 5000 }).not.toBe(allNodes);

  // The re-layout reused the live worker — the spawn count is unchanged. (Poll
  // briefly to let any stray async layout settle; it must never grow.)
  await expect
    .poll(
      async () =>
        page.evaluate(() => (window as unknown as { __elkWorkerCount?: number }).__elkWorkerCount ?? 0),
      { timeout: 3000 },
    )
    .toBe(workersBefore);

  // ── the prNode-vs-sel.models NAV SPLIT: clicking a model PR node sets prNode
  //    (the persisted PR cursor) WITHOUT changing the Models-entity selection ──
  await page.locator('[data-testid="axis-option"][data-axis="all"]').click();
  const modelNode = page.locator('[data-testid="graph-node"][data-kind="model"]').first();
  const clickedName = await modelNode.getAttribute("data-change"); // present iff a PR node
  expect(clickedName).not.toBeNull();
  await modelNode.click();
  await expect(modelNode).toHaveAttribute("data-selected", "true");
  // the Models-entity selection is UNTOUCHED (the split): go back to Models.
  await page.keyboard.press("2");
  await page.waitForSelector('[data-testid="model-list-item"]');
  const selectedAfter = await page
    .locator('[data-testid="model-list-item"][data-selected="true"]')
    .getAttribute("data-model");
  expect(selectedAfter, "clicking a PR node must NOT change the Models selection").toBe(selectedBefore);

  // ── zero external requests, zero page errors (local-first held throughout) ──
  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
  expect(consoleErrors, `page errors: ${consoleErrors.join(" | ")}`).toEqual([]);
});

test("unregistered theme fails loudly (no silent github-dark fallback)", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  const pageErrors: string[] = [];
  page.on("pageerror", (e) => pageErrors.push(e.message));

  // ?theme=monokai is NOT one of the 12 app themes -> raw shiki name -> unregistered
  // -> preloadHighlighter REJECTS at startup -> loud error banner, no app render.
  await page.goto("/index.html?theme=monokai", { waitUntil: "networkidle" });

  const banner = page.getByTestId("highlighter-error");
  await expect(banner).toBeVisible();
  await expect(banner).toContainText("monokai");

  const state = await page.evaluate(() => ({
    diffRendered: !!document.querySelector("diffs-container"),
    bgHasGithubDark: [...document.querySelectorAll("*")].some(
      (el) => getComputedStyle(el).backgroundColor === "rgb(36, 41, 46)",
    ),
    bodyErrAttr: document.body.getAttribute("data-highlighter-error"),
  }));

  expect(state.diffRendered, "no app/diff should render on the loud-fail path").toBe(false);
  expect(state.bgHasGithubDark, "no silent github-dark fallback").toBe(false);
  expect(state.bodyErrAttr).toContain("monokai");
  expect(pageErrors.some((m) => m.includes("monokai"))).toBe(true);
  expect(external, "zero external requests on the failure path").toEqual([]);
});
