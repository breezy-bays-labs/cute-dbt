// PR page aggregation unit tests (S9 / cute-dbt#501). Covers the pure overview /
// files / timeline folds + the honesty invariants:
//   - counts are derived from the pr_dag state taxonomy, context nodes excluded;
//   - removed_models surface as non-navigable removed rows (no false "open");
//   - comment threads group + order honestly (line-less sorts last, never faked);
//   - the temporal feed is HONESTLY ABSENT (the T2 spine gap) — `present:false`,
//     never a fabricated commit/review/check.
import { describe, it, expect } from "vitest";
import {
  buildPrOverview, buildPrFiles, buildCommentTimeline, prTimelineFeed,
  countPrChanges, nodeChangeRole, threadCountsByModel, orderThreads,
  type TimelineThread,
} from "./pr-page";
import type { ContextData, PrDagGraph } from "./context-data";
import { loadFixture } from "../data/fixtures";

const real = loadFixture("context.440") as unknown as ContextData;

// ── a minimal synthetic context for edge-case isolation ──────────────────────
function synthCtx(over: Partial<ContextData> = {}): ContextData {
  return {
    baseline: "main",
    models: [],
    ...over,
  } as ContextData;
}

const graph: PrDagGraph = {
  nodes: [
    { id: "model.p.a", name: "a", state: "modified", is_connector: false, lines_added: 4, lines_removed: 1 },
    { id: "model.p.b", name: "b", state: "new", is_connector: false, lines_added: 9, lines_removed: 0 },
    { id: "model.p.c", name: "c", state: "added", is_connector: false, lines_added: 2, lines_removed: 0 },
    { id: "model.p.d", name: "d", state: "deleted", is_connector: false, lines_added: 0, lines_removed: 7 },
    { id: "model.p.conn", name: "conn", state: "modified", is_connector: true, lines_added: 0, lines_removed: 0 },
    { id: "model.p.halo", name: "halo", state: "modified", is_connector: false, is_halo: true, lines_added: 0, lines_removed: 0 },
  ],
  edges: [{ from: "model.p.a", to: "model.p.b" }],
};

describe("nodeChangeRole", () => {
  it("classifies a real changed node by its state", () => {
    expect(nodeChangeRole({ state: "modified", is_connector: false })).toBe("modified");
    expect(nodeChangeRole({ state: "new", is_connector: false })).toBe("new");
  });
  it("a connector or halo is ALWAYS context (even with a residual state)", () => {
    expect(nodeChangeRole({ state: "modified", is_connector: true })).toBe("context");
    expect(nodeChangeRole({ state: "modified", is_connector: false, is_halo: true })).toBe("context");
  });
});

describe("countPrChanges", () => {
  it("counts each state, excludes context nodes, and sums `changed`", () => {
    const c = countPrChanges(graph, ["models/x.sql", "models/y.sql"]);
    expect(c.modified).toBe(1);
    expect(c.new).toBe(1);
    expect(c.added).toBe(1);
    expect(c.deleted).toBe(1);
    expect(c.context).toBe(2); // conn + halo
    expect(c.removed).toBe(2);
    expect(c.changed).toBe(4); // modified+new+added+deleted, NOT context
  });
  it("an absent graph yields honest zeroes (only removed reflects the list)", () => {
    const c = countPrChanges(null, ["models/x.sql"]);
    expect(c.changed).toBe(0);
    expect(c.context).toBe(0);
    expect(c.removed).toBe(1);
  });
});

