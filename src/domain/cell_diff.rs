//! The cell-level unit-test data-table diff (cute-dbt#98).
//!
//! The structured-table sibling of the cute-dbt#96 inline YAML *line* diff
//! ([`crate::domain::pr_diff::reconstruct_block_diffs`]). Where #96 emits a
//! [`BlockDiff`](crate::domain::pr_diff::BlockDiff) of
//! [`DiffLine`](crate::domain::pr_diff::DiffLine)s, #98 emits a
//! [`UnitTestDataDiff`] of aligned [`RowChange`]s with per-cell
//! before/after [`CellChange`]s — so a single edited value in a fixture
//! renders as one highlighted cell, not a whole-block text diff.
//!
//! ## The two sides
//!
//! - **NEW** rows come from the CURRENT manifest (the authoritative current
//!   fixture data), via [`table_from_manifest_rows`].
//! - **OLD** rows are *diff-sourced* — there is no base manifest. They are
//!   reconstructed from the PR diff's hunks by projecting the test's
//!   complete pre-edit YAML block (Context + Removed lines) through
//!   `pr_diff::block_diff_for` (the crate-internal wrapper over the #96
//!   reconstruction), then slicing
//!   the `given`/`expect` `rows:` sub-regions out by indentation and parsing
//!   each via [`table_from_yaml_fragment`].
//!
//! ## Row alignment: multiset key matching + positional residual pairing
//!
//! [`diff_fixture_tables`] unifies the two tables' column axes, projects
//! both row sets over the union, then keys each row by its canonical
//! [`CellValue`] serialization. Rows whose keys match as a **multiset**
//! (min multiplicity, so a duplicate row matches duplicate-for-duplicate)
//! are [`Unchanged`](RowChangeKind::Unchanged). The unmatched residual OLD
//! and NEW rows are paired **positionally**; a pair sharing ≥1
//! mutually-present column becomes one [`Modified`](RowChangeKind::Modified)
//! row, otherwise it stays a separate `Removed` + `Added` (a wholesale
//! replacement); excess residual is `Removed` / `Added`.
//!
//! Multiset matching is the load-bearing choice for the spec's keystone
//! requirement — a pure **reorder** is a non-event: every OLD key still
//! appears in NEW with the same multiplicity, so every row matches and the
//! diff is all-`Unchanged` (`has_real_change()` is `false` → no entry → the
//! #96 `yaml_diff` fallback). This holds under both the set and the ordered
//! reading of dbt fixtures. (A plain LCS would mis-render a *rotation* —
//! `[A,B,C] → [C,A,B]` — as one row `Removed` + one `Added`, because a
//! rotated row leaves the longest common *subsequence*; multiset matching
//! sidesteps that.)
//!
//! The one documented v1 boundary: a row that is BOTH edited AND reordered
//! relative to its sibling residual rows may mispair in the positional zip.
//! No fixture exercises it; the shared-column heuristic still yields a
//! sensible `Modified`. A move-annotation pass is a deferred v2 nicety.
//!
//! ## Domain purity
//!
//! Imports only *downward* within `domain`
//! ([`unit_test_table`](crate::domain::unit_test_table),
//! [`pr_diff`](crate::domain::pr_diff),
//! [`manifest`](crate::domain::manifest),
//! [`unit_test`](crate::domain::unit_test),
//! [`unit_test_yaml`](crate::domain::unit_test_yaml),
//! [`state`](crate::domain::state)) plus `std` + `serde` + `serde_json`.
//! No I/O, no parser libraries, no `clap`/`askama`. Nothing in `domain`
//! imports this module — the cli/adapters render layer (Workflow 2) is its
//! only consumer.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::domain::manifest::Manifest;
use crate::domain::pr_diff::{
    BlockSpan, DiffLineKind, Hunk, NormalizedDiffIndex, block_aligns_with_hunks, block_diff_for,
    hunk_touches_block,
};
use crate::domain::state::InScopeSet;
use crate::domain::unit_test::UnitTest;
use crate::domain::unit_test_table::{
    CellValue, FixtureTable, TableRow, table_from_manifest_rows, table_from_yaml_fragment,
};
use crate::domain::unit_test_yaml::UnitTestYamlBlock;

// ---------------------------------------------------------------------
// The cell-diff PODs — the structured analogue of BlockDiff
// ---------------------------------------------------------------------

/// The cute-dbt#98 wire analogue of
/// [`BlockDiff`](crate::domain::pr_diff::BlockDiff). Threaded later
/// (Workflow 2) onto a test's render payload under `data_diff`, the sibling
/// of the #96 `yaml_diff`.
///
/// `given` carries one [`NamedTableDiff`] per changed `given` input
/// (keyed by its `input` reference). `expect` carries the `expect`
/// fixture's diff, or `None` when `expect` is sql/opaque or unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UnitTestDataDiff {
    /// Per-`given`-input table diffs, in `given` order, keyed by `input`.
    /// Only inputs whose fixture data actually changed appear here.
    pub given: Vec<NamedTableDiff>,
    /// The `expect` fixture's diff — `None` when `expect` is sql/opaque or
    /// carried no real change.
    pub expect: Option<FixtureTableDiff>,
}

impl UnitTestDataDiff {
    /// `true` when neither a `given` nor the `expect` side carried a real
    /// change — the caller then emits no entry and the view falls back to
    /// the #96 `yaml_diff`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.given.is_empty() && self.expect.is_none()
    }
}

/// One `given` input's table diff, keyed by its `input` reference (e.g.
/// `ref('stg_orders')`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedTableDiff {
    /// The `given` input reference this diff belongs to.
    pub input: String,
    /// The cell-level diff of that input's fixture rows.
    pub diff: FixtureTableDiff,
}

/// One table's diff: the unified column axis + the aligned rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureTableDiff {
    /// The unified columns: NEW columns first (in order), then OLD-only
    /// columns. Each carries its [`ColumnStatus`].
    pub columns: Vec<DiffColumn>,
    /// The aligned rows, in NEW order with deletions spliced in.
    pub rows: Vec<RowChange>,
}

