//! Step definitions for `features/explore_change_context.feature` —
//! cute-dbt#106 PR-diff change context on the explore verb.
//!
//! Self-contained on the explore plan (the `explore_full_manifest.rs`
//! pattern): the Givens accumulate models (with their
//! `original_file_path`) plus the diff directives, the `When`
//! serializes the synthetic manifest via the SHARED
//! [`serialize_plan_manifest`] assembly, synthesizes a
//! `git diff --unified=0` patch, and runs the real
//! `cute-dbt explore --pr-diff @<patch>` subprocess. The Thens assert
//! the `explore-dag-data` carrier facts:
//!
//! - a changed node serializes `"changed": true`;
//! - an unchanged node serializes NO `changed` key at all (the
//!   byte-stable absent-key contract — the no-context payload is
//!   byte-identical to the pre-#106 shape);
//! - the FULL graph always renders (change context never narrows
//!   scope);
//! - the explorer takes no baseline manifest, ever (the founder
//!   respec) — `--baseline-manifest` is an unknown argument on this
//!   verb.

use std::path::PathBuf;

use cucumber::{given, then, when};
use serde_json::Value;

use super::super::common;
use super::World;
use super::explore_cli::run_explore_with;
use super::explore_full_manifest::{lineage_payload, payload_node, serialize_plan_manifest};

// --- Given ------------------------------------------------------------

#[given(regex = r#"^the explore model "([^"]+)" has source path "([^"]+)"$"#)]
fn model_source_path(world: &mut World, bare: String, path: String) {
    let model = world
        .explore_plan
        .models
        .iter_mut()
        .find(|m| m.bare == bare)
        .unwrap_or_else(|| panic!("model {bare:?} must be declared before it is configured"));
    model.original_file_path = Some(path);
}

#[given(regex = r#"^the PR diff changes the explore file "([^"]+)"$"#)]
fn diff_changes_file(world: &mut World, path: String) {
    world.explore_changed_files.push(path);
}

#[given(regex = r#"^the PR diff purely renames "([^"]+)" to "([^"]+)"$"#)]
fn diff_purely_renames(world: &mut World, from: String, to: String) {
    world.explore_renames.push((from, to));
}

#[given(regex = r#"^the explore project root is "([^"]+)"$"#)]
fn explore_project_root(world: &mut World, root: String) {
    world.explore_project_root = Some(root);
}

#[given("a pr-diff file that is not a unified diff")]
fn malformed_patch(world: &mut World) {
    let path = common::tmp("explore_malformed.patch");
    std::fs::write(&path, "this is not a unified diff\n").expect("write the malformed patch");
    world.explore_explicit_patch = Some(path);
}

// --- When -------------------------------------------------------------

/// Synthesize a `git diff --unified=0` patch from the scenario's
/// changed-file and pure-rename directives. Changed files get the
/// real header block plus one minimal hunk (explore's change context
/// is file-granular — hunk content is irrelevant); pure renames get
/// ONLY the `similarity index 100%` + `rename from`/`rename to`
/// extended headers (no `---`/`+++`, no hunks — the real `git diff`
/// shape the cute-dbt#80 parser collects).
pub fn synthesize_explore_patch(world: &World) -> PathBuf {
    let mut patch = String::new();
    for p in &world.explore_changed_files {
        patch.push_str(&format!(
            "diff --git a/{p} b/{p}\n\
             index 1111111..2222222 100644\n\
             --- a/{p}\n\
             +++ b/{p}\n\
             @@ -1 +1 @@\n\
             -old line\n\
             +new line\n"
        ));
    }
    for (from, to) in &world.explore_renames {
        patch.push_str(&format!(
            "diff --git a/{from} b/{to}\n\
             similarity index 100%\n\
             rename from {from}\n\
             rename to {to}\n"
        ));
    }
    let path = common::tmp("explore_change_context.patch");
    std::fs::write(&path, patch).expect("write the synthesized explore patch");
    path
}

#[when("I run cute-dbt explore on the synthetic manifest with the PR diff")]
fn run_explore_with_diff(world: &mut World) {
    let manifest_path = serialize_plan_manifest(world, "explore_change_context");
    let out_dir = common::tmp("explore_change_context_pages");
    let _ = std::fs::remove_dir_all(&out_dir);
    let patch = world
        .explore_explicit_patch
        .clone()
        .unwrap_or_else(|| synthesize_explore_patch(world));
    let mut extra: Vec<String> = vec!["--pr-diff".to_owned(), format!("@{}", common::s(&patch))];
    // `--project-root` is existence-validated by clap, so the
    // sub-directory scenario runs from a temp workdir carrying the
    // sub-dir (the `pr_diff_scoping` precedent); `--manifest`,
    // `--out-dir` and the `@patch` path are absolute, so cwd does not
    // affect them.
    let workdir = common::tmp("explore_change_context_workdir");
    std::fs::create_dir_all(&workdir).expect("create workdir");
    if let Some(root) = world.explore_project_root.clone() {
        std::fs::create_dir_all(workdir.join(&root)).expect("create project-root dir");
        extra.push("--project-root".to_owned());
        extra.push(root);
    }
    run_explore_with(world, &manifest_path, out_dir, &extra, Some(&workdir));
}

