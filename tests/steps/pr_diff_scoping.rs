//! Step definitions for `features/pr_diff_scoping.feature` — the
//! `--scope-from-pr-diff` CI/PR-review path (cute-dbt#84).
//!
//! Each scenario builds a synthetic in-memory `Manifest` via the builders
//! (no committed fixture files — the synthetic-only-fixture invariant is
//! satisfied trivially), serializes it to a temp file, runs the real
//! `cute-dbt` subprocess with `--scope-from-pr-diff`, and asserts against
//! the embedded `cute-dbt-data` JSON payload (the same parse strategy as
//! `consumer_report_contract.rs`).
//!
//! Step partition (cucumber-rs has one global step namespace):
//! - REUSE — `the exit code is {0,non-zero}`, `no file "report.html" is
//!   written` (defined in `report_generation.rs`); `a baseline manifest
//!   "baseline.json"` (no-op in `fail_closed.rs`, harmless for the
//!   clap-conflict scenario).
//! - NEW/RENAME — every Given/When/Then below uses pr-diff-unique wording
//!   so it routes here, not to the baseline-comparison impls.

use cucumber::{given, then, when};
use serde_json::Value;

use super::super::common;
use super::World;
use super::builders::{
    empty_manifest, model_node_with_original_file_path, serialize_to_tmp, unit_test_for,
    unit_test_with_path, with_node, with_unit_test,
};

/// Synthetic compiled SQL with one import CTE, so a rendered model card
/// carries a non-empty CTE DAG (the `contains a CTE diagram for …`
/// assertion).
const COMPILED_WITH_CTE: &str = "with src as (select 1 as id) select id from src";

/// Derive a bare model name from a `.sql` `original_file_path`
/// (`models/marts/core/dim_payers.sql` → `dim_payers`). Non-`.sql` paths
/// fall back to the final path segment.
fn bare_from_ofp(ofp: &str) -> String {
    let file = ofp.rsplit('/').next().unwrap_or(ofp);
    file.strip_suffix(".sql").unwrap_or(file).to_owned()
}

/// Take the current synthetic manifest, seeding an empty (valid v12) one
/// when no model-building Given has run yet (zero-scope scenarios).
fn take_current(world: &mut World) -> cute_dbt::domain::Manifest {
    world.current_manifest.take().unwrap_or_else(empty_manifest)
}

// --- Given: changed-file list ---------------------------------------

#[given(regex = r#"^a list of changed files containing (?:only )?"([^"]+)"$"#)]
fn changed_files_single(world: &mut World, path: String) {
    world.changed_files = vec![path];
}

#[given(regex = r#"^a list of changed files containing (?:only )?"([^"]+)" and "([^"]+)"$"#)]
fn changed_files_two(world: &mut World, a: String, b: String) {
    world.changed_files = vec![a, b];
}

#[given(regex = r#"^the changed-files list is written to a file "([^"]+)" one path per line$"#)]
fn changed_files_written(world: &mut World, name: String) {
    let path = common::tmp(&format!("pr_diff_{name}"));
    std::fs::write(&path, world.changed_files.join("\n")).expect("write changed-files list");
    world.changed_files_path = Some(path);
}

// --- Given: synthetic manifest construction -------------------------

#[given(regex = r#"^the manifest contains a model with original_file_path "([^"]+)"$"#)]
fn manifest_contains_model(world: &mut World, ofp: String) {
    let bare = bare_from_ofp(&ofp);
    let manifest = take_current(world);
    let manifest = with_node(
        manifest,
        model_node_with_original_file_path(&bare, "ck-pr", Some(COMPILED_WITH_CTE), &ofp),
    );
    world.current_manifest = Some(manifest);
    world.last_named_model = Some(bare);
}

#[given(regex = r#"^the manifest contains a model with no compiled SQL at "([^"]+)"$"#)]
fn manifest_contains_uncompiled_model(world: &mut World, ofp: String) {
    // compiled_code: null — Stage-2 fail-closes when this model is the
    // target of an in-scope unit test.
    let bare = bare_from_ofp(&ofp);
    let manifest = take_current(world);
    let manifest = with_node(
        manifest,
        model_node_with_original_file_path(&bare, "ck-pr", None, &ofp),
    );
    world.current_manifest = Some(manifest);
    world.last_named_model = Some(bare);
}

