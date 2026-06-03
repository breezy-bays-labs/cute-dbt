//! Cross-module reconstruction-path tests for the cell-level unit-test
//! data-table diff (cute-dbt#98, File 2).
//!
//! These exercise the full driver
//! [`reconstruct_table_diffs`](cute_dbt::domain::cell_diff::reconstruct_table_diffs)
//! against a real [`BlockDiff`](cute_dbt::domain::pr_diff) reconstructed
//! from synthetic PR-diff hunks — the path the inline unit tests in
//! `src/domain/cell_diff.rs` cannot reach (they test the pure
//! `diff_fixture_tables` in isolation). The keystone is the block-style
//! dict single-cell edit: it proves the Context + Removed projection
//! rebuilds a COMPLETE OLD table from a diff that only carries the one
//! edited key-line in its removed body.
//!
//! All fixtures are inline synthetic YAML + `serde_json::Value` (no
//! committed fixture file → no `tests/fixtures/MANIFEST.toml` entry needed).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cute_dbt::adapters::manifest::FileManifestSource;
use cute_dbt::domain::cell_diff::{RowChangeKind, reconstruct_table_diffs};
use cute_dbt::domain::manifest::{Manifest, ManifestMetadata, NodeId};
use cute_dbt::domain::pr_diff::{FileHunks, Hunk, NormalizedDiffIndex, PrDiff};
use cute_dbt::domain::state::InScopeSet;
use cute_dbt::domain::unit_test::{UnitTest, UnitTestExpect, UnitTestGiven};
use cute_dbt::domain::unit_test_table::{CellValue, table_from_manifest_rows};
use cute_dbt::domain::unit_test_yaml::UnitTestYamlBlock;
use cute_dbt::ports::ManifestSource;

// ---------------------------------------------------------------------
// builders
// ---------------------------------------------------------------------

const OFP: &str = "models/_unit_tests.yml";

/// A unit test with one `given` (input + rows + format) and an empty
/// `expect`, declared at [`OFP`].
fn ut_given(name: &str, input: &str, rows: serde_json::Value, format: Option<&str>) -> UnitTest {
    UnitTest::new(
        name.to_owned(),
        NodeId::new("model.shop.m"),
        vec![UnitTestGiven::new(
            input.to_owned(),
            rows,
            format.map(str::to_owned),
            None,
        )],
        UnitTestExpect::new(serde_json::Value::Null, None, None),
        None,
        cute_dbt::domain::manifest::DependsOn::default(),
        None,
        None,
        Some(OFP.to_owned()),
    )
}

/// A unit test with NO `given` and an `expect` (rows + format), declared at
/// [`OFP`] — drives the `expect`-side reconstruction branch.
fn ut_expect(name: &str, rows: serde_json::Value, format: Option<&str>) -> UnitTest {
    UnitTest::new(
        name.to_owned(),
        NodeId::new("model.shop.m"),
        vec![],
        UnitTestExpect::new(rows, format.map(str::to_owned), None),
        None,
        cute_dbt::domain::manifest::DependsOn::default(),
        None,
        None,
        Some(OFP.to_owned()),
    )
}

/// A `Manifest` carrying a single unit test keyed by `id`.
fn manifest_with(id: &str, ut: UnitTest) -> Manifest {
    let mut tests = HashMap::new();
    tests.insert(id.to_owned(), ut);
    Manifest::new(
        ManifestMetadata::new("v12"),
        HashMap::new(),
        tests,
        HashMap::new(),
    )
}

/// A `UnitTestYamlBlock` whose `raw` is the working-tree (NEW) block,
/// starting at 1-based source line `block_start`.
fn block_at(raw: &str, block_start: usize) -> UnitTestYamlBlock {
    let n = raw.lines().count();
    UnitTestYamlBlock::new(
        raw.to_owned(),
        block_start,
        block_start,
        block_start + n - 1,
    )
}