#[when(
    "I run cute-dbt explore with --manifest current.json --out-dir explore/ --baseline-manifest baseline.json"
)]
fn run_explore_with_baseline(world: &mut World) {
    // The founder-respec pin: NO baseline flag exists on explore — clap
    // rejects it as an unknown argument before any file is read (the
    // baseline path deliberately does not exist).
    let manifest = common::fixture("jaffle-shop-current.json");
    let out_dir = common::tmp("explore_baseline_rejected");
    let _ = std::fs::remove_dir_all(&out_dir);
    run_explore_with(
        world,
        &manifest,
        out_dir,
        &["--baseline-manifest".to_owned(), "baseline.json".to_owned()],
        None,
    );
}

#[when("I run cute-dbt explore with --manifest current.json --out-dir explore/ and that pr-diff")]
fn run_explore_with_explicit_patch(world: &mut World) {
    let manifest = common::fixture("jaffle-shop-current.json");
    let patch = world
        .explore_explicit_patch
        .clone()
        .expect("a Given prepared the explicit patch");
    let out_dir = common::tmp("explore_malformed_patch_pages");
    let _ = std::fs::remove_dir_all(&out_dir);
    run_explore_with(
        world,
        &manifest,
        out_dir,
        &["--pr-diff".to_owned(), format!("@{}", common::s(&patch))],
        None,
    );
}

// --- Then -------------------------------------------------------------

#[then(regex = r#"^dag\.html marks "([^"]+)" as changed$"#)]
fn dag_marks_changed(world: &mut World, bare: String) {
    let payload = lineage_payload(world);
    let node = payload_node(&payload, &bare);
    assert_eq!(
        node["changed"],
        Value::Bool(true),
        "the lineage payload must mark {bare:?} changed: {payload}",
    );
}

#[then(regex = r#"^dag\.html does not mark "([^"]+)" as changed$"#)]
fn dag_does_not_mark_changed(world: &mut World, bare: String) {
    let payload = lineage_payload(world);
    let node = payload_node(&payload, &bare);
    assert!(
        node.get("changed").is_none(),
        "an unchanged node carries NO `changed` key at all (the \
         byte-stable absent-key contract), never `false`: {node}",
    );
}

#[then(regex = r#"^the lineage payload carries exactly (\d+) models$"#)]
fn lineage_payload_carries_n_models(world: &mut World, expected: usize) {
    let payload = lineage_payload(world);
    let count = payload["nodes"].as_array().map_or(0, Vec::len);
    assert_eq!(
        count, expected,
        "change context never narrows scope — the full graph renders \
         every model: {payload}",
    );
}

#[then("no lineage node carries a changed mark")]
fn no_node_carries_a_changed_mark(world: &mut World) {
    let payload = lineage_payload(world);
    for node in payload["nodes"].as_array().expect("nodes array") {
        assert!(
            node.get("changed").is_none(),
            "a no-context render must serialize zero `changed` keys \
             (byte-identical to the pre-#106 payload): {node}",
        );
    }
}

#[then("dag.html shows no change-context legend")]
fn dag_shows_no_change_legend(world: &mut World) {
    let dag = world
        .explore_dag_html
        .as_ref()
        .unwrap_or_else(|| panic!("dag.html was not written; stderr={}", world.last_stderr));
    assert!(
        !dag.contains("<span class=\"legend-chip changed\">"),
        "no change-context legend chip renders without --pr-diff",
    );
}

#[then(regex = r#"^dag\.html counts (\d+) changed in this diff$"#)]
fn dag_counts_changed(world: &mut World, expected: usize) {
    let dag = world
        .explore_dag_html
        .as_ref()
        .unwrap_or_else(|| panic!("dag.html was not written; stderr={}", world.last_stderr));
    let clause = format!("{expected} changed in this diff");
    assert!(
        dag.contains(&clause),
        "the header must count the changed models ({clause:?})",
    );
}

#[then(regex = r#"^stderr rejects the unknown argument "([^"]+)"$"#)]
fn stderr_rejects_unknown_argument(world: &mut World, flag: String) {
    assert!(
        world.last_stderr.contains(&flag),
        "stderr must name the rejected argument {flag:?}: {}",
        world.last_stderr,
    );
}

#[then("stderr explains the pr-diff value could not be parsed as a unified diff")]
fn stderr_explains_malformed_diff(world: &mut World) {
    assert!(
        world
            .last_stderr
            .contains("could not be parsed as a unified diff"),
        "stderr must explain the parse failure: {}",
        world.last_stderr,
    );
}
