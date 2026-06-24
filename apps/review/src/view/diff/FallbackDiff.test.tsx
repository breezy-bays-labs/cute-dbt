// FallbackDiff — the first-party escape-hatch renderer. Rendered to static
// markup (no jsdom). Asserts it draws real diff rows with data-line gutters +
// mounts threads on the right lines + degrades honestly with no hunks.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { FallbackDiff } from "./FallbackDiff";
import type { RenderedThread } from "../../domain/context-data";

const PATCH = [
  "diff --git a/m.sql b/m.sql",
  "--- a/m.sql",
  "+++ b/m.sql",
  "@@ -1,3 +1,4 @@",
  " with src as (",
  "-  select a",
  "+  select a, b",
  " )",
].join("\n");

const thread: RenderedThread = {
  path: "m.sql",
  line: 2,
  side: "Right",
  comments: [{ author: "alice", body: "nit" }],
};

const render = (threads: RenderedThread[] = []): string =>
  renderToStaticMarkup(<FallbackDiff path="m.sql" patch={PATCH} lang="sql" shiki="tokyo-night" threads={threads} />);

describe("FallbackDiff", () => {
  it("labels itself the fallback renderer", () => {
    expect(render()).toContain("fallback renderer");
    expect(render()).toContain('data-testid="fallback-diff"');
  });

  it("renders rows carrying the new-side data-line number", () => {
    const html = render();
    expect(html).toContain('data-line="1"');
    expect(html).toContain('data-line="2"'); // the added line
  });

  it("renders the hunk header", () => {
    expect(render()).toContain("@@ -1 +1 @@");
  });

  it("mounts a thread on its anchored new-side line", () => {
    const html = render([thread]);
    expect(html).toContain('data-testid="comment-thread"');
    expect(html).toContain("alice");
  });

  it("renders ALL threads on the same line (multiple threads do not overwrite)", () => {
    // two distinct threads anchored to the SAME new-side line (2). The fallback
    // must render BOTH — keying a single RenderedThread per line silently dropped
    // all but the last (gemini HIGH, the diff-cluster bug V1 consumes).
    const first: RenderedThread = { path: "m.sql", line: 2, side: "Right", comments: [{ author: "alice", body: "first" }] };
    const second: RenderedThread = { path: "m.sql", line: 2, side: "Right", comments: [{ author: "bob", body: "second" }] };
    const html = render([first, second]);
    expect(html).toContain("alice");
    expect(html).toContain("first");
    expect(html).toContain("bob");
    expect(html).toContain("second");
    // both threads mount (two comment-thread roots on the one line)
    const count = html.split('data-testid="comment-thread"').length - 1;
    expect(count).toBe(2);
  });

  it("an empty patch degrades to an honest no-diff state", () => {
    const html = renderToStaticMarkup(<FallbackDiff path="x.sql" patch="" lang="sql" shiki="tokyo-night" threads={[]} />);
    expect(html).toContain('data-testid="fallback-no-diff"');
  });
});
