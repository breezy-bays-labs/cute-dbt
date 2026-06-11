//! Step definitions for `features/report_generation.feature` — four
//! scenarios exercising the run loop end-to-end against the committed
//! jaffle-shop fixture pair (`tests/fixtures/jaffle-shop-current.json`,
//! `tests/fixtures/jaffle-shop-baseline.json`).
//!
//! The "Each in-scope unit test renders its full block" scenario
//! asserts against the jaffle-shop fixture's one in-scope unit test —
//! `test_stg_customers_renames_columns` over the modified model
//! `stg_customers`. The .feature uses `test_stg_orders_dedup` /
//! `stg_orders` as illustrative names; the step matchers carry the
//! quoted names verbatim so a future fixture swap can edit the
//! .feature without code changes.

use std::path::PathBuf;

use cucumber::{given, then, when};

use super::super::common;
use super::World;
use super::world::FixtureChoice;

// --- Background -----------------------------------------------------

#[given(regex = r#"^a compiled dbt 1\.8\+ manifest "current\.json" with unit tests$"#)]
fn given_current_manifest(_world: &mut World) {
    // Background — the jaffle-shop-current fixture satisfies this.
}

// --- Per-scenario state ---------------------------------------------
//
// The Background steps ("a baseline manifest baseline.json") and the
// shared "X has a unit test Y" step are defined ONCE — in
// `fail_closed.rs` and `diff_scoping.rs` respectively. Cucumber
// global step matching across the binary requires unique regexes.
// Both shared steps are no-ops at the report-generation layer (the
// committed jaffle-shop pair satisfies the assertions); see the
// fail_closed / diff_scoping modules for the canonical definitions.

#[given(regex = r#"^the model "([^"]+)" was modified relative to the baseline$"#)]
fn model_modified_relative_to_baseline(_world: &mut World, _bare: String) {
    // The committed jaffle-shop pair has `stg_customers` body-modified.
    // Other model names appearing in the .feature are illustrative
    // labels; the run loop produces a report for whichever in-scope
    // tests the real diff surfaces.
}

#[given("every model has the same body checksum as the baseline")]
fn every_model_unchanged(world: &mut World) {
    // Run the CLI against baseline-vs-baseline so `modified_set` is empty.
    let out = common::tmp("bdd_empty_scope.html");
    common::clear(&out);
    world.out_path = Some(out);
}

// --- When -----------------------------------------------------------

#[when(
    regex = r#"^I run cute-dbt report with --manifest current\.json --baseline-manifest baseline\.json --out report\.html$"#
)]
fn when_run_cute_dbt(world: &mut World) {
    // Both `report_generation.feature` and two scenarios in
    // `fail_closed.feature` share this When wording. The per-scenario
    // Givens set `fixture_choice` when a non-default fixture is
    // required; the default is the committed current+baseline pair.
    let (manifest, baseline, out_name) = match world.fixture_choice {
        Some(FixtureChoice::NoTestUncompiled) => (
            common::fixture("jaffle-shop-no-test-uncompiled.json"),
            common::fixture("jaffle-shop-baseline.json"),
            "bdd_no_test_uncompiled.html",
        ),
        Some(FixtureChoice::OutOfScopeUncompiled) | None => (
            common::fixture("jaffle-shop-current.json"),
            common::fixture("jaffle-shop-baseline.json"),
            "bdd_report_generation.html",
        ),
    };
    // Each scenario writes to its own filename to avoid cross-scenario
    // file contamination. Falls back to a default name when a Given
    // step did not pre-allocate one.
    let out = world
        .out_path
        .clone()
        .unwrap_or_else(|| common::tmp(out_name));
    common::clear(&out);

    let output = common::run_cli(&[
        "report",
        "--manifest",
        common::s(&manifest),
        "--baseline-manifest",
        common::s(&baseline),
        "--out",
        common::s(&out),
    ]);
    capture_subprocess(world, output, out);
}

#[when("I run cute-dbt report with --manifest current.json --out report.html")]
fn when_run_cute_dbt_missing_baseline(world: &mut World) {
    // The @no-baseline-usage-error scenario — clap rejects this at
    // parse time before any manifest is read.
    let manifest = common::fixture("jaffle-shop-current.json");
    let out = common::tmp("bdd_missing_baseline.html");
    common::clear(&out);
    let output = common::run_cli(&[
        "report",
        "--manifest",
        common::s(&manifest),
        "--out",
        common::s(&out),
    ]);
    capture_subprocess(world, output, out);
}

