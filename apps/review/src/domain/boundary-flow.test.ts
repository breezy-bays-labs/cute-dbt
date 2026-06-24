// Cross-file boundary-flow tests (S6b) — nextFileOnBoundary. Pins the
// skip-empty-comment-files rule + the hard-stop-at-edge contract.
import { describe, it, expect } from "vitest";
import { nextFileOnBoundary } from "./boundary-flow";

// 5 files; only indices 1 and 3 carry comments.
const HAS_COMMENTS = (i: number): boolean => i === 1 || i === 3;
const TOTAL = 5;

describe("nextFileOnBoundary — block/hunk step the immediate neighbor", () => {
  it("flows down to the immediate next file (block)", () => {
    expect(nextFileOnBoundary(0, "down", "block", TOTAL, HAS_COMMENTS)).toBe(1);
    expect(nextFileOnBoundary(2, "down", "hunk", TOTAL, HAS_COMMENTS)).toBe(3);
  });
  it("flows up to the immediate previous file", () => {
    expect(nextFileOnBoundary(3, "up", "block", TOTAL, HAS_COMMENTS)).toBe(2);
  });
  it("returns null at the bottom/top edge (hard stop, no wrap)", () => {
    expect(nextFileOnBoundary(4, "down", "block", TOTAL, HAS_COMMENTS)).toBeNull();
    expect(nextFileOnBoundary(0, "up", "block", TOTAL, HAS_COMMENTS)).toBeNull();
  });
});

describe("nextFileOnBoundary — comment flow SKIPS empty-comment files", () => {
  it("from file 0 down → skips to file 1 (the next with comments)", () => {
    expect(nextFileOnBoundary(0, "down", "comment", TOTAL, HAS_COMMENTS)).toBe(1);
  });
  it("from file 1 down → skips empty file 2, lands on file 3", () => {
    expect(nextFileOnBoundary(1, "down", "comment", TOTAL, HAS_COMMENTS)).toBe(3);
  });
  it("from file 3 down → no later file has comments → null (hard stop)", () => {
    expect(nextFileOnBoundary(3, "down", "comment", TOTAL, HAS_COMMENTS)).toBeNull();
  });
  it("from file 3 up → skips empty file 2, lands on file 1", () => {
    expect(nextFileOnBoundary(3, "up", "comment", TOTAL, HAS_COMMENTS)).toBe(1);
  });
  it("from file 1 up → no earlier file has comments → null", () => {
    expect(nextFileOnBoundary(1, "up", "comment", TOTAL, HAS_COMMENTS)).toBeNull();
  });
});
