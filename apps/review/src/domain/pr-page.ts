// The PR page aggregation (S9 / cute-dbt#501) — PURE reshapers from the validated
// `ContextData` onto the three PR-page surfaces: the OVERVIEW (number/title/url +
// the changed-model summary), the FILES list (one row per changed file with its
// comment counts, navigable), and the comment TIMELINE (the per-model + unanchored
// threads, grouped + ordered honestly).
//
// HONESTY (never-a-false-claim — load-bearing here): every fact is READ from the
// context, never fabricated. The cute-dbt spine does NOT yet emit a commit /
// review / CI-checks feed (only pr_ref + pr_comments + pr_dag + removed_models),
// so `prTimelineFeed` returns an HONEST `{ present: false }` state — the view
// renders a documented "not in this context" panel instead of inventing a commit,
// a review, a check, or a timestamp. The real-gh timeline feed is a tracked T2
// spine gap (cute-dbt#508 / §B6), NOT this slice's job to invent.
//
// LAYER: PURE DOMAIN — std + the context types only. No I/O, no React, no zustand.

import type {
  CommentsView, ContextData, PrDagGraph, PrDagState, PrRef, RenderedThread,
} from "./context-data";

// ── PR overview ──────────────────────────────────────────────────────────────

/** A node's change role in the PR-scope DAG: a real changed state, or a CONTEXT
 *  node (a connector/halo pulled in for lineage, not itself a PR change). */
export type PrChangeRole = PrDagState | "context";

/** The changed-model summary counts — the four PR-change states + the removed
 *  models (which the spine carries as a NODE-LESS list, distinct from a deleted
 *  pr_dag node) + the context (connector/halo) nodes the DAG pulled in. */
export interface PrOverviewCounts {
  /** `state: "new"` nodes — a brand-new model node. */
  new: number;
  /** `state: "added"` nodes — a node added to the scope. */
  added: number;
  /** `state: "modified"` nodes. */
  modified: number;
  /** `state: "deleted"` pr_dag nodes (distinct from `removed`). */
  deleted: number;
  /** `removed_models` — the node-less removed-file list. */
  removed: number;
  /** connector + halo nodes (lineage context, not a PR change of their own). */
  context: number;
  /** the count of CHANGED nodes (new+added+modified+deleted) — excludes context. */
  changed: number;
}

/** The PR overview model — the pr_ref identity + the changed-model summary. Every
 *  field is read from the context; absent pr_ref fields degrade to honest empties. */
export interface PrOverview {
  number: number;
  title: string;
  url: string;
  /** present only when the spine emits it (§B6 optional fields). */
  body?: string;
  author?: string;
  /** the changed-model summary counts. */
  counts: PrOverviewCounts;
  /** the removed-model file paths (node-less; rendered as removed entries). */
  removedModels: string[];
  /** does the context carry a PR-scope DAG at all (else the summary is empty)? */
  hasDag: boolean;
}

/** Classify a pr_dag node's change role (context wins — a connector/halo is never
 *  counted as a change even if it carries a residual `state`). */
export function nodeChangeRole(node: {
  state: PrDagState;
  is_connector: boolean;
  is_halo?: boolean;
}): PrChangeRole {
  if (node.is_connector || node.is_halo) return "context";
  return node.state;
}

function emptyCounts(): PrOverviewCounts {
  return { new: 0, added: 0, modified: 0, deleted: 0, removed: 0, context: 0, changed: 0 };
}

/** Tally one node's change role into the running counts (context excluded from
 *  `changed`). Extracted so `countPrChanges` stays a flat, low-complexity fold. */
function tallyNode(c: PrOverviewCounts, role: PrChangeRole): void {
  c[role]++;
  if (role !== "context") c.changed++;
}

/** Aggregate the changed-model summary from the PR-scope DAG graph + removed list. */
export function countPrChanges(
  graph: PrDagGraph | null | undefined,
  removedModels: readonly string[],
): PrOverviewCounts {
  const c = emptyCounts();
  c.removed = removedModels.length;
  for (const n of graph?.nodes ?? []) tallyNode(c, nodeChangeRole(n));
  return c;
}

/** Build the PR overview from the validated context. The pr_ref identity is read
 *  verbatim; the counts come from the PR-scope DAG; the removed models are the
 *  node-less list. NO fabrication — an absent pr_ref yields honest-zero identity. */
