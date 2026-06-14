//! CLI surface: clap argument parsing, the two named run-loop
//! compositions, and the mapping from a run outcome to a process
//! [`ExitCode`].
//!
//! Since cute-dbt#100 the CLI is verb-structured and this module owns
//! one named composition per verb — `execute_report` and
//! `execute_explore` — dispatched from [`run`]; deliberately two
//! functions, never an if-branch inside one run loop.
//!
//! **`execute_report`** is composed as named stages —
//! `load_current` → `resolve_scope_input` → `gather_project_facts` →
//! `select_in_scope` (+ the cute-dbt#267 config-tree widening) →
//! `preflight_compiled` → `parse_ctes` → `gather_authoring_yaml` →
//! `render` (`ARCHITECTURE.md` §3, §6). `resolve_scope_input` picks
//! between the `--baseline-manifest` and `--pr-diff` scope
//! sources and loads the baseline only on the former path (cute-dbt#85).
//! Composition lives in `cli` by deliberate single-crate design: there
//! is no separate `app` / `usecase` crate. `parse_ctes` is a named
//! no-op call site: each in-scope model parses its own `compiled_code`
//! once during payload assembly inside
//! [`crate::adapters::render::render_report`], so the explicit
//! `parse_ctes` step is purely greppable scaffolding that mirrors the
//! ARCHITECTURE diagram. The `gather_authoring_yaml` step reads each
//! in-scope unit-test's source YAML through the [`ProjectFileReader`]
//! port and slices the authored block — soft-failing per test so a
//! missing file or unsupported manifest never breaks the report
//! (cute-dbt#69). The `render` step invokes the askama renderer.
//!
//! **`execute_explore`** is the cute-dbt#100 walking skeleton —
//! `load_current` → `all_models` → `build_payload` → `render_explore`.
//! Stage-1 pre-flight stays fail-CLOSED (unreadable / pre-v12 manifests
//! abort with remediation); Stage-2 is deliberately fail-OPEN — there
//! is **no** `preflight_compiled` call on this path, and an uncompiled
//! model renders as a "not compiled" node. `PreflightError` keeps its
//! four variants; explore raises no fifth.
//!
//! Four exit codes: `0` success, `1` a run-time failure (a fail-closed
//! manifest or an unwritable output path — no partial report is ever
//! written), `2` an operator usage error (clap rejected the arguments,
//! including a bare `cute-dbt` with no subcommand, or supplying neither
//! or both `report` scope sources — `--baseline-manifest` / `--pr-diff`),
//! and `3` the `report` verb's `--fail-on-uncovered` coverage gate
//! (cute-dbt#386): the report (and any `--findings-out` sidecar) IS
//! written, then the run exits non-zero because the in-scope set carries a
//! Total-tier `Uncovered` finding — distinct from `1` so CI tells a real
//! coverage gap apart from unusable input.

mod args;
mod exit;
mod pr_comments;
mod pr_diff;
mod review;
mod skill;

/// Fuzz seam (cute-dbt#383): drive the **pure** `--pr-diff` unified-diff
/// parser ([`pr_diff::parse_unified_diff`]) with adversarial text.
///
/// The `--pr-diff` patch parser is cute-dbt's highest-risk untrusted-input
/// surface — in CI/PR-review mode arbitrary diff text is fed in. This
/// `#[doc(hidden)]` re-export lets the `tests/fuzz_pr_diff_parser` bolero
/// target (stable Rust, no nightly) feed it random bytes and assert the
/// fail-closed contract: parsing never panics / hangs, only ever returns
/// `Ok(PrDiff)` or `Err(String)`. It targets `parse_unified_diff`, **not**
/// the public `parse_diff` value-parser, so the fuzzed path is pure (no
/// `@file` filesystem I/O). Not part of the v0.x public API surface — it
/// exists solely so a test target outside the crate can reach the private
/// parser, the same internal-reach motivation the `bdd` target has.
///
/// See `.claude/rules/testing.md` (the **Fuzz** rung) for the Q4
/// bring-into-shape context.
#[doc(hidden)]
pub fn fuzz_parse_unified_diff(raw: &str) -> Result<crate::domain::PrDiff, String> {
    pr_diff::parse_unified_diff(raw)
}

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use crate::adapters::explore::{MacroFocus, render_explore};
use crate::adapters::findings_emit::{
    collect_in_scope_findings, envelope_from_findings_anchored, write_sidecar,
};
use crate::adapters::github_annotations::{
    AnnotationLevels, DEFAULT_ANNOTATION_CAP, emit_annotations,
};
use crate::adapters::manifest::{FileManifestSource, load_baseline};
use crate::adapters::project_def::parse as parse_project_definition;
use crate::adapters::project_file::FsProjectFileReader;
use crate::adapters::render::{
    ExternalFixtures, LoadedFixture, MacroLensPayload, PrDagPayload, ScopeSource, build_macro_lens,
    build_payload, index_tests_for_models, render_report_with_externals,
};
use crate::domain::{
    BlockDiff, ChangeAxes, CheckPolicy, CommentsView, ConfigAttribution, DEFAULT_MACRO_BODY_CAP,
    DEFAULT_PR_DAG_NODE_CAP, DEFAULT_REPORT_TITLE, DEFAULT_SEED_ROW_CAP, DepDate,
    EnabledExperiments, EnvelopeScope, Experiment, Finding, FixtureTableDiff, GovernanceFacts,
    HeuristicId, InScopeSet, Manifest, ModelInScopeSet, ModelState, ModelYamlOutcome,
    NamedTableDiff, Node, NodeId, NormalizedDiffIndex, PrConfig, PrRef, PreflightError,
    ProjectChangePanel, ProjectFacts, ProjectFallbackReason, ResolvedAnchor, ScopeInput,
    ScopeSelection, SeedCard, SeedInScopeSet, StateComparator, SuppressRule, SuppressionSource,
    UnitTest, UnitTestDataDiff, UnitTestYamlBlock, VarReference, all_models, all_seeds,
    attach_hook_facts, attach_model_yaml_diffs, attach_var_facts, attribute_config_tree_changes,
    attribute_var_changes, build_seed_cards, changed_macros_baseline, changed_macros_pr_diff,
    changed_models, check_by_id, compute_pr_dag, diff_project_definitions,
    effective_fixture_format, external_fixture_table, extract_model_block, extract_unit_test_block,
    gather_governance, group_comment_threads, has_total_uncovered, hook_operations,
    macro_focus_set, macro_test_consumers, populate_line_counts, pr_dag_lines_from_diff,
    pr_dag_lines_from_raw_code, preflight_compiled, raw_hunk_lines, reconstruct_block_diffs,
    reconstruct_external_fixture_diff, reconstruct_model_sql_diffs, reconstruct_table_diffs,
    refine_changed_by_hunks, resolve_check_policy, resolve_experimental_config,
    resolve_finding_anchor, reverse_apply, scan_pragmas, select_in_scope, select_seeds_in_scope,
    widen_with_config_attributions,
};
use crate::ports::{ManifestSource, ProjectFileReader};

use args::{Cli, Command, ExploreArgs, ReportArgs, validate_argument_conflicts};

/// Exit code for a run-time failure: a fail-closed manifest (Stage-1 or
/// Stage-2) or an unwritable `--out` path.
const EXIT_FAILURE: u8 = 1;

/// Exit code for an operator usage error (clap rejected the arguments).
const EXIT_USAGE: u8 = 2;

/// Exit code for the `--fail-on-uncovered` coverage gate (cute-dbt#386):
/// the run produced its report (and the `--findings-out` sidecar, if
/// requested) successfully, but the in-scope set carries ≥1 Total-tier
/// `Uncovered` finding. Distinct from `EXIT_FAILURE` (a fail-closed manifest
/// — no report written) so CI can tell "a real coverage gap" apart from "the
/// input was unusable".
const EXIT_GATE: u8 = 3;

/// Binary entry point: parse arguments, dispatch the selected verb's
/// composition, and map the outcome to a process exit code.
#[must_use]
pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => return report_arg_error(&err),
    };
    // Post-parse usage validation clap's derive cannot express
    // (cute-dbt#386): `report --findings-out` must differ from `--out`,
    // or the sidecar JSON would clobber the HTML report. Routed through
    // the same exit-2 usage-error path as a parse failure.
    if let Err(err) = validate_argument_conflicts(&cli) {
        return report_arg_error(&err);
    }
    // Per-verb dispatch; every run-time failure is mapped to one
    // stderr message + exit 1 here. `review` wraps its own cli-layer
    // `ReviewError` (cute-dbt#300) alongside the composed report's
    // `RunError`, so its arm converts to the message eagerly. The
    // `report` verb additionally carries the `--fail-on-uncovered` gate
    // outcome (cute-dbt#386): a successful run can still exit
    // `EXIT_GATE` when an in-scope Total-tier coverage gap is present.
    let outcome: Result<ReportOutcome, String> = match &cli.command {
        Command::Review(review_args) => review::execute_review(review_args)
            .map(|()| ReportOutcome::Success)
            .map_err(|failure| failure.message()),
        Command::Report(report) => execute_report(report).map_err(|failure| failure.message()),
        Command::Explore(explore) => execute_explore(explore)
            .map(|()| ReportOutcome::Success)
            .map_err(|failure| failure.message()),
        Command::Skill(skill_args) => skill::execute_skill(skill_args)
            .map(|()| ReportOutcome::Success)
            .map_err(|failure| failure.message()),
    };
    match outcome {
        Ok(ReportOutcome::Success) => ExitCode::SUCCESS,
        Ok(ReportOutcome::UncoveredGate) => ExitCode::from(EXIT_GATE),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// The success-side outcome of a verb run.
///
/// Almost every run is [`ReportOutcome::Success`]; the `report` verb's
/// `--fail-on-uncovered` gate (cute-dbt#386) is the one path that produces
/// its output *and* signals a distinct non-zero exit
/// ([`ReportOutcome::UncoveredGate`]) — a deterministic Total-tier coverage
/// gap, not a fail-closed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReportOutcome {
    /// The run completed; exit `0`.
    Success,
    /// The run wrote its report (and any `--findings-out` sidecar) but the
    /// in-scope set carries a Total-tier `Uncovered` finding and
    /// `--fail-on-uncovered` was set; exit [`EXIT_GATE`].
    UncoveredGate,
}

/// Print a clap parse error and pick its exit code.
///
/// clap routes genuine usage errors to stderr (exit `2`) and a `--help`
/// / `--version` display to stdout (exit `0`); `use_stderr` distinguishes
/// the two.
fn report_arg_error(err: &clap::Error) -> ExitCode {
    let _ = err.print();
    if err.use_stderr() {
        ExitCode::from(EXIT_USAGE)
    } else {
        ExitCode::SUCCESS
    }
}

/// A run-loop failure.
enum RunError {
    /// A fail-closed [`PreflightError`] from Stage-1 (adapter) or
    /// Stage-2 (domain).
    Preflight(PreflightError),
    /// The generated output could not be written — `path` names the
    /// `report` verb's `--out` file or the `explore` verb's `--out-dir`.
    Output {
        /// The output location the operator asked for.
        path: PathBuf,
        /// The underlying I/O failure.
        source: io::Error,
    },
}

impl From<PreflightError> for RunError {
    fn from(err: PreflightError) -> Self {
        Self::Preflight(err)
    }
}

impl RunError {
    /// Wrap an I/O failure with the output path it occurred at.
    fn output(path: &Path, source: io::Error) -> Self {
        Self::Output {
            path: path.to_path_buf(),
            source,
        }
    }

    /// The operator-facing stderr message for this failure.
    fn message(&self) -> String {
        match self {
            Self::Preflight(err) => exit::remediation(err),
            Self::Output { path, source } => format!(
                "cute-dbt: could not write the output to {}: {source}",
                path.display()
            ),
        }
    }
}

/// The named `report` run loop — `load_current` → `resolve_scope_input`
/// → `gather_project_facts` → `select_in_scope` (+ the cute-dbt#267
/// config-tree widening) → `preflight_compiled` → `parse_ctes` →
/// `gather_authoring_yaml` → `render`.
///
/// `resolve_scope_input` runs Stage-1 pre-flight on the baseline manifest
/// only on the `--baseline-manifest` path; the `--pr-diff`
/// path needs no baseline. `gather_project_facts` runs before scope
/// selection because a categorized `dbt_project.yml` config-tree edit
/// widens the selection (cute-dbt#267) — and runs at all only when the
/// `project-state` experiment is enabled (cute-dbt#291): the default
/// run passes default-empty [`ProjectFacts`], so nothing project-state
/// renders and nothing widens. `?` short-circuits before
/// `render`, so a fail-closed manifest never produces a partial
/// `report.html`.
// `too_many_lines`: the report run loop is a linear composition root —
// each gather slice (project-state, governance, macro-lens, the inline-diff
// reconstructions) is one named, already-factored step threaded in sequence.
// Splitting the loop into sub-loops would add indirection that buys nothing
// at this single composition site (the `render_report` rationale, cli edition).
#[allow(clippy::too_many_lines)]
fn execute_report(args: &ReportArgs) -> Result<ReportOutcome, RunError> {
    let current = load_current(args)?;
    let scope_input = resolve_scope_input(args)?;
    // Experimental switch (cute-dbt#289, epic #288): the resolved
    // TOML ∪ env opt-in set. Its first consumer is the project-state
    // gate below (cute-dbt#291).
    let experiments = resolve_enabled_experiments(args);
    // Project-definition facts (cute-dbt#266), gated behind the
    // `project-state` experiment (cute-dbt#291 — the epic #288
    // default-OFF posture; founder 2026-06-12). Enabled: parse the
    // working-tree dbt_project.yml whenever it is present — standing
    // metadata, both scope arms; the categorized "Project definition
    // changed" panel is the diff-gated consumer (PrDiff arm reconstructs
    // the old side by reverse-applying the file's own hunks,
    // drift-guarded; every degrade arm falls back to the Shape-A
    // raw-diff row; fail-open — report generation NEVER fails because
    // of this file). Default (off): default-empty `ProjectFacts` — the
    // STANDING `definition` metadata is gated too, not kept (the
    // cute-dbt#291 Discovery call): the default report is byte-identical
    // to pre-#262 output and dbt_project.yml contributes zero bytes
    // (pinned by `project_state_off_dbt_project_yml_contributes_zero_bytes`
    // in tests/run_loop.rs), which also keeps the default path
    // zero-compute (no file read, no YAML parse). Render reproduces the
    // pre-#266 payload byte-for-byte on default facts and the empty
    // attribution map makes the cute-dbt#267 widening below a no-op —
    // the gate reuses both seams instead of forking the template.
    // Gathered BEFORE scope selection since cute-dbt#267: a categorized
    // config-tree edit carries per-model attributions that widen scope.
    let project_facts = if experiments.is_enabled(Experiment::ProjectState) {
        gather_project_facts(args, &current, &scope_input)
    } else {
        ProjectFacts::default()
    };
    // Config-tree scope widening (cute-dbt#267): models whose fqn falls
    // under an edited `models:` subtree (fusion's get_config_for_fqn
    // prefix descent — TOTAL tier, by-definition change) join the
    // selection by union; their unit tests ride in as context. The one
    // widening category of epic #262 — vars stay contextualize-only.
    let ScopeSelection {
        in_scope,
        models_in_scope,
        changed,
        // cute-dbt#413 Slice B — the per-model `axes` attribution the domain
        // computed (cute-dbt#411 Slice A) is now threaded into the render
        // payload (the Models-lens axis chips + the optgroup grouping key).
        // The `--pr-diff` arm populates an entry for every in-scope model
        // (`axes.keys() == models_in_scope`); the baseline arm produces an
        // empty map (the documented Option-A gap), so baseline goldens stay
        // byte-identical.
        axes,
        // cute-dbt#416 — the per-model NEW/MODIFIED state + the node-less
        // REMOVED model paths, threaded into the render payload (the Models-
        // lens state chips). The `--pr-diff` arm populates a state for every
        // in-scope model and the deleted model paths; the baseline arm
        // produces empty maps (the documented Option-A gap).
        model_states,
        removed_models,
    } = widen_with_config_attributions(
        select_in_scope(&current, &scope_input),
        &current,
        &project_facts.config_attributions,
    );
    // Governance facts (cute-dbt#260, epic #260), gated behind the
    // `governance` experiment (Slice 0 — the same epic #288 default-OFF
    // posture as project-state). Enabled: gather the group/owner header
    // chips for the in-scope models that declare a governance group (a
    // pure pass over the manifest — no file read, no YAML parse). Default
    // (off): the empty `GovernanceFacts::default()` — omitted from the
    // JSON payload and rendering zero DOM via `{%- if has_governance %}`,
    // so the non-experimental golden stays byte-identical. Gathered AFTER
    // scope selection (unlike project-state) because the chips read the
    // resolved in-scope MODEL set, and governance never widens scope. The
    // diff-showcase golden row sets `experimental:"1"` (every experiment),
    // so the grouped playground model surfaces a chip there.
    // cute-dbt#260 Slice 2 — the OLD manifest for per-model contract
    // classification. Available ONLY in `--baseline-manifest` mode (the
    // `ScopeInput::Baseline` arm carries the parsed old `Manifest`); the
    // `--pr-diff` path has NO old manifest — it reconstructs the old side
    // from per-file TEXT hunks, which cannot rebuild the structured
    // contract fields (`classify_contract` needs columns/constraints/
    // config, not text). So contract classification is gated to baseline
    // mode (and, by `gather_governance`'s caller, the `governance`
    // experiment). `None` ⇒ no contract classes ⇒ byte-identical pr-diff
    // goldens + released report. (Slice 2 finding: the diff-showcase
    // `--pr-diff` golden renders no contract drawer — Slice 6 adds a
    // contracted baseline-mode example.)
    let old_manifest = match &scope_input {
        ScopeInput::Baseline { manifest, .. } => Some(manifest.as_ref()),
        ScopeInput::PrDiff { .. } => None,
    };
    let governance_facts = if experiments.is_enabled(Experiment::Governance) {
        // cute-dbt#260 Slice 4 — "today" (UTC civil date) for the
        // deprecation scheduled/elapsed split, computed HERE (the I/O
        // boundary) and threaded into the pure domain so the comparison
        // is deterministic + testable. Far-past/far-future fixture dates
        // keep the golden stable regardless of the real generation date.
        gather_governance(
            &current,
            &models_in_scope,
            old_manifest,
            Some(today_dep_date()),
        )
    } else {
        GovernanceFacts::default()
    };
    // Macro perspective lens (cute-dbt#265, Slice B) — see `gather_macro_lens`.
    // The Slice D inline-body cap is a gen-time knob resolved here at the
    // I/O boundary (`--macro-body-cap` over `[experimental] macro_body_cap`
    // over the default), keeping the render side a pure fn of the cap.
    let macro_body_cap = resolve_macro_body_cap(args);
    let macro_lens = gather_macro_lens(&current, &scope_input, &experiments, macro_body_cap);
    // PR-scope lineage mini-DAG (cute-dbt#404, epic #352) — the focused
    // cross-model subgraph the report puts at the top, gated behind the
    // `pr-scope-mini-dag` experiment (the single gating source lives inside
    // `gather_pr_dag`). It CONSUMES the scope sets (modified ∪ connectors ∪
    // deleted) and the diff/raw_code line counts; it never widens scope. `None`
    // (off, or empty scope) ⇒ omitted from JSON + zero DOM, keeping the
    // non-experimental goldens byte-identical (the `macro_lens` precedent).
    let pr_dag = gather_pr_dag(&current, &scope_input, &experiments, &axes);
    // PR review-comments (cute-dbt#419–#422, epic #353) — the ingested
    // GitHub review threads, anchored onto the rendered diff (the shipped
    // cute-dbt#418 anchoring) and grouped per model, gated behind the
    // `pr-comments` experiment + the `--pr-diff` arm (the single gating source
    // lives inside `gather_pr_comments`). `None` (off, baseline arm, no PR
    // context, or no comments) ⇒ omitted from JSON + the static count
    // container stays empty, keeping every default golden byte-identical (the
    // `pr_dag` precedent).
    let pr_comments = gather_pr_comments(args, &current, &scope_input, &experiments);

    // Stage-2 fail-closed reads the TRUE in-scope set (cute-dbt#91): the
    // widened render set is only for what the report displays. Config-tree
    // widened tests (cute-dbt#267) ARE the true scope, so they preflight.
    preflight_compiled(&current, &in_scope, &models_in_scope)?;
    parse_ctes();
    // The widened render set — every unit test on an in-scope model, not
    // just the in-scope ones — drives BOTH the Authoring-YAML gather and
    // the render payload, so context siblings keep their YAML drawer
    // (cute-dbt#91).
    let render_test_ids = render_test_ids(&current, &models_in_scope);
    let authoring_yaml = gather_authoring_yaml(args, &current, &render_test_ids);
    // External fixture files (cute-dbt#126): read each rendered test's
    // external `given`/`expect` fixture so the report inlines a real grid
    // instead of the #98 affordance. Reads the working tree at generation
    // time only (zero-egress unaffected); soft-fails per fixture.
    let external_fixtures = gather_external_fixtures(args, &current, &render_test_ids);
    // Block-precise narrowing (cute-dbt#96): on the PR-diff path, narrow the
    // file-granular `changed` label down to the tests whose sliced YAML block
    // a diff hunk actually touches. The slice spans (`authoring_yaml`) are
    // already in hand, and `changed′ ⊆ changed` holds because refine only
    // removes ids. Baseline-mode `changed` is already precise (a
    // `StateComparator` struct diff), so refine runs ONLY on the PrDiff arm.
    let changed = match &scope_input {
        ScopeInput::PrDiff { index } => {
            refine_changed_by_hunks(&current, &changed, &authoring_yaml, index)
        }
        ScopeInput::Baseline { .. } => changed,
    };
    // Inline YAML block diffs (cute-dbt#96 concern 2): reconstruct an
    // in-place diff for each test whose own YAML block the diff edited.
    // PrDiff arm only — baseline mode has no hunks to reconstruct from, so
    // the drawer shows the plain authored YAML. Threaded into render
    // exactly like `authoring_yaml` (the slice spans are already in hand).
    let yaml_diffs: HashMap<String, BlockDiff> = match &scope_input {
        ScopeInput::PrDiff { index } => {
            reconstruct_block_diffs(&current, &changed, &authoring_yaml, index)
        }
        ScopeInput::Baseline { .. } => HashMap::new(),
    };
    // Inline model SQL diffs (cute-dbt#111): reconstruct an in-place diff
    // of each in-scope model's RAW `raw_code` whose `.sql` the PR diff
    // changed. PrDiff arm only — baseline mode has no hunks, so the Model
    // SQL section shows the plain raw view. `raw_code` comes from the
    // manifest (no filesystem read needed, unlike the YAML drawer), so this
    // reads `models_in_scope` directly; ADR-3 scope selection is untouched.
    let sql_diffs: HashMap<String, BlockDiff> = match &scope_input {
        ScopeInput::PrDiff { index } => {
            reconstruct_model_sql_diffs(&current, &models_in_scope, index)
        }
        ScopeInput::Baseline { .. } => HashMap::new(),
    };
    // Model-YAML gather (cute-dbt#247): for each in-scope model, slice the
    // authored `models:` entry out of the schema file `patch_path` points
    // at — or record the truthful degrade (no patch_path / no project root
    // / file missing / entry not found) the section renders instead. On
    // the PrDiff arm, attach an inline block diff to each found entry the
    // diff genuinely edited (same gates as the unit-test YAML drawer);
    // baseline mode has no hunks, so the section shows the plain File view.
    let mut model_yaml = gather_model_yaml(args, &current, &models_in_scope);
    if let ScopeInput::PrDiff { index } = &scope_input {
        attach_model_yaml_diffs(&mut model_yaml, index);
    }
    // Seed cards (cute-dbt#350): the seed dual of the model scope, gated
    // behind the `seeds` experiment (epic #350 / epic #288 default-OFF
    // posture — the project-state / governance / macro-lens precedent). This
    // is the SINGLE gating source: when the experiment is off the cards never
    // cross to the render payload, so the "Data tables" section emits zero
    // DOM and every seed-free golden stays byte-identical. Enabled: select
    // the in-scope seeds on EITHER arm (`select_seeds_in_scope` — baseline:
    // changed `checksum`; pr-diff: the `seeds/<name>.csv` is in the diff),
    // read each seed's working-tree CSV into its card (truthful degrade per
    // seed — a card the reader cannot fill keeps `table: None`), and on the
    // pr-diff arm reconstruct the seed CSV's old→new cell-diff from its own
    // hunks (`reconstruct_external_fixture_diff`, the #126 external-fixture
    // machinery — the seed file IS an external tabular file). The render
    // layer (`build_seed_section`) applies the row cap + the honest label.
    let seed_cards = if experiments.is_enabled(Experiment::Seeds) {
        let seeds_in_scope = select_seeds_in_scope(&current, &scope_input);
        let seed_index = match &scope_input {
            ScopeInput::PrDiff { index } => Some(index),
            ScopeInput::Baseline { .. } => None,
        };
        gather_seeds(args, &current, &seeds_in_scope, seed_index)
    } else {
        Vec::new()
    };
    // The seed current-table row cap (cute-dbt#350) — resolved at the I/O
    // boundary from `[seeds] row_cap` over DEFAULT_SEED_ROW_CAP, the
    // macro_body_cap precedent. Inert when no seed is in scope / the
    // experiment is off (an empty `seed_cards` renders nothing to cap).
    let seed_row_cap = resolve_seed_row_cap(args);
    // Cell-level unit-test data-table diffs (cute-dbt#98): the structured
    // sibling of `yaml_diffs`. For each in-scope changed test whose own YAML
    // block the diff touched, reconstruct an aligned given/expect cell diff
    // (NEW from the current manifest, OLD sliced from the reconstructed
    // pre-edit YAML). PrDiff arm only — baseline mode has no hunks, so the
    // given/expect grids show the plain "Current" data view. Reuses the same
    // refined `changed` + `authoring_yaml` block map as `reconstruct_block_diffs`.
    let data_diffs: HashMap<String, UnitTestDataDiff> = match &scope_input {
        ScopeInput::PrDiff { index } => {
            let mut diffs = reconstruct_table_diffs(&current, &changed, &authoring_yaml, index);
            // External fixture FILE cell diffs (cute-dbt#126 AC#3): merged in
            // beside the YAML-block inline diffs. Keyed off the fixture file's
            // OWN hunks (`index.hunks_for(fixture_path)`), INDEPENDENT of the
            // YAML-block `changed` gate — a PR editing only the csv never
            // touches the test's YAML block.
            merge_external_data_diffs(
                &mut diffs,
                &current,
                &render_test_ids,
                &external_fixtures,
                index,
            );
            diffs
        }
        ScopeInput::Baseline { .. } => HashMap::new(),
    };
    // cute-dbt#111: a non-`--unified=0` diff degrades every affected block to
    // the plain view (the `reconstruct_one` contract guard). Surface that ONCE
    // on stderr so a user who forgot `--unified=0` isn't left thinking the
    // inline diffs are broken. PrDiff arm only (baseline mode has no hunks);
    // the domain predicate is pure — the I/O (`eprintln!`) lives here in cli.
    if let ScopeInput::PrDiff { index } = &scope_input {
        warn_if_not_unified_zero(index);
    }
    let (report_title, report_subtitle) = resolve_report_strings(args);
    // cute-dbt#346 — the source-PR ref for the change-context banner link.
    // Merges `--pr-*` flags over `[pr]` config; `None` unless both a url and
    // a title resolve. The renderer further gates it to the PR-diff arm.
    let pr_ref = resolve_pr_ref(args);
    let (baseline_label, scope_source) = scope_banner(args, &scope_input);
    // Check selection + suppression (cute-dbt#171): the `[checks]` config
    // policy plus inline SQL pragmas scanned from each in-scope model's
    // manifest `raw_code`. Display-layer only — applied inside payload
    // assembly strictly after supersedes resolution.
    let check_policy = build_check_policy(args, &current, &models_in_scope, &experiments);
    render(
        &args.out,
        &current,
        &in_scope,
        &models_in_scope,
        &changed,
        &authoring_yaml,
        &yaml_diffs,
        &sql_diffs,
        &model_yaml,
        &data_diffs,
        &external_fixtures,
        &baseline_label,
        scope_source,
        &report_title,
        report_subtitle.as_deref(),
        &check_policy,
        &project_facts,
        &governance_facts,
        macro_lens.as_ref(),
        pr_ref.as_ref(),
        &seed_cards,
        seed_row_cap,
        pr_dag.as_ref(),
        &axes,
        &model_states,
        &removed_models,
        pr_comments.as_ref(),
    )
    .map_err(|err| RunError::output(&args.out, err))?;
    // cute-dbt#386 — the machine-readable findings envelope. Purely
    // additive to the HTML report above (the render path is untouched):
    // the SAME in-scope `model_findings → apply_check_policy` pipeline the
    // renderer ran is re-derived in the emit adapter so the envelope's
    // findings match the report's exactly. Two consumers, both gated on
    // their flags and both no-ops by default:
    //   - `--findings-out <path>` writes the `{ metadata, findings }`
    //     sidecar JSON beside the HTML report;
    //   - `--fail-on-uncovered` exits `EXIT_GATE` iff the in-scope set
    //     carries a Total-tier `Uncovered` finding (deterministic gap).
    // Both share one collection pass — skipped entirely when neither flag
    // is set (zero added work on the default path).
    finalize_findings(
        args,
        &current,
        &models_in_scope,
        &check_policy,
        &scope_input,
    )
}

