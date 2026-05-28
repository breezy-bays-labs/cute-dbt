//! Cucumber-rs ATDD outer loop — the executable acceptance contract for
//! cute-dbt v0.1.
//!
//! This test binary uses `harness = false` (see `Cargo.toml`
//! `[[test]] name = "bdd"`): cucumber owns scenario dispatch instead of
//! libtest. As a consequence the binary is **not** nextest-compatible
//! and must be invoked via `cargo test --test bdd`. The companion
//! `.cargo/mutants.toml` sets `test_tool = "cargo"` and explicitly
//! enumerates the kill-detection test targets so `bdd` is excluded
//! from per-mutant runs.
//!
//! The feature-file count under `features/` is pinned by the
//! `feature-count` CI job; every scenario in those files has a step
//! definition here, organised one module per feature file under
//! `tests/steps/`.

#[path = "common/mod.rs"]
mod common;

mod steps;

use cucumber::World as _;

fn main() {
    // Single-threaded run: every scenario that spawns the `cute-dbt`
    // subprocess writes to a per-scenario filename under
    // `CARGO_TARGET_TMPDIR`, but a few share underlying fixtures, and
    // serial execution keeps the failure messages linear for the BDD
    // contract (concurrency is not the property under test).
    futures::executor::block_on(
        steps::World::cucumber()
            .max_concurrent_scenarios(1)
            .run_and_exit("features"),
    );
}