export function buildPrOverview(context: ContextData): PrOverview {
  const ref: PrRef | undefined = context.pr_ref;
  const removedModels = context.removed_models ?? [];
  const graph = context.pr_dag?.graph;
  return {
    number: ref?.number ?? 0,
    title: ref?.title ?? "",
    url: ref?.url ?? "",
    ...(ref?.body != null ? { body: ref.body } : {}),
    ...(ref?.author != null ? { author: ref.author } : {}),
    counts: countPrChanges(graph, removedModels),
    removedModels,
    hasDag: !!graph,
  };
}

// ── PR files ─────────────────────────────────────────────────────────────────

/** One changed file/model row on the PR Files list. `id` is the model NAME (the
 *  navigation target the Models review surface keys on); `removed` files carry no
 *  live model id (they have no destination surface — never a false "open"). */
export interface PrFileRow {
  /** the model name (= the Models-entity sel target) for a live node; the file
   *  path stem for a removed model (which has no live model record). */
  id: string;
  /** the project-relative path when known (removed files carry their full path). */
  path: string;
  /** the change role (drives the chip color + whether the row is navigable). */
  change: PrChangeRole;
  /** lines added on this node (0 for a removed/context-only row). */
  linesAdded: number;
  /** lines removed on this node. */
  linesRemoved: number;
  /** total threads anchored to this file (from pr_comments). */
  threadTotal: number;
  /** resolved threads anchored to this file. */
  threadResolved: number;
  /** is this row navigable to a live model review surface (removed ⇒ false)? */
  navigable: boolean;
}

/** The Files-list aggregation — every changed node (excluding context connectors/
 *  halo) as a navigable row, plus the removed models as non-navigable removed rows.
 *  Comment counts join from pr_comments by the node NAME (the bucket model id is the
 *  `model.<pkg>.<name>` form; we match on the trailing name + the path stem). */
export interface PrFilesView {
  rows: PrFileRow[];
  /** the count of files that carry at least one comment thread. */
  withThreads: number;
  /** whether the context carries a PR-scope DAG (else this is empty + honest). */
  hasDag: boolean;
}

/** The trailing model name of a comment-bucket `model` id (`model.jaffle.x` → `x`),
 *  or the bucket's path stem, or undefined. The comment buckets key on the dbt
 *  unique_id; the PR-dag nodes key on the bare name — we join on the bare name. */
function bucketName(b: { model?: string; path?: string; model_path?: string }): string | undefined {
  if (b.model) {
    const segs = b.model.split(".");
    return segs[segs.length - 1] || undefined;
  }
  const p = b.path ?? b.model_path;
  if (p) return p.split("/").pop()?.replace(/\.\w+$/, "");
  return undefined;
}

/** A per-model thread-count summary keyed by the bare model name. */
export interface ThreadCounts { total: number; resolved: number; }

/** Build the per-model thread-count map from pr_comments.by_model (joined on the
 *  bare model name). Unanchored threads are NOT joined to a model row (they have no
 *  model anchor) — they surface only in the timeline, never inflate a file's count. */
export function threadCountsByModel(
  comments: CommentsView | null | undefined,
): Record<string, ThreadCounts> {
  // null-proto map: keys are untrusted model names off the wire.
  const out: Record<string, ThreadCounts> = Object.create(null) as Record<string, ThreadCounts>;
  for (const b of comments?.by_model ?? []) {
    const name = bucketName(b);
    if (!name) continue;
    const threads = b.threads ?? [];
    const resolved = threads.filter((t) => t.resolved).length;
    const prev = out[name] ?? { total: 0, resolved: 0 };
    out[name] = { total: prev.total + threads.length, resolved: prev.resolved + resolved };
  }
  return out;
}

