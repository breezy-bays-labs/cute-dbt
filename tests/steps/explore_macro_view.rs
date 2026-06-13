//! Step definitions for `features/explore_macro_view.feature` —
//! cute-dbt#345 Slice 1, the explorer macro-view walking skeleton.
//!
//! The explore verb gains a THIRD sub-page, `macro.html`, emitted ONLY
//! when the `--pr-diff` changed a root-project macro (the
//! `changed_macros_pr_diff` detection on the explore arm). Slice 1
//! proves the seam end-to-end through the real subprocess:
//!
//! - a diff touching a root-project macro's source path emits
//!   `macro.html` and the third nav anchor on every page;
//! - a diff touching no macro (only a model) emits the two-page output
//!   unchanged — no `macro.html`, no anchor (the byte-identity-golden
//!   shape);
//! - no `--pr-diff` at all is the same negative path.
//!
//! Self-contained on the [`ExplorePlan`](super::world::ExplorePlan)
//! accumulator (the `explore_change_context.rs` pattern): the macro
//! Given splices a root-project macro into the wire manifest (so
//! `macro_id_for_path` resolves), the `When` synthesizes the patch and
//! runs `cute-dbt explore --pr-diff @<patch>`, and the Thens assert the
//! `macro.html` presence/absence and the nav-anchor round-trip. The
//! exit-code Then is the shared step (`report_generation.rs`).

use std::path::PathBuf;

use cucumber::{given, then, when};

use super::super::common;
use super::World;
use super::explore_change_context::synthesize_explore_patch;
use super::explore_cli::run_explore_with;
use super::explore_full_manifest::serialize_plan_manifest;

// --- Given ------------------------------------------------------------

#[given(regex = r#"^the explore manifest carries the root-project macro "([^"]+)" at "([^"]+)"$"#)]
fn manifest_carries_macro(world: &mut World, bare: String, path: String) {
    world.explore_plan.macros.push((bare, path));
}

/// Mark a model as a DIRECT caller of a root-project macro (cute-dbt#345)
/// — splices the macro's full id (`macro.jaffle_shop.<bare>`) into the
/// model's wire `depends_on.macros`. A caller is a `user` in the focus
/// set, so its `ref()`-downstream populates the focused macro DAG.
#[given(regex = r#"^the explore model "([^"]+)" calls the macro "([^"]+)"$"#)]
fn model_calls_macro(world: &mut World, model: String, macro_bare: String) {
    let macro_id = format!("macro.jaffle_shop.{macro_bare}");
    let decl = world
        .explore_plan
        .models
        .iter_mut()
        .find(|m| m.bare == model)
        .unwrap_or_else(|| panic!("model {model:?} must be declared before it calls a macro"));
    decl.depends_macros.push(macro_id);
}

// --- When -------------------------------------------------------------

/// Serialize the macro-bearing plan manifest and run
/// `cute-dbt explore --pr-diff @<patch>` against the synthesized patch
/// (the `explore_change_context` synthesizer, shared so the two
/// subprocess paths cannot drift).
#[when("I run cute-dbt explore on the macro manifest with the PR diff")]
fn run_explore_macro_with_diff(world: &mut World) {
    let manifest_path = serialize_plan_manifest(world, "explore_macro_view");
    let out_dir = fresh_out_dir("explore_macro_view_pages");
    let patch = synthesize_explore_patch(world);
    let extra = vec!["--pr-diff".to_owned(), format!("@{}", common::s(&patch))];
    run_explore_with(world, &manifest_path, out_dir, &extra, None);
}

/// Serialize the macro-bearing plan manifest and run
/// `cute-dbt explore` with NO `--pr-diff` (the no-context negative
/// path).
#[when("I run cute-dbt explore on the macro manifest")]
fn run_explore_macro_no_diff(world: &mut World) {
    let manifest_path = serialize_plan_manifest(world, "explore_macro_view_nodiff");
    let out_dir = fresh_out_dir("explore_macro_view_nodiff_pages");
    run_explore_with(world, &manifest_path, out_dir, &[], None);
}

