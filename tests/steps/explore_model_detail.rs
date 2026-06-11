//! Step definitions for `features/explore_model_detail.feature` —
//! cute-dbt#104, the model-detail payload (description, config, grain,
//! columns) on the explore lineage carrier.
//!
//! Reuses the `explore_full_manifest.rs` Givens/When (the synthetic
//! [`ExplorePlan`](super::world::ExplorePlan) accumulator + the real
//! `cute-dbt explore` subprocess). The Givens here declare the
//! detail-bearing wire shapes — model `description`/`tags` (domain
//! round-trip), the flat `config` dict (`materialized` / `meta`), the
//! object-shaped `columns` map, and uniqueness-test nodes carrying the
//! real fusion `test_metadata` (`name` / `namespace` / `kwargs`) — and
//! the Thens assert the lineage carrier's per-node `detail` block. The
//! RENDERED card and the hover tooltip (which must never steal the
//! highlight or write the focus-commit attribute) are the headless
//! Chromium suite's job (`tests/headless_zero_egress.rs`).
//!
//! Kwarg keys per signature: `column_name` (native `unique`,
//! `dbt_expectations.expect_column_values_to_be_unique`),
//! `combination_of_columns` (`dbt_utils.unique_combination_of_columns`),
//! `column_names` (`dbt_constraints.{primary_key,unique_key}`),
//! `column_list` (`dbt_expectations.expect_compound_columns_to_be_
//! unique`). The first two are byte-confirmed against the committed
//! playground fixture; the package signatures are the synthetic
//! wire-splice arm (absent from the fixture), pinned from fusion
//! `primary_key_inference.rs` (`9977b6cb…`) and the packages' macros.

use cucumber::{given, then};
use serde_json::{Value, json};

use super::World;
use super::builders::model_id;
use super::explore_full_manifest::{lineage_payload, payload_node};
use super::world::ExploreUniquenessTestDecl;

/// The wire JSON for one declared uniqueness-test node — the real
/// fusion shape: `attached_node` (the inference linkage),
/// `test_metadata.{name,namespace,kwargs}` and the flat
/// `config.enabled` (the disabled-test scenario).
pub fn wire_uniqueness_test(decl: &ExploreUniquenessTestDecl) -> (String, Value) {
    let id = format!("test.jaffle_shop.{}", decl.name);
    let node = json!({
        "resource_type": "test",
        "checksum": { "name": "none", "checksum": "" },
        "attached_node": model_id(&decl.attached).as_str(),
        "column_name": null,
        "test_metadata": {
            "name": decl.test_name,
            "namespace": decl.namespace,
            "kwargs": decl.kwargs,
        },
        "config": { "enabled": decl.enabled },
        "depends_on": { "macros": [], "nodes": [model_id(&decl.attached).as_str()] },
        "original_file_path": null,
    });
    (id, node)
}

/// Push one uniqueness-test declaration onto the plan.
fn push_uniqueness_test(
    world: &mut World,
    attached: &str,
    test_name: &str,
    namespace: Option<&str>,
    kwargs: Value,
    enabled: bool,
) {
    let n = world.explore_plan.uniqueness_tests.len();
    world
        .explore_plan
        .uniqueness_tests
        .push(ExploreUniquenessTestDecl {
            name: format!("{test_name}_{attached}_{n}"),
            attached: attached.to_owned(),
            test_name: test_name.to_owned(),
            namespace: namespace.map(str::to_owned),
            kwargs,
            enabled,
        });
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

/// The model's flat wire `config` dict, created on first use.
fn flat_config<'w>(world: &'w mut World, bare: &str) -> &'w mut Value {
    let model = model_mut(world, bare);
    model.flat_config.get_or_insert_with(|| json!({}))
}

// --- Given ----------------------------------------------------------------

#[given(regex = r#"^the explore model "([^"]+)" is described as "([^"]+)"$"#)]
fn model_described(world: &mut World, bare: String, description: String) {
    model_mut(world, &bare).description = Some(description);
}

#[given(regex = r#"^the explore model "([^"]+)" is tagged "([^"]+)" and "([^"]+)"$"#)]
fn model_tagged(world: &mut World, bare: String, first: String, second: String) {
    model_mut(world, &bare).tags = vec![first, second];
}

