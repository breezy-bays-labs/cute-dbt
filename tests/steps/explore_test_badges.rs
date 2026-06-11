//! Step definitions for `features/explore_test_badges.feature` —
//! cute-dbt#103, the per-node test-count badges on the explore lineage
//! DAG.
//!
//! Reuses the `explore_full_manifest.rs` Givens/When (the synthetic
//! [`ExplorePlan`](super::world::ExplorePlan) accumulator + the real
//! `cute-dbt explore` subprocess); the Givens here declare data-test
//! nodes that the shared `When` splices into the serialized manifest in
//! the REAL fusion wire shape (`attached_node` / `depends_on` /
//! `original_file_path` — the coverage_checks.rs splice precedent), and
//! the Thens assert the lineage carrier's per-node counts + the
//! Rust-composed badge string. The RENDERED badge (the canvas label's
//! second line) is the headless Chromium suite's job
//! (`tests/headless_zero_egress.rs`).

use cucumber::{given, then};
use serde_json::{Value, json};

use super::World;
use super::builders::model_id;
use super::explore_full_manifest::{lineage_payload, payload_node};
use super::world::ExploreDataTestDecl;

/// The wire JSON for one declared data-test node — the real fusion
/// shape: `attached_node` (the cute-dbt#103 attribution linkage; `null`
/// = the singular-test null-fill), `depends_on.nodes` (a relationships
/// test reaches its `to:` target here WITHOUT attributing) and the
/// declaring file's `original_file_path` (attribution must ignore it).
pub fn wire_data_test(decl: &ExploreDataTestDecl) -> (String, Value) {
    let id = format!("test.jaffle_shop.{}", decl.name);
    let attached = decl
        .attached
        .as_deref()
        .map_or(Value::Null, |bare| Value::from(model_id(bare).as_str()));
    let deps: Vec<Value> = decl
        .depends_on
        .iter()
        .map(|bare| Value::from(model_id(bare).as_str()))
        .collect();
    let node = json!({
        "resource_type": "test",
        "checksum": { "name": "none", "checksum": "" },
        "attached_node": attached,
        "column_name": null,
        // Singular tests carry no test_metadata; generic YAML tests do
        // (attribution keys on attached_node either way).
        "test_metadata": decl.attached.as_deref().map(|_| json!({
            "name": "not_null",
            "namespace": null,
            "kwargs": {},
        })),
        "config": { "enabled": true },
        "depends_on": { "macros": [], "nodes": deps },
        "original_file_path": decl.declared_in,
    });
    (id, node)
}

// --- Given ----------------------------------------------------------------

#[given(regex = r#"^a data test attached to "([^"]+)" is declared in "([^"]+)"$"#)]
fn data_test_declared_elsewhere(world: &mut World, target: String, declared_in: String) {
    let n = world.explore_plan.data_tests.len();
    world.explore_plan.data_tests.push(ExploreDataTestDecl {
        name: format!("not_null_{target}_{n}"),
        attached: Some(target.clone()),
        depends_on: vec![target],
        declared_in: Some(declared_in),
    });
}

#[given(regex = r#"^a relationships data test attached to "([^"]+)" reaches "([^"]+)"$"#)]
fn relationships_data_test(world: &mut World, target: String, reaches: String) {
    let n = world.explore_plan.data_tests.len();
    world.explore_plan.data_tests.push(ExploreDataTestDecl {
        name: format!("relationships_{target}_{n}"),
        attached: Some(target.clone()),
        // dbt puts BOTH the attached model and the `to:` target on
        // depends_on.nodes (verified on the committed playground
        // fixture) — only attached_node may attribute.
        depends_on: vec![reaches, target],
        declared_in: None,
    });
}

#[given(regex = r#"^a singular data test depending on "([^"]+)" carries no attached node$"#)]
fn singular_data_test(world: &mut World, target: String) {
    let n = world.explore_plan.data_tests.len();
    world.explore_plan.data_tests.push(ExploreDataTestDecl {
        name: format!("assert_{target}_{n}"),
        attached: None,
        depends_on: vec![target.clone()],
        declared_in: Some(format!("tests/assert_{target}.sql")),
    });
}

// --- Then -------------------------------------------------------------------

#[then(
    regex = r#"^the lineage carrier counts (\d+) data-tests? and (\d+) unit-tests? for "([^"]+)"$"#
)]
fn carrier_counts(world: &mut World, data_tests: u64, unit_tests: u64, bare: String) {
    let payload = lineage_payload(world);
    let node = payload_node(&payload, &bare);
    assert_eq!(
        node["data_tests"],
        Value::from(data_tests),
        "{bare:?} must count {data_tests} data-tests (attribution is by \
         attached_node — never the declaring file, never depends_on): {payload}",
    );
    assert_eq!(
        node["unit_tests"],
        Value::from(unit_tests),
        "{bare:?} must count {unit_tests} unit-tests (the bare model: \
         reference resolves via resolve_target_model): {payload}",
    );
}

#[then(regex = r#"^the lineage carrier badges "([^"]+)" with "([^"]+)"$"#)]
fn carrier_badges(world: &mut World, bare: String, badge: String) {
    let payload = lineage_payload(world);
    let node = payload_node(&payload, &bare);
    assert_eq!(
        node["badge"],
        Value::String(badge.clone()),
        "{bare:?} must carry the Rust-composed badge {badge:?} (explicit \
         at 0/0 — never an omitted key): {payload}",
    );
}
