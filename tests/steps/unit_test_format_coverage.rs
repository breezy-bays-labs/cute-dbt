//! Step definitions for `features/unit_test_format_coverage.feature` —
//! asserts cute-dbt renders unit_tests authored across dbt's three
//! fixture formats (dict, csv, sql for both `given` and `expect`).
//!
//! Exercises the committed `playground-current.json` +
//! `playground-baseline.json` pair (synthetic Synthea-derived). The
//! pair carries three unit_tests intentionally authored with different
//! formats so this feature pins the renderer against the full surface
//! — a change that breaks any one format would surface here before
//! shipping.
//!
//! Assertions parse the embedded `<script id="cute-dbt-data">` JSON
//! payload (the source of truth for the rendered DOM) rather than
//! string-grepping the HTML. The JS template builds per-model cards
//! at runtime from this payload, so model-scoped facts (does THIS
//! model have zero tests?) only have a structural answer at the
//! payload level.

use cucumber::{given, then, when};
use serde_json::Value;

use super::super::common;
use super::World;

#[given("the committed playground fixture pair")]
fn given_playground_fixture(_world: &mut World) {
    // Background — both playground-{current,baseline}.json are
    // committed under tests/fixtures/ and listed in MANIFEST.toml.
    // The fixture-manifest-listed test enforces presence.
}

#[when("I run cute-dbt against the playground fixture pair")]
fn when_run_against_playground(world: &mut World) {
    let manifest = common::fixture("playground-current.json");
    let baseline = common::fixture("playground-baseline.json");
    let out = common::tmp("bdd_unit_test_format_coverage.html");
    common::clear(&out);

    let output = common::run_cli(&[
        "--manifest",
        common::s(&manifest),
        "--baseline-manifest",
        common::s(&baseline),
        "--out",
        common::s(&out),
    ]);
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    world.out_path = Some(out);
}

/// Extract the embedded `<script id="cute-dbt-data">` JSON payload —
/// the source of truth for what the rendered DOM displays per model.
/// Panics with a useful message if the script element or its JSON is
/// missing, so a renderer regression that drops the payload fails
/// loudly here.
fn extract_payload(html: &str) -> Value {
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("report HTML must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id("cute-dbt-data")
        .expect("report must include <script id=\"cute-dbt-data\">")
        .get(parser)
        .expect("payload script node resolves");
    let raw = node.inner_text(parser);
    serde_json::from_str(&raw).expect("payload script body must be valid JSON")
}

fn find_model<'p>(payload: &'p Value, model_name: &str) -> Option<&'p Value> {
    payload
        .get("models")?
        .as_array()?
        .iter()
        .find(|m| m.get("name").and_then(Value::as_str) == Some(model_name))
}

fn model_tests(model: &Value) -> &[Value] {
    model
        .get("tests")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

#[then(regex = r#"^the playground report contains the unit test "([^"]+)"$"#)]
fn report_contains_unit_test(world: &mut World, test_name: String) {
    assert_eq!(
        world.last_exit_code,
        Some(0),
        "cute-dbt failed; stderr={}",
        world.last_stderr,
    );
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let payload = extract_payload(html);
    let mut found_model: Option<String> = None;
    for model in payload["models"]
        .as_array()
        .expect("payload.models is an array")
    {
        for test in model_tests(model) {
            if test.get("name").and_then(Value::as_str) == Some(&test_name) {
                found_model = model.get("name").and_then(Value::as_str).map(str::to_owned);
                break;
            }
        }
    }
    let owner = found_model.unwrap_or_else(|| {
        panic!(
            "expected the rendered payload to carry a unit test named {test_name}; \
             payload models: {:?}",
            payload["models"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|m| m.get("name").and_then(Value::as_str).unwrap_or("?"))
                .collect::<Vec<_>>(),
        )
    });
    // Stash the owning model so the next And step can verify the
    // test's `model:` binding without restating the name verbatim.
    world.last_named_model = Some(owner);
}

#[then(regex = r#"^that unit test names the target model "([^"]+)"$"#)]
fn unit_test_names_target_model(world: &mut World, model_name: String) {
    let owner = world
        .last_named_model
        .as_ref()
        .expect("the previous Then step set the owning model");
    assert_eq!(
        owner, &model_name,
        "expected unit test to be owned by {model_name}; payload says {owner}",
    );
}

#[then(regex = r#"^the playground report contains a section for the model "([^"]+)"$"#)]
fn report_contains_model_section(world: &mut World, model_name: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let payload = extract_payload(html);
    assert!(
        find_model(&payload, &model_name).is_some(),
        "expected payload to carry a model section for {model_name}; payload models: {:?}",
        payload["models"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|m| m.get("name").and_then(Value::as_str).unwrap_or("?"))
            .collect::<Vec<_>>(),
    );
    world.last_named_model = Some(model_name);
}

#[then("that model's section indicates zero unit tests are wired")]
fn model_section_indicates_empty_state(world: &mut World) {
    let model_name = world
        .last_named_model
        .as_ref()
        .expect("the previous Then step named a model");
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let payload = extract_payload(html);
    let model = find_model(&payload, model_name)
        .unwrap_or_else(|| panic!("model {model_name} present in payload"));
    let tests = model_tests(model);
    assert!(
        tests.is_empty(),
        "expected model {model_name} to have zero unit tests wired; payload says {} test(s): {:?}",
        tests.len(),
        tests
            .iter()
            .map(|t| t.get("name").and_then(Value::as_str).unwrap_or("?"))
            .collect::<Vec<_>>(),
    );
}