describe("buildPrOverview", () => {
  it("reads the real pr_ref identity verbatim (number/title/url)", () => {
    const o = buildPrOverview(real);
    expect(o.number).toBe(440);
    expect(o.title).toContain("dogfood");
    expect(o.url).toBe("https://github.com/breezy-bays-labs/cute-dbt/pull/440");
    expect(o.hasDag).toBe(true);
  });
  it("counts the real PR-scope changes + surfaces the removed model", () => {
    const o = buildPrOverview(real);
    // the 440 dogfood: 11 modified, 1 new, 1 deleted, 1 connector, 1 halo (15 nodes).
    expect(o.counts.changed).toBeGreaterThan(0);
    expect(o.counts.context).toBe(2);
    expect(o.removedModels).toEqual(["models/marts/orders_never_refunded.sql"]);
    expect(o.counts.removed).toBe(1);
  });
  it("an absent pr_ref degrades to honest-zero identity (no fabrication)", () => {
    const o = buildPrOverview(synthCtx());
    expect(o.number).toBe(0);
    expect(o.title).toBe("");
    expect(o.url).toBe("");
    expect(o.hasDag).toBe(false);
    expect(o.body).toBeUndefined();
    expect(o.author).toBeUndefined();
  });
  it("carries optional pr_ref body/author only when the spine emits them", () => {
    const o = buildPrOverview(synthCtx({ pr_ref: { number: 7, title: "t", url: "u", body: "hi", author: "alice" } }));
    expect(o.body).toBe("hi");
    expect(o.author).toBe("alice");
  });
});

describe("threadCountsByModel", () => {
  it("joins thread counts on the bare model name (from the unique_id)", () => {
    const counts = threadCountsByModel(real.pr_comments);
    // customer_order_days has 1 thread; order_metrics has 2; customers_with_no_orders 1 resolved.
    expect(counts["order_metrics"]?.total).toBe(2);
    expect(counts["customers_with_no_orders"]?.resolved).toBe(1);
  });
  it("an absent comments view yields an empty map", () => {
    expect(Object.keys(threadCountsByModel(undefined))).toHaveLength(0);
  });
});

describe("buildPrFiles", () => {
  it("emits a navigable row per changed node + a non-navigable removed row", () => {
    const v = buildPrFiles(synthCtx({ pr_dag: { graph, modified_count: 1, connector_count: 1, halo_count: 1, deleted_count: 1, collapsed: false }, removed_models: ["models/z.sql"] }));
    const ids = v.rows.map((r) => r.id);
    // 4 changed nodes (a/b/c/d) + 1 removed (z) — connector/halo excluded.
    expect(ids).toEqual(["a", "b", "c", "d", "z"]);
    const removed = v.rows.find((r) => r.id === "z")!;
    expect(removed.navigable).toBe(false);
    expect(removed.change).toBe("deleted");
    expect(removed.path).toBe("models/z.sql");
    // a changed node is navigable + carries its line deltas.
    const a = v.rows.find((r) => r.id === "a")!;
    expect(a.navigable).toBe(true);
    expect(a.linesAdded).toBe(4);
    expect(a.linesRemoved).toBe(1);
  });
  it("joins comment counts onto the file rows (real fixture)", () => {
    const v = buildPrFiles(real);
    const om = v.rows.find((r) => r.id === "order_metrics");
    expect(om?.threadTotal).toBe(2);
    const cwno = v.rows.find((r) => r.id === "customers_with_no_orders");
    expect(cwno?.threadResolved).toBe(1);
    expect(v.withThreads).toBeGreaterThan(0);
  });
  it("a context (connector/halo) node is never a file row", () => {
    const v = buildPrFiles(synthCtx({ pr_dag: { graph, modified_count: 1, connector_count: 1, halo_count: 1, deleted_count: 1, collapsed: false } }));
    expect(v.rows.find((r) => r.id === "conn")).toBeUndefined();
    expect(v.rows.find((r) => r.id === "halo")).toBeUndefined();
  });
  it("an absent pr_dag yields an honest-empty file list", () => {
    const v = buildPrFiles(synthCtx());
    expect(v.rows).toHaveLength(0);
    expect(v.hasDag).toBe(false);
  });
});

