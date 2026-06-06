//! Step definitions for `features/cell_table_diff.feature` — the
//! cell-level unit-test data-table diff (cute-dbt#98).
//!
//! Self-contained, mirroring `pr_diff_scoping.rs`'s strategy but with a
//! ROW-BEARING synthesizer: each scenario builds a synthetic in-memory
//! `Manifest` (a model + a unit test carrying inline fixture rows + an
//! `original_file_path`), synthesizes a `git diff --unified=0` patch (and
//! the working-tree YAML it references) that edits ONE fixture row in a
//! chosen way, runs the real `cute-dbt` subprocess with `--pr-diff
//! @<patch>`, and asserts against the embedded `cute-dbt-data` JSON payload
//! (`test["data_diff"]`, the structured analogue of the `yaml_diff` /
//! `sql_diff` assertions in `pr_diff_scoping.rs`).
//!
//! Why a separate module + feature file (not `pr_diff_scoping.rs`): that
//! harness's `synth_yaml` emits fixed-size 5-line blocks with `given: []`,
//! and its block-targeting arithmetic (`SYNTH_BLOCK_LINES`) is load-bearing
//! for all 15 of its scenarios. A row-bearing fixture changes the block
//! size; carving a fresh, isolated synthesizer here keeps that harness
//! untouched. The cell-diff toggle's *visibility* is proven by the real
//! Chromium `tests/headless_toggle.rs`; cucumber has no browser, so these
//! scenarios assert the `data_diff` payload the toggle is built from.
//!
//! Step partition (cucumber-rs has one global step namespace): REUSE `the
//! exit code is {0}` (defined in `report_generation.rs`). Every other
//! Given/When/Then below uses cell-diff-unique wording so it routes here.

use cucumber::{given, then, when};
use serde_json::Value;

use super::super::common;
use super::World;
use super::world::CellDiffPlan;

use cute_dbt::domain::{
    Checksum, DependsOn, Manifest, ManifestMetadata, Node, NodeConfig, NodeId, UnitTest,
    UnitTestExpect, UnitTestGiven,
};

/// The given input the synthetic unit test mocks (and the YAML declares).
const GIVEN_INPUT: &str = "ref('src')";
/// The declaring YAML file (the changed path the PR diff carries).
const YAML_OFP: &str = "models/_unit_tests.yml";
/// Synthetic compiled SQL with one import CTE named `src` so the given
/// binds to a node (and the model card carries a non-empty CTE DAG).
const COMPILED_WITH_SRC: &str = "with src as (select 1 as id) select id from src";

// ---------------------------------------------------------------------
// Given — build the manifest + record the cell-diff plan
// ---------------------------------------------------------------------

#[given(
    regex = r#"^a unit test "([^"]+)" with a "([^"]+)" given row whose "([^"]+)" is "([^"]+)"$"#
)]
fn unit_test_with_one_row(
    world: &mut World,
    test: String,
    format: String,
    column: String,
    value: String,
) {
    // A single given row with one column carrying `value`. The model is
    // derived from the test name's `test_<model>` convention.
    let row = vec![(column, value)];
    set_plan(world, &test, &format, vec![row]);
}

#[given(regex = r#"^a unit test "([^"]+)" with two "([^"]+)" given rows$"#)]
fn unit_test_with_two_rows(world: &mut World, test: String, format: String) {
    // Two rows so a scenario can remove the second one.
    let rows = vec![
        vec![("id".to_owned(), "1".to_owned())],
        vec![("id".to_owned(), "2".to_owned())],
    ];
    set_plan(world, &test, &format, rows);
}

/// Build the model + unit test from `new_rows` and stash the plan.
fn set_plan(world: &mut World, test: &str, format: &str, new_rows: Vec<Vec<(String, String)>>) {
    let model_bare = model_bare_of(test);
    let manifest = build_manifest(test, &model_bare, format, &new_rows);
    world.current_manifest = Some(manifest);
    world.changed_files = vec![YAML_OFP.to_owned()];
    world.cell_diff_plan = Some(CellDiffPlan {
        test: test.to_owned(),
        format: format.to_owned(),
        new_rows,
    });
}

/// `test_dim_users` → `dim_users` (strip the leading `test_`).
fn model_bare_of(test: &str) -> String {
    test.strip_prefix("test_").unwrap_or(test).to_owned()
}

