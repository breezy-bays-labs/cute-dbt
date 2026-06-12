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
use serde_json::Value;

use crate::domain::manifest::Manifest;
use crate::domain::pr_diff::{
    BlockSpan, DiffLineKind, Hunk, NormalizedDiffIndex, block_aligns_with_hunks, block_diff_for,
    hunk_touches_block,
};
use crate::domain::state::InScopeSet;
use crate::domain::unit_test::UnitTest;
use crate::domain::unit_test_table::{
    Cell, CellValue, FixtureTable, TableRow, external_fixture_table, table_from_manifest_rows,
    table_from_yaml_fragment,
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
/// `given` carries one [`NamedTableDiff`] per *changed* `given` input
/// (each identified by its source ordinal, since two givens may share an
/// `input` reference). `expect` carries the `expect` fixture's diff, or
/// `None` when `expect` is sql/opaque or unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UnitTestDataDiff {
    /// Per-`given`-input table diffs, in source order, each tagged with its
    /// source [`ordinal`](NamedTableDiff::ordinal). Only givens whose fixture
    /// data actually changed appear here, so this vec is a *subset* of the
    /// test's givens — the ordinal (not the vec index) is the identity.
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

/// One `given` input's table diff, identified by its **source ordinal**
/// (its 0-based position in the test's `given:` list).
///
/// The ordinal — not the `input` text — is the stable identity, because
/// two `given` blocks can carry the *same* `ref(...)` input (e.g. two
/// fixtures against one upstream model); keying by `input` would collapse
/// both onto the first match. The render loop binds each rendered
/// given-section to its diff by this ordinal (cute-dbt#131).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedTableDiff {
    /// 0-based position of this `given` in the test's `given:` list — the
    /// stable per-given identity. Robust to the fact that
    /// [`UnitTestDataDiff::given`] is *filtered* to changed givens only, so
    /// a dense index into that vec would not align with the full given list
    /// the renderer iterates.
    pub ordinal: usize,
    /// The `given` input reference this diff belongs to (e.g.
    /// `ref('stg_orders')`). Display/debug only — NOT the identity, since
    /// two givens may share it.
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

/// One cell's before/after [`Cell`]s (each carrying its authored `display` +
/// canonical `key`) plus the precomputed semantic verdict (cute-dbt#138).
///
/// `changed` is `old.key != new.key` — the ONE equality oracle, computed once
/// here over the canonical [`CellValue`] **keys** (NEVER the display token) so
/// a format-only reformat (`1` → `1.00`, both keying to `Number("1")`) is
/// `changed: false` while a real value change is `changed: true`. The render
/// layer renders each side's `display` (authored truth) but takes the flag
/// verdict from here; shipping both axes lets cute-dbt#139 re-flag client-side
/// without a Rust round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellChange {
    /// The OLD cell (key [`CellValue::Absent`] for an added row/column).
    pub old: Cell,
    /// The NEW cell (key [`CellValue::Absent`] for a removed row/column).
    pub new: Cell,
    /// `old.key != new.key` — precomputed semantic verdict on the equality
    /// axis only (the display token never participates).
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

    let old_rows: Vec<Vec<Cell>> = project_rows(old, &col_names);
    let new_rows: Vec<Vec<Cell>> = project_rows(new, &col_names);

    let rows = align_rows(&old_rows, &new_rows);

    FixtureTableDiff { columns, rows }
}

