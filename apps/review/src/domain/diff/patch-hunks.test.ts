// patch-hunks — parse a unified-diff patch into structured hunks for the
// FIRST-PARTY fallback renderer (the Pierre escape hatch). The fallback needs
// real rows (old/new gutter numbers + add/del/ctx kind) to draw a GitHub-style
// diff without Pierre. Fail-closed: never throws on malformed input.
import { describe, it, expect } from "vitest";
import { parsePatchHunks } from "./patch-hunks";

const PATCH = [
  "diff --git a/m.sql b/m.sql",
  "--- a/m.sql",
  "+++ b/m.sql",
  "@@ -1,3 +1,4 @@",
  " with src as (",
  "-  select a",
  "+  select a, b",
  " )",
].join("\n");

describe("parsePatchHunks", () => {
  it("parses hunk start lines + body rows with correct kinds", () => {
    const hunks = parsePatchHunks(PATCH);
    expect(hunks).toHaveLength(1);
    expect(hunks[0]!.oldStart).toBe(1);
    expect(hunks[0]!.newStart).toBe(1);
    expect(hunks[0]!.lines.map((l) => l.t)).toEqual(["ctx", "del", "add", "ctx"]);
  });

  it("assigns ascending old/new line numbers", () => {
    const h = parsePatchHunks(PATCH)[0]!;
    // ctx: old1/new1 ; del: old2 ; add: new2 ; ctx: old3/new3
    const ctx2 = h.lines[3]!;
    expect(ctx2.oldNo).toBe(3);
    expect(ctx2.newNo).toBe(3);
  });

  it("an empty / header-only patch yields no hunks", () => {
    expect(parsePatchHunks("")).toEqual([]);
    expect(parsePatchHunks("diff --git a/x b/x\n--- a/x\n+++ b/x")).toEqual([]);
  });

  it("commentable new-side lines = context + additions (not deletions)", () => {
    const h = parsePatchHunks(PATCH)[0]!;
    const commentable = h.lines.filter((l) => l.t !== "del").map((l) => l.newNo);
    expect(commentable).toEqual([1, 2, 3]);
  });

  it("does not throw on malformed hunk headers", () => {
    expect(() => parsePatchHunks("@@ garbage @@\n+x")).not.toThrow();
  });
});