impl FixtureTableDiff {
    /// `true` when any row is non-`Unchanged` OR any column is not
    /// `Present` — the verdict the caller tests before emitting a
    /// [`NamedTableDiff`] / setting [`UnitTestDataDiff::expect`]. A
    /// format-only or pure-reorder diff returns `false` (no entry → #96
    /// `yaml_diff` fallback), mirroring
    /// [`BlockDiff::has_real_change`](crate::domain::pr_diff::BlockDiff::has_real_change).
    #[must_use]
    pub fn has_real_change(&self) -> bool {
        self.rows.iter().any(|r| r.kind != RowChangeKind::Unchanged)
            || self
                .columns
                .iter()
                .any(|c| c.status != ColumnStatus::Present)
    }
}

/// One unified column: its name + whether it is present on both sides,
/// added (NEW only), or removed (OLD only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffColumn {
    /// The column name.
    pub name: String,
    /// Whether the column is present on both sides, added, or removed.
    pub status: ColumnStatus,
}

/// A unified column's presence verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnStatus {
    /// Present in both the OLD and NEW tables.
    Present,
    /// Present only in the NEW table (a column was added).
    Added,
    /// Present only in the OLD table (a column was removed).
    Removed,
}

/// One aligned row: its change kind + the per-column before/after cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowChange {
    /// Whether the row is unchanged, modified in place, added, or removed.
    pub kind: RowChangeKind,
    /// Per-column [`CellChange`]s, parallel to
    /// [`FixtureTableDiff::columns`]. For `Unchanged` the `old`/`new` echo
    /// each other (every `changed` is `false`); for `Added` the `old`
    /// cells are [`CellValue::Absent`]; for `Removed` the `new` cells are
    /// `Absent`.
    pub cells: Vec<CellChange>,
}

/// A row's change kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowChangeKind {
    /// Present and identical on both sides (a reorder collapses to this).
    Unchanged,
    /// Present on both sides but with ≥1 changed cell (an in-place edit).
    Modified,
    /// Present only in the NEW table.
    Added,
    /// Present only in the OLD table.
    Removed,
}

/// One cell's before/after value plus the precomputed semantic verdict.
///
/// `changed` is `old != new` — the ONE equality oracle, computed once here
/// over the canonical [`CellValue`]s so the render layer never re-derives
/// it (a `Str "1"` vs `Number "1"` never collide; a format-only difference
/// is `changed: false`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellChange {
    /// The OLD cell value ([`CellValue::Absent`] for an added row/column).
    pub old: CellValue,
    /// The NEW cell value ([`CellValue::Absent`] for a removed row/column).
    pub new: CellValue,
    /// `old != new` — precomputed semantic verdict.
    pub changed: bool,
}

// ---------------------------------------------------------------------
// The pure table diff: unify columns -> project -> key -> LCS -> emit
// ---------------------------------------------------------------------

/// Diff two normalized [`FixtureTable`]s into a [`FixtureTableDiff`].
///
/// Pure (no I/O, deterministic). The algorithm:
///
/// 1. **Unify columns** — NEW columns first (in order), then OLD-only
///    columns; tag each [`Present`](ColumnStatus::Present) /
///    [`Added`](ColumnStatus::Added) / [`Removed`](ColumnStatus::Removed).
/// 2. **Project** every row over the union (a column the row lacks →
///    [`CellValue::Absent`]).
/// 3. **Key** each projected row by its canonical cell serialization (built
///    from the same [`CellValue`] normalizer as the cells, so a format-only
///    difference hashes equal).
/// 4. **Multiset-match** equal keys (min multiplicity) — matched rows are
///    [`Unchanged`](RowChangeKind::Unchanged); a pure reorder matches every
///    row.
/// 5. **Pair the residual positionally** — the i-th unmatched OLD row with
///    the i-th unmatched NEW row; a pair sharing ≥1 mutually-present column
///    → [`Modified`](RowChangeKind::Modified), else a separate `Removed` +
///    `Added`; excess residual is `Removed` / `Added`.
/// 6. **Emit in NEW order** — Unchanged + paired rows in their NEW
///    positions, then any unpaired-OLD `Removed` rows appended.
#[must_use]
pub fn diff_fixture_tables(old: &FixtureTable, new: &FixtureTable) -> FixtureTableDiff {
    let columns = unify_columns(old, new);
    let col_names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();

    let old_rows: Vec<Vec<CellValue>> = project_rows(old, &col_names);
    let new_rows: Vec<Vec<CellValue>> = project_rows(new, &col_names);

    let rows = align_rows(&old_rows, &new_rows);

    FixtureTableDiff { columns, rows }
}

/// Align the projected OLD/NEW rows: multiset-match equal keys as
/// `Unchanged`, then pair the residual positionally, emitting in NEW order
/// with trailing `Removed` appended.
fn align_rows(old_rows: &[Vec<CellValue>], new_rows: &[Vec<CellValue>]) -> Vec<RowChange> {
    let (new_matched, old_consumed) = match_multiset(old_rows, new_rows);

    // Residual rows, still in source order.
    let residual_old: Vec<usize> = (0..old_rows.len())
        .filter(|oi| !old_consumed[*oi])
        .collect();
    let residual_new: Vec<usize> = (0..new_rows.len()).filter(|nj| !new_matched[*nj]).collect();

    let mut by_new: HashMap<usize, RowChange> = HashMap::new();
    let mut trailing_removed: Vec<RowChange> = Vec::new();
    pair_residual(
        &residual_old,
        &residual_new,
        old_rows,
        new_rows,
        &mut by_new,
        &mut trailing_removed,
    );

    // Emit in strict NEW order — an Unchanged match and a residual
    // Added/Modified interleave at their NEW slots — then append trailing
    // Removed rows (the [Unchanged, Removed] order `d_deleted_row` pins).
    let mut out: Vec<RowChange> = Vec::with_capacity(new_rows.len() + trailing_removed.len());
    for (nj, cells) in new_rows.iter().enumerate() {
        if new_matched[nj] {
            out.push(unchanged_row(cells));
        } else if let Some(change) = by_new.remove(&nj) {
            out.push(change);
        }
    }
    out.extend(trailing_removed);
    out
}

