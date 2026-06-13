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
// Scope variants: --staged / --unstaged (V3, cute-dbt#302)
// ===================================================================

#[test]
fn staged_reviews_only_staged_changes() {
    let repo = TestRepo::init("staged-only");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    // Stage the edit that pulls the unit test into scope.
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- staged\n");
    repo.git(&["add", "-A"]);

    let output = repo.review(&["--staged", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let html = std::fs::read_to_string(repo.root.join("target/cute-dbt-report.html"))
        .expect("the staged change renders a report");
    assert!(
        html.contains(STG_CUSTOMERS_TEST),
        "the staged edit scopes its unit test",
    );
    // The git diff plan for --staged is index-relative.
    let log = repo.dbt_log_contents();
    assert!(log.contains("args=compile"), "compile still runs: {log}");
}

#[test]
fn staged_ignores_a_purely_unstaged_edit() {
    let repo = TestRepo::init("staged-ignores-unstaged");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    // Edit but DO NOT stage: --staged (HEAD -> index) sees nothing.
    repo.write(
        STG_CUSTOMERS_SQL,
        "select 1 as customer_id -- unstaged only\n",
    );

    let output = repo.review(&["--staged", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stderr_of(&output).contains("nothing to review"),
        "an unstaged-only edit is empty under --staged: {}",
        stderr_of(&output),
    );
    assert!(
        !repo.root.join("target/cute-dbt-report.html").exists(),
        "no report for an empty staged diff",
    );
}

#[test]
fn unstaged_reviews_only_unstaged_edits() {
    let repo = TestRepo::init("unstaged-only");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- unstaged\n");
    // NOT staged — bare `git diff` (index -> working tree) sees it.

    let output = repo.review(&["--unstaged", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let html = std::fs::read_to_string(repo.root.join("target/cute-dbt-report.html"))
        .expect("the unstaged edit renders a report");
    assert!(
        html.contains(STG_CUSTOMERS_TEST),
        "the unstaged edit scopes its unit test",
    );
}

#[test]
fn unstaged_ignores_a_purely_staged_edit() {
    let repo = TestRepo::init("unstaged-ignores-staged");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.write(
        STG_CUSTOMERS_SQL,
        "select 1 as customer_id -- staged only\n",
    );
    repo.git(&["add", "-A"]);

    let output = repo.review(&["--unstaged", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stderr_of(&output).contains("nothing to review"),
        "a staged-only edit is empty under --unstaged (index == working tree): {}",
        stderr_of(&output),
    );
}

#[test]
fn staged_with_unstaged_edits_on_the_same_file_warns_about_drift() {
    // The same-revision contract: --staged diffs HEAD -> index, but the
    // manifest is compiled from the working tree. A file that is staged
    // AND further edited unstaged makes them disagree — warn, never
    // block, exit 0.
    let repo = TestRepo::init("staged-drift");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    // Stage one version…
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- staged v1\n");
    repo.git(&["add", "-A"]);
    // …then edit again WITHOUT staging: the file now has both index and
    // worktree changes (porcelain `MM`).
    repo.write(
        STG_CUSTOMERS_SQL,
        "select 1 as customer_id -- staged v1 then unstaged v2\n",
    );

    let output = repo.review(&["--staged", "--no-open"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "drift warns but never blocks: {output:?}",
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("staged but also have unstaged edits")
            && stderr.contains("stg_customers.sql"),
        "the drift warning names the file: {stderr}",
    );
    assert!(
        repo.root.join("target/cute-dbt-report.html").exists(),
        "the report still renders (graceful degrade, not a block)",
    );
}

#[test]
fn staged_without_drift_emits_no_drift_warning() {
    // The negative pin: a file staged with NO further unstaged edits
    // must not trip the drift warning (porcelain `M␠`, not `MM`).
    let repo = TestRepo::init("staged-no-drift");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.write(
        STG_CUSTOMERS_SQL,
        "select 1 as customer_id -- cleanly staged\n",
    );
    repo.git(&["add", "-A"]);

    let output = repo.review(&["--staged", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        !stderr_of(&output).contains("staged but also have unstaged edits"),
        "a cleanly-staged file does not drift: {}",
        stderr_of(&output),
    );
}

#[test]
fn unstaged_never_emits_a_drift_warning() {
    // Drift is a --staged-only concern: --unstaged diffs the working
    // tree, which is exactly what the manifest compiled.
    let repo = TestRepo::init("unstaged-no-drift");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- staged\n");
    repo.git(&["add", "-A"]);
    repo.write(
        STG_CUSTOMERS_SQL,
        "select 1 as customer_id -- staged then unstaged\n",
    );

    let output = repo.review(&["--unstaged", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        !stderr_of(&output).contains("staged but also have unstaged edits"),
        "the drift check never runs on --unstaged: {}",
        stderr_of(&output),
    );
}

#[test]
fn dry_run_reflects_the_staged_variant_command() {
    let repo = TestRepo::init("dryrun-staged");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- staged\n");
    repo.git(&["add", "-A"]);

    let dry = repo.review(&["--staged", "--dry-run", "--no-open"]);
    assert_eq!(dry.status.code(), Some(0), "{dry:?}");
    let stdout = stdout_of(&dry);
    assert!(
        stdout.contains("[git diff]") && stdout.contains("--cached"),
        "the dry-run git plan shows the --cached (staged) form: {stdout}",
    );
    assert!(
        repo.dbt_log_contents().is_empty(),
        "--dry-run spawns nothing",
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

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args(["review", "--no-open"])
        .current_dir(&dir)
        .env_remove("CUTE_DBT_EXPERIMENTAL");
    // Without this scrub, running the suite under a git hook (which
    // exports GIT_DIR) would hand the binary a repository context and
    // flip this scenario's outcome.
    common::scrub_git_env(&mut cmd);
    let output = cmd.output().expect("binary spawns");
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
fn no_compile_with_a_missing_manifest_errors_naming_dbt_compile() {
    let repo = TestRepo::init("nomanifest");
    // Scaffold WITHOUT a manifest copy and WITHOUT any dbt shim: the
    // --no-compile arm needs no dbt at all, and its missing-manifest
    // remediation names `dbt compile`.
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

    let output = repo.review(&["--no-compile", "--no-open"]);
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

    // --no-compile: this test pins manifest LOCATION, not the compile
    // step (no shim is installed — dbt is genuinely absent).
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args(["review", "--no-compile", "--no-open"])
        .current_dir(&repo.root);
    repo.isolate(&mut cmd);
    cmd.env("DBT_TARGET_PATH", "build");
    let output = cmd.output().expect("binary spawns");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        repo.root.join("build/cute-dbt-report.html").exists(),
        "the report follows DBT_TARGET_PATH",
    );
}

#[test]
fn the_target_path_flag_relocates_and_is_forwarded_to_dbt() {
    let repo = TestRepo::init("targetflag");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    // Move the manifest where the flag points (the scaffold put it in
    // target/).
    std::fs::create_dir_all(repo.root.join("build2")).expect("mkdir");
    std::fs::rename(
        repo.root.join("target/manifest.json"),
        repo.root.join("build2/manifest.json"),
    )
    .expect("relocate manifest");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");

    let output = repo.review(&["--target-path", "build2", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        repo.root.join("build2/cute-dbt-report.html").exists(),
        "the report follows --target-path",
    );
    let log = repo.dbt_log_contents();
    assert!(
        log.contains("args=compile --target-path build2"),
        "dbt is told to write where review reads: {log}",
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
        "[dbt compile]",
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
    assert!(
        repo.dbt_log_contents().is_empty(),
        "--dry-run never spawns dbt (not even --version): {}",
        repo.dbt_log_contents(),
    );

    // Prove the sentinel is live: the same repo WITHOUT --dry-run fails.
    let real = repo.review(&["--no-open"]);
    assert_eq!(
        real.status.code(),
        Some(1),
        "the real run hits the corrupt manifest — proving --dry-run executed nothing: {real:?}",
    );
}

#[test]
fn dry_run_with_no_compile_marks_the_skipped_compile() {
    let repo = TestRepo::init("dryrun-nocompile");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");

    let dry = repo.review(&["--dry-run", "--no-compile", "--no-open"]);
    assert_eq!(dry.status.code(), Some(0), "{dry:?}");
    assert!(
        stdout_of(&dry).contains("skipped (--no-compile)"),
        "the listing says the compile is skipped: {}",
        stdout_of(&dry),
    );
}

// ===================================================================
// Engine detection + compile (V2, cute-dbt#301)
// ===================================================================

/// Scaffold + a committed model edit on a feature branch — the standard
/// V2 setup with something to review.
fn repo_with_branch_change(stem: &str) -> TestRepo {
    let repo = TestRepo::init(stem);
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");
    repo
}

#[test]
fn the_compile_step_runs_in_the_project_dir_and_is_announced() {
    let repo = repo_with_branch_change("compile-runs");
    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("dbt engine: fusion 2.0.0-preview.186"),
        "the detected engine is announced: {stderr}",
    );
    let log = repo.dbt_log_contents();
    assert!(
        log.contains("args=--version") && log.contains("args=compile"),
        "detection then compile, via the shim: {log}",
    );
    let canonical_root = std::fs::canonicalize(&repo.root).expect("canonical root");
    assert!(
        log.contains(&format!("cwd={} args=compile", canonical_root.display())),
        "compile runs with cwd = the project dir: {log}",
    );
}

#[test]
fn a_failed_compile_is_fatal_even_though_the_manifest_exists() {
    // THE fusion trap (research-294 dbt-engine-mechanics §2): fusion
    // writes manifest.json even on FAILED compiles — exit code, never
    // artifact presence, is the success signal. The scaffold has a
    // perfectly valid manifest sitting in target/; the run must still
    // fail and write no report.
    let repo = repo_with_branch_change("compile-fails");
    repo.install_dbt_shim(
        "case \"$1\" in\n  --version) printf 'dbt 2.0.0-preview.186\\n'; exit 0;;\n  \
         compile) printf 'Compilation Error in model stg_customers\\n' >&2; exit 1;;\nesac\nexit 0",
    );

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("Compilation Error in model stg_customers"),
        "dbt's own stderr streams through verbatim: {stderr}",
    );
    assert!(
        stderr.contains("dbt compile failed (exit 1)"),
        "the review-stage failure names the exit code: {stderr}",
    );
    assert!(
        stderr.contains("dbt debug"),
        "the engine-uniform profile remediation is present: {stderr}",
    );
    assert!(
        repo.root.join("target/manifest.json").exists(),
        "the manifest exists (the trap precondition holds)",
    );
    assert!(
        !repo.root.join("target/cute-dbt-report.html").exists(),
        "no report is written on a failed compile",
    );
}

#[test]
fn the_python_core_version_shape_is_detected_and_announced() {
    let repo = repo_with_branch_change("core-engine");
    repo.install_dbt_shim(
        "case \"$1\" in\n  --version) printf 'Core:\\n  - installed: 1.10.2\\n  - latest:    \
         1.10.2 - Up to date!\\n\\nPlugins:\\n  - duckdb: 1.9.1\\n'; exit 0;;\n  \
         compile) exit 0;;\nesac\nexit 0",
    );

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stderr_of(&output).contains("dbt engine: dbt-core 1.10.2"),
        "the core engine is announced from the multi-line shape: {}",
        stderr_of(&output),
    );
    assert!(
        repo.root.join("target/cute-dbt-report.html").exists(),
        "the run completes on core",
    );
}

#[test]
fn a_pre_1_8_core_is_rejected_before_compiling() {
    let repo = repo_with_branch_change("core-old");
    repo.install_dbt_shim(
        "case \"$1\" in\n  --version) printf 'Core:\\n  - installed: 1.7.6\\n'; exit 0;;\n  \
         compile) exit 0;;\nesac\nexit 0",
    );

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("1.7.6") && stderr.contains("1.8"),
        "the floor is named: {stderr}",
    );
    let log = repo.dbt_log_contents();
    assert!(
        !log.contains("args=compile"),
        "compile never runs on a rejected engine: {log}",
    );
    assert!(
        !repo.root.join("target/cute-dbt-report.html").exists(),
        "no report on a rejected engine",
    );
}

#[test]
fn the_cloud_cli_is_rejected_with_remediation() {
    let repo = repo_with_branch_change("cloud-cli");
    repo.install_dbt_shim(
        "case \"$1\" in\n  --version) printf 'Cloud CLI - 0.38.0 (abc1234 \
         2026-05-01T00:00:00Z)\\n'; exit 0;;\nesac\nexit 0",
    );

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("dbt Cloud CLI") && stderr.contains("--no-compile"),
        "the Cloud CLI remediation names the local engines and the escape hatch: {stderr}",
    );
}

#[test]
fn a_missing_dbt_gets_the_install_remediation() {
    let repo = repo_with_branch_change("dbt-missing");
    // Remove the scaffold's default shim: the controlled PATH now has
    // no dbt at all (a developer's real dbt can never leak in).
    std::fs::remove_file(repo.root.parent().expect("base").join("bin/dbt"))
        .expect("remove the default shim");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(stderr.contains("`dbt` was not found on PATH"), "{stderr}");
    assert!(
        stderr.contains("--no-compile"),
        "the remediation names the no-dbt escape hatch: {stderr}",
    );
}

#[test]
fn no_compile_needs_no_dbt_at_all() {
    let repo = repo_with_branch_change("nocompile-nodbt");
    std::fs::remove_file(repo.root.parent().expect("base").join("bin/dbt"))
        .expect("remove the default shim");

    let output = repo.review(&["--no-compile", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        repo.root.join("target/cute-dbt-report.html").exists(),
        "--no-compile renders from the existing manifest without dbt",
    );
    assert!(repo.dbt_log_contents().is_empty(), "dbt was never spawned",);
}

#[test]
fn no_compile_warns_when_the_manifest_is_stale() {
    let repo = repo_with_branch_change("stale-warn");
    // Age the manifest behind the branch edit.
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(repo.root.join("target/manifest.json"))
        .expect("open manifest")
        .set_modified(past)
        .expect("age the manifest");

    let output = repo.review(&["--no-compile", "--no-open"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a stale manifest warns, never blocks: {output:?}"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("older than"),
        "the staleness warning fires: {stderr}",
    );
    assert!(
        repo.root.join("target/cute-dbt-report.html").exists(),
        "the report still renders",
    );
}

#[test]
fn no_compile_with_a_fresh_manifest_does_not_warn() {
    let repo = repo_with_branch_change("fresh-quiet");
    // Make the manifest strictly newest.
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(repo.root.join("target/manifest.json"))
        .expect("open manifest")
        .set_modified(future)
        .expect("freshen the manifest");

    let output = repo.review(&["--no-compile", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        !stderr_of(&output).contains("older than"),
        "a fresh manifest stays quiet: {}",
        stderr_of(&output),
    );
}

#[test]
fn no_compile_with_a_custom_target_path_ignores_dbt_artifacts_beside_the_manifest() {
    // The gemini-flagged false positive on this PR: with a custom
    // --target-path, dbt's own run_results.json (written after
    // manifest.json) lives in a dir not named `target` — the staleness
    // walk must exclude the RESOLVED target dir by path, or every
    // --no-compile run with a custom target path warns spuriously.
    let repo = repo_with_branch_change("custom-target-stale");
    std::fs::create_dir_all(repo.root.join("build2")).expect("mkdir");
    std::fs::rename(
        repo.root.join("target/manifest.json"),
        repo.root.join("build2/manifest.json"),
    )
    .expect("relocate manifest");
    // Manifest newer than every source…
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(repo.root.join("build2/manifest.json"))
        .expect("open manifest")
        .set_modified(future)
        .expect("freshen manifest");
    // …but a dbt artifact in the SAME custom target dir is newer still.
    std::fs::write(repo.root.join("build2/run_results.json"), "{}").expect("write");
    std::fs::File::options()
        .write(true)
        .open(repo.root.join("build2/run_results.json"))
        .expect("open artifact")
        .set_modified(future + std::time::Duration::from_secs(60))
        .expect("age artifact newer");

    let output = repo.review(&["--no-compile", "--target-path", "build2", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        !stderr_of(&output).contains("older than"),
        "dbt's own artifacts beside the manifest never trigger the staleness warning: {}",
        stderr_of(&output),
    );
    assert!(
        repo.root.join("build2/cute-dbt-report.html").exists(),
        "the report renders",
    );
}

#[test]
fn no_compile_with_a_custom_target_path_still_warns_on_genuinely_newer_sources() {
    // The positive twin of the exclusion fix: skipping the resolved
    // target dir must NOT swallow real staleness — a model source newer
    // than the manifest still warns under a custom --target-path.
    let repo = repo_with_branch_change("custom-target-genuine");
    std::fs::create_dir_all(repo.root.join("build2")).expect("mkdir");
    std::fs::rename(
        repo.root.join("target/manifest.json"),
        repo.root.join("build2/manifest.json"),
    )
    .expect("relocate manifest");
    // Manifest aged behind the branch edit: the model source is newer.
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(repo.root.join("build2/manifest.json"))
        .expect("open manifest")
        .set_modified(past)
        .expect("age manifest");

    let output = repo.review(&["--no-compile", "--target-path", "build2", "--no-open"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "warn never blocks: {output:?}"
    );
    assert!(
        stderr_of(&output).contains("older than"),
        "a genuinely newer source still warns with a custom target dir: {}",
        stderr_of(&output),
    );
}

#[test]
fn an_empty_diff_exits_before_any_dbt_runs() {
    let repo = TestRepo::init("empty-skips-compile");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stderr_of(&output).contains("nothing to review"),
        "{}",
        stderr_of(&output),
    );
    assert!(
        repo.dbt_log_contents().is_empty(),
        "nothing to review ⇒ no detection, no compile: {}",
        repo.dbt_log_contents(),
    );
}

#[test]
fn a_successful_compile_that_writes_no_manifest_gets_the_target_path_remediation() {
    // The after-compile arm of ManifestMissing: dbt exited 0 but
    // review's resolved manifest path stayed empty — a target-path
    // mismatch, NOT a "run dbt compile" situation.
    let repo = TestRepo::init("ghost-target");
    repo.write(
        "dbt_project.yml",
        "name: jaffle_shop\nversion: \"1.0\"\nprofile: jaffle_shop\n",
    );
    repo.write(".gitignore", "target/\n");
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id\n");
    repo.commit_all("init");
    repo.install_dbt_shim(common::WELL_BEHAVED_FUSION_SHIM);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("no manifest appeared") && stderr.contains("--target-path"),
        "the remediation points at target-path alignment, not dbt compile: {stderr}",
    );
}

// ===================================================================
// --pr anchor + fail-soft gh rung (V4, cute-dbt#303)
// ===================================================================

/// A `gh` shim body that answers `pr view` with the given base/head/
/// number JSON and exits 0; every other subcommand exits 0 with no
/// output.
fn gh_pr_view_shim(base: &str, head: &str, number: u64) -> String {
    format!(
        "case \"$1 $2\" in\n  'pr view') printf '{{\"baseRefName\":\"{base}\",\
         \"headRefName\":\"{head}\",\"number\":{number}}}\\n'; exit 0;;\nesac\nexit 0",
    )
}

#[test]
fn the_gh_rung_resolves_the_open_pr_base_in_the_auto_ladder() {
    // No --base, no cute-dbt.base, no origin/HEAD: the gh rung answers.
    // The PR's base is `main`, which exists as a local branch — review
    // resolves it (origin/main absent in this repo, so the local ref).
    let repo = TestRepo::init_with_branch("gh-rung", "main");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");
    repo.install_gh_shim(&gh_pr_view_shim("main", "feature", 12));

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("via gh pr view"),
        "the gh rung is announced as the answering rung: {stderr}",
    );
    assert!(
        std::fs::read_to_string(repo.root.join("target/cute-dbt-report.html"))
            .expect("report written")
            .contains(STG_CUSTOMERS_TEST),
    );
}

#[test]
fn a_missing_gh_falls_through_the_auto_ladder_silently() {
    // No gh shim installed: the gh rung must fall through to the local
    // main probe, NOT error — gh is never a hard dependency of the
    // auto-ladder.
    let repo = TestRepo::init_with_branch("gh-absent", "main");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");
    // (no install_gh_shim)

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stderr_of(&output).contains("local main/master/trunk probe"),
        "without gh the ladder falls through to the local probe: {}",
        stderr_of(&output),
    );
}

#[test]
fn a_gh_failure_falls_through_the_auto_ladder_silently() {
    // gh present but `pr view` exits non-zero (e.g. not authed / no PR):
    // the auto rung still falls through, no error.
    let repo = TestRepo::init_with_branch("gh-fails", "main");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");
    repo.install_gh_shim("echo 'no pull requests found' >&2; exit 1");

    let output = repo.review(&["--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stderr_of(&output).contains("local main/master/trunk probe"),
        "a failing gh falls through to the local probe: {}",
        stderr_of(&output),
    );
}

#[test]
fn the_auto_gh_rung_never_runs_on_a_detached_head() {
    // The rung is branch-only: on a detached HEAD it must not even spawn
    // gh (the gh shim log stays empty), falling straight through.
    let repo = TestRepo::init_with_branch("gh-detached", "main");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    // Make a second commit, then detach onto its SHA with a tree change.
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- v2\n");
    repo.commit_all("v2");
    let head = String::from_utf8(repo.git(&["rev-parse", "HEAD"]).stdout).expect("utf8");
    repo.git(&["checkout", "-q", head.trim()]);
    repo.write(
        STG_CUSTOMERS_SQL,
        "select 1 as customer_id -- detached edit\n",
    );
    repo.install_gh_shim(&gh_pr_view_shim("main", "feature", 1));

    let output = repo.review(&["--no-open"]);
    // The local `main` probe answers the base; the run succeeds.
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        repo.gh_log_contents().is_empty(),
        "gh is never spawned on a detached HEAD: {}",
        repo.gh_log_contents(),
    );
}

#[test]
fn bare_pr_uses_the_open_prs_base() {
    let repo = TestRepo::init_with_branch("bare-pr", "main");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");
    repo.install_gh_shim(&gh_pr_view_shim("main", "feature", 7));

    let output = repo.review(&["--pr", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stderr_of(&output).contains("via gh pr view"),
        "--pr anchors via the gh rung: {}",
        stderr_of(&output),
    );
    assert!(
        repo.root.join("target/cute-dbt-report.html").exists(),
        "the PR-anchored review renders",
    );
}

#[test]
fn bare_pr_with_no_open_pr_is_a_remediated_error() {
    let repo = TestRepo::init_with_branch("no-pr", "main");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");
    repo.install_gh_shim("echo 'no pull requests found' >&2; exit 1");

    let output = repo.review(&["--pr", "--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("no open pull request") && stderr.contains("gh pr create"),
        "the no-PR remediation fires (explicit --pr surfaces the failure): {stderr}",
    );
    assert!(
        !repo.root.join("target/cute-dbt-report.html").exists(),
        "no report on a --pr failure",
    );
}

#[test]
fn pr_with_missing_gh_errors_with_the_install_remediation() {
    let repo = TestRepo::init_with_branch("pr-no-gh", "main");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");
    // (no gh shim — gh genuinely absent)

    let output = repo.review(&["--pr", "--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr_of(&output).contains("cli.github.com"),
        "explicit --pr surfaces gh-missing (auto rung would fall through): {}",
        stderr_of(&output),
    );
}

#[test]
fn pr_number_on_the_matching_head_branch_runs() {
    let repo = TestRepo::init_with_branch("pr-num-match", "main");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");
    // PR #9's head IS `feature`, the current branch.
    repo.install_gh_shim(&gh_pr_view_shim("main", "feature", 9));

    let output = repo.review(&["--pr", "9", "--no-open"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        repo.root.join("target/cute-dbt-report.html").exists(),
        "the head matches, so the review runs",
    );
}

#[test]
fn pr_number_on_the_wrong_branch_remediates_without_mutating_the_tree() {
    let repo = TestRepo::init_with_branch("pr-num-mismatch", "main");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    repo.git(&["checkout", "-q", "-b", "some-other-branch"]);
    repo.write(STG_CUSTOMERS_SQL, "select 1 as customer_id -- edited\n");
    repo.commit_all("edit");
    // PR #9's head is `feature`, but we are on `some-other-branch`.
    repo.install_gh_shim(&gh_pr_view_shim("main", "feature", 9));

    let branch_before =
        String::from_utf8(repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]).stdout).expect("utf8");
    let output = repo.review(&["--pr", "9", "--no-open"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("gh pr checkout 9") && stderr.contains("never checks out"),
        "the mismatch remediates with checkout + the never-mutate promise: {stderr}",
    );
    // The never-mutate contract: the working tree / branch is untouched.
    let branch_after =
        String::from_utf8(repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]).stdout).expect("utf8");
    assert_eq!(
        branch_before.trim(),
        branch_after.trim(),
        "review never checked out — the branch is unchanged",
    );
    assert!(
        gh_checkout_was_never_invoked(&repo),
        "the gh shim was never asked to `pr checkout`",
    );
}

/// Whether the gh shim's invocation log records any `pr checkout` call.
fn gh_checkout_was_never_invoked(repo: &TestRepo) -> bool {
    !repo.gh_log_contents().contains("pr checkout")
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
