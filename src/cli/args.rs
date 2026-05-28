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

use clap::{ArgGroup, Parser};

use crate::adapters::config_reader::load_config;
use crate::cli::pr_diff::ChangedFiles;
use crate::domain::AnalysisConfig;

/// cute-dbt — render a diff-scoped, self-contained HTML report of a dbt
/// project's unit tests.
#[derive(Debug, Parser)]
#[command(name = "cute-dbt", version, about)]
// Exactly one scope source is required: `--baseline-manifest` (dbt
// `state:modified`) XOR `--scope-from-pr-diff` (PR changed-files list).
// `required(true)` + `multiple(false)` makes "neither" a
// MissingRequiredArgument and "both" an ArgumentConflict — both clap
// usage errors (exit 2), never a `PreflightError` (cute-dbt#85, ADR-2
// precedent). This preserves the v0.1 baseline-required UX: a full
// unscoped run still diffs against an empty/genesis baseline.
#[command(group = ArgGroup::new("scope_source")
    .required(true)
    .multiple(false)
    .args(["baseline_manifest", "scope_from_pr_diff"]))]
pub struct Cli {
    /// Path to the compiled dbt `manifest.json` to visualise.
    #[arg(long, value_name = "PATH")]
    pub manifest: PathBuf,

    /// Path to the baseline `manifest.json` to diff against (dbt
    /// `state:modified` scope source).
    ///
    /// One of the two mutually-exclusive scope sources (the other is
    /// `--scope-from-pr-diff`); exactly one must be supplied. cute-dbt
    /// v0.1 is PR-review-first, so the report is scoped to the unit
    /// tests whose model changed relative to this baseline. For a
    /// full-manifest report, diff against an empty or genesis baseline.
    #[arg(long, value_name = "PATH")]
    pub baseline_manifest: Option<PathBuf>,

    /// PR changed-files scope source (CI/PR-review path) — no baseline
    /// manifest needed.
    ///
    /// Accepts either a literal comma/newline-separated list
    /// (`models/a.sql,models/b.yml`) or an `@file` reference
    /// (`@changed.txt`, one path per line). The workflow / Action
    /// computes the list — `git diff --name-only
    /// ${base.sha}...${head.sha}` — and passes it here; cute-dbt does
    /// not shell out to `git` or read `GITHUB_EVENT_PATH`.
    ///
    /// Mutually exclusive with `--baseline-manifest`. A bad `@file`
    /// (missing / non-UTF-8) is a clap usage error (exit 2).
    #[arg(long, value_name = "LIST|@FILE", value_parser = crate::cli::pr_diff::parse_arg_value)]
    pub scope_from_pr_diff: Option<ChangedFiles>,

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

    /// Optional dbt project root — the directory that contains
    /// `dbt_project.yml` and is the anchor for the manifest's
    /// `original_file_path` entries.
    ///
    /// When supplied, cute-dbt reads each in-scope `unit_test`'s source
    /// YAML and surfaces an "Authoring YAML" drawer in the report
    /// (cute-dbt#69). When absent, cute-dbt attempts to derive the
    /// project root from `--manifest` (by stripping a trailing
    /// `target/manifest.json`) before silently skipping the YAML
    /// extraction.
    ///
    /// An explicit `--project-root` that does not exist or is not a
    /// directory is a clap usage error (exit 2). The implicit-derive
    /// path is soft-failing: if no `dbt_project.yml` is found at the
    /// derived location, no error fires — the report still renders
    /// without the authoring-YAML drawer.
    #[arg(long, value_name = "PATH", value_parser = parse_project_root)]
    pub project_root: Option<PathBuf>,
}

/// clap value-parser: read + deserialize the TOML at `--config <PATH>`.
///
/// Errors are stringified for clap's usage-error path. The resolved
/// [`AnalysisConfig`] is stored in [`Cli::config`].
fn parse_config_file(s: &str) -> Result<AnalysisConfig, String> {
    load_config(Path::new(s)).map_err(|err| err.to_string())
}

