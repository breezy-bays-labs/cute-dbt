//! The cucumber `World` — per-scenario mutable state.
//!
//! Cucumber constructs a fresh `World` via `Default::default()` at the
//! start of every scenario, so this struct intentionally has only
//! `Option<…>` and `HashMap<…>` fields. Scenario steps fill in the
//! pieces they need; later steps read what earlier steps wrote.

use std::collections::HashMap;
use std::path::PathBuf;

use cute_dbt::domain::{CteGraph, EdgeType, InScopeSet, Manifest, ModelInScopeSet};

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    // --- Subprocess execution -------------------------------------------
    /// Filename inside `CARGO_TARGET_TMPDIR` that the next subprocess
    /// scenario will write `--out` to.
    pub out_path: Option<PathBuf>,

    /// Exit code of the last `cute-dbt` subprocess invocation.
    pub last_exit_code: Option<i32>,

    /// Captured stderr of the last subprocess invocation.
    pub last_stderr: String,

    /// Contents of the report file at `out_path`, if the subprocess
    /// wrote one. `None` when the subprocess failed closed.
    pub report_html: Option<String>,

    // --- In-memory manifest state (diff_scoping) ------------------------
    /// Synthetic `current` manifest built by Given steps.
    pub current_manifest: Option<Manifest>,

    /// Synthetic `baseline` manifest built by Given steps.
    pub baseline_manifest: Option<Manifest>,

    /// Most-recent `in_scope_unit_tests` result.
    pub last_in_scope: Option<InScopeSet>,

    /// Most-recent `models_in_scope` result.
    pub last_models_in_scope: Option<ModelInScopeSet>,

    /// Side facts the scenario wants to assert later. Keyed by model
    /// name → list of unit test names targeting it.
    pub model_to_tests: HashMap<String, Vec<String>>,

    /// The most-recently-named model in the current scenario — set by
    /// every model-naming Given so a later step ("its unit test ...
    /// was modified") can recover the target model without the
    /// .feature having to restate the model name.
    pub last_named_model: Option<String>,

    /// The most-recently-named unit test in the current scenario — set
    /// when an assertion step locates a unit test in the rendered
    /// payload, so a follow-on shape-assertion step
    /// (`unit_test_format_coverage.feature`) can look the test back
    /// up without the .feature having to restate the test name.
    pub last_named_unit_test: Option<String>,

    /// Selector for which committed fixture pair the next subprocess
    /// `When` step should use. Set by per-scenario Givens whose
    /// English wording does not uniquely determine the fixture (two
    /// scenarios in `fail_closed.feature` whose `When` clauses share
    /// the `current.json + baseline.json` pattern but expect different
    /// actual fixtures). `None` ⇒ default
    /// `jaffle-shop-current.json` + `jaffle-shop-baseline.json`.
    pub fixture_choice: Option<FixtureChoice>,

    // --- CTE rendering --------------------------------------------------
    /// Parsed CTE graph for the current `cte_rendering` scenario.
    pub last_cte_graph: Option<CteGraph>,

    /// Edge-type currently being checked by the scenario outline.
    pub last_edge_type: Option<EdgeType>,

    // --- PR-diff scoping (pr_diff_scoping) ------------------------------
    /// Changed-file paths configured by a `Given a PR diff that changes …`
    /// step. The When step synthesizes a `git diff --unified=0` patch
    /// covering them (a whole-file hunk for a YAML file that declares
    /// tests — plus the working-tree YAML written under
    /// `<workdir>/<project-root>/` — and a minimal hunk for SQL / non-dbt
    /// files), then passes `--pr-diff @<patch>` (cute-dbt#96).
    pub changed_files: Vec<String>,

    /// An explicit patch file written by a Given (the malformed-diff
    /// scenario) that the When passes verbatim as `@<path>` instead of
    /// synthesizing one. `None` ⇒ synthesize from `changed_files`.
    pub explicit_patch: Option<PathBuf>,

    /// cute-dbt#96 Step 2: block-targeting directives for the synthesized
    /// diff. Empty ⇒ the synthesizer uses the whole-file footprint
    /// (slice-A behavior — every declared block touched). When a YAML file
    /// has targets, the synthesizer still writes its working-tree content
    /// (so the #69 slicer can compute block spans) but places hunks per
    /// these directives instead: inside a named test's block, in the
    /// out-of-block (`models:`) region, as a pure deletion, or as a stale
    /// whole-file hunk whose `+` lines drift from the working tree.
    pub block_targets: Vec<BlockTarget>,

    /// cute-dbt#111: model-SQL-diff directives. When a model's `.sql` is a
    /// changed file, the synthesizer emits a hunk over the model's
    /// manifest `raw_code` (no working-tree file — the SQL diff reads
    /// `raw_code` from the manifest, not disk). Keyed by the model's
    /// `original_file_path`; `kind` says whether the hunk is a real edit,
    /// a stale (drifted) edit, or a whitespace-only re-indent.
    pub model_sql_targets: Vec<ModelSqlTarget>,

    /// cute-dbt#80: git-rename directives. The synthesizer emits the
    /// `diff --git` + `similarity index` + `rename from`/`rename to`
    /// extended-header block real `git diff` produces for a detected
    /// rename — with NO `---`/`+++` headers and no hunks for a pure
    /// rename (`edited: false`), or followed by the usual file headers
    /// and a minimal hunk for a rename-with-edit (`edited: true`).
    pub renames: Vec<RenameDirective>,

    // --- Cell-level data-table diff (cell_table_diff) -------------------
    /// cute-dbt#98: the scenario's fixture-cell-diff plan, set by a
    /// `Given a unit test … with a … given row …` step and consumed by the
    /// `When the PR …` step. Carries everything the self-contained
    /// `cell_table_diff.rs` harness needs to build a model + a unit test
    /// with inline fixture rows AND to synthesize a `--unified=0` patch that
    /// edits ONE fixture row in a chosen way. `None` outside the cell-diff
    /// feature.
    pub cell_diff_plan: Option<CellDiffPlan>,

    /// cute-dbt#145: the incremental-model scenario accumulator. Filled by
    /// the `incremental_models.feature` Givens (which model is incremental,
    /// which is modified, which tests carry an `overrides.macros.is_incremental`
    /// and which `given` inputs), then consumed by the self-contained
    /// `When I render the incremental report` step to build + serialize a
    /// synthetic current/baseline pair (with the wire-shape injection the
    /// flat-domain serialization can't express) and run the subprocess.
    pub incremental_plan: IncrementalPlan,

    /// cute-dbt#169: the coverage-check scenario accumulator. Filled by
    /// the `coverage_checks.feature` Givens (which models declare a
    /// `config.unique_key` and which uniqueness data tests exist), then
    /// consumed by the self-contained `When I render the coverage report`
    /// step to build + serialize a synthetic current/baseline pair (with
    /// the flat-config + test-node wire-shape injection) and run the
    /// subprocess. The Thens assert the embedded payload's `findings`.
    pub coverage_plan: CoveragePlan,
}

