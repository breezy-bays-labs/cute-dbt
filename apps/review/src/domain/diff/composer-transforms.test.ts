// Pure text transforms for the comment Composer toolbar. Each transform takes
// (text, selStart, selEnd) and returns the new text + the new selection. Kept
// pure (no DOM) so the markdown round-trips are unit-testable. Verbatim-port
// parity with prototype composer.js.
import { describe, it, expect } from "vitest";
import {
  wrapSelection,
  prefixLines,
  insertLink,
  insertSuggestion,
  matchMention,
  applyMention,
  type Transform,
} from "./composer-transforms";

const at = (text: string, sel: number): { text: string; selStart: number; selEnd: number } => ({
  text,
  selStart: sel,
  selEnd: sel,
});

describe("wrapSelection", () => {
  it("wraps the selected text with pre/post markers", () => {
    const r = wrapSelection({ text: "make bold", selStart: 5, selEnd: 9 }, "**", "**", "bold text");
    expect(r.text).toBe("make **bold**");
    // selection lands on the wrapped body
    expect(r.text.slice(r.selStart, r.selEnd)).toBe("bold");
  });
  it("with no selection inserts the placeholder, selected", () => {
    const r = wrapSelection(at("", 0), "**", "**", "bold text");
    expect(r.text).toBe("**bold text**");
    expect(r.text.slice(r.selStart, r.selEnd)).toBe("bold text");
  });
});

describe("prefixLines", () => {
  it("prefixes each selected line", () => {
    const r = prefixLines({ text: "a\nb", selStart: 0, selEnd: 3 }, (l) => "> " + l);
    expect(r.text).toBe("> a\n> b");
  });
  it("inserts a leading newline when not at a line start", () => {
    const r = prefixLines({ text: "intro x", selStart: 6, selEnd: 7 }, (l) => "- " + l);
    expect(r.text).toBe("intro \n- x");
  });
  it("numbered lists pass the index", () => {
    const r = prefixLines({ text: "a\nb", selStart: 0, selEnd: 3 }, (l, i) => `${i + 1}. ${l}`);
    expect(r.text).toBe("1. a\n2. b");
  });
});

describe("insertLink", () => {
  it("wraps the selection as a markdown link with the url selected", () => {
    const r = insertLink({ text: "see docs", selStart: 4, selEnd: 8 });
    expect(r.text).toBe("see [docs](url)");
    expect(r.text.slice(r.selStart, r.selEnd)).toBe("url");
  });
});

describe("insertSuggestion", () => {
  it("opens a ```suggestion block seeded with the anchored snippet", () => {
    const r = insertSuggestion({ text: "", selStart: 0, selEnd: 0 }, "  select a");
    expect(r.text).toContain("```suggestion\n  select a\n```");
    expect(r.text.slice(r.selStart, r.selEnd)).toBe("  select a");
  });
  it("a null seed opens an empty suggestion body", () => {
    const r = insertSuggestion({ text: "note", selStart: 4, selEnd: 4 }, null);
    expect(r.text).toContain("```suggestion\n\n```");
  });
});

describe("matchMention", () => {
  it("detects an @-token at the caret (from points AT the @ sigil)", () => {
    const m = matchMention("hey @al", 7);
    expect(m).toEqual({ from: 4, query: "al" });
  });
  it("matches a bare @ (empty query)", () => {
    expect(matchMention("@", 1)).toEqual({ from: 0, query: "" });
  });
  it("returns null when not in an @-token", () => {
    expect(matchMention("hello world", 11)).toBeNull();
    expect(matchMention("a@b c", 5)).toBeNull(); // @ not preceded by start/space
  });
});

describe("applyMention", () => {
  it("replaces the @-token with @name + a trailing space (round-trips as a mention)", () => {
    const r = applyMention("hey @al", 7, { from: 4, query: "al" }, "alice");
    expect(r.text).toBe("hey @alice ");
    expect(r.selStart).toBe("hey @alice ".length);
  });
  it("preserves text after the caret verbatim", () => {
    const r = applyMention("hey @almore", 7, { from: 4, query: "al" }, "alice");
    expect(r.text).toBe("hey @alice more");
  });
});

describe("Transform is composable (text is always a string)", () => {
  it("a transform never returns undefined text", () => {
    const t: Transform = (s) => wrapSelection(s, "_", "_", "i");
    const r = t({ text: "x", selStart: 0, selEnd: 1 });
    expect(typeof r.text).toBe("string");
  });
});
