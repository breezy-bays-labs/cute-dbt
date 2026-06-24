// The review-state machine unit tests (V1 / cute-dbt#495 — the keyboard review
// LOOP). The machine is a PURE module: a POD `ReviewState` + pure transition
// functions. No store, no React, no DOM — every behavior is exercised here at
// the lowest level (the Boundary Rule: this is verification, not a product-level
// BDD contract). The store slice (data layer) and the dispatcher (data layer)
// are thin shells over these functions.
//
// Behaviors pinned here (the council MUST-FIX D "build the VERB"):
//   • markReviewedAndAdvance: marks the current id reviewed AND returns the next
//     UNREVIEWED id in scope order (wraps; null when none remain).
//   • next/prevUnreviewed: traverse the unreviewed set in scope order (wraps).
//   • addDraft / draftCount / hasDrafts: the pending (draft) comment store.
//   • resolveThread / isThreadResolved: the resolved-thread map (Shift+R).
//   • publishReview: pending → published, checkpoint advance, pending cleared
//     (the pending → published transition the council names).
//   • buildReviewPayload: the PORTABLE export — a gh-CLI command + copy-JSON.
//     NEVER posts (no network); it is a pure (state, meta) → payload function.
//   • progressOf: the REAL reviewed/total + open-thread count for the header chip
//     (never fabricated — the honesty/never-a-false-claim contract).
import { describe, it, expect } from "vitest";
import {
  emptyReviewState,
  markReviewed,
  nextUnreviewed,
  prevUnreviewed,
  markReviewedAndAdvance,
  reviewedCount,
  isReviewed,
  addDraft,
  draftCountFor,
  totalDraftCount,
  hasDrafts,
  resolveThread,
  isThreadResolved,
  threadKey,
  publishReview,
  buildReviewPayload,
  progressOf,
  type ReviewState,
  type ReviewDraft,
} from "./review-machine";

const SCOPE = ["customers", "orders", "payments"];

function withReviewed(...ids: string[]): ReviewState {
  return ids.reduce((s, id) => markReviewed(s, id), emptyReviewState());
}

describe("review-machine — reviewed set", () => {
  it("emptyReviewState is empty (no reviewed, no drafts, no resolved, checkpoint null)", () => {
    const s = emptyReviewState();
    expect(reviewedCount(s)).toBe(0);
    expect(totalDraftCount(s)).toBe(0);
    expect(s.checkpoint).toBeNull();
    expect(Object.keys(s.resolved)).toHaveLength(0);
  });

  it("markReviewed flags an id reviewed; isReviewed reflects it", () => {
    const s = markReviewed(emptyReviewState(), "customers");
    expect(isReviewed(s, "customers")).toBe(true);
    expect(isReviewed(s, "orders")).toBe(false);
    expect(reviewedCount(s)).toBe(1);
  });

  it("markReviewed is idempotent (re-marking the same id does not double-count)", () => {
    const s = markReviewed(markReviewed(emptyReviewState(), "customers"), "customers");
    expect(reviewedCount(s)).toBe(1);
  });

  it("markReviewed returns a NEW state (no mutation of the input)", () => {
    const s0 = emptyReviewState();
    const s1 = markReviewed(s0, "customers");
    expect(s1).not.toBe(s0);
    expect(isReviewed(s0, "customers")).toBe(false); // input unchanged
  });
});

