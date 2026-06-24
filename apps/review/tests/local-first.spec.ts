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
