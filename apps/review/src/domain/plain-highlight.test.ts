import { describe, it, expect } from "vitest";
import fc from "fast-check";
import { highlightSQL, highlightYAML, highlightPlain } from "./plain-highlight";

describe("highlightSQL", () => {
  it("classifies a keyword", () => {
    const toks = highlightSQL("select a");
    expect(toks[0]!).toEqual({ text: "select", cls: "kw" });
  });
  it("classifies a jinja block", () => {
    const toks = highlightSQL("{{ ref('x') }}");
    expect(toks[0]!.cls).toBe("jinja");
  });
  it("classifies a line comment", () => {
    const toks = highlightSQL("select 1 -- note");
    expect(toks.some((t) => t.cls === "cmt")).toBe(true);
  });
  it("classifies a function call", () => {
    const toks = highlightSQL("sum(x)");
    expect(toks[0]).toEqual({ text: "sum", cls: "fn" });
  });
});

describe("highlightYAML", () => {
  it("classifies a key/value", () => {
    const toks = highlightYAML("  name: orders");
    expect(toks.find((t) => t.cls === "key")?.text).toBe("name");
  });
  it("classifies a comment tail", () => {
    const toks = highlightYAML("x: 1 # c");
    expect(toks.some((t) => t.cls === "cmt")).toBe(true);
  });
});

describe("highlightPlain", () => {
  it("routes yaml/yml to the yaml highlighter", () => {
    expect(highlightPlain("a: 1", "yaml").some((t) => t.cls === "key")).toBe(true);
  });
  it("an unknown language is plain (one token)", () => {
    expect(highlightPlain("anything", "python")).toEqual([{ text: "anything", cls: "" }]);
  });
  it("PROPERTY: tokens concatenate back to the input (lossless)", () => {
    fc.assert(
      fc.property(fc.string(), fc.constantFrom("sql", "yaml", "python"), (s, lang) => {
        const oneLine = s.replace(/\n/g, " ");
        expect(highlightPlain(oneLine, lang).map((t) => t.text).join("")).toBe(oneLine);
      }),
    );
  });
});
