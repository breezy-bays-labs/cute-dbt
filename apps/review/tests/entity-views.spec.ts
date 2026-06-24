// The entity-views E2E (S8 / cute-dbt#500) — drives the Macros / Seeds / Else
// (sources + tests) review surfaces against the PRODUCTION dist/ on loopback with
// ALL non-localhost traffic DENIED, asserting the HONESTY contract on real data:
//   1. MACROS (entity key `3`) renders a REAL macro card from `macro_lens` — its
//      signature, package/path, and its REAL call-site usages (impacted models).
//   2. SEEDS (entity key `4`) renders the REAL seed card from `seed_cards` — its
//      columns + the downstream models it feeds.
//   3. ELSE (entity key `5`) renders the REAL per-column TEST inventory from
//      `manifest_nodes` AND an HONEST-EMPTY sources panel (the 440 spine carries no
//      discrete source node — no fabricated table identity).
//   4. ZERO external requests throughout (the local-first invariant).
import { test, expect, type Page } from "@playwright/test";

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

test("entity views: macros/seeds/else render REAL context honestly, zero egress", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  const pageErrors: string[] = [];
  page.on("pageerror", (e) => pageErrors.push("PAGEERROR: " + e.message));

  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');

  // ── MACROS (number-row `3`) → the Macros review surface. ────────────────────
  await page.keyboard.press("3");
  await expect(page.locator('[data-testid="tab-macros"]')).toHaveAttribute("data-active", "true");
  const macro = page.getByTestId("entity-macro");
  await expect(macro).toBeVisible();
  // a REAL macro renders with its signature + package + path (never fabricated).
  await expect(macro.getByTestId("macro-signature")).toBeVisible();
  await expect(macro.getByTestId("macro-package")).toContainText("jaffle_shop");
  await expect(macro.getByTestId("entity-path")).toContainText("macros/");
  // the macro source body renders verbatim.
  await expect(macro.getByTestId("macro-body")).toContainText("macro");

  // select a macro the dogfood proves has REAL call-site usages.
  await page.getByTestId("instance-select").selectOption("incremental_high_water_mark");
  await expect(page.getByTestId("entity-macro")).toHaveAttribute("data-macro", "incremental_high_water_mark");
  const usage = page.getByTestId("macro-usage").first();
  await expect(usage).toBeVisible();
  await expect(usage).toContainText("models/marts/");

  // ── SEEDS (number-row `4`) → the Seeds review surface. ──────────────────────
  await page.keyboard.press("4");
  await expect(page.locator('[data-testid="tab-seeds"]')).toHaveAttribute("data-active", "true");
  const seed = page.getByTestId("entity-seed");
  await expect(seed).toBeVisible();
  await expect(seed).toHaveAttribute("data-seed", "raw_payments");
  // REAL columns render.
  await expect(seed.getByTestId("seed-column").first()).toBeVisible();
  await expect(seed).toContainText("payment_method");
  // the downstream feeds the REAL models from feeds_models.
  await expect(seed.getByTestId("seed-downstream")).toContainText("stg_payments");

  // ── ELSE (number-row `5`) → the Sources + Tests project surface. ────────────
  await page.keyboard.press("5");
  await expect(page.locator('[data-testid="tab-else"]')).toHaveAttribute("data-active", "true");
  const els = page.getByTestId("entity-else");
  await expect(els).toBeVisible();
  // the REAL test inventory from manifest_nodes (the 440 dogfood carries 28).
  const inv = els.getByTestId("test-inventory");
  await expect(inv).toHaveAttribute("data-total", "28");
  await expect(inv.getByTestId("test-entry").first()).toBeVisible();
  await expect(inv).toContainText("unique");
  // the SOURCES panel is HONESTLY EMPTY — no fabricated table identity.
  const sources = els.getByTestId("sources-panel");
  await expect(sources.getByTestId("sources-empty")).toBeVisible();
  expect(await els.locator('[data-testid="source-node"]').count()).toBe(0);

  // ── ZERO external requests, zero page errors (local-first). ────────────────
  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
  expect(pageErrors, `page errors: ${pageErrors.join(" | ")}`).toEqual([]);
});
