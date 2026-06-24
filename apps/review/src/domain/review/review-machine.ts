// The review-state machine (V1 / cute-dbt#495 — the keyboard review LOOP).
//
// This is the PURE DOMAIN core of the council MUST-FIX D ("build the VERB, not
// just the nouns"). The prototype lets you *look* at everything but lacks the
// review FLOW; this module IS that flow, as a pure POD + pure transitions:
//   • a reviewed SET,
//   • a per-model pending (draft) + published comment store,
//   • a resolved-thread map (Shift+R),
//   • a review checkpoint that advances on publish,
//   • unreviewed traversal (next/prev, wrapping) + markReviewedAndAdvance,
//   • the PORTABLE write-review export (a gh-CLI command + copy-JSON — NEVER a
//     network post; cute-dbt is local-first / zero-egress by construction).
//
// Ported faithfully from the cute-dbt-next prototype's app.js review state
// (`reviewed`/`pending`/`published`/`resolvedThreads`/`reviews`/`reviewCheckpoint`
// + `markReviewedAndAdvance`/`submitReview`), with the LIVE write-back replaced
// by an export (the prototype's `submitReview` mutated local state to *simulate*
// a submit; cute-dbt produces a portable payload the HOST runs).
//
// LAYER: PURE DOMAIN — no I/O, no zustand, no React, no DOM. Every transition is
// `(state, …) → newState` (immutable; the input is never mutated), so the store
// slice (data layer) and the dispatcher are thin shells the chrome composes.

// ── the POD ──────────────────────────────────────────────────────────────────

/** One drafted (pending) review comment — anchored to a file line + side. */
export interface ReviewDraft {
  /** the project-relative file path the comment anchors to. */
  path: string;
  /** the 1-based line number. */
  line: number;
  /** which diff side ("old"=deletions/LEFT, "new"=additions/RIGHT). */
  side: "old" | "new";
  /** the markdown comment body. */
  body: string;
}

/** A recorded review summary (the published-review header — verdict + body). */
export interface ReviewSummary {
  /** the GitHub review state. */
  state: "APPROVED" | "CHANGES_REQUESTED" | "COMMENTED";
  /** the review-level body (the summary comment). */
  body: string;
  /** the publish timestamp (an ISO-8601 string — computed at the I/O boundary). */
  at: string;
}

/**
 * The whole review flow's state. POD-only: plain maps/arrays, no methods. The
 * keys of `reviewed`/`pending`/`published`/`resolved` are untrusted ids (model
 * names / `model@line` keys), so the maps use a null prototype (a stray
 * `__proto__`/`constructor` id can't pollute the chain — the same discipline as
 * dataset.ts's null-proto maps).
 */
export interface ReviewState {
  /** id → true for every reviewed in-scope instance (the reviewed SET). */
  reviewed: Record<string, true>;
  /** model id → its pending (draft, unpublished) comments. */
  pending: Record<string, ReviewDraft[]>;
  /** model id → its published comments (after a publish). */
  published: Record<string, ReviewDraft[]>;
  /** `model@line` → true for every resolved thread (Shift+R). */
  resolved: Record<string, true>;
  /** the submitted review summaries (newest last). */
  reviews: ReviewSummary[];
  /** the "changed since your last review" checkpoint (advances on publish). */
  checkpoint: string | null;
}

/** A fresh, empty review state (null-proto maps, no reviews, null checkpoint). */
export function emptyReviewState(): ReviewState {
  return {
    reviewed: Object.create(null) as Record<string, true>,
    pending: Object.create(null) as Record<string, ReviewDraft[]>,
    published: Object.create(null) as Record<string, ReviewDraft[]>,
    resolved: Object.create(null) as Record<string, true>,
    reviews: [],
    checkpoint: null,
  };
}

/** Shallow-clone a null-proto string-keyed map (preserves the null prototype). */
function cloneMap<V>(m: Record<string, V>): Record<string, V> {
  const out = Object.create(null) as Record<string, V>;
  for (const k in m) out[k] = m[k] as V;
  return out;
}

// ── reviewed set ──────────────────────────────────────────────────────────────

/** True iff `id` is in the reviewed set. */
export function isReviewed(s: ReviewState, id: string): boolean {
  return s.reviewed[id] === true;
}

/** Mark `id` reviewed (immutable; idempotent — re-marking does not double-count). */
export function markReviewed(s: ReviewState, id: string): ReviewState {
  if (s.reviewed[id] === true) return s; // idempotent no-op (identity preserved)
  const reviewed = cloneMap(s.reviewed);
  reviewed[id] = true;
  return { ...s, reviewed };
}

