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
pub mod check_config;
pub mod checks;
pub mod config;
pub mod cte;
pub mod experimental;
pub mod findings_envelope;
pub mod governance;
pub mod grain;
pub mod macro_lens;
pub mod manifest;
pub mod model_yaml;
pub mod path;
pub mod pr_dag;
pub mod pr_diff;
pub mod preflight;
pub mod project_def;
pub mod scope;
pub mod seed_card;
pub mod state;
pub mod unit_test;
pub mod unit_test_table;
pub mod unit_test_yaml;
pub mod vars;

pub use cell_diff::{
    CellChange, ColumnStatus, DiffColumn, FixtureTableDiff, NamedTableDiff, RowChange,
    RowChangeKind, UnitTestDataDiff, diff_fixture_tables, reconstruct_external_fixture_diff,
    reconstruct_table_diffs,
};
pub use check_config::{
    CheckConfigError, CheckPolicy, CheckPragma, ChecksConfig, ChecksMode, SuppressEntry,
    SuppressRule, apply_check_policy, check_by_id, resolve_check_policy, scan_pragmas,
};
pub use checks::{
    CheckContext, CheckId, DegradedBacking, Evidence, Finding, HeuristicId, HeuristicSpec,
    Instrument, Suppression, SuppressionSource, Tier, Verdict, check_page_markdown,
    checks_index_markdown, evaluate_all, filter_for_display, model_findings, registry_toml,
    resolve_supersedes, supersedes_is_acyclic,
};
pub use config::{AnalysisConfig, DEFAULT_REPORT_TITLE, PrConfig, PrRef, ReportConfig};
pub use cte::{
    CteEdge, CteGraph, CteNode, EdgeType, JoinKeyPair, LeftJoinFact, Span, SubqueryFact,
    SubqueryKind,
};
pub use experimental::{
    DEFAULT_MACRO_BODY_CAP, EnabledExperiments, Experiment, ExperimentalConfig, ExperimentalError,
    parse_experimental_env, resolve_experimental_config,
};
pub use findings_envelope::{
    DiffContext, EnvelopeFinding, EnvelopeMetadata, EnvelopeScope, FindingAnchor, FindingsEnvelope,
    ID_STABILITY, SCHEMA_VERSION, has_total_uncovered,
};
pub use governance::{
    BlastRadius, BreakingReason, ColumnMetaTags, ConstraintSupport, ContractChange, ContractClass,
    ContractColumnDiff, DepDate, GovChip, GovernanceFacts, GroupChip, MetaPair, ModelMetaTags,
    backing_test_for, backing_test_for_columns, classify_contract, constraint_support,
    exposures_reachable_from, gather_governance,
};
pub use grain::{GrainKind, GrainSignal, model_grain_signals, test_is_enabled};
pub use macro_lens::{
    MacroFocusSet, changed_macros_baseline, changed_macros_pr_diff, macro_blast_radius,
    macro_focus_set,
};
pub use manifest::{
    Checksum, ColumnFacts, Constraint, ConstraintKind, DependsOn, DisabledEntry, Exposure, Group,
    MacroIdentity, Manifest, ManifestMetadata, Node, NodeConfig, NodeId, Owner, SourceNode,
    TestMetadata, TestSeverity, UniqueKey,
};
pub use model_yaml::ModelYamlOutcome;
pub use path::{match_changed_path, normalize_path};
pub use pr_dag::{PrDagEdge, PrDagGraph, PrDagNode, PrDagState, compute_pr_dag};
pub use pr_diff::{
    BlockDiff, DiffLine, DiffLineKind, FileHunks, Hunk, NormalizedDiffIndex, PrDiff,
    ReverseApplyError, attach_model_yaml_diffs, diff_lines, raw_hunk_lines,
    reconstruct_block_diffs, reconstruct_macro_sql_diff, reconstruct_model_sql_diffs,
    refine_changed_by_hunks, reverse_apply, ws_equal,
};
pub use preflight::{PreflightError, preflight_compiled};
// `project_def::Span` is deliberately NOT re-exported here — `cte::Span`
// already owns the bare name at the domain root; consumers address the
// YAML source span as `project_def::Span`.
pub use project_def::{
    ConfigAttribution, ConfigLeafPath, ConfigTree, HookChangeFacts, HookManifestPresence,
    HookOperation, HookOperations, ProjectChange, ProjectChangeCategory, ProjectChangePanel,
    ProjectDefinition, ProjectFacts, ProjectFallbackReason, attach_hook_facts,
    attribute_config_tree_changes, diff_project_definitions, hook_operations,
};
pub use scope::{
    ScopeInput, ScopeSelection, all_models, changed_models, select_in_scope, select_seeds_in_scope,
    widen_with_config_attributions,
};
pub use seed_card::SeedCard;
pub use state::{
    BANNER_EMPTY_SCOPE, BodyChecksumModifier, ConfigsModifier, ContractModifier, InScopeSet,
    MacrosModifier, ModelInScopeSet, ModifiedSet, ModifierKind, RelationModifier, SeedInScopeSet,
    StateComparator, StateModifier, build_seed_cards, resolve_target_model, resolve_tested_model,
};
pub use unit_test::{UnitTest, UnitTestExpect, UnitTestGiven, UnitTestOverrides};
pub use unit_test_table::{
    Cell, CellValue, FixtureFormat, FixtureTable, TableRow, effective_fixture_format,
    external_fixture_table, normalize_fixture_file_text, parse_block_dict_rows, parse_csv_rows,
    parse_inline_flow_row, table_from_manifest_rows, table_from_yaml_fragment, type_cell_scalar,
    type_cell_value, type_csv_token,
};
pub use unit_test_yaml::{UnitTestYamlBlock, extract_model_block, extract_unit_test_block};
pub use vars::{
    MacroVarHit, VarAnalysis, VarAttribution, VarChangeFacts, VarEdit, VarPrecedence, VarReference,
    VarScanFootprint, VarTier, attach_var_facts, attribute_var_changes, changed_vars,
    resolve_project_var,
};