export function buildPrFiles(context: ContextData): PrFilesView {
  const graph = context.pr_dag?.graph;
  const counts = threadCountsByModel(context.pr_comments);
  const rows: PrFileRow[] = [];

  for (const n of graph?.nodes ?? []) {
    const change = nodeChangeRole(n);
    if (change === "context") continue; // connectors/halo aren't changed files
    const tc = counts[n.name] ?? { total: 0, resolved: 0 };
    rows.push({
      id: n.name,
      path: n.name,
      change,
      linesAdded: n.lines_added ?? 0,
      linesRemoved: n.lines_removed ?? 0,
      threadTotal: tc.total,
      threadResolved: tc.resolved,
      navigable: true,
    });
  }

  // the removed models (node-less list) — non-navigable removed rows (no live
  // model surface to open; an "open" affordance would be a false claim).
  for (const path of context.removed_models ?? []) {
    const stem = path.split("/").pop()?.replace(/\.\w+$/, "") ?? path;
    rows.push({
      id: stem, path, change: "deleted",
      linesAdded: 0, linesRemoved: 0,
      threadTotal: 0, threadResolved: 0,
      navigable: false,
    });
  }

  return {
    rows,
    withThreads: rows.filter((r) => r.threadTotal > 0).length,
    hasDag: !!graph,
  };
}

// ── PR comment timeline ──────────────────────────────────────────────────────

/** A rendered thread enriched with its grouping + ordering keys (pure). */
export interface TimelineThread {
  /** the model name this thread anchors to (undefined for an unanchored thread). */
  model?: string;
  /** the project-relative file path. */
  path?: string;
  line?: number | null;
  side?: "Left" | "Right";
  resolved: boolean;
  outdated: boolean;
  /** the comment count on the thread. */
  commentCount: number;
  /** the thread's comments (author + body), verbatim. */
  comments: { author: string | null; body: string }[];
}

/** One grouped section of the timeline — a model bucket or the unanchored group. */
export interface TimelineGroup {
  /** the model name, or null for the unanchored group. */
  model: string | null;
  /** the display path (the model_path/path of the bucket, or undefined). */
  path?: string;
  threads: TimelineThread[];
  /** total comments across the group's threads. */
  commentCount: number;
  /** resolved thread count in the group. */
  resolvedCount: number;
}

/** The comment-timeline aggregation: per-model groups (in the spine's bucket order)
 *  + an unanchored group, threads ordered honestly (a line-less/outdated thread
 *  sorts FIRST to match the spine's canonical within-bucket order — see
 *  `orderThreads`; never fabricated to a fake line). */
export interface CommentTimeline {
  groups: TimelineGroup[];
  /** the total COMMENT count — always derived as the sum of every thread's
   *  `commentCount` across all groups. NOT the spine's `pr_comments.total`, which
   *  is a THREAD count (`src/domain/pr_comment_render.rs:167`) — passing it through
   *  under a "comments" label would mislabel a thread count as a comment count. The
   *  thread count lives in `threadTotal`. */
  total: number;
  /** the count of grouped threads across all groups (= the spine's
   *  `pr_comments.total`, recomputed locally from the rendered groups). */
  threadTotal: number;
  /** does the context carry any comments at all (else honest-empty)? */
  hasComments: boolean;
}

function toTimelineThread(t: RenderedThread): TimelineThread {
  const comments = (t.comments ?? []).map((c) => ({ author: c.author, body: c.body }));
  return {
    ...(t.model != null ? { model: t.model } : {}),
    ...(t.path != null ? { path: t.path } : {}),
    line: t.line ?? null,
    ...(t.side != null ? { side: t.side } : {}),
    resolved: !!t.resolved,
    outdated: !!t.outdated,
    commentCount: comments.length,
    comments,
  };
}

/** Order threads to match the spine's canonical within-bucket order
 *  (`src/domain/pr_comment_render.rs:343-348`): a line-less (outdated) thread —
 *  one with no live line — sorts FIRST (`None` before `Some`), then anchored
 *  threads by line ascending. This deliberately mirrors the Rust spine so the PR
 *  page agrees with every other surface that consumes the spine's thread order
 *  (the static report, the `by_model` buckets); do NOT "fix" outdated threads to
 *  sort last — that would diverge from the spine. Never invents a line. Stable for
 *  ties (insertion order preserved). */
export function orderThreads(threads: TimelineThread[]): TimelineThread[] {
  return threads
    .map((t, i) => ({ t, i }))
    .sort((a, b) => {
      // line-less (outdated) threads lead, mirroring the spine's `None`-first sort.
      const aHasLine = a.t.line != null;
      const bHasLine = b.t.line != null;
      if (aHasLine !== bHasLine) return aHasLine ? 1 : -1;
      if (!aHasLine) return a.i - b.i; // both line-less: stable insertion order
      return a.t.line! !== b.t.line! ? a.t.line! - b.t.line! : a.i - b.i;
    })
    .map((x) => x.t);
}

