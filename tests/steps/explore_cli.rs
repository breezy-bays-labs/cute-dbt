//! Step definitions for `features/explore_cli.feature` — the
//! cute-dbt#100 verb-structured CLI surface and the `explore` verb's
//! output + Stage-1 fail-closed behavior.
//!
//! The subprocess scenarios run the real `cute-dbt` binary
//! (`common::run_cli`). Exit-code and the shared stderr Thens are
//! reused from `report_generation.rs` / `fail_closed.rs` (they read
//! `world.last_exit_code` / `world.last_stderr`); the explore-specific
//! Thens here read the pages captured into `world.explore_*`.

use std::path::{Path, PathBuf};

use cucumber::{given, then, when};

use super::super::common;
use super::World;

/// A collision-free explore out-dir under `CARGO_TARGET_TMPDIR`,
/// cleared before each use so a stale page from a previous run can
/// never satisfy a Then.
fn fresh_out_dir(stem: &str) -> PathBuf {
    let dir = common::tmp(stem);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Run `cute-dbt explore` against `manifest`, capturing exit code,
/// stderr, and (when written) both pages into the `World`.
pub fn run_explore(world: &mut World, manifest: &Path, out_dir: PathBuf) {
    run_explore_with(world, manifest, out_dir, &[], None);
}

/// Run `cute-dbt explore` with extra arguments (the cute-dbt#106
/// `--pr-diff` / `--project-root` change-context surface) and an
/// optional working directory (a relative `--project-root` is
/// existence-validated by clap, so its sub-dir must exist relative to
/// the subprocess cwd — the `pr_diff_scoping` precedent). Captures
/// exit code, stderr, and (when written) both pages into the `World`.
pub fn run_explore_with(
    world: &mut World,
    manifest: &Path,
    out_dir: PathBuf,
    extra_args: &[String],
    cwd: Option<&Path>,
) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args([
        "explore",
        "--manifest",
        common::s(manifest),
        "--out-dir",
        common::s(&out_dir),
    ]);
    cmd.args(extra_args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd.output().expect("the cute-dbt binary spawns");
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.explore_dag_html = std::fs::read_to_string(out_dir.join("dag.html")).ok();
    world.explore_tests_html = std::fs::read_to_string(out_dir.join("tests.html")).ok();
    world.explore_out_dir = Some(out_dir);
}

// --- Given ----------------------------------------------------------

#[given("an explore manifest file that is not valid JSON")]
fn given_broken_manifest(world: &mut World) {
    let path = common::tmp("explore_broken.json");
    std::fs::write(&path, "this is not json").expect("write the broken manifest");
    world.explore_manifest_path = Some(path);
}

#[given("an explore manifest whose dbt_schema_version is below the 1.8 floor")]
fn given_pre_floor_manifest(world: &mut World) {
    let path = common::tmp("explore_old_schema.json");
    std::fs::write(
        &path,
        r#"{"metadata":{"dbt_schema_version":"https://schemas.getdbt.com/dbt/manifest/v11.json"}}"#,
    )
    .expect("write the pre-1.8 manifest");
    world.explore_manifest_path = Some(path);
}

// --- When -----------------------------------------------------------

#[when("I run cute-dbt with no arguments")]
fn run_bare(world: &mut World) {
    let output = common::run_cli(&[]);
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
}

#[when("I run cute-dbt explore with --manifest current.json --out-dir explore/")]
fn run_explore_on_fixture(world: &mut World) {
    let manifest = common::fixture("jaffle-shop-current.json");
    let out_dir = fresh_out_dir("explore_cli_pages");
    run_explore(world, &manifest, out_dir);
}

#[when("I run cute-dbt explore against that manifest")]
fn run_explore_on_prepared_manifest(world: &mut World) {
    let manifest = world
        .explore_manifest_path
        .clone()
        .expect("a Given prepared the explore manifest");
    let out_dir = fresh_out_dir("explore_cli_failclosed");
    run_explore(world, &manifest, out_dir);
}

// --- Then -----------------------------------------------------------
//
// "the exit code is 0" / "is non-zero" / "is 2" and the stderr
// remediation Thens are shared steps (report_generation.rs /
// pr_diff_scoping.rs / fail_closed.rs) — they read the same
// `world.last_exit_code` / `world.last_stderr` the explore Whens set,
// so redefining them here would be an ambiguous-step error.

#[then(regex = r#"^stderr lists the subcommands "([^"]+)" and "([^"]+)"$"#)]
fn stderr_lists_subcommands(world: &mut World, first: String, second: String) {
    for verb in [&first, &second] {
        assert!(
            world.last_stderr.contains(verb.as_str()),
            "the usage error must list the {verb:?} subcommand: {}",
            world.last_stderr,
        );
    }
}

#[then(regex = r#"^the explore out directory contains "([^"]+)"$"#)]
fn out_dir_contains(world: &mut World, filename: String) {
    let dir = world.explore_out_dir.as_ref().expect("explore ran");
    let page = dir.join(&filename);
    assert!(
        page.exists(),
        "{} must exist after explore; stderr={}",
        page.display(),
        world.last_stderr,
    );
}

#[then("neither explore page contains external resource references")]
fn explore_pages_are_self_contained(world: &mut World) {
    let dag = world.explore_dag_html.as_ref().expect("dag.html written");
    let tests = world
        .explore_tests_html
        .as_ref()
        .expect("tests.html written");
    common::assert_no_external_refs(dag);
    common::assert_no_external_refs(tests);
}

#[then("no explore pages are written")]
fn no_explore_pages_written(world: &mut World) {
    let dir = world.explore_out_dir.as_ref().expect("explore ran");
    assert!(
        !dir.join("dag.html").exists() && !dir.join("tests.html").exists(),
        "a fail-closed explore run must write no pages (out dir: {})",
        dir.display(),
    );
}
