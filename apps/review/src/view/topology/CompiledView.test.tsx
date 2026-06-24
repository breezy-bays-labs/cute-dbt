// CompiledView static-render tests (S6b). The vitest env is `node` (no jsdom),
// so we render to static markup (react-dom/server) and assert the SYNCHRONOUS
// structure: the honest-empty state, the per-line gutter, the span tint, and the
// cursor marker. The async highlight + the DIRECT-scroll/ring-flash EFFECTS run
// only in a real DOM — those are covered by the Playwright e2e (tests/
// topology-panes.spec.ts). `noHighlight` keeps the render purely structural here.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { CompiledView } from "./CompiledView";

const render = (props: Parameters<typeof CompiledView>[0]): string =>
  renderToStaticMarkup(<CompiledView {...props} />);

describe("CompiledView — honest-empty when there is no source", () => {
  it("renders the no-code_map state (never a fabricated listing) for empty text", () => {
    const html = render({ text: "", lang: "sql", shiki: "tokyo-night", noHighlight: true });
    expect(html).toContain('data-testid="compiled-view-empty"');
    expect(html).toContain("code_map");
    // no code lines at all — the honest-empty path renders no listing.
    expect(html).not.toContain('data-testid="code-line"');
  });
});

describe("CompiledView — line gutter + span tint + cursor marker", () => {
  const text = "select 1\nfrom t\nwhere x = 2\ngroup by 1";

  it("renders one code-line row per source line with 1-based gutter numbers", () => {
    const html = render({ text, lang: "sql", shiki: "tokyo-night", noHighlight: true });
    const rows = html.match(/data-testid="code-line"/g) ?? [];
    expect(rows.length).toBe(text.split("\n").length);
    // 1-based line attributes present.
    expect(html).toContain('data-line="1"');
    expect(html).toContain('data-line="4"');
  });

  it("tints exactly the lines inside the span (inclusive of both endpoints)", () => {
    const html = render({
      text, lang: "sql", shiki: "tokyo-night", noHighlight: true,
      span: { start: { line: 2 }, end: { line: 3 } },
    });
    // lines 2 and 3 are in-span; 1 and 4 are not.
    expect(html).toMatch(/data-line="2"[^>]*data-in-span="true"/);
    expect(html).toMatch(/data-line="3"[^>]*data-in-span="true"/);
    expect(html).toMatch(/data-line="1"[^>]*data-in-span="false"/);
    expect(html).toMatch(/data-line="4"[^>]*data-in-span="false"/);
  });

  it("marks the keyboard cursor line (and only it)", () => {
    const html = render({
      text, lang: "sql", shiki: "tokyo-night", noHighlight: true,
      cursorLine: 3,
    });
    expect(html).toMatch(/data-line="3"[^>]*data-cursor="true"/);
    expect(html).toMatch(/data-line="1"[^>]*data-cursor="false"/);
    // the cursor marker glyph is rendered on the active row.
    expect(html).toContain("▸");
  });

  it("a null span tints nothing (no false selection)", () => {
    const html = render({ text, lang: "sql", shiki: "tokyo-night", noHighlight: true, span: null });
    expect(html).not.toContain('data-in-span="true"');
  });

  it("renders plain-text line content before tokens resolve (loud-fail-safe fallback)", () => {
    const html = render({ text, lang: "sql", shiki: "tokyo-night", noHighlight: true });
    expect(html).toContain("select 1");
    expect(html).toContain("group by 1");
  });
});