#[given(regex = r#"^the explore model "([^"]+)" is materialized as "([^"]+)"$"#)]
fn model_materialized(world: &mut World, bare: String, materialized: String) {
    flat_config(world, &bare)["materialized"] = Value::String(materialized);
}

#[given(regex = r#"^the explore model "([^"]+)" carries meta "([^"]+)" = "([^"]+)"$"#)]
fn model_meta_entry(world: &mut World, bare: String, key: String, value: String) {
    let config = flat_config(world, &bare);
    if config.get("meta").is_none() {
        config["meta"] = json!({});
    }
    config["meta"][key] = Value::String(value);
}

#[given(regex = r#"^the explore model "([^"]+)" declares meta grain "([^"]+)"$"#)]
fn model_meta_grain(world: &mut World, bare: String, grain: String) {
    model_meta_entry(world, bare, "grain".to_owned(), grain);
}

#[given(
    regex = r#"^the explore model "([^"]+)" declares column "([^"]+)" typed "([^"]+)" described as "([^"]+)"$"#
)]
fn model_column(world: &mut World, bare: String, name: String, typed: String, described: String) {
    model_mut(world, &bare)
        .columns
        .push((name, Some(typed), Some(described)));
}

#[given(regex = r#"^a native unique test on "([^"]+)" column "([^"]+)"$"#)]
fn native_unique(world: &mut World, bare: String, column: String) {
    push_uniqueness_test(
        world,
        &bare,
        "unique",
        None,
        json!({ "column_name": column }),
        true,
    );
}

#[given(regex = r#"^a disabled native unique test on "([^"]+)" column "([^"]+)"$"#)]
fn disabled_native_unique(world: &mut World, bare: String, column: String) {
    push_uniqueness_test(
        world,
        &bare,
        "unique",
        None,
        json!({ "column_name": column }),
        false,
    );
}

#[given(regex = r#"^a "([^"]+)" test from "([^"]+)" on "([^"]+)" with column names "([^"]+)"$"#)]
fn constraints_test_single(
    world: &mut World,
    test_name: String,
    namespace: String,
    bare: String,
    column: String,
) {
    push_uniqueness_test(
        world,
        &bare,
        &test_name,
        Some(&namespace),
        json!({ "column_names": [column] }),
        true,
    );
}

#[given(
    regex = r#"^a "([^"]+)" test from "([^"]+)" on "([^"]+)" with column names "([^"]+)" and "([^"]+)"$"#
)]
fn constraints_test_pair(
    world: &mut World,
    test_name: String,
    namespace: String,
    bare: String,
    first: String,
    second: String,
) {
    push_uniqueness_test(
        world,
        &bare,
        &test_name,
        Some(&namespace),
        json!({ "column_names": [first, second] }),
        true,
    );
}

#[given(
    regex = r#"^a "([^"]+)" test from "([^"]+)" on "([^"]+)" combining "([^"]+)" and "([^"]+)"$"#
)]
fn combination_test(
    world: &mut World,
    test_name: String,
    namespace: String,
    bare: String,
    first: String,
    second: String,
) {
    push_uniqueness_test(
        world,
        &bare,
        &test_name,
        Some(&namespace),
        json!({ "combination_of_columns": [first, second] }),
        true,
    );
}

#[given(
    regex = r#"^an "([^"]+)" test from "([^"]+)" on "([^"]+)" listing "([^"]+)" and "([^"]+)"$"#
)]
fn column_list_test(
    world: &mut World,
    test_name: String,
    namespace: String,
    bare: String,
    first: String,
    second: String,
) {
    push_uniqueness_test(
        world,
        &bare,
        &test_name,
        Some(&namespace),
        json!({ "column_list": [first, second] }),
        true,
    );
}

#[given(regex = r#"^an "([^"]+)" test from "([^"]+)" on "([^"]+)" column "([^"]+)"$"#)]
fn column_name_test(
    world: &mut World,
    test_name: String,
    namespace: String,
    bare: String,
    column: String,
) {
    push_uniqueness_test(
        world,
        &bare,
        &test_name,
        Some(&namespace),
        json!({ "column_name": column }),
        true,
    );
}

// --- Then -------------------------------------------------------------------

