// SuggestionBlock renders a ```suggestion as a "Suggested change" diff: the
// anchored (old) lines struck, the proposed (new) lines added — with the same
// per-line highlight + word-emphasis as the main diff. Rendered to static
// markup (react-dom/server, no jsdom) — the project's view-test posture.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { SuggestionBlock } from "./SuggestionBlock";

const render = (props: Parameters<typeof SuggestionBlock>[0]): string => renderToStaticMarkup(<SuggestionBlock {...props} />);

describe("SuggestionBlock", () => {
  it("labels itself a Suggested change", () => {
    const html = render({ oldCode: "select a", newCode: "select a, b", lang: "sql", shiki: "tokyo-night" });
    expect(html).toContain("Suggested change");
    expect(html).toContain('data-testid="suggestion-block"');
  });

  it("renders the old (removed) line and the new (added) line", () => {
    const html = render({ oldCode: "select a", newCode: "select a, b", lang: "sql", shiki: "tokyo-night" });
    expect(html).toContain('data-side="del"');
    expect(html).toContain('data-side="add"');
  });

  it("an empty suggestion (deletion) shows a lines-removed marker, no add rows", () => {
    const html = render({ oldCode: "select a", newCode: "", lang: "sql", shiki: "tokyo-night" });
    expect(html).toContain("lines removed");
    expect(html).not.toContain('data-side="add"');
  });

  it("a null oldCode (no anchored snippet) still renders the proposed lines", () => {
    const html = render({ oldCode: null, newCode: "select 1", lang: "sql", shiki: "tokyo-night" });
    expect(html).toContain('data-side="add"');
  });
});