/// Align the projected OLD/NEW rows: multiset-match equal keys as
/// `Unchanged`, then pair the residual positionally, emitting in NEW order
/// with trailing `Removed` appended.
fn align_rows(old_rows: &[Vec<Cell>], new_rows: &[Vec<Cell>]) -> Vec<RowChange> {
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
fn match_multiset(old_rows: &[Vec<Cell>], new_rows: &[Vec<Cell>]) -> (Vec<bool>, Vec<bool>) {
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
    old_rows: &[Vec<Cell>],
    new_rows: &[Vec<Cell>],
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
/// column the row lacks with an absent [`Cell`]. Each projected cell keeps its
/// authored display alongside the canonical key (cute-dbt#138).
fn project_rows(table: &FixtureTable, col_names: &[&str]) -> Vec<Vec<Cell>> {
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

/// The [`Cell`] of `row` for unified column `col`, or an absent cell (key
/// [`CellValue::Absent`], empty display) when this table has no such column.
fn cell_for_column(table: &FixtureTable, row: &TableRow, col: &str) -> Cell {
    table
        .columns
        .iter()
        .position(|c| c == col)
        .and_then(|idx| row.cells.get(idx))
        .cloned()
        .unwrap_or_else(|| Cell::new(CellValue::Absent))
}

/// The canonical row key — each cell's tag+value, length-prefixed and
/// concatenated. Built from the same [`CellValue`] as the cells, so two rows
/// differing only by source format hash equal; `Absent` and `Null` serialize
/// to distinct tokens.
///
/// The encoding is **injective**: every token is emitted as
/// `<byte-len>:<token>`, so a literal cell value containing `|` (or a
/// token-like prefix such as `str:`) can never realign across the cell
/// boundary and collapse two distinct rows onto one key. A bare separator
/// join (`token|token`) is ambiguous — e.g. `["a", "x|str:b"]` and
/// `["a|str:x", "b"]` both flatten to `str:a|str:x|str:b` — which would let
/// [`match_multiset`] mark unrelated rows `Unchanged` and silently drop a
/// real cell diff.
fn row_key(cells: &[Cell]) -> String {
    let mut key = String::new();
    for cell in cells {
        // Keyed on the canonical equality axis ONLY — the authored `display`
        // never participates, so the display lens (cute-dbt#139) cannot
        // re-pair rows (cute-dbt#138).
        let token = cell_key_token(&cell.key);
        // `<byte-len>:<token>` — the length prefix makes concatenation
        // unambiguous regardless of what bytes the token carries.
        key.push_str(&token.len().to_string());
        key.push(':');
        key.push_str(&token);
    }
    key
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

/// An absent [`Cell`] (key [`CellValue::Absent`], empty display) — the
/// add/remove placeholder for the side a row/column is missing from.
fn absent_cell() -> Cell {
    Cell::new(CellValue::Absent)
}

/// An `Unchanged` row: `old`/`new` echo the projected cells (display + key),
/// every `changed` is `false`.
fn unchanged_row(cells: &[Cell]) -> RowChange {
    RowChange {
        kind: RowChangeKind::Unchanged,
        cells: cells
            .iter()
            .map(|c| CellChange {
                old: c.clone(),
                new: c.clone(),
                changed: false,
            })
            .collect(),
    }
}

/// An `Added` row: `old` is absent, `new` is the projected cell. Honors the
/// `changed == old.key != new.key` contract — an `Absent` projected cell (a
/// column this row lacks) stays NOT changed (cute-dbt#138).
fn added_row(cells: &[Cell]) -> RowChange {
    RowChange {
        kind: RowChangeKind::Added,
        cells: cells
            .iter()
            .map(|c| CellChange {
                old: absent_cell(),
                new: c.clone(),
                changed: c.key != CellValue::Absent,
            })
            .collect(),
    }
}

/// A `Removed` row: `new` is absent, `old` is the projected cell. Honors the
/// `changed == old.key != new.key` contract — an `Absent` projected cell (a
/// column this row lacks) stays NOT changed (cute-dbt#138).
fn removed_row(cells: &[Cell]) -> RowChange {
    RowChange {
        kind: RowChangeKind::Removed,
        cells: cells
            .iter()
            .map(|c| CellChange {
                old: c.clone(),
                new: absent_cell(),
                changed: c.key != CellValue::Absent,
            })
            .collect(),
    }
}

/// Whether two projected rows share at least one column position where
/// BOTH cells are present (non-`Absent` key) — the heuristic that
/// distinguishes an in-place edit (`Modified`) from a wholesale replacement
/// (a separate `Removed` + `Added`).
fn rows_share_present_cell(old_cells: &[Cell], new_cells: &[Cell]) -> bool {
    old_cells
        .iter()
        .zip(new_cells.iter())
        .any(|(o, n)| o.key != CellValue::Absent && n.key != CellValue::Absent)
}

/// Fuse a residual OLD row's cells with a residual NEW row's cells into one
/// `Modified` row, computing each cell's `changed` verdict on the **key**
/// axis only (`old.key != new.key`) — a format-only display change is not a
/// change (cute-dbt#138).
fn modified_row(old_cells: &[Cell], new_cells: &[Cell]) -> RowChange {
    let cells = old_cells
        .iter()
        .zip(new_cells.iter())
        .map(|(o, n)| CellChange {
            old: o.clone(),
            new: n.clone(),
            changed: o.key != n.key,
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
        if let Some(data) = data_diff_for_test(current, id, blocks, index)
            && !data.is_empty()
        {
            out.insert(id.to_owned(), data);
        }
    }
    out
}

/// Reconstruct the old→new cell diff of an **external fixture file**
/// (cute-dbt#126 AC#3).
///
/// Unlike [`reconstruct_table_diffs`], whose OLD side is sliced from the
/// reconstructed unit-test **YAML block**, an external fixture's data lives
/// in its own file (`given[i].fixture` / `expect.fixture`). `new_text` is
/// that file's current body (read by the cli via the `ProjectFileReader`);
/// `hunks` are the PR diff's hunks for **that file's** path
/// (`index.hunks_for(fixture_path)`) — looked up **independently** of the
/// unit-test YAML's `changed` set, because a PR that edits ONLY the csv file
/// never touches the YAML block.
///
/// The OLD file body is reconstructed by reverse-applying the hunks over a
/// **whole-file** [`BlockSpan`] (the same primitive the cute-dbt#111 model-SQL
/// diff uses), taking every non-`Added` line. Both sides are parsed via
/// [`external_fixture_table`] (which normalizes BOM / trailing newlines, so a
/// whitespace-only file edit is a non-diff) and diffed with
/// [`diff_fixture_tables`].
///
/// `None` when: there are no hunks for the file; the diff is stale
/// ([`block_aligns_with_hunks`] fails) or touches nothing; either side is a
/// non-tabulatable (non-literal sql) fixture (→ the plain text/code view); or
/// the diff carries no real change (a format-only / whitespace edit).
#[must_use]
pub fn reconstruct_external_fixture_diff(
    new_text: &str,
    format: Option<&str>,
    hunks: &[Hunk],
) -> Option<FixtureTableDiff> {
    if hunks.is_empty() {
        return None;
    }
    // Whole-file span over the NEW file body, git's single trailing
    // terminator stripped (mirrors `reconstruct_model_sql_diffs`, cute-dbt#111).
    let span_text = new_text.strip_suffix('\n').unwrap_or(new_text);
    let span = BlockSpan::new(span_text, 1, span_text.split('\n').count());
    if !block_aligns_with_hunks(&span, hunks) {
        return None; // stale diff → plain grid (no cell diff)
    }
    let touching: Vec<&Hunk> = hunks
        .iter()
        .filter(|h| hunk_touches_block(span.start, span.end, h))
        .collect();
    if touching.is_empty() {
        return None;
    }
    let bd = block_diff_for(&span, &touching);
    // The complete pre-edit (OLD) file = every line that is NOT an addition,
    // i.e. Context + Removed lines, in order.
    let old_text: String = bd
        .lines
        .iter()
        .filter(|l| l.kind != DiffLineKind::Added)
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    // A non-literal sql fixture (or otherwise non-tabulatable) on either side
    // → `None` → the file falls back to its plain text/code view, never a
    // phantom all-added/all-removed cell grid.
    let new_tbl = external_fixture_table(new_text, format)?;
    let old_tbl = external_fixture_table(&old_text, format)?;
    let diff = diff_fixture_tables(&old_tbl, &new_tbl);
    diff.has_real_change().then_some(diff)
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
    // `ordinal` is the given's position in the test's `given:` list — the
    // stable identity threaded onto each `NamedTableDiff`. Because only
    // *changed* givens are pushed, the source ordinal (not the push index)
    // is what the renderer binds on (cute-dbt#131).
    for (ordinal, g) in ut.given().iter().enumerate() {
        let new_tbl = table_from_manifest_rows(g.rows(), g.format());
        let old = slice_named_region(old_text, "input", Some(g.input()));
        // Build the OLD table ONCE (borrowing `old`); both the rejection check
        // and the diff consume it — no second `table_from_yaml_fragment` parse.
        let old_tbl = old
            .as_ref()
            .and_then(|(region, fmt)| table_from_yaml_fragment(region, fmt.as_deref()));
        let new_rejected = is_rejected_sql_new(g.rows(), g.format(), new_tbl.as_ref());
        let old_rejected = is_rejected_sql_old(old.as_ref(), old_tbl.as_ref());
        if let Some(diff) = table_diff_if_changed(old_tbl, new_tbl, old_rejected, new_rejected) {
            data.given.push(NamedTableDiff {
                ordinal,
                input: g.input().to_owned(),
                diff,
            });
        }
    }
    let new_e = table_from_manifest_rows(ut.expect().rows(), ut.expect().format());
    let old_e = slice_named_region(old_text, "expect", None);
    let old_e_tbl = old_e
        .as_ref()
        .and_then(|(region, fmt)| table_from_yaml_fragment(region, fmt.as_deref()));
    let new_e_rejected =
        is_rejected_sql_new(ut.expect().rows(), ut.expect().format(), new_e.as_ref());
    let old_e_rejected = is_rejected_sql_old(old_e.as_ref(), old_e_tbl.as_ref());
    if let Some(diff) = table_diff_if_changed(old_e_tbl, new_e, old_e_rejected, new_e_rejected) {
        data.expect = Some(diff);
    }
    data
}

/// Whether the NEW (manifest) side is a **present-but-rejected** sql fixture:
/// `format: sql` with a non-empty raw `SELECT` string that
/// [`table_from_manifest_rows`] could NOT tabulate as literal rows
/// (cute-dbt#137). Distinct from a genuinely absent NEW side (an empty/`Null`
/// `rows`) — only a rejected sql degrades the diff to the #96 text fallback.
fn is_rejected_sql_new(rows: &Value, format: Option<&str>, new_tbl: Option<&FixtureTable>) -> bool {
    format == Some("sql")
        && new_tbl.is_none()
        && matches!(rows, Value::String(s) if !s.trim().is_empty())
}

/// Whether the OLD (reconstructed-YAML) side is a **present-but-rejected** sql
/// fixture: the slice found a `format: sql` sub-block whose `rows:` region
/// could NOT tabulate as literal rows (cute-dbt#137). A `None` slice means the
/// OLD fixture was genuinely ABSENT (a newly-added given), which is NOT
/// "rejected" and still flows to an all-added diff.
///
/// Bounded boundary (cute-dbt#137, not fixed by design): an OLD sql written
/// **inline** (`rows: <SELECT…>` on one line) slices to `None` — the
/// pre-existing inline-`rows:` limitation shared by every format — so an
/// inline non-literal OLD + literal NEW reads as "absent", not "rejected", and
/// still produces an all-added table. Real multi-line sql fixtures are block
/// scalars (`rows: |`), which slice correctly and degrade as intended.
///
/// Takes the already-built `old_tbl` (the caller parses the OLD region once)
/// rather than re-parsing — symmetric with [`is_rejected_sql_new`].
fn is_rejected_sql_old(
    old: Option<&(String, Option<String>)>,
    old_tbl: Option<&FixtureTable>,
) -> bool {
    matches!(old, Some((_, fmt)) if fmt.as_deref() == Some("sql")) && old_tbl.is_none()
}

/// Diff an OLD/NEW table pair, returning the diff only when the diff carries
/// a real change AND neither side is a present-but-rejected non-literal sql
/// fixture (cute-dbt#137).
///
/// `*_rejected` flags carry the **mixed-tabulability** signal: if EITHER side
/// is present-but-rejected sql (a non-literal `SELECT` that cannot tabulate),
/// the whole given's diff degrades to the #96 text fallback (`None`) — a
/// cell-level table diff against an empty stand-in would paint phantom
/// all-added / all-removed rows. A genuinely-absent OLD side
/// (`old_rejected == false`, `old == None`) still flows to an all-added diff.
fn table_diff_if_changed(
    old: Option<FixtureTable>,
    new: Option<FixtureTable>,
    old_rejected: bool,
    new_rejected: bool,
) -> Option<FixtureTableDiff> {
    // Mixed tabulability: a present-but-rejected sql on either side → degrade
    // the whole given to the #96 text fallback (cute-dbt#137).
    if old_rejected || new_rejected {
        return None;
    }
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
    let lines: Vec<&str> = old_text.lines().collect();
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
        assert_eq!(diff.rows[0].cells[1].old.key, n("100"));
        assert_eq!(diff.rows[0].cells[1].new.key, n("200"));
    }

    #[test]
    fn c_before_after_orientation_is_not_flipped() {
        // Guard against an old<->new transposition: old=10, new=20.
        let old = table(&["v"], vec![vec![n("10")]]);
        let new = table(&["v"], vec![vec![n("20")]]);
        let diff = diff_fixture_tables(&old, &new);
        assert_eq!(
            diff.rows[0].cells[0].old.key,
            n("10"),
            "old side must be 10"
        );
        assert_eq!(
            diff.rows[0].cells[0].new.key,
            n("20"),
            "new side must be 20"
        );
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
        assert_eq!(diff.rows[1].cells[0].old.key, CellValue::Absent);
        assert_eq!(diff.rows[1].cells[0].new.key, n("2"));
    }

    #[test]
    fn d_deleted_row_is_one_removed_rest_unchanged() {
        let old = table(&["id"], vec![vec![n("1")], vec![n("2")]]);
        let new = table(&["id"], vec![vec![n("1")]]);
        let diff = diff_fixture_tables(&old, &new);
        assert_eq!(diff.rows.len(), 2);
        assert_eq!(diff.rows[0].kind, RowChangeKind::Unchanged);
        assert_eq!(diff.rows[1].kind, RowChangeKind::Removed);
        assert_eq!(diff.rows[1].cells[0].old.key, n("2"));
        assert_eq!(diff.rows[1].cells[0].new.key, CellValue::Absent);
    }

    #[test]
    fn d_delimiter_bearing_cells_do_not_collide_onto_one_row_key() {
        // Regression (Gemini PR #130 row_key collision): a bare `token|token`
        // join is ambiguous when a literal cell value contains `|` or a
        // token-like prefix. OLD row `["a", "x|str:b"]` and NEW row
        // `["a|str:x", "b"]` BOTH flatten to `str:a|str:x|str:b` under the
        // old encoding — so `match_multiset` would mark the NEW row
        // `Unchanged` and silently drop a real cell diff. With injective
        // length-prefixed keys the two rows are distinct, so the diff is real.
        let old = table(&["c1", "c2"], vec![vec![s("a"), s("x|str:b")]]);
        let new = table(&["c1", "c2"], vec![vec![s("a|str:x"), s("b")]]);
        let diff = diff_fixture_tables(&old, &new);
        assert!(
            diff.has_real_change(),
            "distinct delimiter-bearing rows must produce a real diff, not collide"
        );
        // The one row is Modified: c1 a -> a|str:x, c2 x|str:b -> b.
        assert_eq!(diff.rows.len(), 1);
        assert_eq!(diff.rows[0].kind, RowChangeKind::Modified);
        assert_eq!(diff.rows[0].cells[0].old.key, s("a"));
        assert_eq!(diff.rows[0].cells[0].new.key, s("a|str:x"));
        assert_eq!(diff.rows[0].cells[1].old.key, s("x|str:b"));
        assert_eq!(diff.rows[0].cells[1].new.key, s("b"));
    }

    #[test]
    fn d_delimiter_bearing_identical_rows_still_match_as_unchanged() {
        // The flip side: a row whose cells legitimately contain `|`/token-like
        // text, identical on both sides, must still match as Unchanged (the
        // injective key is deterministic, not merely collision-avoidant).
        let t = table(&["c1", "c2"], vec![vec![s("a|str:x"), s("3:str:b")]]);
        let diff = diff_fixture_tables(&t, &t);
        assert!(!diff.has_real_change(), "identical rows → Unchanged");
        assert_eq!(diff.rows[0].kind, RowChangeKind::Unchanged);
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
        assert_eq!(diff.rows[0].cells[0].new.key, n("1"));
        assert_eq!(diff.rows[0].cells[1].old.key, n("10"));
        assert_eq!(diff.rows[0].cells[1].new.key, n("11"));
        assert_eq!(diff.rows[1].cells[0].new.key, n("2"));
        assert_eq!(diff.rows[1].cells[1].old.key, n("20"));
        assert_eq!(diff.rows[1].cells[1].new.key, n("21"));
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
        assert_eq!(modified[0].cells[1].old.key, n("10"));
        assert_eq!(modified[0].cells[1].new.key, n("99"));
    }

    #[test]
    fn e_edit_plus_reorder_of_same_rows_stays_localized_documented_boundary() {
        // The ONE documented v1 boundary (module doc): two rows that are
        // BOTH edited AND swap relative order land crossed in the positional
        // residual zip. old=[id1,id2] -> new=[id2',id1'] (both keys changed,
        // both rows moved). The positional pairing crosses them, so the
        // per-cell `id` before->after is mis-attributed (the diff pairs the
        // 1st residual-OLD with the 1st residual-NEW). This pins the
        // load-bearing invariant that STILL holds at the boundary: the result
        // is LOCALIZED Modified rows, NEVER a false "all rows changed" blowup
        // and never a panic. A future move-detection pass (v2) would tighten
        // attribution; until then this test makes the boundary visible so a
        // regression that turns it into an all-Added/all-Removed blowup fails.
        let old = table(
            &["id", "v"],
            vec![vec![n("1"), n("10")], vec![n("2"), n("20")]],
        );
        // Both rows edited (v changed) AND swapped order.
        let new = table(
            &["id", "v"],
            vec![vec![n("2"), n("21")], vec![n("1"), n("11")]],
        );
        let diff = diff_fixture_tables(&old, &new);
        assert!(diff.has_real_change());
        // Two rows out, both Modified (localized) — not 2 Removed + 2 Added.
        assert_eq!(diff.rows.len(), 2, "stays two rows, no blowup");
        assert!(
            diff.rows.iter().all(|r| r.kind == RowChangeKind::Modified),
            "localized to Modified rows, never a false all-changed split"
        );
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
            assert_eq!(r.cells[1].old.key, CellValue::Absent);
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
        assert_eq!(diff.rows[0].cells[legacy_idx].new.key, CellValue::Absent);
    }

    #[test]
    fn f_added_removed_rows_keep_absent_cells_unchanged() {
        // cute-dbt#138 (CodeRabbit #140): when a row add/remove coincides with
        // an unrelated column add/remove, the unified axis projects an
        // `Absent` cell into the added/removed row. Per the
        // `changed == old.key != new.key` contract that `modified_row` honors,
        // an `Absent -> Absent` cell is NOT a change — the builders must not
        // hardcode `changed: true`. Pins both `&&`/comparison mutants.
        let absent = Cell::new(CellValue::Absent);
        let added = added_row(&[absent.clone(), Cell::new(n("2"))]);
        assert!(
            !added.cells[0].changed,
            "Absent projected cell in an Added row is not a change",
        );
        assert!(
            added.cells[1].changed,
            "a present projected cell in an Added row is a change",
        );
        let removed = removed_row(&[Cell::new(n("2")), absent.clone()]);
        assert!(
            removed.cells[0].changed,
            "a present projected cell in a Removed row is a change",
        );
        assert!(
            !removed.cells[1].changed,
            "Absent projected cell in a Removed row is not a change",
        );
    }

    #[test]
    fn g_added_row_absent_overlap_cell_is_unchanged_end_to_end() {
        // cute-dbt#138 (CI stage review): the integration counterpart of
        // `f_added_removed_rows_keep_absent_cells_unchanged`. A row add that
        // COINCIDES with a column add makes the unified column axis project an
        // `Absent` cell into the Added row. Building the tables through the
        // production normalizer (not the builders directly) proves the
        // end-to-end path leaves that `Absent -> Absent` cell unflagged.
        use crate::domain::unit_test_table::table_from_manifest_rows;
        let old = table_from_manifest_rows(&serde_json::json!([{"a": 1}]), Some("dict")).unwrap();
        let new = table_from_manifest_rows(&serde_json::json!([{"a": 1}, {"b": 2}]), Some("dict"))
            .unwrap();
        let diff = diff_fixture_tables(&old, &new);
        let a = diff
            .columns
            .iter()
            .position(|c| c.name == "a")
            .expect("col a");
        let b = diff
            .columns
            .iter()
            .position(|c| c.name == "b")
            .expect("col b");
        let added = diff
            .rows
            .iter()
            .find(|r| r.kind == RowChangeKind::Added)
            .expect("{b:2} is wholly added — shares no present cell with {a:1}");
        assert_eq!(
            added.cells[a].new.key,
            CellValue::Absent,
            "the added row lacks column a, so its a-cell is Absent",
        );
        assert!(
            !added.cells[a].changed,
            "Absent->Absent in the Added row stays unchanged (row-add overlaps column-add)",
        );
        assert!(
            added.cells[b].changed,
            "the present b-cell in the Added row is a change",
        );
    }

    // ----- cute-dbt#125: override-only edit (verify-and-pin) -----
    //
    // An override-only edit (a change confined to the `overrides:` block —
    // `macros` / `vars` / `env_vars`, all 3 dbt override kinds present here)
    // leaves `given` / `expect` byte-identical. The two diff surfaces must
    // split cleanly:
    //   - the #96 YAML *text* diff (`reconstruct_block_diffs`) carries the
    //     override change pair — because the #69 slicer spans the WHOLE
    //     `- name:` entry, so `overrides:` (a sibling of given/expect) rides
    //     along by construction;
    //   - the #98 cell diff (`reconstruct_table_diffs`) returns NOTHING —
    //     given/expect cells are unchanged, so the drawer surfaces the text
    //     diff, not a misleading empty cell view.
    // This is the load-bearing #125 interaction (a payload-presence headless
    // test pins the *rendered* visibility separately). It must FAIL if the
    // slicer ever stops at given/expect (no override pair) or if the cell
    // diff ever fires on an override-only edit.
    #[test]
    fn override_only_edit_surfaces_in_text_diff_but_not_cell_diff_end_to_end() {
        use crate::domain::manifest::{DependsOn, Manifest, ManifestMetadata, NodeId};
        use crate::domain::pr_diff::{FileHunks, PrDiff, reconstruct_block_diffs};
        use crate::domain::unit_test::{UnitTestExpect, UnitTestGiven};

        let id = "unit_test.shop.orders.test_overrides_only";
        let ofp = "models/_ut.yml";

        // The working-tree (NEW) block, byte-aligned to the manifest rows
        // below. Lines are 1-based; `cutoff_days: 30` is line 7 (the hunk's
        // new-side anchor). All 3 override kinds are present so the slice is
        // proven to carry each.
        let raw = [
            "  - name: test_overrides_only", // 1
            "    model: orders",             // 2
            "    overrides:",                // 3
            "      macros:",                 // 4
            "        is_incremental: false", // 5
            "      vars:",                   // 6
            "        cutoff_days: 30",       // 7  <- edited line
            "      env_vars:",               // 8
            "        DBT_REGION: us-east-1", // 9
            "    given:",                    // 10
            "      - input: ref('orders')",  // 11
            "        format: dict",          // 12
            "        rows:",                 // 13
            "          - id: 1",             // 14
            "            amount: 100",       // 15
            "    expect:",                   // 16
            "      format: dict",            // 17
            "      rows:",                   // 18
            "        - id: 1",               // 19
            "          total: 100",          // 20
        ]
        .join("\n");

        let ut = UnitTest::new(
            "test_overrides_only",
            NodeId::new("model.shop.orders"),
            vec![UnitTestGiven::new(
                "ref('orders')",
                serde_json::json!([{"id": 1, "amount": 100}]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(
                serde_json::json!([{"id": 1, "total": 100}]),
                Some("dict".to_owned()),
                None,
            ),
            None,
            DependsOn::default(),
            None,
            None,
            Some(ofp.to_owned()),
        );
        let mut tests = HashMap::new();
        tests.insert(id.to_owned(), ut);
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            tests,
            HashMap::new(),
        );

        let mut blocks = HashMap::new();
        blocks.insert(id.to_owned(), UnitTestYamlBlock::new(raw.clone(), 1, 1, 20));

        // A `--unified=0` replacement of ONLY the overrides line (`vars`):
        // `cutoff_days: 7` → `cutoff_days: 30`. The `+` body equals the
        // working-tree line 7, so N7b aligns; the hunk sits inside the block.
        let diff = PrDiff {
            renames: Vec::new(),
            files: vec![FileHunks {
                path: ofp.to_owned(),
                hunks: vec![Hunk {
                    new_start: 7,
                    new_len: 1,
                    removed_lines: vec!["        cutoff_days: 7".to_owned()],
                    added_lines: vec!["        cutoff_days: 30".to_owned()],
                }],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        let changed: InScopeSet = [id.to_owned()].into_iter().collect();

        // (1) The #96 YAML text diff carries the override change pair.
        let yaml_diffs = reconstruct_block_diffs(&current, &changed, &blocks, &index);
        let bd = yaml_diffs
            .get(id)
            .expect("an override-only edit produces a reconstructed YAML block diff");
        let removed: Vec<&str> = bd
            .lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Removed)
            .map(|l| l.text.as_str())
            .collect();
        let added: Vec<&str> = bd
            .lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Added)
            .map(|l| l.text.as_str())
            .collect();
        assert!(
            removed.iter().any(|t| t.contains("cutoff_days: 7")),
            "the removed line is the pre-edit override value; got removed {removed:?}",
        );
        assert!(
            added.iter().any(|t| t.contains("cutoff_days: 30")),
            "the added line is the working-tree override value; got added {added:?}",
        );
        // Reconstruction preserves the WHOLE block: sibling override keys AND
        // the given/expect data ride along as context around the change pair.
        // (That the SLICER includes `overrides:` in the block — the
        // foundational #125 link — is pinned separately by
        // `unit_test_yaml::tests::overrides_block_and_all_three_kinds_are_included_in_the_slice`;
        // the block here is hand-built, so this asserts only that untouched
        // sibling lines survive reconstruction.)
        let context: Vec<&str> = bd
            .lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Context)
            .map(|l| l.text.as_str())
            .collect();
        assert!(
            context.iter().any(|t| t.contains("env_vars")),
            "a sibling override key must ride along as context; got context {context:?}",
        );
        assert!(
            context.iter().any(|t| t.contains("amount: 100")),
            "the unchanged given rows ride along as context; got context {context:?}",
        );

        // (2) The #98 cell diff returns NOTHING — given/expect are unchanged,
        // so the cell-level grid carries no spurious diff and the drawer falls
        // back to the YAML text diff (verified above).
        let data_diffs = reconstruct_table_diffs(&current, &changed, &blocks, &index);
        let data_diff = data_diffs.get(id);
        assert!(
            data_diff.is_none(),
            "an override-only edit leaves given/expect cells identical → no cell diff; \
             got {data_diff:?}",
        );
    }

    // ----- cute-dbt#131: per-given ordinal is the SOURCE position -----
    //
    // When an *earlier* given is unchanged, the later changed given's
    // `NamedTableDiff` must carry its SOURCE ordinal (1), not a dense
    // push-index (0). The renderer binds each given-section to its diff by
    // this ordinal, so a dense index would mis-bind two givens that share a
    // `ref(...)`. Fails if `build_data_diff` ever numbers by push order.
    #[test]
    fn data_diff_ordinal_is_source_position_not_push_index() {
        use crate::domain::manifest::{DependsOn, NodeId};
        use crate::domain::unit_test::{UnitTest, UnitTestExpect, UnitTestGiven};

        // OLD working-tree text: given[0] ref('a') (id/name), given[1]
        // ref('b') (id/v=1), expect (id/ok).
        let old_text = [
            "  - name: t",
            "    model: m",
            "    given:",
            "      - input: ref('a')",
            "        format: dict",
            "        rows:",
            "          - id: 1",
            "            name: alice",
            "      - input: ref('b')",
            "        format: dict",
            "        rows:",
            "          - id: 1",
            "            v: 1",
            "    expect:",
            "      format: dict",
            "      rows:",
            "        - id: 1",
            "          ok: 1",
        ]
        .join("\n");

        // NEW (manifest): given[0] UNCHANGED, given[1] v 1 → 2 (changed),
        // expect unchanged.
        let ut = UnitTest::new(
            "t",
            NodeId::new("model.shop.m"),
            vec![
                UnitTestGiven::new(
                    "ref('a')",
                    serde_json::json!([{"id": 1, "name": "alice"}]),
                    Some("dict".to_owned()),
                    None,
                ),
                UnitTestGiven::new(
                    "ref('b')",
                    serde_json::json!([{"id": 1, "v": 2}]),
                    Some("dict".to_owned()),
                    None,
                ),
            ],
            UnitTestExpect::new(
                serde_json::json!([{"id": 1, "ok": 1}]),
                Some("dict".to_owned()),
                None,
            ),
            None,
            DependsOn::default(),
            None,
            None,
            Some("models/_ut.yml".to_owned()),
        );

        let data = build_data_diff(&ut, &old_text);
        // Only the SECOND given changed → exactly one entry, tagged with its
        // SOURCE ordinal 1 (not the dense push-index 0).
        assert_eq!(data.given.len(), 1, "only given[1] changed");
        assert_eq!(
            data.given[0].ordinal, 1,
            "the changed given is at source position 1, not a dense index",
        );
        assert_eq!(data.given[0].input, "ref('b')");
        assert!(data.expect.is_none(), "expect rows are unchanged");
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
    fn cross_source_empty_csv_cell_converges_no_change() {
        // cute-dbt#127 (closes #124): the core csv-as-array-of-string-dicts
        // NEW path now routes string cells through `cell_from_csv_value` →
        // `cell_from_csv_token`, so an empty core cell keys to `Null` — the SAME as
        // the OLD-YAML csv path's empty → `Null`. Both sides converge: `id`
        // "1" → Number on both, `note` "" → Null on both. The previously-
        // documented Str("") vs Null divergence is GONE; this now asserts a
        // true zero-diff (the headline format/engine-convergence guarantee).
        use crate::domain::unit_test_table::table_from_manifest_rows;
        // Core-style: array of string dicts, one empty cell.
        let core = serde_json::json!([{"id": "1", "note": ""}]);
        let new = table_from_manifest_rows(&core, Some("csv")).unwrap();
        // OLD csv block-scalar of the same data (empty trailing field).
        let old = table_from_yaml_fragment("        id,note\n        1,", Some("csv")).unwrap();
        let diff = diff_fixture_tables(&old, &new);
        assert!(
            !diff.has_real_change(),
            "core string-dicts and OLD-YAML csv of the same data now converge \
             (empty → Null both sides, id → Number both sides): zero diff"
        );
    }

    #[test]
    fn csv_real_numeric_value_change_still_diffs_after_inference() {
        // cute-dbt#127 CHECK-1 clause B: value-inference must NOT over-collapse.
        // A reformat-only change is zero-diff (proved above), but a GENUINE
        // value change on the same new csv-inference path MUST still diff. Use
        // a numeric pair (`1` vs `1.5`) — NOT `1` vs `"alice"` — so this guards
        // the `canonicalize_str_number` path specifically, not mere string
        // inequality: both sides infer Number, and `Number("1") != Number("1.5")`
        // must survive all the way through `has_real_change()`.
        use crate::domain::unit_test_table::{CellValue, table_from_manifest_rows};
        let old = table_from_manifest_rows(&serde_json::json!([{"id": "1"}]), Some("csv")).unwrap();
        let new =
            table_from_manifest_rows(&serde_json::json!([{"id": "1.5"}]), Some("csv")).unwrap();
        // Both inferred to Number (the inference fired on each side)…
        assert_eq!(old.rows[0].cells[0].key, CellValue::Number("1".into()));
        assert_eq!(new.rows[0].cells[0].key, CellValue::Number("1.5".into()));
        // …and the genuine value change survives to the diff verdict.
        let diff = diff_fixture_tables(&old, &new);
        assert!(
            diff.has_real_change(),
            "1 → 1.5 is a real value change: must diff even after csv inference"
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
        assert_eq!(tbl_a.rows[0].cells[0].key, n("1"));

        let (region_b, _) = slice_named_region(old_text, "input", Some("ref('b')")).unwrap();
        let tbl_b = table_from_yaml_fragment(&region_b, Some("dict")).unwrap();
        assert_eq!(tbl_b.rows[0].cells[0].key, n("2"));
    }

    #[test]
    fn slice_expect_region_isolated_from_givens() {
        let old_text = "      - input: ref('a')\n        rows:\n          - id: 1\n      expect:\n        rows:\n          - id: 99";
        let (region, _) = slice_named_region(old_text, "expect", None).unwrap();
        let tbl = table_from_yaml_fragment(&region, Some("dict")).unwrap();
        assert_eq!(tbl.rows.len(), 1);
        assert_eq!(tbl.rows[0].cells[0].key, n("99"));
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

    // ----- unquote: the input-value quote stripper -----

    #[test]
    fn unquote_strips_only_a_matching_surrounding_pair() {
        // A matched double- OR single-quote pair is stripped to the inner
        // text; everything else is returned verbatim. Pins every branch of
        // `unquote`: the two `&&` clauses (a one-sided quote must NOT strip),
        // the `||` between them, both `==` quote-char checks, the
        // `bytes.len() - 1` last-byte index, and the `s[1..s.len() - 1]`
        // slice bounds — a wrong stripper silently mis-matches a quoted
        // `input:` reference and attributes OLD rows to the wrong given.
        assert_eq!(unquote("\"x\""), "x", "double-quote pair strips");
        assert_eq!(unquote("'x'"), "x", "single-quote pair strips");
        // One-sided quotes are NOT a pair → verbatim (kills the `&&`→`||`
        // and the `==`→`!=` flips, which would strip a half-quoted token).
        assert_eq!(unquote("\"x"), "\"x", "leading-only quote is not a pair");
        assert_eq!(unquote("x\""), "x\"", "trailing-only quote is not a pair");
        assert_eq!(unquote("'x"), "'x", "leading-only apostrophe is not a pair");
        assert_eq!(
            unquote("x'"),
            "x'",
            "trailing-only apostrophe is not a pair"
        );
        // A mixed pair (`"x'`) is not a matching pair → verbatim.
        assert_eq!(unquote("\"x'"), "\"x'", "mixed quotes are not a pair");
        // Unquoted / too-short stay verbatim.
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(
            unquote("\""),
            "\"",
            "a lone quote is too short to be a pair"
        );
        // The exact two-char pair strips to empty (pins the slice bounds).
        assert_eq!(unquote("\"\""), "", "an empty quoted pair strips to empty");
    }

    #[test]
    fn slice_given_region_matches_a_quoted_input_value() {
        // The end-to-end path: a `given` whose `input:` value is wrapped in
        // double quotes must still be matched by its unquoted reference,
        // exercising `unquote` inside `is_subblock_opener`. Were `unquote`
        // broken, the quoted opener would not match and the region would be
        // dropped (a silent miss of the whole given's data diff).
        let old_text =
            "      - input: \"ref('q')\"\n        format: dict\n        rows:\n          - id: 7";
        let (region, _) = slice_named_region(old_text, "input", Some("ref('q')")).unwrap();
        let tbl = table_from_yaml_fragment(&region, Some("dict")).unwrap();
        assert_eq!(tbl.rows.len(), 1);
        assert_eq!(tbl.rows[0].cells[0].key, n("7"));
    }

    // ----- table_diff_if_changed gating -----

    #[test]
    fn table_diff_if_changed_both_none_is_none() {
        // sql/opaque on both sides → no cell diff.
        assert!(table_diff_if_changed(None, None, false, false).is_none());
    }

    #[test]
    fn table_diff_if_changed_one_sided_none_still_diffs() {
        // Only when BOTH sides are opaque (None) is the diff skipped. A
        // None on ONE side (the OLD fixture was sql/absent, the NEW one has
        // real rows — or vice versa) must still diff against the empty
        // default, surfacing the rows as Added/Removed. Pins the
        // `old.is_none() && new.is_none()` guard: were the `&&` flipped to
        // `||`, a one-sided-None pair would short-circuit to None and the
        // whole table change would silently vanish (→ no cell diff emitted).
        let real = table(&["id"], vec![vec![n("1")]]);
        // NEW present, OLD opaque → the row is Added; a real change.
        assert!(
            table_diff_if_changed(None, Some(real.clone()), false, false).is_some(),
            "OLD-opaque + NEW-present must still emit a diff"
        );
        // OLD present, NEW opaque → the row is Removed; a real change.
        assert!(
            table_diff_if_changed(Some(real), None, false, false).is_some(),
            "OLD-present + NEW-opaque must still emit a diff"
        );
    }

    #[test]
    fn table_diff_if_changed_identical_is_none() {
        let t = table(&["id"], vec![vec![n("1")]]);
        assert!(table_diff_if_changed(Some(t.clone()), Some(t), false, false).is_none());
    }

    #[test]
    fn table_diff_if_changed_emits_on_real_change() {
        let old = table(&["id"], vec![vec![n("1")]]);
        let new = table(&["id"], vec![vec![n("2")]]);
        assert!(table_diff_if_changed(Some(old), Some(new), false, false).is_some());
    }

    // ----- cute-dbt#137 mixed-tabulability guard (the trap-2 trio) -----

    #[test]
    fn k_mixed_rejected_old_literal_new_degrades_to_text_fallback() {
        // OLD side is a present-but-rejected non-literal sql (e.g. a real
        // FROM clause); NEW side is a literal-sql table. A cell diff would
        // paint the NEW rows as phantom all-added against an empty OLD
        // stand-in — so degrade the whole given to the #96 text fallback.
        let literal_new = table(&["id"], vec![vec![n("1")]]);
        assert!(
            table_diff_if_changed(None, Some(literal_new), true, false).is_none(),
            "rejected-OLD + literal-NEW → None (text fallback), not phantom all-added"
        );
    }

    #[test]
    fn k_mixed_literal_old_rejected_new_degrades_to_text_fallback() {
        // The reverse: OLD literal, NEW present-but-rejected sql. A cell diff
        // would paint phantom all-removed rows.
        let literal_old = table(&["id"], vec![vec![n("1")]]);
        assert!(
            table_diff_if_changed(Some(literal_old), None, false, true).is_none(),
            "literal-OLD + rejected-NEW → None (text fallback), not phantom all-removed"
        );
    }

    #[test]
    fn k_genuinely_absent_old_literal_new_stays_all_added() {
        // A genuinely NEW given (OLD absent, `old_rejected == false`) still
        // flows to an all-added cell diff — the absent case is NOT a reject.
        let literal_new = table(&["id"], vec![vec![n("1")], vec![n("2")]]);
        let diff = table_diff_if_changed(None, Some(literal_new), false, false)
            .expect("absent-OLD + literal-NEW must still emit an all-added diff");
        assert!(diff.rows.iter().all(|r| r.kind == RowChangeKind::Added));
    }

    // ----- cute-dbt#137 rejected-sql discriminators -----

    #[test]
    fn k_is_rejected_sql_new_only_for_present_non_literal_sql() {
        // A non-literal sql (real FROM) with a None table → rejected.
        let rows = serde_json::json!("select id from src");
        assert!(is_rejected_sql_new(&rows, Some("sql"), None));
        // A literal sql that DID tabulate (table Some) → NOT rejected.
        let literal = table(&["id"], vec![vec![n("1")]]);
        let lit_rows = serde_json::json!("select 1 as id");
        assert!(!is_rejected_sql_new(&lit_rows, Some("sql"), Some(&literal)));
        // A non-sql format → never "rejected sql".
        assert!(!is_rejected_sql_new(
            &serde_json::json!(null),
            Some("dict"),
            None
        ));
        // An empty/whitespace sql string → genuinely absent, not rejected.
        assert!(!is_rejected_sql_new(
            &serde_json::json!("   "),
            Some("sql"),
            None
        ));
    }

    #[test]
    fn k_is_rejected_sql_old_only_for_present_non_literal_sql_slice() {
        // A sliced sql region that cannot tabulate → rejected. The caller
        // builds the table once and passes it in (None ⇒ rejected).
        let rejected = ("select id from src".to_owned(), Some("sql".to_owned()));
        let rejected_tbl = table_from_yaml_fragment(&rejected.0, rejected.1.as_deref());
        assert!(is_rejected_sql_old(Some(&rejected), rejected_tbl.as_ref()));
        // A sliced sql region that DOES tabulate (literal) → NOT rejected.
        let literal = ("select 1 as id".to_owned(), Some("sql".to_owned()));
        let literal_tbl = table_from_yaml_fragment(&literal.0, literal.1.as_deref());
        assert!(!is_rejected_sql_old(Some(&literal), literal_tbl.as_ref()));
        // A non-sql slice → not "rejected sql".
        let dict = ("- id: 1".to_owned(), Some("dict".to_owned()));
        let dict_tbl = table_from_yaml_fragment(&dict.0, dict.1.as_deref());
        assert!(!is_rejected_sql_old(Some(&dict), dict_tbl.as_ref()));
        // No slice (genuinely absent OLD) → NOT rejected.
        assert!(!is_rejected_sql_old(None, None));
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
                ordinal: 0,
                input: "ref('a')".into(),
                diff: FixtureTableDiff {
                    columns: vec![DiffColumn {
                        name: "id".into(),
                        status: ColumnStatus::Present,
                    }],
                    rows: vec![RowChange {
                        kind: RowChangeKind::Modified,
                        cells: vec![CellChange {
                            old: Cell::new(n("1")),
                            new: Cell::new(n("2")),
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
                        old: Cell::new(CellValue::Absent),
                        new: Cell::new(n("9")),
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

    #[test]
    fn j_data_diff_round_trips_over_cellvalue_x_expect_matrix() {
        // CodeRabbit PR #130: the new nested `data_diff` payload adds
        // optional/nested combinations (every CellValue variant, each
        // RowChangeKind/ColumnStatus, expect present/absent) that are easy to
        // regress without noticing. Exhaust the variant matrix and assert
        // serialize→deserialize is the identity so the wire contract — in
        // particular Absent-vs-Null adjacency (Absent = tag-only, Null =
        // tag-only, distinct tokens) — can't drift silently.
        let cell_variants = [
            CellValue::Null,
            CellValue::Absent,
            CellValue::Bool(true),
            CellValue::Bool(false),
            CellValue::Number("1.5".into()),
            CellValue::Str("x|y".into()), // delimiter-bearing literal
        ];
        let kinds = [
            RowChangeKind::Unchanged,
            RowChangeKind::Added,
            RowChangeKind::Removed,
            RowChangeKind::Modified,
        ];
        let statuses = [
            ColumnStatus::Present,
            ColumnStatus::Added,
            ColumnStatus::Removed,
        ];
        for old in &cell_variants {
            for new in &cell_variants {
                for kind in kinds {
                    for status in statuses {
                        for expect_present in [false, true] {
                            let table = FixtureTableDiff {
                                columns: vec![DiffColumn {
                                    name: "c".into(),
                                    status,
                                }],
                                rows: vec![RowChange {
                                    kind,
                                    cells: vec![CellChange {
                                        old: Cell::new(old.clone()),
                                        new: Cell::new(new.clone()),
                                        changed: old != new,
                                    }],
                                }],
                            };
                            let diff = UnitTestDataDiff {
                                given: vec![NamedTableDiff {
                                    ordinal: 0,
                                    input: "ref('a')".into(),
                                    diff: table.clone(),
                                }],
                                expect: expect_present.then(|| table.clone()),
                            };
                            let back: UnitTestDataDiff =
                                serde_json::from_str(&serde_json::to_string(&diff).unwrap())
                                    .unwrap();
                            assert_eq!(back, diff, "data_diff round-trip failed for {diff:?}");
                        }
                    }
                }
            }
        }
    }

    // ----- external fixture FILE old→new diff (cute-dbt#126 AC#3) -----

    /// One `--unified=0` hunk replacing line `n` (1-based) with `added`,
    /// removing `removed`.
    fn replace_hunk(n: usize, removed: &str, added: &str) -> Hunk {
        Hunk {
            new_start: n,
            new_len: 1,
            removed_lines: vec![removed.to_owned()],
            added_lines: vec![added.to_owned()],
        }
    }

    #[test]
    fn external_diff_csv_cell_change_is_a_modified_row() {
        // The fixture FILE's line 3 changed `2,20` → `2,99`. OLD is
        // reconstructed by reverse-applying the hunk; the cell diff is one
        // Modified row whose `amount` cell goes 20 → 99.
        let new_text = "id,amount\n1,10\n2,99\n";
        let hunks = [replace_hunk(3, "2,20", "2,99")];
        let diff = reconstruct_external_fixture_diff(new_text, Some("csv"), &hunks)
            .expect("a real cell change diffs");
        let modified: Vec<&RowChange> = diff
            .rows
            .iter()
            .filter(|r| r.kind == RowChangeKind::Modified)
            .collect();
        assert_eq!(modified.len(), 1, "exactly one modified row");
        let amount = &modified[0].cells[1];
        assert_eq!(amount.old.key, CellValue::Number("20".into()));
        assert_eq!(amount.new.key, CellValue::Number("99".into()));
        assert!(amount.changed, "the amount cell is flagged changed");
    }

    #[test]
    fn external_diff_no_hunks_is_none() {
        // A fixture file the PR did not touch → no hunks → no cell diff (the
        // grid renders without a diff toggle).
        assert_eq!(
            reconstruct_external_fixture_diff("id,amount\n1,10\n", Some("csv"), &[]),
            None
        );
    }

    #[test]
    fn external_diff_added_row() {
        // A pure-insertion hunk at new-side line 3 (`new_len` 1, no removed).
        let new_text = "id,amount\n1,10\n2,20\n";
        let hunks = [Hunk {
            new_start: 3,
            new_len: 1,
            removed_lines: vec![],
            added_lines: vec!["2,20".to_owned()],
        }];
        let diff = reconstruct_external_fixture_diff(new_text, Some("csv"), &hunks)
            .expect("an added row diffs");
        assert!(
            diff.rows.iter().any(|r| r.kind == RowChangeKind::Added),
            "the new row is an Added row",
        );
    }

    #[test]
    fn external_diff_format_only_change_is_none() {
        // `2,20` → `2,20.0` is a format-only numeric reformat: both cells key
        // to Number("20"), so the rows match and there is no real change.
        let new_text = "id,amount\n1,10\n2,20.0\n";
        let hunks = [replace_hunk(3, "2,20", "2,20.0")];
        assert_eq!(
            reconstruct_external_fixture_diff(new_text, Some("csv"), &hunks),
            None,
            "a format-only reformat is not a cell diff",
        );
    }

    #[test]
    fn external_diff_non_literal_sql_is_none() {
        // A non-literal sql fixture file is not tabulatable → no cell diff (the
        // file falls back to its code-block view, never a phantom grid).
        let new_text = "select id, amount from src where id > 1";
        let hunks = [replace_hunk(1, "select id from src", new_text)];
        assert_eq!(
            reconstruct_external_fixture_diff(new_text, Some("sql"), &hunks),
            None,
        );
    }
}
