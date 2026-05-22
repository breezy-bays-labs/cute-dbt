//! The clap command-line surface.
//!
//! Three required arguments. Baseline-required is the locked v0.1 policy
//! (ADR-2): a missing `--baseline-manifest` is a clap usage error raised
//! before any manifest is read — never a `PreflightError`.

use std::path::PathBuf;

use clap::Parser;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

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
        // `--config` is deliberately not a v0.1 argument — it is deferred
        // to a later PR (cute-dbt#24). clap rejects it as unknown.
        let err = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--config",
            "c.toml",
        ])
        .expect_err("--config is not a v0.1 argument");
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }
}
