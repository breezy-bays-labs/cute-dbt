//! CLI surface: clap argument parsing, the named run loop, and the
//! mapping from a run outcome to a process [`ExitCode`].
//!
//! The run loop is composed here as named stages —
//! `load_current` → `resolve_scope_input` → `select_in_scope` →
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
//! Three exit codes: `0` success, `1` a run-time failure (a fail-closed
//! manifest or an unwritable output path — no partial report is ever
//! written), `2` an operator usage error (clap rejected the arguments,
//! including supplying neither or both scope sources —
//! `--baseline-manifest` / `--pr-diff`).

mod args;
mod exit;
mod pr_diff;

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use crate::adapters::manifest::{FileManifestSource, load_baseline};
use crate::adapters::project_file::FsProjectFileReader;
use crate::adapters::render::{
    ExternalFixtures, LoadedFixture, ScopeSource, index_tests_for_models,
    render_report_with_externals,
};
use crate::domain::{
    BlockDiff, CheckPolicy, DEFAULT_REPORT_TITLE, FixtureTableDiff, HeuristicId, InScopeSet,
    Manifest, ModelInScopeSet, NamedTableDiff, NormalizedDiffIndex, PreflightError, ScopeInput,
    ScopeSelection, SuppressRule, SuppressionSource, UnitTest, UnitTestDataDiff, UnitTestYamlBlock,
    check_by_id, effective_fixture_format, external_fixture_table, extract_unit_test_block,
    preflight_compiled, reconstruct_block_diffs, reconstruct_external_fixture_diff,
    reconstruct_model_sql_diffs, reconstruct_table_diffs, refine_changed_by_hunks,
    resolve_check_policy, scan_pragmas, select_in_scope,
};
use crate::ports::{ManifestSource, ProjectFileReader};

use args::Cli;

/// Exit code for a run-time failure: a fail-closed manifest (Stage-1 or
/// Stage-2) or an unwritable `--out` path.
const EXIT_FAILURE: u8 = 1;

/// Exit code for an operator usage error (clap rejected the arguments).
const EXIT_USAGE: u8 = 2;

/// Binary entry point: parse arguments, run the pipeline, and map the
/// outcome to a process exit code.
#[must_use]
pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => return report_arg_error(&err),
    };
    match execute(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("{}", failure.message(&cli));
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
    /// The generated report could not be written to `--out`.
    Output(io::Error),
}

impl From<PreflightError> for RunError {
    fn from(err: PreflightError) -> Self {
        Self::Preflight(err)
    }
}

impl From<io::Error> for RunError {
    fn from(err: io::Error) -> Self {
        Self::Output(err)
    }
}

impl RunError {
    /// The operator-facing stderr message for this failure.
    fn message(&self, cli: &Cli) -> String {
        match self {
            Self::Preflight(err) => exit::remediation(err),
            Self::Output(err) => format!(
                "cute-dbt: could not write the report to {}: {err}",
                cli.out.display()
            ),
        }
    }
}

