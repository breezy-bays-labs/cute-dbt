//! Step definitions for `features/explore_js_contract.feature` —
//! cute-dbt#105, the explorer's external-drive contract: the
//! server-rendered contract-version attribute on `dag.html`'s `<body>`
//! and the per-node file paths on the lineage payload (`paths.sql` /
//! `paths.schema_yaml` / `paths.unit_tests[]`).
//!
//! Reuses the `explore_full_manifest.rs` Givens/When (the synthetic
//! [`ExplorePlan`](super::world::ExplorePlan) accumulator + the real
//! `cute-dbt explore` subprocess — the wire round-trip, so the
//! adapter's `<package>://` patch-path strip is exercised for real).
//! The Givens here declare the path-bearing wire shapes; the Thens
//! assert the carrier. The LIVE contract surface — `focusModel`'s
//! no-echo rule, `setView`, the dual-bound Space commit with an
//! injected host bridge — is the headless Chromium suite's job
//! (`tests/headless_zero_egress.rs`).

use cucumber::{given, then};
use serde_json::Value;

use super::World;
use super::explore_full_manifest::{lineage_payload, payload_node};
use super::world::ExplorePathTestDecl;

/// Build the domain `UnitTest` for one path-bearing declaration: the
/// declaring YAML as `original_file_path`, each given fixture as the
/// confirmed fusion external-fixture shape (`rows: null` + `fixture`),
/// and the optional expect fixture likewise.
pub fn path_unit_test(decl: &ExplorePathTestDecl) -> cute_dbt::domain::UnitTest {
    let given = decl
        .given_fixtures
        .iter()
        .map(|fixture| {
            cute_dbt::domain::UnitTestGiven::new(
                "ref('stg_orders')",
                Value::Null,
                Some("csv".to_owned()),
                Some(fixture.clone()),
            )
        })
        .collect();
    cute_dbt::domain::UnitTest::new(
        decl.name.clone(),
        cute_dbt::domain::NodeId::new(decl.target.clone()),
        given,
        cute_dbt::domain::UnitTestExpect::new(Value::Null, None, decl.expect_fixture.clone()),
        None,
        cute_dbt::domain::DependsOn::default(),
        None,
        None,
        decl.yaml.clone(),
    )
}

/// Borrow the previously-declared model (the
/// `explore_full_manifest::model_mut` twin — private there).
fn model_mut<'w>(world: &'w mut World, bare: &str) -> &'w mut super::world::ExploreModelDecl {
    world
        .explore_plan
        .models
        .iter_mut()
        .find(|m| m.bare == bare)
        .unwrap_or_else(|| panic!("model {bare:?} must be declared before it is configured"))
}

/// Borrow the previously-declared path test.
fn path_test_mut<'w>(world: &'w mut World, name: &str) -> &'w mut ExplorePathTestDecl {
    world
        .explore_plan
        .path_tests
        .iter_mut()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("path test {name:?} must be declared before it is configured"))
}

// --- Given ----------------------------------------------------------------

#[given(regex = r#"^the explore model "([^"]+)" has its SQL at "([^"]+)"$"#)]
fn model_sql_at(world: &mut World, bare: String, path: String) {
    model_mut(world, &bare).original_file_path = Some(path);
}

#[given(regex = r#"^the explore model "([^"]+)" is patched by the wire schema YAML "([^"]+)"$"#)]
fn model_patched_by(world: &mut World, bare: String, wire_patch_path: String) {
    // Spliced VERBATIM — scheme included — so the subprocess exercises
    // the adapter's package-URI strip (the wire shape both engines
    // emit; fusion `normalize_manifest_patch_path` @ `9977b6cb…`).
    model_mut(world, &bare).wire_patch_path = Some(wire_patch_path);
}

#[given(regex = r#"^a pathed unit test "([^"]+)" on "([^"]+)" declared in "([^"]+)"$"#)]
fn pathed_unit_test(world: &mut World, name: String, target: String, yaml: String) {
    world.explore_plan.path_tests.push(ExplorePathTestDecl {
        name,
        target,
        yaml: Some(yaml),
        given_fixtures: Vec::new(),
        expect_fixture: None,
    });
}

