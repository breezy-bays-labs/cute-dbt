//! Black-box contract for the cross-platform fake-tool entrypoint
//! (cute-dbt#331): the `cute-dbt` binary, when **copied under another
//! name** (`dbt`/`gh`) with a sibling `<name>.spec.toml`, acts as a
//! stand-in tool — the Windows-capable replacement for the retired
//! `#!/bin/sh` shims. Runs on every platform (it copies the real binary
//! and spawns the copy; no shell, no chmod, no env var).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The platform executable suffix (`.exe` on Windows, empty elsewhere).
fn exe_suffix() -> &'static str {
    if cfg!(windows) { ".exe" } else { "" }
}

/// Copy the real `cute-dbt` binary into `dir` under `name` (with the
/// platform suffix) and write its sibling `<name>.spec.toml`. Returns the
/// installed tool path.
fn install_fake(dir: &Path, name: &str, spec: &str) -> PathBuf {
    let tool = dir.join(format!("{name}{}", exe_suffix()));
    std::fs::copy(env!("CARGO_BIN_EXE_cute-dbt"), &tool).expect("copy the cute-dbt binary");
    std::fs::write(dir.join(format!("{name}.spec.toml")), spec).expect("write spec");
    tool
}

/// Spawn the installed fake tool with `args` from `cwd`.
fn run_tool(tool: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(tool)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("the fake tool spawns")
}

/// A unique temp dir under Cargo's integration-test tmp dir.
fn tmp(stem: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fake-tool-{stem}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn a_matching_rule_emits_its_stdout_and_exit_verbatim() {
    let dir = tmp("version-rule");
    let dbt = install_fake(
        &dir,
        "dbt",
        "default_exit = 0\n\
         [[rules]]\n\
         when = \"--version\"\n\
         stdout = \"dbt 2.0.0-preview.186\\n\"\n\
         exit = 0\n",
    );

    let out = run_tool(&dbt, &dir, &["--version"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "dbt 2.0.0-preview.186\n",
        "the matched rule's stdout is byte-exact",
    );
}

#[test]
fn a_rule_can_write_stderr_and_a_nonzero_exit() {
    let dir = tmp("compile-fails");
    let dbt = install_fake(
        &dir,
        "dbt",
        "default_exit = 0\n\
         [[rules]]\n\
         when = \"compile\"\n\
         stderr = \"Compilation Error in model stg_customers\\n\"\n\
         exit = 1\n",
    );

    let out = run_tool(&dbt, &dir, &["compile", "--quiet"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Compilation Error in model stg_customers\n",
    );
    assert!(out.stdout.is_empty(), "no stdout for this rule");
}

#[test]
fn a_two_word_prefix_matches_trailing_args() {
    let dir = tmp("two-word");
    // gh-shaped: dispatch on the two-word `pr view` prefix; trailing
    // `--json …` args are ignored, exactly like the sh `case "$1 $2"`.
    let gh = install_fake(
        &dir,
        "gh",
        "default_exit = 0\n\
         [[rules]]\n\
         when = \"pr view\"\n\
         stdout = \"{\\\"baseRefName\\\":\\\"main\\\"}\\n\"\n\
         exit = 0\n",
    );

    let out = run_tool(
        &gh,
        &dir,
        &["pr", "view", "--json", "baseRefName,headRefName,number"],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "{\"baseRefName\":\"main\"}\n",
    );
}

#[test]
fn no_matching_rule_falls_through_to_the_default_exit() {
    let dir = tmp("default-exit");
    let gh = install_fake(
        &dir,
        "gh",
        "default_exit = 1\n\
         [[rules]]\n\
         when = \"pr view\"\n\
         stdout = \"ignored\"\n\
         exit = 0\n",
    );

    // `pr list` does not match `pr view` → default_exit.
    let out = run_tool(&gh, &dir, &["pr", "list"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty());
}

#[test]
fn every_invocation_is_logged_with_cwd_and_args() {
    let dir = tmp("invocation-log");
    let dbt = install_fake(&dir, "dbt", "default_exit = 0\n");

    let _ = run_tool(&dbt, &dir, &["compile"]);
    // The log is named after the installed tool's stem (`dbt`).
    let log = dir.join("dbt-invocations.log");
    let contents = std::fs::read_to_string(&log).expect("invocation log written");
    let recorded = contents.trim();
    assert!(
        recorded.ends_with("args=compile"),
        "the args are logged: {recorded}",
    );
    let logged_cwd = recorded
        .strip_prefix("cwd=")
        .and_then(|s| s.split(" args=").next())
        .expect("cwd= prefix");
    assert_eq!(
        std::fs::canonicalize(logged_cwd).expect("canonicalize logged cwd"),
        std::fs::canonicalize(&dir).expect("canonicalize dir"),
        "the invocation cwd is logged",
    );
}

#[test]
fn the_real_binary_under_its_own_name_runs_the_cli_not_the_fake_tool() {
    // Belt-and-braces: invoked AS `cute-dbt` (the real binary, no rename)
    // the fake-tool trigger never fires — even if a stray spec sits in the
    // build dir. A bare invocation is a clap usage error (exit 2).
    let out = Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Usage"),
        "the real CLI's usage text, not fake-tool behaviour",
    );
}

#[test]
fn a_renamed_copy_without_a_sibling_spec_runs_the_real_cli() {
    // The trigger requires BOTH a non-`cute-dbt` name AND a sibling spec.
    // A renamed copy with no spec must fall through to the real CLI (so a
    // stray rename can never silently swallow a run).
    let dir = tmp("no-spec");
    let tool = dir.join(format!("dbt{}", exe_suffix()));
    std::fs::copy(env!("CARGO_BIN_EXE_cute-dbt"), &tool).expect("copy binary");
    // No `dbt.spec.toml` written.
    let out = run_tool(&tool, &dir, &[]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("Usage"));
}
