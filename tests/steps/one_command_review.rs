//! Step definitions for `features/one_command_review.feature` — the
//! `review` porcelain verb walking skeleton (cute-dbt#300, epic #294
//! V1).
//!
//! The Givens build a real temp git repo (`common::TestRepo`: real
//! `git init`, fully isolated from the developer's git environment);
//! the Whens spawn the real `cute-dbt review` binary with captured
//! output (never a TTY, so auto-open structurally cannot fire); the
//! Thens read `world.last_exit_code` / `world.last_stderr` /
//! `world.last_stdout` and the expected report path.
//!
//! REUSE — `the exit code is 0` (report_generation.rs) and
//! `stderr recommends running "…" or "…"` (fail_closed.rs) are shared
//! steps; redefining them here would be an ambiguous-step error.

use std::path::PathBuf;

use cucumber::{given, then, when};

use super::super::common::{TestRepo, scaffold_dbt_project};
use super::World;

/// Record a finished review invocation into the `World`.
fn capture(world: &mut World, output: &std::process::Output) {
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.last_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
}

/// The repo a Given prepared.
fn repo(world: &World) -> &TestRepo {
    world
        .review_repo
        .as_ref()
        .expect("a Given built the review repo")
}

/// The default report path the scenario expects.
fn expected_report(world: &World) -> &PathBuf {
    world
        .review_report_path
        .as_ref()
        .expect("a Given set the expected report path")
}

// --- Given ----------------------------------------------------------

#[given(regex = r#"^a git repo with a compiled dbt project on branch "([^"]+)"$"#)]
fn given_compiled_repo(world: &mut World, branch: String) {
    let repo = TestRepo::init_with_branch("bdd-review", &branch);
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    world.review_report_path = Some(repo.root.join("target/cute-dbt-report.html"));
    world.review_repo = Some(repo);
}

#[given(regex = r#"^a git repo with a compiled dbt project whose only branch is "([^"]+)"$"#)]
fn given_repo_on_unprobed_branch(world: &mut World, branch: String) {
    let repo = TestRepo::init_with_branch("bdd-review-nobase", &branch);
    scaffold_dbt_project(&repo, ".", "jaffle-shop-current.json");
    world.review_report_path = Some(repo.root.join("target/cute-dbt-report.html"));
    world.review_repo = Some(repo);
}

#[given("a git repo whose dbt project manifest was produced by dbt parse")]
fn given_parse_only_repo(world: &mut World) {
    let repo = TestRepo::init_with_branch("bdd-review-parseonly", "main");
    scaffold_dbt_project(&repo, ".", "jaffle-shop-parse-only.json");
    world.review_report_path = Some(repo.root.join("target/cute-dbt-report.html"));
    world.review_repo = Some(repo);
}

#[given(regex = r#"^a feature branch that edits the "([^"]+)" model$"#)]
fn given_feature_branch_edit(world: &mut World, model: String) {
    let repo = repo(world);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(
        &format!("models/staging/{model}.sql"),
        "select 1 as customer_id -- edited on the branch\n",
    );
    repo.commit_all("edit the model");
}

#[given(regex = r#"^an uncommitted edit to the "([^"]+)" model$"#)]
fn given_uncommitted_edit(world: &mut World, model: String) {
    repo(world).write(
        &format!("models/staging/{model}.sql"),
        "select 1 as customer_id -- uncommitted wip\n",
    );
}

#[given("an untracked new model file")]
fn given_untracked_model(world: &mut World) {
    repo(world).write("models/staging/brand_new_model.sql", "select 2 as id\n");
}

