// patch-nav — derive keyboard-nav data from a unified-diff patch string.
//
// Pierre VIRTUALIZES its rows, so absolute array indices are unstable; the
// cursor is tracked by DATA-LINE NUMBER instead. The change-run targets are
// computed from the patch DATA (not the virtualized DOM): prefer the run's
// first ADDED line (new-side) — that's the line you'll comment on — and fall
// back to the first DELETED line (old-side) for delete-only runs.
//
// LAYER: domain (pure; std-only). The highest-risk untrusted-input parser in
// the diff cluster — fuzz-gated (patch-nav.fuzz.test.ts) under the Q4 mandate.

export type NavSide = "additions" | "deletions";

export interface NavStart {
  /** new-side line number for an additions start; old-side for a deletions start. */
  no: number;
  side: NavSide;
}

export interface NavData {
  /** ascending change-run start anchors, one per contiguous +/- run. */
  starts: NavStart[];
  /** the largest new-side line number in the patch (>= 1 — a safe floor). */
  maxNo: number;
}

const HUNK_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

/**
 * Parse a unified-diff patch into nav data. Fail-closed: never throws, never
 * hangs, always returns a well-formed NavData (starts ascending within a hunk,
 * maxNo >= 1). Lines before the first hunk header (diff/--- /+++ noise) are
 * ignored; only +/- content lines INSIDE a hunk are change content.
 */
export function parsePatchNav(patch: string): NavData {
  const lines = String(patch ?? "").split("\n");
  const starts: NavStart[] = [];
  let maxNo = 0;
  let inHunk = false;
  let nw = 0; // current new-side line number
  let od = 0; // current old-side line number
  let runAdds: number[] = [];
  let runDels: number[] = [];
  let inRun = false;

  const flush = (): void => {
    if (inRun) {
      if (runAdds[0] != null) starts.push({ no: runAdds[0], side: "additions" });
      else if (runDels[0] != null) starts.push({ no: runDels[0], side: "deletions" });
    }
    runAdds = [];
    runDels = [];
    inRun = false;
  };

  for (const raw of lines) {
    const hm = HUNK_RE.exec(raw);
    if (hm) {
      flush();
      // clamp to >= 1: a `@@ -0,0 +0,0 @@` (empty-file) header is degenerate;
      // line numbers are 1-based, so a 0 floor would emit a no:0 anchor.
      od = Math.max(1, Number(hm[1]));
      nw = Math.max(1, Number(hm[2]));
      inHunk = true;
      continue;
    }
    if (!inHunk) continue; // pre-hunk noise (diff --git / --- / +++) — skip
    // file-header markers (`+++ `/`--- `) are NOT content rows even if they
    // appear after a hunk header (a malformed/interleaved patch). A real diff
    // content line is `+x`/`-x`, never the triple-sigil header.
    if (raw.startsWith("+++") || raw.startsWith("---")) continue;
    const sigil = raw[0];
    if (sigil === "+") {
      inRun = true;
      runAdds.push(nw);
      maxNo = Math.max(maxNo, nw);
      nw++;
    } else if (sigil === "-") {
      inRun = true;
      runDels.push(od);
      od++;
    } else if (sigil === "\\") {
      // "\ No newline at end of file" — not a content line; ignore.
      continue;
    } else {
      // a context line (leading space) OR junk → close the current run and
      // advance both sides. (Treating junk as context keeps the parser total.)
      flush();
      maxNo = Math.max(maxNo, nw);
      nw++;
      od++;
    }
  }
  flush();
  return { starts, maxNo: maxNo || 1 };
}
