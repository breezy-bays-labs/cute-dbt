// The review-slice unit tests (V1 / cute-dbt#495). The slice is a THIN shell
// over the pure review-machine (src/domain/review/review-machine.ts): it owns the
// single `review: ReviewState` field + actions that delegate to the pure
// transitions, plus the I/O-boundary `now()` injection for publish (so the domain
// stays a pure fn of its inputs — golden-determinism). All the LOGIC is tested in
// the machine's own suite; here we test the wiring (delegation + the now-boundary
// + the persist sanitize). Exercised through a plain set/get harness (no zustand,
// no React) — the established slice-test pattern.
import { describe, it, expect } from "vitest";
import {
  createReviewSlice,
  REVIEW_DEFAULTS,
  sanitizeReviewState,
  type ReviewSlice,
} from "./review-slice";
import { emptyReviewState, type ReviewDraft } from "../domain/review/review-machine";

function harness(now: () => string = () => "2026-06-24T00:00:00Z") {
  let state: ReviewSlice;
  const set: (p: ReviewSlice | Partial<ReviewSlice> | ((s: ReviewSlice) => ReviewSlice | Partial<ReviewSlice>)) => void = (p) => {
    const patch = typeof p === "function" ? p(state) : p;
    state = { ...state, ...patch };
  };
  const get = () => state;
  state = createReviewSlice(set, get, now);
  return { get };
}

const draft: ReviewDraft = { path: "models/customers.sql", line: 4, side: "new", body: "nit" };

describe("review slice — defaults", () => {
  it("starts at an empty review state", () => {
    const { get } = harness();
    expect(get().review.reviewed).toEqual({});
    expect(get().review.checkpoint).toBeNull();
  });
  it("REVIEW_DEFAULTS exposes a fresh empty state", () => {
    expect(REVIEW_DEFAULTS.review.checkpoint).toBeNull();
  });
});

describe("review slice — mark + advance delegation", () => {
  it("markReviewedAdvance marks the current id reviewed AND returns the next-unreviewed id", () => {
    const { get } = harness();
    const next = get().markReviewedAdvance(["customers", "orders"], "customers");
    expect(get().review.reviewed.customers).toBe(true);
    expect(next).toBe("orders");
  });
  it("markReviewedAdvance returns null when the loop is complete (and still marks)", () => {
    const { get } = harness();
    get().markReviewedAdvance(["customers"], "customers");
    const next = get().markReviewedAdvance(["customers"], "customers");
    expect(next).toBeNull();
    expect(get().review.reviewed.customers).toBe(true);
  });
});

describe("review slice — draft + resolve delegation", () => {
  it("addReviewDraft appends a pending draft", () => {
    const { get } = harness();
    get().addReviewDraft("customers", draft);
    expect(get().review.pending.customers).toHaveLength(1);
  });
  it("setThreadResolved flips the resolved map", () => {
    const { get } = harness();
    get().setThreadResolved("customers", 4, true);
    expect(get().review.resolved["customers@4"]).toBe(true);
    get().setThreadResolved("customers", 4, false);
    expect(get().review.resolved["customers@4"]).toBeUndefined();
  });
});

describe("review slice — publish uses the injected now() (golden-determinism)", () => {
  it("publishReview stamps the checkpoint with the injected now(), not Date.now()", () => {
    const { get } = harness(() => "2030-01-01T00:00:00Z");
    get().addReviewDraft("customers", draft);
    get().publishReview("approve", "ship");
    expect(get().review.checkpoint).toBe("2030-01-01T00:00:00Z");
    expect(get().review.published.customers).toHaveLength(1);
    expect(get().review.pending).toEqual({}); // cleared
    expect(get().review.reviews).toHaveLength(1);
  });
});

describe("review slice — buildPayload delegation (portable, never posts)", () => {
  it("builds a portable gh payload from the pending drafts", () => {
    const { get } = harness();
    get().addReviewDraft("customers", draft);
    const payload = get().buildPayload({ verdict: "comment", body: "r", repo: "o/r", pr: 9 });
    expect(payload.ghCommand).toContain("gh api repos/o/r/pulls/9/reviews");
    expect(JSON.parse(payload.json).comments).toHaveLength(1);
  });
});

describe("sanitizeReviewState — fail-closed hydration", () => {
  it("a non-object → a fresh empty state", () => {
    expect(sanitizeReviewState(null).checkpoint).toBeNull();
    expect(sanitizeReviewState(42).reviewed).toEqual({});
    expect(sanitizeReviewState("x").reviews).toEqual([]);
  });
  it("a well-formed persisted blob round-trips its fields", () => {
    const s = emptyReviewState();
    s.reviewed.customers = true;
    s.checkpoint = "2026-06-24T00:00:00Z";
    const round = sanitizeReviewState(JSON.parse(JSON.stringify(s)));
    expect(round.reviewed.customers).toBe(true);
    expect(round.checkpoint).toBe("2026-06-24T00:00:00Z");
  });
  it("drops malformed sub-fields (a non-array reviews → [])", () => {
    const round = sanitizeReviewState({ reviewed: { a: true }, reviews: "nope", checkpoint: 5 });
    expect(round.reviewed.a).toBe(true);
    expect(round.reviews).toEqual([]);
    expect(round.checkpoint).toBeNull(); // a non-string checkpoint → null
  });
  it("the sanitized maps are null-proto (a __proto__ key can't pollute)", () => {
    const round = sanitizeReviewState({ reviewed: { ["__proto__"]: true } });
    // the key lands as an OWN property, not on Object.prototype.
    expect(Object.getPrototypeOf(round.reviewed)).toBeNull();
    expect(({} as Record<string, unknown>)["polluted"]).toBeUndefined();
  });
});
