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
//! Three exit codes: `0` success, `1` a run-time failure (a fail-closed
//! manifest or an unwritable output path — no partial report is ever
//! written), `2` an operator usage error (clap rejected the arguments,
//! including a bare `cute-dbt` with no subcommand, or supplying neither
//! or both `report` scope sources — `--baseline-manifest` / `--pr-diff`).

mod args;
mod exit;
mod pr_diff;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use crate::adapters::explore::render_explore;
use crate::adapters::manifest::{FileManifestSource, load_baseline};
use crate::adapters::project_def::parse as parse_project_definition;
use crate::adapters::project_file::FsProjectFileReader;
use crate::adapters::render::{
    ExternalFixtures, LoadedFixture, ScopeSource, build_payload, index_tests_for_models,
    render_report_with_externals,
};
use crate::domain::{
    BlockDiff, CheckPolicy, ConfigAttribution, DEFAULT_REPORT_TITLE, EnabledExperiments,
    FixtureTableDiff, HeuristicId, InScopeSet, Manifest, ModelInScopeSet, ModelYamlOutcome,
    NamedTableDiff, NormalizedDiffIndex, PreflightError, ProjectChangePanel, ProjectFacts,
    ProjectFallbackReason, ScopeInput, ScopeSelection, SuppressRule, SuppressionSource, UnitTest,
    UnitTestDataDiff, UnitTestYamlBlock, VarReference, all_models, attach_hook_facts,
    attach_model_yaml_diffs, attach_var_facts, attribute_config_tree_changes,
    attribute_var_changes, changed_models, check_by_id, diff_project_definitions,
    effective_fixture_format, external_fixture_table, extract_model_block, extract_unit_test_block,
    hook_operations, preflight_compiled, raw_hunk_lines, reconstruct_block_diffs,
    reconstruct_external_fixture_diff, reconstruct_model_sql_diffs, reconstruct_table_diffs,
    refine_changed_by_hunks, resolve_check_policy, resolve_experimental_config, reverse_apply,
    scan_pragmas, select_in_scope, widen_with_config_attributions,
};
use crate::ports::{ManifestSource, ProjectFileReader};

use args::{Cli, Command, ExploreArgs, ReportArgs};

/// Exit code for a run-time failure: a fail-closed manifest (Stage-1 or
/// Stage-2) or an unwritable `--out` path.
const EXIT_FAILURE: u8 = 1;

/// Exit code for an operator usage error (clap rejected the arguments).
const EXIT_USAGE: u8 = 2;

/// Binary entry point: parse arguments, dispatch the selected verb's
/// composition, and map the outcome to a process exit code.
#[must_use]
pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => return report_arg_error(&err),
    };
    let outcome = match &cli.command {
        Command::Report(report) => execute_report(report),
        Command::Explore(explore) => execute_explore(explore),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("{}", failure.message());
            ExitCode::from(EXIT_FAILURE)
        }
    }
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
/// widens the selection (cute-dbt#267). `?` short-circuits before
/// `render`, so a fail-closed manifest never produces a partial
/// `report.html`.
fn execute_report(args: &ReportArgs) -> Result<(), RunError> {
    let current = load_current(args)?;
    let scope_input = resolve_scope_input(args)?;
    // Project-definition facts (cute-dbt#266): parse the working-tree
    // dbt_project.yml whenever it is present — STANDING metadata, both
    // scope arms (the founder's parse-always posture; the parsed model
    // rides the payload for future consumers). The categorized
    // "Project definition changed" panel is the diff-gated consumer: on
    // the PrDiff arm, when dbt_project.yml is in the diff, the old side
    // is reconstructed by reverse-applying the file's own hunks
    // (drift-guarded) and the structural diff categorizes the change;
    // every degrade arm falls back to the Shape-A raw-diff row.
    // Fail-open: report generation NEVER fails because of this file.
    // Gathered BEFORE scope selection since cute-dbt#267: a categorized
    // config-tree edit carries per-model attributions that widen scope.
    let project_facts = gather_project_facts(args, &current, &scope_input);
    // Experimental switch (cute-dbt#289, epic #288): the resolved
    // TOML ∪ env opt-in set, bound ahead of its first consumers — the
    // project-state gate (cute-dbt#291) reads it HERE to gate the
    // cute-dbt#267 widening below and the project-state facts handed to
    // render. Mechanism-only in this slice (named no-op scaffolding,
    // the `parse_ctes` precedent): nothing consumes the binding yet, so
    // it is underscore-prefixed; resolution itself is unit-tested
    // directly (`resolve_enabled_experiments`).
    let _experiments = resolve_enabled_experiments(args);
    // Config-tree scope widening (cute-dbt#267): models whose fqn falls
    // under an edited `models:` subtree (fusion's get_config_for_fqn
    // prefix descent — TOTAL tier, by-definition change) join the
    // selection by union; their unit tests ride in as context. The one
    // widening category of epic #262 — vars stay contextualize-only.
    let ScopeSelection {
        in_scope,
        models_in_scope,
        changed,
    } = widen_with_config_attributions(
        select_in_scope(&current, &scope_input),
        &current,
        &project_facts.config_attributions,
    );
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
    let (baseline_label, scope_source) = scope_banner(args, &scope_input);
    // Check selection + suppression (cute-dbt#171): the `[checks]` config
    // policy plus inline SQL pragmas scanned from each in-scope model's
    // manifest `raw_code`. Display-layer only — applied inside payload
    // assembly strictly after supersedes resolution.
    let check_policy = build_check_policy(args, &current, &models_in_scope);
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
    )
    .map_err(|err| RunError::output(&args.out, err))?;
    Ok(())
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
    let changed = resolve_change_context(args, &current);
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
    render_explore(&args.out_dir, &current, &models, changed.as_ref(), &payload)
        .map_err(|err| RunError::output(&args.out_dir, err))?;
    Ok(())
}