// ---------------------------------------------------------------------
// When — synthesize the patch + working-tree YAML, run the subprocess
// ---------------------------------------------------------------------

/// How the synthesized hunk edits the test's one given fixture row.
#[derive(Clone, Copy)]
enum Edit {
    /// A genuine value change: the OLD cell differs from the working tree.
    Value,
    /// A format-only reformat: the OLD cell is the SAME value in a different
    /// surface form (`1` ↔ `1.00`) — value-inference converges → NO diff.
    FormatOnly,
    /// A second given row added in the working tree (the OLD block had one).
    AddRow,
    /// The second given row removed in the working tree (OLD had two).
    RemoveRow,
}

#[when(regex = r#"^the PR diff edited that cell's value$"#)]
fn pr_value_edit(world: &mut World) {
    run_with_edit(world, Edit::Value);
}

#[when(regex = r#"^the PR diff only reformatted that cell$"#)]
fn pr_format_only(world: &mut World) {
    run_with_edit(world, Edit::FormatOnly);
}

#[when(regex = r#"^the PR diff added the second row$"#)]
fn pr_add_row(world: &mut World) {
    run_with_edit(world, Edit::AddRow);
}

#[when(regex = r#"^the PR diff removed a second row that existed before$"#)]
fn pr_remove_row(world: &mut World) {
    run_with_edit(world, Edit::RemoveRow);
}

/// Synthesize the working-tree YAML + a `--unified=0` patch for `edit`,
/// then run the real `cute-dbt` subprocess and capture the result.
fn run_with_edit(world: &mut World, edit: Edit) {
    let plan = world
        .cell_diff_plan
        .clone()
        .expect("a Given must set the cell-diff plan before the When runs");
    let manifest = world
        .current_manifest
        .take()
        .expect("a Given built the manifest");

    let workdir = common::tmp("cell_diff_workdir");
    std::fs::create_dir_all(&workdir).expect("create workdir");

    // The working-tree YAML carries the manifest's NEW rows verbatim; write
    // it under <workdir>/<ofp> so the #69 slicer can compute the block span.
    let yaml = working_tree_yaml(&plan);
    let abs = workdir.join(YAML_OFP);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).expect("create YAML parent dir");
    }
    std::fs::write(&abs, &yaml).expect("write working-tree YAML");

    let patch = synthesize_patch(&plan, &yaml, edit);
    let patch_path = common::tmp("cell_diff.patch");
    std::fs::write(&patch_path, &patch).expect("write synthesized patch");

    let manifest_path = super::builders::serialize_to_tmp(&manifest, "cell_diff_current");
    let out = common::tmp("cell_diff_report.html");
    common::clear(&out);
    let scope_arg = format!("@{}", common::s(&patch_path));

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args([
            "--manifest",
            common::s(&manifest_path),
            "--pr-diff",
            &scope_arg,
            "--project-root",
            ".",
            "--out",
            common::s(&out),
        ])
        .current_dir(&workdir)
        .output()
        .expect("the cute-dbt binary spawns");

    world.current_manifest = Some(manifest);
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    // Keep this BDD path under the static zero-egress guard so a regression
    // reintroducing an external asset ref is caught here too (cute-dbt#98).
    if let Some(html) = &world.report_html {
        common::assert_no_external_refs(html);
    }
    world.out_path = Some(out);
}

// ---------------------------------------------------------------------
// YAML + patch synthesis
// ---------------------------------------------------------------------

/// Render one row's value-only inline-flow body for column `col` (the
/// fixtures are single-column for the scenarios that edit a cell value;
/// `id`-only for the add/remove-row scenarios).
fn row_line(row: &[(String, String)]) -> String {
    let body = row
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("          - {{{body}}}")
}