#[given(
    regex = r#"^the manifest \(compiled with project root "([^"]+)"\) contains a model with original_file_path "([^"]+)"$"#
)]
fn manifest_subdir_model(world: &mut World, _project_root: String, ofp: String) {
    // The manifest's original_file_path is project-relative; the changed
    // path is repo-relative (carries the project-root prefix). The
    // `--project-root` flag in the When step bridges the two. The
    // parenthetical project-root note is descriptive only here.
    let bare = bare_from_ofp(&ofp);
    let manifest = take_current(world);
    let manifest = with_node(
        manifest,
        model_node_with_original_file_path(&bare, "ck-pr", Some(COMPILED_WITH_CTE), &ofp),
    );
    world.current_manifest = Some(manifest);
    world.last_named_model = Some(bare);
}

#[given(regex = r#"^the model "([^"]+)" has a unit test "([^"]+)"$"#)]
fn model_has_unit_test(world: &mut World, model_bare: String, test: String) {
    let manifest = take_current(world);
    let manifest = with_unit_test(manifest, unit_test_for(&test, &model_bare));
    world.current_manifest = Some(manifest);
}

#[given(regex = r#"^the model "([^"]+)" has a unit test "([^"]+)" declared in "([^"]+)"$"#)]
fn model_has_unit_test_declared_in(
    world: &mut World,
    model_bare: String,
    test: String,
    ofp: String,
) {
    let manifest = take_current(world);
    let manifest = with_unit_test(manifest, unit_test_with_path(&test, &model_bare, &ofp));
    world.current_manifest = Some(manifest);
}

#[given(
    regex = r#"^the manifest also contains an unchanged model "([^"]+)" with a unit test "([^"]+)"$"#
)]
fn manifest_unchanged_model_with_test(world: &mut World, model_bare: String, test: String) {
    // "Unchanged" = its on-disk path is NOT in the changed-files set, so
    // it stays out of scope. A plausible staging path keeps it distinct
    // from the changed marts models.
    let ofp = format!("models/staging/{model_bare}.sql");
    let manifest = take_current(world);
    let manifest = with_node(
        manifest,
        model_node_with_original_file_path(
            &model_bare,
            "ck-unchanged",
            Some(COMPILED_WITH_CTE),
            &ofp,
        ),
    );
    let manifest = with_unit_test(manifest, unit_test_for(&test, &model_bare));
    world.current_manifest = Some(manifest);
}

#[given(regex = r#"^the manifest has no node with original_file_path "([^"]+)"$"#)]
fn manifest_has_no_node(world: &mut World, _ofp: String) {
    // Intentionally builds nothing — the changed path maps to no node.
    // Ensure a valid (possibly empty) manifest exists for the When step.
    let manifest = take_current(world);
    world.current_manifest = Some(manifest);
}

// --- When: run the subprocess ---------------------------------------

/// Resolve the `--scope-from-pr-diff` argument from the harness
/// placeholder: `<changed-files>` → a literal comma-joined list;
/// `@changed.txt` → the `@`-prefixed path of the file a prior Given wrote.
fn resolve_scope_arg(world: &World, token: &str) -> String {
    if token.starts_with('@') {
        let path = world
            .changed_files_path
            .as_ref()
            .expect("the @file scenario wrote a changed-files list");
        format!("@{}", common::s(path))
    } else {
        world.changed_files.join(",")
    }
}

fn capture(world: &mut World, output: std::process::Output, out: std::path::PathBuf) {
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    world.out_path = Some(out);
}

