//! Step definitions for `features/project_definition.feature`
//! (cute-dbt#266 — dbt_project.yml ingestion + the categorized
//! project-change panel).
//!
//! Subprocess wire round-trip (the BDD house style): each scenario
//! writes a real working-tree dbt_project.yml (or deliberately omits
//! it) plus a hand-shaped `--unified=0` patch into a temp workdir, runs
//! the actual `cute-dbt` binary with `--pr-diff @project.patch
//! --project-root .` (or the baseline arm), and asserts the rendered
//! panel markup + the embedded `cute-dbt-data` payload facts. The
//! canonical working-tree file is a fixed 10-line document so the
//! patches' line numbers and `+` bodies are byte-aligned by
//! construction (the same-revision contract the drift guard enforces).

use cucumber::{given, then, when};
use serde_json::Value;

use super::super::common;
use super::World;
use super::builders::{
    empty_manifest, model_node_with_fqn, serialize_to_tmp, unit_test_for, with_node, with_unit_test,
};

/// The canonical working-tree dbt_project.yml every happy-path scenario
/// shares. Line 5 carries the var; line 10 carries the marts config;
/// line 11 carries the project-level config the cute-dbt#267
/// deepest-match scenario edits — the patches below address exactly
/// those lines (`concat!` keeps the authored indentation byte-exact — a
/// `\`-continued literal would eat it).
const PROJECT_YML: &str = concat!(
    "name: bdd_project\n",
    "version: \"1.0\"\n",
    "\n",
    "vars:\n",
    "  dq_threshold: 5\n",
    "\n",
    "models:\n",
    "  bdd_project:\n",
    "    marts:\n",
    "      +materialized: table\n",
    "    +materialized: view\n",
);

/// A one-hunk `--unified=0` patch against dbt_project.yml.
fn project_patch(line: usize, removed: &str, added: &str) -> String {
    format!(
        "diff --git a/dbt_project.yml b/dbt_project.yml\n\
         --- a/dbt_project.yml\n\
         +++ b/dbt_project.yml\n\
         @@ -{line} +{line} @@\n\
         -{removed}\n\
         +{added}\n"
    )
}

// ---------------------------------------------------------------------
// Given
// ---------------------------------------------------------------------

#[given("an empty current manifest")]
fn empty_current_manifest(world: &mut World) {
    world.current_manifest = Some(empty_manifest());
}

#[given("the working tree carries the canonical dbt_project.yml")]
fn working_tree_has_project_yml(world: &mut World) {
    let workdir = project_workdir(world);
    std::fs::write(workdir.join("dbt_project.yml"), PROJECT_YML)
        .expect("write working-tree dbt_project.yml");
}

#[given("the working tree has no dbt_project.yml")]
fn working_tree_has_no_project_yml(world: &mut World) {
    let workdir = project_workdir(world);
    let _ = std::fs::remove_file(workdir.join("dbt_project.yml"));
}

#[given(regex = r#"^the PR diff edits the project var "([^"]+)" from (\d+) to (\d+)$"#)]
fn diff_edits_var(world: &mut World, var: String, old: String, new: String) {
    // Line 5 of the canonical file: the `+` side must byte-match it.
    write_patch(
        world,
        &project_patch(5, &format!("  {var}: {old}"), &format!("  {var}: {new}")),
    );
}

#[given(
    regex = r#"^the PR diff edits the marts folder materialization from "([^"]+)" to "([^"]+)"$"#
)]
fn diff_edits_marts_config(world: &mut World, old: String, new: String) {
    // Line 10 of the canonical file (the `+materialized` leaf).
    write_patch(
        world,
        &project_patch(
            10,
            &format!("      +materialized: {old}"),
            &format!("      +materialized: {new}"),
        ),
    );
}

#[given(
    regex = r#"^the current manifest carries a marts model "([^"]+)" and a staging model "([^"]+)" with fqns$"#
)]
fn manifest_with_fqn_models(world: &mut World, marts_bare: String, staging_bare: String) {
    // fqn first segments match the canonical file's project name
    // (`bdd_project`) — the config-tree prefix-matcher input. The marts
    // model carries one unit test so the widening can be asserted to
    // pull it in as CONTEXT (compiled code keeps Stage-2 green).
    let mut manifest = empty_manifest();
    manifest = with_node(
        manifest,
        model_node_with_fqn(
            &marts_bare,
            "ck-marts",
            Some("select 1"),
            &["bdd_project", "marts", &marts_bare],
        ),
    );
    manifest = with_node(
        manifest,
        model_node_with_fqn(
            &staging_bare,
            "ck-staging",
            Some("select 1"),
            &["bdd_project", "staging", &staging_bare],
        ),
    );
    manifest = with_unit_test(
        manifest,
        unit_test_for(&format!("test_{marts_bare}_rows"), &marts_bare),
    );
    world.current_manifest = Some(manifest);
}