describe("orderThreads (honest ordering)", () => {
  it("orders by anchored line; a line-less (outdated) thread sorts LAST", () => {
    const threads: TimelineThread[] = [
      { line: 30, resolved: false, outdated: false, commentCount: 1, comments: [] },
      { line: null, resolved: false, outdated: true, commentCount: 1, comments: [] },
      { line: 10, resolved: false, outdated: false, commentCount: 1, comments: [] },
    ];
    expect(orderThreads(threads).map((t) => t.line)).toEqual([10, 30, null]);
  });
  it("is stable for equal lines (insertion order preserved)", () => {
    const threads: TimelineThread[] = [
      { line: 5, resolved: false, outdated: false, commentCount: 1, comments: [{ author: "a", body: "1" }] },
      { line: 5, resolved: false, outdated: false, commentCount: 1, comments: [{ author: "b", body: "2" }] },
    ];
    expect(orderThreads(threads).map((t) => t.comments[0]!.author)).toEqual(["a", "b"]);
  });
});

describe("buildCommentTimeline", () => {
  it("groups per-model threads + an unanchored group, with real counts", () => {
    const tl = buildCommentTimeline(real);
    expect(tl.hasComments).toBe(true);
    expect(tl.total).toBe(11); // pr_comments.total (real)
    // the unanchored group is present (the dogfood has 4 unanchored threads).
    const unanchored = tl.groups.find((g) => g.model === null);
    expect(unanchored).toBeDefined();
    expect(unanchored!.threads.length).toBeGreaterThan(0);
    // a model group carries its resolved count honestly.
    const cwno = tl.groups.find((g) => g.model === "customers_with_no_orders");
    expect(cwno?.resolvedCount).toBe(1);
  });
  it("threads within a group are ordered by line", () => {
    const tl = buildCommentTimeline(real);
    const om = tl.groups.find((g) => g.model === "order_metrics");
    const lines = om!.threads.map((t) => t.line);
    expect(lines).toEqual([...lines].sort((a, b) => (a ?? Infinity) - (b ?? Infinity)));
  });
  it("carries each comment's author + body verbatim (never invented)", () => {
    const tl = buildCommentTimeline(real);
    const cod = tl.groups.find((g) => g.model === "customer_order_days");
    expect(cod!.threads[0]!.comments[0]!.author).toBe("dogfood-dev");
    expect(cod!.threads[0]!.comments[0]!.body).toContain("status_variety");
  });
  it("an empty bucket is dropped (no zero-thread group)", () => {
    const ctx = synthCtx({ pr_comments: { by_model: [{ model: "model.p.x", threads: [] }], unanchored: [], total: 0 } });
    const tl = buildCommentTimeline(ctx);
    expect(tl.groups).toHaveLength(0);
    expect(tl.hasComments).toBe(false);
  });
  it("derives the total when the spine omits pr_comments.total", () => {
    const ctx = synthCtx({ pr_comments: { by_model: [{ model: "model.p.x", threads: [{ comments: [{ author: "a", body: "1" }, { author: "b", body: "2" }] }] }] } });
    const tl = buildCommentTimeline(ctx);
    expect(tl.total).toBe(2);
  });
});

describe("prTimelineFeed (the HONEST T2 spine gap)", () => {
  it("the commit/review/CI feed is honestly ABSENT on today's context", () => {
    const feed = prTimelineFeed(real);
    expect(feed.present).toBe(false);
    expect(feed.reason).toBe("spine-gap");
  });
  it("renders the REAL reviewer count + checks when the spine carries them (no fabrication)", () => {
    const ctx = synthCtx({
      pr_ref: {
        number: 1, title: "t", url: "u",
        reviewers: [{ login: "alice" }, { login: "bob" }],
        checks: { passed: 3, failed: 1, pending: 0 },
      },
    });
    const feed = prTimelineFeed(ctx);
    expect(feed.reviewerCount).toBe(2);
    expect(feed.checks).toEqual({ passed: 3, failed: 1, pending: 0 });
    // a feed of EVENTS still doesn't exist — reviewers+checks are standalone facts.
    expect(feed.present).toBe(false);
  });
  it("checks are honest-null when absent (never a fabricated all-passing)", () => {
    const feed = prTimelineFeed(real);
    expect(feed.checks).toBeNull();
    expect(feed.reviewerCount).toBe(0);
  });
});
