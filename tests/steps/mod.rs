//! Step-definition modules — one per feature file under `features/`,
//! plus a small set of synthetic-Manifest builders and the cucumber
//! `World` struct that carries scenario state.
//!
//! Submodule layout mirrors the feature file names verbatim so a
//! reviewer reading `features/<f>.feature` can `tests/steps/<f>.rs` in
//! one hop. The shared `builders` module owns synthetic Manifest /
//! Node / UnitTest construction so step files stay focused on
//! Given/When/Then prose.

// tracked: cute-dbt#331 — these review/skill step modules drive
// `cute-dbt` through the Unix-only shim harness (`common::TestRepo`),
// so they (and the `World` fields they use) are `#[cfg(unix)]`-gated.
// The bdd binary still COMPILES on the windows-latest job
// (cute-dbt#308/#316); the bdd RUN is a Linux CI job. The portable
// step modules below are unaffected.
#[cfg(unix)]
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
#[cfg(unix)]
pub mod one_command_review;
pub mod pr_diff_scoping;
pub mod project_definition;
pub mod report_generation;
#[cfg(unix)]
pub mod review_compile;
#[cfg(unix)]
pub mod review_pr_anchor;
#[cfg(unix)]
pub mod review_scope_variants;
pub mod unit_test_format_coverage;
pub mod unit_test_yaml;
pub mod world;
pub mod zero_egress;

pub use world::World;