#[when(
    regex = r#"^I run cute-dbt with --manifest current\.json --scope-from-pr-diff (\S+) --project-root (\S+) --out report\.html$"#
)]
fn run_scope_from_pr_diff(world: &mut World, scope_token: String, project_root: String) {
    let manifest = take_current(world);
    let manifest_path = serialize_to_tmp(&manifest, "pr_diff_current");
    world.current_manifest = Some(manifest);

    let scope_arg = resolve_scope_arg(world, &scope_token);
    let out = common::tmp("pr_diff_report.html");
    common::clear(&out);

    // `--project-root` is existence-validated by clap (cute-dbt#69), so the
    // sub-directory project-root scenario needs the directory to exist on
    // disk. Run from a temp workdir and create the project-root sub-dir
    // there; `--manifest`/`--out` are absolute, so cwd doesn't affect them.
    // Create `workdir` itself first; only join the sub-dir for a non-"."
    // root — `workdir.join(".")` builds a `…/workdir/.` path whose trailing
    // `.` component makes `create_dir_all` fail with NotFound on Linux
    // (macOS tolerates it), which is why this passed locally but not in CI.
    let workdir = common::tmp("pr_diff_workdir");
    std::fs::create_dir_all(&workdir).expect("create workdir");
    if project_root != "." {
        std::fs::create_dir_all(workdir.join(&project_root)).expect("create project-root dir");
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args([
            "--manifest",
            common::s(&manifest_path),
            "--scope-from-pr-diff",
            &scope_arg,
            "--project-root",
            &project_root,
            "--out",
            common::s(&out),
        ])
        .current_dir(&workdir)
        .output()
        .expect("the cute-dbt binary spawns");
    capture(world, output, out);
}

#[when(
    regex = r#"^I run cute-dbt with --manifest current\.json --baseline-manifest baseline\.json --scope-from-pr-diff <changed-files> --project-root \. --out report\.html$"#
)]
fn run_both_scope_sources(world: &mut World) {
    // clap rejects the conflicting scope sources at parse time, before
    // either manifest is read — the baseline path need not exist.
    let manifest = take_current(world);
    let manifest_path = serialize_to_tmp(&manifest, "pr_diff_both");
    world.current_manifest = Some(manifest);

    let baseline = common::tmp("pr_diff_baseline_unread.json");
    let out = common::tmp("pr_diff_both_report.html");
    common::clear(&out);
    let output = common::run_cli(&[
        "--manifest",
        common::s(&manifest_path),
        "--baseline-manifest",
        common::s(&baseline),
        "--scope-from-pr-diff",
        &world.changed_files.join(","),
        "--project-root",
        ".",
        "--out",
        common::s(&out),
    ]);
    capture(world, output, out);
}

#[when(
    regex = r#"^I run cute-dbt with --manifest current\.json --project-root \. --out report\.html$"#
)]
fn run_no_scope_source(world: &mut World) {
    // Neither scope source — clap's required `scope_source` group fails.
    let manifest = take_current(world);
    let manifest_path = serialize_to_tmp(&manifest, "pr_diff_neither");
    world.current_manifest = Some(manifest);

    let out = common::tmp("pr_diff_neither_report.html");
    common::clear(&out);
    let output = common::run_cli(&[
        "--manifest",
        common::s(&manifest_path),
        "--project-root",
        ".",
        "--out",
        common::s(&out),
    ]);
    capture(world, output, out);
}

// --- Then: payload-based assertions ---------------------------------

