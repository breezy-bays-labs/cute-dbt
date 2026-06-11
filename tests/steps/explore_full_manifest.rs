//! Step definitions for `features/explore_full_manifest.feature` —
//! cute-dbt#100 full-manifest scoping (the `all_models` seam) and the
//! Stage-2 fail-OPEN contract.
//!
//! Self-contained (the `incremental_models.rs` pattern): the Givens
//! accumulate an [`ExplorePlan`](super::world::ExplorePlan), the `When`
//! serializes a synthetic manifest and runs the real `cute-dbt explore`
//! subprocess, and the Thens assert rendered-page facts (tests.html
//! sections / badges, the dag.html lineage definition) plus the
//! embedded `cute-dbt-data` payload (the reused `build_payload`
//! output). The model-count oracle is concrete: rendered sections ==
//! declared manifest models.

use cucumber::{given, then, when};
use serde_json::Value;

use super::World;
use super::builders::{
    empty_manifest, model_node_with_deps, serialize_to_tmp, unit_test_for, with_node,
    with_unit_test,
};
use super::explore_cli::run_explore;
use super::world::ExploreModelDecl;

// --- Background -------------------------------------------------------

#[given("an explore scenario")]
fn given_scenario(world: &mut World) {
    world.explore_plan = Default::default();
}

// --- Given --------------------------------------------------------------

#[given(regex = r#"^the explore manifest declares the model "([^"]+)"$"#)]
fn declares_model(world: &mut World, bare: String) {
    world.explore_plan.models.push(ExploreModelDecl {
        bare,
        compiled: true,
        deps: Vec::new(),
        tests: Vec::new(),
    });
}

#[given(regex = r#"^the explore model "([^"]+)" has no compiled SQL$"#)]
fn model_uncompiled(world: &mut World, bare: String) {
    model_mut(world, &bare).compiled = false;
}

#[given(regex = r#"^the explore model "([^"]+)" depends on "([^"]+)"$"#)]
fn model_depends_on(world: &mut World, bare: String, dep: String) {
    model_mut(world, &bare).deps.push(dep);
}

#[given(regex = r#"^the explore model "([^"]+)" declares unit test "([^"]+)"$"#)]
fn model_declares_test(world: &mut World, bare: String, test: String) {
    model_mut(world, &bare).tests.push(test);
}

/// Borrow the previously-declared model, panicking with context if a
/// configuration step references an undeclared model.
fn model_mut<'w>(world: &'w mut World, bare: &str) -> &'w mut ExploreModelDecl {
    world
        .explore_plan
        .models
        .iter_mut()
        .find(|m| m.bare == bare)
        .unwrap_or_else(|| panic!("model {bare:?} must be declared before it is configured"))
}

// --- When ---------------------------------------------------------------

#[when("I run cute-dbt explore on the synthetic manifest")]
fn run_explore_on_synthetic(world: &mut World) {
    let plan = world.explore_plan.clone();
    let mut manifest = empty_manifest();
    for m in &plan.models {
        let deps: Vec<&str> = m.deps.iter().map(String::as_str).collect();
        let compiled = m.compiled.then_some("select 1");
        manifest = with_node(
            manifest,
            model_node_with_deps(&m.bare, "ck", compiled, &deps),
        );
        for test in &m.tests {
            manifest = with_unit_test(manifest, unit_test_for(test, &m.bare));
        }
    }
    let manifest_path = serialize_to_tmp(&manifest, "explore_full_manifest");
    let out_dir = super::super::common::tmp("explore_full_manifest_pages");
    let _ = std::fs::remove_dir_all(&out_dir);
    run_explore(world, &manifest_path, out_dir);
}

// --- Then ----------------------------------------------------------------

#[then(regex = r#"^tests\.html renders exactly (\d+) model sections?$"#)]
fn tests_html_renders_n_sections(world: &mut World, expected: usize) {
    let html = tests_html(world);
    let count = html.matches("class=\"explore-model\"").count();
    assert_eq!(
        count, expected,
        "tests.html must render one section per manifest model",
    );
}

#[then(regex = r#"^the embedded explore payload carries exactly (\d+) models$"#)]
fn payload_carries_n_models(world: &mut World, expected: usize) {
    let payload = explore_payload(world);
    let count = payload["models"].as_array().map_or(0, Vec::len);
    assert_eq!(
        count, expected,
        "the embedded build_payload output must carry every manifest model: {payload}",
    );
}