export function buildCommentTimeline(context: ContextData): CommentTimeline {
  const comments = context.pr_comments;
  const groups: TimelineGroup[] = [];
  let threadTotal = 0;
  let commentTotal = 0;

  // The spine emits `by_model` in node-id order (`pr_comment_render.rs:151`); we
  // walk it in array order, which preserves that emitted order verbatim (no
  // re-sort of buckets here — only the threads WITHIN a bucket are ordered, to
  // match the spine's within-bucket sort; see `orderThreads`).
  for (const b of comments?.by_model ?? []) {
    const threads = orderThreads((b.threads ?? []).map(toTimelineThread));
    if (threads.length === 0) continue;
    const commentCount = threads.reduce((n, t) => n + t.commentCount, 0);
    threadTotal += threads.length;
    commentTotal += commentCount;
    groups.push({
      model: bucketName(b) ?? b.model ?? null,
      ...(b.model_path ?? b.path ? { path: b.model_path ?? b.path } : {}),
      threads,
      commentCount,
      resolvedCount: threads.filter((t) => t.resolved).length,
    });
  }

  const unanchored = orderThreads((comments?.unanchored ?? []).map(toTimelineThread));
  if (unanchored.length > 0) {
    const commentCount = unanchored.reduce((n, t) => n + t.commentCount, 0);
    threadTotal += unanchored.length;
    commentTotal += commentCount;
    groups.push({
      model: null,
      threads: unanchored,
      commentCount,
      resolvedCount: unanchored.filter((t) => t.resolved).length,
    });
  }

  return {
    groups,
    // `total` is the COMMENT count — always the locally-summed comment total, never
    // the spine's `pr_comments.total` (a THREAD count). `threadTotal` carries the
    // thread count, so the view can label each number with its true noun.
    total: commentTotal,
    threadTotal,
    hasComments: threadTotal > 0,
  };
}

// ── The honest commit / review / CI feed state (the T2 spine gap) ────────────

/** The temporal-feed honesty state. The cute-dbt spine does NOT emit a commit /
 *  review / CI-checks feed today — the context carries only pr_ref + pr_comments +
 *  pr_dag + removed_models. So `present` is ALWAYS false on the current context
 *  shape: the view renders a documented "not in this context" panel rather than
 *  fabricating a commit, a review, a check, or a timestamp.
 *
 *  This is a string-literal-union honesty signal (NOT a bare bool exposed to the
 *  view as truthiness): `reason` names WHY the feed is empty so the panel can link
 *  the T2 spine gap. When T2 lands the feed, `present` becomes true + the typed
 *  feed fields populate — the view switches off the honest-empty panel by reading
 *  `present`, never by inferring from a missing key. */
export interface PrTimelineFeed {
  /** true only when the context carries a real temporal feed (T2). */
  present: boolean;
  /** the honest reason the feed is absent (the tracked spine gap). */
  reason: "spine-gap" | "available";
  /** the count of distinct reviewers the context DOES carry (pr_ref.reviewers) —
   *  honestly rendered even when the full feed is absent (it's real). */
  reviewerCount: number;
  /** the CI-checks summary the context carries, if any (pr_ref.checks) — honest
   *  null when absent (never a fabricated "all passing"). */
  checks: { passed: number; failed: number; pending: number } | null;
}

/** Compute the honest temporal-feed state for a context. Reads ONLY real fields:
 *  the optional `pr_ref.reviewers` + `pr_ref.checks` (§B6 — absent on today's
 *  spine). The commit/review-event feed is a tracked T2 gap → `present: false`. */
export function prTimelineFeed(context: ContextData): PrTimelineFeed {
  const ref = context.pr_ref;
  const reviewers = ref?.reviewers ?? [];
  const checks = ref?.checks ?? null;
  const haveChecks = !!checks && (checks.passed != null || checks.failed != null || checks.pending != null);
  return {
    // A real temporal feed needs commit/review EVENTS — the spine emits none today.
    // Reviewers + checks alone are NOT a feed (they're standalone facts), so the
    // feed stays honestly absent until T2 lands the event stream.
    present: false,
    reason: "spine-gap",
    reviewerCount: reviewers.length,
    checks: haveChecks
      ? { passed: checks!.passed ?? 0, failed: checks!.failed ?? 0, pending: checks!.pending ?? 0 }
      : null,
  };
}
