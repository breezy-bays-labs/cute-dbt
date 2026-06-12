//! End-to-end coverage of the cute-dbt run loop — PR 6's two-stage
//! fail-closed contract and the baseline-required policy.
//!
//! Each test spawns the compiled `cute-dbt` binary
//! (`CARGO_BIN_EXE_cute-dbt`) with a real argument set and asserts the
//! exit code, stderr, and whether `report.html` was written.
//! `cargo llvm-cov nextest` instruments the subprocess, so these tests
//! also cover `cli::run` and `main`.
//!
//! Maps `fail_closed.feature` (SC3) and `report_generation.feature`'s
//! empty-scope + missing-baseline scenarios; the cucumber wiring lands
//! in PR 10.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Absolute path to a committed fixture under `tests/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A path inside the cargo-provided integration-test temp directory.
fn tmp(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

/// Best-effort delete so a re-run starts from a known-absent file.
fn clear(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Run the `cute-dbt` binary with `args`; return its captured output.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args(args)
        .output()
        .expect("the cute-dbt binary spawns")
}

/// Run the `cute-dbt` binary with `args` and `CUTE_DBT_EXPERIMENTAL`
/// set to `value` (cute-dbt#289). Subprocess env is the only safe way
/// to exercise clap's env-fallback — process env is global state and
/// `unsafe_code = "forbid"` rules out `std::env::set_var` in-process.
fn run_with_experimental_env(args: &[&str], value: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args(args)
        .env("CUTE_DBT_EXPERIMENTAL", value)
        .output()
        .expect("the cute-dbt binary spawns")
}

/// Stringify a path argument (every test path is valid UTF-8).
fn s(path: &Path) -> &str {
    path.to_str().expect("test paths are valid UTF-8")
}

#[test]
fn empty_scope_writes_a_valid_report_with_the_banner() {
    // report_generation.feature — "A change touching no models yields an
    // empty but valid report": baseline-vs-baseline modifies nothing.
    let baseline = fixture("jaffle-shop-baseline.json");
    let out = tmp("empty_scope.html");
    clear(&out);
    let output = run(&[
        "report",
        "--manifest",
        s(&baseline),
        "--baseline-manifest",
        s(&baseline),
        "--out",
        s(&out),
    ]);
    assert!(output.status.success(), "empty scope exits 0: {output:?}");
    let html = std::fs::read_to_string(&out).expect("report.html was written");
    assert!(
        html.contains("0 unit tests in scope"),
        "the empty-scope banner is present: {html}",
    );
}

#[test]
fn a_non_empty_diff_writes_a_report() {
    // current vs baseline: stg_customers' body changed, so its unit test
    // is in scope; every in-scope model has compiled SQL => exit 0.
    let out = tmp("non_empty.html");
    clear(&out);
    let output = run(&[
        "report",
        "--manifest",
        s(&fixture("jaffle-shop-current.json")),
        "--baseline-manifest",
        s(&fixture("jaffle-shop-baseline.json")),
        "--out",
        s(&out),
    ]);
    assert!(
        output.status.success(),
        "non-empty diff exits 0: {output:?}"
    );
    let html = std::fs::read_to_string(&out).expect("report.html was written");
    assert!(
        html.contains("in scope"),
        "the diff-scope banner is present: {html}"
    );
}

#[test]
fn an_unknown_experimental_env_value_is_a_usage_error() {
    // cute-dbt#289: CUTE_DBT_EXPERIMENTAL fails closed exactly like the
    // [experimental] TOML arm — an unknown id is a clap usage error
    // (exit 2) with remediation naming the closed vocabulary, raised
    // before any manifest is read.
    let baseline = fixture("jaffle-shop-baseline.json");
    let out = tmp("experimental_bogus_env.html");
    clear(&out);
    let output = run_with_experimental_env(
        &[
            "report",
            "--manifest",
            s(&baseline),
            "--baseline-manifest",
            s(&baseline),
            "--out",
            s(&out),
        ],
        "projcet-state",
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "an unknown experiment id is a usage error: {output:?}"
    );
    assert!(!out.exists(), "no report.html is written on a usage error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("projcet-state"),
        "stderr names the offending entry: {stderr}",
    );
    assert!(
        stderr.contains("project-state"),
        "stderr names the known experiment ids: {stderr}",
    );
}

#[test]
fn an_enabled_experimental_env_value_changes_nothing_in_this_slice() {
    // cute-dbt#289 is mechanism-only: the resolved set threads through
    // the run loop but nothing consumes it yet, so an opted-in render
    // is byte-identical to the default. The first consumer is the
    // project-state gate (cute-dbt#291) — on fixtures that emit
    // project-state surfaces this assertion legitimately flips there
    // (the DEFAULT side will drop them); this fixture has no
    // dbt_project.yml beside it, so the equality should outlive #291.
    let baseline = fixture("jaffle-shop-baseline.json");
    let default_out = tmp("experimental_default.html");
    let opted_out_path = tmp("experimental_opted.html");
    clear(&default_out);
    clear(&opted_out_path);
    let default_run = run(&[
        "report",
        "--manifest",
        s(&baseline),
        "--baseline-manifest",
        s(&baseline),
        "--out",
        s(&default_out),
    ]);
    assert!(default_run.status.success(), "{default_run:?}");
    let opted_run = run_with_experimental_env(
        &[
            "report",
            "--manifest",
            s(&baseline),
            "--baseline-manifest",
            s(&baseline),
            "--out",
            s(&opted_out_path),
        ],
        "1",
    );
    assert!(opted_run.status.success(), "{opted_run:?}");
    let default_html = std::fs::read_to_string(&default_out).expect("default report written");
    let opted_html = std::fs::read_to_string(&opted_out_path).expect("opted report written");
    assert_eq!(
        default_html, opted_html,
        "the switch is mechanism-only in cute-dbt#289: no byte changes",
    );
}

#[test]
fn the_experimental_env_var_is_inert_on_explore() {
    // The founder call (epic #288): explore ships ungated. The env var
    // is read through a report-only clap arg, so even a bogus value
    // must not fail the explore verb.
    let out_dir = tmp("experimental_explore_out");
    let output = run_with_experimental_env(
        &[
            "explore",
            "--manifest",
            s(&fixture("jaffle-shop-current.json")),
            "--out-dir",
            s(&out_dir),
        ],
        "definitely-not-an-experiment",
    );
    assert!(
        output.status.success(),
        "explore ignores CUTE_DBT_EXPERIMENTAL entirely: {output:?}"
    );
    assert!(
        out_dir.join("dag.html").exists(),
        "the explorer pages were written"
    );
}

#[test]
fn a_parse_only_manifest_fails_closed_naming_the_node() {
    // fail_closed.feature — a parse-only manifest whose in-scope target
    // model has compiled_code null. parse-only's stg_customers checksum
    // differs from the baseline, so stg_customers is in scope; its
    // compiled_code is null => NotCompiled.
    let out = tmp("parse_only.html");
    clear(&out);
    let output = run(&[
        "report",
        "--manifest",
        s(&fixture("jaffle-shop-parse-only.json")),
        "--baseline-manifest",
        s(&fixture("jaffle-shop-baseline.json")),
        "--out",
        s(&out),
    ]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "fail-closed exits 1: {output:?}"
    );
    assert!(!out.exists(), "no report.html is written on fail-closed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("model.jaffle_shop.stg_customers"),
        "stderr names the offending node: {stderr}",
    );
    assert!(
        stderr.contains("dbt compile") || stderr.contains("dbt run"),
        "stderr recommends compiling: {stderr}",
    );
}

#[test]
fn a_missing_baseline_manifest_is_a_usage_error() {
    // The locked baseline-required policy: omitting --baseline-manifest
    // is a clap usage error (exit 2), not a PreflightError.
    let out = tmp("missing_baseline.html");
    clear(&out);
    let output = run(&[
        "report",
        "--manifest",
        s(&fixture("jaffle-shop-current.json")),
        "--out",
        s(&out),
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "usage error exits 2: {output:?}"
    );
    assert!(!out.exists(), "no report.html is written");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--baseline-manifest"),
        "stderr names the missing argument: {stderr}",
    );
}

#[test]
fn an_unreadable_manifest_fails_closed() {
    let bad = tmp("unreadable_manifest.json");
    std::fs::write(&bad, "this is not json").expect("write the bad fixture");
    let out = tmp("unreadable.html");
    clear(&out);
    let output = run(&[
        "report",
        "--manifest",
        s(&bad),
        "--baseline-manifest",
        s(&fixture("jaffle-shop-baseline.json")),
        "--out",
        s(&out),
    ]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "fail-closed exits 1: {output:?}"
    );
    assert!(!out.exists(), "no report.html is written");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unreadable"),
        "stderr explains the manifest could not be read: {stderr}",
    );
}