/// clap value-parser: validate that an explicit `--project-root` points
/// at an existing directory. The implicit-derive path (when no flag is
/// passed) is handled in the run loop via [`resolve_project_root`];
/// this value-parser only runs when the operator typed the flag, so
/// silent-fallback semantics are intentionally absent here.
fn parse_project_root(s: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(s);
    if !p.exists() {
        return Err(format!("project root does not exist: {s}"));
    }
    if !p.is_dir() {
        return Err(format!("project root is not a directory: {s}"));
    }
    Ok(p)
}

/// Resolve the effective dbt project root.
///
/// Resolution policy:
/// 1. If `explicit` is `Some(p)`, return it unchanged. clap's
///    value-parser already validated that `p` exists and is a directory.
/// 2. Otherwise try to derive from `manifest_path` by stripping a
///    trailing `target/manifest.json` — the standard dbt layout. If the
///    derived directory exists, return it.
/// 3. Otherwise return `None` — cute-dbt continues silently without
///    the authoring-YAML drawer.
///
/// Returns the resolved root and a boolean: `true` if the result was
/// derived (rather than explicit). The caller may want to emit a
/// stderr breadcrumb noting that a derived root is being used.
#[must_use]
pub fn resolve_project_root(
    explicit: Option<&Path>,
    manifest_path: &Path,
) -> (Option<PathBuf>, bool) {
    if let Some(p) = explicit {
        return (Some(p.to_path_buf()), false);
    }
    if let Some(derived) = derive_project_root_from_manifest(manifest_path) {
        if derived.is_dir() {
            return (Some(derived), true);
        }
    }
    (None, false)
}