/// The working-tree (NEW) YAML for the plan — a `unit_tests:` block with one
/// test whose given carries `plan.new_rows`, then a trivial empty `expect`.
/// The block body layout is fixed so [`block_line_of_first_row`] can target
/// the first row's source line.
fn working_tree_yaml(plan: &CellDiffPlan) -> String {
    let mut s = String::from("version: 2\n\nunit_tests:\n");
    s.push_str(&format!("  - name: {}\n", plan.test));
    s.push_str(&format!("    model: {}\n", model_bare_of(&plan.test)));
    s.push_str("    given:\n");
    s.push_str(&format!("      - input: {GIVEN_INPUT}\n"));
    s.push_str(&format!("        format: {}\n", plan.format));
    s.push_str("        rows:\n");
    for row in &plan.new_rows {
        s.push_str(&row_line(row));
        s.push('\n');
    }
    s.push_str("    expect:\n");
    s.push_str("      rows: []\n");
    s
}

/// The 1-based source line of the FIRST given row in [`working_tree_yaml`]:
/// 1 version, 2 blank, 3 unit_tests, 4 -name, 5 model, 6 given, 7 -input,
/// 8 format, 9 rows: → 10 is the first `- {…}` row.
const FIRST_ROW_LINE: usize = 10;

/// Synthesize a `git diff --unified=0` patch for `edit` over the working
/// tree `yaml`. Each variant produces ONE hunk whose new side matches the
/// working tree (so the block stays N7b-aligned) and whose removed side is
/// the reconstructed OLD row(s).
fn synthesize_patch(plan: &CellDiffPlan, yaml: &str, edit: Edit) -> String {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut patch = String::new();
    patch.push_str(&format!(
        "diff --git a/{YAML_OFP} b/{YAML_OFP}\n--- a/{YAML_OFP}\n+++ b/{YAML_OFP}\n"
    ));

    match edit {
        Edit::Value => {
            // Rewrite the first row's cell value: OLD differs from working.
            let working = lines[FIRST_ROW_LINE - 1].to_owned();
            let old = old_value_row(&plan.new_rows[0], "alice");
            push_replace(&mut patch, FIRST_ROW_LINE, &[old], &[working]);
        }
        Edit::FormatOnly => {
            // OLD reformats the cell (`1` → `1.00`) — value-inference
            // converges, so NO data diff is emitted.
            let working = lines[FIRST_ROW_LINE - 1].to_owned();
            let old = reformatted_row(&plan.new_rows[0]);
            push_replace(&mut patch, FIRST_ROW_LINE, &[old], &[working]);
        }
        Edit::AddRow => {
            // The manifest + working tree carry a SECOND row the OLD block
            // lacked: a pure addition at the second row's line.
            let second = lines[FIRST_ROW_LINE].to_owned();
            push_replace(&mut patch, FIRST_ROW_LINE + 1, &[], &[second]);
        }
        Edit::RemoveRow => {
            // The OLD block carried a SECOND row the working tree dropped:
            // a pure deletion. The new-side gap sits after the surviving
            // first row (a zero-count point-touch inside the block).
            let removed = row_line(&[("id".to_owned(), "2".to_owned())]);
            push_replace(&mut patch, FIRST_ROW_LINE, &[removed], &[]);
        }
    }
    patch
}

/// Build the OLD inline-flow row for a value edit: the same column as the
/// working row but a DIFFERENT value (`fallback` for a non-numeric column,
/// or a distinct number).
fn old_value_row(working_row: &[(String, String)], fallback: &str) -> String {
    let (col, new_val) = &working_row[0];
    // For a numeric working value, pick a different number; else `fallback`.
    let old_val = if new_val.parse::<f64>().is_ok() {
        "9".to_owned()
    } else {
        fallback.to_owned()
    };
    row_line(&[(col.clone(), old_val)])
}

/// Build the OLD inline-flow row for a format-only reformat: the SAME value
/// in a different surface form (`1` → `1.00`, `true` → `TRUE`). Falls back
/// to a trailing-zero decimal for numerics; quotes a bare string otherwise.
fn reformatted_row(working_row: &[(String, String)]) -> String {
    let (col, new_val) = &working_row[0];
    let reformatted = if new_val.parse::<i64>().is_ok() {
        format!("{new_val}.00")
    } else {
        format!("'{new_val}'")
    };
    row_line(&[(col.clone(), reformatted)])
}

/// Append one `--unified=0` replacement hunk.
fn push_replace(patch: &mut String, new_start: usize, removed: &[String], added: &[String]) {
    patch.push_str(&format!(
        "@@ -{new_start},{} +{new_start},{} @@\n",
        removed.len(),
        added.len(),
    ));
    for r in removed {
        patch.push_str(&format!("-{r}\n"));
    }
    for a in added {
        patch.push_str(&format!("+{a}\n"));
    }
}

