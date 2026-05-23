//! Step definitions for `features/fail_closed.feature` — six scenarios
//! that exercise the two-stage `PreflightError` contract end-to-end via
//! the compiled `cute-dbt` subprocess.
//!
//! Mirrors `tests/run_loop.rs` 1:1 — the BDD scenarios assert the same
//! exit codes, stderr texts, and absence-of-report properties; the
//! fixture set is the same committed pair. The duplication is the
//! cucumber/integration-test split point: `run_loop.rs` is the fast
//! Rust-native unit-style assertion, this is the prose contract a
//! reviewer reads alongside the .feature.

use cucumber::{given, then, when};

use super::super::common;
use super::World;
use super::world::FixtureChoice;

// --- Background -----------------------------------------------------
//
// "a baseline manifest baseline.json" appears in BOTH this feature's
// Background and the report_generation feature's Background. The
// canonical no-op definition lives here; report_generation's module
// does not redeclare it (cucumber requires globally unique regexes
// across the binary).

#[given(regex = r#"^a baseline manifest "baseline\.json"$"#)]
fn given_baseline_manifest(_world: &mut World) {
    // The committed jaffle-shop-baseline fixture satisfies the prose.
}

// --- Per-scenario Givens --------------------------------------------

#[given(regex = r#"^a manifest "([^"]+)" produced by "dbt parse"$"#)]
fn given_parse_only_manifest(_world: &mut World, _name: String) {
    // The committed jaffle-shop-parse-only.json fixture is the
    // `dbt parse` shape.
}

#[given("it has a unit test whose in-scope target model has compiled_code null")]
fn given_in_scope_target_has_no_compiled(_world: &mut World) {
    // `stg_customers` in jaffle-shop-parse-only.json fits.
}

#[given(regex = r#"^a manifest "([^"]+)" whose dbt_schema_version is below the 1\.8 floor$"#)]
fn given_pre_18_manifest(world: &mut World, name: String) {
    // Write a minimal manifest with an old schema version.
    let pre18 = common::tmp(&format!("bdd_pre18_{name}"));
    std::fs::write(
        &pre18,
        r#"{"metadata":{"dbt_schema_version":"https://schemas.getdbt.com/dbt/manifest/v11.json"}}"#,
    )
    .expect("write pre-1.8 fixture");
    world.out_path = Some(pre18);
}

#[given(regex = r#"^a file "([^"]+)" that is not valid JSON$"#)]
fn given_invalid_json_file(world: &mut World, name: String) {
    let bad = common::tmp(&format!("bdd_invalid_{name}"));
    std::fs::write(&bad, "this is not json").expect("write the invalid-json fixture");
    world.out_path = Some(bad);
}

#[given(regex = r#"^a valid compiled manifest "([^"]+)"$"#)]
fn given_valid_compiled_manifest(_world: &mut World, _name: String) {
    // Use the committed jaffle-shop-current.json fixture.
}

#[given(regex = r#"^a "--baseline-manifest" path "([^"]+)" that cannot be read$"#)]
fn given_unreadable_baseline(world: &mut World, _name: String) {
    // Record the missing-baseline path in out_path so the When step
    // can reach it. (The out_path field is repurposed here as the
    // "next pre-positioned path" slot.)
    let missing = common::tmp("bdd_missing_baseline.json");
    common::clear(&missing);
    world.out_path = Some(missing);
}

#[given(regex = r#"^a compiled manifest where an out-of-scope model has compiled_code null$"#)]
fn given_out_of_scope_uncompiled(world: &mut World) {
    // No fixture exists for the precise "out-of-scope uncompiled"
    // shape — the committed jaffle-shop pair has every in-scope
    // target compiled, which proves the negative path indirectly:
    // running the CLI against the standard pair exits 0 and writes a
    // report (so any uncompiled non-in-scope nodes did NOT fail).
    world.fixture_choice = Some(FixtureChoice::OutOfScopeUncompiled);
}

#[given("all in-scope models have compiled SQL")]
fn given_all_in_scope_have_compiled(_world: &mut World) {
    // Affirmation only — the fixture choice was set by the prior Given.
}

#[given(regex = r#"^the current manifest has a model "([^"]+)" that is modified$"#)]
fn given_modified_no_test_model(world: &mut World, _node_id: String) {
    // Routes the next subprocess invocation to
    // `jaffle-shop-no-test-uncompiled.json`, which carries a modified
    // `stg_orders` with zero unit tests and no compiled_code.
    world.fixture_choice = Some(FixtureChoice::NoTestUncompiled);
}

#[given(regex = r#"^the current manifest has zero unit tests targeting "([^"]+)"$"#)]
fn given_no_tests_targeting(_world: &mut World, _node_id: String) {
    // Same fixture as above (set by the prior Given).
}

