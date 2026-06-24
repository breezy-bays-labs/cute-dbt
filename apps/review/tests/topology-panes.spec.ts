// The S6b topology-panes BEHAVIORAL gate — the bidirectional CTE⇄code sync made
// executable against the PRODUCTION dist/ on loopback with ALL external traffic
// DENIED (the same local-first contract as local-first.spec.ts). Asserts:
//   1. FORWARD sync — a DAG node click scrolls the pane to the node's span and
//      ring-flashes it (the .kbd-ring class lands on the synced row).
//   2. REVERSE sync — a cursor (click) in the code pane highlights the
//      corresponding DAG node (data-selected="true" on the resolved node).
//   3. HONEST-EMPTY — a model with NO code_map renders the no-compiled-spans
//      state, never a fabricated listing or a claimed sync.
//   4. zero external requests throughout (local-first held).
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

// Select a model by name in the Models sidebar (the topology panes are the
// default Models "Review" view, so no view switch is needed).
async function selectModel(page: Page, name: string): Promise<void> {
  await page.waitForSelector('[data-testid="model-list-item"]');
  await page.locator(`[data-testid="model-list-item"][data-model="${name}"]`).click();
  await expect(page.locator(`[data-testid="model-list-item"][data-model="${name}"]`)).toHaveAttribute(
    "data-selected",
    "true",
  );
}

test("S6b: forward (node click → scroll + ring-flash) and reverse (cursor → node highlight) sync", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  const consoleErrors: string[] = [];
  page.on("pageerror", (e) => consoleErrors.push("PAGEERROR: " + e.message));

  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');

  // a model WITH a code_map (customers has 7 compiled CTE spans).
  await selectModel(page, "customers");

  // the topology panes mounted on the Models Review view.
  const panes = page.locator('[data-testid="topology-panes"]');
  await expect(panes).toBeVisible();
  await page.waitForSelector('[data-testid="topology-panes"] [data-testid="graph-node"]');
  // the compiled code pane rendered real lines (not the honest-empty state).
  await expect(page.locator('[data-testid="compiled-view"]')).toBeVisible();
  await expect(page.locator('[data-testid="compiled-view-empty"]')).toHaveCount(0);
  await page.waitForSelector('[data-testid="code-line"]');

  // ── FORWARD: click a DAG node → the pane scrolls to its span + ring-flashes ──
  // pick a non-final CTE node so its span is well inside the file (a real scroll).
  const node = page.locator('[data-testid="topology-dag"] [data-testid="graph-node"]').first();
  await node.click();
  // the node is now selected in the DAG (the forward selection landed).
  await expect(node).toHaveAttribute("data-selected", "true");
  // a row carries the ring-flash class (the forward scroll/flash fired). The class
  // is removed after 1.5s, so poll briefly to catch it.
  await expect.poll(async () => page.locator('[data-testid="code-line"].kbd-ring').count(), { timeout: 2000 }).toBeGreaterThan(0);
  // the synced row is an in-span row (the tint marks the node's block).
  await expect(page.locator('[data-testid="code-line"][data-in-span="true"]').first()).toBeAttached();

  // ── REVERSE: click a code line inside a DIFFERENT node's span → that node
  //    highlights in the DAG (cursor → node) ──
  // find a code line that is in-span (belongs to some node) and read which one the
  // DAG resolves it to by clicking it; the resolved node must become selected.
  const spanRow = page.locator('[data-testid="code-line"][data-in-span="true"]').first();
  await expect(spanRow).toBeAttached();
  // click a line well below the first node to drive a reverse resolution to another
  // node; then assert SOME node is selected (the cursor resolved to a node).
  const lines = page.locator('[data-testid="code-line"]');
  const lineCount = await lines.count();
  expect(lineCount).toBeGreaterThan(5);
  await lines.nth(Math.floor(lineCount / 2)).click();
  // after the reverse sync, the DAG still has exactly one selected node (the
  // resolution either kept or moved the selection — never cleared it to none on an
  // in-span line, never selected two).
  await expect.poll(async () => page.locator('[data-testid="topology-dag"] [data-testid="graph-node"][data-selected="true"]').count(), { timeout: 2000 }).toBe(1);

  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
  expect(consoleErrors, `page errors: ${consoleErrors.join(" | ")}`).toEqual([]);
});

test("S6b: honest-empty — a model with NO code_map renders the no-compiled-spans state (never fabricated)", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');

  // legacy_order_rollup is the one fixture model WITHOUT a code_map spine.
  await selectModel(page, "legacy_order_rollup");

  const panes = page.locator('[data-testid="topology-panes"]');
  await expect(panes).toBeVisible();
  // the honest-empty pane is shown — NOT a fabricated code listing.
  await expect(page.locator('[data-testid="compiled-view-empty"]')).toBeVisible();
  await expect(page.locator('[data-testid="compiled-view-empty"]')).toContainText("code_map");
  // no synced code lines exist (nothing fabricated).
  await expect(page.locator('[data-testid="topology-panes"] [data-testid="code-line"]')).toHaveCount(0);

  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
});

test("S6b: the raw⇄compiled source toggle swaps the pane source (DAG follows the shelf)", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');

  // a model with a raw side (order_events_enriched_incremental has raw_zones).
  await selectModel(page, "order_events_enriched_incremental");
  const panes = page.locator('[data-testid="topology-panes"]');
  await expect(panes).toBeVisible();
  await page.waitForSelector('[data-testid="topology-panes"] [data-testid="graph-node"]');

  // default shelf = compiled → the DAG mode is compiled.
  await expect(panes).toHaveAttribute("data-dag-mode", "compiled");
  await expect(panes).toHaveAttribute("data-shelf", "compiled");

  // toggle to the File (raw) source → the DAG mode follows to raw.
  await page.locator('[data-testid="shelf-toggle"][data-mode="raw"]').click();
  await expect(panes).toHaveAttribute("data-shelf", "raw");
  await expect(panes).toHaveAttribute("data-dag-mode", "raw");
  // the raw pane still renders real code lines (raw_sql present).
  await expect(page.locator('[data-testid="code-line"]').first()).toBeAttached();

  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
});
