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
    // Stash the owning model + the test name so follow-on And steps
    // can verify the test's `model:` binding and per-fixture shape
    // without restating the name verbatim.
    world.last_named_model = Some(owner);
    world.last_named_unit_test = Some(test_name);
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

/// Locate the most-recently-named unit test inside the rendered
/// payload — used by the per-fixture shape-assertion steps below.
fn find_named_unit_test<'p>(world: &mut World, payload: &'p Value) -> &'p Value {
    let test_name = world
        .last_named_unit_test
        .clone()
        .expect("a previous step named the unit test");
    for model in payload["models"]
        .as_array()
        .expect("payload.models is an array")
    {
        for test in model_tests(model) {
            if test.get("name").and_then(Value::as_str) == Some(&test_name) {
                return test;
            }
        }
    }
    panic!("unit test {test_name} disappeared from payload between steps")
}

/// Translate the BDD wording ("array" / "string") to the structural
/// assertion. Returns the assertion failure message if the shape
/// doesn't match — keeps the calling step a single expression.
fn rows_kind_matches(rows: &Value, kind: &str) -> Result<(), String> {
    match kind {
        "array" => {
            if rows.is_array() {
                Ok(())
            } else {
                Err(format!(
                    "expected rows shape `array`; payload carries {kind_actual}",
                    kind_actual = json_kind(rows),
                ))
            }
        }
        "string" => {
            if rows.is_string() {
                Ok(())
            } else {
                Err(format!(
                    "expected rows shape `string`; payload carries {kind_actual}",
                    kind_actual = json_kind(rows),
                ))
            }
        }
        other => Err(format!(
            "unsupported rows-kind {other:?}; the .feature step grammar accepts only \"array\" or \"string\""
        )),
    }
}

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[then(
    regex = r#"^the unit test's given fixture for input "([^"]+)" has format "([^"]+)" with rows as an? (array|string)$"#
)]
fn given_fixture_shape(world: &mut World, input: String, format: String, kind: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess")
        .clone();
    let payload = extract_payload(&html);
    let test = find_named_unit_test(world, &payload);
    let given_arr = test
        .get("given")
        .and_then(Value::as_array)
        .expect("unit test has a `given` array");
    let given = given_arr
        .iter()
        .find(|g| g.get("input").and_then(Value::as_str) == Some(&input))
        .unwrap_or_else(|| {
            panic!(
                "given fixture for input {input} not found; available inputs: {:?}",
                given_arr
                    .iter()
                    .filter_map(|g| g.get("input").and_then(Value::as_str))
                    .collect::<Vec<_>>(),
            )
        });
    let actual_format = given.get("format").and_then(Value::as_str).unwrap_or("");
    assert_eq!(
        actual_format, &format,
        "given fixture for input {input} has format {actual_format:?}; expected {format:?}"
    );
    let rows = given.get("rows").expect("given fixture has a `rows` field");
    rows_kind_matches(rows, &kind).unwrap_or_else(|msg| {
        panic!("given fixture for input {input}: {msg}");
    });
}

#[then(
    regex = r#"^the unit test's expected fixture has format "([^"]+)" with rows as an? (array|string)$"#
)]
fn expected_fixture_shape(world: &mut World, format: String, kind: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess")
        .clone();
    let payload = extract_payload(&html);
    let test = find_named_unit_test(world, &payload);
    let expected = test
        .get("expected")
        .expect("unit test has an `expected` block");
    let actual_format = expected.get("format").and_then(Value::as_str).unwrap_or("");
    assert_eq!(
        actual_format, &format,
        "expected fixture has format {actual_format:?}; expected {format:?}"
    );
    let rows = expected
        .get("rows")
        .expect("expected fixture has a `rows` field");
    rows_kind_matches(rows, &kind).unwrap_or_else(|msg| {
        panic!("expected fixture: {msg}");
    });
}
