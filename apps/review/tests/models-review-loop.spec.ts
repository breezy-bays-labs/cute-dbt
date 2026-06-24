// THE HEADLINE — the V1 flow-acceptance E2E (cute-dbt#495). The Definition of
// Done for "Models reviewable end-to-end" (council MUST-FIX D — "build the VERB").
//
// It drives the keyboard review LOOP against the PRODUCTION dist/ on loopback with
// ALL non-localhost traffic DENIED, asserting EACH step's real store/DOM side-
// effect (never a vacuous "it rendered"):
//   1. OPEN Models           → the Models entity is active (default), the REAL
//                              progress chip reads reviewed/total + open threads.
//   2. mark-reviewed-advance → `x` marks the current model reviewed AND advances
//                              to the next UNREVIEWED model, landing on the code
//                              review surface (the diff). Chip increments.
//   3. READ the diff         → the advanced model's review surface mounts a real
//                              changed-file diff.
//   4. COMMENT (draft)       → keyboard-typed into the composer + ⌘↵ → a pending
//                              draft lands in the review slice (persisted), the
//                              surface draft-count increments.
//   5. mark-viewed + advance → `x` again marks reviewed + advances (the loop).
//   6. next-unreviewed       → `N` jumps to the next unreviewed model (skipping
//                              reviewed), staying on the code surface.
//   7. write-review (export) → `w` opens the portable write-review surface; it
//                              carries a runnable gh command + the copy-JSON with
//                              the drafted comment — and NEVER posts (zero egress).
//   8. ZERO external requests throughout (the local-first invariant).
//
// All assertions read the REAL state: the DOM testids + the `cute-dbt:review`
// persist blob (`state.review`). This is the executable proof the review VERB
// works keyboard-only, not merely that the surfaces render.
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

/** Read the persisted review state (the `cute-dbt:review` blob's `state.review`). */
async function reviewState(page: Page): Promise<{
  reviewed: Record<string, true>;
  pending: Record<string, { path: string; line: number; side: string; body: string }[]>;
  selModels: string | null;
  viewModels: string;
}> {
  return page.evaluate(() => {
    const blob = JSON.parse(localStorage.getItem("cute-dbt:review") || "{}") as {
      state?: { review?: { reviewed?: Record<string, true>; pending?: Record<string, unknown[]> }; sel?: { models?: string }; viewByEntity?: { models?: string } };
    };
    const st = blob.state ?? {};
    return {
      reviewed: (st.review?.reviewed ?? {}) as Record<string, true>,
      pending: (st.review?.pending ?? {}) as Record<string, { path: string; line: number; side: string; body: string }[]>,
      selModels: st.sel?.models ?? null,
      viewModels: st.viewByEntity?.models ?? "",
    };
  });
}