#[test]
fn a_pre_1_8_manifest_fails_closed_at_the_schema_gate() {
    let old = tmp("schema_v11.json");
    std::fs::write(
        &old,
        r#"{"metadata":{"dbt_schema_version":"https://schemas.getdbt.com/dbt/manifest/v11.json"}}"#,
    )
    .expect("write the pre-1.8 fixture");
    let out = tmp("schema_v11.html");
    clear(&out);
    let output = run(&[
        "report",
        "--manifest",
        s(&old),
        "--baseline-manifest",
        s(&fixture("jaffle-shop-baseline.json")),
        "--out",
        s(&out),
    ]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "fail-closed exits 1: {output:?}"
    );
    assert!(!out.exists(), "no report.html is written");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("v12") || stderr.contains("1.8"),
        "stderr states the minimum supported dbt version: {stderr}",
    );
}

#[test]
fn an_unusable_baseline_fails_closed() {
    let out = tmp("unusable_baseline.html");
    clear(&out);
    let output = run(&[
        "report",
        "--manifest",
        s(&fixture("jaffle-shop-current.json")),
        "--baseline-manifest",
        s(&tmp("does-not-exist-baseline.json")),
        "--out",
        s(&out),
    ]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "fail-closed exits 1: {output:?}"
    );
    assert!(!out.exists(), "no report.html is written");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("baseline"),
        "stderr explains the baseline manifest could not be used: {stderr}",
    );
}

