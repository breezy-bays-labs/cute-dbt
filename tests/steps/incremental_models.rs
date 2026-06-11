//! Step definitions for `features/incremental_models.feature` —
//! incremental-model unit-test semantics (cute-dbt#145).
//!
//! Self-contained (the `cell_table_diff.rs` pattern): the Given steps build
//! an [`IncrementalPlan`](super::world::IncrementalPlan) into the `World`,
//! the `When` builds a synthetic current/baseline manifest pair and runs
//! the real `cute-dbt` subprocess, and the Then steps assert against the
//! embedded `cute-dbt-data` JSON **payload** facts (`is_incremental`,
//! `is_incremental_mode`, `is_this`). The *rendered* DOM/text behaviours —
//! the badge labels and the expect-semantics tooltip wording, which are
//! JS-generated and absent from the static HTML — are asserted by
//! `tests/headless_toggle.rs`.
//!
//! Step wording is deliberately incremental-specific (`declares unit test`,
//! `I render the incremental report`) so it does not collide with the
//! shared scaffolding steps (`report_generation.rs` binds the generic
//! `a compiled dbt 1.8+ manifest …` / `I run cute-dbt with …` wording to
//! the committed jaffle-shop fixture, which carries no incremental model).
//!
//! Two wire-shape divergences are injected by
//! [`serialize_incremental_to_tmp`](super::builders::serialize_incremental_to_tmp):
//! `config.materialized` (the domain serializes `NodeConfig` nested, the
//! wire flattens it) and `overrides.macros.is_incremental` (the domain
//! stores the mode flat, the wire nests it). See that function's docs.

use cucumber::{given, then, when};
use serde_json::Value;

use super::super::common;
use super::World;
use super::builders::{
    empty_manifest, incremental_unit_test, model_node, serialize_incremental_to_tmp,
    serialize_to_tmp, with_node, with_unit_test,
};
use super::world::IncrementalTest;

// --- Background -----------------------------------------------------

#[given("an incremental-model report scenario")]
fn given_scenario(world: &mut World) {
    world.incremental_plan = Default::default();
}

// --- Given ----------------------------------------------------------

#[given(regex = r#"^the model "([^"]+)" is materialized "([^"]+)"$"#)]
fn model_is_materialized(world: &mut World, bare: String, materialized: String) {
    world.incremental_plan.models.push((bare, materialized));
}

#[given(regex = r#"^"([^"]+)" was modified relative to the baseline$"#)]
fn model_modified(world: &mut World, bare: String) {
    world.incremental_plan.modified.push(bare);
}

#[given(regex = r#"^"([^"]+)" declares unit test "([^"]+)"$"#)]
fn declares_unit_test(world: &mut World, model: String, test: String) {
    world.incremental_plan.tests.push(IncrementalTest {
        name: test,
        target: model,
        mode: None,
        givens: Vec::new(),
    });
}

#[given(regex = r#"^the unit test "([^"]+)" overrides is_incremental to (true|false)$"#)]
fn overrides_is_incremental(world: &mut World, test: String, value: String) {
    test_mut(world, &test).mode = Some(value == "true");
}

#[given(regex = r#"^the unit test "([^"]+)" has a given input "([^"]+)"$"#)]
fn given_input(world: &mut World, test: String, input: String) {
    test_mut(world, &test).givens.push(input);
}

/// Borrow the previously-declared `IncrementalTest` named `name`, panicking
/// with context if a configuration step references an undeclared test.
fn test_mut<'w>(world: &'w mut World, name: &str) -> &'w mut IncrementalTest {
    world
        .incremental_plan
        .tests
        .iter_mut()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("unit test {name:?} must be declared before it is configured"))
}

// --- When -----------------------------------------------------------

#[when("I render the incremental report")]
fn render_incremental_report(world: &mut World) {
    let plan = world.incremental_plan.clone();

    let mut current = empty_manifest();
    let mut baseline = empty_manifest();
    for (bare, _materialized) in &plan.models {
        let modified = plan.modified.iter().any(|m| m == bare);
        // A modified model differs in checksum between current and baseline
        // (so the comparator puts it in scope); an unmodified one shares it.
        // `Some("select 1")` compiled_code clears the Stage-2 preflight.
        let (cur_ck, base_ck) = if modified {
            ("current", "baseline")
        } else {
            ("same", "same")
        };
        current = with_node(current, model_node(bare, cur_ck, Some("select 1")));
        baseline = with_node(baseline, model_node(bare, base_ck, Some("select 1")));
    }
    for t in &plan.tests {
        current = with_unit_test(
            current,
            incremental_unit_test(&t.name, &t.target, &t.givens),
        );
    }

    // Wire-shape injection: flat `config.materialized` per model, and
    // `overrides.macros.is_incremental` for the tests whose scenario set a
    // mode. Only the CURRENT manifest needs it — the baseline is read for
    // checksums only (scope), not for materialized/overrides.
    let materialized: Vec<(&str, &str)> = plan
        .models
        .iter()
        .map(|(bare, mat)| (bare.as_str(), mat.as_str()))
        .collect();
    let overrides: Vec<(&str, bool)> = plan
        .tests
        .iter()
        .filter_map(|t| t.mode.map(|m| (t.name.as_str(), m)))
        .collect();
    let current_path =
        serialize_incremental_to_tmp(&current, "incremental_current", &materialized, &overrides);
    let baseline_path = serialize_to_tmp(&baseline, "incremental_baseline");

    let out = common::tmp("incremental_report.html");
    common::clear(&out);
    let output = common::run_cli(&[
        "report",
        "--manifest",
        common::s(&current_path),
        "--baseline-manifest",
        common::s(&baseline_path),
        "--out",
        common::s(&out),
    ]);
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    if let Some(html) = &world.report_html {
        common::assert_no_external_refs(html);
    }
    world.out_path = Some(out);
}

