// S7 (cute-dbt#499) — the Models · Details + Models · Unit-tests views, asserted
// against the PRODUCTION dist/ on loopback with ALL non-localhost traffic DENIED
// (the local-first invariant). Proves the acceptance criteria with REAL DOM:
//   - Details (models-node): the real config facts render (materialization +
//     node-config + lineage summary) for the selected model.
//   - Unit tests (models-data): a model WITH tests renders the given/expect grid
//     + the cell-level diffs; a model WITHOUT renders the honest no-unit-tests
//     empty state (never a fabricated grid).
//   - keyboard-navigable: the ⇧digit positional view keys route to both views.
//   - ZERO external requests throughout.
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

async function selectModel(page: Page, name: string): Promise<void> {
  await page.waitForSelector('[data-testid="model-list-item"]');
  await page.locator(`[data-testid="model-list-item"][data-model="${name}"]`).click();
  await expect(page.locator(`[data-testid="model-list-item"][data-model="${name}"]`)).toHaveAttribute(
    "data-selected",
    "true",
  );
}

test("S7: Models · Details + Unit-tests views render honest facts (network-denied)", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  const pageErrors: string[] = [];
  page.on("pageerror", (e) => pageErrors.push("PAGEERROR: " + e.message));

  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');
  await expect(page.locator('[data-testid="tab-models"]')).toHaveAttribute("data-active", "true");

  // ── DETAILS view (⇧2 = the second Models view, `node`) ──────────────────────
  await selectModel(page, "customers");
  await page.keyboard.press("Shift+Digit2");
  await expect(page.locator('[data-testid="tab-node"]')).toHaveAttribute("data-active", "true");

  const details = page.getByTestId("model-details");
  await expect(details).toBeVisible();
  await expect(details).toHaveAttribute("data-model", "customers");
  // the REAL config facts: the change badge, the materialization chip, the node
  // config table, and the lineage summary all render.
  await expect(page.getByTestId("model-state-badge")).toBeVisible();
  await expect(page.getByTestId("model-materialization")).toBeVisible();
  await expect(page.getByTestId("node-config")).toBeVisible();
  await expect(page.getByTestId("lineage-summary")).toBeVisible();
  // customers documents columns → the columns table mounts (not honest-empty).
  await expect(page.getByTestId("columns-table")).toBeVisible();

  // ── UNIT-TESTS view (⇧3 = the third Models view, `data`) — model WITH tests ──
  await page.keyboard.press("Shift+Digit3");
  await expect(page.locator('[data-testid="tab-data"]')).toHaveAttribute("data-active", "true");

  const ut = page.getByTestId("model-unit-tests");
  await expect(ut).toBeVisible();
  await expect(ut).toHaveAttribute("data-model", "customers");
  // a model WITH a unit test renders the given + expect grids.
  await expect(page.getByTestId("ut-given")).toBeVisible();
  await expect(page.getByTestId("ut-expect")).toBeVisible();
  await expect(page.getByTestId("ut-test-select")).toBeVisible();
  // the cell-level diff treatment: at least one diff row mounts (a changed test).
  await expect(page.getByTestId("cell-diff-row").first()).toBeVisible();

  // ── honest no-unit-tests empty state: customer_order_days ships none ────────
  // (the external-fixture honest note is covered by the ModelUnitTests unit test —
  // the only external-fixture model, `orders`, is a PR-scope connector, not a
  // sidebar-reviewable model.)
  await selectModel(page, "customer_order_days");
  await expect(page.getByTestId("model-unit-tests")).toHaveAttribute("data-model", "customer_order_days");
  await expect(page.getByTestId("ut-empty")).toBeVisible();
  // no given/expect grids fabricated for a test-less model.
  await expect(page.getByTestId("ut-given")).toHaveCount(0);
  await expect(page.getByTestId("ut-expect")).toHaveCount(0);

  // ── the Details view also shows real config facts for the incremental model ──
  await page.keyboard.press("Shift+Digit2");
  await expect(page.locator('[data-testid="tab-node"]')).toHaveAttribute("data-active", "true");
  await expect(page.getByTestId("model-details")).toHaveAttribute("data-model", "customer_order_days");
  // customer_order_days is incremental → the materialization chip reads incremental.
  await expect(page.getByTestId("model-materialization")).toContainText("incremental");

  // ── ZERO external requests + no page errors throughout ──────────────────────
  expect(external, `external requests: ${external.join(", ")}`).toHaveLength(0);
  expect(pageErrors, `page errors: ${pageErrors.join(", ")}`).toHaveLength(0);
});
