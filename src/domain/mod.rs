//! Domain types and pure computation.
//!
//! Depends on `std` and `serde` derive only. No I/O, no parser libraries,
//! no `clap`, no `askama`. POD-only (owned data, constructors, no method
//! machinery beyond what the run loop calls). The single-crate
//! `non-mirror-guard` cannot directly enforce the inward-dependency
//! discipline (one crate cannot fail-to-compile on an inward `use`), so
//! `tests/domain_clean_arch.rs` greps `src/domain/**/*.rs` for
//! `use crate::adapters` to give ADR-1 a build-break.
//!
//! Filled across PRs 3 / 5 / 6:
//!
//! - **PR 3 (#4)** — core types: [`Manifest`], [`Node`], [`UnitTest`],
//!   [`CteGraph`] / [`CteNode`] / [`CteEdge`], [`EdgeType`],
//!   [`ModifiedSet`], [`PreflightError`] (`#[non_exhaustive]`, 4
//!   variants).
//! - **PR 5 (#7)** — `state` submodule additions: `StateModifier`
//!   trait (object-safe, deliberately not `Send + Sync`),
//!   `BodyChecksumModifier`, `StateComparator` (`body_only`,
//!   `modified_set`, `in_scope_unit_tests`), `InScopeSet`, and
//!   `resolve_target_model` (bare `model:` name → full node).
//! - **PR 6 (#8)** — `preflight::preflight_compiled` (the Stage-2
//!   compiled-SQL presence check — runs AFTER scope selects the
//!   in-scope set) and `state::BANNER_EMPTY_SCOPE` (the shared
//!   empty-scope banner constant).
//! - **PR C (#30)** — `state::ModelInScopeSet` (explorer mode: models
//!   targeted by in-scope unit tests plus modified models with zero
//!   unit tests); `StateComparator::models_in_scope`; widened
//!   `PreflightError::NotCompiled.unit_test: Option<String>`.

// `domain::manifest::Manifest` / `unit_test::UnitTest` / `cte::CteGraph`
// are the cleanest names from inside the module (the module name
// disambiguates), but clippy::pedantic's `module_name_repetitions`
// objects at the re-export site. The module names ARE the type
// categories — repetition is intentional, so silence the lint locally.
#![allow(clippy::module_name_repetitions)]

pub mod cell_diff;
pub mod config;
pub mod cte;
pub mod manifest;
pub mod path;
pub mod pr_diff;
pub mod preflight;
pub mod scope;
pub mod state;
pub mod unit_test;
pub mod unit_test_table;
pub mod unit_test_yaml;

pub use cell_diff::{
    CellChange, ColumnStatus, DiffColumn, FixtureTableDiff, NamedTableDiff, RowChange,
    RowChangeKind, UnitTestDataDiff, diff_fixture_tables, reconstruct_external_fixture_diff,
    reconstruct_table_diffs,
};
pub use config::{AnalysisConfig, DEFAULT_REPORT_TITLE, ReportConfig};
pub use cte::{CteEdge, CteGraph, CteNode, EdgeType, Span};
pub use manifest::{Checksum, DependsOn, Manifest, ManifestMetadata, Node, NodeConfig, NodeId};
pub use path::{match_changed_path, normalize_path};
pub use pr_diff::{
    BlockDiff, DiffLine, DiffLineKind, FileHunks, Hunk, NormalizedDiffIndex, PrDiff,
    reconstruct_block_diffs, reconstruct_model_sql_diffs, refine_changed_by_hunks, ws_equal,
};
pub use preflight::{PreflightError, preflight_compiled};
pub use scope::{ScopeInput, ScopeSelection, select_in_scope};
pub use state::{
    BANNER_EMPTY_SCOPE, BodyChecksumModifier, ConfigsModifier, ContractModifier, InScopeSet,
    MacrosModifier, ModelInScopeSet, ModifiedSet, ModifierKind, RelationModifier, StateComparator,
    StateModifier, resolve_target_model,
};
pub use unit_test::{UnitTest, UnitTestExpect, UnitTestGiven};
pub use unit_test_table::{
    Cell, CellValue, FixtureFormat, FixtureTable, TableRow, effective_fixture_format,
    external_fixture_table, normalize_fixture_file_text, parse_block_dict_rows, parse_csv_rows,
    parse_inline_flow_row, table_from_manifest_rows, table_from_yaml_fragment, type_cell_scalar,
    type_cell_value, type_csv_token,
};
pub use unit_test_yaml::{UnitTestYamlBlock, extract_unit_test_block};