// ---------------------------------------------------------------------
// Manifest construction
// ---------------------------------------------------------------------

/// Build a `Manifest` with a model + a unit test whose given carries
/// `new_rows` (the NEW fixture data the manifest ships) at [`YAML_OFP`].
fn build_manifest(
    test: &str,
    model_bare: &str,
    format: &str,
    new_rows: &[Vec<(String, String)>],
) -> Manifest {
    let model_id = format!("model.shop.{model_bare}");
    let node = Node::new(
        NodeId::new(&model_id),
        "model",
        Checksum::new("sha256", "ck-cell"),
        Some(COMPILED_WITH_SRC.to_owned()),
        None,
        DependsOn::default(),
        Some(format!("models/{model_bare}.sql")),
        NodeConfig::default(),
        None,
        std::collections::BTreeMap::new(),
    );
    let rows = rows_to_json(new_rows);
    let unit_test = UnitTest::new(
        test.to_owned(),
        NodeId::new(model_bare),
        vec![UnitTestGiven::new(
            GIVEN_INPUT.to_owned(),
            rows,
            Some(format.to_owned()),
            None,
        )],
        UnitTestExpect::new(Value::Array(Vec::new()), None, None),
        None,
        DependsOn::default(),
        None,
        None,
        Some(YAML_OFP.to_owned()),
    );
    let mut nodes = std::collections::HashMap::new();
    nodes.insert(node.id().clone(), node);
    let mut tests = std::collections::HashMap::new();
    tests.insert(format!("unit_test.shop.{test}"), unit_test);
    Manifest::new(
        ManifestMetadata::new("https://schemas.getdbt.com/dbt/manifest/v12.json"),
        nodes,
        tests,
        std::collections::HashMap::new(),
    )
}

/// Convert `(column, value)` rows into a dict-style `Value::Array` of
/// objects, inferring numbers so the manifest's NEW table matches the
/// working-tree YAML's value-inferred cells. (`format: dict` keeps numbers
/// as JSON numbers; `format: csv` is given as an array of string dicts here,
/// which routes through the same value-inference as the OLD csv side.)
fn rows_to_json(rows: &[Vec<(String, String)>]) -> Value {
    let arr: Vec<Value> = rows
        .iter()
        .map(|row| {
            let map: serde_json::Map<String, Value> = row
                .iter()
                .map(|(k, v)| (k.clone(), infer_json(v)))
                .collect();
            Value::Object(map)
        })
        .collect();
    Value::Array(arr)
}

/// Infer a JSON scalar for a cell string: integer → number, else string.
fn infer_json(v: &str) -> Value {
    if let Ok(i) = v.parse::<i64>() {
        return Value::Number(i.into());
    }
    Value::String(v.to_owned())
}

// ---------------------------------------------------------------------
// Then — payload-based assertions on the test's data_diff
// ---------------------------------------------------------------------

/// Parse the embedded `cute-dbt-data` JSON payload from the rendered report.
fn payload(world: &World) -> Value {
    let html = world
        .report_html
        .as_ref()
        .unwrap_or_else(|| panic!("report.html was not written; stderr={}", world.last_stderr));
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("report HTML must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id("cute-dbt-data")
        .expect("report must include <script id=\"cute-dbt-data\">")
        .get(parser)
        .expect("payload node resolves");
    serde_json::from_str(&node.inner_text(parser)).expect("embedded payload must be valid JSON")
}

/// Find a test object by name in the rendered payload.
fn find_test<'p>(payload: &'p Value, name: &str) -> Option<&'p Value> {
    payload["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["tests"].as_array())
        .flatten()
        .find(|t| t["name"].as_str() == Some(name))
}

fn require_exit_0(world: &World) {
    assert_eq!(
        world.last_exit_code,
        Some(0),
        "cute-dbt failed; stderr={}",
        world.last_stderr,
    );
}

