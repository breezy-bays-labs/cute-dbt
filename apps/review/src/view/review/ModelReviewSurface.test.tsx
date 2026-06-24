// ModelReviewSurface view STRUCTURE (static markup — the house view-test
// pattern). This is the Models code/diff REVIEWABLE surface (V1): the changed
// files' diffs + a draft composer + the reviewed-state indicator. It makes
// Models reviewable end-to-end — the council MUST-FIX D thin vertical. Interactive
// behavior (drafting + mark-reviewed) is covered by the Playwright flow E2E.
//
// LAYER: view.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ModelReviewSurface } from "./ModelReviewSurface";
import type { ReviewContext } from "../../domain/reshape";

const ctx: ReviewContext = {
  id: "customers",
  kind: "model",
  name: "customers",
  path: "models/customers.sql",
  files: [
    {
      path: "models/customers.sql",
      lang: "sql",
      patch:
        "diff --git a/models/customers.sql b/models/customers.sql\n" +
        "--- a/models/customers.sql\n+++ b/models/customers.sql\n" +
        "@@ -1,2 +1,2 @@\n select id\n-from raw\n+from raw_customers\n",
      threads: [],
    },
  ],
};

const render = (props: Partial<Parameters<typeof ModelReviewSurface>[0]> = {}): string =>
  renderToStaticMarkup(
    <ModelReviewSurface
      model="customers"
      ctx={ctx}
      shiki="tokyo-night"
      reviewers={[]}
      reviewed={false}
      draftCount={0}
      onDraft={() => {}}
      onMarkReviewed={() => {}}
      forceFallback
      {...props}
    />,
  );

describe("ModelReviewSurface — the Models reviewable surface (V1)", () => {
  it("renders the model heading + the changed-file diff(s)", () => {
    const html = render();
    expect(html).toContain('data-testid="model-review-surface"');
    expect(html).toContain('data-model="customers"');
    // the diff renders (forceFallback → the first-party fallback diff).
    expect(html).toContain('data-testid="review-file"');
    expect(html).toContain("models/customers.sql");
  });

  it("renders the draft composer (the keyboard-reachable comment surface)", () => {
    expect(render()).toContain('data-testid="review-draft-composer"');
    expect(render()).toContain('data-testid="composer-textarea"');
  });

  it("shows an UNREVIEWED indicator + a Mark-reviewed affordance when not reviewed", () => {
    const html = render({ reviewed: false });
    expect(html).toContain('data-testid="review-state-chip"');
    expect(html).toContain('data-reviewed="false"');
    expect(html).toContain('data-testid="mark-reviewed-btn"');
  });

  it("shows a REVIEWED indicator when reviewed (the real state, never fabricated)", () => {
    const html = render({ reviewed: true });
    expect(html).toContain('data-reviewed="true"');
    expect(html.toLowerCase()).toContain("reviewed");
  });

  it("surfaces the REAL pending-draft count for the model", () => {
    expect(render({ draftCount: 2 })).toContain('data-draft-count="2"');
  });

  it("renders an honest empty-state when the model has no changed files", () => {
    const empty: ReviewContext = { ...ctx, files: [] };
    const html = render({ ctx: empty });
    expect(html).toContain('data-testid="review-no-files"');
  });
});