/** How many of the given in-scope ids are reviewed (ONLY in-scope ids count). */
export function reviewedCount(s: ReviewState, scope?: readonly string[]): number {
  if (scope) return scope.reduce((n, id) => n + (s.reviewed[id] === true ? 1 : 0), 0);
  return Object.keys(s.reviewed).length;
}

// ── unreviewed traversal ──────────────────────────────────────────────────────

/**
 * Walk `scope` from `current` in `dir` (+1 next / -1 prev), wrapping, and return
 * the first UNREVIEWED id that is NOT `current` — or null when none remains. A
 * `current` not in scope starts the walk from the front (dir>0) / back (dir<0).
 *
 * Shared by next/prevUnreviewed AND markReviewedAndAdvance so the "skip reviewed,
 * wrap, never land on self" semantics live in ONE place (the prototype's
 * markReviewedAndAdvance loop).
 */
function walkUnreviewed(s: ReviewState, scope: readonly string[], current: string, dir: 1 | -1): string | null {
  const n = scope.length;
  if (n === 0) return null;
  const at = scope.indexOf(current);
  // a current outside scope: start one step before the front (dir>0) / after the
  // back (dir<0) so the first stepped index is the front/back element.
  const from = at >= 0 ? at : dir > 0 ? -1 : n;
  for (let step = 1; step <= n; step++) {
    const i = (((from + dir * step) % n) + n) % n;
    const id = scope[i]!;
    if (id !== current && s.reviewed[id] !== true) return id;
  }
  return null;
}

/** The next UNREVIEWED id after `current` in scope order (wraps; null when none). */
export function nextUnreviewed(s: ReviewState, scope: readonly string[], current: string): string | null {
  return walkUnreviewed(s, scope, current, 1);
}

/** The previous UNREVIEWED id before `current` in scope order (wraps; null when none). */
export function prevUnreviewed(s: ReviewState, scope: readonly string[], current: string): string | null {
  return walkUnreviewed(s, scope, current, -1);
}

/**
 * The `x` verb (council MUST-FIX D): mark `current` reviewed AND compute the
 * next-unreviewed target to jump to. Returns BOTH the new state and the next id
 * (null when the loop is complete — `current` was the last unreviewed). The
 * advance is computed AFTER marking, so the just-marked id is never the target.
 */
export function markReviewedAndAdvance(
  s: ReviewState,
  scope: readonly string[],
  current: string,
): { state: ReviewState; next: string | null } {
  const state = markReviewed(s, current);
  return { state, next: nextUnreviewed(state, scope, current) };
}

// ── draft (pending) comment store ─────────────────────────────────────────────

/** Add a pending (draft) comment to a model's batch (immutable). */
export function addDraft(s: ReviewState, model: string, draft: ReviewDraft): ReviewState {
  const pending = cloneMap(s.pending);
  pending[model] = [...(pending[model] ?? []), draft];
  return { ...s, pending };
}

/** How many pending drafts a model carries. */
export function draftCountFor(s: ReviewState, model: string): number {
  return (s.pending[model] ?? []).length;
}

/** Total pending drafts across every model. */
export function totalDraftCount(s: ReviewState): number {
  let n = 0;
  for (const k in s.pending) n += (s.pending[k] ?? []).length;
  return n;
}

/** True iff ANY model carries a pending draft. */
export function hasDrafts(s: ReviewState): boolean {
  return totalDraftCount(s) > 0;
}

// ── resolved threads (Shift+R) ────────────────────────────────────────────────

/** The stable `model@line` key for a thread. */
export function threadKey(model: string, line: number): string {
  return `${model}@${line}`;
}

/** Set a thread resolved/unresolved (immutable; the Shift+R toggle). */
export function resolveThread(s: ReviewState, model: string, line: number, resolved: boolean): ReviewState {
  const key = threadKey(model, line);
  const map = cloneMap(s.resolved);
  if (resolved) map[key] = true;
  else delete map[key];
  return { ...s, resolved: map };
}

/** True iff the thread at (model, line) is resolved. */
export function isThreadResolved(s: ReviewState, model: string, line: number): boolean {
  return s.resolved[threadKey(model, line)] === true;
}

// ── publish (pending → published + checkpoint advance) ────────────────────────

/** The write-review verdict vocabulary (UI) → GitHub review state. */
export type Verdict = "approve" | "request" | "comment";

const VERDICT_STATE: Record<Verdict, ReviewSummary["state"]> = {
  approve: "APPROVED",
  request: "CHANGES_REQUESTED",
  comment: "COMMENTED",
};

