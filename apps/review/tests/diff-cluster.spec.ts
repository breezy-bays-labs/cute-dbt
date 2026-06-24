// The S5 diff-cluster behavioral gate (RISK#2 — the @pierre/diffs shadow-DOM
// contract), made executable + network-denied. Drives the ?contract=diff harness
// (src/view/diff/DiffContractHarness) against a real Chromium with all external
// traffic DENIED, asserting the load-bearing Pierre invariants:
//   1. a Left/deletion comment anchors on the DELETIONS slot (a deletion row);
//   2. a Right/addition comment anchors on the ADDITIONS slot (an addition row);
//   3. the ACTIVE theme is the requested one (genuine tokyo-night, NOT a silent
//      github-dark fallback);
//   4. keyboard input inside the Pierre shadow root is GUARDED via composedPath —
//      a key dispatched from INSIDE the diff does NOT trigger the app's entity
//      dispatcher, while the SAME key from the light DOM DOES (the positive
//      control that proves the dispatcher is live — so this fails if the guard is
//      removed; the harness mounts the real useKeydown dispatcher + an
//      entity-reactive probe to make the side-effect observable);
//   5. the FIRST-PARTY fallback renders (a Pierre breakage degrades, never blank);
//   6. ZERO external (non-localhost) requests throughout.
import { test, expect, type Page } from "@playwright/test";

const TOKYO_NIGHT_BG = "rgb(26, 27, 38)";

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