/// The `--findings-out` / `--fail-on-uncovered` tail of the `report` run
/// loop (cute-dbt#386) — runs strictly AFTER the HTML report is written.
///
/// Collects the in-scope findings once (via the emit adapter's mirror of
/// the renderer's `model_findings → apply_check_policy` pipeline) and feeds
/// both consumers from that single pass. Returns
/// [`ReportOutcome::UncoveredGate`] iff `--fail-on-uncovered` is set and a
/// Total-tier `Uncovered` finding is in scope; otherwise
/// [`ReportOutcome::Success`]. Both flags off ⇒ no collection, immediate
/// `Success` (the default path adds zero work).
///
/// `generated_at` is the RFC3339 **date** ([`today_rfc3339_date`]) computed
/// here at the I/O boundary and threaded into the pure envelope builder, so
/// the committed envelope golden stays byte-stable (the golden-determinism
/// rule; the fixture pins a far-past/far-future date) and `cute-dbt` carries
/// no `chrono`/`time` dependency.
fn finalize_findings(
    args: &ReportArgs,
    current: &Manifest,
    models_in_scope: &ModelInScopeSet,
    check_policy: &CheckPolicy<HeuristicId>,
    scope_input: &ScopeInput,
) -> Result<ReportOutcome, RunError> {
    if args.findings_out.is_none() && !args.fail_on_uncovered && !args.annotations {
        return Ok(ReportOutcome::Success);
    }
    // Collect the in-scope findings once. The gate reads the raw `Finding`s
    // (`has_total_uncovered` operates on `&[Finding]`); the envelope wraps
    // the SAME vec into anchor-bearing entries (cute-dbt#386 / #393). One
    // pass feeds the gate, the `--annotations` emit, and the envelope.
    let findings = collect_in_scope_findings(current, models_in_scope, check_policy);
    // The shared finding→line anchor resolver (cute-dbt#393, the #261
    // one-resolver-two-projections arc): bound to the run's manifest + the
    // `--pr-diff` index. Only the PrDiff arm has hunks; the baseline arm has
    // no diff index, so the closure resolves nothing (no honest line to
    // pin) — annotations stay summary-only and envelope anchors stay `None`.
    let diff_index = match scope_input {
        ScopeInput::PrDiff { index } => Some(index),
        ScopeInput::Baseline { .. } => None,
    };
    let anchor_for = |finding: &Finding<HeuristicId>| -> Option<ResolvedAnchor> {
        resolve_finding_anchor(finding, current, diff_index?)
    };
    // The gate decision is read off the raw findings BEFORE the sidecar is
    // written, but a sidecar write failure still wins (returns below via `?`
    // before the gate outcome is returned) — the write-failure-over-gate
    // precedence the run_loop tests pin.
    let gate_tripped = args.fail_on_uncovered && has_total_uncovered(&findings);
    // cute-dbt#393 — print the GitHub workflow-command annotations to
    // stdout (gen-time, never in `report.html`). A Total-tier gap escalates
    // to `::error` only when the uncovered-gate is enforcing; otherwise it
    // rides as `::warning`. Capped at the per-step limit with an honest
    // overflow notice. Emitted before the `--findings-out` sidecar write, so
    // a sidecar-write failure below never swallows the annotations (the HTML
    // render already ran in the caller — a render failure suppresses both).
    if args.annotations {
        let levels = if gate_tripped {
            AnnotationLevels::enforcing()
        } else {
            AnnotationLevels::advisory()
        };
        let emit = emit_annotations(&findings, levels, DEFAULT_ANNOTATION_CAP, &anchor_for);
        for line in &emit.lines {
            println!("{line}");
        }
    }
    // `generated_at` is the I/O-boundary date — the `--generated-at`
    // override (golden regeneration / reproducible builds) over today's
    // computed civil date. Threaded into the pure envelope builder so the
    // committed golden is byte-stable.
    let generated_at = args.generated_at.clone().unwrap_or_else(today_rfc3339_date);
    if let Some(path) = &args.findings_out {
        // cute-dbt#393 — populate each envelope finding's reserved anchor
        // slot from the SAME resolver the annotations consume (the #261
        // arc): a finding whose model file is in the diff carries a concrete
        // `(path, line, diff_context)`; the rest stay anchor-less (an
        // unpopulated anchor is omitted, keeping the golden byte-minimal).
        let envelope = envelope_from_findings_anchored(
            findings,
            env!("CARGO_PKG_VERSION"),
            generated_at,
            envelope_scope(args, scope_input),
            &anchor_for,
        );
        write_sidecar(&envelope, path).map_err(|err| RunError::output(path, err))?;
    }
    Ok(if gate_tripped {
        ReportOutcome::UncoveredGate
    } else {
        ReportOutcome::Success
    })
}

/// Build the envelope's [`EnvelopeScope`] from the run's scope source —
/// the machine-readable twin of [`scope_banner`] (cute-dbt#386).
///
/// `Baseline` carries the `--baseline-manifest` path verbatim (empty when
/// somehow absent — omitted from JSON). `PrDiff` deliberately carries NO
/// source label (`source: None`, omitted): the parsed [`PrDiff`](crate::domain::pr_diff::PrDiff) retains
/// only the changed-file facts, not the `@file` argument, and embedding the
/// raw `@file` path could bake a CI-runner-absolute path into the committed
/// artifact (the same `root_path`-leak class the manifest gitignore guards).
/// The slot is reserved (forward-compatible) but unpopulated — the `mode:
/// "pr-diff"` tag is the arm discriminator a consumer keys on.
fn envelope_scope(args: &ReportArgs, scope_input: &ScopeInput) -> EnvelopeScope {
    match scope_input {
        ScopeInput::Baseline { .. } => EnvelopeScope::Baseline {
            baseline: args
                .baseline_manifest
                .as_ref()
                .map_or_else(String::new, |p| p.display().to_string()),
        },
        ScopeInput::PrDiff { .. } => EnvelopeScope::PrDiff { source: None },
    }
}

/// The named `explore` run loop (cute-dbt#100) — `load_current` →
/// `all_models` → `resolve_change_context` → `build_payload` →
/// `render_explore`.
///
/// Stage-1 pre-flight is fail-CLOSED exactly like `report` (an
/// unreadable or pre-v12 manifest aborts with remediation). Stage-2 is
/// deliberately **fail-OPEN**: there is no `preflight_compiled` call on
/// this path — an uncompiled model renders as a "not compiled" node in
/// the emitted pages instead of raising. No baseline is read and no
/// scope source exists: the scope is the **full manifest** via the
/// [`all_models`] domain seam, and the payload reuses the existing
/// engine-agnostic [`build_payload`] with an empty `changed` set and no
/// diff artifacts.
///
/// The optional `--pr-diff` (cute-dbt#106) adds **change context**:
/// [`changed_models`] marks the models whose files the diff touched and
/// the renderer decorates exactly those nodes. Context **never narrows
/// scope** — the payload below always spans the full `models` set, with
/// or without a diff. The explorer takes no baseline manifest, ever
/// (founder respec 2026-06-10): the developer-native diff signal is
/// git, not environment manifests.
fn execute_explore(args: &ExploreArgs) -> Result<(), RunError> {
    // Experimental-status notice (cute-dbt#290): one line, stderr ONLY —
    // stdout must stay clean so scripted flows are never corrupted. No
    // access gate: the verb stays listed and runnable; this notice (plus
    // the EXPERIMENTAL-prefixed help text) is the whole marking.
    eprintln!(
        "cute-dbt: note: `explore` is experimental — its surface and \
         output may change or be removed in any v0.x release"
    );
    let current = load_explore_manifest(args)?;
    let models = all_models(&current);
    let context = resolve_change_context(args, &current);
    // Stage-2 fail-OPEN: no preflight_compiled here, by design.
    let payload = build_payload(
        &current,
        &InScopeSet::new(),
        &models,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "",
    );
    // cute-dbt#345 — the macro view emits its third sub-page only when the
    // `--pr-diff` changed a root-project macro. The FOCUS SET and the TEST
    // CONSUMERS are RESOLVED here (the scope-resolution lane, including the
    // domain walks) and handed to the renderer, which stays a pure renderer
    // (it never calls a domain walker). When several macros changed, the
    // focused view is built for the lowest-id one (the deterministic
    // `BTreeSet` first) — the multi-macro picker is a later slice (founder
    // Open Q 6).
    let changed_macro = context.changed_macros.iter().next();
    let macro_focus = changed_macro.map(|macro_id| macro_focus_set(&current, macro_id));
    // The filtered artifact directory's tests partition (cute-dbt#345 AC3):
    // the `test`/`unit_test` consumers of the macro, resolved by the SAME
    // pure-domain authority the model partition uses (`macro_test_consumers`
    // twins `macro_blast_radius`). Empty default when no macro changed; the
    // renderer only consumes it on the `Some(macro_focus)` arm.
    let macro_tests = changed_macro
        .map(|macro_id| macro_test_consumers(&current, macro_id))
        .unwrap_or_default();
    // cute-dbt#398 — the seed-node detail card's data: every seed's
    // working-tree CSV (read via `--project-root`, present only on the
    // `--pr-diff` arm), capped to DEFAULT_SEED_ROW_CAP (explore has no
    // `--config`, so it uses the default cap directly). Empty when there
    // are no seeds or no `--project-root` ⇒ the side-map serde-skips ⇒ the
    // seed-free `dag.html` golden stays byte-identical.
    let seed_cards = gather_explore_seeds(args, &current);
    // cute-dbt#270 — standing project facts (the parsed dbt_project.yml)
    // drive the explore project pane / vars inventory / config provenance.
    // Resolved from --project-root (the --pr-diff arm) or derived from the
    // manifest's <root>/target/manifest.json layout; absent ⇒ no pane.
    let project_facts = gather_explore_project_facts(args);
    render_explore(
        &args.out_dir,
        &current,
        &models,
        context.changed_models.as_ref(),
        &payload,
        macro_focus.as_ref().map(|focus| MacroFocus {
            focus,
            tests: &macro_tests,
        }),
        &seed_cards,
        DEFAULT_SEED_ROW_CAP,
        &project_facts,
    )
    .map_err(|err| RunError::output(&args.out_dir, err))?;
    Ok(())
}

/// The resolved `--pr-diff` change context for the `explore` verb
/// (cute-dbt#106 + cute-dbt#345): both the changed-model set (the
/// lineage decoration) and the changed-root-project-macro set (the
/// cute-dbt#345 macro-view trigger), derived from ONE
/// [`NormalizedDiffIndex`] build so the diff is parsed once.
///
/// `changed_models` is `None` when no `--pr-diff` was supplied (the
/// renderer emits the pre-#106 no-context page shape) and `Some` —
/// possibly empty — when a diff was present. `changed_macros` is empty
/// without a diff and otherwise holds the root-project macro ids the
/// diff's hunks/paths resolved to (the macro-view emission gate).
struct ExploreChangeContext {
    changed_models: Option<ModelInScopeSet>,
    changed_macros: BTreeSet<String>,
}

/// The `resolve_change_context` stage (cute-dbt#106 + cute-dbt#345): map
/// the optional `--pr-diff` onto the changed-model set AND the
/// changed-root-project-macro set, from one [`NormalizedDiffIndex`]
/// build (the diff is parsed once and both derivations read it).
///
/// `changed_models` is `None` — no `--pr-diff` — meaning **no change
/// context** (the renderer emits the unchanged no-context page shape),
/// which is distinct from `Some(empty)` — a diff that touched no model
/// files still renders the honest "0 changed in this diff" banner. The
/// [`NormalizedDiffIndex`] is built exactly like the report arm's
/// (`resolve_scope_input`): the `--project-root` strip rebases the
/// diff's repo-relative paths onto the manifest's project-relative
/// `original_file_path` entries.
///
/// `changed_macros` (cute-dbt#345) is the root-project macro ids the
/// same diff resolved to via [`changed_macros_pr_diff`] — empty without
/// a diff, the macro-view emission gate when present.
fn resolve_change_context(args: &ExploreArgs, current: &Manifest) -> ExploreChangeContext {
    match args.pr_diff.as_ref() {
        None => ExploreChangeContext {
            changed_models: None,
            changed_macros: BTreeSet::new(),
        },
        Some(diff) => {
            let index = NormalizedDiffIndex::new(diff, args.project_root.as_deref());
            ExploreChangeContext {
                changed_models: Some(changed_models(current, &index)),
                changed_macros: changed_macros_pr_diff(current, &index),
            }
        }
    }
}

/// Stage-1 pre-flight for the `explore` verb: load `--manifest` through
/// the file-backed [`ManifestSource`]. A load failure is `Unreadable` /
/// `SchemaUnsupported` — the same fail-closed gate as `report`.
fn load_explore_manifest(args: &ExploreArgs) -> Result<Manifest, RunError> {
    let source = FileManifestSource;
    let current = source.load(&args.manifest)?;
    Ok(current)
}

/// The widened render set (cute-dbt#91): the ids of every current unit
/// test whose resolved target model is in scope — a superset of the
/// in-scope set that adds the context siblings on a model that entered
/// scope solely via a changed test. Both [`gather_authoring_yaml`] and the
/// render payload consume this so the rendered tests and their
/// Authoring-YAML drawers stay in lockstep.
fn render_test_ids(current: &Manifest, models_in_scope: &ModelInScopeSet) -> InScopeSet {
    index_tests_for_models(current, models_in_scope)
        .values()
        .flat_map(|tests| tests.iter().map(|(id, _)| (*id).to_owned()))
        .collect()
}

/// The `gather_authoring_yaml` stage — read each in-scope `unit_test`'s
/// source YAML and slice the authored block.
///
/// Resolution semantics:
///
/// - If `cli.project_root` is `Some`, use it (clap already validated).
/// - Else try to derive from `cli.manifest` via the `target/manifest.json`
///   convention. A successful derive emits a one-line stderr breadcrumb
///   so the operator can see what cute-dbt assumed.
/// - Else return an empty map and silently skip — the authoring-YAML
///   drawer simply won't appear in the rendered report.
///
/// Per-test soft failure: a [`io::ErrorKind::NotFound`] on read is
/// silent; any other read error emits a stderr warning but does not
/// fail the run. A slice that returns [`None`] (test name not found
/// inside its declared source YAML) is silent.
fn gather_authoring_yaml(
    args: &ReportArgs,
    current: &Manifest,
    in_scope: &InScopeSet,
) -> HashMap<String, UnitTestYamlBlock> {
    let (resolved, derived) =
        self::args::resolve_project_root(args.project_root.as_deref(), &args.manifest);
    let Some(project_root) = resolved else {
        return HashMap::new();
    };
    if derived {
        eprintln!(
            "cute-dbt: deriving --project-root from --manifest: {}",
            project_root.display(),
        );
    }
    let reader = FsProjectFileReader::new(project_root);
    gather_authoring_yaml_with_reader(&reader, current, in_scope)
}

/// Pure composition step over the [`ProjectFileReader`] port — testable
/// without touching the filesystem by passing an in-memory impl.
fn gather_authoring_yaml_with_reader(
    reader: &dyn ProjectFileReader,
    current: &Manifest,
    in_scope: &InScopeSet,
) -> HashMap<String, UnitTestYamlBlock> {
    let mut out: HashMap<String, UnitTestYamlBlock> = HashMap::new();
    for id in in_scope.iter() {
        let Some(unit_test) = current.unit_test(id) else {
            continue;
        };
        let Some(path) = unit_test.original_file_path() else {
            continue;
        };
        let contents = match reader.read(path) {
            Ok(c) => c,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                eprintln!("cute-dbt: warning: could not read source YAML for {id}: {err}");
                continue;
            }
        };
        let Some(block) = extract_unit_test_block(&contents, unit_test.name()) else {
            continue;
        };
        out.insert(id.to_owned(), block);
    }
    out
}

/// The `gather_model_yaml` stage (cute-dbt#247) — for each in-scope
/// model, read the schema/properties file the manifest's `patch_path`
/// points at and slice the model's authored `models:` entry
/// ([`extract_model_block`]).
///
/// Same project-root resolution as [`gather_authoring_yaml`], but the
/// failure surface is NOT a silent skip: every in-scope model gets a
/// [`ModelYamlOutcome`], including the honest-degrade arms (no
/// `patch_path` in the manifest, no resolvable project root, file
/// missing / outside the root, unreadable, entry not found) — the
/// rendered Model-YAML section must always say something truthful, never
/// vanish or render empty. The run never fails on any arm (a render-time
/// working-tree read, exactly like the authoring-YAML gather; the
/// zero-egress property of the generated HTML is untouched).
fn gather_model_yaml(
    args: &ReportArgs,
    current: &Manifest,
    models_in_scope: &ModelInScopeSet,
) -> HashMap<String, ModelYamlOutcome> {
    let (resolved, _derived) =
        self::args::resolve_project_root(args.project_root.as_deref(), &args.manifest);
    let Some(project_root) = resolved else {
        return model_yaml_without_root(current, models_in_scope);
    };
    let reader = FsProjectFileReader::new(project_root);
    gather_model_yaml_with_reader(&reader, current, models_in_scope)
}

