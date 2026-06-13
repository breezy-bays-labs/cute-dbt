//! Step definitions for `features/review_pr_anchor.feature` — the
//! `--pr [<n>]` anchor and the fail-soft gh rung (cute-dbt#303, epic
//! #294 V4).
//!
//! The gh on PATH is always a shim (the `TestRepo` PATH is fully
//! controlled). The BDD runner is single-threaded
//! (`max_concurrent_scenarios(1)`), so the host-gh race that the
//! `review_cli` nextest tests guard against with a serial group cannot
//! arise here. Repo construction + the shared report/exit Thens live in
//! `one_command_review.rs` (same `World` fields).

use cucumber::{given, then, when};

use super::World;

/// The repo the shared Given built (one_command_review.rs).
fn repo(world: &World) -> &super::super::common::TestRepo {
    world
        .review_repo
        .as_ref()
        .expect("a Given built the review repo")
}

/// Capture a finished review invocation into the `World`.
fn capture(world: &mut World, output: &std::process::Output) {
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.last_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
}

// --- Given ----------------------------------------------------------

#[given(regex = r#"^a branch "([^"]+)" that edits the "([^"]+)" model$"#)]
fn given_named_branch_edit(world: &mut World, branch: String, model: String) {
    let repo = repo(world);
    repo.git(&["checkout", "-q", "-b", &branch]);
    repo.write(
        &format!("models/staging/{model}.sql"),
        "select 1 as customer_id -- edited\n",
    );
    repo.commit_all("edit on a named branch");
}

#[given(regex = r#"^gh reports an open PR with base "([^"]+)" and head "([^"]+)"$"#)]
fn given_gh_pr(world: &mut World, base: String, head: String) {
    repo(world).install_gh_shim(&format!(
        "case \"$1 $2\" in\n  'pr view') printf '{{\"baseRefName\":\"{base}\",\
         \"headRefName\":\"{head}\",\"number\":9}}\\n'; exit 0;;\nesac\nexit 0",
    ));
}

#[given("gh reports no open PR")]
fn given_gh_no_pr(world: &mut World) {
    repo(world).install_gh_shim("echo 'no pull requests found' >&2; exit 1");
}

#[given("gh is not installed on PATH")]
fn given_no_gh(world: &mut World) {
    repo(world).remove_shim("gh");
}

// --- When -----------------------------------------------------------
//
// Bare `--pr` is handled by the shared `review with <flag>` regex step
// (one_command_review.rs, `--[a-z-]+`). Only the value-bearing `--pr
// <n>` form needs a step here.

#[when(regex = r#"^I run cute-dbt review with --pr (\d+) in the repo$"#)]
fn run_review_pr_n(world: &mut World, number: String) {
    let output = repo(world).review(&["--pr", &number, "--no-open"]);
    capture(world, &output);
}

// --- Then -----------------------------------------------------------

#[then("stderr announces the base came from the gh pr rung")]
fn stderr_gh_rung(world: &mut World) {
    assert!(
        world.last_stderr.contains("via gh pr view"),
        "the gh rung is the announced answering rung: {}",
        world.last_stderr,
    );
}

#[then("stderr announces the base came from the local branch probe")]
fn stderr_local_probe(world: &mut World) {
    assert!(
        world.last_stderr.contains("local main/master/trunk probe"),
        "the ladder fell through to the local probe: {}",
        world.last_stderr,
    );
}

#[then("stderr tells me to open a PR with gh pr create")]
fn stderr_gh_create(world: &mut World) {
    assert!(
        world.last_stderr.contains("no open pull request")
            && world.last_stderr.contains("gh pr create"),
        "the no-PR remediation fires: {}",
        world.last_stderr,
    );
}

#[then(regex = r#"^stderr tells me to run gh pr checkout (\d+) first$"#)]
fn stderr_gh_checkout(world: &mut World, number: String) {
    assert!(
        world
            .last_stderr
            .contains(&format!("gh pr checkout {number}")),
        "the checkout remediation names PR #{number}: {}",
        world.last_stderr,
    );
}

#[then("review never checked out the working tree")]
fn review_never_checked_out(world: &mut World) {
    // The gh shim log must record no `pr checkout` — review never
    // mutates the working tree.
    assert!(
        !repo(world).gh_log_contents().contains("pr checkout"),
        "review must never ask gh to check out: {}",
        repo(world).gh_log_contents(),
    );
}