#[given("a dbt project directory that is not inside a git repository")]
fn given_plain_dir(world: &mut World) {
    // env::temp_dir() — NOT CARGO_TARGET_TMPDIR, which lives inside the
    // cute-dbt repo itself and would be a (surprising) git context.
    let dir = std::env::temp_dir().join(format!("cute-dbt-bdd-norepo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the plain dir");
    std::fs::write(dir.join("dbt_project.yml"), "name: plain\n").expect("write dbt_project.yml");
    world.review_plain_dir = Some(dir);
}

// --- When -----------------------------------------------------------

#[when("I run cute-dbt review in the repo")]
fn run_review(world: &mut World) {
    let output = repo(world).review(&["--no-open"]);
    capture(world, &output);
}

#[when(regex = r#"^I run cute-dbt review with (--[a-z-]+) in the repo$"#)]
fn run_review_with_flag(world: &mut World, flag: String) {
    let output = repo(world).review(&[&flag, "--no-open"]);
    capture(world, &output);
}

#[when("I run cute-dbt review in that directory")]
fn run_review_in_plain_dir(world: &mut World) {
    let dir = world
        .review_plain_dir
        .clone()
        .expect("a Given prepared the plain directory");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args(["review", "--no-open"])
        .current_dir(&dir)
        .env_remove("CUTE_DBT_EXPERIMENTAL");
    // Without this scrub, running the suite under a git hook (which
    // exports GIT_DIR) would hand the binary a repository context and
    // flip this scenario's outcome.
    super::super::common::scrub_git_env(&mut cmd);
    let output = cmd.output().expect("the cute-dbt binary spawns");
    capture(world, &output);
}

// --- Then -----------------------------------------------------------
//
// "the exit code is 0" and `stderr recommends running "…" or "…"` are
// shared steps (report_generation.rs / fail_closed.rs).

#[then("the exit code is 1")]
fn exit_code_is_one(world: &mut World) {
    assert_eq!(
        world.last_exit_code,
        Some(1),
        "review-stage failures exit 1; stderr: {}",
        world.last_stderr,
    );
}

#[then("the review report is written to the default target path")]
fn report_at_default_path(world: &mut World) {
    let path = expected_report(world);
    assert!(
        path.exists(),
        "expected the report at {}; stderr: {}",
        path.display(),
        world.last_stderr,
    );
}

#[then("no review report is written")]
fn no_report_written(world: &mut World) {
    let path = expected_report(world);
    assert!(
        !path.exists(),
        "no report must exist at {}; stderr: {}",
        path.display(),
        world.last_stderr,
    );
}

#[then("stdout prints the review report path")]
fn stdout_prints_path(world: &mut World) {
    assert!(
        world.last_stdout.contains("report written to")
            && world.last_stdout.contains("cute-dbt-report.html"),
        "stdout names the written report: {}",
        world.last_stdout,
    );
}

#[then(regex = r#"^the review report includes the unit test "([^"]+)"$"#)]
fn report_includes_test(world: &mut World, test: String) {
    let html = std::fs::read_to_string(expected_report(world)).expect("the report is readable");
    assert!(
        html.contains(&test),
        "the report scopes the unit test {test:?}",
    );
}

#[then("the review report shows zero unit tests in scope")]
fn report_zero_scope(world: &mut World) {
    let html = std::fs::read_to_string(expected_report(world)).expect("the report is readable");
    assert!(
        html.contains("0 unit tests in scope"),
        "the zero-scope banner is honest",
    );
}

#[then("stderr says there is nothing to review")]
fn stderr_nothing_to_review(world: &mut World) {
    assert!(
        world.last_stderr.contains("nothing to review"),
        "the empty diff is said out loud: {}",
        world.last_stderr,
    );
}

#[then("stderr explains review needs a git repository")]
fn stderr_needs_git_repo(world: &mut World) {
    assert!(
        world.last_stderr.contains("not inside a git repository"),
        "{}",
        world.last_stderr,
    );
}

#[then("stderr tells me to pass --base")]
fn stderr_names_base_flag(world: &mut World) {
    assert!(
        world.last_stderr.contains("--base"),
        "the remediation names --base: {}",
        world.last_stderr,
    );
}

#[then(regex = r#"^stderr warns about untracked files naming "([^"]+)"$"#)]
fn stderr_warns_untracked(world: &mut World, hint: String) {
    assert!(
        world.last_stderr.contains("untracked") && world.last_stderr.contains(&hint),
        "the untracked warning carries the {hint:?} hint: {}",
        world.last_stderr,
    );
}

#[then(regex = r#"^stdout lists the planned git diff command with "([^"]+)"$"#)]
fn stdout_lists_git_plan(world: &mut World, flag: String) {
    assert!(
        world.last_stdout.contains("git") && world.last_stdout.contains(&flag),
        "the dry-run listing carries the git diff plan with {flag:?}: {}",
        world.last_stdout,
    );
}

#[then("stdout lists the equivalent cute-dbt report invocation")]
fn stdout_lists_report_plan(world: &mut World) {
    assert!(
        world.last_stdout.contains("cute-dbt report") && world.last_stdout.contains("--pr-diff"),
        "the dry-run listing carries the equivalent report invocation: {}",
        world.last_stdout,
    );
}
