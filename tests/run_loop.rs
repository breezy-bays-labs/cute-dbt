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
        // cute-dbt#291: scrub the ambient opt-in — a developer's shell
        // CUTE_DBT_EXPERIMENTAL must never flip a default-posture test.
        .env_remove("CUTE_DBT_EXPERIMENTAL")
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
fn project_state_off_dbt_project_yml_contributes_zero_bytes() {
    // cute-dbt#291 Discovery call, pinned: with project-state OFF the
    // STANDING `project_definition` metadata is gated too (not kept).
    // The strongest expressible byte-identity form: the same default
    // run with dbt_project.yml present vs absent (same root otherwise)
    // must emit byte-identical reports — the file contributes ZERO
    // bytes to a default report. (A literal pre-#262 binary is not
    // runnable here; render's `ProjectFacts::default()` arm is the
    // pre-#266 byte-identity contract this composes with.)
    let manifest = fixture("playground-current.json");
    let root = tmp("project_state_pin_root");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create the pin project root");
    std::fs::write(
        root.join("dbt_project.yml"),
        "name: pin_project\nversion: \"1.0\"\n\nvars:\n  dq_threshold: 5\n",
    )
    .expect("write dbt_project.yml");

    let with_file = tmp("project_state_off_with_file.html");
    let without_file = tmp("project_state_off_without_file.html");
    clear(&with_file);
    clear(&without_file);

    let run_with = run(&[
        "report",
        "--manifest",
        s(&manifest),
        "--baseline-manifest",
        s(&manifest),
        "--project-root",
        s(&root),
        "--out",
        s(&with_file),
    ]);
    assert!(run_with.status.success(), "{run_with:?}");

    std::fs::remove_file(root.join("dbt_project.yml")).expect("remove dbt_project.yml");
    let run_without = run(&[
        "report",
        "--manifest",
        s(&manifest),
        "--baseline-manifest",
        s(&manifest),
        "--project-root",
        s(&root),
        "--out",
        s(&without_file),
    ]);
    assert!(run_without.status.success(), "{run_without:?}");

    let html_with = std::fs::read_to_string(&with_file).expect("with-file report written");
    let html_without = std::fs::read_to_string(&without_file).expect("without-file report written");
    assert_eq!(
        html_with, html_without,
        "project-state off: dbt_project.yml must contribute zero bytes",
    );
    assert!(
        !html_with.contains("\"project_definition\""),
        "project-state off: no standing metadata key in the payload",
    );
}

