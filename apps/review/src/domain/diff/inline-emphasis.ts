// Intra-line (word-level) diff — shared by the diff viewer and suggestion
// blocks. Difftastic/GitHub-style: when a removed line pairs with an added
// line, emphasize only the tokens that actually changed, leaving the rest at
// the calmer whole-line tint. Token granularity = whitespace runs · identifiers
// · single punctuation, so adding a trailing "," lights up just the comma.
//
// LAYER: domain (pure; std-only). Verbatim port of the prototype inlinediff.js.

/** One diff line: a kind tag, the raw text, and (after enrichment) char-ranges. */
export type DiffKind = "ctx" | "del" | "add";
export interface DiffLine {
  t: DiffKind;
  s: string;
  /** changed char-ranges [start,end) attached by `enrichLines` (added/removed only). */
  emph?: [number, number][];
}

/** char-range [start, end) — half-open, into the line's string. */
export type Range = [number, number];

export interface InlineEmphasis {
  /** changed ranges on the OLD (removed) line. */
  oldR: Range[];
  /** changed ranges on the NEW (added) line. */
  newR: Range[];
}

export type LcsOp = { t: "eq" | "del" | "add"; s: string };

/** Tokenize into whitespace runs · identifiers · single punctuation chars. */
export function tokenizeLine(s: string): string[] {
  return String(s).match(/(\s+|[A-Za-z0-9_]+|[^\sA-Za-z0-9_])/g) ?? [];
}

/** Token-level longest-common-subsequence ops (eq / del / add), in order. */
export function lcsOps(a: string[], b: string[]): LcsOp[] {
  const n = a.length;
  const m = b.length;
  // (n+1)×(m+1) DP table; every index below is provably in-bounds (the loops
  // bound i<n, j<m and only reach i+1/j+1 ≤ n/m), so a non-null assert is sound.
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    const row = dp[i]!;
    const rowNext = dp[i + 1]!;
    for (let j = m - 1; j >= 0; j--) {
      row[j] = a[i] === b[j] ? rowNext[j + 1]! + 1 : Math.max(rowNext[j]!, row[j + 1]!);
    }
  }
  const ops: LcsOp[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      ops.push({ t: "eq", s: a[i]! });
      i++;
      j++;
    } else if (dp[i + 1]![j]! >= dp[i]![j + 1]!) {
      ops.push({ t: "del", s: a[i++]! });
    } else {
      ops.push({ t: "add", s: b[j++]! });
    }
  }
  while (i < n) ops.push({ t: "del", s: a[i++]! });
  while (j < m) ops.push({ t: "add", s: b[j++]! });
  return ops;
}

function mergeRanges(rs: Range[]): Range[] {
  if (!rs.length) return rs;
  const sorted = rs.map((r) => [...r] as Range).sort((x, y) => x[0] - y[0]);
  const out: Range[] = [[...sorted[0]!] as Range];
  for (let k = 1; k < sorted.length; k++) {
    const cur = sorted[k]!;
    const last = out[out.length - 1]!;
    if (cur[0] <= last[1]) last[1] = Math.max(last[1], cur[1]);
    else out.push([...cur] as Range);
  }
  return out;
}

/** Drop leading/trailing whitespace from a range so highlights have clean edges. */
function trimWs([a, b]: Range, str: string): Range | null {
  while (a < b && /\s/.test(str[a]!)) a++;
  while (b > a && /\s/.test(str[b - 1]!)) b--;
  return b > a ? [a, b] : null;
}

/**
 * Returns { oldR, newR } changed char-ranges, or null when the two lines are
 * too dissimilar (a rewrite) — there, whole-line tint reads better than noise.
 * Anchors the common prefix + suffix first (so "X" → "wrap(X) as X" lights up
 * only the inserted wrapper), then token-LCS the middle. The gate measures how
 * much of the *shorter* line survives, so a small line wrapped by a lot of new
 * code still emphasizes cleanly.
 */
export function inlineEmphasis(oldStr: string, newStr: string): InlineEmphasis | null {
  if (oldStr === newStr) return null;
  const A = tokenizeLine(oldStr);
  const B = tokenizeLine(newStr);
  let p = 0;
  while (p < A.length && p < B.length && A[p] === B[p]) p++;
  let sa = A.length;
  let sb = B.length;
  while (sa > p && sb > p && A[sa - 1] === B[sb - 1]) {
    sa--;
    sb--;
  }
  const ops = lcsOps(A.slice(p, sa), B.slice(p, sb));
  const oldR: Range[] = [];
  const newR: Range[] = [];
  let oc = A.slice(0, p).join("").length; // char cursor past the common prefix
  let nc = B.slice(0, p).join("").length;
  let matched = (A.slice(0, p).join("") + A.slice(sa).join("")).replace(/\s/g, "").length;
  for (const op of ops) {
    const len = op.s.length;
    if (op.t === "eq") {
      matched += op.s.replace(/\s/g, "").length;
      oc += len;
      nc += len;
    } else if (op.t === "del") {
      oldR.push([oc, oc + len]); // include ws; trimmed after merge
      oc += len;
    } else {
      newR.push([nc, nc + len]);
      nc += len;
    }
  }
  const oldNW = oldStr.replace(/\s/g, "").length;
  const newNW = newStr.replace(/\s/g, "").length;
  const survived = matched / Math.max(1, Math.min(oldNW, newNW));
  if (survived < 0.45) return null; // a rewrite — keep the calm whole-line tint
  const fin = (rs: Range[], str: string): Range[] =>
    mergeRanges(rs)
      .map((r) => trimWs(r, str))
      .filter((r): r is Range => r != null);
  return { oldR: fin(oldR, oldStr), newR: fin(newR, newStr) };
}

/**
 * Walk a hunk's lines; for each del-run immediately followed by an add-run,
 * pair index-wise and attach .emph char-ranges to the paired lines. Returns a
 * fresh array (does NOT mutate the input).
 */
export function enrichLines(lines: DiffLine[]): DiffLine[] {
  const out = lines.map((l) => ({ ...l }));
  let i = 0;
  while (i < out.length) {
    if (out[i]!.t !== "del") {
      i++;
      continue;
    }
    const dStart = i;
    while (i < out.length && out[i]!.t === "del") i++;
    const aStart = i;
    while (i < out.length && out[i]!.t === "add") i++;
    const dels = out.slice(dStart, aStart);
    const adds = out.slice(aStart, i);
    const pairs = Math.min(dels.length, adds.length);
    for (let k = 0; k < pairs; k++) {
      const del = dels[k]!;
      const add = adds[k]!;
      const em = inlineEmphasis(del.s, add.s);
      if (em) {
        del.emph = em.oldR;
        add.emph = em.newR;
      }
    }
  }
  return out;
}

export interface EmphLine {
  s: string;
  emph?: Range[];
}

/**
 * Pair two blocks of lines (old → new) the way a suggestion does: align by
 * index and attach inline emphasis. Returns { oldLines, newLines }.
 */
export function pairBlocks(oldArr: string[], newArr: string[]): { oldLines: EmphLine[]; newLines: EmphLine[] } {
  const oldLines: EmphLine[] = oldArr.map((s) => ({ s }));
  const newLines: EmphLine[] = newArr.map((s) => ({ s }));
  const pairs = Math.min(oldArr.length, newArr.length);
  for (let k = 0; k < pairs; k++) {
    const em = inlineEmphasis(oldArr[k]!, newArr[k]!);
    if (em) {
      oldLines[k]!.emph = em.oldR;
      newLines[k]!.emph = em.newR;
    }
  }
  return { oldLines, newLines };
}
