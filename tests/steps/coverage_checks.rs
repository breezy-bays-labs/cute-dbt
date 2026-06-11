//! Step definitions for `features/coverage_checks.feature` — the
//! check-engine walking skeleton at the payload level (cute-dbt#169).
//!
//! Self-contained (the `incremental_models.rs` pattern): the Givens build
//! a [`CoveragePlan`](super::world::CoveragePlan) into the `World`, the
//! `When` serializes a synthetic current/baseline pair — injecting the
//! two wire shapes the flat-domain serialization cannot express (flat
//! model `config` carrying `unique_key`, and generic-test NODES with
//! `test_metadata` / `attached_node` / flat `config.enabled`) — and runs
//! the real `cute-dbt` subprocess. The Thens assert the embedded
//! `cute-dbt-data` payload's per-model `findings` facts (check id,
//! verdict status, attribution, recommendation). Rendering of findings
//! is cute-dbt#170's surface — nothing here asserts DOM.
//!
//! Step wording is deliberately coverage-specific (`coverage model`,
//! `I render the coverage report`) so it cannot collide with the
//! scaffolding bound by other feature modules.

use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use serde_json::{Value, json};

use super::super::common;
use super::World;
use super::builders::{
    empty_manifest, model_id, model_node, serialize_coverage_to_tmp, serialize_to_tmp, with_node,
};
use super::world::CoverageDataTest;

// --- Background -----------------------------------------------------

#[given("a coverage-check report scenario")]
fn given_scenario(world: &mut World) {
    world.coverage_plan = Default::default();
}

// --- Given ----------------------------------------------------------

#[given(regex = r#"^the modified coverage model "([^"]+)" declares unique_key (.+)$"#)]
fn model_declares_unique_key(world: &mut World, bare: String, key: String) {
    let key: Value = serde_json::from_str(&key)
        .unwrap_or_else(|err| panic!("unique_key literal {key:?} must be JSON: {err}"));
    world.coverage_plan.models.push((bare, key));
}

#[given(regex = r#"^the modified coverage model "([^"]+)" declares no unique_key$"#)]
fn model_declares_no_unique_key(world: &mut World, bare: String) {
    world.coverage_plan.models.push((bare, Value::Null));
}

#[given(regex = r#"^the modified coverage model "([^"]+)" compiles to:$"#)]
fn model_compiles_to(world: &mut World, step: &Step, bare: String) {
    // The cute-dbt#173 join-pair scenarios: the docstring is the
    // model's compiled SQL, fed through the real CTE-graph parse.
    let sql = step
        .docstring
        .as_ref()
        .expect("the step carries a SQL docstring")
        .trim()
        .to_owned();
    world.coverage_plan.sql_models.push((bare, sql));
}

#[given(regex = r#"^an? (enabled|disabled) unique data test on column "([^"]+)" of "([^"]+)"$"#)]
fn unique_data_test(world: &mut World, state: String, column: String, target: String) {
    world.coverage_plan.tests.push(CoverageDataTest {
        target,
        combo: false,
        columns: vec![column],
        enabled: state == "enabled",
    });
}

#[given(
    regex = r#"^an? (enabled|disabled) unique_combination_of_columns data test on columns (\[.+\]) of "([^"]+)"$"#
)]
fn combo_data_test(world: &mut World, state: String, columns: String, target: String) {
    let columns: Vec<String> = serde_json::from_str(&columns)
        .unwrap_or_else(|err| panic!("columns literal {columns:?} must be a JSON array: {err}"));
    world.coverage_plan.tests.push(CoverageDataTest {
        target,
        combo: true,
        columns,
        enabled: state == "enabled",
    });
}

// --- When -----------------------------------------------------------

/// Deterministic test-node id for a declared uniqueness data test — the
/// attribution Thens reconstruct the same id.
fn test_node_id(test: &CoverageDataTest) -> String {
    let kind = if test.combo { "combo" } else { "unique" };
    format!(
        "test.jaffle_shop.{kind}_{}_{}",
        test.target,
        test.columns.join("_")
    )
}

/// The wire JSON for one generic-test node — the real fusion shape
/// (`test_metadata` kwargs carry `column_name` for `unique` /
/// `combination_of_columns` for the dbt_utils composite; `config` is the
/// FLAT wire dict carrying `enabled`).
fn wire_test_node(test: &CoverageDataTest) -> Value {
    let test_metadata = if test.combo {
        json!({
            "name": "unique_combination_of_columns",
            "namespace": "dbt_utils",
            "kwargs": { "combination_of_columns": test.columns },
        })
    } else {
        json!({
            "name": "unique",
            "namespace": null,
            "kwargs": { "column_name": test.columns[0] },
        })
    };
    json!({
        "resource_type": "test",
        "checksum": { "name": "none", "checksum": "" },
        "attached_node": model_id(&test.target).as_str(),
        "column_name": if test.combo { Value::Null } else { Value::from(test.columns[0].clone()) },
        "test_metadata": test_metadata,
        "config": { "enabled": test.enabled },
    })
}