/// The no-project-root arm of [`gather_model_yaml`]: nothing can be
/// read, so each model degrades to [`ModelYamlOutcome::NoProjectRoot`]
/// (naming the unread schema file) or [`ModelYamlOutcome::NoPatchPath`].
fn model_yaml_without_root(
    current: &Manifest,
    models_in_scope: &ModelInScopeSet,
) -> HashMap<String, ModelYamlOutcome> {
    let mut out = HashMap::new();
    for model_id in models_in_scope.iter() {
        let Some(node) = current.node(model_id) else {
            continue;
        };
        let outcome = match node.patch_path() {
            Some(path) => ModelYamlOutcome::NoProjectRoot {
                path: path.to_owned(),
            },
            None => ModelYamlOutcome::NoPatchPath,
        };
        out.insert(model_id.as_str().to_owned(), outcome);
    }
    out
}

/// Pure composition step over the [`ProjectFileReader`] port — testable
/// without touching the filesystem by passing an in-memory impl.
///
/// Per-model outcome mapping: a [`io::ErrorKind::NotFound`] read — and an
/// [`io::ErrorKind::InvalidInput`] (the adapter's path-safety rejection
/// of a package model's escaping `patch_path`) — is
/// [`ModelYamlOutcome::FileMissing`]; any other read error warns on
/// stderr and degrades to [`ModelYamlOutcome::Unreadable`]; a readable
/// file without the model's `models:` entry is
/// [`ModelYamlOutcome::EntryNotFound`].
fn gather_model_yaml_with_reader(
    reader: &dyn ProjectFileReader,
    current: &Manifest,
    models_in_scope: &ModelInScopeSet,
) -> HashMap<String, ModelYamlOutcome> {
    let mut out = HashMap::new();
    for model_id in models_in_scope.iter() {
        let Some(node) = current.node(model_id) else {
            continue;
        };
        let Some(path) = node.patch_path() else {
            out.insert(model_id.as_str().to_owned(), ModelYamlOutcome::NoPatchPath);
            continue;
        };
        let outcome = match reader.read(path) {
            Ok(contents) => {
                // The `models:` entry is keyed by the model's AUTHORED
                // name — the ingested wire `name` with the final-id-
                // segment fallback (cute-dbt#256: a versioned model's
                // leaf segment is the `.vN` suffix, never the name).
                let name = node.bare_name();
                match extract_model_block(&contents, name) {
                    Some(block) => ModelYamlOutcome::Found {
                        path: path.to_owned(),
                        block,
                        diff: None,
                    },
                    None => ModelYamlOutcome::EntryNotFound {
                        path: path.to_owned(),
                    },
                }
            }
            Err(err)
                if err.kind() == io::ErrorKind::NotFound
                    || err.kind() == io::ErrorKind::InvalidInput =>
            {
                ModelYamlOutcome::FileMissing {
                    path: path.to_owned(),
                }
            }
            Err(err) => {
                eprintln!("cute-dbt: warning: could not read schema YAML for {model_id}: {err}");
                ModelYamlOutcome::Unreadable {
                    path: path.to_owned(),
                }
            }
        };
        out.insert(model_id.as_str().to_owned(), outcome);
    }
    out
}

/// The `gather_external_fixtures` stage (cute-dbt#126) — for each rendered
/// unit test, read any external `given[i].fixture` / `expect.fixture` file
/// through the [`ProjectFileReader`] port and parse it, so the render layer
/// inlines a real grid instead of the cute-dbt#98 silently-empty-grid
/// affordance.
///
/// Same project-root resolution + per-test soft-failure as
/// [`gather_authoring_yaml`]: an unresolvable project root yields an empty
/// map, and an unreadable / non-tabulatable fixture is silently skipped
/// (the report falls back to the affordance). The fixture data never leaves
/// the working tree — this is a render-time read, so the zero-egress
/// property of the generated HTML is untouched.
fn gather_external_fixtures(
    args: &ReportArgs,
    current: &Manifest,
    in_scope: &InScopeSet,
) -> HashMap<String, ExternalFixtures> {
    let (resolved, _derived) =
        self::args::resolve_project_root(args.project_root.as_deref(), &args.manifest);
    let Some(project_root) = resolved else {
        return HashMap::new();
    };
    let reader = FsProjectFileReader::new(project_root);
    gather_external_fixtures_with_reader(&reader, current, in_scope)
}

/// Pure composition step over the [`ProjectFileReader`] port — testable
/// without touching the filesystem by passing an in-memory impl.
fn gather_external_fixtures_with_reader(
    reader: &dyn ProjectFileReader,
    current: &Manifest,
    in_scope: &InScopeSet,
) -> HashMap<String, ExternalFixtures> {
    let mut out: HashMap<String, ExternalFixtures> = HashMap::new();
    for id in in_scope.iter() {
        let Some(unit_test) = current.unit_test(id) else {
            continue;
        };
        let ext = external_fixtures_for_test(reader, id, unit_test);
        if !ext.given.is_empty() || ext.expect.is_some() {
            out.insert(id.to_owned(), ext);
        }
    }
    out
}

/// Build one unit test's [`ExternalFixtures`] — load each `given` block's
/// external fixture (keyed by its positional ordinal) and the single
/// `expect` block's, soft-failing per fixture (the [`load_external_fixture`]
/// contract). Factored out of [`gather_external_fixtures_with_reader`] so the
/// gather loop stays a thin per-test driver and this per-test assembly is
/// unit-testable in isolation. An inline-rows or missing/unreadable fixture
/// leaves its slot empty; the returned value may be wholly empty (the caller
/// drops an all-empty test from the map).
fn external_fixtures_for_test(
    reader: &dyn ProjectFileReader,
    id: &str,
    unit_test: &UnitTest,
) -> ExternalFixtures {
    let mut ext = ExternalFixtures::default();
    for (ordinal, given) in unit_test.given().iter().enumerate() {
        if let Some(loaded) =
            load_external_fixture(reader, id, given.fixture(), given.rows(), given.format())
        {
            ext.given.insert(ordinal, loaded);
        }
    }
    let expect = unit_test.expect();
    if let Some(loaded) =
        load_external_fixture(reader, id, expect.fixture(), expect.rows(), expect.format())
    {
        ext.expect = Some(loaded);
    }
    ext
}

/// The `gather_seeds` stage (cute-dbt#350) — build the identity-and-lineage
/// [`SeedCard`] skeleton for each in-scope seed
/// ([`build_seed_cards`]), then read each seed's working-tree CSV
/// (`seeds/<name>.csv`, the `original_file_path`) through the
/// [`ProjectFileReader`] port and parse it ([`external_fixture_table`] on
/// the `.csv`-derived format) into the card's
/// [`table`](crate::domain::SeedCard::table). The header row of the CSV
/// supplies the column names — **not** the manifest `columns` map, which is
/// empty for the common un-YAML'd seed.
///
/// **Truthful degrade, never a silent skip** (the cute-dbt#126 lesson, and
/// the [`gather_model_yaml`] posture — deliberately *unlike*
/// [`gather_external_fixtures`]'s silent skip): every in-scope seed yields a
/// `SeedCard`. When `--project-root` is unresolvable, every card keeps
/// `table: None` (the no-root degrade — the cards still carry identity +
/// lineage + config facts, which the later "Data tables" section renders as
/// a *labeled* empty-data state). Per-seed, an unreadable / missing /
/// non-tabulatable CSV also leaves that one card's `table: None` while its
/// siblings still fill. No arm drops a card or fails the run (a render-time
/// working-tree read, exactly like the YAML-drawer gather — the zero-egress
/// property of the generated HTML is untouched).
fn gather_seeds(
    args: &ReportArgs,
    current: &Manifest,
    seeds_in_scope: &SeedInScopeSet,
    index: Option<&NormalizedDiffIndex>,
) -> Vec<SeedCard> {
    let cards = build_seed_cards(current, seeds_in_scope);
    let (resolved, _derived) =
        self::args::resolve_project_root(args.project_root.as_deref(), &args.manifest);
    let Some(project_root) = resolved else {
        // No-root degrade: return the identity-and-lineage skeletons
        // unchanged — every card keeps `table: None` (a labeled empty-data
        // state, NOT a dropped card), so the section can say truthfully
        // "data unavailable — no project root".
        return cards;
    };
    let reader = FsProjectFileReader::new(project_root);
    gather_seeds_with_reader(&reader, cards, index)
}

/// The explorer's `gather_seeds` (cute-dbt#398) — build a [`SeedCard`] for
/// **every** seed in the manifest ([`all_seeds`], the full-manifest seam, not
/// the report's modified-only [`select_seeds_in_scope`]) and fill each card's
/// [`table`](crate::domain::SeedCard::table) from its working-tree CSV through
/// the same [`ProjectFileReader`] port the report's [`gather_seeds`] uses.
///
/// The explorer's `--project-root` is only present alongside `--pr-diff`
/// (clap `requires`), and is the diff-side strip — but it equally names the
/// working tree the seed CSVs live in, so it is the project root passed to
/// [`FsProjectFileReader`]. Without it (no `--pr-diff` run), every card keeps
/// `table: None` — the labeled "data unavailable" degrade (the cute-dbt#126
/// lesson), exactly the report's no-root behavior. No diff index is threaded:
/// the explorer takes no baseline, so its seed detail cards show the current
/// table only (no seed cell-diff) — `gather_seeds_with_reader` is called with
/// `None`, leaving every card's `diff` empty.
fn gather_explore_seeds(args: &ExploreArgs, current: &Manifest) -> Vec<SeedCard> {
    let cards = build_seed_cards(current, &all_seeds(current));
    let Some(project_root) = args.project_root.as_deref() else {
        // No-root degrade: identity-and-lineage skeletons unchanged (every
        // card keeps `table: None`), so the seed detail card can say
        // truthfully "data unavailable".
        return cards;
    };
    let reader = FsProjectFileReader::new(project_root.to_path_buf());
    gather_seeds_with_reader(&reader, cards, None)
}

/// Gather the explore-side standing project facts (cute-dbt#270): the
/// parsed working-tree `dbt_project.yml`, the SAME
/// [`ProjectDefinition`](crate::domain::ProjectDefinition) the report
/// reads (R4), driving the explore project pane / vars inventory / config
/// provenance.
///
/// Project-root resolution mirrors the report's standing-metadata path
/// ([`gather_project_facts`]): the explicit `--project-root` (present
/// only on the explore `--pr-diff` arm, clap-gated) wins, else the root is
/// derived from the conventional `<root>/target/manifest.json` layout via
/// [`crate::cli::args::resolve_project_root`]. With no resolvable root
/// nothing is read and the pane stays absent (the no-pane golden shape).
///
/// Explore takes no baseline and renders no project-change panel — only
/// the `definition` arm of [`ProjectFacts`] is populated; `panel` /
/// attributions stay empty (no diff is threaded here). The manifest is
/// not needed: the parse reads the file alone (the per-model attribution
/// happens later, in the renderer, against the threaded manifest).
fn gather_explore_project_facts(args: &ExploreArgs) -> ProjectFacts {
    let (resolved, _derived) =
        self::args::resolve_project_root(args.project_root.as_deref(), &args.manifest);
    let Some(project_root) = resolved else {
        return ProjectFacts::default();
    };
    let reader = FsProjectFileReader::new(project_root);
    explore_project_facts_with_reader(&reader)
}

/// Pure composition step over the [`ProjectFileReader`] port — testable
/// without touching the filesystem. Reads + parses `dbt_project.yml` into
/// the standing [`ProjectFacts::definition`]; a missing/unreadable or
/// unparseable file degrades to [`ProjectFacts::default`] (no pane), the
/// report's fail-open posture (cute-dbt#266) without the diff panel.
fn explore_project_facts_with_reader(reader: &dyn ProjectFileReader) -> ProjectFacts {
    let new_text = match reader.read(DBT_PROJECT_YML) {
        Ok(text) => text,
        Err(err)
            if err.kind() == io::ErrorKind::NotFound
                || err.kind() == io::ErrorKind::InvalidInput =>
        {
            return ProjectFacts::default();
        }
        Err(err) => {
            eprintln!("cute-dbt: warning: could not read dbt_project.yml: {err}");
            return ProjectFacts::default();
        }
    };
    match parse_project_definition(&new_text) {
        Ok(def) => ProjectFacts {
            definition: Some(def),
            ..ProjectFacts::default()
        },
        Err(err) => {
            eprintln!(
                "cute-dbt: warning: could not parse dbt_project.yml ({err:?}); \
                 the explore project pane is omitted"
            );
            ProjectFacts::default()
        }
    }
}

/// Pure composition step over the [`ProjectFileReader`] port — testable
/// without touching the filesystem by passing an in-memory impl. Takes the
/// already-built skeleton cards (so the projection / lineage derivation is
/// not re-run here) and fills each card's
/// [`table`](crate::domain::SeedCard::table) from its working-tree CSV.
///
/// Per-card degrade: a card with no `original_file_path`, or whose file is
/// unreadable / missing / non-tabulatable, keeps `table: None` (the
/// truthful empty-data state). A [`io::ErrorKind::NotFound`] (including the
/// adapter's `InvalidInput` rejection of an absolute / `..`-escaping path)
/// is silent; any other read error warns on stderr but never fails the run.
fn gather_seeds_with_reader(
    reader: &dyn ProjectFileReader,
    mut cards: Vec<SeedCard>,
    index: Option<&NormalizedDiffIndex>,
) -> Vec<SeedCard> {
    for card in &mut cards {
        let Some(path) = card.original_file_path.as_deref() else {
            continue; // no path on the node ⇒ truthful empty (table stays None)
        };
        let text = match reader.read(path) {
            Ok(text) => text,
            Err(err)
                if err.kind() == io::ErrorKind::NotFound
                    || err.kind() == io::ErrorKind::InvalidInput =>
            {
                continue; // missing / rejected path ⇒ truthful empty
            }
            Err(err) => {
                eprintln!(
                    "cute-dbt: warning: could not read seed CSV for {}: {err}",
                    card.id,
                );
                continue; // unreadable ⇒ truthful empty
            }
        };
        // Seeds are CSV by definition (`seeds/<name>.csv`); derive the
        // effective format from the path so the parser hits the CSV branch
        // (header-keyed, value-inferred rows). A non-`.csv` extension or a
        // non-tabulatable body leaves `table: None` (truthful empty).
        let format = effective_fixture_format(None, path);
        card.table = external_fixture_table(&text, format.as_deref());
        // cute-dbt#350 — on the pr-diff arm, reconstruct the seed CSV's
        // old→new cell-diff from its OWN hunks (the working-tree text is NEW;
        // reverse-applying the hunks rebuilds OLD). The seed file is an
        // external tabular file, so this reuses the #126 reconstruction
        // wholesale. A seed the diff did not touch (no hunks) yields `None`
        // (`reconstruct_external_fixture_diff` returns `None`), so its card
        // renders the plain current table. Baseline arm: `index` is `None`,
        // so `diff` stays `None` (no hunks exist in either manifest — seeds
        // carry zero row data in the manifest).
        if let Some(index) = index {
            card.diff =
                reconstruct_external_fixture_diff(&text, format.as_deref(), index.hunks_for(path));
        }
    }
    cards
}

/// Load one external fixture, or `None` when this given/expect is not an
/// external fixture or the file cannot be read.
///
/// The trigger is **exactly** `fixture: Some` AND `rows: null` — the
/// confirmed fusion shape (the data lives in the file). A `fixture` present
/// *alongside* inline `rows` (a shape a dbt-core engine MAY emit by
/// inlining the fixture) renders from the inline rows and is left untouched,
/// so the reader is never invoked for it.
fn load_external_fixture(
    reader: &dyn ProjectFileReader,
    id: &str,
    fixture: Option<&str>,
    rows: &serde_json::Value,
    format: Option<&str>,
) -> Option<LoadedFixture> {
    let path = fixture?;
    if !rows.is_null() {
        return None; // fixture + inline rows ⇒ render inline, do not read
    }
    let (text, effective_format) = read_external_fixture(reader, id, path, format)?;
    let table = external_fixture_table(&text, effective_format.as_deref());
    Some(LoadedFixture {
        text,
        format: effective_format,
        table,
    })
}

/// Read an external fixture file through the reader, returning its body +
/// the effective format (manifest `format:`, else the path extension).
///
/// Honors the cute-dbt#126 AC#4 **bare-name fallback**: a dbt-core engine
/// MAY emit a bare fixture name (no path separator) rather than fusion's
/// resolved `tests/fixtures/<name>.csv`; when the bare name is not found,
/// retry the dbt convention `tests/fixtures/<name>.csv` (re-verified
/// against dbt-core at cute-dbt#64).
///
/// Soft-fails: a [`io::ErrorKind::NotFound`] (including a rejected
/// `..`/absolute path, which the adapter maps to `InvalidInput` — also
/// non-fatal here) is silent; any other read error warns on stderr but
/// never fails the run.
fn read_external_fixture(
    reader: &dyn ProjectFileReader,
    id: &str,
    path: &str,
    format: Option<&str>,
) -> Option<(String, Option<String>)> {
    match reader.read(path) {
        Ok(text) => Some((text, effective_fixture_format(format, path))),
        Err(err) if err.kind() == io::ErrorKind::NotFound && is_bare_fixture_name(path) => {
            let fallback = format!("tests/fixtures/{path}.csv");
            reader
                .read(&fallback)
                .ok()
                .map(|text| (text, effective_fixture_format(format, &fallback)))
        }
        Err(err)
            if err.kind() == io::ErrorKind::NotFound
                || err.kind() == io::ErrorKind::InvalidInput =>
        {
            None
        }
        Err(err) => {
            eprintln!("cute-dbt: warning: could not read external fixture {path} for {id}: {err}");
            None
        }
    }
}

/// A bare fixture name has no path separator (a single segment) — the shape
/// a dbt-core engine MAY emit instead of fusion's resolved
/// `tests/fixtures/<name>.csv` path (cute-dbt#126 AC#4 cross-engine guard).
fn is_bare_fixture_name(path: &str) -> bool {
    !path.contains('/')
}

/// The project-relative name of the file the `gather_project_facts`
/// stage reads and the panel keys on (cute-dbt#266). A fixed name by dbt
/// contract — the one file every dbt project defines at its root.
const DBT_PROJECT_YML: &str = "dbt_project.yml";

/// The `gather_project_facts` stage (cute-dbt#266) — parse the
/// working-tree `dbt_project.yml` (standing metadata, both scope arms) and,
/// on the `PrDiff` arm, build the categorized project-change panel plus
/// the per-model config-tree attributions (cute-dbt#267 — the scope
/// widening input, computed only when the panel categorized).
///
/// Called by the run loop only when the `project-state` experiment is
/// enabled (cute-dbt#291): the default run substitutes
/// [`ProjectFacts::default()`] without reading the file at all.
///
/// Same project-root resolution as [`gather_authoring_yaml`]. With no
/// resolvable root nothing can be read: standing metadata stays `None`,
/// and when `dbt_project.yml` IS in the diff the panel degrades to the
/// absence-note fallback (the hunks are still in hand) — the change is
/// never silently invisible.
fn gather_project_facts(
    args: &ReportArgs,
    current: &Manifest,
    scope_input: &ScopeInput,
) -> ProjectFacts {
    let index = match scope_input {
        ScopeInput::PrDiff { index } => Some(index),
        ScopeInput::Baseline { .. } => None,
    };
    let (resolved, _derived) =
        self::args::resolve_project_root(args.project_root.as_deref(), &args.manifest);
    let Some(project_root) = resolved else {
        return ProjectFacts {
            definition: None,
            panel: project_panel_without_file(index),
            config_attributions: BTreeMap::new(),
            var_references: BTreeMap::new(),
        };
    };
    let reader = FsProjectFileReader::new(project_root);
    gather_project_facts_with_reader(&reader, current, index)
}

/// The absence-note panel arm: `Some(Fallback{FileUnreadable})` exactly
/// when `dbt_project.yml` is in the diff (its raw hunk lines still show),
/// else no panel.
fn project_panel_without_file(index: Option<&NormalizedDiffIndex>) -> Option<ProjectChangePanel> {
    let index = index?;
    index
        .contains_changed(DBT_PROJECT_YML)
        .then(|| ProjectChangePanel::Fallback {
            reason: ProjectFallbackReason::FileUnreadable,
            raw: raw_hunk_lines(index.hunks_for(DBT_PROJECT_YML)),
        })
}

/// Pure composition step over the [`ProjectFileReader`] port — testable
/// without touching the filesystem by passing an in-memory impl.
///
/// Standing metadata: the parsed working-tree file whenever it reads +
/// parses (`None` otherwise — soft failure, stderr warning on non-NotFound
/// I/O errors and on a parse failure). Panel (diff-gated, fail-open):
///
/// - file unreadable → [`ProjectFallbackReason::FileUnreadable`] absence
///   note;
/// - new side unparseable → [`ProjectFallbackReason::NewParseFailed`];
/// - [`reverse_apply`] refuses (drift / malformed hunks) →
///   [`ProjectFallbackReason::OldNotReconstructable`];
/// - reconstructed old side unparseable →
///   [`ProjectFallbackReason::OldParseFailed`];
/// - otherwise → `Categorized` rows from [`diff_project_definitions`]
///   (a file created in the PR reverses to empty text, which parses to
///   the default definition, so every entry reports as added).
fn gather_project_facts_with_reader(
    reader: &dyn ProjectFileReader,
    current: &Manifest,
    index: Option<&NormalizedDiffIndex>,
) -> ProjectFacts {
    let new_text = match reader.read(DBT_PROJECT_YML) {
        Ok(text) => Some(text),
        Err(err)
            if err.kind() == io::ErrorKind::NotFound
                || err.kind() == io::ErrorKind::InvalidInput =>
        {
            None
        }
        Err(err) => {
            eprintln!("cute-dbt: warning: could not read dbt_project.yml: {err}");
            None
        }
    };
    let Some(new_text) = new_text else {
        return ProjectFacts {
            definition: None,
            panel: project_panel_without_file(index),
            config_attributions: BTreeMap::new(),
            var_references: BTreeMap::new(),
        };
    };
    let definition = match parse_project_definition(&new_text) {
        Ok(def) => Some(def),
        Err(err) => {
            eprintln!(
                "cute-dbt: warning: could not parse dbt_project.yml ({err:?}); \
                 the project panel shows the raw diff"
            );
            None
        }
    };
    let (panel, config_attributions, var_references) = index
        .filter(|index| index.contains_changed(DBT_PROJECT_YML))
        .map_or((None, BTreeMap::new(), BTreeMap::new()), |index| {
            let (panel, attributions, var_references) =
                project_change_panel(&new_text, definition.as_ref(), current, index);
            (Some(panel), attributions, var_references)
        });
    ProjectFacts {
        definition,
        panel,
        config_attributions,
        var_references,
    }
}

/// Build the diff-gated panel content for an in-diff `dbt_project.yml`
/// whose working-tree text is in hand, plus the per-model config-tree
/// attributions (cute-dbt#267) when categorization succeeds. See
/// [`gather_project_facts_with_reader`] for the arm map; every fallback
/// arm attributes nothing (no parsed pair ⇒ never a guessed widening).
///
/// The diff-gated panel build's output (cute-dbt#268): the panel
/// itself, the cute-dbt#267 per-model config-tree attributions, and the
/// cute-dbt#268 per-model var-reference chips.
type PanelFacts = (
    ProjectChangePanel,
    BTreeMap<String, Vec<ConfigAttribution>>,
    BTreeMap<String, Vec<VarReference>>,
);

