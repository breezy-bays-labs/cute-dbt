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
use super::builders::{empty_manifest, serialize_to_tmp};

/// The canonical working-tree dbt_project.yml every happy-path scenario
/// shares. Line 5 carries the var; line 10 the marts config; line 13
/// the on-run-start hook; line 17 the dispatch search order
/// (cute-dbt#269) — the patches below address exactly those lines
/// (`concat!` keeps the authored indentation byte-exact — a
/// `\`-continued literal would eat it). Lines 1–10 are pinned: the
/// pre-#269 patches' line numbers depend on them.
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
    "\n",
    "on-run-start:\n",
    "  - \"grant usage on schema reporting to role analyst\"\n",
    "\n",
    "dispatch:\n",
    "  - macro_namespace: dbt_utils\n",
    "    search_order: [\"bdd_project\", \"dbt_utils\"]\n",
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

#[given("the current manifest carries the matching on-run-start operation node")]
fn manifest_carries_operation_node(world: &mut World) {
    // The operation node's raw_code byte-matches canonical line 13's
    // parsed value — the Matched arm (the same-revision contract).
    world.current_manifest = Some(super::builders::manifest_with_operation_node(
        "bdd_project",
        "start",
        0,
        "grant usage on schema reporting to role analyst",
    ));
}

#[given("the PR diff rewrites the on-run-start hook from a revoke statement")]
fn diff_rewrites_hook(world: &mut World) {
    // Line 13 of the canonical file: the `+` side must byte-match it.
    write_patch(
        world,
        &project_patch(
            13,
            "  - \"revoke all on schema reporting from role analyst\"",
            "  - \"grant usage on schema reporting to role analyst\"",
        ),
    );
}

#[given("the PR diff reorders the dispatch search order")]
fn diff_reorders_dispatch(world: &mut World) {
    // Line 17 of the canonical file (the search_order flow list).
    write_patch(
        world,
        &project_patch(
            17,
            "    search_order: [\"dbt_utils\", \"bdd_project\"]",
            "    search_order: [\"bdd_project\", \"dbt_utils\"]",
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

#[then(regex = r#"^that row's note contains "([^"]+)"$"#)]
fn row_note_contains(world: &mut World, fragment: String) {
    // The #269 hook/dispatch notes are full sentences — assert the
    // load-bearing fragment inside a rendered note span. askama
    // HTML-escapes the em dash's surrounding text verbatim, so a plain
    // substring over the document body suffices once we know a note
    // span exists.
    let html = html(world);
    assert!(
        html.contains(r#"<span class="project-def-note">"#),
        "a note span must render",
    );
    assert!(
        html.contains(&fragment),
        "the note must contain {fragment:?}",
    );
}

#[then(regex = r#"^the panel carries a "hooks" row for "([^"]+)" with the hook-diff slot$"#)]
fn panel_carries_hook_row_with_slot(world: &mut World, label: String) {
    let html = html(world);
    assert!(
        html.contains(&format!(
            r#"<code class="project-def-label">{label}</code>"#
        )),
        "panel must carry a hooks row labelled {label}",
    );
    assert!(
        html.contains(&format!(r#"data-hook-slot="{label}""#)),
        "the hooks row must emit the JS-fill diff slot",
    );
}

#[then("the panel carries the dispatch banner row at the UNKNOWN tier")]
fn panel_carries_dispatch_banner(world: &mut World) {
    let html = html(world);
    assert!(
        html.contains(r#"class="project-def-row is-banner" data-category="dispatch""#),
        "the dispatch row must render as a banner",
    );
    assert!(
        html.contains(r#"<span class="tier-chip tier-unknown">UNKNOWN</span>"#),
        "the dispatch banner must carry the UNKNOWN tier chip",
    );
}

#[then(regex = r#"^the payload hooks row is matched and its sql diff adds "([^"]+)"$"#)]
fn payload_hooks_row_matched(world: &mut World, added: String) {
    let payload = payload(world);
    let changes = payload["project_change_panel"]["changes"]
        .as_array()
        .expect("categorized changes ride the payload")
        .clone();
    let hooks = changes
        .iter()
        .find(|c| c["category"] == "hooks")
        .expect("a hooks change in the payload");
    assert_eq!(
        hooks["hook"]["manifest"], "matched",
        "the manifest-side presence verdict rides the wire: {hooks}",
    );
    assert_eq!(
        hooks["hook"]["operation_ids"][0],
        "operation.bdd_project.bdd_project-on-run-start-0",
    );
    let lines = hooks["hook"]["sql_diff"]["lines"]
        .as_array()
        .expect("the inline SQL diff rides the wire");
    assert!(
        lines
            .iter()
            .any(|l| l["kind"] == "added" && l["text"] == added.as_str()),
        "the diff must add {added:?}; got {lines:?}",
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