/// The named run loop — `load_current` → `resolve_scope_input` →
/// `select_in_scope` → `preflight_compiled` → `parse_ctes` →
/// `gather_authoring_yaml` → `render`.
///
/// `resolve_scope_input` runs Stage-1 pre-flight on the baseline manifest
/// only on the `--baseline-manifest` path; the `--pr-diff`
/// path needs no baseline. `?` short-circuits before `render`, so a
/// fail-closed manifest never produces a partial `report.html`.
fn execute(cli: &Cli) -> Result<(), RunError> {
    let current = load_current(cli)?;
    let scope_input = resolve_scope_input(cli)?;
    let ScopeSelection {
        in_scope,
        models_in_scope,
        changed,
    } = select_in_scope(&current, &scope_input);
    // Stage-2 fail-closed reads the TRUE in-scope set (cute-dbt#91): the
    // widened render set is only for what the report displays.
    preflight_compiled(&current, &in_scope, &models_in_scope)?;
    parse_ctes();
    // The widened render set — every unit test on an in-scope model, not
    // just the in-scope ones — drives BOTH the Authoring-YAML gather and
    // the render payload, so context siblings keep their YAML drawer
    // (cute-dbt#91).
    let render_test_ids = render_test_ids(&current, &models_in_scope);
    let authoring_yaml = gather_authoring_yaml(cli, &current, &render_test_ids);
    // External fixture files (cute-dbt#126): read each rendered test's
    // external `given`/`expect` fixture so the report inlines a real grid
    // instead of the #98 affordance. Reads the working tree at generation
    // time only (zero-egress unaffected); soft-fails per fixture.
    let external_fixtures = gather_external_fixtures(cli, &current, &render_test_ids);
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
    let (report_title, report_subtitle) = resolve_report_strings(cli);
    let (baseline_label, scope_source) = scope_banner(cli, &scope_input);
    // Check selection + suppression (cute-dbt#171): the `[checks]` config
    // policy plus inline SQL pragmas scanned from each in-scope model's
    // manifest `raw_code`. Display-layer only — applied inside payload
    // assembly strictly after supersedes resolution.
    let check_policy = build_check_policy(cli, &current, &models_in_scope);
    render(
        &cli.out,
        &current,
        &in_scope,
        &models_in_scope,
        &changed,
        &authoring_yaml,
        &yaml_diffs,
        &sql_diffs,
        &data_diffs,
        &external_fixtures,
        &baseline_label,
        scope_source,
        &report_title,
        report_subtitle.as_deref(),
        &check_policy,
    )?;
    Ok(())
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
    cli: &Cli,
    current: &Manifest,
    in_scope: &InScopeSet,
) -> HashMap<String, UnitTestYamlBlock> {
    let (resolved, derived) =
        args::resolve_project_root(cli.project_root.as_deref(), &cli.manifest);
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
    cli: &Cli,
    current: &Manifest,
    in_scope: &InScopeSet,
) -> HashMap<String, ExternalFixtures> {
    let (resolved, _derived) =
        args::resolve_project_root(cli.project_root.as_deref(), &cli.manifest);
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
fn resolve_report_strings(cli: &Cli) -> (String, Option<String>) {
    let report_cfg = cli.config.as_ref().map(|c| &c.report);
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
    cli: &Cli,
    current: &Manifest,
    models_in_scope: &ModelInScopeSet,
) -> CheckPolicy<HeuristicId> {
    let mut policy = cli.config.as_ref().map_or_else(CheckPolicy::default, |c| {
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
fn load_current(cli: &Cli) -> Result<Manifest, RunError> {
    let source = FileManifestSource;
    let current = source.load(&cli.manifest)?;
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
fn resolve_scope_input(cli: &Cli) -> Result<ScopeInput, RunError> {
    if let Some(baseline_path) = cli.baseline_manifest.as_deref() {
        let source = FileManifestSource;
        let baseline = load_baseline(&source, baseline_path)?;
        let sub_selectors = cli
            .modified_selectors
            .iter()
            .map(|selector| selector.kind())
            .collect();
        Ok(ScopeInput::Baseline {
            manifest: baseline,
            sub_selectors,
        })
    } else if let Some(diff) = cli.pr_diff.as_ref() {
        // Build the single NormalizedDiffIndex ONCE here and thread the
        // one instance through scope selection (and cute-dbt#96's
        // block-precise refinement + inline diff). It is the sole
        // normalization authority — the `--project-root` strip is baked
        // in as its diff-side strip (CAO plan-audit Decision 2).
        let index = NormalizedDiffIndex::new(diff, cli.project_root.as_deref());
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
fn scope_banner(cli: &Cli, scope_input: &ScopeInput) -> (String, ScopeSource) {
    match scope_input {
        ScopeInput::Baseline { .. } => (
            cli.baseline_manifest
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
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    external_fixtures: &HashMap<String, ExternalFixtures>,
    baseline_label: &str,
    scope_source: ScopeSource,
    report_title: &str,
    report_subtitle: Option<&str>,
    check_policy: &CheckPolicy<HeuristicId>,
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
        data_diffs,
        external_fixtures,
        baseline_label,
        scope_source,
        report_title,
        report_subtitle,
        check_policy,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(out: &str) -> Cli {
        Cli {
            manifest: "current.json".into(),
            baseline_manifest: Some("baseline.json".into()),
            out: out.into(),
            config: None,
            project_root: None,
            pr_diff: None,
            modified_selectors: Vec::new(),
        }
    }

    #[test]
    fn a_preflight_failure_message_is_the_remediation_text() {
        let failure = RunError::Preflight(PreflightError::NotCompiled {
            node_id: "model.shop.stg_orders".to_owned(),
            unit_test: Some("t".to_owned()),
        });
        let msg = failure.message(&cli("report.html"));
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

    fn cli_with_checks(checks: crate::domain::ChecksConfig) -> Cli {
        let mut cli = cli("report.html");
        cli.config = Some(crate::domain::AnalysisConfig {
            report: crate::domain::ReportConfig::default(),
            checks,
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
        let manifest = manifest_of_models(vec![model_with_raw("model.shop.orders", None)]);
        let policy = build_check_policy(
            &cli_with_checks(crate::domain::ChecksConfig {
                disable: Some(vec!["grain.*".to_owned()]),
                ..Default::default()
            }),
            &manifest,
            &scope_of(&["model.shop.orders"]),
        );
        assert!(policy.displayed.is_empty(), "grain.* disables every check");
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

    #[test]
    fn an_output_failure_message_names_the_out_path() {
        let failure = RunError::Output(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "permission denied",
        ));
        let msg = failure.message(&cli("/locked/report.html"));
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
}