describe("review-machine — unreviewed traversal (next/prev, wrapping)", () => {
  it("nextUnreviewed from the first item lands on the next UNREVIEWED in scope order", () => {
    const s = emptyReviewState();
    expect(nextUnreviewed(s, SCOPE, "customers")).toBe("orders");
  });

  it("nextUnreviewed SKIPS reviewed items", () => {
    const s = withReviewed("orders");
    // from customers, orders is reviewed → skip to payments.
    expect(nextUnreviewed(s, SCOPE, "customers")).toBe("payments");
  });

  it("nextUnreviewed WRAPS around the end of scope", () => {
    const s = emptyReviewState();
    // from payments (last), the next unreviewed wraps to customers.
    expect(nextUnreviewed(s, SCOPE, "payments")).toBe("customers");
  });

  it("prevUnreviewed walks backward + wraps", () => {
    const s = emptyReviewState();
    expect(prevUnreviewed(s, SCOPE, "orders")).toBe("customers");
    expect(prevUnreviewed(s, SCOPE, "customers")).toBe("payments"); // wrap
  });

  it("next/prevUnreviewed return null when EVERY item is reviewed (none to advance to)", () => {
    const s = withReviewed("customers", "orders", "payments");
    expect(nextUnreviewed(s, SCOPE, "customers")).toBeNull();
    expect(prevUnreviewed(s, SCOPE, "customers")).toBeNull();
  });

  it("next/prevUnreviewed return null on an empty scope", () => {
    const s = emptyReviewState();
    expect(nextUnreviewed(s, [], "customers")).toBeNull();
    expect(prevUnreviewed(s, [], "customers")).toBeNull();
  });

  it("nextUnreviewed when the CURRENT is the only unreviewed returns null (no other target)", () => {
    const s = withReviewed("orders", "payments");
    // only customers is unreviewed; advancing from it finds no OTHER unreviewed.
    expect(nextUnreviewed(s, SCOPE, "customers")).toBeNull();
  });

  it("nextUnreviewed tolerates a current id NOT in scope (starts the walk from the front)", () => {
    const s = emptyReviewState();
    expect(nextUnreviewed(s, SCOPE, "not_in_scope")).toBe("customers");
  });
});

describe("review-machine — markReviewedAndAdvance (the x verb)", () => {
  it("marks the current id reviewed AND returns the next-unreviewed id", () => {
    const { state, next } = markReviewedAndAdvance(emptyReviewState(), SCOPE, "customers");
    expect(isReviewed(state, "customers")).toBe(true);
    expect(next).toBe("orders"); // advanced
  });

  it("the just-marked id is NEVER the advance target (it's now reviewed)", () => {
    // from payments: mark payments reviewed, advance wraps to the first unreviewed.
    const { state, next } = markReviewedAndAdvance(emptyReviewState(), SCOPE, "payments");
    expect(isReviewed(state, "payments")).toBe(true);
    expect(next).toBe("customers");
    expect(next).not.toBe("payments");
  });

  it("marking the LAST unreviewed item returns next=null (loop complete) but still marks it", () => {
    const s = withReviewed("customers", "orders");
    const { state, next } = markReviewedAndAdvance(s, SCOPE, "payments");
    expect(isReviewed(state, "payments")).toBe(true);
    expect(reviewedCount(state)).toBe(3);
    expect(next).toBeNull();
  });
});

describe("review-machine — draft (pending) comment store", () => {
  const draft: ReviewDraft = { path: "models/customers.sql", line: 4, side: "new", body: "nit: alias this CTE" };

  it("addDraft accumulates drafts per model; counts reflect it", () => {
    let s = emptyReviewState();
    s = addDraft(s, "customers", draft);
    expect(hasDrafts(s)).toBe(true);
    expect(draftCountFor(s, "customers")).toBe(1);
    expect(totalDraftCount(s)).toBe(1);
  });

  it("addDraft on a second model keeps both batches; total sums them", () => {
    let s = emptyReviewState();
    s = addDraft(s, "customers", draft);
    s = addDraft(s, "orders", { ...draft, path: "models/orders.sql", body: "question" });
    expect(draftCountFor(s, "customers")).toBe(1);
    expect(draftCountFor(s, "orders")).toBe(1);
    expect(totalDraftCount(s)).toBe(2);
  });

  it("hasDrafts is false on an empty state", () => {
    expect(hasDrafts(emptyReviewState())).toBe(false);
  });

  it("addDraft does not mutate the input state", () => {
    const s0 = emptyReviewState();
    const s1 = addDraft(s0, "customers", draft);
    expect(s1).not.toBe(s0);
    expect(totalDraftCount(s0)).toBe(0);
  });
});

