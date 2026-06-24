// Markdown + CommentBody — react-markdown + remark-gfm + rehype-sanitize, plus
// the first-party suggestion split. Rendered to static markup (no jsdom).
// rehype-sanitize is the load-bearing safety: raw HTML / scripts are stripped.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { Markdown, CommentBody } from "./Markdown";

const md = (text: string): string => renderToStaticMarkup(<Markdown text={text} />);

describe("Markdown", () => {
  it("renders gfm: a table", () => {
    const html = md("| a | b |\n|---|---|\n| 1 | 2 |");
    expect(html).toContain("<table");
    expect(html).toContain("<td");
  });

  it("renders gfm: a task list", () => {
    const html = md("- [x] done\n- [ ] todo");
    expect(html).toContain('type="checkbox"');
  });

  it("renders inline code + bold", () => {
    const html = md("use `ref()` and **bold**");
    expect(html).toContain("<code");
    expect(html).toContain("<strong");
  });

  it("SANITIZES raw HTML (rehype-sanitize strips the <script> tag; inert text may remain)", () => {
    const html = md("hi <script>alert(1)</script> there");
    // the executable TAG is gone (the load-bearing property). The script's inner
    // text rendering as inert prose is harmless — what matters is no live tag.
    expect(html).not.toContain("<script");
    expect(html).not.toContain("</script>");
  });

  it("SANITIZES an inline event handler (no onerror/onclick survives)", () => {
    const html = md('<img src=x onerror="alert(1)">');
    expect(html).not.toContain("onerror");
  });

  it("SANITIZES a javascript: link href", () => {
    const html = md("[click](javascript:alert(1))");
    expect(html).not.toContain("javascript:alert");
  });

  it("an empty / null body renders nothing harmful", () => {
    expect(() => md("")).not.toThrow();
    expect(renderToStaticMarkup(<Markdown text={null as unknown as string} />)).toBeDefined();
  });

  it("LOCAL-FIRST: a remote markdown image is rendered INERT, never a fetching <img src>", () => {
    // the exact dogfood case: a gemini/CodeRabbit badge image in a review body.
    const html = md("![medium](https://www.gstatic.com/codereviewagent/medium-priority.svg)");
    // no fetching <img src=…> tag (the zero-egress violation the gate caught).
    expect(html).not.toMatch(/<img[^>]+src=/);
    // an honest placeholder names the asset without loading it (never-a-false-claim).
    expect(html).toContain('data-testid="md-image-placeholder"');
    expect(html).toContain("not loaded");
  });

  it("LOCAL-FIRST: a markdown link does not become a navigating/prefetching <a href>", () => {
    const html = md("[docs](https://example.com/x)");
    expect(html).not.toMatch(/<a[^>]+href=/);
    expect(html).toContain('data-testid="md-link"');
    expect(html).toContain("docs"); // the link TEXT still renders
  });
});

describe("CommentBody", () => {
  it("a plain body renders as markdown", () => {
    const html = renderToStaticMarkup(<CommentBody text="**hi**" shiki="tokyo-night" />);
    expect(html).toContain("<strong");
  });

  it("a body with a ```suggestion renders the SuggestionBlock", () => {
    const html = renderToStaticMarkup(
      <CommentBody text={"please\n```suggestion\nselect a, b\n```"} snippet="select a" shiki="tokyo-night" />,
    );
    expect(html).toContain('data-testid="suggestion-block"');
    expect(html).toContain("Suggested change");
    // and the leading prose still renders.
    expect(html).toContain("please");
  });
});