/// A collision-free explore out-dir under `CARGO_TARGET_TMPDIR`, cleared
/// before each use so a stale page from a previous run can never satisfy
/// a Then (the `explore_cli::fresh_out_dir` shape, re-expressed here so
/// this module stays self-contained).
fn fresh_out_dir(stem: &str) -> PathBuf {
    let dir = common::tmp(stem);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

// --- Then -------------------------------------------------------------
//
// "the exit code is 0" and "the explore out directory contains <file>"
// are shared steps (report_generation.rs / explore_cli.rs) — they read
// the same `world.last_exit_code` / `world.explore_out_dir` the macro
// Whens set, so redefining them here would be an ambiguous-step error.

#[then("no macro.html is written")]
fn no_macro_html_written(world: &mut World) {
    let dir = world.explore_out_dir.as_ref().expect("explore ran");
    assert!(
        !dir.join("macro.html").exists(),
        "the no-macro path must not emit macro.html (out dir: {}); stderr={}",
        dir.display(),
        world.last_stderr,
    );
}

#[then(regex = r#"^(dag|tests)\.html links to the macro-focus page$"#)]
fn page_links_to_macro(world: &mut World, page: String) {
    let html = page_html(world, &page);
    assert!(
        html.contains(r#"<a href="macro.html">Macro focus &rarr;</a>"#),
        "{page}.html must carry the macro-focus nav anchor when macro.html is emitted",
    );
}

#[then(regex = r#"^(dag|tests)\.html does not link to the macro-focus page$"#)]
fn page_does_not_link_to_macro(world: &mut World, page: String) {
    let html = page_html(world, &page);
    assert!(
        !html.contains(r#"href="macro.html""#),
        "{page}.html must carry NO macro-focus anchor on the no-macro path \
         (the byte-identity-golden shape)",
    );
}

#[then("macro.html carries the macro-focus heading")]
fn macro_html_carries_heading(world: &mut World) {
    let dir = world.explore_out_dir.as_ref().expect("explore ran");
    let html = std::fs::read_to_string(dir.join("macro.html"))
        .unwrap_or_else(|_| panic!("macro.html must be readable; stderr={}", world.last_stderr));
    assert!(
        html.contains("<h1>Macro focus</h1>"),
        "macro.html must render the macro-focus page heading",
    );
}

// --- Then: Slice 3 focused-DAG carrier facts --------------------------

#[then(regex = r#"^macro\.html marks the model "([^"]+)" as a macro "(user|downstream)"$"#)]
fn macro_node_role(world: &mut World, bare: String, role: String) {
    let payload = macro_lineage_payload(world);
    let id = format!("model.jaffle_shop.{bare}");
    let node = payload["nodes"]
        .as_array()
        .expect("the macro carrier has a nodes array")
        .iter()
        .find(|n| n["id"] == id)
        .unwrap_or_else(|| panic!("{id} must render in the focused macro DAG: {payload}"));
    assert_eq!(
        node["macro_role"].as_str(),
        Some(role.as_str()),
        "{bare} must carry macro_role={role:?} in the focused carrier: {node}",
    );
}

#[then(regex = r#"^macro\.html does not render the model "([^"]+)"$"#)]
fn macro_node_absent(world: &mut World, bare: String) {
    let payload = macro_lineage_payload(world);
    let id = format!("model.jaffle_shop.{bare}");
    let present = payload["nodes"]
        .as_array()
        .is_some_and(|nodes| nodes.iter().any(|n| n["id"] == id));
    assert!(
        !present,
        "{id} is outside the focus set and must not render: {payload}",
    );
}

#[then(regex = r#"^the focused macro DAG carries exactly (\d+) nodes?$"#)]
fn macro_node_count(world: &mut World, expected: usize) {
    let payload = macro_lineage_payload(world);
    let count = payload["nodes"].as_array().map_or(0, Vec::len);
    assert_eq!(
        count, expected,
        "the focused macro DAG renders exactly the users ∪ downstream set: {payload}",
    );
}

/// Parse the `explore-dag-data` JSON carrier embedded in `macro.html`
/// (the focused [`LineagePayload`]). Self-contained on the `World`'s
/// captured `macro.html` (the subprocess wrote it).
fn macro_lineage_payload(world: &World) -> serde_json::Value {
    let html = world
        .explore_macro_html
        .clone()
        .unwrap_or_else(|| panic!("macro.html was not written; stderr={}", world.last_stderr));
    let dom = tl::parse(&html, tl::ParserOptions::default()).expect("macro.html must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id("explore-dag-data")
        .expect("macro.html must embed <script id=\"explore-dag-data\">")
        .get(parser)
        .expect("dag data node resolves");
    serde_json::from_str(&node.inner_text(parser)).expect("macro dag data must be valid JSON")
}

/// The rendered `dag.html`/`tests.html` captured into the `World`,
/// panicking with stderr context when absent.
fn page_html(world: &World, page: &str) -> String {
    match page {
        "dag" => world.explore_dag_html.clone(),
        "tests" => world.explore_tests_html.clone(),
        other => panic!("unknown explore page {other:?}"),
    }
    .unwrap_or_else(|| panic!("{page}.html was not written; stderr={}", world.last_stderr))
}