/// A `--unified=0` replacement hunk: `new_len == added.len()`.
fn replace(new_start: usize, removed: &[&str], added: &[&str]) -> Hunk {
    Hunk {
        new_start,
        new_len: added.len(),
        removed_lines: removed.iter().map(|s| (*s).to_owned()).collect(),
        added_lines: added.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// Build the single-file `NormalizedDiffIndex` for [`OFP`].
fn index_for(hunks: Vec<Hunk>) -> NormalizedDiffIndex {
    let diff = PrDiff {
        files: vec![FileHunks {
            path: OFP.to_owned(),
            hunks,
        }],
    };
    NormalizedDiffIndex::new(&diff, None)
}

fn changed_set(id: &str) -> InScopeSet {
    [id.to_owned()].into_iter().collect()
}

// ---------------------------------------------------------------------
// H. reconstruction path — the spine's keystone
// ---------------------------------------------------------------------

/// Block-style dict, single-cell edit. The diff's removed body carries
/// ONLY the one edited key-line (`payer_key: 1`); the sibling key
/// (`payer_id`) survives as Context. This proves the Context + Removed
/// projection rebuilds the complete 1-row OLD table, so the diff is exactly
/// one Modified cell (`payer_key 1 → 2`) with the sibling unchanged.
#[test]
fn h31_block_style_dict_single_cell_edit_is_one_modified_cell() {
    let id = "unit_test.shop.m.dim_payer";
    // Working-tree (NEW) block. The `- input:` is source line 1.
    let raw = "  - name: dim_payer\n    given:\n      - input: ref('payers')\n        format: dict\n        rows:\n          - payer_key: 2\n            payer_id: 'ACME'";
    let block = block_at(raw, 1);
    // NEW rows from the manifest: payer_key edited 1 -> 2.
    let manifest = manifest_with(
        id,
        ut_given(
            "dim_payer",
            "ref('payers')",
            serde_json::json!([{"payer_key": 2, "payer_id": "ACME"}]),
            Some("dict"),
        ),
    );
    // The edit lives at new-side line 6 (`          - payer_key: 2`).
    let index = index_for(vec![replace(
        6,
        &["          - payer_key: 1"],
        &["          - payer_key: 2"],
    )]);
    let mut blocks = HashMap::new();
    blocks.insert(id.to_owned(), block);

    let diffs = reconstruct_table_diffs(&manifest, &changed_set(id), &blocks, &index);
    let data = diffs.get(id).expect("a data diff must be emitted");
    assert_eq!(data.given.len(), 1, "one given changed");
    let td = &data.given[0].diff;
    assert_eq!(td.rows.len(), 1, "one row");
    assert_eq!(td.rows[0].kind, RowChangeKind::Modified);
    // Columns: payer_key, payer_id (first-seen order from the manifest).
    let pk = td
        .columns
        .iter()
        .position(|c| c.name == "payer_key")
        .unwrap();
    let pid = td
        .columns
        .iter()
        .position(|c| c.name == "payer_id")
        .unwrap();
    assert!(td.rows[0].cells[pk].changed, "payer_key changed");
    assert!(
        !td.rows[0].cells[pid].changed,
        "payer_id unchanged (Context)"
    );
    // Old payer_key was 1 (from the Removed line), new is 2 (manifest).
    use cute_dbt::domain::unit_test_table::CellValue;
    assert_eq!(
        td.rows[0].cells[pk].old,
        CellValue::Number("1".into()),
        "old payer_key reconstructed from the Removed line"
    );
    assert_eq!(td.rows[0].cells[pk].new, CellValue::Number("2".into()));
}

/// Inline-flow dict, single-cell edit. Each list item is a whole row, so
/// the removed body is the entire old row `- {id: 1, name: alice}`.
#[test]
fn h33_inline_flow_single_cell_edit_is_one_modified_cell() {
    let id = "unit_test.shop.m.dim_users";
    let raw = "  - name: dim_users\n    given:\n      - input: ref('users')\n        rows:\n          - {id: 1, name: bob}";
    let block = block_at(raw, 1);
    let manifest = manifest_with(
        id,
        ut_given(
            "dim_users",
            "ref('users')",
            serde_json::json!([{"id": 1, "name": "bob"}]),
            Some("dict"),
        ),
    );
    let index = index_for(vec![replace(
        5,
        &["          - {id: 1, name: alice}"],
        &["          - {id: 1, name: bob}"],
    )]);
    let mut blocks = HashMap::new();
    blocks.insert(id.to_owned(), block);

    let diffs = reconstruct_table_diffs(&manifest, &changed_set(id), &blocks, &index);
    let td = &diffs.get(id).expect("emitted").given[0].diff;
    assert_eq!(td.rows.len(), 1);
    assert_eq!(td.rows[0].kind, RowChangeKind::Modified);
    let name = td.columns.iter().position(|c| c.name == "name").unwrap();
    let idc = td.columns.iter().position(|c| c.name == "id").unwrap();
    assert!(td.rows[0].cells[name].changed, "name alice -> bob");
    assert!(!td.rows[0].cells[idc].changed, "id unchanged");
}

/// EXPECT-side, block-style dict single-cell edit — drives the
/// `build_data_diff` expect branch + `data.expect = Some(...)` (the
/// given-tests all leave expect Null). Proves the expect path reconstructs
/// the OLD expect table from Context + Removed lines, same as a given.
#[test]
fn expect_side_single_cell_edit_populates_data_expect() {
    let id = "unit_test.shop.m.expect_edit";
    // Working-tree (NEW) block: an `expect:` sub-block, row count edited.
    let raw = "  - name: expect_edit\n    expect:\n      format: dict\n      rows:\n        - count: 7\n          label: 'ok'";
    let block = block_at(raw, 1);
    // NEW expect rows from the manifest: count edited 5 -> 7.
    let manifest = manifest_with(
        id,
        ut_expect(
            "expect_edit",
            serde_json::json!([{"count": 7, "label": "ok"}]),
            Some("dict"),
        ),
    );
    // The edit is at new-side line 5 (`        - count: 7`).
    let index = index_for(vec![replace(
        5,
        &["        - count: 5"],
        &["        - count: 7"],
    )]);
    let mut blocks = HashMap::new();
    blocks.insert(id.to_owned(), block);

    let diffs = reconstruct_table_diffs(&manifest, &changed_set(id), &blocks, &index);
    let data = diffs.get(id).expect("a data diff must be emitted");
    assert!(data.given.is_empty(), "no given changed");
    let td = data.expect.as_ref().expect("data.expect must be Some");
    assert_eq!(td.rows.len(), 1);
    assert_eq!(td.rows[0].kind, RowChangeKind::Modified);
    use cute_dbt::domain::unit_test_table::CellValue;
    let count = td.columns.iter().position(|c| c.name == "count").unwrap();
    let label = td.columns.iter().position(|c| c.name == "label").unwrap();
    assert!(td.rows[0].cells[count].changed, "count changed 5 -> 7");
    assert!(
        !td.rows[0].cells[label].changed,
        "label unchanged (Context)"
    );
    assert_eq!(td.rows[0].cells[count].old, CellValue::Number("5".into()));
    assert_eq!(td.rows[0].cells[count].new, CellValue::Number("7".into()));
}

/// csv block-scalar edit: the header line survives as Context (only the data
/// row's removed body is in the hunk), so the reconstructed OLD csv body is
/// complete and one data row is Modified.
#[test]
fn h32_csv_block_scalar_edit_header_survives_one_row_modified() {
    let id = "unit_test.shop.m.mart_dq";
    let raw = "  - name: mart_dq\n    given:\n      - input: ref('dq')\n        format: csv\n        rows: |\n          id,status\n          1,pass";
    let block = block_at(raw, 1);
    // Core ships csv as array-of-string-dicts; NEW status = pass.
    let manifest = manifest_with(
        id,
        ut_given(
            "mart_dq",
            "ref('dq')",
            serde_json::json!([{"id": "1", "status": "pass"}]),
            Some("csv"),
        ),
    );
    // Edit the data row only (line 7); the header (line 6) is Context.
    let index = index_for(vec![replace(
        7,
        &["          1,fail"],
        &["          1,pass"],
    )]);
    let mut blocks = HashMap::new();
    blocks.insert(id.to_owned(), block);

    let diffs = reconstruct_table_diffs(&manifest, &changed_set(id), &blocks, &index);
    let td = &diffs.get(id).expect("emitted").given[0].diff;
    assert_eq!(td.columns.len(), 2, "id + status, header reconstructed");
    assert_eq!(td.rows.len(), 1);
    assert_eq!(td.rows[0].kind, RowChangeKind::Modified);
    let status = td.columns.iter().position(|c| c.name == "status").unwrap();
    assert!(td.rows[0].cells[status].changed, "status fail -> pass");
}

// ---------------------------------------------------------------------
// I. fallback / gating
// ---------------------------------------------------------------------

/// A sql-format given is opaque → no cell table → no entry (→ the #96
/// yaml_diff fallback renders). The diff still touches the block, so the
/// gating passes; it is the per-fixture opacity that suppresses the entry.
#[test]
fn i34_sql_format_given_emits_no_data_diff() {
    let id = "unit_test.shop.m.sql_given";
    let raw = "  - name: sql_given\n    given:\n      - input: ref('src')\n        format: sql\n        rows: SELECT 2 AS id";
    let block = block_at(raw, 1);
    let manifest = manifest_with(
        id,
        ut_given(
            "sql_given",
            "ref('src')",
            serde_json::json!("SELECT 2 AS id"),
            Some("sql"),
        ),
    );
    let index = index_for(vec![replace(
        5,
        &["        rows: SELECT 1 AS id"],
        &["        rows: SELECT 2 AS id"],
    )]);
    let mut blocks = HashMap::new();
    blocks.insert(id.to_owned(), block);

    let diffs = reconstruct_table_diffs(&manifest, &changed_set(id), &blocks, &index);
    assert!(
        !diffs.contains_key(id),
        "sql given is opaque → no data_diff entry (yaml_diff fallback)"
    );
}

/// A stale diff (the hunk's added body does not match the working-tree
/// block at its new-side line) is rejected by `block_aligns_with_hunks` →
/// no entry, mirroring `reconstruct_block_diffs`'s gating.
#[test]
fn i35_stale_diff_emits_no_entry() {
    let id = "unit_test.shop.m.stale";
    let raw = "  - name: stale\n    given:\n      - input: ref('x')\n        format: dict\n        rows:\n          - id: 2";
    let block = block_at(raw, 1);
    let manifest = manifest_with(
        id,
        ut_given(
            "stale",
            "ref('x')",
            serde_json::json!([{"id": 2}]),
            Some("dict"),
        ),
    );
    // The added body claims `- id: DRIFTED` at line 6, but the block's line
    // 6 is `          - id: 2` → misaligned (stale).
    let index = index_for(vec![replace(
        6,
        &["          - id: 1"],
        &["          - id: DRIFTED"],
    )]);
    let mut blocks = HashMap::new();
    blocks.insert(id.to_owned(), block);

    let diffs = reconstruct_table_diffs(&manifest, &changed_set(id), &blocks, &index);
    assert!(!diffs.contains_key(id), "stale diff → no entry");
}

/// A change entirely outside the test's block (no touching hunk) → no entry.
#[test]
fn i35_untouched_block_emits_no_entry() {
    let id = "unit_test.shop.m.untouched";
    let raw = "  - name: untouched\n    given:\n      - input: ref('x')\n        format: dict\n        rows:\n          - id: 1";
    // Block sits at source lines [10, 15].
    let block = block_at(raw, 10);
    let manifest = manifest_with(
        id,
        ut_given(
            "untouched",
            "ref('x')",
            serde_json::json!([{"id": 1}]),
            Some("dict"),
        ),
    );
    // The only hunk touches line 2 — far outside [10,15].
    let index = index_for(vec![replace(2, &["  was"], &["  now"])]);
    let mut blocks = HashMap::new();
    blocks.insert(id.to_owned(), block);

    let diffs = reconstruct_table_diffs(&manifest, &changed_set(id), &blocks, &index);
    assert!(
        !diffs.contains_key(id),
        "change outside the block → no entry"
    );
}

/// A whitespace-only edit reconstructs to an all-Context BlockDiff
/// (`reconstruct_one` drops ws-only pairs), so OLD == NEW text → equal
/// tables → no entry. Mirrors `BlockDiff::has_real_change` suppression.
#[test]
fn i36_whitespace_only_edit_emits_no_entry() {
    let id = "unit_test.shop.m.ws";
    // The working-tree row is `          - id: 1` (the NEW side).
    let raw = "  - name: ws\n    given:\n      - input: ref('x')\n        format: dict\n        rows:\n          - id: 1";
    let block = block_at(raw, 1);
    let manifest = manifest_with(
        id,
        ut_given(
            "ws",
            "ref('x')",
            serde_json::json!([{"id": 1}]),
            Some("dict"),
        ),
    );
    // The removed body differs from the added ONLY by trailing whitespace —
    // `ws_equal` treats the pair as Context (cute-dbt#111), all-Context diff.
    let index = index_for(vec![replace(
        6,
        &["          - id: 1   "],
        &["          - id: 1"],
    )]);
    let mut blocks = HashMap::new();
    blocks.insert(id.to_owned(), block);

    let diffs = reconstruct_table_diffs(&manifest, &changed_set(id), &blocks, &index);
    assert!(
        !diffs.contains_key(id),
        "whitespace-only edit → all-Context → no entry"
    );
}

/// A test with NO sliced block (no --project-root) → no entry.
#[test]
fn i35_absent_block_emits_no_entry() {
    let id = "unit_test.shop.m.noblock";
    let manifest = manifest_with(
        id,
        ut_given(
            "noblock",
            "ref('x')",
            serde_json::json!([{"id": 2}]),
            Some("dict"),
        ),
    );
    let index = index_for(vec![replace(
        6,
        &["          - id: 1"],
        &["          - id: 2"],
    )]);
    // blocks is empty → no slice for this id.
    let blocks: HashMap<String, UnitTestYamlBlock> = HashMap::new();

    let diffs = reconstruct_table_diffs(&manifest, &changed_set(id), &blocks, &index);
    assert!(!diffs.contains_key(id), "absent block → no entry");
}

/// A row ADDED in the working tree (the OLD block had one row, the NEW
/// manifest has two) reconstructs as one Unchanged + one Added through the
/// full driver — the end-to-end add path, not just the pure-diff unit test.
#[test]
fn reconstruction_row_added_is_unchanged_plus_added() {
    let id = "unit_test.shop.m.addrow";
    // Working-tree (NEW) block: two rows.
    let raw = "  - name: addrow\n    given:\n      - input: ref('x')\n        format: dict\n        rows:\n          - id: 1\n          - id: 2";
    let block = block_at(raw, 1);
    let manifest = manifest_with(
        id,
        ut_given(
            "addrow",
            "ref('x')",
            serde_json::json!([{"id": 1}, {"id": 2}]),
            Some("dict"),
        ),
    );
    // The diff adds line 7 (`- id: 2`); line 6 (`- id: 1`) is Context.
    let index = index_for(vec![Hunk {
        new_start: 7,
        new_len: 1,
        removed_lines: vec![],
        added_lines: vec!["          - id: 2".to_owned()],
    }]);
    let mut blocks = HashMap::new();
    blocks.insert(id.to_owned(), block);

    let diffs = reconstruct_table_diffs(&manifest, &changed_set(id), &blocks, &index);
    let td = &diffs.get(id).expect("emitted").given[0].diff;
    assert_eq!(td.rows.len(), 2);
    let kinds: Vec<RowChangeKind> = td.rows.iter().map(|r| r.kind).collect();
    assert!(
        kinds.contains(&RowChangeKind::Unchanged) && kinds.contains(&RowChangeKind::Added),
        "row add reconstructs as Unchanged + Added, got {kinds:?}"
    );
}

// ---------------------------------------------------------------------
// K. fusion csv-as-raw-string NEW path — end-to-end through the real
//    manifest adapter (the committed fusion-csv-raw-string.json fixture).
//    Closes the spine's tracked E2E gap: every other csv test here uses
//    core-style array-of-string-dicts or inline YAML; this is the ONLY
//    coverage of dbt-fusion's `rows: <raw CSV string>` encoding going
//    through `FileManifestSource.load` → the IR (cute-dbt#127).
// ---------------------------------------------------------------------

/// Absolute path to a committed fixture under `tests/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn fusion_csv_raw_string_new_path_value_infers_through_the_adapter() {
    // Load the synthetic dbt-fusion manifest through the REAL adapter. Its
    // unit test's given/expect carry `format: csv` with `rows` as a raw CSV
    // STRING body (the fusion encoding), not core's array-of-string-dicts.
    let manifest = FileManifestSource
        .load(&fixture("fusion-csv-raw-string.json"))
        .expect("fusion-csv-raw-string fixture loads as a v12 manifest");
    let ut = manifest
        .unit_test("unit_test.fusion_demo.test_dq_rollup_csv_raw_string")
        .expect("the csv-raw-string unit test is present");

    // --- GIVEN side: a raw CSV string `rows` drives the fusion NEW path.
    let given = &ut.given()[0];
    assert_eq!(given.format(), Some("csv"));
    assert!(
        given.rows().is_string(),
        "fusion encodes csv rows as a RAW STRING, not an array (got {:?})",
        given.rows()
    );
    let given_tbl = table_from_manifest_rows(given.rows(), given.format())
        .expect("the raw-string csv body parses into a FixtureTable");
    assert_eq!(
        given_tbl.columns,
        vec![
            "entity_type".to_string(),
            "quarantined_count".to_string(),
            "is_dq_valid".to_string(),
        ],
        "the RFC 4180 header is the column order"
    );
    assert_eq!(given_tbl.rows.len(), 2);
    // Value inference (cute-dbt#127) fired on the raw-string fields:
    // `encounters` → Str, `1` → Number, `false` → Bool.
    assert_eq!(
        given_tbl.rows[0].cells[0].value,
        CellValue::Str("encounters".into())
    );
    assert_eq!(
        given_tbl.rows[0].cells[1].value,
        CellValue::Number("1".into()),
        "a fusion csv numeric field infers Number, not Str"
    );
    assert_eq!(
        given_tbl.rows[0].cells[2].value,
        CellValue::Bool(false),
        "a fusion csv `false` field infers Bool"
    );
    assert_eq!(given_tbl.rows[1].cells[2].value, CellValue::Bool(true));

    // --- EXPECT side: the SAME logical data, reformatted (1 → 1.00, true →
    // TRUE). Because csv is value-inferred, the expect table is EQUAL to the
    // given table — a reformat-only change is a zero data diff (the #127
    // headline guarantee), proven end-to-end on a committed fixture.
    let expect = ut.expect();
    assert_eq!(expect.format(), Some("csv"));
    assert!(
        expect.rows().is_string(),
        "fusion expect csv is raw string too"
    );
    let expect_tbl = table_from_manifest_rows(expect.rows(), expect.format())
        .expect("the raw-string csv expect body parses");
    assert_eq!(
        given_tbl, expect_tbl,
        "1 vs 1.00 and false vs TRUE are value-equal: the reformatted expect \
         table converges with the given table (cute-dbt#127)"
    );
}
