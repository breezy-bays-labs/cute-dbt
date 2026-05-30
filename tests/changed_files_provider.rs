//! End-to-end subprocess contract for `--pr-diff @file` (cute-dbt#85,
//! reshaped from a changed-file list to a raw `git diff --unified=0`
//! patch at cute-dbt#96).
//!
//! This binary owns the *process-level* contract that unit tests cannot
//! exercise: a missing `@file` is a clap usage error (exit 2, no report
//! written), an empty patch renders a zero-scope report (exit 0), a
//! non-diff `@file` is a usage error (exit 2), and a valid diff touching
//! a test's declaring YAML brings that test into scope (exit 0). The
//! *exact* parse result (paths, hunk extraction, edge cases) is pinned by
//! the unit suite in `cli::pr_diff`; scope-selection correctness is
//! pinned by `src/domain/scope.rs` + the `pr_diff_scoping.feature` BDD.

#[path = "common/mod.rs"]
mod common;

use std::fs;

use common::{clear, fixture, run_cli, s, tmp};

/// Run cute-dbt scoping from a `--pr-diff @file` and return the `Output`.
fn run_with_pr_diff_file(diff_path: &str, out: &std::path::Path) -> std::process::Output {
    let current = fixture("jaffle-shop-current.json");
    let at_arg = format!("@{diff_path}");
    run_cli(&[
        "--manifest",
        s(&current),
        "--pr-diff",
        &at_arg,
        "--out",
        s(out),
    ])
}

#[test]
fn missing_pr_diff_file_is_a_usage_error_exit_2_no_report() {
    let missing = tmp("cfp-does-not-exist.patch");
    clear(&missing);
    let out = tmp("cfp-missing-report.html");
    clear(&out);

    let output = run_with_pr_diff_file(s(&missing), &out);

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
fn empty_pr_diff_file_renders_zero_scope_exit_0() {
    let empty = tmp("cfp-empty.patch");
    fs::write(&empty, "").expect("write empty patch");
    let out = tmp("cfp-empty-report.html");
    clear(&out);

    let output = run_with_pr_diff_file(s(&empty), &out);

    assert!(
        output.status.success(),
        "an empty diff is a zero-scope report, not an error; stderr: {}",
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
fn malformed_pr_diff_file_is_a_usage_error_exit_2_no_report() {
    // A non-empty `@file` that is not a unified diff is a clap usage
    // error (exit 2) — the cute-dbt#96 malformed-diff contract.
    let path = tmp("cfp-malformed.patch");
    fs::write(&path, "this is not a unified diff\njust some prose\n")
        .expect("write malformed patch");
    let out = tmp("cfp-malformed-report.html");
    clear(&out);

    let output = run_with_pr_diff_file(s(&path), &out);

    assert_eq!(
        output.status.code(),
        Some(2),
        "a non-diff @file is a clap usage error (exit 2); stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !out.exists(),
        "no report is written when the diff cannot be parsed",
    );
}

#[test]
fn valid_diff_touching_unit_test_yaml_puts_that_test_in_scope_exit_0() {
    // The behavior the flag exists for: a real diff whose changed file is
    // a test's declaring YAML brings that test into scope. Uses
    // playground-current.json (whose unit_tests carry
    // `original_file_path`). `_core__models.yml` declares
    // `test_dim_payers_injects_unknown_sentinel`; with no --project-root
    // the strip is `None`, so the path matches the manifest entry
    // directly. End-to-end through resolve_scope_input → select_in_scope
    // → render that the empty/malformed cases cannot exercise.
    let current = fixture("playground-current.json");
    let diff = tmp("cfp-scoped.patch");
    fs::write(
        &diff,
        "--- a/models/marts/core/_core__models.yml\n\
+++ b/models/marts/core/_core__models.yml\n\
@@ -3 +3 @@\n\
-      rows: []\n\
+      rows: [{id: 1}]\n",
    )
    .expect("write scoped patch");
    let out = tmp("cfp-scoped-report.html");
    clear(&out);

    let output = run_cli(&[
        "--manifest",
        s(&current),
        "--pr-diff",
        &format!("@{}", s(&diff)),
        "--out",
        s(&out),
    ]);

    assert!(
        output.status.success(),
        "a valid diff touching a test's YAML renders a scoped report; stderr: {}",
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