/// Hooks rows are enriched from the manifest's `operation.*` nodes
/// (cute-dbt#269): the project name comes from the parsed file's own
/// `name:` (the file IS the root project definition — no wire field
/// needed; operation node names are built from exactly this name), and
/// [`attach_hook_facts`] adds the inline SQL diff + the manifest-side
/// presence verdict to each `Hooks` change.
///
/// Vars rows are enriched by [`attribute_var_changes`] +
/// [`attach_var_facts`] (cute-dbt#268): precedence-resolved per-var
/// entries with tiered affected-model lists ride the rows, and the
/// per-model [`VarReference`] chips ride [`ProjectFacts`] — context
/// only, never a scope input (contextualize-don't-widen).
fn project_change_panel(
    new_text: &str,
    definition: Option<&crate::domain::ProjectDefinition>,
    current: &Manifest,
    index: &NormalizedDiffIndex,
) -> PanelFacts {
    let hunks = index.hunks_for(DBT_PROJECT_YML);
    let fallback = |reason: ProjectFallbackReason| {
        (
            ProjectChangePanel::Fallback {
                reason,
                raw: raw_hunk_lines(hunks),
            },
            BTreeMap::new(),
            BTreeMap::new(),
        )
    };
    let Some(new_def) = definition else {
        return fallback(ProjectFallbackReason::NewParseFailed);
    };
    let Ok(old_text) = reverse_apply(new_text, hunks) else {
        return fallback(ProjectFallbackReason::OldNotReconstructable);
    };
    // An empty old text (file created in this PR) parses to the default
    // definition, so the diff below reports every entry as added.
    let Ok(old_def) = parse_project_definition(&old_text) else {
        return fallback(ProjectFallbackReason::OldParseFailed);
    };
    let mut changes = diff_project_definitions(&old_def, new_def);
    let ops = hook_operations(current, new_def.name.as_deref().unwrap_or_default());
    attach_hook_facts(&mut changes, &ops);
    let var_analysis = attribute_var_changes(current, &old_def, new_def);
    attach_var_facts(&mut changes, &var_analysis);
    let attributions = attribute_config_tree_changes(current, &old_def, new_def, &changes);
    (
        ProjectChangePanel::Categorized { changes },
        attributions,
        var_analysis.references,
    )
}

/// Merge external fixture FILE cell diffs (cute-dbt#126 AC#3) into the
/// YAML-block-derived `data_diffs`. For each rendered test's loaded external
/// `given`/`expect`, reconstruct the file's old→new cell diff from its OWN
/// hunks (`index.hunks_for(fixture_path)`) and splice it in by source ordinal.
///
/// A test's givens are inline XOR external by ordinal, so the YAML-block path
/// (which yields nothing for an external given) and this path never collide;
/// the merged `given` vec is re-sorted by ordinal for deterministic output.
fn merge_external_data_diffs(
    data_diffs: &mut HashMap<String, UnitTestDataDiff>,
    current: &Manifest,
    in_scope: &InScopeSet,
    external_fixtures: &HashMap<String, ExternalFixtures>,
    index: &NormalizedDiffIndex,
) {
    for id in in_scope.iter() {
        let Some(ext) = external_fixtures.get(id) else {
            continue;
        };
        let Some(unit_test) = current.unit_test(id) else {
            continue;
        };
        let given_diffs = external_given_diffs(unit_test, ext, index);
        let expect_diff = external_expect_diff(unit_test, ext, index);
        if given_diffs.is_empty() && expect_diff.is_none() {
            continue;
        }
        let entry = data_diffs.entry(id.to_owned()).or_default();
        entry.given.extend(given_diffs);
        entry.given.sort_by_key(|n| n.ordinal);
        if let Some(diff) = expect_diff {
            entry.expect = Some(diff);
        }
    }
}

/// The external fixture FILE cell diffs for one test's `given` inputs
/// (cute-dbt#126 AC#3), each tagged with its source ordinal.
///
/// The hunk lookup keys on the manifest `fixture` path. For a dbt-core
/// BARE-name fixture (AC#4) that bare name != the file the diff touched
/// (`tests/fixtures/<name>.csv`), so `hunks_for` misses → no external cell
/// diff (graceful: the grid still renders, just without a diff toggle).
/// fusion — the verified primary — emits the resolved path here, so it is
/// unaffected. Re-verify dbt-core at cute-dbt#64.
fn external_given_diffs(
    unit_test: &UnitTest,
    ext: &ExternalFixtures,
    index: &NormalizedDiffIndex,
) -> Vec<NamedTableDiff> {
    let mut diffs = Vec::new();
    for (&ordinal, loaded) in &ext.given {
        let Some(given) = unit_test.given().get(ordinal) else {
            continue;
        };
        let Some(path) = given.fixture() else {
            continue;
        };
        if let Some(diff) = reconstruct_external_fixture_diff(
            &loaded.text,
            loaded.format.as_deref(),
            index.hunks_for(path),
        ) {
            diffs.push(NamedTableDiff {
                ordinal,
                input: given.input().to_owned(),
                diff,
            });
        }
    }
    diffs
}

/// The external fixture FILE cell diff for one test's `expect`, when its
/// fixture file was touched by the PR diff (cute-dbt#126 AC#3).
fn external_expect_diff(
    unit_test: &UnitTest,
    ext: &ExternalFixtures,
    index: &NormalizedDiffIndex,
) -> Option<FixtureTableDiff> {
    let loaded = ext.expect.as_ref()?;
    let path = unit_test.expect().fixture()?;
    reconstruct_external_fixture_diff(
        &loaded.text,
        loaded.format.as_deref(),
        index.hunks_for(path),
    )
}

/// Resolve the rendered report's title + subtitle from `--config`,
/// falling back to [`DEFAULT_REPORT_TITLE`] for an absent / unset title.
///
/// Returns `(title, subtitle)` where `subtitle` is `None` when no
/// config is supplied or the config omits `[report].subtitle` (the
/// renderer then omits the `<p class="report-subtitle">` element
/// entirely).
fn resolve_report_strings(args: &ReportArgs) -> (String, Option<String>) {
    let report_cfg = args.config.as_ref().map(|c| &c.report);
    let title = report_cfg
        .and_then(|r| r.title.clone())
        .unwrap_or_else(|| DEFAULT_REPORT_TITLE.to_owned());
    let subtitle = report_cfg.and_then(|r| r.subtitle.clone());
    (title, subtitle)
}

/// Resolve the source-PR reference (cute-dbt#346) for the change-context
/// banner link, merging the `--pr-url` / `--pr-title` / `--pr-number`
/// flags over the `[pr]` config section (the CLI-over-TOML precedence the
/// `[experimental] macro_body_cap` knob already follows). Each flag, when
/// supplied, overrides the matching config key; the merged
/// [`PrConfig`] is then
/// [`resolve`](crate::domain::PrConfig::resolve)d — yielding `Some` only
/// when both a url and a title are present (graceful degradation otherwise).
///
/// `review` supplies the same shape via `--pr-url`/`--pr-title`/`--pr-number`
/// in its composed [`ReportArgs`], derived from `gh pr view`.
fn resolve_pr_ref(args: &ReportArgs) -> Option<PrRef> {
    let cfg = args
        .config
        .as_ref()
        .map(|c| c.pr.clone())
        .unwrap_or_default();
    let merged = PrConfig {
        url: args.pr_url.clone().or(cfg.url),
        title: args.pr_title.clone().or(cfg.title),
        number: args.pr_number.or(cfg.number),
    };
    merged.resolve()
}

/// Build the resolved check display policy (cute-dbt#171): the
/// `[checks]` config selection + suppress entries, extended with the
/// inline `-- cute-dbt: ignore(check-id, "reason")` pragmas scanned
/// from each in-scope model's raw SQL.
///
/// The pragma source is the manifest's `raw_code` — the verbatim
/// authored model file (the cute-dbt#111 precedent: no filesystem read,
/// so pragmas work in both scope modes with or without
/// `--project-root`). A pragma naming an unknown check id is **not** a
/// run failure (it is source text, not config — the config arm fails
/// closed at `--config` parse time instead): it warns on stderr via
/// [`warn_unknown_pragma`] and is otherwise inert.
///
/// The `[checks]` section was already validated by the `--config`
/// value-parser ([`crate::adapters::config_reader::load_config`]), so
/// re-resolving here cannot fail; the `expect` pins that invariant for
/// any future caller constructing [`Cli`] by hand.
fn build_check_policy(
    args: &ReportArgs,
    current: &Manifest,
    models_in_scope: &ModelInScopeSet,
    experiments: &EnabledExperiments,
) -> CheckPolicy<HeuristicId> {
    let mut policy = args.config.as_ref().map_or_else(CheckPolicy::default, |c| {
        resolve_check_policy::<HeuristicId>(&c.checks)
            .expect("[checks] was validated by the --config value-parser at parse time")
    });
    // cute-dbt#260 Slice 3 — reconcile the experiment-gated `enforcement`
    // group with the governance flag, AFTER either policy arm (the
    // `CheckPolicy::default()` arm already filters experimental checks;
    // the `[checks]` config arm resolves from `Id::ALL` and does not). So
    // this single post-step is the one authority: governance OFF removes
    // every experimental check from the display set (byte-identical to
    // pre-#260 output, and the gate-free `explore` page already uses the
    // filtered default); governance ON re-adds them in registry order.
    // The detector still EVALUATES regardless (the suppression-hierarchy
    // invariant) — this is a display filter only.
    reconcile_experimental_checks(&mut policy.displayed, experiments);
    // Model order is deterministic (the scope set iterates in node-id
    // order), so pragma rule order — and warning order — is stable.
    for model_id in models_in_scope.iter() {
        let Some(raw_code) = current.node(model_id).and_then(|node| node.raw_code()) else {
            continue;
        };
        for pragma in scan_pragmas(raw_code) {
            match check_by_id::<HeuristicId>(&pragma.check) {
                Some(check) => policy.suppressions.push(SuppressRule {
                    check,
                    model: model_id.as_str().to_owned(),
                    reason: pragma.reason,
                    source: SuppressionSource::Pragma,
                }),
                None => warn_unknown_pragma(model_id.as_str(), &pragma.check),
            }
        }
    }
    policy
}

/// Reconcile the experiment-gated checks (the `enforcement` group, gated
/// behind `Experiment::Governance`) in `displayed` with the active
/// experiment set (cute-dbt#260 Slice 3). Governance OFF ⇒ drop every
/// experimental check; governance ON ⇒ ensure every experimental check is
/// present, re-inserted in registry (`HeuristicId::ALL`) order so the
/// display set stays deterministic. The single authority over both
/// policy arms (default + `[checks]` config).
fn reconcile_experimental_checks(
    displayed: &mut Vec<HeuristicId>,
    experiments: &EnabledExperiments,
) {
    use crate::domain::CheckId as _;
    // For Slice 3 the only experiment-gated group is `enforcement`,
    // gated behind `Experiment::Governance`. Future gated groups join
    // this predicate.
    let gated_enabled = experiments.is_enabled(Experiment::Governance);
    if gated_enabled {
        // Rebuild from the registry, keeping declaration order: a
        // non-experimental check stays iff it was displayed; an
        // experimental check is shown.
        let keep: std::collections::BTreeSet<HeuristicId> = displayed.iter().copied().collect();
        *displayed = HeuristicId::ALL
            .iter()
            .copied()
            .filter(|id| id.is_experimental() || keep.contains(id))
            .collect();
    } else {
        displayed.retain(|id| !id.is_experimental());
    }
}

/// Resolve the experimental opt-in set (cute-dbt#289, epic #288):
/// enabled = `[experimental]` TOML set ∪ `CUTE_DBT_EXPERIMENTAL` env
/// set.
///
/// Both arms were already validated at parse time — the TOML by the
/// `--config` value-parser
/// ([`crate::adapters::config_reader::load_config`]), the env value by
/// the clap env-fallback value-parser on
/// [`args::ReportArgs::experimental`] — so re-resolving here cannot
/// fail; the `expect` pins that invariant for any future caller
/// constructing [`Cli`] by hand (the [`build_check_policy`] posture).
fn resolve_enabled_experiments(args: &ReportArgs) -> EnabledExperiments {
    let toml = args.config.as_ref().map_or_else(Default::default, |c| {
        resolve_experimental_config(&c.experimental)
            .expect("[experimental] was validated by the --config value-parser at parse time")
    });
    let no_env = BTreeSet::new();
    let env = args.experimental.as_ref().unwrap_or(&no_env);
    EnabledExperiments::from_union(&toml, env)
}

/// Today's UTC civil date as a [`DepDate`] (cute-dbt#260 Slice 4) — the
/// I/O-boundary `SystemTime::now()` read, kept OUT of the pure domain so
/// the deprecation scheduled/elapsed comparison stays deterministic +
/// testable (the domain takes the date as a parameter). A
/// pre-`UNIX_EPOCH` clock (impossible in practice) falls back to the
/// epoch date — the chip degrades to a stable result, never a panic.
fn today_dep_date() -> DepDate {
    // `as_secs()` is `u64`; whole days since the epoch fit in `i64` for
    // ~25 trillion years, so the conversion is exact (and the `Err`
    // pre-epoch arm — impossible in practice — degrades to day 0).
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0_i64, |d| i64::try_from(d.as_secs() / 86_400).unwrap_or(0));
    let (year, month, day) = civil_from_days(days);
    DepDate { year, month, day }
}

/// The findings envelope's `generated_at`, computed at the I/O boundary
/// (cute-dbt#386).
///
/// An **RFC3339 date** (`YYYY-MM-DD`) — a deliberate precision choice: a
/// full RFC3339 *date-time* would need timezone + time-of-field formatting
/// that std cannot produce without a `chrono`/`time` dependency, and
/// cute-dbt is std-only by posture (cargo-deny + the no-extra-date-crate
/// line). The date is the deterministic, golden-stable granularity the
/// envelope needs (the committed golden pins a fixed fixture date); a
/// finer-grained timestamp would defeat byte-identity gating. Reuses the
/// same `today_dep_date` → `civil_from_days` machinery as the governance
/// deprecation chips, so "today" is computed once, the same way, at the one
/// I/O boundary. `YYYY-MM-DD` is a valid RFC3339 `full-date`.
fn today_rfc3339_date() -> String {
    let DepDate { year, month, day } = today_dep_date();
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days-since-Unix-epoch → `(year, month, day)` (proleptic Gregorian) —
/// Howard Hinnant's `civil_from_days` algorithm, std-only (the domain
/// forbids `chrono`). Pure + total. `m`/`d` are in `[1, 12]` / `[1, 31]`
/// and `year` in a realistic range, so the narrowing conversions are
/// lossless; `try_from` makes that explicit + total (any impossible
/// out-of-range value degrades to 0 rather than wrapping).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (
        i32::try_from(year).unwrap_or(0),
        u32::try_from(m).unwrap_or(0),
        u32::try_from(d).unwrap_or(0),
    )
}

/// Emit a stderr note for an inline pragma naming an unknown check id
/// (cute-dbt#171). The pragma is inert — without this note a typo'd id
/// would silently suppress nothing while the author believes it does.
fn warn_unknown_pragma(model_id: &str, check: &str) {
    eprintln!(
        "cute-dbt: warning: {model_id} carries the pragma `-- cute-dbt: \
         ignore({check}, ...)` but {check:?} is not a registered check; \
         the pragma has no effect (see heuristics/registry.toml for \
         known check ids)"
    );
}

/// Stage-1 pre-flight: load the primary `--manifest` through the
/// file-backed [`ManifestSource`].
///
/// A load failure is `Unreadable` / `SchemaUnsupported`. The baseline
/// manifest (when scoping via `--baseline-manifest`) is loaded separately
/// in [`resolve_scope_input`] so the `--pr-diff` path can skip
/// it entirely.
fn load_current(args: &ReportArgs) -> Result<Manifest, RunError> {
    let source = FileManifestSource;
    let current = source.load(&args.manifest)?;
    Ok(current)
}

/// Resolve the scope source the operator selected into a [`ScopeInput`].
///
/// - `--baseline-manifest` → load the baseline (Stage-1 pre-flight; a
///   failure is remapped to `BaselineUnusable` by [`load_baseline`]) and
///   wrap it in [`ScopeInput::Baseline`] together with any opt-in
///   `--modified-selectors` sub-selector kinds (cute-dbt#160; clap
///   rejects the flag on the `--pr-diff` arm at parse time).
/// - `--pr-diff` → build the single [`NormalizedDiffIndex`] from the
///   parsed diff and the `--project-root` strip, and wrap it in
///   [`ScopeInput::PrDiff`]. The index rebases the diff's repo-relative
///   paths onto the manifest's project-relative `original_file_path`.
///
/// clap's `scope_source` [`ArgGroup`](clap::ArgGroup) (`required`,
/// single) guarantees exactly one arm is set, so the trailing branch is
/// unreachable.
fn resolve_scope_input(args: &ReportArgs) -> Result<ScopeInput, RunError> {
    if let Some(baseline_path) = args.baseline_manifest.as_deref() {
        let source = FileManifestSource;
        let baseline = load_baseline(&source, baseline_path)?;
        let sub_selectors = args
            .modified_selectors
            .iter()
            .map(|selector| selector.kind())
            .collect();
        Ok(ScopeInput::Baseline {
            manifest: Box::new(baseline),
            sub_selectors,
        })
    } else if let Some(diff) = args.pr_diff.as_ref() {
        // Build the single NormalizedDiffIndex ONCE here and thread the
        // one instance through scope selection (and cute-dbt#96's
        // block-precise refinement + inline diff). It is the sole
        // normalization authority — the `--project-root` strip is baked
        // in as its diff-side strip (CAO plan-audit Decision 2).
        let index = NormalizedDiffIndex::new(diff, args.project_root.as_deref());
        Ok(ScopeInput::PrDiff { index })
    } else {
        unreachable!(
            "clap's scope_source ArgGroup guarantees exactly one of \
             --baseline-manifest / --pr-diff is provided"
        )
    }
}

/// Gather the macro perspective lens (cute-dbt#265, Slice B), gated behind
/// the `macro-lens` experiment (the epic #288 default-OFF posture).
///
/// Enabled: resolve the changed root-project macros by scope arm — the
/// `PrDiff` arm runs the path-primary + name-fallback heuristic over the
/// diff index, the `Baseline` arm runs the exact `macro_sql` body
/// comparison (fusion's `check_modified_macros` semantics) — then build the
/// section
/// (per-macro body diff + reverse blast-radius directory tree + count +
/// fidelity chip). Default (off): `None` ⇒ omitted from the JSON payload
/// and zero DOM via `{% if macro_lens %}`, so the non-macro goldens stay
/// byte-identical. A pure pass over the already-parsed manifest + the diff
/// index (no file read, zero-egress); the blast radius never widens scope
/// (it is a render-only contextualizer, the governance posture). The
/// diff-showcase golden row sets `experimental:"1"`, so a changed
/// playground macro surfaces here.
fn gather_macro_lens(
    current: &Manifest,
    scope_input: &ScopeInput,
    experiments: &EnabledExperiments,
    body_cap: usize,
) -> Option<MacroLensPayload> {
    if !experiments.is_enabled(Experiment::MacroLens) {
        return None;
    }
    let (changed_macros, index, source) = match scope_input {
        ScopeInput::PrDiff { index } => (
            changed_macros_pr_diff(current, index),
            Some(index),
            ScopeSource::PrDiff,
        ),
        ScopeInput::Baseline { manifest, .. } => (
            changed_macros_baseline(current, manifest),
            None,
            ScopeSource::Baseline,
        ),
    };
    build_macro_lens(current, &changed_macros, source, index, body_cap)
}

/// Gather the PR-scope lineage mini-DAG (cute-dbt#404, epic #352), gated
/// behind the `pr-scope-mini-dag` experiment (the epic #288 default-OFF
/// posture). **This is the single gating source** — the function returns
/// `None` before computing anything when the experiment is off, so nothing
/// crosses to the render payload, the `{% match pr_dag %}` section emits zero
/// DOM, and every default golden stays byte-identical (the `gather_macro_lens`
/// precedent).
///
/// Enabled, per scope arm:
/// - **pr-diff** — the modified set is [`changed_models`] (models whose
///   `original_file_path` is in the diff); the deleted set is the diff's
///   `deleted` paths resolved against the BASELINE manifest if one were
///   present (none on this arm), so it is empty here; per-node line counts
///   come from [`pr_dag_lines_from_diff`] (the `+`/`-` hunk counts for the
///   node's file). The `new` (added) subset is left empty: the pr-diff arm has
///   no baseline to diff membership against, so an added model is reported as
///   `Modified` (the safe default the topology documents).
/// - **baseline** — the modified set is the [`StateComparator`] modified
///   models (changed `checksum`); the `new` subset is the current models
///   absent from the baseline; the deleted set is the baseline models absent
///   from the current manifest (the baseline−current set-diff); per-node line
///   counts come from [`pr_dag_lines_from_raw_code`] (the `raw_code` old→new
///   line diff), which also surfaces a deleted ghost's removed-everything count.
///
/// A pure pass over the already-parsed manifest(s) + the diff index (no file
/// read, zero-egress). The mini-DAG is a render-only contextualizer — it never
/// widens scope (it *consumes* the scope sets, the `pr_dag` module contract).
/// `None` whenever the scope set is empty (nothing to draw), so an empty-scope
/// report carries no mini-DAG bytes.
/// Gather the PR review-comments render view (cute-dbt#419–#422, epic
/// #353), gated behind the `pr-comments` experiment.
///
/// **This is the single gating source** — the function returns `None`
/// before resolving anything when the experiment is off, so nothing
/// crosses to the render payload, the static count container stays empty
/// (the JS never fills it), and every default golden stays byte-identical
/// (the `gather_pr_dag` precedent).
///
/// The comment surface anchors onto a **rendered diff**, so it is the
/// **`--pr-diff` arm only**: the baseline arm has no diff hunks to anchor a
/// thread to (the `pr_ref` / annotations precedent). On the baseline arm
/// (or with no diff) this returns `None`.
///
/// The ingested threads come from one of two surfaces, in precedence
/// order:
/// 1. `--pr-comments @<file>` (the deterministic golden / test seam, the
///    `--pr-diff @file` precedent) — already parsed into a
///    [`PrComments`](crate::domain::PrComments) by the clap value-parser;
/// 2. a live `gh api graphql` fetch
///    ([`fetch_pr_comments`](crate::adapters::pr_comments::fetch_pr_comments))
///    when a PR can be identified from `--pr-url` + `--pr-number` (or
///    `[pr]`-config-derived) and `gh` is available — fail-soft to empty if
///    not (PR comments are context, never a dependency).
///
/// The threads are then anchored + grouped per model by the pure domain
/// [`group_comment_threads`] (which re-uses the shipped
/// [`anchor_comment_thread`](crate::domain::anchor_comment_thread), never
/// re-anchoring). An empty result ⇒ `None` so the section stays byte-quiet.
fn gather_pr_comments(
    args: &ReportArgs,
    current: &Manifest,
    scope_input: &ScopeInput,
    experiments: &EnabledExperiments,
) -> Option<CommentsView> {
    if !experiments.is_enabled(Experiment::PrComments) {
        return None;
    }
    // Comments anchor to a rendered diff → PrDiff arm only.
    let ScopeInput::PrDiff { index } = scope_input else {
        return None;
    };
    let comments = resolve_pr_comments(args);
    if comments.threads.is_empty() {
        // No line-anchored review threads to surface (general issue-level
        // comments are not part of this inline-anchoring slice).
        return None;
    }
    // The project-root strip the diff index was built with (cute-dbt#418):
    // GitHub thread paths are repo-relative; the diff index + manifest are
    // project-relative. Resolve the same way the diff index was built so a
    // sub-directory dbt project's thread paths reconcile.
    let project_root = args.project_root.clone();
    let view = group_comment_threads(&comments.threads, current, index, project_root.as_deref());
    if view.is_empty() { None } else { Some(view) }
}

