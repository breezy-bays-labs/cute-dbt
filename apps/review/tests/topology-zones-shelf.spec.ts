// The S6c zone-regions + DetailShelf BEHAVIORAL gate — the concentric-ring
// overlay, the click-through fill, the fan-out collapse, the honest 3-state
// presence treatment, and the first-party resizable/dockable detail shelf, made
// executable against the PRODUCTION dist/ on loopback with ALL external traffic
// DENIED (the same local-first contract as local-first.spec.ts). Asserts:
//   1. ZONE RING CLICK-THROUGH — a click on a concentric-ring region lands via
//      pointer-events (the click-through fill routes the ring's selection), and
//      the ring renders the fan-out collapse (templated zone:N nodes).
//   2. COMPILED_OUT EXPLAINER — an incremental-only (compiled_out) {% for %}
//      renders the honest incremental-only explainer (names is_incremental,
//      never a fabricated body) — the never-a-false-claim 3-state.
//   3. SHELF — the first-party detail shelf resizes (keyboard) + docks + goes
//      fullscreen, WITHOUT regressing the cursor-sync.
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

async function selectModel(page: Page, name: string): Promise<void> {
  await page.waitForSelector('[data-testid="model-list-item"]');
  await page.locator(`[data-testid="model-list-item"][data-model="${name}"]`).click();
  await expect(page.locator(`[data-testid="model-list-item"][data-model="${name}"]`)).toHaveAttribute(
    "data-selected",
    "true",
  );
}

test("S6c: zone ring click-through + fan-out collapse (the real looped model)", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  const consoleErrors: string[] = [];
  page.on("pageerror", (e) => consoleErrors.push("PAGEERROR: " + e.message));

  // The ?contract=topology harness mounts the real looped model (order_status_pivot
  // — 3 {% for %} loops incl. a NESTED loop → concentric rings + a fan-out collapse
  // into templated zone:N nodes). The looped fixtures are out of the sidebar's PR
  // scope (cute-dbt#523), so the LIVE ring click-through is driven here, not via the
  // sidebar. The geometry/fold is also unit-covered (zone-regions.test.tsx).
  await page.goto("/index.html?contract=topology", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="topology-contract-harness"]');

  const panes = page.locator('[data-testid="topology-panes"]');
  await expect(panes).toBeVisible();
  await page.waitForSelector('[data-testid="topology-panes"] [data-testid="graph-node"]');

  // switch to the File (raw) shelf via the shelf-mode segmented → the DAG becomes
  // the RAW graph (where the {% for %} fan-out is collapsed + the rings render).
  await page.locator('[data-testid="shelf-toggle"][data-mode="raw"]').click();
  await expect(panes).toHaveAttribute("data-dag-mode", "raw");
  await page.waitForSelector('[data-testid="topology-dag"] [data-testid="graph-node"]');

  // ── FAN-OUT COLLAPSE: the generating loops fold into templated zone:N nodes
  //    (order_status_pivot collapses TWO generating loops → ≥2 templated nodes,
  //    each carrying the TEMPLATE treatment, NEVER the incremental-amber one). ──
  const templated = page.locator('[data-testid="topology-dag"] [data-testid="graph-node"][data-templated="true"]');
  await expect(templated.first()).toBeAttached();
  expect(await templated.count()).toBeGreaterThanOrEqual(2);

  // ── CONCENTRIC RINGS: the zone overlay drew selectable ring regions (the
  //    nested for-loops render as concentric rings). ──
  const rings = page.locator('[data-testid="zone-overlay"] [data-testid="zone-ring"]');
  await expect(rings.first()).toBeAttached();
  // the nested loops → ≥2 concentric rings (zones-in-zones).
  await expect.poll(async () => rings.count(), { timeout: 4000 }).toBeGreaterThanOrEqual(2);

  // ── CLICK-THROUGH: a click on a ring's legend pill lands via pointer-events and
  //    SELECTS that ring. The fill is pointer-events:none (click-through), so the
  //    legend/frame is the click target — proving the click-through fill routes the
  //    pick to the intended ring instead of being swallowed by the fill or the pan. ──
  const ring = rings.first();
  const zoneId = await ring.getAttribute("data-zone");
  // the click-through is REAL: the ring FILL carries pointer-events:none (so a
  // click on it falls THROUGH to the nodes/pan beneath), while the legend pill is
  // the click target (pointer-events:all) — the exact prototype contract. Assert
  // the fill is click-through FIRST, then prove the legend pick still lands.
  const fillPe = await ring.locator("rect").first().evaluate((el) => getComputedStyle(el).pointerEvents);
  expect(fillPe).toBe("none");
  // click the legend pill (pointer-events:all) → the click-through fill routes the
  // pick to THIS ring (it never swallows it, and the pan never steals it).
  await ring.locator('[data-testid="zone-legend-hit"]').click();
  await expect.poll(
    async () => page.locator(`[data-testid="zone-ring"][data-zone="${zoneId}"]`).getAttribute("data-selected"),
    { timeout: 2000 },
  ).toBe("true");

  // ── NEVER-A-FALSE-CLAIM: order_status_pivot's outer `for region` loop is
  //    presence "compiled_in" but generated NO CTE of its own (the nested loop
  //    does) — node_map.raw["zone:1"] = []. Its zone-presence card must render the
  //    honest WRAPPER treatment, NEVER a purple "templated · 0 CTEs" fan-out chip
  //    sitting next to a body that denies any CTE. The harness mounts the full
  //    ZonePresenceList, so this contradiction is reachable live. ──
  const presenceList = page.locator('[data-testid="zone-presence-list"]');
  await expect(presenceList).toBeAttached();
  // a 0-CTE compiled_in chip must never claim "templated · 0 CTEs".
  await expect(presenceList.getByText(/templated · 0 CTEs/)).toHaveCount(0);
  // the compiled_in card that DID generate names its real fan-out count instead.
  await expect(presenceList.locator('[data-testid="zone-presence"][data-presence="compiled_in"]').first()).toBeAttached();

  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
  expect(consoleErrors, `page errors: ${consoleErrors.join(" | ")}`).toEqual([]);
});

