//! Subprocess integration tests for `cute-dbt skill` (cute-dbt#304,
//! epic #294 V5): the print byte-identity pin, the per-agent install
//! paths, and the non-repo refusal.
//!
//! Reuses the `review` test harness's git-isolated temp repos
//! (`common::TestRepo`) so an install runs inside a real `git rev-parse
//! --show-toplevel` boundary, fully isolated from the developer's git
//! environment.

#[path = "common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::TestRepo;

/// Absolute path to the committed canonical SKILL.md (the single source
/// of truth the binary embeds via `include_str!`).
fn repo_skill_md() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/dbt-pr-review/SKILL.md")
}

/// Run `cute-dbt skill <args>` from `cwd`, captured.
fn run_skill_in(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .arg("skill")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("the cute-dbt binary spawns")
}

#[test]
fn skill_print_is_byte_identical_to_the_repo_file() {
    // The zero-drift-by-construction pin (Laravel Boost pattern): the
    // bytes `--print` emits must equal the committed SKILL.md exactly —
    // they are the same file (`include_str!`). Any whitespace / content
    // edit to one without the other fails here.
    let expected = std::fs::read(repo_skill_md()).expect("the canonical SKILL.md is readable");
    let output = run_skill_in(Path::new(env!("CARGO_MANIFEST_DIR")), &["--print"]);
    assert!(output.status.success(), "skill --print exits 0: {output:?}");
    assert_eq!(
        output.stdout, expected,
        "skill --print must be byte-identical to skills/dbt-pr-review/SKILL.md",
    );
    assert!(
        output.stderr.is_empty(),
        "skill --print writes nothing to stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn skill_install_claude_code_writes_under_dot_claude() {
    let repo = TestRepo::init("skill-claude");
    repo.write("README.md", "a repo\n");
    repo.commit_all("init");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args(["skill", "--install", "--agent", "claude-code"])
        .current_dir(&repo.root);
    repo.isolate(&mut cmd);
    let output = cmd.output().expect("binary spawns");
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    let installed = repo.root.join(".claude/skills/dbt-pr-review/SKILL.md");
    assert!(
        installed.exists(),
        "the skill lands at the Claude Code path",
    );
    // The installed file is byte-identical to the canonical source.
    let installed_bytes = std::fs::read(&installed).expect("installed file readable");
    let expected = std::fs::read(repo_skill_md()).expect("canonical readable");
    assert_eq!(installed_bytes, expected, "installed == canonical");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Claude Code"),
        "the confirmation names the agent",
    );
}

#[test]
fn skill_install_cross_agents_write_under_dot_agents() {
    for agent in ["codex", "cursor", "copilot"] {
        let repo = TestRepo::init(&format!("skill-{agent}"));
        repo.write("README.md", "a repo\n");
        repo.commit_all("init");

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
        cmd.args(["skill", "--install", "--agent", agent])
            .current_dir(&repo.root);
        repo.isolate(&mut cmd);
        let output = cmd.output().expect("binary spawns");
        assert_eq!(output.status.code(), Some(0), "{agent}: {output:?}");

        assert!(
            repo.root
                .join(".agents/skills/dbt-pr-review/SKILL.md")
                .exists(),
            "{agent} installs under .agents/skills",
        );
        assert!(
            !repo.root.join(".claude/skills").exists(),
            "{agent} does NOT write the Claude Code path",
        );
    }
}

#[test]
fn skill_install_defaults_to_claude_code() {
    let repo = TestRepo::init("skill-default-agent");
    repo.write("README.md", "a repo\n");
    repo.commit_all("init");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args(["skill", "--install"]).current_dir(&repo.root);
    repo.isolate(&mut cmd);
    let output = cmd.output().expect("binary spawns");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        repo.root
            .join(".claude/skills/dbt-pr-review/SKILL.md")
            .exists(),
        "the default agent is Claude Code",
    );
}

#[test]
fn skill_install_in_a_subdirectory_writes_to_the_repo_root() {
    // Install resolves the repo toplevel, so running from a subdir still
    // writes at the root (where agents discover skills).
    let repo = TestRepo::init("skill-subdir");
    repo.write("README.md", "a repo\n");
    repo.write("models/x.sql", "select 1\n");
    repo.commit_all("init");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args(["skill", "--install"])
        .current_dir(repo.root.join("models"));
    repo.isolate(&mut cmd);
    let output = cmd.output().expect("binary spawns");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        repo.root
            .join(".claude/skills/dbt-pr-review/SKILL.md")
            .exists(),
        "the skill lands at the repo root, not the subdirectory",
    );
    assert!(
        !repo.root.join("models/.claude").exists(),
        "nothing is written in the subdirectory",
    );
}

#[test]
fn skill_install_outside_a_git_repo_is_refused() {
    // env::temp_dir() — NOT CARGO_TARGET_TMPDIR, which lives inside the
    // cute-dbt repo itself (a git context).
    let dir = std::env::temp_dir().join(format!("cute-dbt-skill-norepo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the plain dir");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args(["skill", "--install"]).current_dir(&dir);
    // Scrub GIT_DIR (a git hook exports it) so the cwd is genuinely
    // outside any repo, and exclude a host PATH leak.
    common::scrub_git_env(&mut cmd);
    let output = cmd.output().expect("binary spawns");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("git repository") && stderr.contains("--print"),
        "the refusal names the condition and the no-write escape: {stderr}",
    );
    assert!(
        !dir.join(".claude").exists() && !dir.join(".agents").exists(),
        "nothing is written outside a repo",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn skill_requires_an_action() {
    // `--print` XOR `--install`: bare `cute-dbt skill` is a usage error
    // (the skill_action ArgGroup is required).
    let output = run_skill_in(Path::new(env!("CARGO_MANIFEST_DIR")), &[]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a missing action is a clap usage error: {output:?}",
    );
}

#[test]
fn skill_print_and_install_conflict() {
    let output = run_skill_in(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &["--print", "--install"],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "--print and --install are mutually exclusive: {output:?}",
    );
}