#[given(
    regex = r#"^the PR diff edits the project-level materialization from "([^"]+)" to "([^"]+)"$"#
)]
fn diff_edits_project_level_config(world: &mut World, old: String, new: String) {
    // Line 11 of the canonical file (the bdd_project-level
    // `+materialized` leaf the marts subtree shadows).
    write_patch(
        world,
        &project_patch(
            11,
            &format!("    +materialized: {old}"),
            &format!("    +materialized: {new}"),
        ),
    );
}

#[given("the PR diff claims a dbt_project.yml line that does not match the working tree")]
fn diff_is_stale(world: &mut World) {
    // The `+` body disagrees with working-tree line 5 — the reverse-apply
    // drift guard must refuse and the panel must fall back, never
    // fabricate an old side.
    write_patch(
        world,
        &project_patch(5, "  dq_threshold: 10", "  dq_threshold: 999"),
    );
}

// ---------------------------------------------------------------------
// When — run the real binary
// ---------------------------------------------------------------------

#[when(
    regex = r#"^I run cute-dbt report with --manifest current\.json --pr-diff @project\.patch --project-root \. --out report\.html$"#
)]
fn run_pr_diff_arm(world: &mut World) {
    let manifest = world
        .current_manifest
        .take()
        .expect("the Background built a manifest");
    let manifest_path = serialize_to_tmp(&manifest, "project_def_current");
    world.current_manifest = Some(manifest);

    let workdir = project_workdir(world);
    let patch_path = world
        .explicit_patch
        .clone()
        .expect("a Given wrote project.patch");
    let out = common::tmp("project_def_report.html");
    common::clear(&out);

    let scope_arg = format!("@{}", common::s(&patch_path));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args([
            "report",
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
    capture(world, output, out);
}

#[when(
    regex = r#"^I run cute-dbt report with --manifest current\.json --baseline-manifest baseline\.json --project-root \. --out report\.html$"#
)]
fn run_baseline_arm(world: &mut World) {
    let manifest = world
        .current_manifest
        .take()
        .expect("the Background built a manifest");
    let manifest_path = serialize_to_tmp(&manifest, "project_def_current");
    // Identical baseline — scope is empty; only the standing-metadata
    // posture is under test here.
    let baseline_path = serialize_to_tmp(&manifest, "project_def_baseline");
    world.current_manifest = Some(manifest);

    let workdir = project_workdir(world);
    let out = common::tmp("project_def_report.html");
    common::clear(&out);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args([
            "report",
            "--manifest",
            common::s(&manifest_path),
            "--baseline-manifest",
            common::s(&baseline_path),
            "--project-root",
            ".",
            "--out",
            common::s(&out),
        ])
        .current_dir(&workdir)
        .output()
        .expect("the cute-dbt binary spawns");
    capture(world, output, out);
}

// ---------------------------------------------------------------------
// Then
// ---------------------------------------------------------------------

#[then("the report carries the project-definition panel")]
fn report_carries_panel(world: &mut World) {
    assert!(
        html(world).contains(r#"data-testid="project-def-panel""#),
        "the project-definition panel must render; stderr={}",
        world.last_stderr,
    );
}

#[then("the report carries no project-definition panel")]
fn report_carries_no_panel(world: &mut World) {
    assert!(
        !html(world).contains(r#"data-testid="project-def-panel""#),
        "no panel may render outside the pr-diff-with-dbt_project.yml case",
    );
}

#[then(regex = r#"^the panel carries a "([^"]+)" row for "([^"]+)" showing "([^"]+)"$"#)]
fn panel_carries_row(world: &mut World, chip: String, label: String, detail: String) {
    let html = html(world);
    assert!(
        html.contains(&format!(
            r#"<code class="project-def-label">{label}</code>"#
        )),
        "panel must carry a row labelled {label}",
    );
    assert!(
        html.contains(&detail),
        "the {label} row must show {detail:?}",
    );
    assert!(
        html.contains(&format!(">{chip}</span>")),
        "the row must carry the {chip:?} category chip",
    );
}

#[then(
    regex = r#"^the panel carries a "([^"]+)" row for "([^"]+)" showing "([^"]+)" then "([^"]+)"$"#
)]
fn panel_carries_quoted_row(
    world: &mut World,
    chip: String,
    label: String,
    old: String,
    new: String,
) {
    // JSON-compact values render with quotes; askama HTML-escapes them
    // to `&#34;` — assert the escaped form the browser displays as
    // `"view" → "table"`.
    let detail = format!("&#34;{old}&#34; \u{2192} &#34;{new}&#34;");
    panel_carries_row(world, chip, label, detail);
}

