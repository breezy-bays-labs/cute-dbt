//! CLI surface: clap argument parsing, the named run loop, and the
//! mapping from a run outcome to a process [`ExitCode`].
//!
//! The run loop is composed here as four named stages —
//! `scope` → `preflight_compiled` → `parse_ctes` → `render`
//! (`ARCHITECTURE.md` §3, §6). Composition lives in `cli` by deliberate
//! single-crate design: there is no separate `app` / `usecase` crate.
//! `parse_ctes` is a named no-op call site: each in-scope model parses
//! its own `compiled_code` once during payload assembly inside
//! [`crate::adapters::render::render_report`], so the explicit
//! `parse_ctes` step is purely greppable scaffolding that mirrors the
//! ARCHITECTURE diagram. The `render` step invokes the askama renderer.
//!
//! Three exit codes: `0` success, `1` a run-time failure (a fail-closed
//! manifest or an unwritable output path — no partial report is ever
//! written), `2` an operator usage error (clap rejected the arguments,
//! including a missing required `--baseline-manifest`).

mod args;
mod exit;

use std::io;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use crate::adapters::manifest::{FileManifestSource, load_baseline};
use crate::adapters::render::render_report;
use crate::domain::{
    InScopeSet, Manifest, ModelInScopeSet, PreflightError, StateComparator, preflight_compiled,
};
use crate::ports::ManifestSource;

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

/// The named run loop — `scope` → `preflight_compiled` → `parse_ctes` →
/// `render`.
///
/// `?` short-circuits before `render`, so a fail-closed manifest never
/// produces a partial `report.html`.
fn execute(cli: &Cli) -> Result<(), RunError> {
    let (current, baseline) = load(cli)?;
    let (in_scope, models_in_scope) = scope(&current, &baseline);
    preflight_compiled(&current, &in_scope, &models_in_scope)?;
    parse_ctes();
    render(
        &cli.out,
        &current,
        &in_scope,
        &models_in_scope,
        &cli.baseline_manifest,
    )?;
    Ok(())
}

/// Stage-1 pre-flight: load the primary and baseline manifests through
/// the file-backed [`ManifestSource`].
///
/// A primary load failure is `Unreadable` / `SchemaUnsupported`; a
/// baseline load failure is remapped to `BaselineUnusable` by
/// [`load_baseline`].
fn load(cli: &Cli) -> Result<(Manifest, Manifest), RunError> {
    let source = FileManifestSource;
    let current = source.load(&cli.manifest)?;
    let baseline = load_baseline(&source, &cli.baseline_manifest)?;
    Ok((current, baseline))
}

/// The `scope` stage: select the unit tests and models in scope for this
/// diff (dbt `state:modified`, body-checksum fidelity — ADR-3).
///
/// Returns `(unit_tests_in_scope, models_in_scope)`. `models_in_scope`
/// is the explorer-mode set: every model targeted by an in-scope unit
/// test plus every modified model with zero unit tests (PR C / #30).
fn scope(current: &Manifest, baseline: &Manifest) -> (InScopeSet, ModelInScopeSet) {
    let comparator = StateComparator::body_only();
    let in_scope = comparator.in_scope_unit_tests(current, baseline);
    let models_in_scope = comparator.models_in_scope(current, baseline);
    (in_scope, models_in_scope)
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
/// `baseline_label` is the human-readable reference shown in the
/// diff-scope banner; v0.1 uses the `--baseline-manifest` path verbatim.
fn render(
    out: &Path,
    current: &Manifest,
    in_scope: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    baseline_path: &Path,
) -> Result<(), io::Error> {
    let baseline_label = baseline_path.display().to_string();
    render_report(out, current, in_scope, models_in_scope, &baseline_label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(out: &str) -> Cli {
        Cli {
            manifest: "current.json".into(),
            baseline_manifest: "baseline.json".into(),
            out: out.into(),
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
}