/// Multiset-match NEW rows against OLD rows by key (min multiplicity): a
/// NEW row is matched (→ Unchanged) iff an as-yet-unconsumed OLD row carries
/// the same key, which consumes that OLD row. Returns `(new_matched,
/// old_consumed)`. Duplicate keys match duplicate-for-duplicate (the
/// LCS-on-repeats property without LCS).
fn match_multiset(
    old_rows: &[Vec<CellValue>],
    new_rows: &[Vec<CellValue>],
) -> (Vec<bool>, Vec<bool>) {
    let old_keys: Vec<String> = old_rows.iter().map(|r| row_key(r)).collect();
    let mut consumed = vec![false; old_rows.len()];
    let mut matched = vec![false; new_rows.len()];
    for (nj, ncells) in new_rows.iter().enumerate() {
        let nkey = row_key(ncells);
        if let Some(oi) = first_unconsumed(&old_keys, &consumed, &nkey) {
            consumed[oi] = true;
            matched[nj] = true;
        }
    }
    (matched, consumed)
}

/// The first OLD index whose key equals `nkey` and is not yet consumed.
fn first_unconsumed(old_keys: &[String], consumed: &[bool], nkey: &str) -> Option<usize> {
    old_keys
        .iter()
        .enumerate()
        .find(|(oi, okey)| okey.as_str() == nkey && !consumed[*oi])
        .map(|(oi, _)| oi)
}

/// Pair the residual OLD/NEW rows positionally: the i-th unmatched OLD with
/// the i-th unmatched NEW. A pair [sharing a present cell](rows_share_present_cell)
/// → one `Modified` (in the NEW slot); else a separate `Added` (NEW slot) +
/// `Removed` (trailing). Excess NEW residual → `Added`; excess OLD residual
/// → trailing `Removed`.
fn pair_residual(
    residual_old: &[usize],
    residual_new: &[usize],
    old_rows: &[Vec<CellValue>],
    new_rows: &[Vec<CellValue>],
    by_new: &mut HashMap<usize, RowChange>,
    trailing_removed: &mut Vec<RowChange>,
) {
    let paired = residual_old.len().min(residual_new.len());
    for k in 0..paired {
        let old_cells = &old_rows[residual_old[k]];
        let new_cells = &new_rows[residual_new[k]];
        if rows_share_present_cell(old_cells, new_cells) {
            by_new.insert(residual_new[k], modified_row(old_cells, new_cells));
        } else {
            by_new.insert(residual_new[k], added_row(new_cells));
            trailing_removed.push(removed_row(old_cells));
        }
    }
    for &nj in &residual_new[paired..] {
        by_new.insert(nj, added_row(&new_rows[nj]));
    }
    for &oi in &residual_old[paired..] {
        trailing_removed.push(removed_row(&old_rows[oi]));
    }
}

/// The unified column axis: NEW columns first (in source order), then
/// OLD-only columns. Tags each with its presence status.
fn unify_columns(old: &FixtureTable, new: &FixtureTable) -> Vec<DiffColumn> {
    let mut columns: Vec<DiffColumn> = Vec::new();
    for name in &new.columns {
        let status = if old.columns.iter().any(|c| c == name) {
            ColumnStatus::Present
        } else {
            ColumnStatus::Added
        };
        columns.push(DiffColumn {
            name: name.clone(),
            status,
        });
    }
    for name in &old.columns {
        if !new.columns.iter().any(|c| c == name) {
            columns.push(DiffColumn {
                name: name.clone(),
                status: ColumnStatus::Removed,
            });
        }
    }
    columns
}

/// Project each of `table`'s rows over the unified `col_names`, filling a
/// column the row lacks with [`CellValue::Absent`].
fn project_rows(table: &FixtureTable, col_names: &[&str]) -> Vec<Vec<CellValue>> {
    table
        .rows
        .iter()
        .map(|row| {
            col_names
                .iter()
                .map(|col| cell_for_column(table, row, col))
                .collect()
        })
        .collect()
}

/// The [`CellValue`] of `row` for unified column `col`, or
/// [`CellValue::Absent`] when this table has no such column.
fn cell_for_column(table: &FixtureTable, row: &TableRow, col: &str) -> CellValue {
    table
        .columns
        .iter()
        .position(|c| c == col)
        .and_then(|idx| row.cells.get(idx))
        .map_or(CellValue::Absent, |cell| cell.value.clone())
}

/// The canonical row key — each cell's tag+value joined by `|`. Built from
/// the same [`CellValue`] as the cells, so two rows differing only by
/// source format hash equal; `Absent` and `Null` serialize to distinct
/// tokens.
fn row_key(cells: &[CellValue]) -> String {
    cells
        .iter()
        .map(cell_key_token)
        .collect::<Vec<_>>()
        .join("|")
}

/// One cell's key token (`tag:value`, or just the tag for unit variants).
fn cell_key_token(v: &CellValue) -> String {
    match v {
        CellValue::Null => "null".to_owned(),
        CellValue::Absent => "absent".to_owned(),
        CellValue::Bool(b) => format!("bool:{b}"),
        CellValue::Number(n) => format!("number:{n}"),
        CellValue::Str(s) => format!("str:{s}"),
    }
}

/// An `Unchanged` row: `old`/`new` echo the projected cells, every
/// `changed` is `false`.
fn unchanged_row(cells: &[CellValue]) -> RowChange {
    RowChange {
        kind: RowChangeKind::Unchanged,
        cells: cells
            .iter()
            .map(|v| CellChange {
                old: v.clone(),
                new: v.clone(),
                changed: false,
            })
            .collect(),
    }
}

/// An `Added` row: `old` is `Absent`, `new` is the projected cell.
fn added_row(cells: &[CellValue]) -> RowChange {
    RowChange {
        kind: RowChangeKind::Added,
        cells: cells
            .iter()
            .map(|v| CellChange {
                old: CellValue::Absent,
                new: v.clone(),
                changed: true,
            })
            .collect(),
    }
}