/// The `resolve_change_context` stage (cute-dbt#106): map the optional
/// `--pr-diff` onto the changed-model set.
///
/// `None` — no `--pr-diff` — means **no change context** (the renderer
/// emits the unchanged no-context page shape), which is distinct from
/// `Some(empty)` — a diff that touched no model files still renders the
/// honest "0 changed in this diff" banner. The [`NormalizedDiffIndex`]
/// is built exactly like the report arm's (`resolve_scope_input`):
/// the `--project-root` strip rebases the diff's repo-relative paths
/// onto the manifest's project-relative `original_file_path` entries.
fn resolve_change_context(args: &ExploreArgs, current: &Manifest) -> Option<ModelInScopeSet> {
    args.pr_diff.as_ref().map(|diff| {
        let index = NormalizedDiffIndex::new(diff, args.project_root.as_deref());
        changed_models(current, &index)
    })
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
        if !ext.given.is_empty() || ext.expect.is_some() {
            out.insert(id.to_owned(), ext);
        }
    }
    out
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
) -> CheckPolicy<HeuristicId> {
    let mut policy = args.config.as_ref().map_or_else(CheckPolicy::default, |c| {
        resolve_check_policy::<HeuristicId>(&c.checks)
            .expect("[checks] was validated by the --config value-parser at parse time")
    });
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
        });
        let (title, subtitle) = resolve_report_strings(&cli);
        assert_eq!(title, "title-only");
        assert!(subtitle.is_none());
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
        });
        cli
    }

    #[test]
    fn build_check_policy_without_config_or_pragmas_is_the_default() {
        let manifest = manifest_of_models(vec![model_with_raw("model.shop.orders", None)]);
        let policy = build_check_policy(
            &cli("report.html"),
            &manifest,
            &scope_of(&["model.shop.orders"]),
        );
        assert_eq!(policy, CheckPolicy::default());
    }

    #[test]
    fn build_check_policy_resolves_the_config_selection() {
        // Registry-shape-robust: `grain.*` removes exactly the grain
        // group; every other registered check (e.g. union.arm-coverage,
        // cute-dbt#172) stays displayed.
        let manifest = manifest_of_models(vec![model_with_raw("model.shop.orders", None)]);
        let policy = build_check_policy(
            &cli_with_checks(crate::domain::ChecksConfig {
                disable: Some(vec!["grain.*".to_owned()]),
                ..Default::default()
            }),
            &manifest,
            &scope_of(&["model.shop.orders"]),
        );
        let expected: Vec<HeuristicId> = CheckPolicy::default()
            .displayed
            .into_iter()
            .filter(|id: &HeuristicId| {
                use crate::domain::CheckId as _;
                id.spec().group != "grain"
            })
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
        );
        assert!(
            policy.suppressions.is_empty(),
            "unknown id warns + stays inert; out-of-scope models are not scanned: {:?}",
            policy.suppressions
        );
        assert_eq!(policy.displayed, CheckPolicy::default().displayed);
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
            },
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

    #[test]
    fn project_facts_panel_absent_when_file_not_in_diff() {
        let diff = PrDiff {
            renames: Vec::new(),
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
}