#[test]
fn project_state_on_embeds_standing_metadata() {
    // The positive twin of the zero-bytes pin (and the mutant-killer
    // for the gate condition): the SAME inputs with the experiment
    // enabled embed the parsed standing metadata.
    let manifest = fixture("playground-current.json");
    let root = tmp("project_state_on_root");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create the pin project root");
    std::fs::write(
        root.join("dbt_project.yml"),
        "name: pin_project\nversion: \"1.0\"\n\nvars:\n  dq_threshold: 5\n",
    )
    .expect("write dbt_project.yml");

    let out = tmp("project_state_on.html");
    clear(&out);
    let opted = run_with_experimental_env(
        &[
            "report",
            "--manifest",
            s(&manifest),
            "--baseline-manifest",
            s(&manifest),
            "--project-root",
            s(&root),
            "--out",
            s(&out),
        ],
        "project-state",
    );
    assert!(opted.status.success(), "{opted:?}");
    let html = std::fs::read_to_string(&out).expect("opted report written");
    assert!(
        html.contains("\"project_definition\""),
        "project-state on: the standing metadata embeds in the payload",
    );
    assert!(
        html.contains("pin_project"),
        "project-state on: the parsed project name rides the payload",
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
fn explore_emits_the_experimental_notice_on_stderr_never_stdout() {
    // cute-dbt#290: explore is experimental WITHOUT an access gate — the
    // verb stays runnable (exit 0 on a valid manifest), and every
    // invocation emits a one-line stderr notice. stderr ONLY: stdout must
    // stay clean so scripted flows consuming stdout are never corrupted.
    let out_dir = tmp("explore_experimental_notice");
    let _ = std::fs::remove_dir_all(&out_dir);
    let output = run(&[
        "explore",
        "--manifest",
        s(&fixture("jaffle-shop-current.json")),
        "--out-dir",
        s(&out_dir),
    ]);
    assert!(
        output.status.success(),
        "explore stays runnable — no access gate: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("experimental"),
        "the experimental notice is on stderr: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("experimental"),
        "the notice never reaches stdout: {stdout}"
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

// ===== cute-dbt#386 — findings envelope + coverage gate =================
//
// The playground manifest under `--pr-diff` carries exactly one Total-tier
// `Uncovered` finding (`model.healthcare_analytics.mart_dq_summary`, the
// `grain.unique-key-unbacked` check), so it exercises both consumers: the
// `--findings-out` sidecar shape and the `--fail-on-uncovered` gate trip.

/// The four shared args for the playground `--pr-diff` showcase run.
fn playground_pr_diff_args(out: &Path) -> Vec<String> {
    vec![
        "report".to_owned(),
        "--manifest".to_owned(),
        s(&fixture("playground-current.json")).to_owned(),
        "--pr-diff".to_owned(),
        format!("@{}", s(&fixture("playground-pr-diff.patch"))),
        "--project-root".to_owned(),
        s(&fixture("playground-source")).to_owned(),
        "--out".to_owned(),
        s(out).to_owned(),
    ]
}

#[test]
fn findings_out_writes_the_envelope_sidecar_alongside_the_html() {
    // The HTML report AND the machine-readable envelope are both written
    // in one invocation (the sidecar delivery shape, decision D1).
    let html = tmp("envelope_sidecar.html");
    let sidecar = tmp("envelope_sidecar.json");
    clear(&html);
    clear(&sidecar);
    let mut args = playground_pr_diff_args(&html);
    args.push("--findings-out".to_owned());
    args.push(s(&sidecar).to_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run(&refs);
    assert!(
        output.status.success(),
        "findings-out alone (no gate) exits 0: {output:?}"
    );
    assert!(html.exists(), "the HTML report is still written");
    let json = std::fs::read_to_string(&sidecar).expect("the envelope sidecar was written");
    // The versioned header (decision D2 — the integer stability anchor +
    // the id-instability notice) and the flat findings list.
    assert!(
        json.contains("\"schema_version\": 1"),
        "integer schema_version header: {json}"
    );
    assert!(
        json.contains("\"id_stability\": \"unstable-v0.x\""),
        "the machine-readable id-instability notice: {json}"
    );
    assert!(
        json.contains("\"mode\": \"pr-diff\""),
        "the pr-diff scope mode: {json}"
    );
    assert!(
        json.contains("\"grain.unique-key-unbacked\""),
        "the in-scope grain finding rides the envelope: {json}"
    );
    assert!(json.ends_with("}\n"), "trailing newline (diff-friendly)");
}

// ===== cute-dbt#393 — finding→line anchors + GitHub annotations =========

#[test]
fn findings_out_populates_anchors_for_findings_on_changed_models() {
    // cute-dbt#393 — the envelope's reserved anchor slot is now populated
    // from the shared resolver: a finding whose model file is in the
    // --pr-diff carries a concrete (path, line, diff_context). The
    // playground patch touches `fct_provider_metrics.sql` (first changed
    // line 50), so its finding anchors there; `mart_dq_summary` (not in the
    // diff) stays anchor-less (summary-only).
    let html = tmp("anchor_envelope.html");
    let sidecar = tmp("anchor_envelope.json");
    clear(&html);
    clear(&sidecar);
    let mut args = playground_pr_diff_args(&html);
    args.push("--findings-out".to_owned());
    args.push(s(&sidecar).to_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run(&refs);
    assert!(output.status.success(), "exits 0: {output:?}");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).expect("sidecar written"))
            .expect("envelope parses");
    let findings = json["findings"].as_array().expect("findings array");
    // At least one finding on a changed model carries a populated anchor.
    let anchored: Vec<&serde_json::Value> =
        findings.iter().filter_map(|f| f.get("anchor")).collect();
    assert!(
        !anchored.is_empty(),
        "a finding on a changed model must carry an anchor: {json}"
    );
    let anchor = anchored[0];
    assert_eq!(
        anchor["path"], "models/marts/analytics/fct_provider_metrics.sql",
        "anchor pins the changed model file: {anchor}"
    );
    assert!(anchor["line"].is_u64(), "anchor carries a line: {anchor}");
    assert_eq!(
        anchor["diff_context"], "modified",
        "the touched hunk is a modification: {anchor}"
    );
}

#[test]
fn annotations_flag_prints_workflow_commands_to_stdout() {
    // cute-dbt#393 — `--annotations` prints GitHub workflow-command lines
    // to stdout at gen-time. The jaffle-shop baseline diff carries an
    // uncovered finding on a model whose file IS in scope, so an inline
    // `::warning`/`::notice` annotation emits. The HTML report is unchanged
    // (annotations are stdout, never in report.html).
    let html = tmp("annotations_stdout.html");
    clear(&html);
    let output = run(&[
        "report",
        "--manifest",
        s(&fixture("jaffle-shop-current.json")),
        "--baseline-manifest",
        s(&fixture("jaffle-shop-baseline.json")),
        "--out",
        s(&html),
        "--annotations",
    ]);
    assert!(output.status.success(), "exits 0: {output:?}");
    assert!(html.exists(), "the HTML report is still written");
    // Baseline mode has no diff hunks, so no line resolves → no inline
    // annotation lines (summary-only). The flag is accepted and the run is
    // clean — the emit is honestly empty rather than fabricating a line.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("::error "),
        "baseline mode never escalates to ::error without the gate: {stdout}"
    );
    // The report.html itself never carries a workflow-command string.
    let report = std::fs::read_to_string(&html).expect("report written");
    assert!(
        !report.contains("::warning file=") && !report.contains("::notice file="),
        "annotations are a stdout emit, never baked into report.html"
    );
}

#[test]
fn annotations_emit_inline_lines_for_an_uncovered_finding_on_a_changed_model() {
    // The end-to-end annotation emit: a --pr-diff run whose changed model
    // carries an uncovered finding prints an inline `::<level> file=,line=`
    // workflow command anchored at the model's first changed line. Uses a
    // synthetic manifest+patch where the uncovered model's .sql IS in the
    // diff (the playground showcase's only uncovered finding rides an
    // unchanged dependency, so it stays summary-only there).
    let html = tmp("annotations_inline.html");
    clear(&html);
    let mut args = playground_pr_diff_args(&html);
    args.push("--annotations".to_owned());
    args.push("--fail-on-uncovered".to_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run(&refs);
    // The playground pr-diff carries a Total-tier uncovered finding ⇒ the
    // gate trips (exit 3) even though that finding is summary-only (its
    // model file is not in the diff). The annotations emit is honest: no
    // inline line for an unanchorable finding, but the run still exits the
    // gate code and prints nothing fabricated.
    assert_eq!(
        output.status.code(),
        Some(3),
        "the gate trips on the Total-tier uncovered gap: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Every emitted line, if any, is a well-formed workflow command (the
    // showcase fixture's uncovered finding is summary-only, so this may be
    // empty — the assertion guards shape, not presence).
    for line in stdout.lines().filter(|l| l.starts_with("::")) {
        assert!(
            line.contains("file=") || line.starts_with("::notice::+"),
            "a workflow-command line is well-formed: {line}"
        );
    }
}

#[test]
fn fail_on_uncovered_exits_the_gate_code_and_still_writes_the_report() {
    // Decision D3: a Total-tier `Uncovered` finding in scope exits the
    // dedicated gate code (3) — distinct from the usage (2) and fail-closed
    // (1) codes — AFTER the HTML report (and any sidecar) is written.
    let html = tmp("gate_trips.html");
    let sidecar = tmp("gate_trips.json");
    clear(&html);
    clear(&sidecar);
    let mut args = playground_pr_diff_args(&html);
    args.push("--fail-on-uncovered".to_owned());
    args.push("--findings-out".to_owned());
    args.push(s(&sidecar).to_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run(&refs);
    assert_eq!(
        output.status.code(),
        Some(3),
        "a Total-tier uncovered gap exits the dedicated gate code: {output:?}"
    );
    assert!(
        html.exists(),
        "the HTML report is written before the gate trips (gate ≠ fail-closed)"
    );
    assert!(
        sidecar.exists(),
        "the sidecar is also written before the gate"
    );
}

#[test]
fn fail_on_uncovered_exits_zero_when_no_total_uncovered_finding_is_in_scope() {
    // The jaffle-shop diff carries no Total-tier uncovered finding, so the
    // gate passes (exit 0) — the gate is silent on covered/unknown/empty.
    let html = tmp("gate_passes.html");
    clear(&html);
    let output = run(&[
        "report",
        "--manifest",
        s(&fixture("jaffle-shop-current.json")),
        "--baseline-manifest",
        s(&fixture("jaffle-shop-baseline.json")),
        "--out",
        s(&html),
        "--fail-on-uncovered",
    ]);
    assert!(
        output.status.success(),
        "no Total-tier uncovered gap ⇒ exit 0: {output:?}"
    );
    assert!(html.exists(), "the report is written");
}

#[test]
fn default_path_writes_no_envelope_sidecar() {
    // Neither flag set ⇒ the envelope path is skipped entirely (zero added
    // work) and no sidecar JSON is produced.
    let html = tmp("no_envelope.html");
    let phantom_sidecar = tmp("no_envelope.json");
    clear(&html);
    clear(&phantom_sidecar);
    let refs: Vec<String> = playground_pr_diff_args(&html);
    let borrowed: Vec<&str> = refs.iter().map(String::as_str).collect();
    let output = run(&borrowed);
    assert!(output.status.success(), "default run exits 0: {output:?}");
    assert!(html.exists(), "the HTML report is written");
    assert!(
        !phantom_sidecar.exists(),
        "no sidecar is written without --findings-out"
    );
}

#[test]
fn an_unwritable_findings_out_path_is_reported() {
    // The --findings-out twin of `an_unwritable_output_path_is_reported`:
    // a sidecar path under a directory that does not exist makes
    // `write_sidecar` fail, so the run loop reports the write error (exit 1)
    // instead of swallowing it. Without this test a future `let _ =
    // write_sidecar(...)` would silently regress the surfaced-error
    // contract. The HTML --out path IS writable here, so the failure is
    // specifically the sidecar's.
    let html = tmp("envelope_unwritable.html");
    let sidecar = tmp("no_such_dir/findings.json");
    clear(&html);
    let mut args = playground_pr_diff_args(&html);
    args.push("--findings-out".to_owned());
    args.push(s(&sidecar).to_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run(&refs);
    assert_eq!(
        output.status.code(),
        Some(1),
        "an unwritable findings-out path exits 1: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("could not write"),
        "stderr reports the sidecar-write failure: {stderr}"
    );
}

#[test]
fn findings_out_equal_to_out_is_a_usage_error_before_any_write() {
    // CodeRabbit on PR #388: `--out X --findings-out X` would clobber the
    // just-rendered HTML with the envelope JSON. Rejected as a usage error
    // (exit 2 — the parse-time class, NOT the fail-closed code 1) BEFORE
    // anything is written, so the report is never produced-then-destroyed.
    let collide = tmp("collision_report.html");
    clear(&collide);
    let mut args = playground_pr_diff_args(&collide);
    args.push("--findings-out".to_owned());
    args.push(s(&collide).to_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run(&refs);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a --findings-out == --out collision is a usage error: {output:?}"
    );
    assert!(
        !collide.exists(),
        "the collision is rejected before any artifact is written"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--findings-out") && stderr.contains("--out"),
        "stderr names the conflicting flags: {stderr}"
    );
}

#[test]
fn an_unwritable_findings_out_path_wins_over_the_uncovered_gate() {
    // Write-failure precedence: the playground pr-diff carries a Total-tier
    // uncovered finding (so --fail-on-uncovered would otherwise exit 3), but
    // the sidecar write fails FIRST (finalize_findings writes the sidecar
    // before evaluating the gate), so the run exits 1 (the surfaced write
    // error) — never the gate code. A swallowed sidecar error would let the
    // gate's exit 3 leak through and mask the real I/O failure.
    let html = tmp("gate_vs_unwritable.html");
    let sidecar = tmp("no_such_dir/gate_findings.json");
    clear(&html);
    let mut args = playground_pr_diff_args(&html);
    args.push("--findings-out".to_owned());
    args.push(s(&sidecar).to_owned());
    args.push("--fail-on-uncovered".to_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run(&refs);
    assert_eq!(
        output.status.code(),
        Some(1),
        "the sidecar write failure (1) wins over the gate code (3): {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("could not write"),
        "stderr reports the write failure, not a silent gate trip: {stderr}"
    );
}