/// The `detail` block of one lineage payload node.
fn detail(world: &World, bare: &str) -> Value {
    let payload = lineage_payload(world);
    payload_node(&payload, bare)["detail"].clone()
}

#[then(regex = r#"^the detail payload describes "([^"]+)" as "([^"]+)"$"#)]
fn detail_describes(world: &mut World, bare: String, description: String) {
    let d = detail(world, &bare);
    assert_eq!(
        d["description"],
        Value::String(description),
        "{bare:?} detail.description: {d}",
    );
}

#[then(regex = r#"^the detail payload materializes "([^"]+)" as "([^"]+)"$"#)]
fn detail_materializes(world: &mut World, bare: String, materialized: String) {
    let d = detail(world, &bare);
    assert_eq!(
        d["materialized"],
        Value::String(materialized),
        "{bare:?} detail.materialized: {d}",
    );
}

#[then(regex = r#"^the detail payload tags "([^"]+)" with "([^"]+)" and "([^"]+)"$"#)]
fn detail_tags(world: &mut World, bare: String, first: String, second: String) {
    let d = detail(world, &bare);
    assert_eq!(
        d["tags"],
        json!([first, second]),
        "{bare:?} detail.tags: {d}",
    );
}

#[then(regex = r#"^the detail payload carries meta "([^"]+)" = "([^"]+)" for "([^"]+)"$"#)]
fn detail_meta(world: &mut World, key: String, value: String, bare: String) {
    let d = detail(world, &bare);
    let entries = d["meta"].as_array().expect("detail.meta array");
    assert!(
        entries
            .iter()
            .any(|e| e["key"] == Value::String(key.clone())
                && e["value"] == Value::String(value.clone())),
        "{bare:?} detail.meta must carry {key:?} = {value:?}: {d}",
    );
}

#[then(
    regex = r#"^the detail payload lists column "([^"]+)" typed "([^"]+)" described as "([^"]+)" for "([^"]+)"$"#
)]
fn detail_column(world: &mut World, name: String, typed: String, described: String, bare: String) {
    let d = detail(world, &bare);
    let columns = d["columns"].as_array().expect("detail.columns array");
    assert!(
        columns
            .iter()
            .any(|c| c["name"] == Value::String(name.clone())
                && c["data_type"] == Value::String(typed.clone())
                && c["description"] == Value::String(described.clone())),
        "{bare:?} detail.columns must list {name:?} typed {typed:?} described {described:?}: {d}",
    );
}

#[then(regex = r#"^the grain of "([^"]+)" is "([^"]+)" sourced from "([^"]+)"$"#)]
fn grain_resolves(world: &mut World, bare: String, value: String, source: String) {
    let d = detail(world, &bare);
    assert_eq!(
        d["grain"]["value"],
        Value::String(value),
        "{bare:?} grain value (precedence ladder): {d}",
    );
    assert_eq!(
        d["grain"]["source"],
        Value::String(source),
        "{bare:?} grain source: {d}",
    );
    assert_eq!(
        d["grain"]["known"],
        Value::Bool(true),
        "{bare:?} resolved grain is known: {d}",
    );
}

#[then(regex = r#"^the grain of "([^"]+)" is explicitly unknown$"#)]
fn grain_unknown(world: &mut World, bare: String) {
    let d = detail(world, &bare);
    assert_eq!(
        d["grain"]["value"],
        Value::String("unknown".to_owned()),
        "{bare:?} grain must render the explicit unknown — never a guess: {d}",
    );
    assert_eq!(d["grain"]["source"], Value::String("unknown".to_owned()));
    assert_eq!(
        d["grain"]["known"],
        Value::Bool(false),
        "{bare:?} grain.known must be false on the unknown rung: {d}",
    );
}

#[then(regex = r#"^the grain detection for "([^"]+)" also surfaces an? "([^"]+)" of "([^"]+)"$"#)]
fn grain_also_surfaces(world: &mut World, bare: String, kind: String, value: String) {
    let d = detail(world, &bare);
    let detected = d["grain"]["detected"].as_array().expect("detected array");
    assert!(
        detected
            .iter()
            .any(|s| s["kind"] == Value::String(kind.clone())
                && s["value"] == Value::String(value.clone())),
        "{bare:?} grain.detected must surface the {kind:?} signal {value:?} \
         (all detected grains surfaced): {d}",
    );
}