/// A cute-dbt#145 incremental scenario plan — the models (each with its
/// `config.materialized`), which models are modified-vs-baseline, and the
/// unit tests (each with an optional `overrides.macros.is_incremental` mode
/// and its `given` inputs). Built up by the feature's Given steps and
/// consumed once by the `When`.
#[derive(Debug, Default, Clone)]
pub struct IncrementalPlan {
    /// `(bare model name, config.materialized value)` per model.
    pub models: Vec<(String, String)>,
    /// Bare names of models the scenario marks modified vs the baseline.
    pub modified: Vec<String>,
    /// Unit tests the scenario declares.
    pub tests: Vec<IncrementalTest>,
}

/// One unit test in an [`IncrementalPlan`].
#[derive(Debug, Default, Clone)]
pub struct IncrementalTest {
    /// Bare unit-test name (`test_order_events_incremental`).
    pub name: String,
    /// Bare target model name (the `model:` field).
    pub target: String,
    /// `overrides.macros.is_incremental`, when the scenario sets it
    /// (`Some(true)` incremental branch / `Some(false)` full refresh);
    /// `None` when no override is declared.
    pub mode: Option<bool>,
    /// Ordered `given` input strings (`this`, `ref('stg_orders')`, …).
    pub givens: Vec<String>,
}

/// A cute-dbt#169 coverage-check scenario plan — the models (each with
/// an optional `config.unique_key` wire value) and the uniqueness data
/// tests the scenario declares. Every declared model is modified vs the
/// baseline (coverage scenarios always want the model in scope).
#[derive(Debug, Default, Clone)]
pub struct CoveragePlan {
    /// `(bare model name, config.unique_key wire value)` per model —
    /// `serde_json::Value::Null` when the scenario declares no key.
    pub models: Vec<(String, serde_json::Value)>,
    /// Uniqueness data tests the scenario declares.
    pub tests: Vec<CoverageDataTest>,
}

/// One uniqueness data test in a [`CoveragePlan`].
#[derive(Debug, Clone)]
pub struct CoverageDataTest {
    /// Bare target model name the test is attached to.
    pub target: String,
    /// `true` for `dbt_utils.unique_combination_of_columns`; `false`
    /// for the dbt-core `unique` generic test.
    pub combo: bool,
    /// The asserted column set (one column for `unique`).
    pub columns: Vec<String>,
    /// `config.enabled` on the test node.
    pub enabled: bool,
}

