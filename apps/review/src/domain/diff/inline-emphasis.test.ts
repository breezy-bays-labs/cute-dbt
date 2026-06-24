// Inline (word-level) diff emphasis — the difftastic/GitHub posture: when a
// removed line pairs with an added line, emphasize only the changed tokens.
// Verbatim-port parity with the prototype inlinediff.js, plus property invariants.
import { describe, it, expect } from "vitest";
import fc from "fast-check";
import {
  tokenizeLine,
  lcsOps,
  inlineEmphasis,
  enrichLines,
  pairBlocks,
  type DiffLine,
} from "./inline-emphasis";

describe("tokenizeLine", () => {
  it("splits into whitespace runs · identifiers · single punctuation", () => {
    expect(tokenizeLine("a + b")).toEqual(["a", " ", "+", " ", "b"]);
  });
  it("an empty string yields no tokens", () => {
    expect(tokenizeLine("")).toEqual([]);
  });
  it("joining the tokens reconstructs the input (lossless)", () => {
    fc.assert(
      fc.property(fc.string(), (s) => {
        expect(tokenizeLine(s).join("")).toBe(s);
      }),
    );
  });
});

describe("lcsOps", () => {
  it("identical sequences are all eq", () => {
    const ops = lcsOps(["a", "b"], ["a", "b"]);
    expect(ops.every((o) => o.t === "eq")).toBe(true);
  });
  it("a pure insertion yields del-free ops", () => {
    const ops = lcsOps(["a"], ["a", "b"]);
    expect(ops.some((o) => o.t === "add")).toBe(true);
    expect(ops.some((o) => o.t === "del")).toBe(false);
  });
  it("the eq+del ops reconstruct `a`; the eq+add ops reconstruct `b`", () => {
    fc.assert(
      fc.property(fc.array(fc.constantFrom("a", "b", "c")), fc.array(fc.constantFrom("a", "b", "c")), (a, b) => {
        const ops = lcsOps(a, b);
        const left = ops.filter((o) => o.t !== "add").map((o) => o.s);
        const right = ops.filter((o) => o.t !== "del").map((o) => o.s);
        expect(left).toEqual(a);
        expect(right).toEqual(b);
      }),
    );
  });
});

describe("inlineEmphasis", () => {
  it("identical lines → null (nothing to emphasize)", () => {
    expect(inlineEmphasis("select 1", "select 1")).toBeNull();
  });
  it("a trailing comma lights up only the comma", () => {
    const em = inlineEmphasis("  col_a", "  col_a,");
    expect(em).not.toBeNull();
    // new side has exactly one emphasized range covering the trailing comma.
    const [a, b] = em!.newR[0]!;
    expect("  col_a,".slice(a, b)).toBe(",");
    // old side has no changed range (pure addition).
    expect(em!.oldR).toEqual([]);
  });
  it("wrapping a value emphasizes only the inserted wrapper, not the inner token", () => {
    const em = inlineEmphasis("amount", "round(amount, 2)");
    expect(em).not.toBeNull();
    // the inner `amount` survives as common; the wrapper is the emphasis.
    const newText = "round(amount, 2)";
    const emphasized = em!.newR.map(([a, b]) => newText.slice(a, b)).join("|");
    expect(emphasized).toContain("round(");
    expect(emphasized).not.toBe("amount");
  });
  it("a full rewrite (too dissimilar) → null (calm whole-line tint reads better)", () => {
    expect(inlineEmphasis("select a from x", "delete from y where z")).toBeNull();
  });
  it("emphasis ranges never include leading/trailing whitespace", () => {
    const em = inlineEmphasis("a   b", "a   c");
    expect(em).not.toBeNull();
    for (const [a, b] of em!.newR) {
      expect("a   c"[a]).not.toMatch(/\s/);
      expect("a   c"[b - 1]).not.toMatch(/\s/);
    }
  });
  it("PROPERTY: ranges are within bounds, ascending, non-overlapping, on the correct string", () => {
    const tok = fc.stringMatching(/^[A-Za-z0-9_ ()+,]{0,30}$/);
    fc.assert(
      fc.property(tok, tok, (oldS, newS) => {
        const em = inlineEmphasis(oldS, newS);
        if (em == null) return;
        const check = (ranges: [number, number][], str: string) => {
          let prevEnd = -1;
          for (const [a, b] of ranges) {
            expect(a).toBeGreaterThanOrEqual(0);
            expect(b).toBeLessThanOrEqual(str.length);
            expect(a).toBeLessThan(b);
            expect(a).toBeGreaterThanOrEqual(prevEnd); // ascending, non-overlapping
            prevEnd = b;
          }
        };
        check(em.oldR, oldS);
        check(em.newR, newS);
      }),
    );
  });
});

describe("enrichLines", () => {
  it("attaches emph only to paired del/add runs", () => {
    const lines: DiffLine[] = [
      { t: "ctx", s: "with src as (" },
      { t: "del", s: "  select a" },
      { t: "add", s: "  select a, b" },
      { t: "ctx", s: ")" },
    ];
    const out = enrichLines(lines);
    expect(out[0]!.emph).toBeUndefined(); // context untouched
    expect(out[1]!.emph).toBeDefined(); // del got emphasis
    expect(out[2]!.emph).toBeDefined(); // add got emphasis
    expect(out[3]!.emph).toBeUndefined();
  });
  it("does not mutate the input array", () => {
    const lines: DiffLine[] = [
      { t: "del", s: "x" },
      { t: "add", s: "x y" },
    ];
    enrichLines(lines);
    expect(lines[0]!.emph).toBeUndefined();
  });
  it("an add-only run (no preceding del) gets no emphasis", () => {
    const out = enrichLines([{ t: "add", s: "new line" }]);
    expect(out[0]!.emph).toBeUndefined();
  });
});

describe("pairBlocks", () => {
  it("aligns old→new by index and attaches inline emphasis", () => {
    const { oldLines, newLines } = pairBlocks(["select a"], ["select a, b"]);
    expect(oldLines).toHaveLength(1);
    expect(newLines).toHaveLength(1);
    expect(newLines[0]!.emph).toBeDefined();
  });
  it("an empty new block (deletion) leaves the old lines un-emphasized", () => {
    const { oldLines, newLines } = pairBlocks(["select a"], []);
    expect(oldLines[0]!.emph).toBeUndefined();
    expect(newLines).toEqual([]);
  });
});