#[given(regex = r#"^the pathed unit test "([^"]+)" reads the external fixture "([^"]+)"$"#)]
fn pathed_test_given_fixture(world: &mut World, name: String, fixture: String) {
    path_test_mut(world, &name).given_fixtures.push(fixture);
}

#[given(regex = r#"^the pathed unit test "([^"]+)" expects the external fixture "([^"]+)"$"#)]
fn pathed_test_expect_fixture(world: &mut World, name: String, fixture: String) {
    path_test_mut(world, &name).expect_fixture = Some(fixture);
}

// --- Then -------------------------------------------------------------------

#[then(regex = r#"^dag\.html carries the external-drive contract version "([^"]+)" on its body$"#)]
fn dag_carries_contract_version(world: &mut World, version: String) {
    let html = world
        .explore_dag_html
        .clone()
        .unwrap_or_else(|| panic!("dag.html was not written; stderr={}", world.last_stderr));
    let marker = format!("<body data-cute-dbt-contract=\"{version}\">");
    assert!(
        html.contains(&marker),
        "dag.html must server-render the contract version on <body> \
         (attribute-only observers read it without executing JS); \
         wanted {marker:?}",
    );
}

/// The `paths` block of one lineage payload node.
fn paths(world: &World, bare: &str) -> Value {
    let payload = lineage_payload(world);
    payload_node(&payload, bare)["paths"].clone()
}

#[then(regex = r#"^the paths payload for "([^"]+)" carries sql "([^"]+)"$"#)]
fn paths_carries_sql(world: &mut World, bare: String, sql: String) {
    let p = paths(world, &bare);
    assert_eq!(p["sql"], Value::String(sql), "{bare:?} paths.sql: {p}");
}

#[then(regex = r#"^the paths payload for "([^"]+)" carries schema YAML "([^"]+)"$"#)]
fn paths_carries_schema_yaml(world: &mut World, bare: String, yaml: String) {
    let p = paths(world, &bare);
    assert_eq!(
        p["schema_yaml"],
        Value::String(yaml),
        "{bare:?} paths.schema_yaml must be the SCHEME-STRIPPED \
         project-relative path (the adapter drops `<package>://`): {p}",
    );
}

/// One unit-test entry off the paths block, by test name.
fn paths_test(p: &Value, name: &str) -> Value {
    p["unit_tests"]
        .as_array()
        .expect("paths.unit_tests array")
        .iter()
        .find(|t| t["name"] == Value::String(name.to_owned()))
        .unwrap_or_else(|| panic!("no paths.unit_tests entry named {name:?}: {p}"))
        .clone()
}

#[then(
    regex = r#"^the paths payload for "([^"]+)" lists unit test "([^"]+)" declared in "([^"]+)"$"#
)]
fn paths_lists_unit_test(world: &mut World, bare: String, name: String, yaml: String) {
    let p = paths(world, &bare);
    let t = paths_test(&p, &name);
    assert_eq!(
        t["yaml"],
        Value::String(yaml),
        "{name:?} must carry its declaring YAML path: {p}",
    );
}

#[then(
    regex = r#"^the paths payload for "([^"]+)" lists fixture "([^"]+)" for unit test "([^"]+)"$"#
)]
fn paths_lists_fixture(world: &mut World, bare: String, fixture: String, name: String) {
    let p = paths(world, &bare);
    let t = paths_test(&p, &name);
    let fixtures = t["fixtures"].as_array().expect("fixtures array");
    assert!(
        fixtures.contains(&Value::String(fixture.clone())),
        "{name:?} must list the external fixture ref {fixture:?} \
         VERBATIM as the manifest emits it: {p}",
    );
}

#[then(regex = r#"^the paths payload for "([^"]+)" is explicitly empty$"#)]
fn paths_explicitly_empty(world: &mut World, bare: String) {
    let p = paths(world, &bare);
    assert_eq!(p["sql"], Value::Null, "explicit null, never omitted: {p}");
    assert_eq!(p["schema_yaml"], Value::Null, "explicit null: {p}");
    assert_eq!(
        p["unit_tests"],
        Value::Array(Vec::new()),
        "explicit empty list: {p}",
    );
}