#[test]
fn an_unwritable_output_path_is_reported() {
    // --out under a directory that does not exist: fs::write fails, so
    // the run loop reports a clear error instead of panicking.
    let out = tmp("no_such_dir/report.html");
    let output = run(&[
        "report",
        "--manifest",
        s(&fixture("jaffle-shop-baseline.json")),
        "--baseline-manifest",
        s(&fixture("jaffle-shop-baseline.json")),
        "--out",
        s(&out),
    ]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "an output failure exits 1: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("could not write"),
        "stderr reports the output-write failure: {stderr}",
    );
}

#[test]
fn a_modified_model_with_zero_unit_tests_and_no_compiled_sql_fails_closed() {
    // fail_closed.feature — None-shape NotCompiled: a modified model that
    // has zero unit tests targeting it in the current manifest but whose
    // compiled_code is null. The error must name the model node id but
    // must NOT name a unit test (the None arm of NotCompiled::display_for).
    let out = tmp("no_test_uncompiled.html");
    clear(&out);
    let output = run(&[
        "report",
        "--manifest",
        s(&fixture("jaffle-shop-no-test-uncompiled.json")),
        "--baseline-manifest",
        s(&fixture("jaffle-shop-baseline.json")),
        "--out",
        s(&out),
    ]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "fail-closed exits 1: {output:?}"
    );
    assert!(!out.exists(), "no report.html is written on fail-closed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("model.jaffle_shop.stg_orders"),
        "stderr names the not-compiled model: {stderr}",
    );
    assert!(
        !stderr.contains("unit test"),
        "stderr must not mention a unit test for the no-test case: {stderr}",
    );
    assert!(
        stderr.contains("dbt compile") || stderr.contains("dbt run"),
        "stderr recommends compiling: {stderr}",
    );
}

#[test]
fn help_exits_zero() {
    let output = run(&["--help"]);
    assert!(output.status.success(), "--help exits 0: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cute-dbt"), "help text is shown: {stdout}");
}

#[test]
fn no_arguments_is_a_usage_error_listing_both_subcommands() {
    // The cute-dbt#100 CLI restructure: bare `cute-dbt` is a usage error
    // (exit 2, never the help-on-missing display that can exit 0), and
    // the error names BOTH verbs so the operator can self-serve. The
    // clap ErrorKind itself (`MissingSubcommand`) is pinned by the
    // src/cli/args.rs unit test; this asserts the process-level mapping.
    let output = run(&[]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a missing subcommand is a usage error: {output:?}",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("report") && stderr.contains("explore"),
        "the usage error lists both subcommands: {stderr}",
    );
}

#[test]
fn flat_pre_verb_invocation_is_a_usage_error() {
    // The pre-#100 flat surface must not silently keep working: flags
    // without a verb are rejected at parse time (deliberate v0.x break).
    let output = run(&[
        "--manifest",
        s(&fixture("jaffle-shop-current.json")),
        "--baseline-manifest",
        s(&fixture("jaffle-shop-baseline.json")),
        "--out",
        s(&tmp("flat_invocation.html")),
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "the flat pre-verb shape is a usage error: {output:?}",
    );
}