/// A `Removed` row: `new` is `Absent`, `old` is the projected cell.
fn removed_row(cells: &[CellValue]) -> RowChange {
    RowChange {
        kind: RowChangeKind::Removed,
        cells: cells
            .iter()
            .map(|v| CellChange {
                old: v.clone(),
                new: CellValue::Absent,
                changed: true,
            })
            .collect(),
    }
}

/// Whether two projected rows share at least one column position where
/// BOTH cells are present (non-`Absent`) — the heuristic that distinguishes
/// an in-place edit (`Modified`) from a wholesale replacement (a separate
/// `Removed` + `Added`).
fn rows_share_present_cell(old_cells: &[CellValue], new_cells: &[CellValue]) -> bool {
    old_cells
        .iter()
        .zip(new_cells.iter())
        .any(|(o, n)| *o != CellValue::Absent && *n != CellValue::Absent)
}

/// Fuse a residual OLD row's cells with a residual NEW row's cells into one
/// `Modified` row, computing each cell's `changed` verdict (`old != new`).
fn modified_row(old_cells: &[CellValue], new_cells: &[CellValue]) -> RowChange {
    let cells = old_cells
        .iter()
        .zip(new_cells.iter())
        .map(|(o, n)| CellChange {
            old: o.clone(),
            new: n.clone(),
            changed: o != n,
        })
        .collect();
    RowChange {
        kind: RowChangeKind::Modified,
        cells,
    }
}

// ---------------------------------------------------------------------
// The reconstruction driver — the structured sibling of
// reconstruct_block_diffs (cute-dbt#96)
// ---------------------------------------------------------------------

/// Reconstruct a cell-level data diff for each in-scope changed unit test.
///
/// The structured-table sibling of
/// [`reconstruct_block_diffs`](crate::domain::pr_diff::reconstruct_block_diffs),
/// gated identically: a test gets an entry only when its YAML block is
/// present, the hunks still [align](block_aligns_with_hunks) with it, and at
/// least one hunk [touches](hunk_touches_block) it. The NEW tables come from
/// the CURRENT manifest; the OLD tables are sliced out of the reconstructed
/// pre-edit YAML (Context + Removed lines of the `pr_diff::block_diff_for`
/// reconstruction).
///
/// A test is omitted (→ #96 `yaml_diff` fallback) when its block is
/// absent/stale/untouched, OR every fixture is sql/opaque, OR the diff
/// carried no real cell change (a format-only or whitespace-only edit).
///
/// The run loop always passes the default-hasher `authoring_yaml` map, so
/// generalizing `blocks` over the hasher (`clippy::implicit_hasher`) buys
/// nothing — same rationale as `reconstruct_block_diffs`.
#[allow(clippy::implicit_hasher)]
#[must_use]
pub fn reconstruct_table_diffs(
    current: &Manifest,
    changed: &InScopeSet,
    blocks: &HashMap<String, UnitTestYamlBlock>,
    index: &NormalizedDiffIndex,
) -> HashMap<String, UnitTestDataDiff> {
    let mut out = HashMap::new();
    for id in changed.iter() {
        if let Some(data) = data_diff_for_test(current, id, blocks, index) {
            if !data.is_empty() {
                out.insert(id.to_owned(), data);
            }
        }
    }
    out
}

/// Build one test's [`UnitTestDataDiff`], or `None` when the gating
/// (block present + aligned + ≥1 touching hunk) does not hold.
fn data_diff_for_test(
    current: &Manifest,
    id: &str,
    blocks: &HashMap<String, UnitTestYamlBlock>,
    index: &NormalizedDiffIndex,
) -> Option<UnitTestDataDiff> {
    let ut = current.unit_test(id)?;
    let block = blocks.get(id)?;
    let hunks = ut
        .original_file_path()
        .map_or(&[][..], |ofp| index.hunks_for(ofp));
    let span = BlockSpan::new(&block.raw, block.block_start, block.block_end);
    if !block_aligns_with_hunks(&span, hunks) {
        return None; // stale diff → #96 fallback
    }
    let touching: Vec<&Hunk> = hunks
        .iter()
        .filter(|h| hunk_touches_block(block.block_start, block.block_end, h))
        .collect();
    if touching.is_empty() {
        return None; // change is elsewhere in the file → #96 fallback
    }
    let bd = block_diff_for(&span, &touching);
    // The complete pre-edit (OLD) block = every line that is NOT an
    // addition, i.e. Context + Removed lines, in order.
    let old_text: String = bd
        .lines
        .iter()
        .filter(|l| l.kind != DiffLineKind::Added)
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    Some(build_data_diff(ut, &old_text))
}

/// Assemble a test's `given` + `expect` table diffs from its current
/// manifest unit test (NEW) and the reconstructed OLD YAML text.
fn build_data_diff(ut: &UnitTest, old_text: &str) -> UnitTestDataDiff {
    let mut data = UnitTestDataDiff::default();
    for g in ut.given() {
        let new_tbl = table_from_manifest_rows(g.rows(), g.format());
        let old = slice_named_region(old_text, "input", Some(g.input()));
        let old_tbl =
            old.and_then(|(region, fmt)| table_from_yaml_fragment(&region, fmt.as_deref()));
        if let Some(diff) = table_diff_if_changed(old_tbl, new_tbl) {
            data.given.push(NamedTableDiff {
                input: g.input().to_owned(),
                diff,
            });
        }
    }
    let new_e = table_from_manifest_rows(ut.expect().rows(), ut.expect().format());
    let old_e = slice_named_region(old_text, "expect", None);
    let old_e_tbl =
        old_e.and_then(|(region, fmt)| table_from_yaml_fragment(&region, fmt.as_deref()));
    if let Some(diff) = table_diff_if_changed(old_e_tbl, new_e) {
        data.expect = Some(diff);
    }
    data
}

/// Diff an OLD/NEW table pair, returning the diff only when both sides are
/// non-opaque (`None` from sql) and the diff carries a real change.
fn table_diff_if_changed(
    old: Option<FixtureTable>,
    new: Option<FixtureTable>,
) -> Option<FixtureTableDiff> {
    // Both sides opaque (sql/None) → no cell diff (→ #96 fallback).
    if old.is_none() && new.is_none() {
        return None;
    }
    let old = old.unwrap_or_default();
    let new = new.unwrap_or_default();
    let diff = diff_fixture_tables(&old, &new);
    diff.has_real_change().then_some(diff)
}