/// Strip the conventional `target/manifest.json` suffix from a manifest
/// path. Returns the parent of `target/`, which is the dbt project root
/// in the standard layout. Returns `None` for any other shape.
fn derive_project_root_from_manifest(manifest_path: &Path) -> Option<PathBuf> {
    // The suffix we recognize is exactly: <root>/target/manifest.json.
    if manifest_path.file_name()? != "manifest.json" {
        return None;
    }
    let target_dir = manifest_path.parent()?;
    if target_dir.file_name()? != "target" {
        return None;
    }
    target_dir.parent().map(Path::to_path_buf)
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
            .map_or(0, |d| d.as_micros());
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
        assert_eq!(cli.baseline_manifest, Some(PathBuf::from("baseline.json")));
        assert_eq!(cli.out, PathBuf::from("report.html"));
        // --config absent: the field is None.
        assert!(cli.config.is_none());
        // --scope-from-pr-diff absent: the field is None (baseline path).
        assert!(cli.scope_from_pr_diff.is_none());
    }

    #[test]
    fn a_missing_baseline_manifest_is_a_usage_error() {
        // Passing NEITHER scope source: the `scope_source` ArgGroup is
        // required, so omitting both --baseline-manifest and
        // --scope-from-pr-diff is a clap usage error, never a
        // PreflightError (cute-dbt#85).
        let err = parse(&["cute-dbt", "--manifest", "m.json", "--out", "o.html"])
            .expect_err("a scope source is required");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert!(
            err.to_string().contains("--baseline-manifest"),
            "the error names the missing scope source: {err}"
        );
    }

    #[test]
    fn passing_both_scope_sources_is_an_argument_conflict() {
        // The `scope_source` group is `multiple(false)` — supplying both
        // --baseline-manifest and --scope-from-pr-diff is a conflict.
        let err = parse(&[
            "cute-dbt",
            "--manifest",
            "current.json",
            "--baseline-manifest",
            "baseline.json",
            "--scope-from-pr-diff",
            "models/a.sql",
            "--out",
            "report.html",
        ])
        .expect_err("both scope sources is a conflict");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn scope_from_pr_diff_alone_parses_without_a_baseline() {
        let cli = parse(&[
            "cute-dbt",
            "--manifest",
            "current.json",
            "--scope-from-pr-diff",
            "models/a.sql,models/b.yml",
            "--out",
            "report.html",
        ])
        .expect("pr-diff-only is a complete argument set");
        assert!(cli.baseline_manifest.is_none());
        let changed = cli.scope_from_pr_diff.expect("scope_from_pr_diff is Some");
        assert_eq!(changed.paths, vec!["models/a.sql", "models/b.yml"]);
    }

    #[test]
    fn scope_from_pr_diff_at_missing_file_is_a_value_validation_error() {
        let path = unique_temp_path("missing-changed-list");
        // Deliberately do NOT create the file.
        let arg = format!("@{}", path.display());
        let err = parse(&[
            "cute-dbt",
            "--manifest",
            "current.json",
            "--scope-from-pr-diff",
            &arg,
            "--out",
            "report.html",
        ])
        .expect_err("a missing @file is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string().contains("could not read"),
            "error explains the read failure: {err}"
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

    fn unique_temp_dir(stem: &str) -> std::path::PathBuf {
        let nonce = COUNTER.fetch_add(1, Ordering::SeqCst);
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_micros());
        let pid = std::process::id();
        let p = std::env::temp_dir().join(format!("cute-dbt-args-{pid}-{micros}-{nonce}-{stem}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create temp dir");
        p
    }

    #[test]
    fn project_root_is_optional_when_omitted() {
        let cli = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
        ])
        .expect("no --project-root parses");
        assert!(cli.project_root.is_none());
    }

    #[test]
    fn explicit_project_root_is_validated_to_exist_and_be_a_dir() {
        let dir = unique_temp_dir("valid-root");
        let cli = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--project-root",
            dir.to_str().expect("temp dir utf-8"),
        ])
        .expect("an existing directory parses");
        assert_eq!(cli.project_root, Some(dir.clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_project_root_directory_is_a_usage_error() {
        let path = unique_temp_path("missing-root");
        // Deliberately do NOT create the directory.
        let err = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--project-root",
            path.to_str().expect("temp path utf-8"),
        ])
        .expect_err("missing --project-root is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string().contains("does not exist"),
            "error names the missing directory: {err}"
        );
    }

    #[test]
    fn a_file_supplied_as_project_root_is_a_usage_error() {
        // A non-directory path that DOES exist (a file) is still
        // wrong — the project root must be a directory.
        let file = write_fixture("not-a-dir", "irrelevant");
        let err = parse(&[
            "cute-dbt",
            "--manifest",
            "m.json",
            "--baseline-manifest",
            "b.json",
            "--out",
            "o.html",
            "--project-root",
            file.to_str().expect("temp path utf-8"),
        ])
        .expect_err("non-dir --project-root is a usage error");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(
            err.to_string().contains("not a directory"),
            "error names the not-a-directory condition: {err}"
        );
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn resolve_uses_explicit_root_when_supplied() {
        let dir = unique_temp_dir("explicit");
        let (resolved, derived) = resolve_project_root(Some(&dir), Path::new("/tmp/no.json"));
        assert_eq!(resolved.as_deref(), Some(dir.as_path()));
        assert!(!derived, "explicit is not derived");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_derives_from_manifest_path_with_target_layout() {
        // Set up a synthetic <root>/target/manifest.json layout.
        let project_root = unique_temp_dir("project-with-target");
        let target = project_root.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let manifest = target.join("manifest.json");
        std::fs::write(&manifest, "{}").unwrap();

        let (resolved, derived) = resolve_project_root(None, &manifest);
        assert_eq!(resolved.as_deref(), Some(project_root.as_path()));
        assert!(derived, "resolved-via-derive is flagged");

        let _ = std::fs::remove_dir_all(&project_root);
    }

    #[test]
    fn resolve_returns_none_when_manifest_path_is_unconventional() {
        // A manifest not under `target/manifest.json` — no derive
        // is possible. The result is `None` for both fields.
        let resolved = resolve_project_root(None, Path::new("/tmp/arbitrary/foo.json"));
        assert_eq!(resolved.0, None);
        assert!(!resolved.1);
    }
}
