//! The clap command-line surface.
//!
//! Three required arguments. Baseline-required is the locked v0.1 policy
//! (ADR-2): a missing `--baseline-manifest` is a clap usage error raised
//! before any manifest is read — never a `PreflightError`.
//!
//! One optional argument: `--config <PATH>` (PR 14, #24). The clap
//! value-parser opens + parses the TOML eagerly; a bad / unreadable
//! file is a clap usage error (exit 2), not a `PreflightError` variant
//! — the same baseline-missing precedent (ARCHITECTURE.md §3) applies.

use std::path::{Path, PathBuf};

use clap::Parser;

use crate::adapters::config_reader::load_config;
use crate::domain::AnalysisConfig;

/// cute-dbt — render a diff-scoped, self-contained HTML report of a dbt
/// project's unit tests.
#[derive(Debug, Parser)]
#[command(name = "cute-dbt", version, about)]
pub struct Cli {
    /// Path to the compiled dbt `manifest.json` to visualise.
    #[arg(long, value_name = "PATH")]
    pub manifest: PathBuf,

    /// Path to the baseline `manifest.json` to diff against.
    ///
    /// Required: cute-dbt v0.1 is PR-review-first, so the report is
    /// scoped to the unit tests whose model changed relative to this
    /// baseline. For a full-manifest report, diff against an empty or
    /// genesis baseline.
    #[arg(long, value_name = "PATH")]
    pub baseline_manifest: PathBuf,

    /// Path the generated `report.html` is written to.
    #[arg(long, value_name = "PATH")]
    pub out: PathBuf,

    /// Optional TOML configuration. Currently exposes `[report].title`
    /// and `[report].subtitle`; both override the rendered HTML's
    /// `<title>`/`<h1>` and (subtitle only) inject a new
    /// `<p class="report-subtitle">` element.
    ///
    /// A missing, unreadable, or invalid file is a clap usage error
    /// (exit 2) — never a `PreflightError`.
    #[arg(long, value_name = "PATH", value_parser = parse_config_file)]
    pub config: Option<AnalysisConfig>,
}

/// clap value-parser: read + deserialize the TOML at `--config <PATH>`.
///
/// Errors are stringified for clap's usage-error path. The resolved
/// [`AnalysisConfig`] is stored in [`Cli::config`].
fn parse_config_file(s: &str) -> Result<AnalysisConfig, String> {
    load_config(Path::new(s)).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_path(stem: &str) -> std::path::PathBuf {
        let nonce = COUNTER.fetch_add(1, Ordering::SeqCst);
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("cute-dbt-args-{pid}-{micros}-{nonce}-{stem}.toml"))
    }

    fn write_fixture(stem: &str, content: &str) -> std::path::PathBuf {
        let path = unique_temp_path(stem);
        let mut f = std::fs::File::create(&path).expect("create temp fixture");
        f.write_all(content.as_bytes()).expect("write temp fixture");
        path
    }

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    #[test]
    fn all_three_arguments_parse_into_their_paths() {
        let cli = parse(&[
            "cute-dbt",
            "--manifest",
            "current.json",
            "--baseline-manifest",
            "baseline.json",
            "--out",
            "report.html",
        ])
        .expect("a complete argument set parses");
        assert_eq!(cli.manifest, PathBuf::from("current.json"));
        assert_eq!(cli.baseline_manifest, PathBuf::from("baseline.json"));
        assert_eq!(cli.out, PathBuf::from("report.html"));
        // --config absent: the field is None.
        assert!(cli.config.is_none());
    }

    #[test]
    fn a_missing_baseline_manifest_is_a_usage_error() {
        // The locked baseline-required policy: omitting --baseline-manifest
        // is a clap usage error, never a PreflightError.
        let err = parse(&["cute-dbt", "--manifest", "m.json", "--out", "o.html"])
            .expect_err("--baseline-manifest is required");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert!(
            err.to_string().contains("--baseline-manifest"),
            "the error names the missing argument: {err}"
        );
    }

    #[test]
    fn a_missing_manifest_is_a_usage_error() {
        let err = parse(&[
            "cute-dbt",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
        ])
        .expect_err("--manifest is required");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn a_missing_out_is_a_usage_error() {
        let err = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
        ])
        .expect_err("--out is required");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn help_is_a_display_help_error_kind() {
        let err = parse(&["cute-dbt", "--help"]).expect_err("--help short-circuits parsing");
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
    }

    #[test]
    fn an_unknown_argument_is_a_usage_error() {
        // clap rejects any flag not on the v0.1 surface. PR 14 (cute-dbt#24)
        // added --config to the surface, so the test now uses a different
        // genuinely-unknown flag to pin clap's unknown-arg behavior.
        let err = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--frobnitz",
            "value",
        ])
        .expect_err("--frobnitz is not a cute-dbt argument");
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn a_valid_config_file_parses_into_some() {
        let path = write_fixture(
            "valid",
            r#"
[report]
title = "Q3 review"
subtitle = "PR 1234"
"#,
        );
        let cli = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--config",
            path.to_str().expect("temp path utf-8"),
        ])
        .expect("a valid config parses");
        let cfg = cli.config.expect("config is Some");
        assert_eq!(cfg.report.title.as_deref(), Some("Q3 review"));
        assert_eq!(cfg.report.subtitle.as_deref(), Some("PR 1234"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_config_file_is_a_value_validation_error() {
        let path = unique_temp_path("does-not-exist");
        // Deliberately do NOT create the file.
        let err = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--config",
            path.to_str().expect("temp path utf-8"),
        ])
        .expect_err("missing config file is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string().contains("could not read config file"),
            "error explains the read failure: {err}"
        );
    }

    #[test]
    fn an_invalid_toml_config_is_a_value_validation_error() {
        let path = write_fixture("broken", "not valid toml { = =");
        let err = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--config",
            path.to_str().expect("temp path utf-8"),
        ])
        .expect_err("invalid TOML is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string().contains("invalid TOML in config file"),
            "error explains the parse failure: {err}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unknown_config_field_is_a_value_validation_error() {
        // deny_unknown_fields rejects typo'd keys; surfaces as the same
        // clap usage error path as wholesale-invalid TOML.
        let path = write_fixture(
            "typo",
            r#"
[report]
tilte = "typo'd"
"#,
        );
        let err = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--config",
            path.to_str().expect("temp path utf-8"),
        ])
        .expect_err("typo'd config key is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        let _ = std::fs::remove_file(&path);
    }
}
