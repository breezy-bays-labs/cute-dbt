//! Step definitions for `features/config.feature` — the operator-supplied
//! `--config <PATH>` flag landed in PR 14 (cute-dbt#24).
//!
//! Five scenarios exercise the run loop end-to-end against the committed
//! jaffle-shop fixture pair (`tests/fixtures/jaffle-shop-current.json`,
//! `tests/fixtures/jaffle-shop-baseline.json`) plus the new
//! `tests/fixtures/config-*.toml` fixtures.
//!
//! `Background` re-uses the shared "current.json" / baseline.json"
//! Given steps defined once (in `report_generation.rs` and
//! `fail_closed.rs`) — cucumber-rs requires step regexes to be unique
//! across the binary, so the per-feature module owns only the steps
//! that are NEW to that feature.

use std::path::PathBuf;

use cucumber::{given, then, when};

use super::super::common;
use super::World;

// --- Given ----------------------------------------------------------

#[given(regex = r#"^a config file "([^"]+)"$"#)]
fn given_config_fixture(_world: &mut World, _name: String) {
    // The fixture is committed under `tests/fixtures/`; the When step
    // resolves the absolute path. No-op here — the assertion belongs
    // with the subprocess invocation, not with this Given.
}

// --- When -----------------------------------------------------------
//
// One When per fixture combination + one for the missing-file path.
// We can't share the existing report_generation `When` regex because
// the `--config <PATH>` suffix is part of the cucumber Gherkin literal
// (not a parameter), and cucumber matches the LONGEST regex first only
// when patterns differ; tying scenario → step via a literal config
// filename keeps the matching unambiguous.

#[when(
    regex = r#"^I run cute-dbt report with --manifest current\.json --baseline-manifest baseline\.json --out report\.html --config ([^ ]+)$"#
)]
fn when_run_cute_dbt_with_config(world: &mut World, config_name: String) {
    let manifest = common::fixture("jaffle-shop-current.json");
    let baseline = common::fixture("jaffle-shop-baseline.json");
    let out_name = format!("bdd_config_{}.html", config_name.replace(['.', '/'], "_"),);
    let out = common::tmp(&out_name);
    common::clear(&out);

    // The fixture path is resolved against `tests/fixtures/` for every
    // file that DOES exist; non-existent fixtures retain the supplied
    // filename so the value-parser raises the Io variant.
    let config_path = if config_name == "does-not-exist.toml" {
        common::tmp(&config_name)
    } else {
        common::fixture(&config_name)
    };

    let output = common::run_cli(&[
        "report",
        "--manifest",
        common::s(&manifest),
        "--baseline-manifest",
        common::s(&baseline),
        "--out",
        common::s(&out),
        "--config",
        common::s(&config_path),
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

#[then(regex = r#"^"report\.html" contains a <title> element with "([^"]+)"$"#)]
fn report_title_contains(world: &mut World, expected: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let needle = format!("<title>{expected}</title>");
    assert!(
        html.contains(&needle),
        "expected <title> with {expected:?}; html[0..400]={}",
        &html[..html.len().min(400)]
    );
}

#[then(regex = r#"^"report\.html" contains an <h1> element with "([^"]+)"$"#)]
fn report_h1_contains(world: &mut World, expected: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let needle = format!("<h1>{expected}</h1>");
    assert!(
        html.contains(&needle),
        "expected <h1> with {expected:?}; html length {}",
        html.len()
    );
}

#[then(regex = r#"^"report\.html" contains a <p class="report-subtitle"> element with "([^"]+)"$"#)]
fn report_subtitle_contains(world: &mut World, expected: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let needle = format!("<p class=\"report-subtitle\">{expected}</p>");
    assert!(
        html.contains(&needle),
        "expected subtitle paragraph with {expected:?}; html length {}",
        html.len()
    );
}

#[then(r#""report.html" does NOT contain a <p class="report-subtitle"> element"#)]
fn report_omits_subtitle(world: &mut World) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    assert!(
        !html.contains("class=\"report-subtitle\""),
        "expected NO subtitle element; html length {}",
        html.len()
    );
}

#[then("stderr explains the config file could not be read")]
fn stderr_explains_read_failure(world: &mut World) {
    assert!(
        world.last_stderr.contains("could not read config file"),
        "expected stderr to explain the read failure; got: {}",
        world.last_stderr
    );
}

#[then("stderr explains the config file could not be parsed")]
fn stderr_explains_parse_failure(world: &mut World) {
    assert!(
        world.last_stderr.contains("invalid TOML in config file"),
        "expected stderr to explain the parse failure; got: {}",
        world.last_stderr
    );
}
