//! Step definitions for `features/agent_skill.feature` — the `skill`
//! verb (cute-dbt#304, epic #294 V5): print byte-identity, per-agent
//! install paths, and the non-repo refusal.
//!
//! Reuses the review scenario's `World` fields (`review_repo`,
//! `review_plain_dir`, `last_*`) and the git-isolated `common::TestRepo`
//! harness.

use std::path::{Path, PathBuf};
use std::process::Command;

use cucumber::{given, then, when};

use super::super::common::TestRepo;
use super::World;

/// Absolute path to the committed canonical SKILL.md.
fn canonical_skill_md() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/dbt-pr-review/SKILL.md")
}

/// The repo a Given built.
fn repo(world: &World) -> &TestRepo {
    world.review_repo.as_ref().expect("a Given built the repo")
}

// --- Given ----------------------------------------------------------

#[given("a git repo")]
fn given_git_repo(world: &mut World) {
    let repo = TestRepo::init("bdd-skill");
    repo.write("README.md", "a repo\n");
    repo.commit_all("init");
    world.review_repo = Some(repo);
}

#[given("a directory that is not a git repository")]
fn given_plain_dir(world: &mut World) {
    let dir =
        std::env::temp_dir().join(format!("cute-dbt-bdd-skill-norepo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the plain dir");
    world.review_plain_dir = Some(dir);
}

// --- When -----------------------------------------------------------

#[when("I run cute-dbt skill --print")]
fn run_skill_print(world: &mut World) {
    let output = Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args(["skill", "--print"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("the cute-dbt binary spawns");
    world.last_exit_code = output.status.code();
    // stdout is binary-exact for the byte-identity assertion; keep the
    // raw bytes alongside the lossy string steps use.
    world.last_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
}

#[when(regex = r#"^I run cute-dbt skill --install --agent ([a-z-]+) in the repo$"#)]
fn run_skill_install_agent(world: &mut World, agent: String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args(["skill", "--install", "--agent", &agent])
        .current_dir(&repo(world).root);
    repo(world).isolate(&mut cmd);
    let output = cmd.output().expect("the cute-dbt binary spawns");
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
}

#[when("I run cute-dbt skill --install in that directory")]
fn run_skill_install_plain(world: &mut World) {
    let dir = world
        .review_plain_dir
        .clone()
        .expect("a Given prepared the plain directory");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args(["skill", "--install"]).current_dir(&dir);
    super::super::common::scrub_git_env(&mut cmd);
    let output = cmd.output().expect("the cute-dbt binary spawns");
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
}

// --- Then -----------------------------------------------------------
//
// `the exit code is 0` / `is 1` are shared steps
// (report_generation.rs / one_command_review.rs).

#[then("stdout is byte-identical to the packaged SKILL.md")]
fn stdout_matches_skill(world: &mut World) {
    let expected = std::fs::read_to_string(canonical_skill_md()).expect("canonical readable");
    assert_eq!(
        world.last_stdout, expected,
        "skill --print is byte-identical to the committed SKILL.md",
    );
}

#[then(regex = r#"^stdout carries the skill frontmatter name "([^"]+)"$"#)]
fn stdout_has_name(world: &mut World, name: String) {
    assert!(
        world.last_stdout.contains(&format!("name: {name}")),
        "the printed frontmatter names the skill: {}",
        world.last_stdout,
    );
}

#[then(regex = r#"^the file "([^"]+)" exists in the repo$"#)]
fn repo_file_exists(world: &mut World, rel: String) {
    let path = repo(world).root.join(&rel);
    assert!(path.exists(), "{} must exist after install", path.display());
}

#[then("the installed skill is byte-identical to the packaged SKILL.md")]
fn installed_matches_canonical(world: &mut World) {
    let installed = repo(world)
        .root
        .join(".claude/skills/dbt-pr-review/SKILL.md");
    let installed_bytes = std::fs::read(&installed).expect("installed readable");
    let expected = std::fs::read(canonical_skill_md()).expect("canonical readable");
    assert_eq!(installed_bytes, expected, "installed == canonical");
}

#[then("stderr explains skill install needs a git repository")]
fn stderr_needs_repo(world: &mut World) {
    assert!(
        world.last_stderr.contains("git repository"),
        "the refusal names the condition: {}",
        world.last_stderr,
    );
}

#[then("no skill file is written")]
fn no_skill_written(world: &mut World) {
    let dir = world
        .review_plain_dir
        .as_ref()
        .expect("the plain dir was prepared");
    assert!(
        !dir.join(".claude").exists() && !dir.join(".agents").exists(),
        "nothing is written outside a repo",
    );
}
