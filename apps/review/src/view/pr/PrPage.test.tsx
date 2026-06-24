// PR page view tests (S9 / cute-dbt#501) — static-markup assertions on the three
// PR surfaces. The interactive flow (keyboard nav, click-to-open) is covered by
// the network-denied Playwright e2e; here we assert the structural shell + the
// HONESTY contract (real identity rendered, the feed honest-empty, no fabrication).
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { PrOverview, PrFiles, PrTimeline } from "./PrPage";
import {
  buildPrOverview, buildPrFiles, buildCommentTimeline, prTimelineFeed,
} from "../../domain/pr-page";
import type { ContextData } from "../../domain/context-data";
import { loadFixture } from "../../data/fixtures";

const real = loadFixture("context.440") as unknown as ContextData;

describe("PrOverview", () => {
  it("renders the real pr_ref identity (number/title/url link)", () => {
    const html = renderToStaticMarkup(<PrOverview overview={buildPrOverview(real)} />);
    expect(html).toContain('data-testid="view-pr-overview"');
    expect(html).toContain("PR #440");
    expect(html).toContain("https://github.com/breezy-bays-labs/cute-dbt/pull/440");
    expect(html).toContain("dogfood");
  });
  it("renders the changed-model summary counts from the DAG", () => {
    const o = buildPrOverview(real);
    const html = renderToStaticMarkup(<PrOverview overview={o} />);
    expect(html).toContain('data-testid="pr-change-summary"');
    // the real modified count (11) is rendered with its testid.
    expect(html).toContain(`data-testid="count-modified" data-count="${o.counts.modified}"`);
    expect(html).toContain(`data-testid="count-changed" data-count="${o.counts.changed}"`);
  });
  it("surfaces the removed model distinctly", () => {
    const html = renderToStaticMarkup(<PrOverview overview={buildPrOverview(real)} />);
    expect(html).toContain('data-testid="pr-removed-models"');
    expect(html).toContain("orders_never_refunded.sql");
  });
  it("an absent pr_ref renders honest-zero identity + no-dag note (never blank-faked)", () => {
    const o = buildPrOverview({ baseline: "main", models: [] } as ContextData);
    const html = renderToStaticMarkup(<PrOverview overview={o} />);
    expect(html).toContain("PR #0");
    expect(html).toContain('data-testid="pr-no-dag"');
    expect(html).toContain('data-testid="pr-no-body"');
  });
});

describe("PrFiles", () => {
  it("renders a navigable row per changed model + the removed file as non-navigable", () => {
    const html = renderToStaticMarkup(<PrFiles files={buildPrFiles(real)} onOpen={() => {}} />);
    expect(html).toContain('data-testid="view-pr-files"');
    expect(html).toContain('data-file="order_metrics"');
    // the removed model is a non-navigable row.
    expect(html).toContain('data-navigable="false"');
    expect(html).toContain("orders_never_refunded.sql");
  });
  it("renders the per-file comment counts", () => {
    const html = renderToStaticMarkup(<PrFiles files={buildPrFiles(real)} />);
    expect(html).toContain('data-testid="pr-file-threads"');
  });
  it("an empty file list renders the honest-empty note", () => {
    const html = renderToStaticMarkup(<PrFiles files={buildPrFiles({ baseline: "main", models: [] } as ContextData)} />);
    expect(html).toContain('data-testid="pr-files-empty"');
  });
});

describe("PrTimeline", () => {
  it("renders the per-model comment threads with real authors + bodies", () => {
    const html = renderToStaticMarkup(<PrTimeline timeline={buildCommentTimeline(real)} feed={prTimelineFeed(real)} onOpen={() => {}} />);
    expect(html).toContain('data-testid="view-pr-timeline"');
    expect(html).toContain('data-testid="timeline-thread"');
    expect(html).toContain("dogfood-dev");
    expect(html).toContain("status_variety");
  });
  it("renders the unanchored group", () => {
    const html = renderToStaticMarkup(<PrTimeline timeline={buildCommentTimeline(real)} feed={prTimelineFeed(real)} />);
    expect(html).toContain('data-testid="timeline-group-unanchored"');
  });
  it("the conversation header labels comment count and thread count truthfully (14 vs 11)", () => {
    // honesty: the noun must match the number. `total` is the comment count (14),
    // `threadTotal` the thread count (11) — never the same number labeled twice.
    const html = renderToStaticMarkup(<PrTimeline timeline={buildCommentTimeline(real)} feed={prTimelineFeed(real)} />);
    expect(html).toContain("14 comments");
    expect(html).toContain("11 threads");
  });
  it("renders the HONEST feed panel — not present, with the spine-gap note (no fabricated commit/check)", () => {
    const html = renderToStaticMarkup(<PrTimeline timeline={buildCommentTimeline(real)} feed={prTimelineFeed(real)} />);
    expect(html).toContain('data-testid="pr-timeline-feed"');
    expect(html).toContain('data-present="false"');
    expect(html).toContain('data-testid="feed-spine-gap"');
    expect(html).toContain("not in this context");
    // honest-null checks (the 440 context carries no checks summary).
    expect(html).toContain('data-testid="feed-no-checks"');
  });
  it("renders real checks when the context carries them (no fabrication)", () => {
    const ctx = {
      baseline: "main", models: [],
      pr_ref: { number: 1, title: "t", url: "u", checks: { passed: 5, failed: 0, pending: 1 }, reviewers: [{ login: "a" }] },
      pr_comments: { by_model: [], unanchored: [], total: 0 },
    } as ContextData;
    const html = renderToStaticMarkup(<PrTimeline timeline={buildCommentTimeline(ctx)} feed={prTimelineFeed(ctx)} />);
    expect(html).toContain('data-testid="feed-checks"');
    expect(html).toContain("5 passing");
    expect(html).toContain('data-testid="timeline-empty"');
  });
});
