//! Step-definition modules — one per feature file under `features/`,
//! plus a small set of synthetic-Manifest builders and the cucumber
//! `World` struct that carries scenario state.
//!
//! Submodule layout mirrors the feature file names verbatim so a
//! reviewer reading `features/<f>.feature` can `tests/steps/<f>.rs` in
//! one hop. The shared `builders` module owns synthetic Manifest /
//! Node / UnitTest construction so step files stay focused on
//! Given/When/Then prose.

// cute-dbt#331 — the review/skill step modules drive `cute-dbt` through
// the shim harness (`common::TestRepo`), now cross-platform (the shims
// are renamed binary copies, not `#!/bin/sh` scripts), so they no longer
// need a `#[cfg(unix)]` gate. The bdd RUN remains a Linux CI job; these
// modules simply compile (and would run) on every platform.
pub mod agent_skill;
pub mod builders;
pub mod cell_table_diff;
pub mod check_selection;
pub mod config;
pub mod consumer_report_contract;
pub mod coverage_checks;
pub mod cte_rendering;
pub mod diff_scoping;
pub mod explore_change_context;
pub mod explore_cli;
pub mod explore_full_manifest;
pub mod explore_js_contract;
pub mod explore_lineage_dag;
pub mod explore_model_detail;
pub mod explore_test_badges;
pub mod explore_view_toggle;
pub mod fail_closed;
pub mod incremental_models;
pub mod macro_perspective;
pub mod one_command_review;
pub mod pr_diff_scoping;
pub mod project_definition;
pub mod report_generation;
pub mod review_compile;
pub mod review_pr_anchor;
pub mod review_scope_variants;
pub mod unit_test_format_coverage;
pub mod unit_test_yaml;
pub mod world;
pub mod zero_egress;

pub use world::World;