describe("review-machine — resolved threads (Shift+R)", () => {
  it("threadKey composes a stable model@line key", () => {
    expect(threadKey("customers", 4)).toBe("customers@4");
  });

  it("resolveThread(true) marks a thread resolved; isThreadResolved reflects it", () => {
    const s = resolveThread(emptyReviewState(), "customers", 4, true);
    expect(isThreadResolved(s, "customers", 4)).toBe(true);
  });

  it("resolveThread(false) UN-resolves (toggle the other way)", () => {
    let s = resolveThread(emptyReviewState(), "customers", 4, true);
    s = resolveThread(s, "customers", 4, false);
    expect(isThreadResolved(s, "customers", 4)).toBe(false);
  });

  it("an unresolved thread reads false", () => {
    expect(isThreadResolved(emptyReviewState(), "customers", 9)).toBe(false);
  });
});

describe("review-machine — publishReview (pending → published + checkpoint advance)", () => {
  const d1: ReviewDraft = { path: "models/customers.sql", line: 4, side: "new", body: "a" };
  const d2: ReviewDraft = { path: "models/orders.sql", line: 2, side: "new", body: "b" };

  it("moves pending drafts into published, clears pending, records a review summary", () => {
    let s = emptyReviewState();
    s = addDraft(s, "customers", d1);
    s = addDraft(s, "orders", d2);
    const at = "2026-06-24T12:00:00Z";
    const out = publishReview(s, "comment", "LGTM with nits", at);
    expect(totalDraftCount(out)).toBe(0); // pending cleared
    expect(out.published.customers).toHaveLength(1);
    expect(out.published.orders).toHaveLength(1);
    expect(out.reviews).toHaveLength(1);
    expect(out.reviews[0]!.state).toBe("COMMENTED");
    expect(out.reviews[0]!.body).toBe("LGTM with nits");
    expect(out.reviews[0]!.at).toBe(at);
  });

  it("advances the checkpoint to the publish timestamp (the reviewCheckpoint advance)", () => {
    const s = addDraft(emptyReviewState(), "customers", d1);
    const at = "2026-06-24T12:00:00Z";
    const out = publishReview(s, "approve", "", at);
    expect(out.checkpoint).toBe(at);
  });

  it("maps verdict → GitHub review state (approve/request/comment)", () => {
    const s = addDraft(emptyReviewState(), "customers", d1);
    expect(publishReview(s, "approve", "", "t").reviews[0]!.state).toBe("APPROVED");
    expect(publishReview(s, "request", "", "t").reviews[0]!.state).toBe("CHANGES_REQUESTED");
    expect(publishReview(s, "comment", "", "t").reviews[0]!.state).toBe("COMMENTED");
  });

  it("publishing with NO pending drafts still records the review + advances checkpoint", () => {
    const out = publishReview(emptyReviewState(), "approve", "ship it", "t2");
    expect(out.reviews).toHaveLength(1);
    expect(out.checkpoint).toBe("t2");
    expect(totalDraftCount(out)).toBe(0);
  });

  it("does not mutate the input state", () => {
    const s0 = addDraft(emptyReviewState(), "customers", d1);
    const out = publishReview(s0, "comment", "", "t");
    expect(out).not.toBe(s0);
    expect(totalDraftCount(s0)).toBe(1); // input pending intact
  });
});

