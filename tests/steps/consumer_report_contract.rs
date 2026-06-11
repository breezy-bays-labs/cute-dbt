//! Step definitions for `features/consumer_report_contract.feature` —
//! the formal articulation of cute-dbt's expected report properties for
//! the CI sticky-comment use case (cute-dbt#71). The recipe at
//! `book/src/recipes/ci-sticky-comment.md` documents the workflow
//! shape; these scenarios pin the structural properties of the rendered
//! report that make that workflow useful.
//!
//! Doc-feature framing: the Given + When steps are reused from
//! `unit_test_format_coverage.rs` (the rendered playground report is
//! the same artifact regardless of which contract we're asserting);
//! the Then steps below delegate to existing test infrastructure
//! (`common::assert_no_external_refs`, embedded-payload parsing,
//! `std::fs::metadata`). The .feature file is the formal statement of
//! the contract; these step defs are thin glue over the existing
//! proof.

use cucumber::{then, when};
use serde_json::Value;

use super::super::common;
use super::World;

/// Soft size budget for the rendered report's single-file HTML. Not a
/// GitHub Actions artifact upload limit (those are generously sized);
/// the budget is for the reviewer's local download + browser-open
/// experience. Today's committed examples are ~3.5 MB each; 10 MB
/// gives ~3x headroom before this gate fires.
const REPORT_SIZE_BUDGET_BYTES: u64 = 10 * 1024 * 1024;

#[then("the resulting HTML contains zero external resource references")]
fn then_zero_external_refs(world: &mut World) {
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
    common::assert_no_external_refs(html);
}

#[then(regex = r#"^the resulting HTML embeds the "([^"]+)" payload with at least one model$"#)]
fn then_embeds_payload_with_models(world: &mut World, payload_id: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("report HTML must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id(payload_id.as_str())
        .unwrap_or_else(|| panic!("report must include <script id={payload_id:?}>"))
        .get(parser)
        .expect("payload node resolves");
    let raw = node.inner_text(parser);
    let payload: Value = serde_json::from_str(&raw).expect("embedded payload must be valid JSON");
    let models = payload["models"]
        .as_array()
        .expect("payload.models is an array");
    assert!(
        !models.is_empty(),
        "expected at least one model in the rendered payload; got 0 — the modified-set \
         selection is not surfacing any models, which would defeat the sticky-comment \
         affordance for the reviewer",
    );
}

/// Re-runs cute-dbt against the playground fixture pair WITH
/// `--project-root` pointing at the committed playground source YAML.
/// Required for the cute-dbt#73 substring scenarios because the default
/// playground When step (`when_run_against_playground` in
/// `unit_test_format_coverage.rs`) doesn't surface the Authoring YAML
/// drawer (no `--project-root`). Mirrors the CI matrix's playground
/// invocation so the BDD layer exercises the same path reviewers see in
/// the rendered example.
#[when(
    "I run cute-dbt against the playground fixture pair with --project-root \
     pointing at the committed playground source"
)]
fn when_run_against_playground_with_project_root(world: &mut World) {
    let manifest = common::fixture("playground-current.json");
    let baseline = common::fixture("playground-baseline.json");
    let project_root = common::fixture("playground-source");
    let project_root_arg = common::s(&project_root).to_owned();
    let out = common::tmp("bdd_consumer_report_drawer_substring.html");
    common::clear(&out);

    let output = common::run_cli(&[
        "report",
        "--manifest",
        common::s(&manifest),
        "--baseline-manifest",
        common::s(&baseline),
        "--project-root",
        &project_root_arg,
        "--out",
        common::s(&out),
    ]);
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    world.out_path = Some(out);
}

#[then(
    regex = r#"^the Authoring YAML drawer for at least one unit test contains the substring "([^"]+)"$"#
)]
fn then_drawer_contains_substring(world: &mut World, substring: String) {
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
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("report HTML must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id("cute-dbt-data")
        .expect("report must include <script id=\"cute-dbt-data\">")
        .get(parser)
        .expect("payload node resolves");
    let raw = node.inner_text(parser);
    let payload: Value = serde_json::from_str(&raw).expect("embedded payload must be valid JSON");

    let mut found = false;
    let empty = Vec::new();
    for model in payload["models"].as_array().unwrap_or(&empty) {
        let tests = model
            .get("tests")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        for test in tests {
            if let Some(yaml) = test.get("authoring_yaml").and_then(Value::as_str) {
                if yaml.contains(&substring) {
                    found = true;
                    break;
                }
            }
        }
        if found {
            break;
        }
    }
    assert!(
        found,
        "expected at least one unit test's authoring_yaml to contain {substring:?}; \
         no test in the rendered payload carried that substring. This typically means \
         the slicer didn't surface the playground YAML's bracket comments — verify \
         tests/fixtures/playground-source/ matches the source-of-truth in \
         cmbays/dbt-playground (see MANIFEST.toml origin_url SHA).",
    );
}

/// cute-dbt#74 (re-homed by cute-dbt#201): pin the structural ordering of
/// the test section — the always-open test card hosting the unit-test
/// selector, badge row, and description — between the CTE DAG and the
/// given/expected panels. The byte-identity insta snapshot also catches a
/// regression, but snapshots get rebaselined reflexively; this scenario
/// fails loudly with a load-bearing message if the test context drifts
/// back to the top of the page.
#[then("the rendered HTML places the test section between the cte-dag section and the panel-row")]
fn then_test_section_between_dag_and_panels(world: &mut World) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    // Match the class attribute substring (no trailing `"`) so the
    // assertion is robust to additional classes on the element — the BDD
    // is asserting structural DOM order only.
    let dag_pos = html
        .find(r#"<section class="cte-dag"#)
        .expect("rendered HTML must include <section class=\"cte-dag\">");
    let test_pos = html.find(r#"<section class="test-section"#).expect(
        "rendered HTML must include <section class=\"test-section\"> \
         (cute-dbt#201 relocated the test card — selector, badges, \
         description — there)",
    );
    let panel_pos = html
        .find(r#"<div class="panel-row"#)
        .expect("rendered HTML must include <div class=\"panel-row\">");
    assert!(
        dag_pos < test_pos,
        "test-section should appear AFTER the cte-dag section in DOM order; \
         cte-dag at byte {dag_pos}, test-section at byte {test_pos}. \
         The cute-dbt#74/#201 relocation invariant is broken — the test \
         context moved back above the CTE DAG.",
    );
    assert!(
        test_pos < panel_pos,
        "test-section should appear BEFORE panel-row in DOM order; \
         test-section at byte {test_pos}, panel-row at byte {panel_pos}. \
         The cute-dbt#74/#201 relocation invariant is broken — the test \
         context moved below the given/expected panels.",
    );
}

#[then("the resulting HTML file size is under 10 megabytes")]
fn then_under_artifact_size_budget(world: &mut World) {
    let path = world
        .out_path
        .as_ref()
        .expect("subprocess wrote --out path");
    let size = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .len();
    assert!(
        size < REPORT_SIZE_BUDGET_BYTES,
        "report HTML at {} is {} bytes (>= {} byte budget) — too large for a comfortable \
         artifact download in CI sticky-comment review",
        path.display(),
        size,
        REPORT_SIZE_BUDGET_BYTES,
    );
}
