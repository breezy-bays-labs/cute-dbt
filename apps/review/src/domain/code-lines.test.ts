// highlightLines — per-line Shiki tokenization for the topology panes. Asserts
// the row-count == line-count invariant (the gutter↔span-line lock-step) and the
// lossless round-trip (concatenated token text reproduces each source line).
import { describe, it, expect } from "vitest";
import { highlightLines } from "./code-lines";

describe("highlightLines — one colored-token row per source line", () => {
  it("returns exactly one row per line (gutter ↔ spine line lock-step)", async () => {
    const code = "select 1\nfrom t\nwhere x = 2";
    const rows = await highlightLines(code, "sql", "tokyo-night");
    expect(rows.length).toBe(code.split("\n").length);
  });

  it("round-trips: concatenated token text reproduces each source line", async () => {
    const code = "with a as (\n  select id\n)\nselect * from a";
    const rows = await highlightLines(code, "sql", "tokyo-night");
    const lines = code.split("\n");
    rows.forEach((row, i) => {
      expect(row.map((t) => t.text).join("")).toBe(lines[i]);
    });
  });

  it("assigns syntax colors to keyword tokens (genuine highlight, not plain)", async () => {
    const rows = await highlightLines("select 1", "sql", "tokyo-night");
    const colored = rows.flat().some((t) => typeof t.color === "string" && t.color.length > 0);
    expect(colored).toBe(true);
  });

  it("an empty string yields a single (empty) row — never zero rows", async () => {
    const rows = await highlightLines("", "sql", "tokyo-night");
    expect(rows.length).toBe(1);
  });

  it("rejects loudly on an unregistered theme (no silent fallback)", async () => {
    await expect(highlightLines("select 1", "sql", "not-a-real-theme")).rejects.toBeTruthy();
  });
});
