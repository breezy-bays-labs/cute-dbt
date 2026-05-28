//! Step-definition modules — one per feature file under `features/`,
//! plus a small set of synthetic-Manifest builders and the cucumber
//! `World` struct that carries scenario state.
//!
//! Submodule layout mirrors the feature file names verbatim so a
//! reviewer reading `features/<f>.feature` can `tests/steps/<f>.rs` in
//! one hop. The shared `builders` module owns synthetic Manifest /
//! Node / UnitTest construction so step files stay focused on
//! Given/When/Then prose.

pub mod builders;
pub mod config;
pub mod consumer_report_contract;
pub mod cte_rendering;
pub mod diff_scoping;
pub mod fail_closed;
pub mod pr_diff_scoping;
pub mod report_generation;
pub mod unit_test_format_coverage;
pub mod unit_test_yaml;
pub mod world;
pub mod zero_egress;

pub use world::World;
