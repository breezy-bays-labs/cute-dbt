//! Subprocess integration tests for `cute-dbt review` (cute-dbt#300,
//! epic #294 V1): real temp git repos, the real binary, the real git.
//!
//! Every test runs the spawned binary with captured output — stdout is
//! never a TTY, so the `IsTerminal` auto-open guard structurally cannot
//! fire here (and the happy-path test deliberately omits `--no-open` to
//! exercise exactly that guard). The git environment is fully isolated
//! per repo (`TestRepo::isolate`): a developer's `commit.gpgsign`,
//! `diff.noprefix`, or global `cute-dbt.base` can never steer a test.

#[path = "common/mod.rs"]
mod common;

use common::{TestRepo, scaffold_dbt_project};

/// The unit test the jaffle-shop fixtures declare on `stg_customers` —
/// the scope oracle for the happy paths.
const STG_CUSTOMERS_TEST: &str = "test_stg_customers_renames_columns";

/// Path of the model whose edit pulls that test into scope.
const STG_CUSTOMERS_SQL: &str = "models/staging/stg_customers.sql";

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ===================================================================
// Happy path + scope
// ===================================================================

#[test]
fn a_branch_with_a_committed_model_change_renders_a_scoped_report() {
    let repo = TestRepo::init("happy");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit stg_customers");

    // Deliberately NO --no-open: captured stdout is not a TTY, so the
    // IsTerminal guard must keep the platform opener un-spawned.
    let output = repo.review(&[]);
    assert_eq!(output.status.code(), Some(0), "review exits 0: {output:?}");
    let report = repo.root.join("target/cute-dbt-report.html");
    assert!(report.exists(), "the report lands at the default path");
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("report written to") && stdout.contains("cute-dbt-report.html"),
        "stdout prints the report path: {stdout}",
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("base:") && stderr.contains("local main/master/trunk probe"),
        "the answering ladder rung is announced: {stderr}",
    );
    let html = std::fs::read_to_string(&report).expect("report readable");
    assert!(
        html.contains(STG_CUSTOMERS_TEST),
        "the changed model's unit test is in the report",
    );
}

#[test]
fn an_uncommitted_working_tree_edit_is_included_by_default() {
    let repo = TestRepo::init("dirty-default");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    // No branch, no commit — just a dirty working tree on main.
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- wip\n");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let html = std::fs::read_to_string(repo.root.join("target/cute-dbt-report.html"))
        .expect("report written from the dirty tree");
    assert!(
        html.contains(STG_CUSTOMERS_TEST),
        "the uncommitted edit scopes the test (working-tree endpoint)",
    );
}