test("RISK#2: Pierre side-slot anchoring + genuine theme + shadow keyboard guard + first-party fallback (network-denied)", async ({ page }) => {
  const external = await denyExternalNetwork(page);
  const pageErrors: string[] = [];
  page.on("pageerror", (e) => pageErrors.push("PAGEERROR: " + e.message));

  await page.goto("/index.html?contract=diff", { waitUntil: "networkidle" });
  await page.waitForSelector('[data-testid="diff-contract-harness"]');

  // both Pierre sections + the fallback section mounted.
  await page.waitForSelector('[data-testid="contract-pierre-left"] diffs-container');
  await page.waitForSelector('[data-testid="contract-pierre-right"] diffs-container');

  // ── (1)(2) side-slot anchoring (THE RISK#2 contract). Pierre projects each
  //    annotation through a NAMED shadow-DOM slot: `annotation-deletions-<line>`
  //    for a deletions-side comment, `annotation-additions-<line>` for an
  //    additions-side comment. The rendered CommentThread lives in the LIGHT DOM
  //    and is slotted into it. So: a Left comment MUST produce a deletions slot
  //    (and its thread carries data-thread-side="Left"); a Right comment MUST
  //    produce an additions slot. This proves the deletion-comment fix is live. ─
  const sideAnchor = await page.evaluate(() => {
    const probe = (sectionId: string): { lightSide: string | null; slots: string[] } => {
      const sect = document.querySelector(`[data-testid="${sectionId}"]`);
      const host = sect?.querySelector("diffs-container");
      const sr = (host as Element & { shadowRoot?: ShadowRoot } | null)?.shadowRoot;
      const slots = sr ? [...sr.querySelectorAll("slot")].map((s) => s.name).filter((n) => /annotation-(deletions|additions)/.test(n)) : [];
      // the projected thread sits in the LIGHT DOM under the section.
      const thread = sect?.querySelector('[data-testid="comment-thread"]') ?? null;
      return { lightSide: thread?.getAttribute("data-thread-side") ?? null, slots };
    };
    return { left: probe("contract-pierre-left"), right: probe("contract-pierre-right") };
  });

  // the Left comment opened a DELETIONS annotation slot…
  expect(sideAnchor.left.slots.join(","), "Left comment must anchor on the deletions slot").toContain("annotation-deletions");
  expect(sideAnchor.left.slots.join(","), "Left comment must NOT anchor on the additions slot").not.toContain("annotation-additions");
  // …and its projected thread is the Left (old/deletions) thread.
  expect(sideAnchor.left.lightSide).toBe("Left");

  // the Right comment opened an ADDITIONS annotation slot.
  expect(sideAnchor.right.slots.join(","), "Right comment must anchor on the additions slot").toContain("annotation-additions");
  expect(sideAnchor.right.slots.join(","), "Right comment must NOT anchor on the deletions slot").not.toContain("annotation-deletions");
  expect(sideAnchor.right.lightSide).toBe("Right");

  // ── (3) genuine tokyo-night, NOT github-dark fallback ──────────────────────
  const theme = await page.evaluate(
    ({ tn }) => {
      let bgMatch = false;
      let anyGithubDark = false;
      for (const host of document.querySelectorAll("diffs-container")) {
        const scope = (host as Element & { shadowRoot?: ShadowRoot }).shadowRoot ?? host;
        for (const el of scope.querySelectorAll<HTMLElement>("*")) {
          const bg = getComputedStyle(el).backgroundColor;
          if (bg === tn) bgMatch = true;
          if (bg === "rgb(36, 41, 46)") anyGithubDark = true;
        }
      }
      return { bgMatch, anyGithubDark };
    },
    { tn: TOKYO_NIGHT_BG },
  );
  expect(theme.bgMatch, "genuine tokyo-night background present in the Pierre diff").toBe(true);
  expect(theme.anyGithubDark, "no silent github-dark fallback").toBe(false);
  expect(await page.getByTestId("highlighter-error").count()).toBe(0);

  // ── (4) shadow keyboard guard via composedPath — exercised against the REAL
  //    dispatcher this harness mounts (`useKeydown`). The entity key "3" routes to
  //    set-entity("macros"); the entity-reactive probe ([data-entity]) is the
  //    dispatcher's observable side-effect. The contract: a "3" whose composedPath
  //    CROSSES the Pierre shadow host is GUARDED (entity unchanged), while a "3"
  //    from the LIGHT DOM is NOT (entity flips) — the positive control proving the
  //    dispatcher is live, so this whole rung FAILS if the guard is removed
  //    (the in-shadow "3" would then flip the entity too). ─────────────────────
  const probe = page.getByTestId("contract-entity-probe");
  await expect(probe).toHaveAttribute("data-entity", "models"); // dispatcher baseline

  // (4a) GUARDED: dispatch "3" from inside the Pierre shadow root → NO route.
  await page.evaluate(() => {
    const host = document.querySelector('[data-testid="contract-pierre-left"] diffs-container');
    const sr = (host as Element & { shadowRoot?: ShadowRoot } | null)?.shadowRoot;
    const target = sr?.querySelector<HTMLElement>("[data-line]") ?? null;
    if (!target) throw new Error("contract precondition: no [data-line] node inside the Pierre shadow root");
    // dispatch a keydown whose composedPath crosses the shadow host.
    const ev = new KeyboardEvent("keydown", { key: "3", bubbles: true, composed: true, cancelable: true });
    target.dispatchEvent(ev);
  });
  // the guard suppressed it — the dispatcher did NOT set-entity("macros").
  await expect(probe).toHaveAttribute("data-entity", "models");
  // the harness is still the mounted surface (the dispatcher did not navigate away).
  await expect(page.getByTestId("diff-contract-harness")).toBeVisible();

  // (4b) POSITIVE CONTROL: the SAME "3" from the LIGHT DOM IS routed → entity
  //      flips to "macros". This proves the dispatcher is live + reactive, so
  //      (4a)'s "unchanged" assertion is load-bearing (it would break with the
  //      guard removed). composedPath here crosses no shadow host / editable.
  await page.evaluate(() => {
    const ev = new KeyboardEvent("keydown", { key: "3", bubbles: true, composed: true, cancelable: true });
    document.body.dispatchEvent(ev);
  });
  await expect(probe).toHaveAttribute("data-entity", "macros");

  // ── (5) first-party fallback rendered (Pierre forced down) ─────────────────
  await expect(page.locator('[data-testid="contract-fallback"] [data-testid="fallback-diff"]')).toBeVisible();
  await expect(page.locator('[data-testid="contract-fallback"]').getByText("fallback renderer")).toBeVisible();
  // the fallback mounts BOTH threads (Left + Right) on their lines.
  expect(await page.locator('[data-testid="contract-fallback"] [data-testid="comment-thread"]').count()).toBeGreaterThanOrEqual(1);

  // ── (6) zero external requests, zero page errors ───────────────────────────
  expect(external, `external requests: ${external.join(", ")}`).toEqual([]);
  expect(pageErrors, `page errors: ${pageErrors.join(" | ")}`).toEqual([]);
});
