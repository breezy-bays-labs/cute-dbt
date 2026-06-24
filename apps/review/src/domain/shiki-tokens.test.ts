// The ONE Shiki singleton's token API — splits a highlighted line into tokens
// with the syntax color preserved, then overlays inline word-emphasis ranges.
// The emphasis-overlay fold (splitting a colored token by char-ranges) is PURE
// and lives here so it's unit-testable without a real highlighter.
import { describe, it, expect } from "vitest";
import { overlayEmphasis, type ShikiToken } from "./shiki-tokens";

describe("overlayEmphasis", () => {
  const toks: ShikiToken[] = [
    { text: "select", color: "#c0caf5" },
    { text: " ", color: "#a9b1d6" },
    { text: "a", color: "#7aa2f7" },
  ];

  it("with no ranges returns one span per token, color preserved", () => {
    const spans = overlayEmphasis(toks, []);
    expect(spans).toEqual([
      { text: "select", color: "#c0caf5", emph: false },
      { text: " ", color: "#a9b1d6", emph: false },
      { text: "a", color: "#7aa2f7", emph: false },
    ]);
  });

  it("splits a token at an emphasis boundary, keeping the color", () => {
    // emphasize chars [0,3) — i.e. "sel" of "select".
    const spans = overlayEmphasis(toks, [[0, 3]]);
    expect(spans[0]).toEqual({ text: "sel", color: "#c0caf5", emph: true });
    expect(spans[1]).toEqual({ text: "ect", color: "#c0caf5", emph: false });
  });

  it("emphasizes a whole later token (char range spanning it)", () => {
    // "select a" → emphasize the final "a" at char index 7.
    const spans = overlayEmphasis(toks, [[7, 8]]);
    const aSpan = spans.find((s) => s.text === "a");
    expect(aSpan?.emph).toBe(true);
  });

  it("the concatenated span text equals the concatenated token text (lossless)", () => {
    const spans = overlayEmphasis(toks, [[2, 5]]);
    expect(spans.map((s) => s.text).join("")).toBe("select a");
  });

  it("an empty token list yields no spans", () => {
    expect(overlayEmphasis([], [[0, 3]])).toEqual([]);
  });
});
