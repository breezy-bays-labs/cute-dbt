// CommentThread (the nonce-command pattern). The host drives reply/quote/edit
// imperatively by BUMPING a nonce prop (replyNonce / quoteNonce / editTarget.n)
// rather than calling methods — a render-pure command channel. Rendered to
// static markup (no jsdom); the nonce-EFFECT behavior is exercised by the
// Playwright keyboard flow. Here we assert the structural projection.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { CommentThread } from "./CommentThread";
import type { RenderedThread } from "../../domain/context-data";

const thread = (over: Partial<RenderedThread> = {}): RenderedThread => ({
  path: "models/orders.sql",
  line: 12,
  side: "Right",
  comments: [
    { author: "alice", body: "this **looks** good" },
    { author: null, body: "ghost reply" },
  ],
  ...over,
});

const render = (props: Partial<Parameters<typeof CommentThread>[0]> = {}): string =>
  renderToStaticMarkup(<CommentThread thread={thread()} shiki="tokyo-night" {...props} />);

describe("CommentThread", () => {
  it("renders each comment author + markdown body", () => {
    const html = render();
    expect(html).toContain("alice");
    expect(html).toContain("<strong"); // **looks** rendered as markdown
  });

  it("a null author renders as ghost", () => {
    expect(render()).toContain("ghost");
  });

  it("the side badge reflects the anchored side", () => {
    expect(renderToStaticMarkup(<CommentThread thread={thread({ side: "Left" })} shiki="tokyo-night" />)).toContain("old / deletions");
    expect(render()).toContain("new / additions");
  });

  it("a resolved thread shows the resolved chip + an Unresolve affordance", () => {
    const html = renderToStaticMarkup(<CommentThread thread={thread({ resolved: true })} shiki="tokyo-night" onResolve={() => {}} />);
    expect(html).toContain("resolved");
    expect(html).toContain("Unresolve");
  });

  it("the active comment index draws a focus ring", () => {
    const html = render({ activeIdx: 0 });
    expect(html).toContain('data-active-comment="0"');
  });

  it("a body with a ```suggestion renders the SuggestionBlock", () => {
    const t = thread({ comments: [{ author: "bob", body: "```suggestion\nselect a, b\n```" }] });
    const html = renderToStaticMarkup(<CommentThread thread={t} shiki="tokyo-night" snippet="select a" />);
    expect(html).toContain('data-testid="suggestion-block"');
  });

  it("renders a Reply affordance when not resolved", () => {
    expect(render()).toContain('data-testid="thread-reply"');
  });
});