#[when("I render the coverage report")]
fn render_coverage_report(world: &mut World) {
    let plan = world.coverage_plan.clone();

    // Every declared model is modified vs the baseline (differing body
    // checksums) so the comparator puts it in scope; compiled_code
    // clears the Stage-2 preflight.
    let mut current = empty_manifest();
    let mut baseline = empty_manifest();
    for (bare, _key) in &plan.models {
        current = with_node(current, model_node(bare, "current", Some("select 1")));
        baseline = with_node(baseline, model_node(bare, "baseline", Some("select 1")));
    }
    for (bare, sql) in &plan.sql_models {
        current = with_node(current, model_node(bare, "current", Some(sql)));
        baseline = with_node(baseline, model_node(bare, "baseline", Some(sql)));
    }

    // Wire-shape injection: flat config (materialized + unique_key as
    // fusion serializes them) per model, plus the generic-test nodes.
    let configs: Vec<(&str, Value)> = plan
        .models
        .iter()
        .map(|(bare, key)| {
            let config = match key {
                Value::Null => json!({ "materialized": "incremental" }),
                key => json!({ "materialized": "incremental", "unique_key": key }),
            };
            (bare.as_str(), config)
        })
        .collect();
    let test_nodes: Vec<(String, Value)> = plan
        .tests
        .iter()
        .map(|test| (test_node_id(test), wire_test_node(test)))
        .collect();
    let current_path =
        serialize_coverage_to_tmp(&current, "coverage_current", &configs, &test_nodes);
    let baseline_path = serialize_to_tmp(&baseline, "coverage_baseline");

    let out = common::tmp("coverage_report.html");
    common::clear(&out);
    let output = common::run_cli(&[
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

#[then(regex = r#"^the payload carries a "([^"]+)" finding for "([^"]+)" with verdict "([^"]+)"$"#)]
fn payload_carries_finding(world: &mut World, check: String, model: String, status: String) {
    let finding = find_finding(world, &check, &model);
    assert_eq!(
        finding["verdict"]["status"].as_str(),
        Some(status.as_str()),
        "finding for {model:?} should have verdict {status:?}; got {finding}"
    );
}

#[then(regex = r#"^the "([^"]+)" finding for "([^"]+)" recommends adding a uniqueness test$"#)]
fn finding_carries_recommendation(world: &mut World, check: String, model: String) {
    let finding = find_finding(world, &check, &model);
    let recommendation = finding["recommendation"]
        .as_str()
        .unwrap_or_else(|| panic!("uncovered finding must carry a recommendation: {finding}"));
    assert!(
        recommendation.contains("unique"),
        "recommendation should name a uniqueness instrument; got {recommendation:?}"
    );
}

#[then(
    regex = r#"^the finding for "([^"]+)" attributes coverage to the unique data test on "([^"]+)"$"#
)]
fn finding_attributes_coverage(world: &mut World, model: String, column: String) {
    let finding = find_finding(world, "grain.unique-key-unbacked", &model);
    let expected = format!("test.jaffle_shop.unique_{model}_{column}");
    let by: Vec<&str> = finding["verdict"]["by"]
        .as_array()
        .unwrap_or_else(|| panic!("covered verdict carries `by`: {finding}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        by,
        vec![expected.as_str()],
        "coverage attributed to exactly the satisfying test node"
    );
}

#[then(regex = r#"^the payload carries no findings for "([^"]+)"$"#)]
fn payload_carries_no_findings(world: &mut World, model: String) {
    let p = payload(world);
    let m = find_model(&p, &model);
    assert!(
        m.get("findings").is_none(),
        "model {model:?} must carry NO findings key (serde-skipped when empty); got {m}"
    );
}

#[then(regex = r#"^the payload carries no "([^"]+)" finding for "([^"]+)"$"#)]
fn payload_carries_no_finding_of_check(world: &mut World, check: String, model: String) {
    // The supersedes contract at the wire level (cute-dbt#173): the
    // silenced check's finding must be ABSENT from the payload.
    let p = payload(world);
    let m = find_model(&p, &model);
    let offending: Vec<&Value> = m["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["check"].as_str() == Some(check.as_str()))
        .collect();
    assert!(
        offending.is_empty(),
        "model {model:?} must carry no {check:?} finding; got {offending:?}"
    );
}

#[then(regex = r#"^the "([^"]+)" finding for "([^"]+)" suggests a given row that matches$"#)]
fn finding_suggests_matching_given(world: &mut World, check: String, model: String) {
    // The INVERTED anti-join recommendation: a left row that DOES
    // match, proving the matched class is excluded.
    let sketch = suggested_given(world, &check, &model);
    assert!(
        sketch.contains("# matches the right row below"),
        "anti-join sketch suggests a MATCHING row; got {sketch:?}"
    );
    assert!(
        sketch.contains("must exclude it"),
        "anti-join sketch asserts the exclusion; got {sketch:?}"
    );
}

#[then(regex = r#"^the "([^"]+)" finding for "([^"]+)" suggests a no-match given row$"#)]
fn finding_suggests_no_match_given(world: &mut World, check: String, model: String) {
    let sketch = suggested_given(world, &check, &model);
    assert!(
        sketch.contains("# 404 has no match below"),
        "left-null sketch suggests a NO-MATCH row; got {sketch:?}"
    );
}

/// The `suggested given` evidence value on a finding.
fn suggested_given(world: &World, check: &str, model: &str) -> String {
    let finding = find_finding(world, check, model);
    finding["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|e| e["label"].as_str() == Some("suggested given"))
        .and_then(|e| e["value"].as_str())
        .unwrap_or_else(|| panic!("finding carries a suggested-given sketch: {finding}"))
        .to_owned()
}

// --- Payload helpers ------------------------------------------------

/// Parse the embedded `cute-dbt-data` JSON payload from the rendered
/// report (the `incremental_models.rs` shape).
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

/// Find the finding with check id `check` on the model named `model`.
fn find_finding(world: &World, check: &str, model: &str) -> Value {
    let p = payload(world);
    let m = find_model(&p, model);
    m["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["check"].as_str() == Some(check))
        .cloned()
        .unwrap_or_else(|| panic!("no {check:?} finding on model {model:?}: {m}"))
}