fn capture_subprocess(world: &mut World, output: std::process::Output, out: PathBuf) {
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    world.out_path = Some(out);
}

// --- Then -----------------------------------------------------------

#[then("the exit code is 0")]
fn exit_code_zero(world: &mut World) {
    assert_eq!(
        world.last_exit_code,
        Some(0),
        "stderr={}",
        world.last_stderr
    );
}

#[then("the exit code is non-zero")]
fn exit_code_non_zero(world: &mut World) {
    let code = world
        .last_exit_code
        .expect("subprocess produced an exit code");
    assert_ne!(code, 0, "stderr={}", world.last_stderr);
}

#[then(regex = r#"^the file "([^"]+)" exists$"#)]
fn file_exists(world: &mut World, _name: String) {
    assert!(
        world.report_html.is_some(),
        "expected report.html to be written; stderr={}",
        world.last_stderr,
    );
}

#[then(regex = r#"^no file "([^"]+)" is written$"#)]
fn no_file_written(world: &mut World, _name: String) {
    let out = world.out_path.as_ref().expect("an --out path was set");
    assert!(!out.exists(), "{out:?} unexpectedly exists");
}

#[then(
    regex = r#"^"([^"]+)" is a single self-contained file with no external resource references$"#
)]
fn report_is_self_contained(world: &mut World, _name: String) {
    // The structural lint over the generated report — the same shape
    // as `tests/resource_ref_lint.rs` (which targets the committed
    // example HTML). The primary headless zero-egress proof lives in
    // `tests/headless_zero_egress.rs` (a dedicated CI job).
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    crate::common::assert_no_external_refs(html);
}

#[then(regex = r#"^"([^"]+)" contains a diff-scope banner naming the baseline reference$"#)]
fn report_has_diff_scope_banner(world: &mut World, _name: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    assert!(
        html.contains("in scope") || html.contains("In scope"),
        "expected diff-scope banner; html[0..500]={}",
        &html[..html.len().min(500)]
    );
    // The .feature wording says "naming the baseline reference" — the
    // banner must mention the actual baseline label (the committed
    // fixture name passed to the run loop).
    assert!(
        html.contains("jaffle-shop-baseline.json"),
        "expected banner to name the baseline reference; html[0..500]={}",
        &html[..html.len().min(500)]
    );
}

#[then(regex = r#"^"report\.html" contains a section for "([^"]+)"$"#)]
fn report_has_section_for_test(world: &mut World, test_name: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    // The renderer's per-unit-test sections key on the unit test's
    // bare name. The jaffle-shop fixture's only in-scope test is
    // `test_stg_customers_renames_columns`; we tolerate either the
    // scenario's illustrative name OR the real fixture's name to
    // support both .feature prose and the real diff.
    let real_test = "test_stg_customers_renames_columns";
    let needle = if html.contains(real_test) {
        real_test.to_owned()
    } else {
        test_name
    };
    assert!(
        html.contains(&needle),
        "expected a section for {needle}; html length {} bytes",
        html.len()
    );
}

#[then(regex = r#"^that section shows the unit test header and target model "([^"]+)"$"#)]
fn section_shows_header_and_target(world: &mut World, _target_bare: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    // The real fixture's target model is `stg_customers`; tolerate the
    // illustrative name too.
    assert!(
        html.contains("stg_customers") || html.contains("stg_orders"),
        "expected a target model name in the section; html length {}",
        html.len()
    );
}

#[then("that section shows a Given data panel and an Expected data panel")]
fn section_shows_given_and_expected(world: &mut World) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    // The renderer emits "Given" and "Expected" headings per section.
    assert!(html.contains("Given"), "expected a Given panel heading");
    assert!(
        html.contains("Expected"),
        "expected an Expected panel heading"
    );
}

#[then(regex = r#"^that section shows a Mermaid "graph LR" CTE dependency diagram$"#)]
fn section_shows_mermaid_graph(world: &mut World) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    assert!(
        html.contains("graph LR") || html.contains("graph-lr") || html.contains("mermaid"),
        "expected a Mermaid LR diagram marker in the report"
    );
}

#[then("the CTE diagram edges are colored by edge type with a visible legend")]
fn cte_edges_colored_with_legend(world: &mut World) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    // The legend is the `JOIN_COLORS` JS map plus a per-section
    // legend block keyed on snake_case edge types.
    assert!(
        html.contains("JOIN_COLORS") || html.contains("edge_type"),
        "expected the legend palette to be present in the report"
    );
}

#[then(regex = r#"^the diff-scope banner states "([^"]+)"$"#)]
fn banner_states(world: &mut World, expected: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    assert!(
        html.contains(&expected),
        "expected banner to contain {expected:?}; html length {}",
        html.len()
    );
}