// ---------------------------------------------------------------------
// OLD-side YAML region slicing — by INDENTATION, not line number
// ---------------------------------------------------------------------

/// Slice a `given`/`expect` sub-block's `rows:` region (and its `format:`)
/// out of the reconstructed OLD YAML text, by indentation.
///
/// `key` is `"input"` (a `given`, matched by its `input:` value against
/// `match_value`) or `"expect"` (the sole `expect:` sub-block, `match_value`
/// `None`). Returns `(rows_region, format)` where `rows_region` is the text
/// under the `rows:` key (the `rows:` line excluded) and `format` is the
/// `format:` value read from the same sub-block. `None` when the sub-block
/// or its `rows:` key is absent (the OLD side had no such fixture → the NEW
/// rows are all `Added`).
///
/// Sliced by INDENT, not line number: a Removed splice shifts line numbers
/// but preserves each line's leading whitespace, so indentation is the
/// stable structural signal.
fn slice_named_region(
    old_text: &str,
    key: &str,
    match_value: Option<&str>,
) -> Option<(String, Option<String>)> {
    let lines: Vec<&str> = old_text.split('\n').collect();
    let start = find_subblock_start(&lines, key, match_value)?;
    let sub_indent = indent_of(lines[start]);
    let end = subblock_end(&lines, start, sub_indent);
    let sub = &lines[start..end];
    let rows_region = slice_rows_region(sub)?;
    let format = find_format(sub);
    Some((rows_region, format))
}

/// Find the 0-based line index of the sub-block opener. For `given`
/// (`key == "input"`) this is the `- input: <value>` line whose value
/// matches `match_value`; for `expect` it is the `expect:` key line.
fn find_subblock_start(lines: &[&str], key: &str, match_value: Option<&str>) -> Option<usize> {
    lines
        .iter()
        .position(|line| is_subblock_opener(line.trim_start(), key, match_value))
}

/// Whether `trimmed` (a leading-whitespace-stripped line) opens the target
/// sub-block. A `given` opener is `- input: <ref>` whose `input:` value
/// equals `match_value`; an `expect` opener is the bare `expect:` key.
fn is_subblock_opener(trimmed: &str, key: &str, match_value: Option<&str>) -> bool {
    if key == "input" {
        return trimmed
            .strip_prefix("- ")
            .and_then(|rest| field_value(rest, "input"))
            .is_some_and(|val| match_value == Some(val.as_str()));
    }
    // `expect:` is a bare key — its presence (any/empty value) is the match.
    field_value(trimmed, key).is_some()
}

/// The exclusive end of a sub-block: the first line at indent ≤ `sub_indent`
/// that is non-blank, after `start`.
fn subblock_end(lines: &[&str], start: usize, sub_indent: usize) -> usize {
    for (offset, line) in lines.iter().enumerate().skip(start + 1) {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if indent_of(line) <= sub_indent {
            return offset;
        }
    }
    lines.len()
}

/// Within a sub-block, find the `rows:` key and return its child region
/// (every line deeper than the `rows:` key's indent), joined by `\n`.
/// `None` when there is no `rows:` key, or it has no child lines.
fn slice_rows_region(sub: &[&str]) -> Option<String> {
    let rows_idx = sub.iter().position(|l| {
        let t = l.trim_start();
        t == "rows:" || t.starts_with("rows:")
    })?;
    let rows_indent = indent_of(sub[rows_idx]);

    // An inline `rows:` value (e.g. `rows: []`) carries no child rows.
    let mut region: Vec<&str> = Vec::new();
    for line in sub.iter().skip(rows_idx + 1) {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            region.push(line);
            continue;
        }
        if indent_of(line) <= rows_indent {
            break; // left the rows: child region
        }
        region.push(line);
    }
    // Trim trailing blank lines so an empty region is None.
    while region.last().is_some_and(|l| l.trim().is_empty()) {
        region.pop();
    }
    if region.is_empty() {
        return None;
    }
    Some(region.join("\n"))
}

/// The `format:` value declared anywhere in `sub` (engine-agnostic — the
/// OLD side is the authored YAML). `None` when no `format:` key is present
/// (dbt defaults to `dict`).
fn find_format(sub: &[&str]) -> Option<String> {
    for line in sub {
        if let Some(val) = field_value(line.trim_start(), "format") {
            return Some(val);
        }
    }
    None
}

/// If `s` is `<field>: <value>`, return the trimmed, quote-stripped value
/// (an empty value yields `Some("")`). `None` when `s` is not that field.
fn field_value(s: &str, field: &str) -> Option<String> {
    let rest = s.strip_prefix(field)?;
    let rest = rest.strip_prefix(':')?;
    Some(unquote(rest.trim()))
}

/// Strip a matching surrounding single/double quote pair (for matching an
/// `input:` value, which may be quoted in the YAML).
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return s[1..s.len() - 1].to_owned();
    }
    s.to_owned()
}

/// The leading-whitespace width of a line.
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[cfg(test)]
#[allow(clippy::pedantic, clippy::cargo)]
mod tests {
    use super::*;
    use crate::domain::unit_test_table::Cell;

    // ----- builders -----

    fn n(s: &str) -> CellValue {
        CellValue::Number(s.into())
    }
    fn s(v: &str) -> CellValue {
        CellValue::Str(v.into())
    }
    fn row(cells: Vec<CellValue>) -> TableRow {
        TableRow::new(cells.into_iter().map(Cell::new).collect())
    }
    fn table(cols: &[&str], rows: Vec<Vec<CellValue>>) -> FixtureTable {
        FixtureTable::new(
            cols.iter().map(|c| (*c).to_owned()).collect(),
            rows.into_iter().map(row).collect(),
        )
    }

    // ----- B. format-only / identity = NO diff (headline) -----

    #[test]
    fn b_identical_tables_yield_no_real_change() {
        let t = table(&["id", "name"], vec![vec![n("1"), s("alice")]]);
        let diff = diff_fixture_tables(&t, &t);
        assert!(!diff.has_real_change(), "identical tables → no change");
        assert!(diff.rows.iter().all(|r| r.kind == RowChangeKind::Unchanged));
        assert!(
            diff.rows[0].cells.iter().all(|c| !c.changed),
            "every cell unchanged"
        );
    }

