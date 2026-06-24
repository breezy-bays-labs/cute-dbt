// GitHub ```suggestion fenced-block parsing — splits a comment body into
// plain-markdown + suggestion segments so the latter renders as a diff against
// the anchored snippet. Verbatim-port parity with prototype markdown.js.
import { describe, it, expect } from "vitest";
import { hasSuggestion, splitSuggestions } from "./suggestion";

describe("hasSuggestion", () => {
  it("detects a ```suggestion fence", () => {
    expect(hasSuggestion("text\n```suggestion\nx\n```\nmore")).toBe(true);
  });
  it("is false for a plain ``` fence", () => {
    expect(hasSuggestion("```sql\nselect 1\n```")).toBe(false);
  });
  it("is false for empty / null", () => {
    expect(hasSuggestion("")).toBe(false);
    expect(hasSuggestion(null as unknown as string)).toBe(false);
  });
  it("is case-insensitive on the keyword", () => {
    expect(hasSuggestion("```SUGGESTION\nx\n```")).toBe(true);
  });
});

describe("splitSuggestions", () => {
  it("splits md + suggestion + md in order", () => {
    const segs = splitSuggestions("before\n```suggestion\nselect 1\n```\nafter");
    expect(segs.map((s) => s.type)).toEqual(["md", "suggestion", "md"]);
    expect(segs[0]).toMatchObject({ type: "md", text: "before" });
    expect(segs[1]).toMatchObject({ type: "suggestion", code: "select 1" });
    expect(segs[2]).toMatchObject({ type: "md", text: "after" });
  });
  it("a body with only a suggestion yields one suggestion segment", () => {
    const segs = splitSuggestions("```suggestion\nx\n```");
    expect(segs).toHaveLength(1);
    expect(segs[0]).toMatchObject({ type: "suggestion", code: "x" });
  });
  it("a plain body yields one md segment", () => {
    const segs = splitSuggestions("just a comment");
    expect(segs).toEqual([{ type: "md", text: "just a comment" }]);
  });
  it("an empty suggestion (deletion) carries an empty code string", () => {
    const segs = splitSuggestions("```suggestion\n```");
    expect(segs).toEqual([{ type: "suggestion", code: "" }]);
  });
  it("whitespace-only md between fences is dropped", () => {
    const segs = splitSuggestions("```suggestion\na\n```\n\n```suggestion\nb\n```");
    expect(segs.map((s) => s.type)).toEqual(["suggestion", "suggestion"]);
  });
});