/**
 * Publish the pending review: move every pending draft into published, clear
 * pending, record a review summary, and advance the checkpoint to `at`. This is
 * the LOCAL bookkeeping half of a review (the prototype's `submitReview`); the
 * portable export (`buildReviewPayload`) is what the host actually runs against
 * GitHub. `at` is computed at the I/O boundary (golden-determinism: the domain
 * stays a pure fn of its inputs — no `Date.now()` here).
 */
export function publishReview(s: ReviewState, verdict: Verdict, body: string, at: string): ReviewState {
  const published = cloneMap(s.published);
  for (const model in s.pending) {
    const drafts = s.pending[model] ?? [];
    if (drafts.length) published[model] = [...(published[model] ?? []), ...drafts];
  }
  const summary: ReviewSummary = { state: VERDICT_STATE[verdict], body, at };
  return {
    ...s,
    published,
    pending: Object.create(null) as Record<string, ReviewDraft[]>,
    reviews: [...s.reviews, summary],
    checkpoint: at,
  };
}

// ── the PORTABLE export (NEVER posts) ─────────────────────────────────────────

/** The GitHub review API event for each verdict (gh api review `event` field). */
const VERDICT_EVENT: Record<Verdict, "APPROVE" | "REQUEST_CHANGES" | "COMMENT"> = {
  approve: "APPROVE",
  request: "REQUEST_CHANGES",
  comment: "COMMENT",
};

export interface PayloadMeta {
  verdict: Verdict;
  /** the review-level summary body. */
  body: string;
  /** "owner/repo" (for the gh api endpoint). */
  repo: string;
  /** the PR number. */
  pr: number;
}

/** One GitHub-review-API comment (the wire shape `gh api …/reviews` expects). */
interface GhReviewComment {
  path: string;
  line: number;
  side: "LEFT" | "RIGHT";
  body: string;
}

/** The portable write-review payload — copy-JSON + a runnable gh-CLI command. */
export interface ReviewPayload {
  /** the pretty-printed JSON the user copies (the gh api request body). */
  json: string;
  /** a runnable `gh api …` command the HOST runs (cute-dbt never does). */
  ghCommand: string;
}

/** Flatten every pending draft into the GitHub review-API comment shape. */
function pendingComments(s: ReviewState): GhReviewComment[] {
  const out: GhReviewComment[] = [];
  for (const model in s.pending) {
    for (const d of s.pending[model] ?? []) {
      out.push({ path: d.path, line: d.line, side: d.side === "old" ? "LEFT" : "RIGHT", body: d.body });
    }
  }
  return out;
}

/**
 * Build the PORTABLE review payload (council "write-back = export"; the
 * local-first / zero-egress contract). This is a PURE function: it never opens a
 * socket, never calls fetch/gh — it RETURNS a copy-JSON blob + a `gh api`
 * command STRING the user pastes into their own shell. cute-dbt itself never
 * posts to GitHub (the never-a-false-claim + zero-egress invariant).
 */
export function buildReviewPayload(s: ReviewState, meta: PayloadMeta): ReviewPayload {
  const comments = pendingComments(s);
  const body = {
    event: VERDICT_EVENT[meta.verdict],
    body: meta.body,
    comments,
  };
  const json = JSON.stringify(body, null, 2);
  // `gh api` reads the body from stdin (`--input -`) so no secret/URL is inlined;
  // the HOST runs it (cute-dbt produces only the string).
  const endpoint = `repos/${meta.repo}/pulls/${meta.pr}/reviews`;
  const ghCommand = `gh api ${endpoint} --method POST --input -`;
  return { json, ghCommand };
}

// ── progress (the REAL header chip) ───────────────────────────────────────────

/** The header review-progress chip facts — REAL counts, never fabricated. */
export interface ReviewProgress {
  /** how many in-scope models are reviewed. */
  reviewed: number;
  /** the in-scope model count. */
  total: number;
  /** the open (unresolved) thread count — computed by the caller from live threads. */
  openThreads: number;
  /** true iff every in-scope model is reviewed. */
  complete: boolean;
}

/**
 * The header chip's facts: REAL reviewed/total over the in-scope `scope`, plus
 * the caller-supplied open-thread count (the caller derives it from the live
 * dataset threads minus `resolved`; passing it through keeps this fn pure +
 * scope-agnostic). Never fabricates a number — the honesty contract.
 */
export function progressOf(s: ReviewState, scope: readonly string[], openThreads: number): ReviewProgress {
  const reviewed = reviewedCount(s, scope);
  const total = scope.length;
  return { reviewed, total, openThreads, complete: total > 0 && reviewed === total };
}