#[then(regex = r#"^dag\.html marks "([^"]+)" as not compiled$"#)]
fn dag_marks_not_compiled(world: &mut World, bare: String) {
    let def = lineage_def(world);
    assert!(
        def.contains(&format!("\"{bare} (not compiled)\"]:::notcompiled")),
        "the lineage def must mark {bare:?} not-compiled (fail-open): {def}",
    );
}

#[then(regex = r#"^tests\.html badges "([^"]+)" as not compiled$"#)]
fn tests_html_badges_not_compiled(world: &mut World, bare: String) {
    let html = tests_html(world);
    let section = model_section(&html, &bare);
    assert!(
        section.contains("not-compiled-badge"),
        "the {bare:?} section must carry the not-compiled badge: {section}",
    );
}

#[then(regex = r#"^tests\.html shows "([^"]+)" with zero unit tests wired$"#)]
fn tests_html_zero_tests(world: &mut World, bare: String) {
    let html = tests_html(world);
    let section = model_section(&html, &bare);
    assert!(
        section.contains("0 unit tests wired"),
        "a test-less model still renders, with the empty state: {section}",
    );
}

#[then(regex = r#"^tests\.html lists the unit test "([^"]+)" under "([^"]+)"$"#)]
fn tests_html_lists_test(world: &mut World, test: String, bare: String) {
    let html = tests_html(world);
    let section = model_section(&html, &bare);
    assert!(
        section.contains(&test),
        "the {bare:?} section must list {test:?}: {section}",
    );
}

#[then(regex = r#"^dag\.html carries a lineage edge from "([^"]+)" to "([^"]+)"$"#)]
fn dag_carries_edge(world: &mut World, from: String, to: String) {
    let def = lineage_def(world);
    // Recover the positional node ids (`n<i>["<label>"...`) from the def,
    // then assert the `n<from> --> n<to>` edge line.
    let node_id = |bare: &str| -> String {
        def.lines()
            .find_map(|line| {
                let line = line.trim();
                line.contains(&format!("[\"{bare}\"]"))
                    .then(|| line.split('[').next().unwrap_or_default().to_owned())
            })
            .unwrap_or_else(|| panic!("model {bare:?} is not a lineage node: {def}"))
    };
    let (from_id, to_id) = (node_id(&from), node_id(&to));
    assert!(
        def.contains(&format!("{from_id} --> {to_id}")),
        "expected the {from:?} -> {to:?} edge ({from_id} --> {to_id}): {def}",
    );
}

// --- helpers ---------------------------------------------------------

/// The rendered tests.html, panicking with stderr context when absent.
fn tests_html(world: &World) -> String {
    world
        .explore_tests_html
        .clone()
        .unwrap_or_else(|| panic!("tests.html was not written; stderr={}", world.last_stderr))
}

/// Slice one model's `<section class="explore-model">` out of
/// tests.html by its `aria-label`.
fn model_section(html: &str, bare: &str) -> String {
    let marker = format!("aria-label=\"Model {bare}\"");
    let start = html
        .find(&marker)
        .unwrap_or_else(|| panic!("no section for model {bare:?}: {html}"));
    let end = html[start..]
        .find("</section>")
        .map_or(html.len(), |e| start + e);
    html[start..end].to_owned()
}

/// Parse the embedded `cute-dbt-data` payload out of tests.html.
fn explore_payload(world: &World) -> Value {
    let html = tests_html(world);
    let dom = tl::parse(&html, tl::ParserOptions::default()).expect("tests.html must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id("cute-dbt-data")
        .expect("tests.html must embed <script id=\"cute-dbt-data\">")
        .get(parser)
        .expect("payload node resolves");
    serde_json::from_str(&node.inner_text(parser)).expect("embedded payload must be valid JSON")
}

/// Parse the Mermaid lineage definition out of dag.html's
/// `explore-dag-data` JSON carrier.
fn lineage_def(world: &World) -> String {
    let html = world
        .explore_dag_html
        .clone()
        .unwrap_or_else(|| panic!("dag.html was not written; stderr={}", world.last_stderr));
    let dom = tl::parse(&html, tl::ParserOptions::default()).expect("dag.html must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id("explore-dag-data")
        .expect("dag.html must embed <script id=\"explore-dag-data\">")
        .get(parser)
        .expect("dag data node resolves");
    let data: Value =
        serde_json::from_str(&node.inner_text(parser)).expect("dag data must be valid JSON");
    data["def"]
        .as_str()
        .expect("dag data carries the def string")
        .to_owned()
}