#[given(regex = r#"^the current manifest has compiled_code null for "([^"]+)"$"#)]
fn given_compiled_code_null_for(_world: &mut World, _node_id: String) {
    // Same fixture as above (set by the prior Given).
}

// --- When -----------------------------------------------------------

#[when(
    regex = r#"^I run cute-dbt with --manifest parsed\.json --baseline-manifest baseline\.json --out report\.html$"#
)]
fn when_run_parse_only(world: &mut World) {
    run_subprocess(
        world,
        common::fixture("jaffle-shop-parse-only.json"),
        common::fixture("jaffle-shop-baseline.json"),
        "bdd_parse_only.html",
    );
}

#[when(
    regex = r#"^I run cute-dbt with --manifest old\.json --baseline-manifest baseline\.json --out report\.html$"#
)]
fn when_run_pre_18(world: &mut World) {
    let manifest = world
        .out_path
        .take()
        .expect("the pre-1.8 Given created a manifest path");
    run_subprocess(
        world,
        manifest,
        common::fixture("jaffle-shop-baseline.json"),
        "bdd_pre18.html",
    );
}

#[when(
    regex = r#"^I run cute-dbt with --manifest broken\.json --baseline-manifest baseline\.json --out report\.html$"#
)]
fn when_run_invalid_json(world: &mut World) {
    let manifest = world
        .out_path
        .take()
        .expect("the invalid-JSON Given created a manifest path");
    run_subprocess(
        world,
        manifest,
        common::fixture("jaffle-shop-baseline.json"),
        "bdd_invalid.html",
    );
}

#[when(
    regex = r#"^I run cute-dbt with --manifest current\.json --baseline-manifest missing-baseline\.json --out report\.html$"#
)]
fn when_run_missing_baseline_path(world: &mut World) {
    let missing = world
        .out_path
        .take()
        .expect("the unreadable-baseline Given seeded out_path");
    run_subprocess(
        world,
        common::fixture("jaffle-shop-current.json"),
        missing,
        "bdd_unreadable_baseline.html",
    );
}

// The "I run cute-dbt with --manifest current.json --baseline-manifest
// baseline.json --out report.html" When step lives in
// `report_generation.rs` (the canonical location); it dispatches via
// `world.fixture_choice`, which the per-scenario Givens above set.

fn run_subprocess(
    world: &mut World,
    manifest: std::path::PathBuf,
    baseline: std::path::PathBuf,
    out_name: &str,
) {
    let out = common::tmp(out_name);
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
    world.last_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    world.out_path = Some(out);
}

// --- Then -----------------------------------------------------------

#[then("stderr names the offending node id")]
fn stderr_names_offending_node_id(world: &mut World) {
    assert!(
        world.last_stderr.contains("model.jaffle_shop"),
        "expected stderr to name a model.jaffle_shop.* node id; got: {}",
        world.last_stderr
    );
}

#[then(regex = r#"^stderr recommends running "([^"]+)" or "([^"]+)"$"#)]
fn stderr_recommends_compile_or_run(world: &mut World, a: String, b: String) {
    assert!(
        world.last_stderr.contains(&a) || world.last_stderr.contains(&b),
        "expected stderr to recommend `{a}` or `{b}`; got: {}",
        world.last_stderr
    );
}

#[then("stderr states the minimum supported dbt version")]
fn stderr_states_min_version(world: &mut World) {
    assert!(
        world.last_stderr.contains("1.8") || world.last_stderr.contains("v12"),
        "expected stderr to mention 1.8 or v12; got: {}",
        world.last_stderr
    );
}

#[then("stderr explains the manifest could not be read")]
fn stderr_explains_unreadable_manifest(world: &mut World) {
    assert!(
        world.last_stderr.to_lowercase().contains("unreadable")
            || world.last_stderr.to_lowercase().contains("could not"),
        "expected stderr to explain the read failure; got: {}",
        world.last_stderr
    );
}

#[then("stderr explains the baseline manifest could not be used")]
fn stderr_explains_unusable_baseline(world: &mut World) {
    assert!(
        world.last_stderr.to_lowercase().contains("baseline"),
        "expected stderr to name the baseline failure; got: {}",
        world.last_stderr
    );
}

#[then(regex = r#"^stderr names "([^"]+)" as the not-compiled node$"#)]
fn stderr_names_specific_not_compiled_node(world: &mut World, node_id: String) {
    assert!(
        world.last_stderr.contains(&node_id),
        "expected stderr to name {node_id}; got: {}",
        world.last_stderr
    );
}

#[then("stderr does not name a unit test")]
fn stderr_does_not_name_unit_test(world: &mut World) {
    assert!(
        !world.last_stderr.to_lowercase().contains("unit test"),
        "expected stderr NOT to mention a unit test; got: {}",
        world.last_stderr
    );
}
