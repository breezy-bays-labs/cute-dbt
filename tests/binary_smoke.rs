//! Binary-smoke integration test.
//!
//! Spawns the compiled `cute-dbt` binary via `CARGO_BIN_EXE_cute-dbt` and
//! asserts it exits successfully on a no-args invocation. This is the
//! dry-rs coverage-trap pattern: `cargo llvm-cov` instruments subprocess
//! executions when run via `cargo llvm-cov nextest`, so this test covers
//! `main` for the bootstrap coverage gate.
//!
//! Real behavioral coverage of the run loop lands with PR 6 (#TBD); the
//! placeholder `cli::run` keeps this test passing through bootstrap.

use std::process::Command;

#[test]
fn binary_runs_and_exits_success() {
    let bin = env!("CARGO_BIN_EXE_cute-dbt");
    let status = Command::new(bin)
        .status()
        .expect("failed to spawn cute-dbt binary");
    assert!(
        status.success(),
        "cute-dbt placeholder exit was not success: {status:?}"
    );
}