#[test]
fn committed_only_excludes_the_uncommitted_edit() {
    let repo = TestRepo::init("committed-only");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- wip\n");

    let output = repo.review(&["--committed-only", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stderr_of(&output).contains("nothing to review"),
        "merge-base..HEAD is empty, so there is nothing to review: {}",
        stderr_of(&output),
    );
    assert!(
        !repo.root.join("target/cute-dbt-report.html").exists(),
        "no report is written for an empty committed-only diff",
    );
}

// ===================================================================
// Empty diff posture
// ===================================================================

#[test]
fn a_clean_tree_on_the_base_branch_is_nothing_to_review() {
    let repo = TestRepo::init("clean");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");

    let output = repo.review(&["--no-open"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "exit 0 by design: {output:?}"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("nothing to review"),
        "the empty diff is said out loud: {stderr}",
    );
    assert!(
        stderr.contains("--force"),
        "the message names the --force override: {stderr}",
    );
    assert!(
        !repo.root.join("target/cute-dbt-report.html").exists(),
        "no file is written on an empty diff",
    );
}

#[test]
fn force_renders_the_zero_scope_report_on_an_empty_diff() {
    let repo = TestRepo::init("force");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");

    let output = repo.review(&["--force", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let html = std::fs::read_to_string(repo.root.join("target/cute-dbt-report.html"))
        .expect("--force writes the zero-scope report");
    assert!(
        html.contains("0 unit tests in scope"),
        "the zero-scope banner is honest",
    );
}

// ===================================================================
// Warnings
// ===================================================================

#[test]
fn untracked_model_files_warn_with_a_git_add_n_hint() {
    let repo = TestRepo::init("untracked");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.write("models/staging/brand_new_model.sql", "select 2 as id\n");
    // NOT added — invisible to git diff.

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("untracked") && stderr.contains("brand_new_model.sql"),
        "the warning names the invisible file: {stderr}",
    );
    assert!(
        stderr.contains("git add -N"),
        "the warning carries the include hint: {stderr}",
    );
}

// ===================================================================
// Review-stage errors (ReviewError, exit 1)
// ===================================================================

#[test]
fn outside_a_git_repository_review_fails_with_remediation() {
    // env::temp_dir() — NOT CARGO_TARGET_TMPDIR, which lives inside the
    // cute-dbt repo itself and would be a (surprising) git context.
    let dir = std::env::temp_dir().join(format!("cute-dbt-review-norepo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create dir");
    std::fs::write(dir.join("dbt_project.yml"), "name: x\n").expect("write");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args(["review", "--no-open"])
        .current_dir(&dir)
        .env_remove("CUTE_DBT_EXPERIMENTAL")
        .output()
        .expect("binary spawns");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("not inside a git repository"),
        "the error names the condition: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_detectable_base_errors_naming_the_base_flag() {
    // Only branch is `work`: no config, no remote, no main/master/trunk.
    let repo = TestRepo::init_with_branch("nobase", "work");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("could not determine the base branch"),
        "{stderr}"
    );
    assert!(
        stderr.contains("--base"),
        "remediation names --base: {stderr}"
    );
    assert!(
        stderr.contains("cute-dbt.base"),
        "remediation names the persistence option: {stderr}",
    );
}

#[test]
fn an_unresolvable_explicit_base_errors_instead_of_falling_through() {
    let repo = TestRepo::init("badbase");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");

    let output = repo.review(&["--base", "release-9.9", "--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("release-9.9") && stderr.contains("does not resolve"),
        "{stderr}"
    );
    assert!(
        stderr.contains("git fetch origin release-9.9"),
        "remediation suggests fetching the ref: {stderr}",
    );
}

#[test]
fn a_persisted_git_config_base_answers_the_ladder() {
    let repo = TestRepo::init_with_branch("configbase", "develop");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["config", "cute-dbt.base", "develop"]);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("base: develop") && stderr.contains("git config cute-dbt.base"),
        "the config rung answers and is announced: {stderr}",
    );
}

#[test]
fn disjoint_histories_are_diagnosed_with_the_base_remediation() {
    let repo = TestRepo::init("disjoint");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "--orphan", "lonely"]);
    repo.write("README.md", "an unrelated history\n");
    repo.commit_all("orphan root");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("no common ancestor"),
        "disjoint histories are named: {stderr}",
    );
    assert!(stderr.contains("--base"), "{stderr}");
}

#[test]
fn a_missing_manifest_errors_naming_dbt_compile() {
    let repo = TestRepo::init("nomanifest");
    // Scaffold WITHOUT a manifest copy.
    repo.write(
        "dbt_project.yml",
        "name: jaffle_shop\nversion: \"1.0\"\nprofile: jaffle_shop\n",
    );
    repo.write(".gitignore", "target/\n");
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id\n");
    repo.commit_all("init");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("no compiled manifest") && stderr.contains("dbt compile"),
        "{stderr}"
    );
}

#[test]
fn not_compiled_preflight_passes_through_with_its_own_remediation() {
    // The parse-only fixture: stg_customers carries compiled_code null,
    // so the in-scope test fails Stage-2 closed THROUGH review — the
    // PreflightError remediation must arrive verbatim, exit 1, no file.
    let repo = TestRepo::init("parseonly");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-parse-only.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("dbt compile") || stderr.contains("dbt run"),
        "the NotCompiled remediation passes through: {stderr}",
    );
    assert!(
        !repo.root.join("target/cute-dbt-report.html").exists(),
        "fail-closed writes no report",
    );
}

// ===================================================================
// Project discovery
// ===================================================================

