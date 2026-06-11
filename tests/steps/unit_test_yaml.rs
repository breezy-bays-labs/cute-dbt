//! Step definitions for `features/unit_test_yaml.feature` —
//! pins the end-to-end wiring for the authoring-YAML drawer
//! (cute-dbt#69).
//!
//! Exercises the committed `source-yaml/manifest-{current,baseline}.json`
//! pair plus the paired source YAML stub at
//! `source-yaml/project/models/_unit_tests.yml`. The 20 inline tests in
//! `src/domain/unit_test_yaml.rs` cover slicer bracketing exhaustively;
//! these scenarios only assert the pipeline-level integration where
//! new wiring could regress without those unit tests catching it.
//!
//! Assertions parse the embedded `<script id="cute-dbt-data">` JSON
//! payload (the source of truth for what the rendered DOM displays).
//! The renderer wires `test.authoring_yaml` from this payload at
//! runtime, so structural payload-level assertions are the right
//! grain for "does the drawer show what we authored?".

use cucumber::{given, then, when};
use serde_json::Value;

use super::super::common;
use super::World;

#[given("the committed source-yaml fixture pair")]
fn given_source_yaml_fixture(_world: &mut World) {
    // Background — the manifest pair AND the source YAML stub are
    // committed under tests/fixtures/source-yaml/ and listed in
    // MANIFEST.toml. The fixture-manifest-listed test enforces
    // presence + SHA-256 integrity.
}

fn run_against_source_yaml(world: &mut World, extra_args: &[&str], out_name: &str) {
    let manifest = common::fixture("source-yaml/manifest-current.json");
    let baseline = common::fixture("source-yaml/manifest-baseline.json");
    let out = common::tmp(out_name);
    common::clear(&out);

    let mut args = vec![
        "report",
        "--manifest",
        common::s(&manifest),
        "--baseline-manifest",
        common::s(&baseline),
        "--out",
        common::s(&out),
    ];
    args.extend_from_slice(extra_args);

    let output = common::run_cli(&args);
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    world.out_path = Some(out);
}

#[when(
    "I run cute-dbt against the source-yaml fixture pair with --project-root \
     pointing at the synthetic project"
)]
fn when_with_project_root(world: &mut World) {
    let project_root = common::fixture("source-yaml/project");
    let project_root_arg = common::s(&project_root).to_owned();
    run_against_source_yaml(
        world,
        &["--project-root", &project_root_arg],
        "bdd_unit_test_yaml_with_root.html",
    );
}

#[when("I run cute-dbt against the source-yaml fixture pair without --project-root")]
fn when_without_project_root(world: &mut World) {
    run_against_source_yaml(world, &[], "bdd_unit_test_yaml_without_root.html");
}

#[when(
    "I run cute-dbt against the source-yaml fixture pair with --project-root \
     pointing at an empty directory"
)]
fn when_project_root_empty_dir(world: &mut World) {
    // Create (or reuse) an empty directory inside CARGO_TARGET_TMPDIR
    // that contains no `models/_unit_tests.yml` — the gather stage's
    // FsProjectFileReader should soft-fail per test and the payload
    // should carry no `authoring_yaml` field for any unit test.
    let empty_root = common::tmp("source-yaml-empty-root");
    std::fs::create_dir_all(&empty_root)
        .expect("create empty project-root scratch dir under CARGO_TARGET_TMPDIR");
    let empty_root_arg = common::s(&empty_root).to_owned();
    run_against_source_yaml(
        world,
        &["--project-root", &empty_root_arg],
        "bdd_unit_test_yaml_empty_root.html",
    );
}

/// Extract the embedded `<script id="cute-dbt-data">` JSON payload —
/// the source of truth for what the rendered DOM displays per model.
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

fn model_tests(model: &Value) -> &[Value] {
    model
        .get("tests")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

#[then(regex = r#"^the source-yaml report contains the unit test "([^"]+)"$"#)]
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
    let mut found = false;
    for model in payload["models"]
        .as_array()
        .expect("payload.models is an array")
    {
        for test in model_tests(model) {
            if test.get("name").and_then(Value::as_str) == Some(&test_name) {
                found = true;
                break;
            }
        }
    }
    assert!(
        found,
        "expected the rendered payload to carry a unit test named {test_name}; \
         payload models: {:?}",
        payload["models"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|m| m.get("name").and_then(Value::as_str).unwrap_or("?"))
            .collect::<Vec<_>>(),
    );
    world.last_named_unit_test = Some(test_name);
}

fn find_named_test<'p>(world: &World, payload: &'p Value) -> Option<&'p Value> {
    let test_name = world.last_named_unit_test.as_ref()?;
    for model in payload["models"].as_array()? {
        for test in model_tests(model) {
            if test.get("name").and_then(Value::as_str) == Some(test_name.as_str()) {
                return Some(test);
            }
        }
    }
    None
}

#[then(regex = r#"^the unit test "([^"]+)" carries authoring YAML containing "([^"]+)"$"#)]
fn unit_test_authoring_yaml_contains(
    world: &mut World,
    test_name: String,
    expected_substring: String,
) {
    assert_eq!(
        Some(&test_name),
        world.last_named_unit_test.as_ref(),
        "step references {test_name}, but the previous step named {:?}",
        world.last_named_unit_test,
    );
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let payload = extract_payload(html);
    let test = find_named_test(world, &payload)
        .unwrap_or_else(|| panic!("unit test {test_name} disappeared from payload"));
    let yaml = test
        .get("authoring_yaml")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "expected unit test {test_name} to carry an `authoring_yaml` field; \
                 payload keys: {:?}",
                test.as_object()
                    .map(|o| o.keys().collect::<Vec<_>>())
                    .unwrap_or_default(),
            )
        });
    assert!(
        yaml.contains(&expected_substring),
        "expected authoring_yaml for {test_name} to contain {expected_substring:?}; \
         got: {yaml:?}",
    );
}

#[then(regex = r#"^the unit test "([^"]+)" carries no authoring YAML in the payload$"#)]
fn unit_test_authoring_yaml_absent(world: &mut World, test_name: String) {
    assert_eq!(
        Some(&test_name),
        world.last_named_unit_test.as_ref(),
        "step references {test_name}, but the previous step named {:?}",
        world.last_named_unit_test,
    );
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let payload = extract_payload(html);
    let test = find_named_test(world, &payload)
        .unwrap_or_else(|| panic!("unit test {test_name} disappeared from payload"));
    assert!(
        test.get("authoring_yaml").is_none(),
        "expected unit test {test_name} to carry NO `authoring_yaml` field; \
         payload has authoring_yaml={:?}",
        test.get("authoring_yaml"),
    );
}
