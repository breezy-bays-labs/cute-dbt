//! CLI surface: clap argument parsing, the named run loop, and the
//! mapping from a run outcome to a process [`ExitCode`].
//!
//! The run loop is composed here as named stages —
//! `load_current` → `resolve_scope_input` → `select_in_scope` →
//! `preflight_compiled` → `parse_ctes` → `gather_authoring_yaml` →
//! `render` (`ARCHITECTURE.md` §3, §6). `resolve_scope_input` picks
//! between the `--baseline-manifest` and `--scope-from-pr-diff` scope
//! sources and loads the baseline only on the former path (cute-dbt#85).
//! Composition lives in `cli` by deliberate single-crate design: there
//! is no separate `app` / `usecase` crate. `parse_ctes` is a named
//! no-op call site: each in-scope model parses its own `compiled_code`
//! once during payload assembly inside
//! [`crate::adapters::render::render_report`], so the explicit
//! `parse_ctes` step is purely greppable scaffolding that mirrors the
//! ARCHITECTURE diagram. The `gather_authoring_yaml` step reads each
//! in-scope unit-test's source YAML through the [`SourceYamlReader`]
//! port and slices the authored block — soft-failing per test so a
//! missing file or unsupported manifest never breaks the report
//! (cute-dbt#69). The `render` step invokes the askama renderer.
//!
//! Three exit codes: `0` success, `1` a run-time failure (a fail-closed
//! manifest or an unwritable output path — no partial report is ever
//! written), `2` an operator usage error (clap rejected the arguments,
//! including supplying neither or both scope sources —
//! `--baseline-manifest` / `--scope-from-pr-diff`).

mod args;
mod exit;
mod pr_diff;

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use crate::adapters::manifest::{FileManifestSource, load_baseline};
use crate::adapters::render::{ScopeSource, render_report};
use crate::adapters::source_yaml::FsSourceYamlReader;
use crate::domain::{
    DEFAULT_REPORT_TITLE, InScopeSet, Manifest, ModelInScopeSet, PreflightError, ScopeInput,
    UnitTestYamlBlock, extract_unit_test_block, preflight_compiled, select_in_scope,
};
use crate::ports::{ManifestSource, SourceYamlReader};

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
/// only on the `--baseline-manifest` path; the `--scope-from-pr-diff`
/// path needs no baseline. `?` short-circuits before `render`, so a
/// fail-closed manifest never produces a partial `report.html`.
fn execute(cli: &Cli) -> Result<(), RunError> {
    let current = load_current(cli)?;
    let scope_input = resolve_scope_input(cli)?;
    let (in_scope, models_in_scope) = select_in_scope(&current, &scope_input);
    preflight_compiled(&current, &in_scope, &models_in_scope)?;
    parse_ctes();
    let authoring_yaml = gather_authoring_yaml(cli, &current, &in_scope);
    let (report_title, report_subtitle) = resolve_report_strings(cli);
    let (baseline_label, scope_source) = scope_banner(cli, &scope_input);
    render(
        &cli.out,
        &current,
        &in_scope,
        &models_in_scope,
        &authoring_yaml,
        &baseline_label,
        scope_source,
        &report_title,
        report_subtitle.as_deref(),
    )?;
    Ok(())
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
    let reader = FsSourceYamlReader::new(project_root);
    gather_authoring_yaml_with_reader(&reader, current, in_scope)
}

/// Pure composition step over the [`SourceYamlReader`] port — testable
/// without touching the filesystem by passing an in-memory impl.
fn gather_authoring_yaml_with_reader(
    reader: &dyn SourceYamlReader,
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

/// Stage-1 pre-flight: load the primary `--manifest` through the
/// file-backed [`ManifestSource`].
///
/// A load failure is `Unreadable` / `SchemaUnsupported`. The baseline
/// manifest (when scoping via `--baseline-manifest`) is loaded separately
/// in [`resolve_scope_input`] so the `--scope-from-pr-diff` path can skip
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
///   wrap it in [`ScopeInput::Baseline`].
/// - `--scope-from-pr-diff` → wrap the already-parsed changed-files list
///   in [`ScopeInput::PrDiff`], rebasing PR-diff paths against the
///   manifest's project-relative `original_file_path` via
///   `--project-root`.
///
/// clap's `scope_source` [`ArgGroup`](clap::ArgGroup) (`required`,
/// single) guarantees exactly one arm is set, so the trailing branch is
/// unreachable.
fn resolve_scope_input(cli: &Cli) -> Result<ScopeInput, RunError> {
    if let Some(baseline_path) = cli.baseline_manifest.as_deref() {
        let source = FileManifestSource;
        let baseline = load_baseline(&source, baseline_path)?;
        Ok(ScopeInput::Baseline { manifest: baseline })
    } else if let Some(changed) = cli.scope_from_pr_diff.as_ref() {
        Ok(ScopeInput::PrDiff {
            changed_files: changed.paths.clone(),
            project_root_strip: cli.project_root.clone(),
        })
    } else {
        unreachable!(
            "clap's scope_source ArgGroup guarantees exactly one of \
             --baseline-manifest / --scope-from-pr-diff is provided"
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
fn render(
    out: &Path,
    current: &Manifest,
    in_scope: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    baseline_label: &str,
    scope_source: ScopeSource,
    report_title: &str,
    report_subtitle: Option<&str>,
) -> Result<(), io::Error> {
    render_report(
        out,
        current,
        in_scope,
        models_in_scope,
        authoring_yaml,
        baseline_label,
        scope_source,
        report_title,
        report_subtitle,
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
            scope_from_pr_diff: None,
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
        });
        let (title, subtitle) = resolve_report_strings(&cli);
        assert_eq!(title, "title-only");
        assert!(subtitle.is_none());
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

    use crate::domain::{DependsOn, Manifest, ManifestMetadata, NodeId, UnitTest, UnitTestExpect};

    enum StubResult {
        Ok(String),
        Err(io::ErrorKind, &'static str),
    }

    struct StubReader {
        entries: StdHashMap<String, StubResult>,
    }

    impl SourceYamlReader for StubReader {
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
            UnitTestExpect::new(serde_json::Value::Null, None),
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
}
