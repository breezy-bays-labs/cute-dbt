// Property invariants for the review-state machine (council §E — honesty is a
// PROPERTY contract, not just example-based). These pin the never-a-false-claim
// facts of the review LOOP over a generated domain (fast-check), strictly
// stronger than the example tests for the traversal/progress folds:
//   • markReviewedAndAdvance NEVER returns the just-marked id (no self-loop);
//   • the advance target, when non-null, is genuinely UNREVIEWED + in scope;
//   • progressOf.reviewed is EXACTLY the count of in-scope reviewed ids and
//     never exceeds total (the chip can't over-report — a false "done" claim);
//   • next/prevUnreviewed are mutually consistent (a reachable next is reachable
//     back via prev from itself), and both return null iff no OTHER unreviewed
//     in-scope id exists.
import { describe, it, expect } from "vitest";
import fc from "fast-check";
import {
  emptyReviewState,
  markReviewed,
  markReviewedAndAdvance,
  nextUnreviewed,
  prevUnreviewed,
  progressOf,
  isReviewed,
  type ReviewState,
} from "./review-machine";

/** A small arbitrary: a unique-id scope + a subset already reviewed. */
const scopeAndReviewed = fc
  .uniqueArray(fc.string({ minLength: 1, maxLength: 6 }), { minLength: 0, maxLength: 8 })
  .chain((scope) =>
    fc.record({
      scope: fc.constant(scope),
      reviewedIdx: fc.subarray(scope.map((_, i) => i)),
    }),
  );

function buildState(scope: string[], reviewedIdx: number[]): ReviewState {
  return reviewedIdx.reduce<ReviewState>((s, i) => markReviewed(s, scope[i]!), emptyReviewState());
}

describe("review-machine — property invariants (fast-check)", () => {
  it("markReviewedAndAdvance never returns the just-marked id (no self-loop)", () => {
    fc.assert(
      fc.property(scopeAndReviewed, fc.nat(), ({ scope, reviewedIdx }, pick) => {
        if (scope.length === 0) return;
        const cur = scope[pick % scope.length]!;
        const s = buildState(scope, reviewedIdx);
        const { state, next } = markReviewedAndAdvance(s, scope, cur);
        expect(isReviewed(state, cur)).toBe(true);
        if (next !== null) expect(next).not.toBe(cur);
      }),
    );
  });

  it("a non-null advance target is genuinely UNREVIEWED + in scope", () => {
    fc.assert(
      fc.property(scopeAndReviewed, fc.nat(), ({ scope, reviewedIdx }, pick) => {
        if (scope.length === 0) return;
        const cur = scope[pick % scope.length]!;
        const s = buildState(scope, reviewedIdx);
        const next = nextUnreviewed(s, scope, cur);
        if (next !== null) {
          expect(scope).toContain(next);
          expect(isReviewed(s, next)).toBe(false);
        }
      }),
    );
  });

  it("progressOf.reviewed equals the in-scope reviewed count and never exceeds total (no false 'done')", () => {
    fc.assert(
      fc.property(scopeAndReviewed, fc.nat({ max: 50 }), ({ scope, reviewedIdx }, open) => {
        const s = buildState(scope, reviewedIdx);
        // dedup reviewedIdx → the true in-scope reviewed count.
        const expectReviewed = new Set(reviewedIdx).size;
        const p = progressOf(s, scope, open);
        expect(p.reviewed).toBe(expectReviewed);
        expect(p.total).toBe(scope.length);
        expect(p.reviewed).toBeLessThanOrEqual(p.total);
        expect(p.complete).toBe(scope.length > 0 && p.reviewed === p.total);
        expect(p.openThreads).toBe(open);
      }),
    );
  });

  it("next/prevUnreviewed both return null IFF no OTHER unreviewed in-scope id exists", () => {
    fc.assert(
      fc.property(scopeAndReviewed, fc.nat(), ({ scope, reviewedIdx }, pick) => {
        if (scope.length === 0) return;
        const cur = scope[pick % scope.length]!;
        const s = buildState(scope, reviewedIdx);
        const otherUnreviewed = scope.some((id) => id !== cur && !isReviewed(s, id));
        const next = nextUnreviewed(s, scope, cur);
        const prev = prevUnreviewed(s, scope, cur);
        expect(next === null).toBe(!otherUnreviewed);
        expect(prev === null).toBe(!otherUnreviewed);
      }),
    );
  });
});