#[test]
fn a_project_one_level_down_is_discovered_and_scoped() {
    // The subdirectory layout exercises the toplevel-relative
    // --project-root strip end-to-end: diff paths are
    // `analytics/models/...`, manifest paths are `models/...`.
    let repo = TestRepo::init("subdir");
    scaffold_dbt_project(&repo, "analytics", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(
        "analytics/models/staging/stg_customers.sql",
        "select 1 as customer_id -- edited\n",
    );
    repo.commit_all("edit");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let html = std::fs::read_to_string(repo.root.join("analytics/target/cute-dbt-report.html"))
        .expect("the report lands in the discovered project's target/");
    assert!(
        html.contains(STG_CUSTOMERS_TEST),
        "the subdirectory project's change scopes its test",
    );
}

#[test]
fn running_from_inside_the_project_subdirectory_works_and_relative_out_is_cwd_anchored() {
    // cwd = the project subdir: discovery hits the cwd itself, and a
    // relative --out resolves against the OPERATOR's cwd — never the
    // repo toplevel the composed run loop later moves to.
    let repo = TestRepo::init("insidesub");
    scaffold_dbt_project(&repo, "analytics", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(
        "analytics/models/staging/stg_customers.sql",
        "select 1 as customer_id -- edited\n",
    );
    repo.commit_all("edit");

    let output = repo.review_in("analytics", &["--out", "my-report.html", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        repo.root.join("analytics/my-report.html").exists(),
        "a relative --out lands in the operator's cwd",
    );
    assert!(
        !repo.root.join("my-report.html").exists(),
        "…and never at the repo toplevel",
    );
}

#[test]
fn two_candidate_projects_are_ambiguous_listing_both() {
    let repo = TestRepo::init("ambiguous");
    repo.write("alpha/dbt_project.yml", "name: alpha\n");
    repo.write("beta/dbt_project.yml", "name: beta\n");
    repo.commit_all("two projects");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("alpha") && stderr.contains("beta"),
        "both candidates are listed: {stderr}",
    );
    assert!(stderr.contains("--project-dir"), "{stderr}");
}

#[test]
fn dbt_target_path_relocates_the_manifest_and_the_default_out() {
    let repo = TestRepo::init("targetpath");
    repo.write(
        "dbt_project.yml",
        "name: jaffle_shop\nversion: \"1.0\"\nprofile: jaffle_shop\n",
    );
    repo.write(".gitignore", "build/\n");
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id\n");
    repo.commit_all("init");
    std::fs::create_dir_all(repo.root.join("build")).expect("mkdir build");
    std::fs::copy(
        common::fixture("jaffle-shop-current.json"),
        repo.root.join("build/manifest.json"),
    )
    .expect("copy manifest");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args(["review", "--no-open"]).current_dir(&repo.root);
    repo.isolate(&mut cmd);
    cmd.env("DBT_TARGET_PATH", "build");
    let output = cmd.output().expect("binary spawns");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        repo.root.join("build/cute-dbt-report.html").exists(),
        "the report follows DBT_TARGET_PATH",
    );
}

// ===================================================================
// --dry-run
// ===================================================================

#[test]
fn dry_run_prints_the_exact_plans_and_executes_nothing() {
    let repo = TestRepo::init("dryrun");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");
    // The sentinel: corrupt the manifest so any executed report stage
    // WOULD fail closed (exit 1). --dry-run must still exit 0.
    repo.write("target/manifest.json", "this is not json");

    let dry = repo.review(&["--dry-run", "--no-open"]);
    assert_eq!(
        dry.status.code(),
        Some(0),
        "--dry-run executes nothing, so the corrupt manifest cannot fail it: {dry:?}",
    );
    let stdout = stdout_of(&dry);
    for needle in [
        "git",
        "-c diff.noprefix=false",
        "--unified=0",
        "--find-renames",
        "--no-ext-diff",
        "cute-dbt report",
        "--pr-diff",
        "--project-root",
        "cute-dbt-report.html",
    ] {
        assert!(
            stdout.contains(needle),
            "the dry-run listing carries {needle:?}: {stdout}",
        );
    }
    assert!(
        !repo.root.join("target/cute-dbt-report.html").exists(),
        "--dry-run writes nothing",
    );

    // Prove the sentinel is live: the same repo WITHOUT --dry-run fails.
    let real = repo.review(&["--no-open"]);
    assert_eq!(
        real.status.code(),
        Some(1),
        "the real run hits the corrupt manifest — proving --dry-run executed nothing: {real:?}",
    );
}

// ===================================================================
// Usage errors stay exit 2
// ===================================================================

#[test]
fn an_unknown_review_flag_is_a_usage_error() {
    let repo = TestRepo::init("usage");
    let output = repo.review(&["--frobnitz"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "clap usage errors keep exit 2: {output:?}",
    );
}
