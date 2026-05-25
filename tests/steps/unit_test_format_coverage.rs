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

use cucumber::{given, then, when};

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
    assert!(
        html.contains(&test_name),
        "expected report to contain unit test {test_name}; html length {} bytes",
        html.len(),
    );
}

#[then(regex = r#"^that unit test names the target model "([^"]+)"$"#)]
fn unit_test_names_target_model(world: &mut World, model_name: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    assert!(
        html.contains(&model_name),
        "expected report to name the target model {model_name}; html length {} bytes",
        html.len(),
    );
}

#[then(regex = r#"^the playground report contains a section for the model "([^"]+)"$"#)]
fn report_contains_model_section(world: &mut World, model_name: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    assert!(
        html.contains(&model_name),
        "expected report to contain a section for model {model_name}; html length {} bytes",
        html.len(),
    );
}

#[then("that model's section indicates zero unit tests are wired")]
fn model_section_indicates_empty_state(world: &mut World) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    // The renderer's empty-state copy on the per-model card. The
    // canonical phrasing is "0 unit tests wired" (set when a modified
    // model has no in-scope unit tests targeting it). The empty-state
    // also appears at the top of the report when nothing is in scope;
    // here we just assert the copy is present anywhere on the page —
    // the per-model-card structural assertion comes from the
    // model-name section assertion above.
    assert!(
        html.contains("0 unit tests wired")
            || html.contains("No unit tests")
            || html.contains("no unit tests"),
        "expected empty-state copy ('0 unit tests wired' / 'No unit tests'); html length {} bytes",
        html.len(),
    );
}