/// Resolve the PR review comments to ingest (cute-dbt#419): the
/// `--pr-comments @file` payload when supplied (the deterministic golden /
/// test seam), else a live `gh api graphql` fetch when a PR can be
/// identified from `--pr-url` / `--pr-number` (or `[pr]` config). Fail-soft
/// to empty when neither is available — PR comments are context, never a
/// dependency.
fn resolve_pr_comments(args: &ReportArgs) -> crate::domain::PrComments {
    if let Some(comments) = &args.pr_comments {
        return comments.clone();
    }
    // The live fetch path: identify the PR (owner/repo/number) and shell
    // `gh`. The owner/repo come from the resolved PR url; the number from
    // `--pr-number` / the url's `/pull/<n>` segment (the `resolve_pr_ref`
    // merge). With no resolvable PR, ingest nothing (the file seam is the
    // canonical golden path; the live path is best-effort).
    let Some(pr_ref) = resolve_pr_ref(args) else {
        return crate::domain::PrComments::default();
    };
    let Some((owner, repo)) = owner_repo_from_url(&pr_ref.url) else {
        return crate::domain::PrComments::default();
    };
    let (resolved_root, _derived) =
        self::args::resolve_project_root(args.project_root.as_deref(), &args.manifest);
    let cwd = resolved_root.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    crate::adapters::pr_comments::fetch_pr_comments(&cwd, &owner, &repo, pr_ref.number)
}

/// Extract `(owner, repo)` from a GitHub PR url
/// (`https://github.com/<owner>/<repo>/pull/<n>`). `None` for any url that
/// does not carry both segments before `/pull/`.
fn owner_repo_from_url(url: &str) -> Option<(String, String)> {
    // Take the path after the host, then the first two non-empty segments
    // before `/pull/`.
    let after_host = url.split("github.com/").nth(1)?;
    let before_pull = after_host.split("/pull/").next()?;
    let mut segs = before_pull.split('/').filter(|s| !s.is_empty());
    let owner = segs.next()?.to_owned();
    let repo = segs.next()?.to_owned();
    Some((owner, repo))
}

fn gather_pr_dag(
    current: &Manifest,
    scope_input: &ScopeInput,
    experiments: &EnabledExperiments,
    axes: &BTreeMap<NodeId, ChangeAxes>,
) -> Option<PrDagPayload> {
    if !experiments.is_enabled(Experiment::PrScopeMiniDag) {
        return None;
    }
    let (modified, new, removed) = pr_dag_scope_sets(current, scope_input);
    if modified.is_empty() && removed.is_empty() {
        // Nothing modified and nothing deleted ⇒ no graph to draw.
        return None;
    }
    let mut graph = compute_pr_dag(current, &modified, &new, &removed);
    populate_pr_dag_line_counts(&mut graph, current, scope_input);
    // cute-dbt#430 — the per-axis pre-computed subgraphs feeding the #414
    // filter-reactive mini-DAG. For each change-axis (`body` / `config` /
    // `unit_test`), recompute the mini-DAG over the SUBSET of modified models
    // whose `axes.<axis>` fired (connectors + halo re-derived over that
    // smaller seed set, IN RUST), so the JS only toggles between the
    // pre-emitted sets. Empty `axes` (the baseline arm) yields an empty map ⇒
    // the payload's `by_axis` serde-skips ⇒ baseline goldens stay
    // byte-identical.
    let by_axis = pr_dag_axis_subgraphs(current, scope_input, &modified, &new, &removed, axes);
    Some(PrDagPayload::from_graph_with_axes(
        graph,
        by_axis,
        DEFAULT_PR_DAG_NODE_CAP,
    ))
}

/// Recompute one mini-DAG per change-axis over the axis-filtered modified
/// subset (cute-dbt#430). Returns `axis-token → recomputed graph` for the
/// three axes (`body` / `config` / `unit_test`), each populated with per-node
/// line counts identically to the `all` graph. An axis whose subset is empty
/// still gets an entry (the empty graph) so the JS always has a view to swap
/// to (it renders the honest "0 models" empty state for that filter).
///
/// The `removed` ghosts are intentionally **not** carried into the per-axis
/// subgraphs: a deletion has no current node to carry a `ChangeAxes`
/// attribution, so it belongs only to the `all` view. (On the `--pr-diff`
/// arm — the only arm with `axes` — `removed` is always empty anyway, so this
/// is a no-op there; the documented choice is for forward-safety.)
///
/// Empty `axes` (the baseline arm) short-circuits to an empty map, keeping the
/// payload byte-identical to the pre-#430 / baseline shape.
fn pr_dag_axis_subgraphs(
    current: &Manifest,
    scope_input: &ScopeInput,
    modified: &ModelInScopeSet,
    new: &BTreeSet<NodeId>,
    removed: &[NodeId],
    axes: &BTreeMap<NodeId, ChangeAxes>,
) -> BTreeMap<String, crate::domain::PrDagGraph> {
    /// The `ChangeAxes`-bit predicate one filter axis reads (`a.body` etc.).
    type AxisPredicate = fn(&ChangeAxes) -> bool;
    if axes.is_empty() {
        return BTreeMap::new();
    }
    // The three #414 filter axes, paired with the `ChangeAxes` bit each reads.
    // (`removed` is dropped per the function contract above.)
    let _ = removed;
    let axis_predicates: [(&str, AxisPredicate); 3] = [
        ("body", |a| a.body),
        ("config", |a| a.config),
        ("unit_test", |a| a.unit_test),
    ];
    let mut by_axis = BTreeMap::new();
    for (token, fired) in axis_predicates {
        // The modified subset whose this-axis bit fired. A modified model with
        // no `axes` entry (e.g. a config-tree-widened context model) never
        // fires a specific axis, matching the JS `modelMatchesAxis` semantics.
        let subset: ModelInScopeSet = modified
            .iter()
            .filter(|id| axes.get(*id).is_some_and(fired))
            .cloned()
            .collect();
        // The `new` subset restricted to this axis subset (so an added model
        // stays New within its axis view).
        let new_subset: BTreeSet<NodeId> = new
            .iter()
            .filter(|id| subset.contains(id))
            .cloned()
            .collect();
        let mut graph = compute_pr_dag(current, &subset, &new_subset, &[]);
        populate_pr_dag_line_counts(&mut graph, current, scope_input);
        by_axis.insert(token.to_owned(), graph);
    }
    by_axis
}

/// Derive the `(modified, new, removed)` node sets `compute_pr_dag` consumes,
/// per scope arm (cute-dbt#404). See [`gather_pr_dag`] for the per-arm
/// semantics; this is the set-derivation half, factored out so the gather
/// stays a thin compose.
fn pr_dag_scope_sets(
    current: &Manifest,
    scope_input: &ScopeInput,
) -> (ModelInScopeSet, BTreeSet<NodeId>, Vec<NodeId>) {
    match scope_input {
        ScopeInput::PrDiff { index } => {
            let modified = changed_models(current, index);
            // The pr-diff arm has no baseline manifest to diff membership
            // against — an added model is reported as `Modified` (the safe
            // default), and deletions are not surfaced as ghosts here (a
            // deleted model has no current node to anchor a reliable id;
            // the baseline arm owns the deletion ghosts).
            (modified, BTreeSet::new(), Vec::new())
        }
        ScopeInput::Baseline { manifest, .. } => {
            let baseline = manifest.as_ref();
            let cmp = StateComparator::body_only();
            let modified = cmp.models_in_scope(current, baseline);
            // `new`: current models absent from the baseline (the added set).
            let new: BTreeSet<NodeId> = current
                .nodes()
                .iter()
                .filter(|(_, node)| node.resource_type() == "model")
                .filter(|(id, _)| baseline.node(id).is_none())
                .map(|(id, _)| id.clone())
                .collect();
            // `removed`: baseline models absent from the current manifest (the
            // deleted ghosts — the baseline−current set-diff).
            let removed: Vec<NodeId> = baseline
                .nodes()
                .iter()
                .filter(|(_, node)| node.resource_type() == "model")
                .filter(|(id, _)| current.node(id).is_none())
                .map(|(id, _)| id.clone())
                .collect();
            (modified, new, removed)
        }
    }
}

/// Fold the per-node line counts onto `graph` (cute-dbt#404 Slice C
/// population), keyed by scope arm — the pr-diff arm sums the diff hunks for
/// each node's file ([`pr_dag_lines_from_diff`]); the baseline arm diffs each
/// node's `raw_code` old→new ([`pr_dag_lines_from_raw_code`]).
fn populate_pr_dag_line_counts(
    graph: &mut crate::domain::PrDagGraph,
    current: &Manifest,
    scope_input: &ScopeInput,
) {
    match scope_input {
        ScopeInput::PrDiff { index } => {
            populate_line_counts(graph, |node| {
                let id = NodeId::new(&node.id);
                let ofp = current.node(&id).and_then(Node::original_file_path);
                pr_dag_lines_from_diff(ofp, index)
            });
        }
        ScopeInput::Baseline { manifest, .. } => {
            let baseline = manifest.as_ref();
            populate_line_counts(graph, |node| {
                let id = NodeId::new(&node.id);
                let old = baseline.node(&id).and_then(Node::raw_code);
                let new = current.node(&id).and_then(Node::raw_code);
                pr_dag_lines_from_raw_code(old, new)
            });
        }
    }
}

/// Resolve the macro-lens inline-body cap (cute-dbt#265 Slice D, founder
/// D5) — the gen-time knob bounding how many impacted-model SQL bodies the
/// experimental section server-renders inline.
///
/// Precedence: the `--macro-body-cap` flag wins over the
/// `[experimental] macro_body_cap` config key, which wins over
/// [`DEFAULT_MACRO_BODY_CAP`]. The CLI-over-TOML order mirrors the
/// surrounding flag-over-config posture; both surfaces are already
/// validated at parse time (a `usize` value-parser), so this is a pure
/// pick with no failure mode. The cap is inert when the `macro-lens`
/// experiment is off (`gather_macro_lens` returns `None` before consuming
/// it), so resolving it unconditionally here is free of side effects.
fn resolve_macro_body_cap(args: &ReportArgs) -> usize {
    args.macro_body_cap
        .or_else(|| {
            args.config
                .as_ref()
                .and_then(|c| c.experimental.macro_body_cap)
        })
        .unwrap_or(DEFAULT_MACRO_BODY_CAP)
}

/// The seed current-table row cap (cute-dbt#350), resolved at the I/O
/// boundary: `[seeds] row_cap` from `--config` over
/// [`DEFAULT_SEED_ROW_CAP`]. Config-only (no CLI flag — the cap is a
/// gen-time knob authored once in the config, the report-title precedent
/// rather than the macro-lens dual flag/config knob). Keeps the render side
/// a pure fn of the cap value.
fn resolve_seed_row_cap(args: &ReportArgs) -> usize {
    args.config
        .as_ref()
        .and_then(|c| c.seeds.row_cap)
        .unwrap_or(DEFAULT_SEED_ROW_CAP)
}

/// The diff-scope banner inputs for the selected scope source.
///
/// Returns `(baseline_label, scope_source)`. The baseline arm carries the
/// `--baseline-manifest` path verbatim (rendered in the banner's
/// `.diff-scope-baseline` element); the PR-diff arm carries an empty
/// label — its banner names no baseline manifest (cute-dbt#85).
fn scope_banner(args: &ReportArgs, scope_input: &ScopeInput) -> (String, ScopeSource) {
    match scope_input {
        ScopeInput::Baseline { .. } => (
            args.baseline_manifest
                .as_ref()
                .map_or_else(String::new, |p| p.display().to_string()),
            ScopeSource::Baseline,
        ),
        ScopeInput::PrDiff { .. } => (String::new(), ScopeSource::PrDiff),
    }
}

/// Emit a single stderr note when the supplied `--pr-diff` is not
/// `git diff --unified=0` (cute-dbt#111).
///
/// A context-bearing diff degrades every affected block to the plain view
/// (the `reconstruct_one` contract guard in `domain::pr_diff`), so without
/// this note a user who forgot `--unified=0` would see plain views and
/// assume the inline diffs are broken. The decision is a pure domain
/// predicate ([`NormalizedDiffIndex::context_bearing_hunk_count`]); the
/// `eprintln!` (the only I/O) lives here in cli, not in `domain`. Emitted
/// once per run on the `PrDiff` arm only.
fn warn_if_not_unified_zero(index: &NormalizedDiffIndex) {
    let n = index.context_bearing_hunk_count();
    if n > 0 {
        let plural = if n == 1 { "hunk" } else { "hunks" };
        eprintln!(
            "cute-dbt: warning: the supplied diff is not `git diff --unified=0` \
             ({n} context-bearing {plural}); inline diffs are disabled — showing \
             plain views. Re-run the diff with --unified=0 for inline diffs."
        );
    }
}

/// The `parse_ctes` stage — a named no-op call site.
///
/// Per-model CTE parsing happens inside the renderer's payload
/// assembly: each in-scope model parses its own `compiled_code` exactly
/// once when `render_report` walks `models_in_scope`. The run loop keeps
/// this name greppable so `ARCHITECTURE.md` §3's four-stage diagram
/// still resolves; the work happens one stage downstream.
fn parse_ctes() {}