test("S6c: the compiled_out incremental-only explainer (honest 3-state, never a fabricated body)", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');

  // order_events_enriched_incremental — a `for_loop:compiled_out` zone (a {% for %}
  // inside {% if is_incremental() %} that a fresh dbt compile strips). It is `new`
  // (in PR scope, reachable).
  await selectModel(page, "order_events_enriched_incremental");
  const panes = page.locator('[data-testid="topology-panes"]');
  await expect(panes).toBeVisible();

  // the honest 3-state zone-presence list renders in the shelf.
  await expect(page.locator('[data-testid="zone-presence-list"]')).toBeVisible();
  // a compiled_out zone surfaces the INCREMENTAL-ONLY treatment.
  const compiledOut = page.locator('[data-testid="zone-presence"][data-presence="compiled_out"]').first();
  await expect(compiledOut).toBeAttached();
  await expect(compiledOut).toHaveAttribute("data-incremental-only", "true");
  // the load-bearing honest explainer — names is_incremental, never a fake body.
  const explainer = compiledOut.locator('[data-testid="incremental-only-explainer"]');
  await expect(explainer).toBeVisible();
  await expect(explainer).toContainText(/is_incremental/i);
  await expect(explainer).toContainText(/compile|strip/i);

  // ── WRONG-KIND COPY: order_events_enriched_incremental ALSO carries a STRUCTURAL
  //    incremental_guard zone (raw_zones[1]). Its card must read as a GUARD region
  //    (naming is_incremental()), NEVER "A {% for %} wrapper region" — a for-loop
  //    description applied to an is_incremental guard is a copy fabrication. ──
  const structuralGuard = page.locator('[data-testid="zone-presence"][data-presence="structural"]').first();
  await expect(structuralGuard).toBeAttached();
  await expect(structuralGuard).toContainText(/is_incremental|guard/i);
  await expect(structuralGuard).not.toContainText(/\{% for %\} wrapper region/i);

  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
});

test("S6c: the first-party detail shelf resizes (keyboard) + docks + goes fullscreen (no cursor-sync regression)", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  const consoleErrors: string[] = [];
  page.on("pageerror", (e) => consoleErrors.push("PAGEERROR: " + e.message));

  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');

  // customers — has a code_map spine (the forward/reverse sync works here).
  await selectModel(page, "customers");
  const panes = page.locator('[data-testid="topology-panes"]');
  await expect(panes).toBeVisible();
  await page.waitForSelector('[data-testid="code-line"]');

  const shelf = page.locator('[data-testid="detail-shelf"]');
  await expect(shelf).toBeVisible();

  // ── KEYBOARD RESIZE: focus the role="separator" handle + arrow it; the
  //    aria-valuenow (the persisted size) must change. ──
  const handle = page.locator('[data-testid="shelf-resize"]');
  await expect(handle).toHaveAttribute("role", "separator");
  await handle.focus();
  const before = Number(await shelf.getAttribute("data-size"));
  // side dock: ArrowLeft GROWS the shelf (it docks on the right edge).
  await handle.press("ArrowLeft");
  await handle.press("ArrowLeft");
  await expect.poll(async () => Number(await shelf.getAttribute("data-size")), { timeout: 2000 }).toBeGreaterThan(before);

  // ── DOCK: toggle side → bottom; the panes + shelf reflect the new dock. ──
  await expect(panes).toHaveAttribute("data-dock", "side");
  await page.locator('[data-testid="shelf-dock"]').click();
  await expect(panes).toHaveAttribute("data-dock", "bottom");
  await expect(shelf).toHaveAttribute("data-dock", "bottom");
  // the resize handle re-orients horizontally when docked to the bottom.
  await expect(page.locator('[data-testid="shelf-resize"]')).toHaveAttribute("aria-orientation", "horizontal");

  // ── FULLSCREEN: the shelf goes full-bleed (the DAG hides, the handle hides). ──
  await page.locator('[data-testid="shelf-fullscreen"]').click();
  await expect(panes).toHaveAttribute("data-fullscreen", "true");
  await expect(page.locator('[data-testid="topology-dag"]')).toHaveCount(0);
  await expect(page.locator('[data-testid="shelf-resize"]')).toHaveCount(0);
  // exit fullscreen → the DAG returns.
  await page.locator('[data-testid="shelf-fullscreen"]').click();
  await expect(panes).toHaveAttribute("data-fullscreen", "false");
  await expect(page.locator('[data-testid="topology-dag"]')).toBeVisible();

  // ── PIN: pin the model-info panel; it appears + the button reflects state. ──
  await page.locator('[data-testid="shelf-pin"]').click();
  await expect(shelf).toHaveAttribute("data-pinned", "true");
  await expect(page.locator('[data-testid="shelf-info"]')).toBeVisible();

  // ── NO CURSOR-SYNC REGRESSION: a DAG node click still scrolls + ring-flashes
  //    the pane (the forward sync survived the shelf wrapping). ──
  const node = page.locator('[data-testid="topology-dag"] [data-testid="graph-node"]').first();
  await node.click();
  await expect(node).toHaveAttribute("data-selected", "true");
  await expect.poll(async () => page.locator('[data-testid="code-line"].kbd-ring').count(), { timeout: 2000 }).toBeGreaterThan(0);

  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
  expect(consoleErrors, `page errors: ${consoleErrors.join(" | ")}`).toEqual([]);
});