    // ----- C. single-cell before->after -----

    #[test]
    fn c_single_cell_edit_is_one_modified_row_one_changed_cell() {
        let old = table(&["id", "qty"], vec![vec![n("1"), n("100")]]);
        let new = table(&["id", "qty"], vec![vec![n("1"), n("200")]]);
        let diff = diff_fixture_tables(&old, &new);
        assert!(diff.has_real_change());
        assert_eq!(diff.rows.len(), 1);
        assert_eq!(diff.rows[0].kind, RowChangeKind::Modified);
        // id unchanged, qty changed.
        assert!(!diff.rows[0].cells[0].changed, "id cell unchanged");
        assert!(diff.rows[0].cells[1].changed, "qty cell changed");
        assert_eq!(diff.rows[0].cells[1].old, n("100"));
        assert_eq!(diff.rows[0].cells[1].new, n("200"));
    }

    #[test]
    fn c_before_after_orientation_is_not_flipped() {
        // Guard against an old<->new transposition: old=10, new=20.
        let old = table(&["v"], vec![vec![n("10")]]);
        let new = table(&["v"], vec![vec![n("20")]]);
        let diff = diff_fixture_tables(&old, &new);
        assert_eq!(diff.rows[0].cells[0].old, n("10"), "old side must be 10");
        assert_eq!(diff.rows[0].cells[0].new, n("20"), "new side must be 20");
    }

    // ----- D. row add / remove / pairing boundary -----

    #[test]
    fn d_appended_row_is_one_added_rest_unchanged() {
        let old = table(&["id"], vec![vec![n("1")]]);
        let new = table(&["id"], vec![vec![n("1")], vec![n("2")]]);
        let diff = diff_fixture_tables(&old, &new);
        assert_eq!(diff.rows.len(), 2);
        assert_eq!(diff.rows[0].kind, RowChangeKind::Unchanged);
        assert_eq!(diff.rows[1].kind, RowChangeKind::Added);
        assert_eq!(diff.rows[1].cells[0].old, CellValue::Absent);
        assert_eq!(diff.rows[1].cells[0].new, n("2"));
    }

    #[test]
    fn d_deleted_row_is_one_removed_rest_unchanged() {
        let old = table(&["id"], vec![vec![n("1")], vec![n("2")]]);
        let new = table(&["id"], vec![vec![n("1")]]);
        let diff = diff_fixture_tables(&old, &new);
        assert_eq!(diff.rows.len(), 2);
        assert_eq!(diff.rows[0].kind, RowChangeKind::Unchanged);
        assert_eq!(diff.rows[1].kind, RowChangeKind::Removed);
        assert_eq!(diff.rows[1].cells[0].old, n("2"));
        assert_eq!(diff.rows[1].cells[0].new, CellValue::Absent);
    }

    #[test]
    fn d_disjoint_add_and_remove_not_coalesced() {
        // old has only column a; new has only column b — share-zero present
        // columns → NOT a Modified, stays separate Removed + Added.
        let old = table(&["a"], vec![vec![n("1")]]);
        let new = table(&["b"], vec![vec![n("2")]]);
        let diff = diff_fixture_tables(&old, &new);
        let kinds: Vec<RowChangeKind> = diff.rows.iter().map(|r| r.kind).collect();
        assert!(
            kinds.contains(&RowChangeKind::Removed) && kinds.contains(&RowChangeKind::Added),
            "share-zero columns stay separate Removed + Added, got {kinds:?}"
        );
        assert!(
            !kinds.contains(&RowChangeKind::Modified),
            "must NOT coalesce a share-zero pair"
        );
    }

    #[test]
    fn d_share_one_present_column_coalesces_to_modified() {
        // Both rows have column id present → coalesce to one Modified.
        let old = table(&["id", "x"], vec![vec![n("1"), n("9")]]);
        let new = table(&["id", "x"], vec![vec![n("1"), n("8")]]);
        let diff = diff_fixture_tables(&old, &new);
        assert_eq!(diff.rows.len(), 1);
        assert_eq!(diff.rows[0].kind, RowChangeKind::Modified);
    }

    #[test]
    fn d_two_simultaneous_single_cell_edits_pair_correctly() {
        // THE advisor-flagged keystone: old=[A,B] -> new=[A',B'], all four
        // row keys distinct, no survivor between. Must emit exactly two
        // Modified rows, correctly paired (A->A', B->B'), NOT crossed.
        let old = table(
            &["id", "v"],
            vec![vec![n("1"), n("10")], vec![n("2"), n("20")]],
        );
        let new = table(
            &["id", "v"],
            vec![vec![n("1"), n("11")], vec![n("2"), n("21")]],
        );
        let diff = diff_fixture_tables(&old, &new);
        assert_eq!(
            diff.rows.len(),
            2,
            "two edits must be two rows, not a crossed 1R+1M+1A"
        );
        assert!(diff.rows.iter().all(|r| r.kind == RowChangeKind::Modified));
        // Row 0 pairs id=1: old v=10, new v=11. Row 1 pairs id=2: old 20, new 21.
        assert_eq!(diff.rows[0].cells[0].new, n("1"));
        assert_eq!(diff.rows[0].cells[1].old, n("10"));
        assert_eq!(diff.rows[0].cells[1].new, n("11"));
        assert_eq!(diff.rows[1].cells[0].new, n("2"));
        assert_eq!(diff.rows[1].cells[1].old, n("20"));
        assert_eq!(diff.rows[1].cells[1].new, n("21"));
    }

    // ----- E. row reorder (the riskiest seam) -----

    #[test]
    fn e_pure_reorder_is_all_unchanged() {
        // [A,B,C] -> [C,A,B], no cell edits → ALL Unchanged, no real change.
        let old = table(&["id"], vec![vec![n("1")], vec![n("2")], vec![n("3")]]);
        let new = table(&["id"], vec![vec![n("3")], vec![n("1")], vec![n("2")]]);
        let diff = diff_fixture_tables(&old, &new);
        assert!(
            !diff.has_real_change(),
            "a pure reorder must NOT be a real change"
        );
        assert!(
            diff.rows.iter().all(|r| r.kind == RowChangeKind::Unchanged),
            "reorder collapses to all-Unchanged, never all-changed"
        );
    }