test("V1 flow-acceptance: Models reviewable end-to-end, KEYBOARD-ONLY (network-denied)", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  const pageErrors: string[] = [];
  page.on("pageerror", (e) => pageErrors.push("PAGEERROR: " + e.message));

  await page.goto("/index.html", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="entity-tabs"]');

  // ── STEP 1: OPEN Models — the REAL progress chip (never fabricated). ────────
  await expect(page.locator('[data-testid="tab-models"]')).toHaveAttribute("data-active", "true");
  const chip = page.getByTestId("review-progress");
  // the chip reads "✓ R/T …" with the REAL in-scope total (the dogfood fixture
  // has > 1 in-scope model). Capture R/T to assert the increment is real.
  const chip0 = (await chip.textContent()) ?? "";
  const total = Number(/✓\s*\d+\/(\d+)/.exec(chip0)?.[1] ?? "0");
  expect(total, `progress chip total parsed from "${chip0}"`).toBeGreaterThan(2);
  expect(chip0).toMatch(/✓\s*0\//); // nothing reviewed yet (honest zero)

  const before = await reviewState(page);
  expect(Object.keys(before.reviewed)).toHaveLength(0);
  const firstModel = before.selModels;
  expect(firstModel, "a default model is selected").toBeTruthy();

  // ── STEP 2: mark-reviewed-advance (`x`) — marks reviewed AND advances. ──────
  await page.keyboard.press("x");
  await expect(page.getByTestId("model-review-surface")).toBeVisible();
  // the chip incremented to ✓ 1/T (REAL — read back, not assumed).
  await expect(chip).toHaveText(new RegExp(`✓\\s*1/${total}`));
  const afterX = await reviewState(page);
  expect(afterX.reviewed[firstModel!], "the first model is now reviewed").toBe(true);
  expect(afterX.selModels, "advanced off the just-reviewed model").not.toBe(firstModel);
  expect(afterX.viewModels, "landed on the code review surface").toBe("code");
  const secondModel = afterX.selModels!;
  // the advanced model is UNREVIEWED (the loop never lands on a reviewed one).
  expect(afterX.reviewed[secondModel]).toBeUndefined();

  // ── STEP 3: READ the diff — the surface mounts a real changed-file diff. ────
  const surface = page.getByTestId("model-review-surface");
  await expect(surface).toHaveAttribute("data-model", secondModel);
  await expect(surface.getByTestId("review-file").first()).toBeVisible();
  // the unreviewed state chip is honest (this model is not yet reviewed).
  await expect(surface.getByTestId("review-state-chip")).toHaveAttribute("data-reviewed", "false");

  // ── STEP 4: COMMENT (draft) — keyboard-typed into the composer + ⌘↵. ────────
  const textarea = surface.locator('[data-testid="review-draft-composer"] [data-testid="composer-textarea"]');
  await textarea.focus();
  await textarea.pressSequentially("alias this CTE for readability");
  await page.keyboard.press("Meta+Enter"); // the composer's ⌘↵ submit
  // a pending draft landed in the review slice (persisted), anchored to a real line.
  await expect(surface).toHaveAttribute("data-draft-count", "1");
  const afterDraft = await reviewState(page);
  const drafts = afterDraft.pending[secondModel] ?? [];
  expect(drafts, "one pending draft on the active model").toHaveLength(1);
  expect(drafts[0]!.body).toBe("alias this CTE for readability");
  expect(drafts[0]!.line, "anchored to a real change-run line (>= 1)").toBeGreaterThanOrEqual(1);
  // the composer cleared + blurred → the keyboard loop has the keyboard back.
  await expect(textarea).toHaveValue("");

  // ── STEP 5: mark-viewed + advance (`x`) — the loop continues. ───────────────
  await page.keyboard.press("x");
  await expect(chip).toHaveText(new RegExp(`✓\\s*2/${total}`));
  const afterX2 = await reviewState(page);
  expect(afterX2.reviewed[secondModel], "the second model is now reviewed").toBe(true);
  expect(afterX2.selModels, "advanced again").not.toBe(secondModel);
  // the draft survives the advance (it's per-model pending, not cleared on advance).
  expect((afterX2.pending[secondModel] ?? []).length, "the draft persists past the advance").toBe(1);

  // ── STEP 6: next-unreviewed (`N`) — jump to the next unreviewed model. ──────
  const beforeN = (await reviewState(page)).selModels;
  await page.keyboard.press("N");
  const afterN = await reviewState(page);
  expect(afterN.selModels, "N jumped to a different model").not.toBe(beforeN);
  expect(afterN.reviewed[afterN.selModels!], "…that is unreviewed").toBeUndefined();
  expect(afterN.viewModels, "still on the code review surface").toBe("code");

  // ── STEP 7: write-review (`w`) — the PORTABLE export (NEVER posts). ─────────
  await page.keyboard.press("w");
  const wr = page.getByTestId("write-review");
  await expect(wr).toBeVisible();
  // a runnable gh command targeting the real repo + PR (parsed from the context).
  const ghCmd = (await wr.getByTestId("write-review-gh-command").textContent()) ?? "";
  expect(ghCmd).toMatch(/^gh api repos\/[^/]+\/[^/]+\/pulls\/\d+\/reviews --method POST --input -$/);
  // the copy-JSON carries the drafted comment (the host runs it; cute-dbt does not).
  const json = await wr.getByTestId("write-review-json").inputValue();
  const parsed = JSON.parse(json) as { comments: { body: string }[]; event: string };
  expect(parsed.comments.some((c) => c.body === "alias this CTE for readability"), "the draft is in the payload").toBe(true);
  // the honesty note: cute-dbt NEVER posts.
  expect(((await wr.getByTestId("write-review-note").textContent()) ?? "").toLowerCase()).toContain("never");
  // Esc dismisses the overlay.
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("write-review")).toHaveCount(0);

  // ── STEP 8: ZERO external requests, zero page errors (local-first). ─────────
  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
  expect(pageErrors, `page errors: ${pageErrors.join(" | ")}`).toEqual([]);
});