/// A cute-dbt#98 cell-diff scenario plan — the test name, fixture format,
/// the NEW (working-tree) fixture rows, and (after the `When`) how a hunk
/// edits one of them.
#[derive(Debug, Clone)]
pub struct CellDiffPlan {
    /// The unit test's bare name (`test_dim_users`).
    pub test: String,
    /// The fixture format (`dict` or `csv`).
    pub format: String,
    /// The NEW (working-tree) given rows, as `(column, value)` pairs per
    /// row in column order. The manifest carries these verbatim; the
    /// synthesized working-tree YAML renders them; the chosen edit rewrites
    /// the OLD side of one cell/row.
    pub new_rows: Vec<Vec<(String, String)>>,
}

/// A git-rename directive for the synthesized diff (cute-dbt#80). Paths
/// are repo-relative, exactly as `git diff` names them in its
/// `rename from`/`rename to` extended headers.
#[derive(Debug, Clone)]
pub struct RenameDirective {
    /// The old (pre-rename) path.
    pub from: String,
    /// The new (post-rename) path.
    pub to: String,
    /// `false` ⇒ a pure rename (similarity 100%, no hunks); `true` ⇒ a
    /// rename-with-edit (the new path also gets file headers + a minimal
    /// hunk, the real `git diff` shape for similarity < 100%).
    pub edited: bool,
}

/// A model-SQL-diff hunk directive for the synthesized diff (cute-dbt#111).
/// `ofp` is the model's `.sql` `original_file_path`; `kind` says how the
/// hunk edits the model's `raw_code`.
#[derive(Debug, Clone)]
pub struct ModelSqlTarget {
    /// The model's `.sql` `original_file_path` the hunk lands in.
    pub ofp: String,
    /// How the hunk edits the model's `raw_code`.
    pub kind: ModelSqlTargetKind,
}

/// How a [`ModelSqlTarget`] edits a model's `raw_code`.
#[derive(Debug, Clone)]
pub enum ModelSqlTargetKind {
    /// A real value change to one `raw_code` line — the `+` matches the
    /// working `raw_code` (N7b-aligned, touches the file ⇒ a real SQL diff).
    Edit,
    /// A whitespace-only re-indent of one `raw_code` line — the `+` matches
    /// the working `raw_code`, the `-` differs only in leading whitespace
    /// ⇒ no SQL diff (plain view).
    Whitespace,
    /// A hunk whose `+` lines drift from the model's `raw_code` (revision
    /// drift) ⇒ N7b fails ⇒ no SQL diff (plain view).
    Stale,
}

/// A block-precise hunk-placement directive for the synthesized diff
/// (cute-dbt#96 Step 2). `yaml` is the declaring file the hunk lands in;
/// `kind` says where/how.
#[derive(Debug, Clone)]
pub struct BlockTarget {
    /// The changed YAML file this directive places a hunk in.
    pub yaml: String,
    /// Where/how the hunk lands.
    pub kind: BlockTargetKind,
}

/// How a [`BlockTarget`] places its hunk.
#[derive(Debug, Clone)]
pub enum BlockTargetKind {
    /// An in-block edit of the named test — the hunk lands inside that
    /// test's block span and its `+` line matches the working tree
    /// (N7b-aligned, touches the block ⇒ stays `updated`).
    EditsTest(String),
    /// A pure-deletion hunk (zero new-side lines) inside the named test's
    /// block — exercises the point-touch overlap path.
    DeletesFromTest(String),
    /// An edit in the file's out-of-block (`models:`) region, above every
    /// test block — touches no block ⇒ every test narrows to `context`.
    EditsOutside,
    /// A whole-file hunk whose `+` lines do not match the working tree
    /// (revision drift) — every block's N7b alignment fails ⇒ cute-dbt
    /// degrades to the file-granular `updated` label.
    Stale,
}

/// Which committed fixture pair the next subprocess `When` step
/// should use, when the English wording does not uniquely identify
/// it. See `World::fixture_choice`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureChoice {
    /// `jaffle-shop-no-test-uncompiled.json` + `jaffle-shop-baseline.json`.
    /// Used by the "modified model with zero unit tests and no
    /// compiled_code" scenario.
    NoTestUncompiled,

    /// `jaffle-shop-current.json` + `jaffle-shop-baseline.json`. The
    /// committed pair is fully compiled, so an out-of-scope
    /// uncompiled assertion is vacuously satisfied (no in-scope
    /// uncompiled nodes) and the run exits 0. Set by
    /// `given_out_of_scope_uncompiled` in `fail_closed.rs`.
    OutOfScopeUncompiled,
}
