// The PR page E2E (S9 / cute-dbt#501) — drives the PR entity's Overview / Files /
// Timeline surfaces against the PRODUCTION dist/ on loopback with ALL non-localhost
// traffic DENIED, asserting the HONESTY contract on real data:
//   1. OVERVIEW renders the REAL pr_ref (number/title/url) + the change-count
//      summary derived from the PR-scope DAG + the removed model.
//   2. FILES lists the changed models navigably + the removed model non-navigably;
//      clicking a file routes to that model's review surface.
//   3. TIMELINE shows the real per-model + unanchored comment threads, AND the
//      commit/review/CI feed renders an HONEST "not in this context" state — no
//      fabricated commit, review, check, or timestamp (the tracked T2 spine gap).
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

test("PR page: overview/files/timeline render REAL context, feed honest-empty, zero egress", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  const pageErrors: string[] = [];
  page.on("pageerror", (e) => pageErrors.push("PAGEERROR: " + e.message));

  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');

  // ── jump to the PR entity (number-row `1`) → its default view (Overview). ──
  await page.keyboard.press("1");
  await expect(page.locator('[data-testid="tab-pr"]')).toHaveAttribute("data-active", "true");

  // ── OVERVIEW — the REAL pr_ref identity + change-count summary. ─────────────
  const overview = page.getByTestId("view-pr-overview");
  await expect(overview).toBeVisible();
  await expect(overview.getByTestId("pr-title")).toContainText("PR #440");
  await expect(overview.getByTestId("pr-title")).toContainText("dogfood");
  // the number link points at the REAL PR url (and opens in a new tab — never an
  // in-app fetch; it's an href, not a network call).
  await expect(overview.getByTestId("pr-number-link")).toHaveAttribute(
    "href",
    "https://github.com/breezy-bays-labs/cute-dbt/pull/440",
  );
  // the change summary reads the REAL DAG-derived counts (440: 11 modified, etc.).
  const modified = Number(
    (await overview.getByTestId("count-modified").getAttribute("data-count")) ?? "0",
  );
  expect(modified, "the real modified-model count").toBeGreaterThan(0);
  const changed = Number(
    (await overview.getByTestId("count-changed").getAttribute("data-count")) ?? "0",
  );
  expect(changed, "changed >= modified").toBeGreaterThanOrEqual(modified);
  // the removed model surfaces distinctly (the node-less removed_models list).
  await expect(overview.getByTestId("pr-removed-models")).toContainText("orders_never_refunded.sql");

  // ── FILES (⇧3 — third positional view) — navigable changed files. ──────────
  await page.keyboard.press("Shift+Digit3");
  const files = page.getByTestId("view-pr-files");
  await expect(files).toBeVisible();
  // a real changed model row is present + navigable; the removed model is NOT.
  const navRow = files.locator('[data-testid="pr-file-row"][data-navigable="true"]').first();
  await expect(navRow).toBeVisible();
  await expect(
    files.locator('[data-testid="pr-file-row"][data-navigable="false"]'),
  ).toContainText("orders_never_refunded.sql");
  // per-file comment counts render (the dogfood has commented files).
  await expect(files.getByTestId("pr-file-threads").first()).toBeVisible();

  // clicking a navigable file routes OUT to that model's review surface (Models).
  const targetModel = await navRow.getAttribute("data-file");
  await navRow.getByTestId("pr-file-open").click();
  await expect(page.locator('[data-testid="tab-models"]')).toHaveAttribute("data-active", "true");
  await expect(page.getByTestId("model-review-surface")).toHaveAttribute("data-model", targetModel!);

  // ── TIMELINE (back to PR, ⇧4 — fourth positional view). ────────────────────
  await page.keyboard.press("1");
  await page.keyboard.press("Shift+Digit4");
  const timeline = page.getByTestId("view-pr-timeline");
  await expect(timeline).toBeVisible();

  // the REAL comment threads render with real authors + bodies (never invented).
  await expect(timeline.getByTestId("timeline-thread").first()).toBeVisible();
  await expect(timeline).toContainText("dogfood-dev");
  // a resolved thread carries the honest resolved badge (the dogfood has one).
  await expect(timeline.getByTestId("thread-resolved").first()).toBeVisible();
  // the unanchored (file-level / project) group is present + honestly labeled.
  await expect(timeline.getByTestId("timeline-group-unanchored")).toBeVisible();

  // ── the HONEST feed state — NOT present, the spine-gap note, NO fabrication. ─
  const feed = timeline.getByTestId("pr-timeline-feed");
  await expect(feed).toHaveAttribute("data-present", "false");
  await expect(feed.getByTestId("feed-spine-gap")).toContainText("not in this context");
  // the 440 context carries no CI-check summary → honest-null, never "all passing".
  await expect(feed.getByTestId("feed-no-checks")).toBeVisible();
  // there is NO fabricated commit/review/check event anywhere on the page.
  expect(await page.locator('[data-testid="timeline-commit-event"]').count()).toBe(0);
  expect(await page.locator('[data-testid="timeline-check-event"]').count()).toBe(0);

  // ── ZERO external requests, zero page errors (local-first). ────────────────
  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
  expect(pageErrors, `page errors: ${pageErrors.join(" | ")}`).toEqual([]);
});