describe("review-machine — buildReviewPayload (PORTABLE export, NEVER posts)", () => {
  const d1: ReviewDraft = { path: "models/customers.sql", line: 4, side: "new", body: "alias this CTE" };
  const d2: ReviewDraft = { path: "models/customers.sql", line: 9, side: "old", body: "why removed?" };
  const d3: ReviewDraft = { path: "models/orders.sql", line: 2, side: "new", body: "nit" };

  function stateWith3(): ReviewState {
    let s = emptyReviewState();
    s = addDraft(s, "customers", d1);
    s = addDraft(s, "customers", d2);
    s = addDraft(s, "orders", d3);
    return s;
  }

  it("produces a JSON payload carrying EVERY pending draft (the GitHub review API shape)", () => {
    const p = buildReviewPayload(stateWith3(), { verdict: "comment", body: "review", repo: "o/r", pr: 42 });
    const json = JSON.parse(p.json) as {
      event: string; body: string; comments: { path: string; line: number; side: string; body: string }[];
    };
    expect(json.event).toBe("COMMENT");
    expect(json.body).toBe("review");
    expect(json.comments).toHaveLength(3);
    // a left/old-side draft maps to GitHub's "LEFT" side; new → "RIGHT".
    const cust4 = json.comments.find((c) => c.path === "models/customers.sql" && c.line === 4);
    expect(cust4).toMatchObject({ side: "RIGHT", body: "alias this CTE" });
    const cust9 = json.comments.find((c) => c.path === "models/customers.sql" && c.line === 9);
    expect(cust9).toMatchObject({ side: "LEFT" });
  });

  it("maps verdict → the gh review event (APPROVE / REQUEST_CHANGES / COMMENT)", () => {
    const s = stateWith3();
    expect((JSON.parse(buildReviewPayload(s, { verdict: "approve", body: "", repo: "o/r", pr: 1 }).json) as { event: string }).event).toBe("APPROVE");
    expect((JSON.parse(buildReviewPayload(s, { verdict: "request", body: "", repo: "o/r", pr: 1 }).json) as { event: string }).event).toBe("REQUEST_CHANGES");
    expect((JSON.parse(buildReviewPayload(s, { verdict: "comment", body: "", repo: "o/r", pr: 1 }).json) as { event: string }).event).toBe("COMMENT");
  });

  it("emits a runnable gh-CLI command referencing the repo + PR + the API endpoint (NO inline secret, NO http call)", () => {
    const p = buildReviewPayload(stateWith3(), { verdict: "comment", body: "review", repo: "breezy-bays-labs/cute-dbt", pr: 42 });
    expect(p.ghCommand).toContain("gh api");
    expect(p.ghCommand).toContain("repos/breezy-bays-labs/cute-dbt/pulls/42/reviews");
    expect(p.ghCommand).toContain("--method POST");
    // the command runs the payload through stdin (the host runs it; cute-dbt never does).
    expect(p.ghCommand).toContain("--input");
    // it is a STRING the user copies — there is no URL/fetch here.
    expect(p.ghCommand).not.toMatch(/https?:\/\//);
  });

  it("the payload is honest about an EMPTY batch (zero comments, still a valid review)", () => {
    const p = buildReviewPayload(emptyReviewState(), { verdict: "approve", body: "ship", repo: "o/r", pr: 7 });
    const json = JSON.parse(p.json) as { comments: unknown[]; event: string };
    expect(json.comments).toHaveLength(0);
    expect(json.event).toBe("APPROVE");
  });
});

describe("review-machine — progressOf (the REAL header chip, never fabricated)", () => {
  it("reports reviewed/total over the in-scope models", () => {
    const s = withReviewed("customers");
    const p = progressOf(s, SCOPE, 0);
    expect(p.reviewed).toBe(1);
    expect(p.total).toBe(3);
  });

  it("carries the open-thread count through verbatim (computed by the caller from live threads)", () => {
    const p = progressOf(emptyReviewState(), SCOPE, 5);
    expect(p.openThreads).toBe(5);
  });

  it("a fully-reviewed scope reports reviewed === total (the complete state)", () => {
    const s = withReviewed("customers", "orders", "payments");
    const p = progressOf(s, SCOPE, 0);
    expect(p.reviewed).toBe(3);
    expect(p.total).toBe(3);
    expect(p.complete).toBe(true);
  });

  it("counts ONLY in-scope reviewed ids (a reviewed id outside scope does not inflate)", () => {
    const s = withReviewed("customers", "ghost_model_not_in_scope");
    const p = progressOf(s, SCOPE, 0);
    expect(p.reviewed).toBe(1); // ghost excluded
    expect(p.total).toBe(3);
    expect(p.complete).toBe(false);
  });
});
