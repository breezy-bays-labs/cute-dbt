// patch-nav — parse a unified-diff patch string into the keyboard-nav data the
// Pierre diff surface drives by DATA-LINE NUMBER (not array index, which Pierre
// virtualizes). The change-run starts + max new line number are computed from
// the patch text, never the (virtualized) DOM.
import { describe, it, expect } from "vitest";
import { parsePatchNav, type NavData } from "./patch-nav";

const PATCH = [
  "diff --git a/m.sql b/m.sql",
  "--- a/m.sql",
  "+++ b/m.sql",
  "@@ -1,4 +1,5 @@",
  " with src as (",
  "-  select a",
  "+  select a, b",
  "+  , c",
  "   from x",
  " )",
].join("\n");

describe("parsePatchNav", () => {
  it("emits a change-run start keyed on the ADDED new-side line number", () => {
    const nav: NavData = parsePatchNav(PATCH);
    // the change run starts at new line 2 (the first added line), additions side.
    expect(nav.starts[0]).toEqual({ no: 2, side: "additions" });
  });

  it("maxNo is the largest new-side line number seen", () => {
    const nav = parsePatchNav(PATCH);
    expect(nav.maxNo).toBe(5); // 5 new-side lines (1 ctx + 2 add + 2 ctx)
  });

  it("a delete-only run falls back to the first DELETED old-side line", () => {
    const patch = ["@@ -1,2 +1,1 @@", " keep", "-gone"].join("\n");
    const nav = parsePatchNav(patch);
    expect(nav.starts[0]).toEqual({ no: 2, side: "deletions" });
  });

  it("multiple change runs separated by context yield multiple starts", () => {
    const patch = ["@@ -1,5 +1,5 @@", " a", "-b", "+B", " c", "-d", "+D", " e"].join("\n");
    const nav = parsePatchNav(patch);
    expect(nav.starts).toHaveLength(2);
    expect(nav.starts[0]!.no).toBe(2);
    expect(nav.starts[1]!.no).toBe(4);
  });

  it("an empty patch yields no starts and maxNo 1 (never 0 — a safe floor)", () => {
    const nav = parsePatchNav("");
    expect(nav.starts).toEqual([]);
    expect(nav.maxNo).toBe(1);
  });

  it("ignores diff-header lines (---, +++) — they are not +/- content", () => {
    const nav = parsePatchNav(PATCH);
    // had the header `+++`/`---` been miscounted as add/del content, the first
    // start would not be new-line 2.
    expect(nav.starts[0]!.no).toBe(2);
  });

  it("a trailing newline does not change maxNo or starts (off-by-one guard)", () => {
    // split("\n") leaves a final "" element on a trailing-newline patch; lacking
    // a sigil it would otherwise be counted as a context line, inflating maxNo
    // by one and shifting the new-side gutter.
    const withNl = parsePatchNav(PATCH + "\n");
    const withoutNl = parsePatchNav(PATCH);
    expect(withNl.maxNo).toBe(withoutNl.maxNo);
    expect(withNl.starts).toEqual(withoutNl.starts);
  });
});
