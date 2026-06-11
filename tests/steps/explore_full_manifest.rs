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
    empty_manifest, model_node_with_deps, serialize_explore_to_tmp, unit_test_for, with_node,
    with_unit_test,
};
use super::explore_cli::run_explore;
use super::explore_model_detail::wire_uniqueness_test;
use super::explore_test_badges::wire_data_test;
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
        compiled_sql: None,
        deps: Vec::new(),
        tests: Vec::new(),
        description: None,
        tags: Vec::new(),
        flat_config: None,
        columns: Vec::new(),
        original_file_path: None,
        wire_patch_path: None,
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
    // cute-dbt#104 — wire-level patches the flat-domain serialization
    // cannot express: the flat `config` dict and the object-shaped
    // `columns` map (`(bare, top-level key, value)` triples).
    let mut node_patches: Vec<(String, String, serde_json::Value)> = Vec::new();
    for m in &plan.models {
        let deps: Vec<&str> = m.deps.iter().map(String::as_str).collect();
        // cute-dbt#102 — an explicit compiled body (the CTE-view
        // scenarios author a WITH clause) over the `select 1` default.
        let compiled = m
            .compiled
            .then(|| m.compiled_sql.as_deref().unwrap_or("select 1"));
        // cute-dbt#104 — description + tags ride the domain node (both
        // round-trip the wire verbatim as top-level keys).
        let node = model_node_with_deps(&m.bare, "ck", compiled, &deps)
            .with_model_metadata(m.description.clone(), m.tags.clone());
        manifest = with_node(manifest, node);
        for test in &m.tests {
            manifest = with_unit_test(manifest, unit_test_for(test, &m.bare));
        }
        if let Some(config) = &m.flat_config {
            node_patches.push((m.bare.clone(), "config".to_owned(), config.clone()));
        }
        // cute-dbt#105 — per-node file paths: the SQL source path and
        // the schema-YAML patch path. `patch_path` splices VERBATIM
        // (scheme included) so the subprocess exercises the adapter's
        // package-URI strip for real.
        if let Some(path) = &m.original_file_path {
            node_patches.push((
                m.bare.clone(),
                "original_file_path".to_owned(),
                serde_json::json!(path),
            ));
        }
        if let Some(patch) = &m.wire_patch_path {
            node_patches.push((
                m.bare.clone(),
                "patch_path".to_owned(),
                serde_json::json!(patch),
            ));
        }
        if !m.columns.is_empty() {
            let columns: serde_json::Map<String, serde_json::Value> = m
                .columns
                .iter()
                .map(|(name, data_type, description)| {
                    (
                        name.clone(),
                        serde_json::json!({
                            "name": name,
                            "data_type": data_type,
                            "description": description.clone().unwrap_or_default(),
                        }),
                    )
                })
                .collect();
            node_patches.push((
                m.bare.clone(),
                "columns".to_owned(),
                serde_json::Value::Object(columns),
            ));
        }
    }
    // cute-dbt#105 — path-bearing unit tests (declaring YAML + external
    // fixture refs), built as DOMAIN unit tests: the domain serializes
    // the confirmed fusion wire shape directly (`original_file_path`
    // top-level; `rows: null` + `fixture` on external given/expect).
    for t in &plan.path_tests {
        manifest = with_unit_test(manifest, super::explore_js_contract::path_unit_test(t));
    }
    // cute-dbt#103 — splice the declared data-test nodes in the REAL
    // fusion wire shape (the coverage_checks.rs precedent: the domain
    // types do not round-trip a flat test-node `config`). An empty
    // declaration list degenerates to the plain serialization.
    // cute-dbt#104 — the uniqueness-test nodes ride the same splice.
    let mut test_nodes: Vec<(String, serde_json::Value)> =
        plan.data_tests.iter().map(wire_data_test).collect();
    test_nodes.extend(plan.uniqueness_tests.iter().map(wire_uniqueness_test));
    let manifest_path = serialize_explore_to_tmp(
        &manifest,
        "explore_full_manifest",
        &node_patches,
        &test_nodes,
    );
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
    let payload = lineage_payload(world);
    let node = payload_node(&payload, &bare);
    assert_eq!(
        node["not_compiled"],
        Value::Bool(true),
        "the lineage payload must mark {bare:?} not-compiled (fail-open): {payload}",
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
    let payload = lineage_payload(world);
    let from_id = payload_node(&payload, &from)["id"].clone();
    let to_id = payload_node(&payload, &to)["id"].clone();
    let edges = payload["edges"].as_array().expect("edges array");
    assert!(
        edges
            .iter()
            .any(|e| e["from"] == from_id && e["to"] == to_id),
        "expected the {from:?} -> {to:?} edge in the lineage payload: {payload}",
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

/// Parse the lineage payload (cute-dbt#101 — nodes + forward edges) out
/// of dag.html's `explore-dag-data` JSON carrier.
pub fn lineage_payload(world: &World) -> Value {
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
    serde_json::from_str(&node.inner_text(parser)).expect("dag data must be valid JSON")
}

/// Find one payload node by bare model name, panicking with the payload
/// as context when absent.
pub fn payload_node<'p>(payload: &'p Value, bare: &str) -> &'p Value {
    payload["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .find(|n| n["name"] == Value::String(bare.to_owned()))
        .unwrap_or_else(|| panic!("model {bare:?} is not a lineage node: {payload}"))
}