/// Parse the embedded `cute-dbt-data` JSON payload from the rendered
/// report (mirrors `consumer_report_contract.rs`).
fn payload(world: &World) -> Value {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("report HTML must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id("cute-dbt-data")
        .expect("report must include <script id=\"cute-dbt-data\">")
        .get(parser)
        .expect("payload node resolves");
    serde_json::from_str(&node.inner_text(parser)).expect("embedded payload must be valid JSON")
}

/// Bare model names present in the rendered payload (`models[].name`).
fn model_names(payload: &Value) -> Vec<String> {
    payload["models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Every unit-test name in the rendered payload (`models[].tests[].name`).
fn test_names(payload: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(models) = payload["models"].as_array() {
        for model in models {
            if let Some(tests) = model["tests"].as_array() {
                for test in tests {
                    if let Some(name) = test["name"].as_str() {
                        out.push(name.to_owned());
                    }
                }
            }
        }
    }
    out
}

fn require_exit_0(world: &World) {
    assert_eq!(
        world.last_exit_code,
        Some(0),
        "cute-dbt failed; stderr={}",
        world.last_stderr,
    );
}

#[then(regex = r#"^the rendered report's models-in-scope listing contains "([^"]+)"$"#)]
fn models_listing_contains(world: &mut World, name: String) {
    require_exit_0(world);
    let names = model_names(&payload(world));
    assert!(
        names.contains(&name),
        "expected model {name:?} in scope; got {names:?}",
    );
}

#[then(
    regex = r#"^the rendered report's models-in-scope listing contains both "([^"]+)" and "([^"]+)"$"#
)]
fn models_listing_contains_both(world: &mut World, a: String, b: String) {
    require_exit_0(world);
    let names = model_names(&payload(world));
    assert!(
        names.contains(&a) && names.contains(&b),
        "expected both {a:?} and {b:?} in scope; got {names:?}",
    );
}

#[then(regex = r#"^the rendered report's models-in-scope listing does NOT contain "([^"]+)"$"#)]
fn models_listing_excludes(world: &mut World, name: String) {
    require_exit_0(world);
    let names = model_names(&payload(world));
    assert!(
        !names.contains(&name),
        "{name:?} should be out of scope; got {names:?}",
    );
}

#[then(regex = r#"^the rendered report contains a CTE diagram for "([^"]+)"$"#)]
fn cte_diagram_for(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let model = p["models"]
        .as_array()
        .and_then(|models| {
            models
                .iter()
                .find(|m| m["name"].as_str() == Some(name.as_str()))
        })
        .unwrap_or_else(|| panic!("model {name:?} not in payload; got {:?}", model_names(&p)));
    let dag_nodes = model["dag"]["nodes"].as_array().map_or(0, Vec::len);
    assert!(
        dag_nodes > 0,
        "expected a non-empty CTE DAG for {name:?}; got {dag_nodes} nodes",
    );
}

#[then(regex = r#"^the rendered report's test rows include "([^"]+)"$"#)]
fn test_rows_include(world: &mut World, test: String) {
    require_exit_0(world);
    let names = test_names(&payload(world));
    assert!(
        names.contains(&test),
        "expected test {test:?} in the rendered report; got {names:?}",
    );
}

#[then(regex = r#"^the rendered report does NOT contain a test row for "([^"]+)"$"#)]
fn no_test_row_for(world: &mut World, test: String) {
    require_exit_0(world);
    let names = test_names(&payload(world));
    assert!(
        !names.contains(&test),
        "{test:?} should not render; got {names:?}",
    );
}

#[then(
    regex = r#"^the rendered report shows "([^"]+)" with the "no unit tests wired" empty state$"#
)]
fn model_empty_state(world: &mut World, name: String) {
    require_exit_0(world);
    let p = payload(world);
    let model = p["models"]
        .as_array()
        .and_then(|models| {
            models
                .iter()
                .find(|m| m["name"].as_str() == Some(name.as_str()))
        })
        .unwrap_or_else(|| panic!("model {name:?} not in payload; got {:?}", model_names(&p)));
    let tests = model["tests"].as_array().map_or(0, Vec::len);
    assert_eq!(
        tests, 0,
        "expected {name:?} to show the no-unit-tests-wired empty state (zero in-scope tests)",
    );
}

#[then(regex = r#"^the rendered report shows the "0 unit tests in scope" banner$"#)]
fn zero_scope_banner(world: &mut World) {
    require_exit_0(world);
    let names = model_names(&payload(world));
    assert!(
        names.is_empty(),
        "expected zero models in scope; got {names:?}",
    );
}

#[then("the rendered report contains no CTE diagrams")]
fn no_cte_diagrams(world: &mut World) {
    require_exit_0(world);
    let names = model_names(&payload(world));
    assert!(
        names.is_empty(),
        "expected no models (hence no CTE diagrams); got {names:?}",
    );
}

#[then(
    "stderr explains exactly one of --scope-from-pr-diff or --baseline-manifest must be provided"
)]
fn stderr_explains_one_scope_source(world: &mut World) {
    let stderr = &world.last_stderr;
    assert!(
        stderr.contains("--scope-from-pr-diff") && stderr.contains("--baseline-manifest"),
        "expected stderr to name both scope-source flags; got: {stderr}",
    );
}

#[then(
    regex = r#"^stderr names "([^"]+)" as the offending node and recommends running "([^"]+)"$"#
)]
fn stderr_names_offending_node(world: &mut World, node: String, command: String) {
    let stderr = &world.last_stderr;
    assert!(
        stderr.contains(&node),
        "expected stderr to name the offending node {node:?}; got: {stderr}",
    );
    assert!(
        stderr.contains(&command),
        "expected stderr to recommend {command:?}; got: {stderr}",
    );
}
