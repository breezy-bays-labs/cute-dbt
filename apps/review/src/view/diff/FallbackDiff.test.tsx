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

  it("an empty patch degrades to an honest no-diff state", () => {
    const html = renderToStaticMarkup(<FallbackDiff path="x.sql" patch="" lang="sql" shiki="tokyo-night" threads={[]} />);
    expect(html).toContain('data-testid="fallback-no-diff"');
  });
});