// --- Then -----------------------------------------------------------

#[then(regex = r#"^"[^"]+" marks the model "([^"]+)" as incremental$"#)]
fn marks_model_incremental(world: &mut World, model: String) {
    let p = payload(world);
    let m = find_model(&p, &model);
    assert_eq!(
        m["is_incremental"].as_bool(),
        Some(true),
        "model {model:?} should carry is_incremental=true; got {m}"
    );
}

#[then(regex = r#"^"[^"]+" does not mark the model "([^"]+)" as incremental$"#)]
fn model_not_incremental(world: &mut World, model: String) {
    let p = payload(world);
    let m = find_model(&p, &model);
    assert_ne!(
        m["is_incremental"].as_bool(),
        Some(true),
        "model {model:?} must NOT be incremental (skip-when-false ⇒ absent/false); got {m}"
    );
}

#[then(
    regex = r#"^the section for "([^"]+)" marks the test as exercising the incremental branch$"#
)]
fn test_incremental_branch(world: &mut World, test: String) {
    let p = payload(world);
    let t = find_test(&p, &test);
    assert_eq!(
        t["is_incremental_mode"].as_bool(),
        Some(true),
        "test {test:?} mode should be Some(true); got {t}"
    );
}

#[then(
    regex = r#"^the section for "([^"]+)" marks the test as exercising the full-refresh branch$"#
)]
fn test_full_refresh_branch(world: &mut World, test: String) {
    let p = payload(world);
    let t = find_test(&p, &test);
    assert_eq!(
        t["is_incremental_mode"].as_bool(),
        Some(false),
        "test {test:?} mode should be Some(false); got {t}"
    );
}

#[then(regex = r#"^the section for "([^"]+)" explains the incremental expect semantics$"#)]
fn explains_incremental_expect(world: &mut World, test: String) {
    // The expect-semantics tooltip rides the authoritative payload bool: it
    // renders iff is_incremental_mode === true. cucumber asserts that
    // driving payload fact; `headless_toggle.rs` asserts the rendered text.
    // cute-dbt#159: the rendered copy is strategy-invariant (true for all 5
    // incremental strategies, not just merge/append).
    let p = payload(world);
    let t = find_test(&p, &test);
    assert_eq!(
        t["is_incremental_mode"].as_bool(),
        Some(true),
        "the incremental expect-semantics tooltip requires is_incremental_mode===true; got {t}"
    );
}

#[then(regex = r#"^the section for "([^"]+)" does not explain the incremental expect semantics$"#)]
fn no_incremental_expect_tooltip(world: &mut World, test: String) {
    let p = payload(world);
    let t = find_test(&p, &test);
    assert_ne!(
        t["is_incremental_mode"].as_bool(),
        Some(true),
        "the incremental expect-semantics tooltip must be absent unless is_incremental_mode===true; got {t}"
    );
}

#[then(regex = r#"^the section for "([^"]+)" marks the given "([^"]+)" as the prior model state$"#)]
fn given_is_prior_state(world: &mut World, test: String, input: String) {
    let p = payload(world);
    let t = find_test(&p, &test);
    let g = find_given(t, &input);
    assert_eq!(
        g["is_this"].as_bool(),
        Some(true),
        "given {input:?} should carry is_this=true; got {g}"
    );
}

#[then(
    regex = r#"^the section for "([^"]+)" does not mark the given "([^"]+)" as the prior model state$"#
)]
fn given_not_prior_state(world: &mut World, test: String, input: String) {
    let p = payload(world);
    let t = find_test(&p, &test);
    let g = find_given(t, &input);
    assert_ne!(
        g["is_this"].as_bool(),
        Some(true),
        "given {input:?} must NOT be prior-model-state (skip-when-false ⇒ absent/false); got {g}"
    );
}

#[then(
    regex = r#"^the section for "([^"]+)" does not mark the test with an incremental or full-refresh branch$"#
)]
fn test_no_mode_badge(world: &mut World, test: String) {
    let p = payload(world);
    let t = find_test(&p, &test);
    assert!(
        t["is_incremental_mode"].is_null(),
        "test {test:?} on a non-incremental model carries no mode (skip-when-none ⇒ absent); got {t}"
    );
}

// --- Payload helpers ------------------------------------------------

/// Parse the embedded `cute-dbt-data` JSON payload from the rendered report.
fn payload(world: &World) -> Value {
    let html = world
        .report_html
        .as_ref()
        .unwrap_or_else(|| panic!("report.html was not written; stderr={}", world.last_stderr));
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("report HTML must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id("cute-dbt-data")
        .expect("report must include <script id=\"cute-dbt-data\">")
        .get(parser)
        .expect("payload node resolves");
    serde_json::from_str(&node.inner_text(parser)).expect("embedded payload must be valid JSON")
}

/// Find a model object by its bare name in the rendered payload.
fn find_model<'p>(payload: &'p Value, name: &str) -> &'p Value {
    payload["models"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("model {name:?} not in payload: {payload}"))
}

/// Find a unit-test object by name across every model's `tests`.
fn find_test<'p>(payload: &'p Value, name: &str) -> &'p Value {
    payload["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["tests"].as_array())
        .flatten()
        .find(|t| t["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("test {name:?} not in payload: {payload}"))
}

/// Find a `given` object by its `input` value within a test object.
fn find_given<'t>(test: &'t Value, input: &str) -> &'t Value {
    test["given"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|g| g["input"].as_str() == Some(input))
        .unwrap_or_else(|| panic!("given {input:?} not in test: {test}"))
}