#[then(regex = r#"^that row states "([^"]+)"$"#)]
fn row_states(world: &mut World, note: String) {
    assert!(
        html(world).contains(&format!(r#"<span class="project-def-note">{note}</span>"#)),
        "the row's honesty note must render: {note:?}",
    );
}

#[then(regex = r#"^the panel shows the raw-diff fallback stating "([^"]+)"$"#)]
fn panel_shows_fallback(world: &mut World, copy: String) {
    let html = html(world);
    assert!(
        html.contains(r#"data-testid="project-def-fallback""#),
        "the Shape-A fallback row must render; stderr={}",
        world.last_stderr,
    );
    assert!(
        html.contains(&copy),
        "the fallback copy must state {copy:?}",
    );
    assert!(
        html.contains("project-def-raw-line"),
        "the raw diff lines must render",
    );
}

#[then("the payload carries the parsed project definition")]
fn payload_carries_definition(world: &mut World) {
    let payload = payload(world);
    let def = &payload["project_definition"];
    assert_eq!(
        def["name"].as_str(),
        Some("bdd_project"),
        "standing metadata must carry the parsed name; got {def}",
    );
    assert_eq!(
        def["vars"]["dq_threshold"],
        Value::from(5),
        "standing metadata must carry the parsed vars",
    );
}

// ---------------------------------------------------------------------
// Then — cute-dbt#267 config-tree widening + provenance chips
// ---------------------------------------------------------------------

/// The payload's model entry by bare name, or `None`.
fn payload_model(payload: &Value, name: &str) -> Option<Value> {
    payload["models"]
        .as_array()
        .expect("payload carries a models array")
        .iter()
        .find(|m| m["name"].as_str() == Some(name))
        .cloned()
}

#[then(regex = r#"^the payload carries the model "([^"]+)" in scope$"#)]
fn payload_carries_model(world: &mut World, name: String) {
    let payload = payload(world);
    assert!(
        payload_model(&payload, &name).is_some(),
        "model {name:?} must be in the rendered scope; stderr={}",
        world.last_stderr,
    );
}

#[then(regex = r#"^the payload carries no model "([^"]+)"$"#)]
fn payload_carries_no_model(world: &mut World, name: String) {
    let payload = payload(world);
    assert!(
        payload_model(&payload, &name).is_none(),
        "model {name:?} must NOT be widened into scope",
    );
}

#[then(
    regex = r#"^the payload model "([^"]+)" carries the config attribution "([^"]+)" via "([^"]+)"$"#
)]
fn payload_model_carries_attribution(world: &mut World, name: String, key: String, path: String) {
    let payload = payload(world);
    let model = payload_model(&payload, &name).expect("the model is in scope");
    let attributions = model["config_attributions"]
        .as_array()
        .unwrap_or_else(|| panic!("model {name:?} carries config_attributions; got {model}"));
    assert!(
        attributions.iter().any(|a| {
            a["key"].as_str() == Some(key.as_str()) && a["path"].as_str() == Some(path.as_str())
        }),
        "model {name:?} must carry the +{key} chip via {path}; got {attributions:?}",
    );
}

#[then(regex = r#"^the payload carries the unit test "([^"]+)" as context, not changed$"#)]
fn payload_carries_context_test(world: &mut World, test_name: String) {
    let payload = payload(world);
    let test = payload["models"]
        .as_array()
        .expect("models array")
        .iter()
        .flat_map(|m| m["tests"].as_array().cloned().unwrap_or_default())
        .find(|t| t["name"].as_str() == Some(test_name.as_str()))
        .unwrap_or_else(|| panic!("unit test {test_name:?} must ride into scope"));
    assert_eq!(
        test["changed"],
        Value::Bool(false),
        "a config-widened test is context — its definition was not updated",
    );
}

#[then(regex = r#"^the panel's config-tree row states "([^"]+)"$"#)]
fn panel_config_tree_row_states(world: &mut World, sentence: String) {
    let html = html(world);
    assert!(
        html.contains(r#"data-testid="project-def-affected""#),
        "the config-tree row must carry the affected-models listing",
    );
    assert!(
        html.contains(&sentence),
        "the affected listing must state {sentence:?}",
    );
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

/// The scenario's temp workdir (created on first use) — the
/// `--project-root .` the subprocess runs from. Scenarios share the
/// path (the bdd harness is single-threaded), so each Given states the
/// file's presence explicitly — `working_tree_has_no_project_yml`
/// removes a previous scenario's file rather than assuming a clean dir.
fn project_workdir(_world: &mut World) -> std::path::PathBuf {
    let workdir = common::tmp("project_def_workdir");
    std::fs::create_dir_all(&workdir).expect("create workdir");
    workdir
}

fn write_patch(world: &mut World, patch: &str) {
    let path = common::tmp("project_def.patch");
    std::fs::write(&path, patch).expect("write project.patch");
    world.explicit_patch = Some(path);
}

fn capture(world: &mut World, output: std::process::Output, out: std::path::PathBuf) {
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    // Keep this BDD path under the static zero-egress guard (house rule).
    if let Some(html) = &world.report_html {
        common::assert_no_external_refs(html);
    }
    world.out_path = Some(out);
}

fn html(world: &World) -> &str {
    world
        .report_html
        .as_deref()
        .unwrap_or_else(|| panic!("report.html was not written; stderr={}", world.last_stderr))
}

/// Parse the embedded `cute-dbt-data` JSON payload.
fn payload(world: &World) -> Value {
    let html = html(world);
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("report HTML parses");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id("cute-dbt-data")
        .expect("report carries <script id=\"cute-dbt-data\">")
        .get(parser)
        .expect("payload node resolves");
    serde_json::from_str(&node.inner_text(parser)).expect("embedded payload is valid JSON")
}
