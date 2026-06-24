// Fuzz the unified-diff patch parser (the Q4 mandate — the TS analog of the
// Rust `--pr-diff` bolero target). The patch string is the highest-risk
// untrusted-input surface in the diff cluster. The fail-closed contract:
//   - never throws / never hangs;
//   - always returns a well-formed NavData (starts ascending, maxNo >= 1);
//   - every start references a real positive line number on a valid side.
import { describe, it, expect } from "vitest";
import fc from "fast-check";
import { parsePatchNav } from "./patch-nav";

// Generators that look LIKE patch text — hunk headers, +/-/space lines, junk.
const hunkHeader = fc
  .tuple(fc.integer({ min: 0, max: 9999 }), fc.integer({ min: 0, max: 999 }), fc.integer({ min: 0, max: 9999 }), fc.integer({ min: 0, max: 999 }))
  .map(([os, oc, ns, nc]) => `@@ -${os},${oc} +${ns},${nc} @@`);
const contentLine = fc.oneof(
  fc.string().map((s) => " " + s),
  fc.string().map((s) => "+" + s),
  fc.string().map((s) => "-" + s),
  fc.string(), // raw junk (no sigil)
  hunkHeader,
  fc.constant("--- a/x"),
  fc.constant("+++ b/x"),
  fc.constant("@@ malformed"),
  fc.constant("\\ No newline at end of file"),
);

// A hunk header opens a fresh gutter. The HUNK_RE the parser uses is mirrored
// here so the test can partition the patch into single-hunk slices and assert
// the within-hunk ordering invariant per slice (the gutter resets across hunks,
// so monotonicity is only meaningful within one hunk and per side).
const HUNK_HEADER_RE = /^@@ -\d+(?:,\d+)? \+\d+(?:,\d+)? @@/;

describe("parsePatchNav — fuzz (fail-closed)", () => {
  it("never throws, always returns well-formed NavData", () => {
    fc.assert(
      fc.property(fc.array(contentLine, { maxLength: 80 }), (lines) => {
        const patch = lines.join("\n");
        const nav = parsePatchNav(patch);
        expect(nav.maxNo).toBeGreaterThanOrEqual(1);
        expect(Number.isInteger(nav.maxNo)).toBe(true);
        for (const st of nav.starts) {
          expect(st.side === "additions" || st.side === "deletions").toBe(true);
          expect(Number.isInteger(st.no)).toBe(true);
          expect(st.no).toBeGreaterThanOrEqual(1);
        }
      }),
      { numRuns: 600 },
    );
  });

  it("starts ascend within a contiguous hunk run, per side (reset across hunks)", () => {
    fc.assert(
      fc.property(fc.array(contentLine, { maxLength: 80 }), (lines) => {
        // Partition the patch into one slice per hunk (each slice = the header
        // through the line before the next header). Parsing each slice in
        // isolation gives the starts for exactly that hunk, with a fresh gutter.
        const slices: string[][] = [];
        let cur: string[] | null = null;
        for (const raw of lines) {
          if (HUNK_HEADER_RE.test(raw)) {
            cur = [raw];
            slices.push(cur);
          } else if (cur) {
            cur.push(raw);
          }
        }
        for (const slice of slices) {
          const { starts } = parsePatchNav(slice.join("\n"));
          // Within one hunk the parser advances each side's gutter monotonically,
          // so the start `no` values must be NON-DECREASING per side in emission
          // order. An out-of-order-start regression (e.g. emitting a run anchor
          // before advancing the gutter) would break this. Cross-side starts use
          // different gutters (new-side vs old-side) — compare each side alone.
          let prevAdd = -Infinity;
          let prevDel = -Infinity;
          for (const st of starts) {
            if (st.side === "additions") {
              expect(st.no).toBeGreaterThanOrEqual(prevAdd);
              prevAdd = st.no;
            } else {
              expect(st.no).toBeGreaterThanOrEqual(prevDel);
              prevDel = st.no;
            }
          }
        }
      }),
      { numRuns: 600 },
    );
  });

  it("terminates promptly even on a huge degenerate patch", () => {
    const patch = Array.from({ length: 5000 }, (_, i) => (i % 2 ? "+x" : "-y")).join("\n");
    const t0 = Date.now();
    const nav = parsePatchNav(patch);
    expect(Date.now() - t0).toBeLessThan(1000);
    expect(nav.maxNo).toBeGreaterThanOrEqual(1);
  });
});