/// The `render` stage — invokes the askama renderer.
///
/// `render` is the last stage: an earlier fail-closed `?` short-circuits
/// before this is reached, so no `report.html` is ever partially written.
/// `baseline_label` + `scope_source` drive the diff-scope banner:
/// `ScopeSource::Baseline` names the baseline manifest (the
/// `--baseline-manifest` path verbatim); `ScopeSource::PrDiff` omits the
/// baseline clause (`baseline_label` is then empty).
// Thin pass-through to `render_report_with_externals`; mirrors its argument
// list (the composition-root rationale lives there).
#[allow(clippy::too_many_arguments)]
fn render(
    out: &Path,
    current: &Manifest,
    in_scope: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    changed: &InScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    model_yaml: &HashMap<String, ModelYamlOutcome>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    external_fixtures: &HashMap<String, ExternalFixtures>,
    baseline_label: &str,
    scope_source: ScopeSource,
    report_title: &str,
    report_subtitle: Option<&str>,
    check_policy: &CheckPolicy<HeuristicId>,
    project_facts: &ProjectFacts,
    governance: &GovernanceFacts,
    macro_lens: Option<&MacroLensPayload>,
    pr_ref: Option<&PrRef>,
    seed_cards: &[SeedCard],
    seed_row_cap: usize,
    pr_dag: Option<&PrDagPayload>,
    axes: &BTreeMap<NodeId, ChangeAxes>,
    model_states: &BTreeMap<NodeId, ModelState>,
    removed_models: &[String],
    pr_comments: Option<&CommentsView>,
) -> Result<(), io::Error> {
    render_report_with_externals(
        out,
        current,
        in_scope,
        models_in_scope,
        changed,
        authoring_yaml,
        yaml_diffs,
        sql_diffs,
        model_yaml,
        data_diffs,
        external_fixtures,
        baseline_label,
        scope_source,
        report_title,
        report_subtitle,
        check_policy,
        project_facts,
        governance,
        macro_lens,
        pr_ref,
        seed_cards,
        seed_row_cap,
        pr_dag,
        axes,
        model_states,
        removed_models,
        pr_comments,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(out: &str) -> ReportArgs {
        ReportArgs {
            manifest: "current.json".into(),
            baseline_manifest: Some("baseline.json".into()),
            out: out.into(),
            config: None,
            project_root: None,
            pr_diff: None,
            modified_selectors: Vec::new(),
            experimental: None,
            macro_body_cap: None,
            pr_url: None,
            pr_title: None,
            pr_number: None,
            pr_comments: None,
            findings_out: None,
            fail_on_uncovered: false,
            annotations: false,
            generated_at: None,
        }
    }

    #[test]
    fn a_preflight_failure_message_is_the_remediation_text() {
        let failure = RunError::Preflight(PreflightError::NotCompiled {
            node_id: "model.shop.stg_orders".to_owned(),
            unit_test: Some("t".to_owned()),
        });
        let msg = failure.message();
        assert!(msg.contains("model.shop.stg_orders"), "{msg}");
        assert!(msg.contains("dbt compile"), "{msg}");
    }

    #[test]
    fn resolve_report_strings_uses_the_default_title_without_config() {
        let cli = cli("report.html");
        let (title, subtitle) = resolve_report_strings(&cli);
        assert_eq!(title, DEFAULT_REPORT_TITLE);
        assert!(subtitle.is_none());
    }

    #[test]
    fn resolve_report_strings_uses_the_default_title_when_config_omits_title() {
        let mut cli = cli("report.html");
        cli.config = Some(crate::domain::AnalysisConfig {
            report: crate::domain::ReportConfig {
                title: None,
                subtitle: Some("PR 1234".to_owned()),
            },
            checks: crate::domain::ChecksConfig::default(),
            experimental: crate::domain::ExperimentalConfig::default(),
            pr: crate::domain::PrConfig::default(),
            seeds: crate::domain::SeedsConfig::default(),
        });
        let (title, subtitle) = resolve_report_strings(&cli);
        assert_eq!(title, DEFAULT_REPORT_TITLE);
        assert_eq!(subtitle.as_deref(), Some("PR 1234"));
    }

    #[test]
    fn resolve_report_strings_uses_configured_title_and_subtitle() {
        let mut cli = cli("report.html");
        cli.config = Some(crate::domain::AnalysisConfig {
            report: crate::domain::ReportConfig {
                title: Some("Q3 review".to_owned()),
                subtitle: Some("PR 1234 / staging diff".to_owned()),
            },
            checks: crate::domain::ChecksConfig::default(),
            experimental: crate::domain::ExperimentalConfig::default(),
            pr: crate::domain::PrConfig::default(),
            seeds: crate::domain::SeedsConfig::default(),
        });
        let (title, subtitle) = resolve_report_strings(&cli);
        assert_eq!(title, "Q3 review");
        assert_eq!(subtitle.as_deref(), Some("PR 1234 / staging diff"));
    }

    #[test]
    fn resolve_report_strings_omits_subtitle_when_absent() {
        let mut cli = cli("report.html");
        cli.config = Some(crate::domain::AnalysisConfig {
            report: crate::domain::ReportConfig {
                title: Some("title-only".to_owned()),
                subtitle: None,
            },
            checks: crate::domain::ChecksConfig::default(),
            experimental: crate::domain::ExperimentalConfig::default(),
            pr: crate::domain::PrConfig::default(),
            seeds: crate::domain::SeedsConfig::default(),
        });
        let (title, subtitle) = resolve_report_strings(&cli);
        assert_eq!(title, "title-only");
        assert!(subtitle.is_none());
    }

    // -----------------------------------------------------------------
    // pr_dag_axis_subgraphs (cute-dbt#430) — the per-axis pre-computed
    // mini-DAG subgraphs feeding the #414 filter-reactive render.
    // -----------------------------------------------------------------

    /// A `model` node with the given full id + `depends_on` producers.
    fn axis_model(id: &str, producers: &[&str]) -> Node {
        Node::new(
            NodeId::new(id),
            "model",
            crate::domain::Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            Some("select 1".to_owned()),
            DependsOn::new(
                Vec::new(),
                producers.iter().map(|p| NodeId::new(*p)).collect(),
            ),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    fn axis_manifest(specs: &[(&str, &[&str])]) -> Manifest {
        let nodes = specs
            .iter()
            .map(|(id, prods)| (NodeId::new(*id), axis_model(id, prods)))
            .collect();
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn axes_entry(body: bool, config: bool, unit_test: bool) -> ChangeAxes {
        ChangeAxes {
            body,
            config,
            unit_test,
        }
    }

    /// An empty `--pr-diff` scope input (the arm the per-axis map populates
    /// on) — the diff is empty so line counts are 0/0, but the topology
    /// recomputation is what the test asserts.
    fn empty_pr_diff_scope() -> ScopeInput {
        let index = NormalizedDiffIndex::new(&crate::domain::PrDiff::default(), None);
        ScopeInput::PrDiff { index }
    }

    #[test]
    fn axis_subgraphs_recompute_connectors_per_axis_subset() {
        // chain stg -> int -> fct. stg + fct modified (body axis fired for
        // both); fct ALSO fired config. int is the unchanged connector.
        let manifest = axis_manifest(&[
            ("model.s.stg", &[]),
            ("model.s.int", &["model.s.stg"]),
            ("model.s.fct", &["model.s.int"]),
        ]);
        let modified: ModelInScopeSet = [NodeId::new("model.s.stg"), NodeId::new("model.s.fct")]
            .into_iter()
            .collect();
        let new = BTreeSet::new();
        let mut axes = BTreeMap::new();
        axes.insert(NodeId::new("model.s.stg"), axes_entry(true, false, false));
        axes.insert(NodeId::new("model.s.fct"), axes_entry(true, true, false));

        let scope = empty_pr_diff_scope();
        let by_axis = pr_dag_axis_subgraphs(&manifest, &scope, &modified, &new, &[], &axes);

        // body: both stg + fct fired body ⇒ int is recomputed as the
        // connector between them (3 nodes, the same as "all").
        let body = by_axis.get("body").expect("body view");
        let body_ids: BTreeSet<&str> = body.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(
            body_ids.contains("model.s.int"),
            "body subset keeps both modified ⇒ int reappears as connector"
        );
        assert!(
            body.nodes
                .iter()
                .any(|n| n.id == "model.s.int" && n.is_connector),
            "int is the recomputed connector in the body view"
        );

        // config: ONLY fct fired config ⇒ a single-seed subset, NO connector
        // (one seed cannot have a between-connector). int must NOT appear as a
        // connector; fct is disconnected and halos its one parent int instead.
        let config = by_axis.get("config").expect("config view");
        assert!(
            !config.nodes.iter().any(|n| n.is_connector),
            "single-seed config subset has no connector"
        );
        assert!(
            config
                .nodes
                .iter()
                .any(|n| n.id == "model.s.fct" && !n.is_connector),
            "fct is the lone modified seed in the config view"
        );

        // unit_test: no model fired unit_test ⇒ empty subset ⇒ empty graph,
        // but the entry still exists so the JS always has a view to swap to.
        let ut = by_axis.get("unit_test").expect("unit_test view present");
        assert!(
            ut.nodes.is_empty(),
            "no unit_test-axis model ⇒ empty subset"
        );
    }

    #[test]
    fn axis_subgraphs_empty_axes_yields_empty_map() {
        // The baseline arm carries no `axes` ⇒ no per-axis views ⇒ byte-
        // identical baseline goldens.
        let manifest = axis_manifest(&[("model.s.a", &[])]);
        let modified: ModelInScopeSet = [NodeId::new("model.s.a")].into_iter().collect();
        let map = pr_dag_axis_subgraphs(
            &manifest,
            &empty_pr_diff_scope(),
            &modified,
            &BTreeSet::new(),
            &[],
            &BTreeMap::new(),
        );
        assert!(map.is_empty(), "no axes ⇒ no per-axis subgraphs");
    }

    // -----------------------------------------------------------------
    // resolve_pr_ref (cute-dbt#346) — the change-context banner PR link,
    // merging --pr-* flags over the [pr] config section.
    // -----------------------------------------------------------------

    #[test]
    fn resolve_pr_ref_is_none_without_flags_or_config() {
        assert!(resolve_pr_ref(&cli("report.html")).is_none());
    }

    #[test]
    fn resolve_pr_ref_from_flags_alone() {
        let mut cli = cli("report.html");
        cli.pr_url = Some("https://github.com/o/r/pull/42".to_owned());
        cli.pr_title = Some("Add churn".to_owned());
        let pr = resolve_pr_ref(&cli).expect("flags resolve a ref");
        assert_eq!(pr.number, 42);
        assert_eq!(pr.title, "Add churn");
        assert_eq!(pr.url, "https://github.com/o/r/pull/42");
    }

    #[test]
    fn resolve_pr_ref_url_without_title_is_none() {
        let mut cli = cli("report.html");
        cli.pr_url = Some("https://github.com/o/r/pull/42".to_owned());
        assert!(resolve_pr_ref(&cli).is_none(), "no title ⇒ no link");
    }

    #[test]
    fn resolve_pr_ref_from_config_section() {
        let mut cli = cli("report.html");
        cli.config = Some(crate::domain::AnalysisConfig {
            report: crate::domain::ReportConfig::default(),
            checks: crate::domain::ChecksConfig::default(),
            experimental: crate::domain::ExperimentalConfig::default(),
            pr: crate::domain::PrConfig {
                url: Some("https://github.com/o/r/pull/9".to_owned()),
                title: Some("from config".to_owned()),
                number: None,
            },
            seeds: crate::domain::SeedsConfig::default(),
        });
        let pr = resolve_pr_ref(&cli).expect("config resolves a ref");
        assert_eq!(pr.number, 9);
        assert_eq!(pr.title, "from config");
    }

    #[test]
    fn resolve_pr_ref_flags_override_config_per_key() {
        let mut cli = cli("report.html");
        cli.config = Some(crate::domain::AnalysisConfig {
            report: crate::domain::ReportConfig::default(),
            checks: crate::domain::ChecksConfig::default(),
            experimental: crate::domain::ExperimentalConfig::default(),
            pr: crate::domain::PrConfig {
                url: Some("https://github.com/o/r/pull/9".to_owned()),
                title: Some("config title".to_owned()),
                number: Some(9),
            },
            seeds: crate::domain::SeedsConfig::default(),
        });
        // The flag overrides the matching config key (CLI-over-TOML).
        cli.pr_title = Some("flag title".to_owned());
        cli.pr_number = Some(99);
        let pr = resolve_pr_ref(&cli).expect("merged ref");
        assert_eq!(pr.title, "flag title", "flag title wins");
        assert_eq!(pr.number, 99, "flag number wins");
        assert_eq!(
            pr.url, "https://github.com/o/r/pull/9",
            "config url retained"
        );
    }

    // -----------------------------------------------------------------
    // resolve_macro_body_cap (cute-dbt#265 Slice D) — the gen-time
    // inline-body cap precedence ladder (flag > config > default).
    // -----------------------------------------------------------------

    fn cli_with_experimental_config(cap: Option<usize>) -> ReportArgs {
        let mut cli = cli("report.html");
        cli.config = Some(crate::domain::AnalysisConfig {
            report: crate::domain::ReportConfig::default(),
            checks: crate::domain::ChecksConfig::default(),
            experimental: crate::domain::ExperimentalConfig {
                enable: vec!["macro-lens".to_owned()],
                macro_body_cap: cap,
            },
            pr: crate::domain::PrConfig::default(),
            seeds: crate::domain::SeedsConfig::default(),
        });
        cli
    }

    #[test]
    fn resolve_macro_body_cap_defaults_when_neither_surface_sets_it() {
        let cli = cli("report.html");
        assert_eq!(resolve_macro_body_cap(&cli), DEFAULT_MACRO_BODY_CAP);
    }

    #[test]
    fn resolve_macro_body_cap_reads_the_config_key() {
        let cli = cli_with_experimental_config(Some(3));
        assert_eq!(resolve_macro_body_cap(&cli), 3);
    }

    #[test]
    fn resolve_macro_body_cap_flag_wins_over_config() {
        // CLI-over-TOML precedence: the flag overrides the config key.
        let mut cli = cli_with_experimental_config(Some(3));
        cli.macro_body_cap = Some(7);
        assert_eq!(resolve_macro_body_cap(&cli), 7);
    }

    #[test]
    fn resolve_macro_body_cap_flag_wins_over_default() {
        let mut cli = cli("report.html");
        cli.macro_body_cap = Some(2);
        assert_eq!(resolve_macro_body_cap(&cli), 2);
    }

    #[test]
    fn resolve_macro_body_cap_accepts_zero() {
        let cli = cli_with_experimental_config(Some(0));
        assert_eq!(resolve_macro_body_cap(&cli), 0);
    }

    // resolve_seed_row_cap (cute-dbt#350) — config-only (no CLI flag);
    // `[seeds] row_cap` over DEFAULT_SEED_ROW_CAP.

    fn cli_with_seed_row_cap(cap: Option<usize>) -> ReportArgs {
        let mut cli = cli("report.html");
        cli.config = Some(crate::domain::AnalysisConfig {
            seeds: crate::domain::SeedsConfig { row_cap: cap },
            ..crate::domain::AnalysisConfig::default()
        });
        cli
    }

    #[test]
    fn resolve_seed_row_cap_defaults_without_config() {
        assert_eq!(
            resolve_seed_row_cap(&cli("report.html")),
            DEFAULT_SEED_ROW_CAP
        );
    }

    #[test]
    fn resolve_seed_row_cap_defaults_when_config_omits_the_key() {
        assert_eq!(
            resolve_seed_row_cap(&cli_with_seed_row_cap(None)),
            DEFAULT_SEED_ROW_CAP
        );
    }

    #[test]
    fn resolve_seed_row_cap_reads_the_config_key() {
        assert_eq!(resolve_seed_row_cap(&cli_with_seed_row_cap(Some(42))), 42);
    }

    #[test]
    fn resolve_seed_row_cap_accepts_zero() {
        assert_eq!(resolve_seed_row_cap(&cli_with_seed_row_cap(Some(0))), 0);
    }

    // -----------------------------------------------------------------
    // build_check_policy (cute-dbt#171) — config selection + pragma
    // scanning over the in-scope models' raw_code.
    // -----------------------------------------------------------------

    fn model_with_raw(id: &str, raw_code: Option<&str>) -> crate::domain::Node {
        crate::domain::Node::new(
            NodeId::new(id),
            "model",
            crate::domain::Checksum::new("sha256", "x"),
            Some("select 1".to_owned()),
            raw_code.map(str::to_owned),
            DependsOn::default(),
            None,
            crate::domain::NodeConfig::default(),
            None,
            std::collections::BTreeMap::new(),
        )
    }

    fn manifest_of_models(nodes: Vec<crate::domain::Node>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            StdHashMap::new(),
            StdHashMap::new(),
        )
    }

    fn scope_of(ids: &[&str]) -> ModelInScopeSet {
        ids.iter().map(|s| NodeId::new(*s)).collect()
    }

    fn cli_with_checks(checks: crate::domain::ChecksConfig) -> ReportArgs {
        let mut cli = cli("report.html");
        cli.config = Some(crate::domain::AnalysisConfig {
            report: crate::domain::ReportConfig::default(),
            checks,
            experimental: crate::domain::ExperimentalConfig::default(),
            pr: crate::domain::PrConfig::default(),
            seeds: crate::domain::SeedsConfig::default(),
        });
        cli
    }

    #[test]
    fn build_check_policy_without_config_or_pragmas_is_the_default_minus_enforcement() {
        // cute-dbt#260 Slice 3 — governance OFF (the default): the
        // `enforcement` group is filtered out of the displayed set, so the
        // policy is the default minus the enforcement check(s). Everything
        // else (grain/union/join/incremental) stays displayed; no
        // suppressions.
        use crate::domain::CheckId as _;
        let manifest = manifest_of_models(vec![model_with_raw("model.shop.orders", None)]);
        let policy = build_check_policy(
            &cli("report.html"),
            &manifest,
            &scope_of(&["model.shop.orders"]),
            &EnabledExperiments::default(),
        );
        let expected: Vec<HeuristicId> = CheckPolicy::default()
            .displayed
            .into_iter()
            .filter(|id: &HeuristicId| id.spec().group != "enforcement")
            .collect();
        assert_eq!(policy.displayed, expected);
        assert!(policy.suppressions.is_empty());
        assert!(
            !policy
                .displayed
                .contains(&HeuristicId::EnforcementConstraintUnbacked),
            "enforcement is gated off by default",
        );
    }

    /// Governance-enabled experiment set (so the enforcement group is
    /// not filtered out — for the registry-shape policy tests).
    fn governance_on() -> EnabledExperiments {
        EnabledExperiments::from_union(
            &std::collections::BTreeSet::from([Experiment::Governance]),
            &std::collections::BTreeSet::new(),
        )
    }

    #[test]
    fn build_check_policy_with_governance_keeps_the_enforcement_group() {
        // Governance ON ⇒ EVERY registered check displays, including the
        // experiment-gated `enforcement` group (which `CheckPolicy::default`
        // excludes). No config/pragmas ⇒ no suppressions.
        use crate::domain::CheckId as _;
        let manifest = manifest_of_models(vec![model_with_raw("model.shop.orders", None)]);
        let policy = build_check_policy(
            &cli("report.html"),
            &manifest,
            &scope_of(&["model.shop.orders"]),
            &governance_on(),
        );
        assert_eq!(policy.displayed, HeuristicId::ALL.to_vec());
        assert!(policy.suppressions.is_empty());
        assert!(
            policy
                .displayed
                .contains(&HeuristicId::EnforcementConstraintUnbacked),
        );
        // And the default policy (governance off) does NOT carry it.
        assert!(
            !CheckPolicy::<HeuristicId>::default()
                .displayed
                .contains(&HeuristicId::EnforcementConstraintUnbacked),
            "the experiment-gated check is off in the default policy",
        );
    }

    #[test]
    fn build_check_policy_resolves_the_config_selection() {
        use crate::domain::CheckId as _;
        // Registry-shape-robust: `grain.*` removes exactly the grain
        // group; every other registered check (e.g. union.arm-coverage,
        // cute-dbt#172) stays displayed. Governance ON so the enforcement
        // group is not also filtered (Slice 3 gating tested separately).
        let manifest = manifest_of_models(vec![model_with_raw("model.shop.orders", None)]);
        let policy = build_check_policy(
            &cli_with_checks(crate::domain::ChecksConfig {
                disable: Some(vec!["grain.*".to_owned()]),
                ..Default::default()
            }),
            &manifest,
            &scope_of(&["model.shop.orders"]),
            &governance_on(),
        );
        // Governance ON ⇒ start from the FULL registry (enforcement
        // included); `grain.*` removes only the grain group.
        let expected: Vec<HeuristicId> = HeuristicId::ALL
            .iter()
            .copied()
            .filter(|id: &HeuristicId| id.spec().group != "grain")
            .collect();
        assert_eq!(policy.displayed, expected, "grain.* removes only grain");
        assert!(
            !expected.is_empty(),
            "the registry carries non-grain checks (union.arm-coverage)"
        );
    }

    #[test]
    fn build_check_policy_collects_pragmas_from_in_scope_raw_code() {
        let manifest = manifest_of_models(vec![model_with_raw(
            "model.shop.orders",
            Some("-- cute-dbt: ignore(grain.unique-key-unbacked, \"backfill dupes\")\nselect 1"),
        )]);
        let policy = build_check_policy(
            &cli("report.html"),
            &manifest,
            &scope_of(&["model.shop.orders"]),
            &EnabledExperiments::default(),
        );
        assert_eq!(
            policy.suppressions,
            vec![SuppressRule {
                check: HeuristicId::GrainUniqueKeyUnbacked,
                model: "model.shop.orders".to_owned(),
                reason: Some("backfill dupes".to_owned()),
                source: SuppressionSource::Pragma,
            }],
        );
    }

    #[test]
    fn build_check_policy_skips_unknown_pragma_ids_and_out_of_scope_models() {
        use crate::domain::CheckId as _;
        let manifest = manifest_of_models(vec![
            model_with_raw(
                "model.shop.orders",
                Some("-- cute-dbt: ignore(join.nonexistent, \"typo\")\nselect 1"),
            ),
            // In the manifest but NOT in scope: its pragma must not fire.
            model_with_raw(
                "model.shop.customers",
                Some("-- cute-dbt: ignore(grain.unique-key-unbacked)\nselect 1"),
            ),
        ]);
        let policy = build_check_policy(
            &cli("report.html"),
            &manifest,
            &scope_of(&["model.shop.orders"]),
            &governance_on(),
        );
        assert!(
            policy.suppressions.is_empty(),
            "unknown id warns + stays inert; out-of-scope models are not scanned: {:?}",
            policy.suppressions
        );
        // Governance ON ⇒ the full registry displays (enforcement
        // included), unaffected by the inert unknown pragma.
        assert_eq!(policy.displayed, HeuristicId::ALL.to_vec());
    }

    // -----------------------------------------------------------------
    // resolve_enabled_experiments (cute-dbt#289) — TOML ∪ env union at
    // the run-loop seam. The pure resolution/parsing semantics live in
    // domain::experimental; these pin the cli threading.
    // -----------------------------------------------------------------

    fn cli_with_experimental(enable: &[&str]) -> ReportArgs {
        let mut cli = cli("report.html");
        cli.config = Some(crate::domain::AnalysisConfig {
            report: crate::domain::ReportConfig::default(),
            checks: crate::domain::ChecksConfig::default(),
            experimental: crate::domain::ExperimentalConfig {
                enable: enable.iter().map(|s| (*s).to_owned()).collect(),
                macro_body_cap: None,
            },
            pr: crate::domain::PrConfig::default(),
            seeds: crate::domain::SeedsConfig::default(),
        });
        cli
    }

    #[test]
    fn resolve_enabled_experiments_without_either_surface_is_empty() {
        let resolved = resolve_enabled_experiments(&cli("report.html"));
        assert_eq!(resolved, EnabledExperiments::default());
        assert!(!resolved.is_enabled(crate::domain::Experiment::ProjectState));
    }

    #[test]
    fn resolve_enabled_experiments_reads_the_toml_arm() {
        let resolved = resolve_enabled_experiments(&cli_with_experimental(&["project-state"]));
        assert!(resolved.is_enabled(crate::domain::Experiment::ProjectState));
    }

    #[test]
    fn resolve_enabled_experiments_reads_the_env_arm() {
        let mut args = cli("report.html");
        args.experimental = Some([crate::domain::Experiment::ProjectState].into());
        let resolved = resolve_enabled_experiments(&args);
        assert!(resolved.is_enabled(crate::domain::Experiment::ProjectState));
    }

    #[test]
    fn resolve_enabled_experiments_unions_both_arms() {
        // With a one-experiment vocabulary the union is exercised
        // exhaustively in domain::experimental; this pins that BOTH
        // arms feed the cli-level union (either alone suffices, and
        // together they dedup).
        let mut args = cli_with_experimental(&["project-state"]);
        args.experimental = Some([crate::domain::Experiment::ProjectState].into());
        let resolved = resolve_enabled_experiments(&args);
        assert_eq!(resolved.enabled.len(), 1);
        assert!(resolved.is_enabled(crate::domain::Experiment::ProjectState));
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        // The Hinnant days→civil port (cute-dbt#260 Slice 4). Days since
        // 1970-01-01: 0, 18262 (2020-01-01), 47117 (2099-01-01),
        // 20617 (2026-06-13).
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(18_262), (2020, 1, 1));
        assert_eq!(civil_from_days(47_117), (2099, 1, 1));
        assert_eq!(civil_from_days(20_617), (2026, 6, 13));
        // A pre-epoch day still resolves (proleptic), never panics.
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn today_dep_date_is_a_plausible_modern_date() {
        // Threaded from the I/O boundary; just pin it produces a sane
        // value (the comparison logic is tested deterministically in the
        // domain).
        let today = today_dep_date();
        assert!(today.year >= 2026 && today.year < 2200);
        assert!((1..=12).contains(&today.month));
        assert!((1..=31).contains(&today.day));
    }

    #[test]
    fn an_output_failure_message_names_the_out_path() {
        let failure = RunError::output(
            Path::new("/locked/report.html"),
            io::Error::new(io::ErrorKind::PermissionDenied, "permission denied"),
        );
        let msg = failure.message();
        assert!(msg.contains("/locked/report.html"), "names the path: {msg}");
        assert!(msg.contains("could not write"), "{msg}");
        assert!(
            msg.contains("permission denied"),
            "carries the cause: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // gather_authoring_yaml_with_reader — covers every branch of the
    // soft-failure pipeline (cute-dbt#69 / crap4rs scorecard for #70).
    // -----------------------------------------------------------------

    use std::collections::HashMap as StdHashMap;

    use crate::domain::{
        DependsOn, Manifest, ManifestMetadata, NodeId, UnitTest, UnitTestExpect, UnitTestGiven,
    };

    enum StubResult {
        Ok(String),
        Err(io::ErrorKind, &'static str),
    }

    struct StubReader {
        entries: StdHashMap<String, StubResult>,
    }

    impl ProjectFileReader for StubReader {
        fn read(&self, project_relative: &str) -> io::Result<String> {
            match self.entries.get(project_relative) {
                Some(StubResult::Ok(s)) => Ok(s.clone()),
                Some(StubResult::Err(kind, msg)) => Err(io::Error::new(*kind, *msg)),
                None => Err(io::Error::new(io::ErrorKind::NotFound, "stub: missing")),
            }
        }
    }

    fn unit_test_with_path(name: &str, original_file_path: Option<&str>) -> UnitTest {
        UnitTest::new(
            name.to_owned(),
            NodeId::new("model.shop.dim_users"),
            Vec::new(),
            UnitTestExpect::new(serde_json::Value::Null, None, None),
            None,
            DependsOn::default(),
            None,
            None,
            original_file_path.map(str::to_owned),
        )
    }

    fn manifest_with(test_id: &str, unit_test: UnitTest) -> Manifest {
        let mut unit_tests = StdHashMap::new();
        unit_tests.insert(test_id.to_owned(), unit_test);
        Manifest::new(
            ManifestMetadata::new("v12"),
            StdHashMap::new(),
            unit_tests,
            StdHashMap::new(),
        )
    }

    fn in_scope_of(ids: &[&str]) -> InScopeSet {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    // -----------------------------------------------------------------
    // gather_model_yaml_with_reader / model_yaml_without_root — the
    // cute-dbt#247 Model-YAML gather. Unlike the authoring-YAML gather
    // (silent skips), EVERY in-scope model gets an outcome so the
    // rendered section can degrade truthfully instead of vanishing.
    // -----------------------------------------------------------------

    use std::collections::BTreeMap as StdBTreeMap;

    use crate::domain::{Checksum, ModelInScopeSet, ModelYamlOutcome, Node, NodeConfig};

    fn model_node_with_patch(id: &str, patch_path: Option<&str>) -> Node {
        Node::new(
            NodeId::new(id),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            StdBTreeMap::new(),
        )
        .with_patch_path(patch_path.map(str::to_owned))
    }

    fn manifest_with_models(nodes: Vec<Node>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            StdHashMap::new(),
            StdHashMap::new(),
        )
    }

    fn models_in_scope_of(ids: &[&str]) -> ModelInScopeSet {
        ids.iter().map(|id| NodeId::new(*id)).collect()
    }

    #[test]
    fn gather_model_yaml_slices_the_models_entry_when_reader_resolves() {
        let model_id = "model.shop.dim_users";
        let manifest = manifest_with_models(vec![model_node_with_patch(
            model_id,
            Some("models/schema.yml"),
        )]);
        let mut entries = StdHashMap::new();
        entries.insert(
            "models/schema.yml".to_owned(),
            StubResult::Ok("models:\n  - name: dim_users\n    description: a model\n".to_owned()),
        );
        let reader = StubReader { entries };

        let result =
            gather_model_yaml_with_reader(&reader, &manifest, &models_in_scope_of(&[model_id]));

        assert_eq!(result.len(), 1);
        match result.get(model_id).expect("outcome stored under model id") {
            ModelYamlOutcome::Found { path, block, diff } => {
                assert_eq!(path, "models/schema.yml");
                assert!(block.raw.contains("- name: dim_users"));
                assert!(block.raw.contains("description: a model"));
                assert!(diff.is_none(), "the gather never attaches a diff");
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn gather_model_yaml_resolves_a_versioned_model_by_ingested_name() {
        // cute-dbt#256 (the #254 handoff): a versioned model's id leaf
        // is the `.vN` version suffix — the schema file's `models:`
        // entry is keyed by the AUTHORED name. The ingested wire `name`
        // (Node::bare_name) is the lookup key; pre-#256 the slicer
        // searched for `- name: v2` and degraded to EntryNotFound.
        let model_id = "model.shop.dim_users.v2";
        let manifest = manifest_with_models(vec![
            model_node_with_patch(model_id, Some("models/schema.yml"))
                .with_identity(Some("dim_users".to_owned()), Some("shop".to_owned())),
        ]);
        let mut entries = StdHashMap::new();
        entries.insert(
            "models/schema.yml".to_owned(),
            StubResult::Ok("models:\n  - name: dim_users\n    description: a model\n".to_owned()),
        );
        let reader = StubReader { entries };

        let result =
            gather_model_yaml_with_reader(&reader, &manifest, &models_in_scope_of(&[model_id]));

        match result.get(model_id).expect("outcome stored under model id") {
            ModelYamlOutcome::Found { block, .. } => {
                assert!(block.raw.contains("- name: dim_users"));
            }
            other => panic!("expected Found via the ingested name, got {other:?}"),
        }
    }

    #[test]
    fn gather_model_yaml_marks_a_model_without_patch_path() {
        let model_id = "model.shop.dim_users";
        let manifest = manifest_with_models(vec![model_node_with_patch(model_id, None)]);
        let reader = StubReader {
            entries: StdHashMap::new(),
        };

        let result =
            gather_model_yaml_with_reader(&reader, &manifest, &models_in_scope_of(&[model_id]));

        assert_eq!(result.get(model_id), Some(&ModelYamlOutcome::NoPatchPath));
    }

    #[test]
    fn gather_model_yaml_marks_a_missing_schema_file() {
        let model_id = "model.shop.dim_users";
        let manifest = manifest_with_models(vec![model_node_with_patch(
            model_id,
            Some("models/missing.yml"),
        )]);
        // Empty stub returns NotFound by default.
        let reader = StubReader {
            entries: StdHashMap::new(),
        };

        let result =
            gather_model_yaml_with_reader(&reader, &manifest, &models_in_scope_of(&[model_id]));

        assert_eq!(
            result.get(model_id),
            Some(&ModelYamlOutcome::FileMissing {
                path: "models/missing.yml".to_owned(),
            }),
        );
    }

    #[test]
    fn gather_model_yaml_marks_a_path_guard_rejection_as_file_missing() {
        // A package model's patch file outside the project root: the
        // FsProjectFileReader path-safety guard maps it to InvalidInput.
        // Honest degrade = "not found under the project root".
        let model_id = "model.shop.pkg_model";
        let manifest = manifest_with_models(vec![model_node_with_patch(
            model_id,
            Some("../dbt_packages/pkg/models/schema.yml"),
        )]);
        let mut entries = StdHashMap::new();
        entries.insert(
            "../dbt_packages/pkg/models/schema.yml".to_owned(),
            StubResult::Err(io::ErrorKind::InvalidInput, "stub: escapes project root"),
        );
        let reader = StubReader { entries };

        let result =
            gather_model_yaml_with_reader(&reader, &manifest, &models_in_scope_of(&[model_id]));

        assert_eq!(
            result.get(model_id),
            Some(&ModelYamlOutcome::FileMissing {
                path: "../dbt_packages/pkg/models/schema.yml".to_owned(),
            }),
        );
    }

    #[test]
    fn gather_model_yaml_marks_an_unreadable_schema_file() {
        let model_id = "model.shop.dim_users";
        let manifest = manifest_with_models(vec![model_node_with_patch(
            model_id,
            Some("models/locked.yml"),
        )]);
        let mut entries = StdHashMap::new();
        entries.insert(
            "models/locked.yml".to_owned(),
            StubResult::Err(io::ErrorKind::PermissionDenied, "stub: permission denied"),
        );
        let reader = StubReader { entries };

        // The stage warns to stderr but does NOT propagate the error.
        let result =
            gather_model_yaml_with_reader(&reader, &manifest, &models_in_scope_of(&[model_id]));

        assert_eq!(
            result.get(model_id),
            Some(&ModelYamlOutcome::Unreadable {
                path: "models/locked.yml".to_owned(),
            }),
        );
    }

    #[test]
    fn gather_model_yaml_marks_a_file_without_the_models_entry() {
        let model_id = "model.shop.dim_users";
        let manifest = manifest_with_models(vec![model_node_with_patch(
            model_id,
            Some("models/schema.yml"),
        )]);
        let mut entries = StdHashMap::new();
        // File exists but only declares a DIFFERENT model.
        entries.insert(
            "models/schema.yml".to_owned(),
            StubResult::Ok("models:\n  - name: dim_payers\n".to_owned()),
        );
        let reader = StubReader { entries };

        let result =
            gather_model_yaml_with_reader(&reader, &manifest, &models_in_scope_of(&[model_id]));

        assert_eq!(
            result.get(model_id),
            Some(&ModelYamlOutcome::EntryNotFound {
                path: "models/schema.yml".to_owned(),
            }),
        );
    }

    #[test]
    fn gather_model_yaml_skips_a_model_id_absent_from_the_manifest() {
        let manifest = manifest_with_models(Vec::new());
        let reader = StubReader {
            entries: StdHashMap::new(),
        };

        let result = gather_model_yaml_with_reader(
            &reader,
            &manifest,
            &models_in_scope_of(&["model.shop.ghost"]),
        );

        assert!(result.is_empty());
    }

    #[test]
    fn model_yaml_without_root_marks_every_in_scope_model() {
        // No resolvable project root: a patch-carrying model degrades to
        // NoProjectRoot (naming the unread file); a patch-less model is
        // NoPatchPath — the section never silently vanishes.
        let with_patch = "model.shop.dim_users";
        let without_patch = "model.shop.int_orphan";
        let manifest = manifest_with_models(vec![
            model_node_with_patch(with_patch, Some("models/schema.yml")),
            model_node_with_patch(without_patch, None),
        ]);

        let result =
            model_yaml_without_root(&manifest, &models_in_scope_of(&[with_patch, without_patch]));

        assert_eq!(
            result.get(with_patch),
            Some(&ModelYamlOutcome::NoProjectRoot {
                path: "models/schema.yml".to_owned(),
            }),
        );
        assert_eq!(
            result.get(without_patch),
            Some(&ModelYamlOutcome::NoPatchPath),
        );
    }

    #[test]
    fn gather_authoring_yaml_returns_block_when_reader_resolves() {
        let test_id = "unit_test.shop.dim_users.test_demo";
        let manifest = manifest_with(
            test_id,
            unit_test_with_path("test_demo", Some("models/_ut.yml")),
        );
        let mut entries = StdHashMap::new();
        entries.insert(
            "models/_ut.yml".to_owned(),
            StubResult::Ok("unit_tests:\n  - name: test_demo\n    model: dim_users\n".to_owned()),
        );
        let reader = StubReader { entries };

        let result =
            gather_authoring_yaml_with_reader(&reader, &manifest, &in_scope_of(&[test_id]));

        assert_eq!(result.len(), 1);
        let block = result.get(test_id).expect("block stored under test id");
        assert!(block.raw.contains("name: test_demo"));
    }

    #[test]
    fn gather_authoring_yaml_skips_test_with_no_original_file_path() {
        let test_id = "unit_test.shop.dim_users.test_demo";
        let manifest = manifest_with(test_id, unit_test_with_path("test_demo", None));
        let reader = StubReader {
            entries: StdHashMap::new(),
        };

        let result =
            gather_authoring_yaml_with_reader(&reader, &manifest, &in_scope_of(&[test_id]));

        assert!(result.is_empty());
    }

    #[test]
    fn gather_authoring_yaml_skips_test_when_reader_returns_not_found() {
        let test_id = "unit_test.shop.dim_users.test_demo";
        let manifest = manifest_with(
            test_id,
            unit_test_with_path("test_demo", Some("models/missing.yml")),
        );
        // Empty stub returns NotFound by default.
        let reader = StubReader {
            entries: StdHashMap::new(),
        };

        let result =
            gather_authoring_yaml_with_reader(&reader, &manifest, &in_scope_of(&[test_id]));

        assert!(result.is_empty());
    }

    #[test]
    fn gather_authoring_yaml_skips_test_when_reader_returns_other_error() {
        let test_id = "unit_test.shop.dim_users.test_demo";
        let manifest = manifest_with(
            test_id,
            unit_test_with_path("test_demo", Some("models/locked.yml")),
        );
        let mut entries = StdHashMap::new();
        entries.insert(
            "models/locked.yml".to_owned(),
            StubResult::Err(io::ErrorKind::PermissionDenied, "stub: permission denied"),
        );
        let reader = StubReader { entries };

        // The stage warns to stderr but does NOT propagate the error.
        let result =
            gather_authoring_yaml_with_reader(&reader, &manifest, &in_scope_of(&[test_id]));

        assert!(result.is_empty());
    }

    #[test]
    fn gather_authoring_yaml_skips_test_when_name_not_in_source() {
        let test_id = "unit_test.shop.dim_users.test_demo";
        let manifest = manifest_with(
            test_id,
            unit_test_with_path("test_demo", Some("models/_ut.yml")),
        );
        let mut entries = StdHashMap::new();
        // Source file exists and parses, but contains a different test.
        entries.insert(
            "models/_ut.yml".to_owned(),
            StubResult::Ok("unit_tests:\n  - name: some_other_test\n    model: foo\n".to_owned()),
        );
        let reader = StubReader { entries };

        let result =
            gather_authoring_yaml_with_reader(&reader, &manifest, &in_scope_of(&[test_id]));

        assert!(result.is_empty());
    }

    #[test]
    fn gather_authoring_yaml_skips_id_absent_from_manifest_unit_tests() {
        // In-scope id with no matching entry in manifest.unit_tests —
        // can happen during a transient diff-vs-manifest mismatch and
        // must be a silent skip, not a panic.
        let manifest = Manifest::new(
            ManifestMetadata::new("v12"),
            StdHashMap::new(),
            StdHashMap::new(),
            StdHashMap::new(),
        );
        let reader = StubReader {
            entries: StdHashMap::new(),
        };

        let result = gather_authoring_yaml_with_reader(
            &reader,
            &manifest,
            &in_scope_of(&["unit_test.shop.dim_users.ghost"]),
        );

        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------
    // gather_external_fixtures_with_reader (cute-dbt#126) — the external
    // fixture file reader over the ProjectFileReader port.
    // -----------------------------------------------------------------

    fn stub_reader(entries: &[(&str, StubResult)]) -> StubReader {
        let mut map = StdHashMap::new();
        for (k, v) in entries {
            let cloned = match v {
                StubResult::Ok(s) => StubResult::Ok(s.clone()),
                StubResult::Err(kind, msg) => StubResult::Err(*kind, msg),
            };
            map.insert((*k).to_owned(), cloned);
        }
        StubReader { entries: map }
    }

    /// An external given: `rows: null` + a `fixture` path (the confirmed
    /// fusion shape).
    fn external_given(input: &str, format: &str, fixture: &str) -> UnitTestGiven {
        UnitTestGiven::new(
            input,
            serde_json::Value::Null,
            Some(format.to_owned()),
            Some(fixture.to_owned()),
        )
    }

    fn ut_with(given: Vec<UnitTestGiven>, expect: UnitTestExpect) -> UnitTest {
        UnitTest::new(
            "t",
            NodeId::new("model.shop.m"),
            given,
            expect,
            None,
            DependsOn::default(),
            None,
            None,
            Some("models/_ut.yml".to_owned()),
        )
    }

    fn null_expect() -> UnitTestExpect {
        UnitTestExpect::new(serde_json::Value::Null, None, None)
    }

    #[test]
    fn external_csv_given_is_loaded_with_grid() {
        let manifest = manifest_with(
            "ut.m.t",
            ut_with(
                vec![external_given("ref('a')", "csv", "tests/fixtures/a.csv")],
                null_expect(),
            ),
        );
        let reader = stub_reader(&[(
            "tests/fixtures/a.csv",
            StubResult::Ok("id,amount\n1,10\n".to_owned()),
        )]);
        let out =
            gather_external_fixtures_with_reader(&reader, &manifest, &in_scope_of(&["ut.m.t"]));
        let ext = out.get("ut.m.t").expect("test has loaded externals");
        let loaded = ext.given.get(&0).expect("given 0 loaded");
        assert_eq!(loaded.text, "id,amount\n1,10\n");
        assert_eq!(loaded.format.as_deref(), Some("csv"));
        let table = loaded.table.as_ref().expect("csv tabulates to a grid");
        assert_eq!(table.rows.len(), 1);
    }

    #[test]
    fn inline_given_is_not_loaded() {
        // fixture: None → not external → reader never consulted.
        let given = UnitTestGiven::new(
            "ref('a')",
            serde_json::json!([{"id": 1}]),
            Some("dict".to_owned()),
            None,
        );
        let manifest = manifest_with("ut.m.t", ut_with(vec![given], null_expect()));
        let reader = stub_reader(&[]);
        let out =
            gather_external_fixtures_with_reader(&reader, &manifest, &in_scope_of(&["ut.m.t"]));
        assert!(out.is_empty(), "inline given produces no external entry");
    }

    #[test]
    fn fixture_with_inline_rows_is_not_read() {
        // A `fixture` present ALONGSIDE inline rows (a shape dbt-core MAY emit
        // by inlining the fixture) renders inline — the reader is NOT invoked,
        // so even a poisoned reader entry is never hit.
        let given = UnitTestGiven::new(
            "ref('a')",
            serde_json::json!([{"id": 1}]),
            Some("csv".to_owned()),
            Some("tests/fixtures/a.csv".to_owned()),
        );
        let manifest = manifest_with("ut.m.t", ut_with(vec![given], null_expect()));
        let reader = stub_reader(&[(
            "tests/fixtures/a.csv",
            StubResult::Err(io::ErrorKind::Other, "reader must not be called"),
        )]);
        let out =
            gather_external_fixtures_with_reader(&reader, &manifest, &in_scope_of(&["ut.m.t"]));
        assert!(out.is_empty(), "fixture + inline rows stays inline");
    }

    #[test]
    fn unreadable_external_fixture_is_skipped() {
        // NotFound → silent skip → the test has no external entry → the
        // template falls back to the #98 affordance.
        let manifest = manifest_with(
            "ut.m.t",
            ut_with(
                vec![external_given(
                    "ref('a')",
                    "csv",
                    "tests/fixtures/missing.csv",
                )],
                null_expect(),
            ),
        );
        let reader = stub_reader(&[]);
        let out =
            gather_external_fixtures_with_reader(&reader, &manifest, &in_scope_of(&["ut.m.t"]));
        assert!(out.is_empty());
    }

    #[test]
    fn bare_name_fixture_falls_back_to_tests_fixtures() {
        // AC#4 cross-engine guard: a bare name not found as-is retries the
        // dbt convention tests/fixtures/<name>.csv.
        let manifest = manifest_with(
            "ut.m.t",
            ut_with(
                vec![external_given("ref('a')", "csv", "stg_a")],
                null_expect(),
            ),
        );
        let reader = stub_reader(&[(
            "tests/fixtures/stg_a.csv",
            StubResult::Ok("id\n1\n".to_owned()),
        )]);
        let out =
            gather_external_fixtures_with_reader(&reader, &manifest, &in_scope_of(&["ut.m.t"]));
        let ext = out.get("ut.m.t").expect("bare-name fallback loaded");
        assert!(ext.given.get(&0).expect("given 0").table.is_some());
    }

    #[test]
    fn expect_side_external_fixture_is_loaded() {
        let expect = UnitTestExpect::new(
            serde_json::Value::Null,
            Some("csv".to_owned()),
            Some("tests/fixtures/exp.csv".to_owned()),
        );
        let manifest = manifest_with("ut.m.t", ut_with(vec![], expect));
        let reader = stub_reader(&[(
            "tests/fixtures/exp.csv",
            StubResult::Ok("id\n9\n".to_owned()),
        )]);
        let out =
            gather_external_fixtures_with_reader(&reader, &manifest, &in_scope_of(&["ut.m.t"]));
        let ext = out.get("ut.m.t").expect("expect external loaded");
        assert!(ext.given.is_empty());
        assert!(ext.expect.is_some(), "expect-side external fixture loaded");
    }

    #[test]
    fn two_givens_sharing_a_fixture_both_load_by_ordinal() {
        // Two givens against the SAME fixture path each load under their own
        // ordinal (the #131 per-given identity).
        let manifest = manifest_with(
            "ut.m.t",
            ut_with(
                vec![
                    external_given("ref('a')", "csv", "tests/fixtures/shared.csv"),
                    external_given("ref('a')", "csv", "tests/fixtures/shared.csv"),
                ],
                null_expect(),
            ),
        );
        let reader = stub_reader(&[(
            "tests/fixtures/shared.csv",
            StubResult::Ok("id\n1\n".to_owned()),
        )]);
        let out =
            gather_external_fixtures_with_reader(&reader, &manifest, &in_scope_of(&["ut.m.t"]));
        let ext = out.get("ut.m.t").expect("loaded");
        assert!(ext.given.contains_key(&0));
        assert!(ext.given.contains_key(&1));
    }

    #[test]
    fn external_sql_nonliteral_loads_text_without_a_grid() {
        // A non-literal sql fixture file loads its TEXT (for the code-block
        // fallback) but tabulates to no grid (table None → AC#5).
        let manifest = manifest_with(
            "ut.m.t",
            ut_with(
                vec![external_given("ref('a')", "sql", "tests/fixtures/a.sql")],
                null_expect(),
            ),
        );
        let reader = stub_reader(&[(
            "tests/fixtures/a.sql",
            StubResult::Ok("select id, name from src".to_owned()),
        )]);
        let out =
            gather_external_fixtures_with_reader(&reader, &manifest, &in_scope_of(&["ut.m.t"]));
        let loaded = out
            .get("ut.m.t")
            .and_then(|e| e.given.get(&0))
            .expect("sql fixture loaded");
        assert!(loaded.table.is_none(), "non-literal sql → no grid");
        assert_eq!(loaded.text, "select id, name from src");
    }

    #[test]
    fn absent_manifest_format_derives_from_extension() {
        // dbt-core MAY omit `format` — the .csv extension fills it so the
        // data still tabulates (cross-engine guard).
        let given = UnitTestGiven::new(
            "ref('a')",
            serde_json::Value::Null,
            None, // no manifest format
            Some("tests/fixtures/a.csv".to_owned()),
        );
        let manifest = manifest_with("ut.m.t", ut_with(vec![given], null_expect()));
        let reader = stub_reader(&[(
            "tests/fixtures/a.csv",
            StubResult::Ok("id,amount\n1,10\n".to_owned()),
        )]);
        let out =
            gather_external_fixtures_with_reader(&reader, &manifest, &in_scope_of(&["ut.m.t"]));
        let loaded = out
            .get("ut.m.t")
            .and_then(|e| e.given.get(&0))
            .expect("loaded");
        assert_eq!(loaded.format.as_deref(), Some("csv"));
        assert!(loaded.table.is_some(), "extension-derived csv tabulates");
    }

    #[test]
    fn path_traversal_fixture_is_skipped_softly() {
        // The adapter rejects a `..`/absolute path with InvalidInput; the
        // gather treats it as a soft skip (no arbitrary read, no crash) — the
        // test simply has no external entry.
        let manifest = manifest_with(
            "ut.m.t",
            ut_with(
                vec![external_given("ref('a')", "csv", "../../../etc/passwd")],
                null_expect(),
            ),
        );
        let reader = stub_reader(&[(
            "../../../etc/passwd",
            StubResult::Err(io::ErrorKind::InvalidInput, "rejected by path guard"),
        )]);
        let out =
            gather_external_fixtures_with_reader(&reader, &manifest, &in_scope_of(&["ut.m.t"]));
        assert!(out.is_empty(), "rejected path is a soft skip");
    }

    // -----------------------------------------------------------------
    // merge_external_data_diffs (cute-dbt#126 AC#3) — splice the external
    // fixture FILE cell diff into data_diffs, keyed off the file's own hunks.
    // -----------------------------------------------------------------

    fn loaded_csv(text: &str) -> LoadedFixture {
        LoadedFixture {
            text: text.to_owned(),
            format: Some("csv".to_owned()),
            table: external_fixture_table(text, Some("csv")),
        }
    }

    fn index_for(path: &str, removed: &str, added: &str, line: usize) -> NormalizedDiffIndex {
        use crate::domain::{FileHunks, Hunk, PrDiff};
        let diff = PrDiff {
            renames: Vec::new(),
            deleted: Vec::new(),
            added: Vec::new(),
            files: vec![FileHunks {
                path: path.to_owned(),
                hunks: vec![Hunk {
                    new_start: line,
                    new_len: 1,
                    removed_lines: vec![removed.to_owned()],
                    added_lines: vec![added.to_owned()],
                }],
            }],
        };
        NormalizedDiffIndex::new(&diff, None)
    }

    #[test]
    fn merge_external_data_diffs_splices_a_csv_cell_diff_by_ordinal() {
        use crate::domain::RowChangeKind;
        let manifest = manifest_with(
            "ut.m.t",
            ut_with(
                vec![external_given("ref('a')", "csv", "tests/fixtures/a.csv")],
                null_expect(),
            ),
        );
        let mut externals = StdHashMap::new();
        let mut ext = ExternalFixtures::default();
        ext.given.insert(0, loaded_csv("id,amount\n1,10\n2,99\n"));
        externals.insert("ut.m.t".to_owned(), ext);
        let index = index_for("tests/fixtures/a.csv", "2,20", "2,99", 3);

        let mut data_diffs: StdHashMap<String, UnitTestDataDiff> = StdHashMap::new();
        merge_external_data_diffs(
            &mut data_diffs,
            &manifest,
            &in_scope_of(&["ut.m.t"]),
            &externals,
            &index,
        );

        let dd = data_diffs.get("ut.m.t").expect("external diff spliced in");
        assert_eq!(dd.given.len(), 1);
        assert_eq!(dd.given[0].ordinal, 0, "keyed by source ordinal");
        assert!(
            dd.given[0]
                .diff
                .rows
                .iter()
                .any(|r| r.kind == RowChangeKind::Modified),
            "the touched csv cell is a Modified row",
        );
    }

    #[test]
    fn merge_external_data_diffs_skips_an_untouched_fixture() {
        // The fixture loaded, but the PR diff did not touch THAT file (the
        // index has hunks for a different path) → no cell diff entry (the grid
        // renders without a diff toggle). This is the independence property:
        // AC#3 keys off the fixture file's own hunks.
        let manifest = manifest_with(
            "ut.m.t",
            ut_with(
                vec![external_given("ref('a')", "csv", "tests/fixtures/a.csv")],
                null_expect(),
            ),
        );
        let mut externals = StdHashMap::new();
        let mut ext = ExternalFixtures::default();
        ext.given.insert(0, loaded_csv("id,amount\n1,10\n"));
        externals.insert("ut.m.t".to_owned(), ext);
        // Hunks for a DIFFERENT file — the fixture itself is untouched.
        let index = index_for("models/other.sql", "x", "y", 1);

        let mut data_diffs: StdHashMap<String, UnitTestDataDiff> = StdHashMap::new();
        merge_external_data_diffs(
            &mut data_diffs,
            &manifest,
            &in_scope_of(&["ut.m.t"]),
            &externals,
            &index,
        );
        assert!(
            data_diffs.is_empty(),
            "an untouched external fixture produces no cell diff",
        );
    }

    // -----------------------------------------------------------------
    // gather_project_facts_with_reader — the cute-dbt#266 project-
    // definition stage: standing metadata (parse always) + the
    // diff-gated categorized panel with its fail-open fallback arms.
    // -----------------------------------------------------------------

    use crate::domain::{
        FileHunks, Hunk, PrDiff, ProjectChangeCategory, ProjectChangePanel, ProjectFallbackReason,
    };

    /// A reader carrying one `dbt_project.yml` body.
    fn project_reader(text: &str) -> StubReader {
        let mut entries = StdHashMap::new();
        entries.insert(
            "dbt_project.yml".to_owned(),
            StubResult::Ok(text.to_owned()),
        );
        StubReader { entries }
    }

    /// An empty current manifest — the gather stage's manifest input for
    /// the scenarios that exercise no attribution (cute-dbt#267 added
    /// the parameter; with no model nodes nothing can attribute).
    fn empty_current() -> Manifest {
        manifest_with_models(Vec::new())
    }

    /// An index whose only changed file is `dbt_project.yml` with the
    /// given hunks.
    fn project_diff_index(hunks: Vec<Hunk>) -> NormalizedDiffIndex {
        let diff = PrDiff {
            renames: Vec::new(),
            deleted: Vec::new(),
            added: Vec::new(),
            files: vec![FileHunks {
                path: "dbt_project.yml".to_owned(),
                hunks,
            }],
        };
        NormalizedDiffIndex::new(&diff, None)
    }

    fn replacement_hunk(new_start: usize, removed: &str, added: &str) -> Hunk {
        Hunk {
            new_start,
            new_len: 1,
            removed_lines: vec![removed.to_owned()],
            added_lines: vec![added.to_owned()],
        }
    }

    const PROJECT_NEW: &str = "name: playground\nvars:\n  default_state: VT\n";

    #[test]
    fn project_facts_parse_standing_metadata_with_no_diff() {
        // Baseline arm (no index): the file still parses — the founder's
        // parse-always posture; the panel stays absent.
        let facts =
            gather_project_facts_with_reader(&project_reader(PROJECT_NEW), &empty_current(), None);
        let def = facts.definition.expect("standing metadata parsed");
        assert_eq!(def.name.as_deref(), Some("playground"));
        assert!(facts.panel.is_none(), "no diff, no panel");
    }

    // ----- explore standing project facts (cute-dbt#270) -----

    #[test]
    fn explore_project_facts_parse_standing_definition_only() {
        // The explore gather reads only the definition — no panel, no diff.
        let facts = explore_project_facts_with_reader(&project_reader(PROJECT_NEW));
        let def = facts.definition.expect("standing metadata parsed");
        assert_eq!(def.name.as_deref(), Some("playground"));
        assert!(facts.panel.is_none(), "explore renders no project panel");
        assert!(facts.config_attributions.is_empty());
        assert!(facts.var_references.is_empty());
    }

    #[test]
    fn explore_project_facts_degrade_to_default_when_file_absent() {
        // A NotFound read ⇒ no pane (ProjectFacts::default), fail-open.
        let reader = StubReader {
            entries: StdHashMap::new(),
        };
        let facts = explore_project_facts_with_reader(&reader);
        assert!(facts.definition.is_none());
    }

    #[test]
    fn explore_project_facts_degrade_to_default_on_unparseable_yaml() {
        // A present-but-unparseable file ⇒ no pane (the warning path).
        let mut entries = StdHashMap::new();
        entries.insert(
            "dbt_project.yml".to_owned(),
            StubResult::Ok("name: [unterminated\n".to_owned()),
        );
        let facts = explore_project_facts_with_reader(&StubReader { entries });
        assert!(
            facts.definition.is_none(),
            "an unparseable project file degrades to no pane, never a failed run",
        );
    }

    #[test]
    fn project_facts_panel_absent_when_file_not_in_diff() {
        let diff = PrDiff {
            renames: Vec::new(),
            deleted: Vec::new(),
            added: Vec::new(),
            files: vec![FileHunks {
                path: "models/dim_users.sql".to_owned(),
                hunks: vec![replacement_hunk(1, "a", "b")],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        let facts = gather_project_facts_with_reader(
            &project_reader(PROJECT_NEW),
            &empty_current(),
            Some(&index),
        );
        assert!(facts.definition.is_some(), "standing metadata still rides");
        assert!(
            facts.panel.is_none(),
            "dbt_project.yml untouched ⇒ no panel"
        );
    }

    #[test]
    fn project_facts_categorize_an_in_diff_vars_edit() {
        // The diff replaced `default_state: CT` with `default_state: VT`
        // (line 3 of the new text). Reverse-apply reconstructs the old
        // side; the structural diff categorizes the var change.
        let index = project_diff_index(vec![replacement_hunk(
            3,
            "  default_state: CT",
            "  default_state: VT",
        )]);
        let facts = gather_project_facts_with_reader(
            &project_reader(PROJECT_NEW),
            &empty_current(),
            Some(&index),
        );
        match facts.panel.expect("panel present") {
            ProjectChangePanel::Categorized { changes } => {
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0].category, ProjectChangeCategory::Vars);
                assert_eq!(changes[0].label, "default_state");
                assert_eq!(changes[0].old, Some(serde_json::json!("CT")));
                assert_eq!(changes[0].new, Some(serde_json::json!("VT")));
            }
            ProjectChangePanel::Fallback { reason, .. } => {
                panic!("expected categorized panel, got fallback: {reason:?}")
            }
        }
    }

    #[test]
    fn project_facts_vars_edit_attaches_tiered_facts_and_reference_chips() {
        // cute-dbt#268: the categorized vars row carries the tiered
        // attribution and ProjectFacts carries the per-model reference
        // chips — context only (scope selection reads neither).
        let reader_id = NodeId::new("model.shop.reads_state");
        let reader_node = Node::new(
            reader_id.clone(),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            Some("select '{{ var('default_state') }}' as state".to_owned()),
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            StdBTreeMap::new(),
        );
        let current = manifest_with_models(vec![reader_node]);
        let index = project_diff_index(vec![replacement_hunk(
            3,
            "  default_state: CT",
            "  default_state: VT",
        )]);
        let facts =
            gather_project_facts_with_reader(&project_reader(PROJECT_NEW), &current, Some(&index));
        let ProjectChangePanel::Categorized { changes } = facts.panel.expect("panel present")
        else {
            panic!("expected categorized panel");
        };
        let var_facts = changes[0].vars.as_ref().expect("vars facts attached");
        assert_eq!(var_facts.entries.len(), 1);
        assert_eq!(var_facts.entries[0].name, "default_state");
        assert_eq!(
            var_facts.entries[0].direct,
            vec![reader_id.as_str().to_owned()],
        );
        assert_eq!(var_facts.footprint.models_scanned, 1);
        let chips = &facts.var_references[reader_id.as_str()];
        assert_eq!(chips.len(), 1);
        assert_eq!(chips[0].name, "default_state");
        assert_eq!(chips[0].tier, crate::domain::VarTier::Direct);
    }

    #[test]
    fn project_facts_drift_degrades_to_the_not_reconstructable_fallback() {
        // The hunk's + body does not match the working tree (stale diff)
        // — never a silently wrong old side.
        let index = project_diff_index(vec![replacement_hunk(
            3,
            "  default_state: CT",
            "  default_state: SOMETHING-ELSE",
        )]);
        let facts = gather_project_facts_with_reader(
            &project_reader(PROJECT_NEW),
            &empty_current(),
            Some(&index),
        );
        match facts.panel.expect("panel present") {
            ProjectChangePanel::Fallback { reason, raw } => {
                assert_eq!(reason, ProjectFallbackReason::OldNotReconstructable);
                assert!(!raw.is_empty(), "the raw hunk lines still show");
            }
            ProjectChangePanel::Categorized { .. } => {
                panic!("a drifting hunk must not categorize")
            }
        }
    }

    #[test]
    fn project_facts_unparseable_new_side_degrades_to_new_parse_failed() {
        let broken = "models:\n  - [unclosed\n";
        let index = project_diff_index(vec![replacement_hunk(2, "  ok: 1", "  - [unclosed")]);
        let facts = gather_project_facts_with_reader(
            &project_reader(broken),
            &empty_current(),
            Some(&index),
        );
        assert!(facts.definition.is_none(), "no standing metadata");
        match facts.panel.expect("panel present") {
            ProjectChangePanel::Fallback { reason, .. } => {
                assert_eq!(reason, ProjectFallbackReason::NewParseFailed);
            }
            ProjectChangePanel::Categorized { .. } => {
                panic!("an unparseable working tree must not categorize")
            }
        }
    }

    #[test]
    fn project_facts_missing_file_degrades_to_the_absence_note() {
        // dbt_project.yml is in the diff but the working tree (under the
        // resolved project root) has no such file.
        let reader = StubReader {
            entries: StdHashMap::new(),
        };
        let index = project_diff_index(vec![replacement_hunk(1, "name: a", "name: b")]);
        let facts = gather_project_facts_with_reader(&reader, &empty_current(), Some(&index));
        assert!(facts.definition.is_none());
        match facts
            .panel
            .expect("panel present — never silently invisible")
        {
            ProjectChangePanel::Fallback { reason, raw } => {
                assert_eq!(reason, ProjectFallbackReason::FileUnreadable);
                assert!(!raw.is_empty(), "the hunks are still in hand");
            }
            ProjectChangePanel::Categorized { .. } => {
                panic!("an unreadable file must not categorize")
            }
        }
    }

    #[test]
    fn project_facts_file_created_in_the_pr_reports_all_added() {
        // One hunk covering the whole (new) file, no removed lines —
        // reverse-apply yields empty old text, which parses to the
        // default definition, so every entry reports as added.
        let lines: Vec<String> = PROJECT_NEW.lines().map(str::to_owned).collect();
        let hunk = Hunk {
            new_start: 1,
            new_len: lines.len(),
            removed_lines: Vec::new(),
            added_lines: lines,
        };
        let index = project_diff_index(vec![hunk]);
        let facts = gather_project_facts_with_reader(
            &project_reader(PROJECT_NEW),
            &empty_current(),
            Some(&index),
        );
        match facts.panel.expect("panel present") {
            ProjectChangePanel::Categorized { changes } => {
                assert!(!changes.is_empty());
                assert!(
                    changes.iter().all(|c| c.old.is_none()),
                    "a created file reports every entry as added",
                );
            }
            ProjectChangePanel::Fallback { reason, .. } => {
                panic!("a clean creation diff categorizes, got {reason:?}")
            }
        }
    }

    #[test]
    fn project_facts_formatting_only_edit_categorizes_to_zero_changes() {
        // A comment edit changes no semantic configuration: categorized,
        // empty — the panel renders the truthful "formatting only" note.
        let new_text = "# a comment\nname: playground\n";
        let index =
            project_diff_index(vec![replacement_hunk(1, "# an old comment", "# a comment")]);
        let facts = gather_project_facts_with_reader(
            &project_reader(new_text),
            &empty_current(),
            Some(&index),
        );
        match facts.panel.expect("panel present") {
            ProjectChangePanel::Categorized { changes } => {
                assert!(changes.is_empty(), "no semantic change to report");
            }
            ProjectChangePanel::Fallback { reason, .. } => {
                panic!("expected categorized-empty, got {reason:?}")
            }
        }
    }

    #[test]
    fn project_facts_hook_edit_enriches_the_hooks_row_from_operation_nodes() {
        // cute-dbt#269: a hooks edit + a manifest carrying the matching
        // operation node (declaring path `./dbt_project.yml` VERBATIM,
        // the fusion shape) ⇒ the hooks row carries HookChangeFacts:
        // Matched presence, the operation id, and the inline SQL diff
        // whose new side came from the manifest node.
        let new_text = "name: playground\non-run-start:\n  - \"grant select on schema x\"\n";
        let index = project_diff_index(vec![replacement_hunk(
            3,
            "  - \"grant usage on schema x\"",
            "  - \"grant select on schema x\"",
        )]);
        let op_id = crate::domain::NodeId::new("operation.playground.playground-on-run-start-0");
        let op = crate::domain::Node::new(
            op_id.clone(),
            "operation",
            crate::domain::Checksum::new("sha256", "feed"),
            None,
            Some("grant select on schema x".to_owned()),
            crate::domain::DependsOn::default(),
            Some("./dbt_project.yml".to_owned()),
            crate::domain::NodeConfig::default(),
            None,
            std::collections::BTreeMap::new(),
        )
        .with_identity(
            Some("playground-on-run-start-0".to_owned()),
            Some("playground".to_owned()),
        );
        let manifest = Manifest::new(
            ManifestMetadata::new("v12"),
            StdHashMap::from([(op_id, op)]),
            StdHashMap::new(),
            StdHashMap::new(),
        );
        let facts =
            gather_project_facts_with_reader(&project_reader(new_text), &manifest, Some(&index));
        let ProjectChangePanel::Categorized { changes } = facts.panel.expect("panel present")
        else {
            panic!("a clean hook edit categorizes");
        };
        let hooks_row = changes
            .iter()
            .find(|c| c.category == ProjectChangeCategory::Hooks)
            .expect("a hooks row");
        let hook = hooks_row.hook.as_ref().expect("hook facts attached");
        assert_eq!(hook.manifest, crate::domain::HookManifestPresence::Matched);
        assert_eq!(
            hook.operation_ids,
            vec!["operation.playground.playground-on-run-start-0"],
        );
        let diff = hook.sql_diff.as_ref().expect("inline SQL diff");
        assert!(
            diff.lines
                .iter()
                .any(|l| l.text == "grant select on schema x"),
        );
    }

    // -----------------------------------------------------------------
    // cute-dbt#267 — config-tree attribution wiring through the gather
    // stage: a categorized models-tree edit attributes the fqn-matched
    // models; every degrade arm attributes nothing.
    // -----------------------------------------------------------------

    /// The canonical project body whose marts subtree the #267 tests
    /// edit (line 4 carries the `+materialized` leaf).
    const PROJECT_TREE: &str = concat!(
        "name: shop\n",
        "models:\n",
        "  shop:\n",
        "    marts:\n",
        "      +materialized: table\n",
    );

    /// A manifest holding one marts model and one staging model, fqn
    /// populated (the cute-dbt#278 wire).
    fn fqn_manifest() -> Manifest {
        let with_fqn = |id: &str, fqn: &[&str]| {
            model_node_with_patch(id, None).with_fqn(fqn.iter().map(|s| (*s).to_owned()).collect())
        };
        manifest_with_models(vec![
            with_fqn("model.shop.fct_orders", &["shop", "marts", "fct_orders"]),
            with_fqn("model.shop.stg_raw", &["shop", "staging", "stg_raw"]),
        ])
    }

    #[test]
    fn project_facts_attribute_a_categorized_config_tree_edit_to_fqn_matched_models() {
        // Line 5 of PROJECT_TREE: `      +materialized: table` (the `+`
        // side must byte-match the working tree).
        let index = project_diff_index(vec![replacement_hunk(
            5,
            "      +materialized: view",
            "      +materialized: table",
        )]);
        let facts = gather_project_facts_with_reader(
            &project_reader(PROJECT_TREE),
            &fqn_manifest(),
            Some(&index),
        );
        assert!(matches!(
            facts.panel,
            Some(ProjectChangePanel::Categorized { .. })
        ));
        assert_eq!(
            facts.config_attributions.keys().collect::<Vec<_>>(),
            vec!["model.shop.fct_orders"],
            "only the marts model attributes",
        );
        let attribution = &facts.config_attributions["model.shop.fct_orders"][0];
        assert_eq!(attribution.key, "materialized");
        assert_eq!(attribution.path, "models.shop.marts");
    }

    #[test]
    fn project_facts_attribute_nothing_on_a_fallback_arm() {
        // The same fqn-bearing manifest, but the hunk drifts (stale diff)
        // — no parsed old/new pair, so no attribution may be guessed.
        let index = project_diff_index(vec![replacement_hunk(
            5,
            "      +materialized: view",
            "      +materialized: DRIFTED",
        )]);
        let facts = gather_project_facts_with_reader(
            &project_reader(PROJECT_TREE),
            &fqn_manifest(),
            Some(&index),
        );
        assert!(matches!(
            facts.panel,
            Some(ProjectChangePanel::Fallback { .. })
        ));
        assert!(
            facts.config_attributions.is_empty(),
            "a degrade arm never widens",
        );
    }

    // -----------------------------------------------------------------
    // gather_seeds / gather_seeds_with_reader (cute-dbt#350) — the seed
    // working-tree CSV read. Unlike `gather_external_fixtures` (silent
    // skip), this follows `gather_model_yaml`'s TRUTHFUL DEGRADE: every
    // in-scope seed yields a card, and a card the reader cannot fill keeps
    // `table: None` (a labeled empty-data state, NEVER a silent blank grid
    // — the cute-dbt#126 lesson). Headers come from the CSV header row, NOT
    // the manifest `columns` map (empty for an un-YAML'd seed).
    // -----------------------------------------------------------------

    fn seed_node(id: &str, original_file_path: Option<&str>) -> crate::domain::Node {
        crate::domain::Node::new(
            NodeId::new(id),
            "seed",
            crate::domain::Checksum::new("sha256", "ck"),
            None,
            None,
            DependsOn::default(),
            original_file_path.map(str::to_owned),
            crate::domain::NodeConfig::default(),
            None,
            StdBTreeMap::new(),
        )
    }

    /// A `model` node that `ref()`s the given seed id (its downstream
    /// consumer — the "feeds N models" edge).
    fn model_consuming_seed(id: &str, seed_id: &str) -> crate::domain::Node {
        crate::domain::Node::new(
            NodeId::new(id),
            "model",
            crate::domain::Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            None,
            DependsOn::new(Vec::new(), vec![NodeId::new(seed_id)]),
            None,
            crate::domain::NodeConfig::default(),
            None,
            StdBTreeMap::new(),
        )
    }

    fn seed_manifest(nodes: Vec<crate::domain::Node>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            StdHashMap::new(),
            StdHashMap::new(),
        )
    }

    fn seeds_in_scope_of(ids: &[&str]) -> SeedInScopeSet {
        ids.iter().map(|id| NodeId::new(*id)).collect()
    }

    #[test]
    fn gather_seeds_reads_the_working_tree_csv_into_a_table() {
        // Happy path: an in-scope seed with a readable CSV gets `table:
        // Some` whose columns come from the CSV HEADER ROW (the manifest
        // `columns` map is empty for this un-YAML'd seed).
        let seed_id = "seed.shop.raw_customers";
        let manifest = seed_manifest(vec![
            seed_node(seed_id, Some("seeds/raw_customers.csv")),
            model_consuming_seed("model.shop.stg_customers", seed_id),
        ]);
        let mut entries = StdHashMap::new();
        entries.insert(
            "seeds/raw_customers.csv".to_owned(),
            StubResult::Ok("id,first_name\n1,Ada\n2,Grace\n".to_owned()),
        );
        let reader = StubReader { entries };

        let cards = build_seed_cards(&manifest, &seeds_in_scope_of(&[seed_id]));
        let cards = gather_seeds_with_reader(&reader, cards, None);

        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!(card.id, NodeId::new(seed_id));
        assert_eq!(card.name, "raw_customers");
        // Lineage carried from the projection skeleton (direct consumer).
        assert_eq!(card.feeds_models, vec!["stg_customers"]);
        let table = card.table.as_ref().expect("CSV read fills the table");
        // Header row supplies the column names — NOT the (empty) manifest
        // `columns` map.
        assert_eq!(
            table.columns,
            vec!["id".to_owned(), "first_name".to_owned()],
        );
        assert_eq!(table.rows.len(), 2);
    }

    #[test]
    fn gather_seeds_reconstructs_the_cell_diff_on_the_pr_diff_arm() {
        // cute-dbt#350 — when an `index` is passed (the pr-diff arm), the seed
        // CSV's own hunks reconstruct the old→new cell-diff: working-tree text
        // is NEW, reverse-applying the hunk rebuilds OLD. The seed file is an
        // external tabular file, so this reuses the #126 reconstruction.
        use crate::domain::RowChangeKind;
        let seed_id = "seed.shop.raw_customers";
        let manifest = seed_manifest(vec![seed_node(seed_id, Some("seeds/raw_customers.csv"))]);
        let mut entries = StdHashMap::new();
        // NEW working-tree body (row 2 amount = 99).
        entries.insert(
            "seeds/raw_customers.csv".to_owned(),
            StubResult::Ok("id,amount\n1,10\n2,99\n".to_owned()),
        );
        let reader = StubReader { entries };
        // The hunk on the seed file: row 2 changed 20 -> 99 (line 3).
        let index = index_for("seeds/raw_customers.csv", "2,20", "2,99", 3);

        let cards = build_seed_cards(&manifest, &seeds_in_scope_of(&[seed_id]));
        let cards = gather_seeds_with_reader(&reader, cards, Some(&index));

        let card = &cards[0];
        assert!(card.table.is_some(), "the current table still fills");
        let diff = card
            .diff
            .as_ref()
            .expect("pr-diff arm reconstructs the cell diff");
        assert!(
            diff.rows.iter().any(|r| r.kind == RowChangeKind::Modified),
            "the touched seed cell is a Modified row",
        );
    }

    #[test]
    fn gather_seeds_leaves_diff_none_on_the_baseline_arm() {
        // The baseline arm passes `index: None` ⇒ no hunks to reconstruct from
        // (seeds carry zero row data in either manifest), so `diff` stays
        // `None` — the card renders the plain current table.
        let seed_id = "seed.shop.raw_customers";
        let manifest = seed_manifest(vec![seed_node(seed_id, Some("seeds/raw_customers.csv"))]);
        let mut entries = StdHashMap::new();
        entries.insert(
            "seeds/raw_customers.csv".to_owned(),
            StubResult::Ok("id,amount\n1,10\n".to_owned()),
        );
        let reader = StubReader { entries };

        let cards = build_seed_cards(&manifest, &seeds_in_scope_of(&[seed_id]));
        let cards = gather_seeds_with_reader(&reader, cards, None);

        assert!(cards[0].table.is_some());
        assert!(cards[0].diff.is_none(), "baseline arm carries no cell diff");
    }

    #[test]
    fn gather_seeds_degrades_truthfully_when_the_file_is_missing() {
        // A missing CSV ⇒ the card is STILL emitted (identity + lineage),
        // just with `table: None` — never dropped, never a silent skip.
        let seed_id = "seed.shop.raw_customers";
        let manifest = seed_manifest(vec![seed_node(seed_id, Some("seeds/raw_customers.csv"))]);
        // Empty reader ⇒ every read is NotFound.
        let reader = StubReader {
            entries: StdHashMap::new(),
        };

        let cards = build_seed_cards(&manifest, &seeds_in_scope_of(&[seed_id]));
        let cards = gather_seeds_with_reader(&reader, cards, None);

        assert_eq!(cards.len(), 1, "a missing file degrades, never drops");
        assert_eq!(cards[0].id, NodeId::new(seed_id));
        assert!(
            cards[0].table.is_none(),
            "the truthful empty-data state — not a fabricated grid",
        );
    }

    #[test]
    fn gather_seeds_degrades_truthfully_when_no_project_root_resolves() {
        // The no-`--project-root` arm of the public `gather_seeds`: nothing
        // can be read, so EVERY in-scope seed still yields a card carrying
        // identity + lineage with `table: None`. Driving the full
        // `gather_seeds` (not the `_with_reader` core) exercises the
        // root-resolution degrade: a `--pr-diff` ReportArgs with no
        // `--project-root` and a relative `--manifest` cannot derive a root.
        let seed_id = "seed.shop.raw_customers";
        let manifest = seed_manifest(vec![
            seed_node(seed_id, Some("seeds/raw_customers.csv")),
            model_consuming_seed("model.shop.stg_customers", seed_id),
        ]);
        // No `--project-root` and a bare `--manifest` name (not the
        // `<root>/target/manifest.json` layout) ⇒ `resolve_project_root`
        // yields `None`: the no-root degrade. The default `cli()` args
        // already carry `manifest: "current.json"`, `project_root: None`.
        let args = cli("out.html");

        let cards = gather_seeds(&args, &manifest, &seeds_in_scope_of(&[seed_id]), None);

        assert_eq!(cards.len(), 1, "no-root degrade emits every card");
        assert_eq!(cards[0].id, NodeId::new(seed_id));
        assert_eq!(cards[0].feeds_models, vec!["stg_customers"]);
        assert!(
            cards[0].table.is_none(),
            "no project root ⇒ labeled empty-data state, never a blank grid",
        );
    }

    #[test]
    fn gather_seeds_returns_an_empty_vec_when_no_seed_is_in_scope() {
        // The not-called path (the conditional-clear lesson): an empty
        // in-scope set yields an empty vec — no spurious cards, no read
        // attempts. Pin it so a future refactor cannot leak a card on the
        // zero-seed path.
        let manifest = seed_manifest(vec![seed_node(
            "seed.shop.raw_customers",
            Some("seeds/raw_customers.csv"),
        )]);
        // A reader that WOULD serve the file — proving emptiness comes from
        // the empty scope set, not a read miss.
        let mut entries = StdHashMap::new();
        entries.insert(
            "seeds/raw_customers.csv".to_owned(),
            StubResult::Ok("id\n1\n".to_owned()),
        );
        let reader = StubReader { entries };

        let cards = build_seed_cards(&manifest, &seeds_in_scope_of(&[]));
        let cards = gather_seeds_with_reader(&reader, cards, None);

        assert!(cards.is_empty(), "no seed in scope ⇒ empty vec");
    }
}
