// WriteReview view STRUCTURE (rendered to static markup — the house view-test
// pattern; interactive behavior is covered by the Playwright flow E2E). The
// surface NEVER posts to GitHub: it builds a portable payload (a gh-CLI command
// + copy-JSON) the HOST runs. The component is a thin view over the pure
// `buildReviewPayload` (passed as `onBuild`); it has no network code by
// construction (the honesty / zero-egress contract).
//
// LAYER: view.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { WriteReview } from "./WriteReview";
import type { ReviewPayload, Verdict } from "../../domain/review/review-machine";

const build = (verdict: Verdict, body: string): ReviewPayload => ({
  json: JSON.stringify({ event: verdict.toUpperCase(), body, comments: [] }, null, 2),
  ghCommand: `gh api repos/o/r/pulls/9/reviews --method POST --input -`,
});

const render = (props: Partial<Parameters<typeof WriteReview>[0]> = {}): string =>
  renderToStaticMarkup(
    <WriteReview draftCount={0} onBuild={build} onPublish={() => {}} onClose={() => {}} {...props} />,
  );

describe("WriteReview — the portable write-review surface", () => {
  it("renders the surface shell + verdict picker + body + publish/close", () => {
    const html = render();
    expect(html).toContain('data-testid="write-review"');
    expect(html).toContain('data-testid="write-review-verdict-approve"');
    expect(html).toContain('data-testid="write-review-verdict-request"');
    expect(html).toContain('data-testid="write-review-verdict-comment"');
    expect(html).toContain('data-testid="write-review-body"');
    expect(html).toContain('data-testid="write-review-publish"');
    expect(html).toContain('data-testid="write-review-close"');
  });

  it("shows the REAL pending-draft count (never fabricated)", () => {
    expect(render({ draftCount: 3 })).toContain("3");
    const html = render({ draftCount: 0 });
    expect(html).toContain('data-testid="write-review-draftcount"');
  });

  it("renders the portable gh command + copy-JSON (the payload the HOST runs)", () => {
    const html = render({ draftCount: 1 });
    expect(html).toContain('data-testid="write-review-gh-command"');
    expect(html).toContain("gh api repos/o/r/pulls/9/reviews");
    expect(html).toContain('data-testid="write-review-json"');
    expect(html).toContain("&quot;event&quot;"); // the JSON renders into the textarea
  });

  it("the default verdict is comment (the safe non-blocking default)", () => {
    // the comment payload event renders by default (no verdict click needed).
    expect(render()).toContain("COMMENT");
  });

  it("carries an explicit local-first note that cute-dbt NEVER posts (honesty)", () => {
    expect(render().toLowerCase()).toContain("never");
    // and no network primitive (URL/protocol) is present in the rendered surface.
    expect(render()).not.toMatch(/https?:\/\//);
  });
});