    #[test]
    fn e_reorder_plus_one_edit_localizes_to_the_edited_row() {
        // [A,B,C] -> [C, A', B] where A' is A with one edited cell.
        let old = table(
            &["id", "v"],
            vec![
                vec![n("1"), n("10")],
                vec![n("2"), n("20")],
                vec![n("3"), n("30")],
            ],
        );
        let new = table(
            &["id", "v"],
            vec![
                vec![n("3"), n("30")],
                vec![n("1"), n("99")],
                vec![n("2"), n("20")],
            ],
        );
        let diff = diff_fixture_tables(&old, &new);
        let modified: Vec<&RowChange> = diff
            .rows
            .iter()
            .filter(|r| r.kind == RowChangeKind::Modified)
            .collect();
        // Only the id=1 row is a genuine edit; the moved rows are Unchanged.
        assert_eq!(modified.len(), 1, "exactly one row genuinely edited");
        assert_eq!(modified[0].cells[1].old, n("10"));
        assert_eq!(modified[0].cells[1].new, n("99"));
    }

    #[test]
    fn e_duplicate_rows_remove_one_not_both() {
        // [A,A,B] -> [A,B]: exactly one A Removed, not both (LCS-on-repeats).
        let old = table(&["id"], vec![vec![n("1")], vec![n("1")], vec![n("2")]]);
        let new = table(&["id"], vec![vec![n("1")], vec![n("2")]]);
        let diff = diff_fixture_tables(&old, &new);
        let removed = diff
            .rows
            .iter()
            .filter(|r| r.kind == RowChangeKind::Removed)
            .count();
        assert_eq!(removed, 1, "exactly one duplicate A removed, not both");
    }

    #[test]
    fn e_all_added_when_old_empty() {
        let old = FixtureTable::default();
        let new = table(&["id"], vec![vec![n("1")], vec![n("2")]]);
        let diff = diff_fixture_tables(&old, &new);
        assert_eq!(diff.rows.len(), 2);
        assert!(diff.rows.iter().all(|r| r.kind == RowChangeKind::Added));
    }

    #[test]
    fn e_all_removed_when_new_empty() {
        let old = table(&["id"], vec![vec![n("1")], vec![n("2")]]);
        let new = FixtureTable::default();
        let diff = diff_fixture_tables(&old, &new);
        assert_eq!(diff.rows.len(), 2);
        assert!(diff.rows.iter().all(|r| r.kind == RowChangeKind::Removed));
    }

    #[test]
    fn e_both_empty_is_no_change() {
        let diff = diff_fixture_tables(&FixtureTable::default(), &FixtureTable::default());
        assert!(!diff.has_real_change());
        assert!(diff.rows.is_empty());
    }

    // ----- F. column axis -----

    #[test]
    fn f_added_column_tagged_and_old_cell_absent() {
        let old = table(&["id"], vec![vec![n("1")], vec![n("2")]]);
        let new = table(
            &["id", "city"],
            vec![vec![n("1"), s("NYC")], vec![n("2"), s("LA")]],
        );
        let diff = diff_fixture_tables(&old, &new);
        // The unified columns: id Present, city Added.
        assert_eq!(diff.columns[0].status, ColumnStatus::Present);
        assert_eq!(diff.columns[1].name, "city");
        assert_eq!(diff.columns[1].status, ColumnStatus::Added);
        assert!(diff.has_real_change(), "a column add is a real change");
        // Each row's new city cell is present, old is Absent, changed.
        for r in &diff.rows {
            assert_eq!(r.cells[1].old, CellValue::Absent);
            assert!(r.cells[1].changed);
        }
    }

    #[test]
    fn f_removed_column_tagged_and_new_cell_absent() {
        let old = table(&["id", "legacy"], vec![vec![n("1"), s("x")]]);
        let new = table(&["id"], vec![vec![n("1")]]);
        let diff = diff_fixture_tables(&old, &new);
        let legacy = diff
            .columns
            .iter()
            .find(|c| c.name == "legacy")
            .expect("legacy column present in unified axis");
        assert_eq!(legacy.status, ColumnStatus::Removed);
        assert!(diff.has_real_change(), "a column removal is a real change");
        // The legacy column's new cell is Absent on the (unchanged-key) row.
        let legacy_idx = diff
            .columns
            .iter()
            .position(|c| c.name == "legacy")
            .unwrap();
        assert_eq!(diff.rows[0].cells[legacy_idx].new, CellValue::Absent);
    }

    // ----- Cross-source equivalence (the headline kill) -----

    #[test]
    fn cross_source_dict_value_vs_block_yaml_zero_changed_cells() {
        use crate::domain::unit_test_table::table_from_manifest_rows;
        let manifest = serde_json::json!([
            {"id": 1, "name": "alice"},
            {"id": 2, "name": "bob"}
        ]);
        let new = table_from_manifest_rows(&manifest, Some("dict")).unwrap();
        let yaml = "      - id: 1\n        name: 'alice'\n      - id: 2\n        name: 'bob'";
        let old = table_from_yaml_fragment(yaml, Some("dict")).unwrap();
        let diff = diff_fixture_tables(&old, &new);
        assert!(
            !diff.has_real_change(),
            "same logical data, different source format → zero changed cells"
        );
    }

