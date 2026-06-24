// Composer view structure (rendered to static markup). The TEXT TRANSFORM logic
// is unit-tested in domain/diff/composer-transforms.test.ts; here we assert the
// structural affordances render (tabs, toolbar, submit, conditional suggest /
// delete / cancel).
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { Composer } from "./Composer";

const render = (props: Partial<Parameters<typeof Composer>[0]> = {}): string =>
  renderToStaticMarkup(<Composer shiki="tokyo-night" onSubmit={() => {}} {...props} />);

describe("Composer", () => {
  it("renders Write/Preview tabs with Write active by default", () => {
    const html = render();
    expect(html).toContain('data-testid="composer-tab-write"');
    expect(html).toContain('data-testid="composer-tab-preview"');
    expect(html).toContain('data-testid="composer-tab-write" data-active="true"');
  });

  it("renders the formatting toolbar + textarea + submit", () => {
    const html = render({ submitLabel: "Reply" });
    expect(html).toContain('data-testid="composer-toolbar"');
    expect(html).toContain('data-testid="composer-textarea"');
    expect(html).toContain('data-testid="composer-submit"');
    expect(html).toContain("Reply");
  });

  it("shows ± Suggest a change ONLY when a suggestionSeed is present", () => {
    expect(render({ suggestionSeed: "select a" })).toContain('data-testid="composer-suggest"');
    expect(render({})).not.toContain('data-testid="composer-suggest"');
  });

  it("shows Delete only when onDelete is wired", () => {
    expect(render({ onDelete: () => {} })).toContain('data-testid="composer-delete"');
    expect(render({})).not.toContain('data-testid="composer-delete"');
  });

  it("shows Cancel only when onCancel is wired", () => {
    expect(render({ onCancel: () => {} })).toContain('data-testid="composer-cancel"');
  });

  it("seeds the textarea with initialValue", () => {
    expect(render({ initialValue: "> quoted\n\n" })).toContain("&gt; quoted");
  });
});
