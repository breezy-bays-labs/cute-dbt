//! Step definitions for `features/review_scope_variants.feature` — the
//! `--staged` / `--unstaged` scope variants and the staged
//! same-revision drift warning (cute-dbt#302, epic #294 V3).
//!
//! Repo construction, the parametrized `review with <flag>` When, and
//! the shared report/exit/stderr Thens live in `one_command_review.rs`
//! (same `World` fields); this file adds the staging-specific Givens
//! and the drift-warning Then.

use cucumber::{given, then};

use super::World;

/// The repo the shared Given built (one_command_review.rs).
fn repo(world: &World) -> &super::super::common::TestRepo {
    world
        .review_repo
        .as_ref()
        .expect("a Given built the review repo")
}

// --- Given ----------------------------------------------------------

#[given(regex = r#"^a staged edit to the "([^"]+)" model$"#)]
fn given_staged_edit(world: &mut World, model: String) {
    let repo = repo(world);
    repo.write(
        &format!("models/staging/{model}.sql"),
        "select 1 as customer_id -- staged\n",
    );
    repo.git(&["add", "-A"]);
}

#[given(regex = r#"^a staged-then-further-unstaged edit to the "([^"]+)" model$"#)]
fn given_staged_then_unstaged_edit(world: &mut World, model: String) {
    let repo = repo(world);
    let path = format!("models/staging/{model}.sql");
    // Stage one version…
    repo.write(&path, "select 1 as customer_id -- staged v1\n");
    repo.git(&["add", "-A"]);
    // …then edit again without staging — the file is now `MM` in
    // porcelain (both index and worktree dirty): the drift signal.
    repo.write(
        &path,
        "select 1 as customer_id -- staged v1 then unstaged v2\n",
    );
}

// --- Then -----------------------------------------------------------

#[then(regex = r#"^stderr warns about the staged same-revision drift naming "([^"]+)"$"#)]
fn stderr_warns_drift(world: &mut World, file: String) {
    assert!(
        world
            .last_stderr
            .contains("staged but also have unstaged edits"),
        "the drift warning fires: {}",
        world.last_stderr,
    );
    assert!(
        world.last_stderr.contains(&file),
        "the drift warning names {file:?}: {}",
        world.last_stderr,
    );
}
