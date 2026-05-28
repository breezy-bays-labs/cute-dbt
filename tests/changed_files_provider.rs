//! End-to-end subprocess contract for `--scope-from-pr-diff @file`
//! (cute-dbt#85).
//!
//! This binary owns the *process-level* contract that unit tests cannot
//! exercise: a missing `@file` is a clap usage error (exit 2, no report
//! written), an empty list renders a zero-scope report (exit 0), and a
//! file with CRLF / blank-line / whitespace noise is read without error
//! (exit 0). The *exact* parse result (which paths survive trimming,
//! unicode handling, comma-vs-newline splitting) is pinned by the unit
//! suite in `cli::pr_diff`; scope-selection correctness (which models
//! land in scope) is pinned by `src/domain/scope.rs` + the
//! `pr_diff_scoping.feature` BDD (cute-dbt#84).
//!
//! `--manifest` points at the committed jaffle-shop fixture purely so
//! the binary has a valid v12 manifest to render; the fixture predates
//! `Node::original_file_path`, so these PR-diff runs resolve to an empty
//! in-scope set by design — the assertions are about the exit contract,
//! not the scoped content.

#[path = "common/mod.rs"]
mod common;

use std::fs;

use common::{clear, fixture, run_cli, s, tmp};

/// Run cute-dbt scoping from a PR-diff `@file` and return the `Output`.
fn run_with_changed_files_file(
    changed_files_path: &str,
    out: &std::path::Path,
) -> std::process::Output {
    let current = fixture("jaffle-shop-current.json");
    let at_arg = format!("@{changed_files_path}");
    run_cli(&[
        "--manifest",
        s(&current),
        "--scope-from-pr-diff",
        &at_arg,
        "--out",
        s(out),
    ])
}

#[test]
fn missing_changed_files_file_is_a_usage_error_exit_2_no_report() {
    let missing = tmp("cfp-does-not-exist.txt");
    clear(&missing);
    let out = tmp("cfp-missing-report.html");
    clear(&out);

    let output = run_with_changed_files_file(s(&missing), &out);

    assert_eq!(
        output.status.code(),
        Some(2),
        "a missing @file is a clap usage error (exit 2); stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !out.exists(),
        "no report is written when argument parsing fails",
    );
}

#[test]
fn empty_changed_files_file_renders_zero_scope_exit_0() {
    let empty = tmp("cfp-empty.txt");
    fs::write(&empty, "").expect("write empty changed-files list");
    let out = tmp("cfp-empty-report.html");
    clear(&out);

    let output = run_with_changed_files_file(s(&empty), &out);

    assert!(
        output.status.success(),
        "an empty changed-files list is a zero-scope report, not an error; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(out.exists(), "a zero-scope report is still written");
    let html = fs::read_to_string(&out).expect("report is readable");
    assert!(
        html.contains("from PR file diff"),
        "the report banner states its PR-diff provenance",
    );
    assert!(
        !html.contains("vs baseline manifest"),
        "a PR-diff report names no baseline manifest",
    );
}

#[test]
fn crlf_and_blank_lines_in_changed_files_file_are_accepted_exit_0() {
    // CRLF line endings, blank lines, and trailing whitespace must not
    // break the file reader — they are tolerated and stripped.
    let path = tmp("cfp-crlf.txt");
    fs::write(
        &path,
        "  models/marts/dim_payers.sql  \r\n\r\n   \r\nmodels/staging/stg_orders.sql\r\n",
    )
    .expect("write CRLF changed-files list");
    let out = tmp("cfp-crlf-report.html");
    clear(&out);

    let output = run_with_changed_files_file(s(&path), &out);

    assert!(
        output.status.success(),
        "a CRLF / blank-line changed-files list is read without error; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(out.exists(), "the report is written");
}

#[test]
fn literal_comma_separated_list_is_accepted_exit_0() {
    // The literal (non-@) form resolves through the same binary path.
    let current = fixture("jaffle-shop-current.json");
    let out = tmp("cfp-literal-report.html");
    clear(&out);

    let output = run_cli(&[
        "--manifest",
        s(&current),
        "--scope-from-pr-diff",
        "models/marts/dim_payers.sql,models/staging/stg_orders.sql",
        "--out",
        s(&out),
    ]);

    assert!(
        output.status.success(),
        "a literal comma-separated list is accepted; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(out.exists(), "the report is written");
}

#[test]
fn changed_unit_test_yaml_path_puts_that_test_in_scope_exit_0() {
    // The behavior the flag exists for: a changed path that maps to a
    // manifest node brings it into scope. Uses playground-current.json
    // (whose unit_tests carry `original_file_path`, unlike the
    // jaffle-shop fixture used by the exit-contract cases above).
    // `_core__models.yml` is the source file declaring
    // `test_dim_payers_injects_unknown_sentinel`; with no --project-root
    // the strip is `None`, so the path matches the manifest entry
    // directly via the test_yaml_changed arm. This is the end-to-end
    // happy path through resolve_scope_input → select_in_scope → render
    // that the empty-scope cases above cannot exercise; broader
    // scope-content coverage lives in scope.rs units + the
    // pr_diff_scoping.feature BDD (cute-dbt#84).
    let current = fixture("playground-current.json");
    let out = tmp("cfp-scoped-report.html");
    clear(&out);

    let output = run_cli(&[
        "--manifest",
        s(&current),
        "--scope-from-pr-diff",
        "models/marts/core/_core__models.yml",
        "--out",
        s(&out),
    ]);

    assert!(
        output.status.success(),
        "a changed unit-test YAML path renders a scoped report; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let html = fs::read_to_string(&out).expect("report is readable");
    assert!(
        html.contains("test_dim_payers_injects_unknown_sentinel"),
        "the test declared in the changed YAML is in the rendered report",
    );
    assert!(
        html.contains("from PR file diff"),
        "the banner states PR-diff provenance",
    );
}