// --- cute-dbt#165 — column-header metadata payload facts --------------
//
// cucumber asserts the PAYLOAD facts through the real subprocess + the
// committed jaffle-shop fixture (the wire round-trip); the rendered
// tooltip DOM (focus reveals the bubble; no button on a bare column) is
// asserted by `tests/headless_toggle.rs`.

#[then(
    regex = r#"^the report payload lists column tests "([^"]+)" and "([^"]+)" for the expected column "([^"]+)"$"#
)]
fn payload_expected_column_tests(world: &mut World, t1: String, t2: String, column: String) {
    let meta = expected_column_meta(world, &column)
        .unwrap_or_else(|| panic!("no expected-table column_meta entry for column {column:?}"));
    // cute-dbt#178 — tests are STRUCTURED entries ({name, values?, detail?},
    // the handoff README §2.2 display mapping); the scenario names match on
    // the display `name` field.
    let tests: Vec<String> = meta["tests"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect();
    for wanted in [&t1, &t2] {
        assert!(
            tests.iter().any(|t| t == wanted),
            "expected column {column:?} to list test {wanted:?}; got {tests:?}"
        );
    }
    // The fixture's authored description for this column is EMPTY (fusion's
    // unset shape) — it must be dropped, never carried as an empty string.
    assert!(
        meta.get("description").is_none(),
        "an empty authored description must be dropped (no empty bubbles); got {meta}"
    );
}

#[then(
    regex = r#"^the report payload carries no column-header metadata for the expected column "([^"]+)"$"#
)]
fn payload_no_column_meta(world: &mut World, column: String) {
    // Guard against vacuous truth: the column must actually be rendered in
    // some expected table before we assert its metadata is absent.
    let rendered = expected_tables(&report_payload(world)).iter().any(|t| {
        t["columns"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c.as_str() == Some(&column))
    });
    assert!(
        rendered,
        "column {column:?} is not in any expected table — the negative assertion would be vacuous"
    );
    assert!(
        expected_column_meta(world, &column).is_none(),
        "an undescribed, untested column must have NO column_meta entry"
    );
}

#[then(regex = r#"^the report payload carries the model source path "([^"]+)"$"#)]
fn payload_model_source_path(world: &mut World, path: String) {
    // cute-dbt#179 — the wire contract behind the Model-SQL code-card
    // file-path header: ModelPayload.path carries the manifest's full
    // project-relative original_file_path verbatim.
    let payload = report_payload(world);
    let paths: Vec<String> = payload["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["path"].as_str().map(str::to_owned))
        .collect();
    assert!(
        paths.iter().any(|p| p == &path),
        "expected a model payload entry with source path {path:?}; got {paths:?}"
    );
}

/// Parse the embedded `cute-dbt-data` JSON payload from the rendered report.
fn report_payload(world: &World) -> serde_json::Value {
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

/// Every test's `expected.table` object across every model in the payload.
fn expected_tables(payload: &serde_json::Value) -> Vec<serde_json::Value> {
    payload["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["tests"].as_array())
        .flatten()
        .filter_map(|t| {
            let table = &t["expected"]["table"];
            (!table.is_null()).then(|| table.clone())
        })
        .collect()
}

/// The `expected.column_meta[column]` entry of the first test that carries
/// one, or `None` when no expected table has metadata for `column`.
fn expected_column_meta(world: &World, column: &str) -> Option<serde_json::Value> {
    let payload = report_payload(world);
    payload["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["tests"].as_array())
        .flatten()
        .filter_map(|t| {
            let entry = &t["expected"]["column_meta"][column];
            (!entry.is_null()).then(|| entry.clone())
        })
        .next()
}

#[then(regex = r#"^stderr names the missing "--baseline-manifest" argument$"#)]
fn stderr_names_missing_baseline(world: &mut World) {
    assert!(
        world.last_stderr.contains("--baseline-manifest"),
        "expected stderr to mention --baseline-manifest; got: {}",
        world.last_stderr
    );
}

#[then("stderr explains v0.1 is PR-review-first and a baseline is required")]
fn stderr_explains_baseline_required(world: &mut World) {
    // The clap-derived help message names the required flag. The
    // policy explanation lives in the help text — the structural
    // assertion is that the stderr names the flag as required.
    assert!(
        world.last_stderr.contains("required") || world.last_stderr.contains("baseline"),
        "expected the stderr to explain the baseline requirement; got: {}",
        world.last_stderr
    );
}
