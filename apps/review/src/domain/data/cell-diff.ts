// The NULL / absent / empty cell trichotomy + the {old,new,changed} cell-diff —
// FIRST-PARTY, never delegated. The single most honesty-load-bearing fold: a
// reviewer must be able to tell a SQL NULL (`null`) from a missing cell
// (`absent`) from an empty-but-present value (`number`/`str` with display "").
//
// This is the 100%-mutation-kill target (the council strict tier: 100% Stryker
// kill on the cell trichotomy). VERBATIM PORT of prototype/context.js diffSide /
// adaptDiffTable + the seed-card cellSide fold.

import type { Cell, DiffCell, DiffTable } from "../context-data";

/** One side of a data_diff cell, normalized: NULL & absent are EXPLICIT + DISTINCT.
 *  - `absent`  ⇒ { text: "",     null: false, absent: true  }  (no cell)
 *  - `null`    ⇒ { text: "NULL", null: true,  absent: false }  (a SQL NULL)
 *  - typed     ⇒ { text: display,null: false, absent: false }  (present; "" is empty-but-present)
 */
export interface NormSide { text: string; null: boolean; absent: boolean; }

/**
 * diffSide — the trichotomy fold. The three states are MUTUALLY EXCLUSIVE and
 * NEVER collapse: `absent` (missing cell) ≠ `null` (SQL NULL) ≠ "" (empty string,
 * a present typed value). A missing side (`undefined`) is `absent`.
 */
export function diffSide(s: Cell | null | undefined): NormSide {
  if (!s) return { text: "", null: false, absent: true };
  const t = s.key && s.key.t;
  if (t === "absent") return { text: "", null: false, absent: true };
  if (t === "null") return { text: "NULL", null: true, absent: false };
  // present (number/str, or a side with a `display` but no key): empty string
  // stays empty-but-PRESENT — never folded to absent or NULL.
  return { text: s.display != null ? String(s.display) : "", null: false, absent: false };
}

/**
 * cellSide — the seed-card / flat-cell display fold. Same trichotomy basis, but
 * yields the raw DISPLAY string (no "NULL" sentinel) for a present cell, and a
 * caller-chosen `nullText` for a SQL NULL. Distinct from diffSide (which always
 * renders "NULL"): seed cells render "(null)". Honest absent ⇒ "".
 */
export function cellSide(s: Cell | null | undefined, nullText = "(null)"): string {
  if (!s) return "";
  const t = s.key && s.key.t;
  if (t === "absent") return "";
  if (t === "null") return nullText;
  return s.display != null ? String(s.display) : "";
}

/** A normalized cell-diff cell: each side trichotomy-folded + the changed flag. */
export interface NormDiffCell { old: NormSide; new: NormSide; changed: boolean; }
/** A normalized cell-diff table. */
export interface NormDiffTable {
  columns: { name: string; status: string }[];
  rows: { kind: string; cells: NormDiffCell[] }[];
}

/**
 * adaptDiffTable — a data_diff table → our normalized cell-diff table. Each cell's
 * old/new side runs through diffSide (the trichotomy), and `changed` is preserved
 * verbatim (never inferred — a fold that recomputed `changed` could silently flip
 * a chip; the wire is authoritative). Honest-null when the table is absent.
 */
export function adaptDiffTable(tbl: DiffTable | null | undefined): NormDiffTable | null {
  if (!tbl) return null;
  return {
    columns: (tbl.columns ?? []).map((c) => ({ name: c.name, status: c.status ?? "present" })),
    rows: (tbl.rows ?? []).map((r) => ({
      kind: r.kind ?? "modified",
      cells: (r.cells ?? []).map((c: DiffCell) => ({
        old: diffSide(c.old),
        new: diffSide(c.new),
        changed: !!c.changed,
      })),
    })),
  };
}

/**
 * allAdded — a brand-new table predicate: every column added + every row added.
 * Empty table (no columns) is NOT all-added (honest: nothing to assert).
 */
export function allAdded(tbl: NormDiffTable | null | undefined): boolean {
  return !!tbl && tbl.columns.length > 0
    && tbl.columns.every((c) => c.status === "added")
    && tbl.rows.every((r) => r.kind === "added");
}
