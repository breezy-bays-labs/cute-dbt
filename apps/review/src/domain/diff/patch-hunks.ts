// patch-hunks — parse a unified-diff patch into structured hunks (old/new line
// numbers + add/del/ctx kind) for the FIRST-PARTY fallback diff renderer. Pierre
// owns the primary surface; this is the data the escape-hatch renderer draws
// when Pierre is unavailable. Fail-closed: total over any string.
//
// LAYER: domain (pure; std-only).

export type HunkLineKind = "ctx" | "add" | "del";

export interface HunkLine {
  t: HunkLineKind;
  s: string;
  /** old-side 1-based line number (null on an added line). */
  oldNo: number | null;
  /** new-side 1-based line number (null on a deleted line). */
  newNo: number | null;
}

export interface Hunk {
  oldStart: number;
  newStart: number;
  lines: HunkLine[];
}

const HUNK_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

export function parsePatchHunks(patch: string): Hunk[] {
  const lines = String(patch ?? "").split("\n");
  // a trailing newline leaves a final "" element; lacking a sigil it would be
  // parsed as a bogus context row (extra line + gutter counters off by one).
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  const hunks: Hunk[] = [];
  let cur: Hunk | null = null;
  let oldNo = 0;
  let newNo = 0;
  for (const raw of lines) {
    const hm = HUNK_RE.exec(raw);
    if (hm) {
      // clamp to >= 1 (1-based lines; a degenerate `@@ -0,0 +0,0 @@` would
      // otherwise number the first row 0).
      cur = { oldStart: Math.max(1, Number(hm[1])), newStart: Math.max(1, Number(hm[2])), lines: [] };
      oldNo = cur.oldStart;
      newNo = cur.newStart;
      hunks.push(cur);
      continue;
    }
    if (!cur) continue; // pre-hunk header noise
    // file-header markers are not content rows (a malformed/interleaved patch).
    if (raw.startsWith("+++") || raw.startsWith("---")) continue;
    const sigil = raw[0];
    const text = raw.slice(1);
    if (sigil === "+") {
      cur.lines.push({ t: "add", s: text, oldNo: null, newNo });
      newNo++;
    } else if (sigil === "-") {
      cur.lines.push({ t: "del", s: text, oldNo, newNo: null });
      oldNo++;
    } else if (sigil === "\\") {
      // "\ No newline at end of file" — not a content row.
      continue;
    } else {
      // a context line (leading space) — text is raw.slice(1).
      cur.lines.push({ t: "ctx", s: text, oldNo, newNo });
      oldNo++;
      newNo++;
    }
  }
  // drop hunks that ended up with no content rows (header-only / malformed).
  return hunks.filter((h) => h.lines.length > 0);
}