/// The first given table's diff for `test`, panicking with context when
/// the test or its `data_diff` is absent.
fn first_given_diff<'p>(payload: &'p Value, name: &str) -> &'p Value {
    let test = find_test(payload, name)
        .unwrap_or_else(|| panic!("test {name:?} not in payload: {payload}"));
    let given = test["data_diff"]["given"]
        .as_array()
        .unwrap_or_else(|| panic!("test {name:?} should carry a data_diff.given; got {test}"));
    assert!(
        !given.is_empty(),
        "test {name:?} data_diff.given must be non-empty; got {test}",
    );
    &given[0]["diff"]
}

#[then(
    regex = r#"^the test "([^"]+)" carries a data diff with one changed cell from "([^"]+)" to "([^"]+)"$"#
)]
fn data_diff_one_changed_cell(world: &mut World, name: String, old: String, new: String) {
    require_exit_0(world);
    let p = payload(world);
    let diff = first_given_diff(&p, &name);
    let rows = diff["rows"].as_array().expect("diff has rows");
    assert_eq!(rows.len(), 1, "exactly one row in the diff; got {diff}");
    assert_eq!(
        rows[0]["kind"], "modified",
        "the row is Modified; got {diff}"
    );
    let cells = rows[0]["cells"].as_array().expect("row has cells");
    let changed: Vec<&Value> = cells
        .iter()
        .filter(|c| c["changed"] == Value::Bool(true))
        .collect();
    assert_eq!(changed.len(), 1, "exactly one changed cell; got {cells:?}");
    // cute-dbt#138 — each cell side is a `{display, key}` Cell; the value
    // (equality) axis lives under `key`. The authored `display` echoes it for
    // these string values, so assert both for completeness.
    assert_eq!(
        changed[0]["old"]["key"]["v"], old,
        "old key value is {old:?}"
    );
    assert_eq!(
        changed[0]["new"]["key"]["v"], new,
        "new key value is {new:?}"
    );
    assert_eq!(
        changed[0]["old"]["display"], old,
        "old authored display is {old:?}"
    );
    assert_eq!(
        changed[0]["new"]["display"], new,
        "new authored display is {new:?}"
    );
}

#[then(regex = r#"^the test "([^"]+)" carries no data diff$"#)]
fn data_diff_absent(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let test = find_test(&p, &name).unwrap_or_else(|| panic!("test {name:?} not in payload: {p}"));
    // `skip_serializing_if` omits the key entirely when there is no data diff
    // (format-only reformat / unchanged) → the grids fall back to Current.
    assert!(
        test.get("data_diff").is_none_or(Value::is_null),
        "test {name:?} should carry NO data_diff (format-only reformat); got {:?}",
        test.get("data_diff"),
    );
}

#[then(regex = r#"^the test "([^"]+)" carries a data diff with an added row$"#)]
fn data_diff_added_row(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let diff = first_given_diff(&p, &name);
    let kinds = row_kinds(diff);
    assert!(
        kinds.contains(&"added".to_owned()),
        "the data diff should carry an Added row; got kinds {kinds:?}",
    );
}

#[then(regex = r#"^the test "([^"]+)" carries a data diff with a removed row$"#)]
fn data_diff_removed_row(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let diff = first_given_diff(&p, &name);
    let kinds = row_kinds(diff);
    assert!(
        kinds.contains(&"removed".to_owned()),
        "the data diff should carry a Removed row; got kinds {kinds:?}",
    );
}

#[then(regex = r#"^the test "([^"]+)" data diff has exactly one given table with a Modified row$"#)]
fn data_diff_one_given_table_modified(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let test = find_test(&p, &name).unwrap_or_else(|| panic!("test {name:?} not in payload: {p}"));
    let given = test["data_diff"]["given"]
        .as_array()
        .expect("data_diff.given present");
    assert_eq!(
        given.len(),
        1,
        "exactly one given table changed; got {given:?}"
    );
    let kinds = row_kinds(&given[0]["diff"]);
    assert!(
        kinds.contains(&"modified".to_owned()),
        "the given table carries a Modified row; got kinds {kinds:?}",
    );
    // The Diff view is the default whenever a data_diff is present (the JS
    // contract); the payload's mere presence is what drives default-Diff.
}

/// The row-kind tokens of a `FixtureTableDiff` JSON value.
fn row_kinds(diff: &Value) -> Vec<String> {
    diff["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["kind"].as_str().map(str::to_owned))
        .collect()
}
