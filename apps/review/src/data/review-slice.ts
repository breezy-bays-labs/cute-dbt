// The review Zustand slice (V1 / cute-dbt#495) — the 7th store slice. It owns the
// single `review: ReviewState` POD + actions that DELEGATE to the pure
// review-machine (src/domain/review/review-machine.ts). No review LOGIC lives
// here; the slice only:
//   • holds the state,
//   • injects `now()` at the I/O boundary for publish (so the domain stays a pure
//     fn of its inputs — the golden-determinism rule; the App passes a real
//     `() => new Date().toISOString()`, tests pass a fixed clock),
//   • exposes a fail-closed `sanitizeReviewState` for persist hydration.
//
// LAYER: data (may import domain; never view/chrome). Composed into the app store
// (src/data/store.ts) and persisted under the `review` field of the durable blob.

import {
  emptyReviewState,
  markReviewedAndAdvance,
  addDraft,
  resolveThread,
  publishReview,
  buildReviewPayload,
  type ReviewState,
  type ReviewDraft,
  type ReviewSummary,
  type Verdict,
  type PayloadMeta,
  type ReviewPayload,
} from "../domain/review/review-machine";

/** The review slice's state + actions (composed into the app store). */
export interface ReviewSlice {
  /** the whole review-flow state (the pure POD). */
  review: ReviewState;

  /** mark `current` reviewed + return the next-unreviewed id (or null). */
  markReviewedAdvance: (scope: readonly string[], current: string) => string | null;
  /** add a pending (draft) comment to a model's batch. */
  addReviewDraft: (model: string, draft: ReviewDraft) => void;
  /** set a thread resolved/unresolved (the Shift+R toggle). */
  setThreadResolved: (model: string, line: number, resolved: boolean) => void;
  /** publish the pending review (pending→published, checkpoint advance via now()). */
  publishReview: (verdict: Verdict, body: string) => void;
  /** build the PORTABLE export payload (gh command + copy-JSON; never posts). */
  buildPayload: (meta: PayloadMeta) => ReviewPayload;
}

/** The persisted defaults — a fresh empty review state. */
export const REVIEW_DEFAULTS: { review: ReviewState } = { review: emptyReviewState() };

export type ReviewSliceSet = (
  partial: ReviewSlice | Partial<ReviewSlice> | ((s: ReviewSlice) => ReviewSlice | Partial<ReviewSlice>),
) => void;
export type ReviewSliceGet = () => ReviewSlice;

/**
 * Build the review slice. `now` is the I/O-boundary clock (the App passes the
 * real wall-clock; tests pass a fixed string) — the domain never calls
 * `Date.now()` itself (golden-determinism).
 */
export function createReviewSlice(set: ReviewSliceSet, get: ReviewSliceGet, now: () => string): ReviewSlice {
  return {
    review: emptyReviewState(),

    markReviewedAdvance: (scope, current) => {
      const { state, next } = markReviewedAndAdvance(get().review, scope, current);
      set({ review: state });
      return next;
    },

    addReviewDraft: (model, draft) => set({ review: addDraft(get().review, model, draft) }),

    setThreadResolved: (model, line, resolved) =>
      set({ review: resolveThread(get().review, model, line, resolved) }),

    publishReview: (verdict, body) => set({ review: publishReview(get().review, verdict, body, now()) }),

    buildPayload: (meta) => buildReviewPayload(get().review, meta),
  };
}

// ── fail-closed persist hydration ─────────────────────────────────────────────

/** Coerce a persisted value into a null-proto string-keyed map (fail-closed). */
function asMap<V>(raw: unknown, keep: (v: unknown) => v is V): Record<string, V> {
  const out = Object.create(null) as Record<string, V>;
  if (!raw || typeof raw !== "object") return out;
  for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
    if (keep(v)) out[k] = v;
  }
  return out;
}

const isTrue = (v: unknown): v is true => v === true;
const isDraftArray = (v: unknown): v is ReviewDraft[] =>
  Array.isArray(v) &&
  v.every(
    (d) =>
      !!d &&
      typeof d === "object" &&
      typeof (d as ReviewDraft).path === "string" &&
      typeof (d as ReviewDraft).body === "string",
  );

function asReviews(raw: unknown): ReviewSummary[] {
  if (!Array.isArray(raw)) return [];
  return raw.filter(
    (r): r is ReviewSummary =>
      !!r && typeof r === "object" && typeof (r as ReviewSummary).state === "string" && typeof (r as ReviewSummary).at === "string",
  );
}

/**
 * Sanitize a persisted review blob on hydration (fail-closed, merge-over-empty).
 * A non-object → a fresh empty state; each sub-field that fails validation
 * degrades to its empty default IN PLACE (a corrupt/partial blob can never crash
 * hydration or smuggle a non-state value). Maps are rebuilt null-proto so a stale
 * `__proto__`/`constructor` key can't pollute the chain.
 */
export function sanitizeReviewState(raw: unknown): ReviewState {
  if (!raw || typeof raw !== "object") return emptyReviewState();
  const r = raw as Partial<ReviewState>;
  return {
    reviewed: asMap(r.reviewed, isTrue),
    pending: asMap(r.pending, isDraftArray),
    published: asMap(r.published, isDraftArray),
    resolved: asMap(r.resolved, isTrue),
    reviews: asReviews(r.reviews),
    checkpoint: typeof r.checkpoint === "string" ? r.checkpoint : null,
  };
}