    #[test]
    fn cross_source_empty_csv_cell_divergence_is_documented() {
        // tracked: cute-dbt#124 — core csv ships empty cell as Str("") while
        // fusion/OLD-YAML csv map empty → Null, so a core manifest with an
        // empty csv cell diffs as changed against the OLD-YAML side. No
        // committed fixture exercises empty csv cells; the real fix lives in
        // File-1 typing (type_cell_value of an empty Value::String). This
        // test pins the CURRENT (divergent) behavior so it is visible.
        use crate::domain::unit_test_table::table_from_manifest_rows;
        // Core-style: array of string dicts, one empty cell.
        let core = serde_json::json!([{"id": "1", "note": ""}]);
        let new = table_from_manifest_rows(&core, Some("csv")).unwrap();
        // OLD csv block-scalar of the same data (empty trailing field).
        let old = table_from_yaml_fragment("        id,note\n        1,", Some("csv")).unwrap();
        let diff = diff_fixture_tables(&old, &new);
        // Current behavior: the empty cell diverges Str("") vs Null → changed.
        assert!(
            diff.has_real_change(),
            "documents the empty-csv divergence (tracked: cute-dbt#85)"
        );
    }

    // ----- OLD-side YAML region slicing -----

    #[test]
    fn slice_given_region_attributes_by_input_value() {
        // Two givens; slice must attribute rows to the matching input.
        let old_text = "      - input: ref('a')\n        format: dict\n        rows:\n          - id: 1\n      - input: ref('b')\n        format: dict\n        rows:\n          - id: 2";
        let (region_a, fmt_a) = slice_named_region(old_text, "input", Some("ref('a')")).unwrap();
        assert_eq!(fmt_a.as_deref(), Some("dict"));
        let tbl_a = table_from_yaml_fragment(&region_a, fmt_a.as_deref()).unwrap();
        assert_eq!(tbl_a.rows.len(), 1);
        assert_eq!(tbl_a.rows[0].cells[0].value, n("1"));

        let (region_b, _) = slice_named_region(old_text, "input", Some("ref('b')")).unwrap();
        let tbl_b = table_from_yaml_fragment(&region_b, Some("dict")).unwrap();
        assert_eq!(tbl_b.rows[0].cells[0].value, n("2"));
    }

    #[test]
    fn slice_expect_region_isolated_from_givens() {
        let old_text = "      - input: ref('a')\n        rows:\n          - id: 1\n      expect:\n        rows:\n          - id: 99";
        let (region, _) = slice_named_region(old_text, "expect", None).unwrap();
        let tbl = table_from_yaml_fragment(&region, Some("dict")).unwrap();
        assert_eq!(tbl.rows.len(), 1);
        assert_eq!(tbl.rows[0].cells[0].value, n("99"));
    }

    #[test]
    fn slice_absent_input_returns_none() {
        let old_text = "      - input: ref('a')\n        rows:\n          - id: 1";
        assert!(slice_named_region(old_text, "input", Some("ref('zzz')")).is_none());
    }

    #[test]
    fn slice_csv_block_scalar_region() {
        let old_text = "      - input: ref('c')\n        format: csv\n        rows: |\n          id,name\n          1,alice";
        let (region, fmt) = slice_named_region(old_text, "input", Some("ref('c')")).unwrap();
        assert_eq!(fmt.as_deref(), Some("csv"));
        let tbl = table_from_yaml_fragment(&region, fmt.as_deref()).unwrap();
        assert_eq!(tbl.columns, vec!["id".to_string(), "name".to_string()]);
        assert_eq!(tbl.rows.len(), 1);
    }

    // ----- table_diff_if_changed gating -----

    #[test]
    fn table_diff_if_changed_both_none_is_none() {
        // sql/opaque on both sides → no cell diff.
        assert!(table_diff_if_changed(None, None).is_none());
    }

    #[test]
    fn table_diff_if_changed_identical_is_none() {
        let t = table(&["id"], vec![vec![n("1")]]);
        assert!(table_diff_if_changed(Some(t.clone()), Some(t)).is_none());
    }

    #[test]
    fn table_diff_if_changed_emits_on_real_change() {
        let old = table(&["id"], vec![vec![n("1")]]);
        let new = table(&["id"], vec![vec![n("2")]]);
        assert!(table_diff_if_changed(Some(old), Some(new)).is_some());
    }

    // ----- J. wire-shape / serde round-trip -----

    #[test]
    fn j_enum_tokens_are_exact() {
        assert_eq!(
            serde_json::to_string(&RowChangeKind::Modified).unwrap(),
            r#""modified""#
        );
        assert_eq!(
            serde_json::to_string(&RowChangeKind::Added).unwrap(),
            r#""added""#
        );
        assert_eq!(
            serde_json::to_string(&RowChangeKind::Removed).unwrap(),
            r#""removed""#
        );
        assert_eq!(
            serde_json::to_string(&RowChangeKind::Unchanged).unwrap(),
            r#""unchanged""#
        );
        assert_eq!(
            serde_json::to_string(&ColumnStatus::Present).unwrap(),
            r#""present""#
        );
        assert_eq!(
            serde_json::to_string(&ColumnStatus::Added).unwrap(),
            r#""added""#
        );
        assert_eq!(
            serde_json::to_string(&ColumnStatus::Removed).unwrap(),
            r#""removed""#
        );
    }

    #[test]
    fn j_unit_test_data_diff_round_trips() {
        let diff = UnitTestDataDiff {
            given: vec![NamedTableDiff {
                input: "ref('a')".into(),
                diff: FixtureTableDiff {
                    columns: vec![DiffColumn {
                        name: "id".into(),
                        status: ColumnStatus::Present,
                    }],
                    rows: vec![RowChange {
                        kind: RowChangeKind::Modified,
                        cells: vec![CellChange {
                            old: n("1"),
                            new: n("2"),
                            changed: true,
                        }],
                    }],
                },
            }],
            expect: Some(FixtureTableDiff {
                columns: vec![DiffColumn {
                    name: "id".into(),
                    status: ColumnStatus::Added,
                }],
                rows: vec![RowChange {
                    kind: RowChangeKind::Added,
                    cells: vec![CellChange {
                        old: CellValue::Absent,
                        new: n("9"),
                        changed: true,
                    }],
                }],
            }),
        };
        let back: UnitTestDataDiff =
            serde_json::from_str(&serde_json::to_string(&diff).unwrap()).unwrap();
        assert_eq!(back, diff);
    }

    #[test]
    fn j_default_data_diff_is_empty() {
        let d = UnitTestDataDiff::default();
        assert!(d.is_empty());
        assert!(d.given.is_empty());
        assert!(d.expect.is_none());
    }
}
