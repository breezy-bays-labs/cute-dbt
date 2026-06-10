//! TOML config-file reader for the operator-supplied `--config <PATH>`.
//!
//! Two-line wrapper around `std::fs::read_to_string` + `toml::from_str`,
//! split so the parse logic stays testable without filesystem access. A
//! failure here is a **clap usage error** (exit 2), not a
//! [`crate::domain::PreflightError`] — the ARCHITECTURE.md §3
//! baseline-missing precedent applies: config errors are usage-time, not
//! runtime preflight.
//!
//! Returned `ConfigLoadError` is opaque to the domain — it is consumed
//! by the `cli::args` value-parser fn and stringified into the clap
//! error path. Adding a fifth `PreflightError::BadConfig` variant would
//! conflate usage-time and runtime errors and is explicitly out of
//! scope.

use std::fs;
use std::path::Path;

use thiserror::Error;

use crate::domain::{AnalysisConfig, CheckConfigError, HeuristicId, resolve_check_policy};

/// Reasons a `--config <PATH>` file could not be loaded.
///
/// Both variants carry the operator-supplied path so the stderr message
/// names the file the operator typed (not a `.canonicalize()`-ed form
/// that would surprise them).
#[derive(Debug, Error)]
pub enum ConfigLoadError {
    /// The file could not be read (missing, permission denied, not UTF-8).
    #[error("could not read config file {path}: {source}")]
    Io {
        /// The operator-supplied path, verbatim.
        path: String,
        /// Underlying `std::io::Error` (file-not-found, etc.).
        #[source]
        source: std::io::Error,
    },
    /// The bytes were read but did not parse as TOML, did not match the
    /// `AnalysisConfig` schema, or carried an unknown field (rejected by
    /// `deny_unknown_fields` at both nesting levels).
    #[error("invalid TOML in config file {path}: {source}")]
    Toml {
        /// The operator-supplied path, verbatim.
        path: String,
        /// Underlying `toml::de::Error` (parse failure, schema mismatch,
        /// or `deny_unknown_fields` rejection).
        #[source]
        source: toml::de::Error,
    },
    /// The TOML parsed but the `[checks]` section failed the
    /// fail-closed registry validation (cute-dbt#171): mode/field
    /// legality, unknown check ids or group globs, glob/empty-reason
    /// suppress entries. The underlying
    /// [`CheckConfigError`] carries the remediation text (including the
    /// registry's known checks and groups).
    #[error("invalid [checks] in config file {path}: {source}")]
    Checks {
        /// The operator-supplied path, verbatim.
        path: String,
        /// Underlying validation failure, remediation-bearing.
        #[source]
        source: CheckConfigError,
    },
}

/// Load + parse + validate the operator-supplied TOML config.
///
/// Reads `path` as UTF-8, deserializes into [`AnalysisConfig`], then
/// runs the `[checks]` fail-closed validation against the production
/// [`HeuristicId`] registry ([`resolve_check_policy`], cute-dbt#171) so
/// every config failure — syntax, schema, or check-registry — surfaces
/// on the same clap usage-error path (exit 2). The resolved policy is
/// discarded here; the run loop re-resolves it (infallibly, post
/// validation) when building the render-time display policy.
///
/// # Errors
///
/// Returns [`ConfigLoadError::Io`] when the file cannot be read,
/// [`ConfigLoadError::Toml`] when the content is not valid TOML or does
/// not match the schema, [`ConfigLoadError::Checks`] when the
/// `[checks]` section fails registry validation.
pub fn load_config(path: &Path) -> Result<AnalysisConfig, ConfigLoadError> {
    let path_str = path.display().to_string();
    let bytes = fs::read_to_string(path).map_err(|source| ConfigLoadError::Io {
        path: path_str.clone(),
        source,
    })?;
    let config: AnalysisConfig =
        toml::from_str(&bytes).map_err(|source| ConfigLoadError::Toml {
            path: path_str.clone(),
            source,
        })?;
    resolve_check_policy::<HeuristicId>(&config.checks).map_err(|source| {
        ConfigLoadError::Checks {
            path: path_str,
            source,
        }
    })?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        std::env::temp_dir().join(format!("cute-dbt-test-{pid}-{micros}-{nonce}-{stem}.toml"))
    }

    fn write_fixture(stem: &str, content: &str) -> std::path::PathBuf {
        let path = unique_temp_path(stem);
        let mut f = fs::File::create(&path).expect("create temp fixture");
        f.write_all(content.as_bytes()).expect("write temp fixture");
        path
    }

    #[test]
    fn loads_a_valid_config_with_both_keys() {
        let path = write_fixture(
            "both-keys",
            r#"
[report]
title = "Q3 unit test review"
subtitle = "PR 1234 / staging diff"
"#,
        );
        let cfg = load_config(&path).expect("valid config loads");
        assert_eq!(cfg.report.title.as_deref(), Some("Q3 unit test review"));
        assert_eq!(
            cfg.report.subtitle.as_deref(),
            Some("PR 1234 / staging diff")
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn loads_an_empty_file_as_default() {
        let path = write_fixture("empty", "");
        let cfg = load_config(&path).expect("empty file loads as default");
        assert_eq!(cfg, AnalysisConfig::default());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_io_error_naming_the_path() {
        let path = unique_temp_path("does-not-exist");
        // Deliberately do NOT create the file.
        let err = load_config(&path).expect_err("missing file errors");
        match err {
            ConfigLoadError::Io { path: reported, .. } => {
                assert!(
                    reported.contains("does-not-exist"),
                    "Io error carries the operator path: {reported}"
                );
            }
            other => {
                panic!("expected Io error, got {other:?}");
            }
        }
    }

    #[test]
    fn invalid_toml_is_toml_error_naming_the_path() {
        let path = write_fixture("broken", "not valid toml { = =");
        let err = load_config(&path).expect_err("invalid TOML errors");
        match err {
            ConfigLoadError::Toml { path: reported, .. } => {
                assert!(
                    reported.contains("broken"),
                    "Toml error carries the operator path: {reported}"
                );
            }
            other => {
                panic!("expected Toml error, got {other:?}");
            }
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn unknown_field_is_toml_error_naming_the_path() {
        // deny_unknown_fields rejects typo'd keys; the loader surfaces
        // these as Toml errors (not a separate variant).
        let path = write_fixture(
            "typo",
            r#"
[report]
tilte = "typo'd"
"#,
        );
        let err = load_config(&path).expect_err("typo'd key errors");
        match err {
            ConfigLoadError::Toml {
                path: reported,
                source,
            } => {
                assert!(
                    reported.contains("typo"),
                    "Toml error carries the operator path: {reported}"
                );
                let detail = source.to_string();
                assert!(
                    detail.contains("tilte") || detail.contains("unknown field"),
                    "underlying TOML error names the typo'd field: {detail}"
                );
            }
            other => {
                panic!("expected Toml error, got {other:?}");
            }
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn valid_checks_section_loads() {
        let path = write_fixture(
            "checks-valid",
            r#"
[checks]
mode = "opt-in"
enable = ["grain.*"]

[[checks.suppress]]
check = "grain.unique-key-unbacked"
model = "orders"
reason = "duplicate grain accepted during backfill"
"#,
        );
        let cfg = load_config(&path).expect("valid [checks] loads");
        assert_eq!(cfg.checks.suppress.len(), 1);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn unknown_check_id_is_a_checks_error_with_remediation() {
        let path = write_fixture(
            "checks-unknown-id",
            "[checks]\ndisable = [\"grain.nonexistent\"]\n",
        );
        let err = load_config(&path).expect_err("unknown check id errors");
        match err {
            ConfigLoadError::Checks {
                path: reported,
                source,
            } => {
                assert!(reported.contains("checks-unknown-id"), "{reported}");
                let detail = source.to_string();
                assert!(detail.contains("grain.nonexistent"), "{detail}");
                assert!(
                    detail.contains("grain.unique-key-unbacked"),
                    "remediation names known checks: {detail}"
                );
            }
            other => panic!("expected Checks error, got {other:?}"),
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn enable_in_opt_out_mode_is_a_checks_error() {
        let path = write_fixture("checks-enable-optout", "[checks]\nenable = [\"grain.*\"]\n");
        let err = load_config(&path).expect_err("enable needs opt-in");
        let msg = err.to_string();
        assert!(msg.contains("invalid [checks]"), "{msg}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn suppress_missing_reason_is_a_toml_error_naming_the_field() {
        // `reason` is serde-required — a missing field is a Toml error,
        // before the Checks validation even runs.
        let path = write_fixture(
            "checks-no-reason",
            "[[checks.suppress]]\ncheck = \"grain.unique-key-unbacked\"\nmodel = \"orders\"\n",
        );
        let err = load_config(&path).expect_err("missing reason errors");
        match err {
            ConfigLoadError::Toml { source, .. } => {
                assert!(source.to_string().contains("reason"), "{source}");
            }
            other => panic!("expected Toml error, got {other:?}"),
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn error_display_mentions_path_and_underlying_cause() {
        let path = unique_temp_path("never-created");
        let err = load_config(&path).expect_err("missing file errors");
        let msg = err.to_string();
        assert!(msg.contains("could not read config file"), "Display: {msg}");
        assert!(msg.contains("never-created"), "Display: {msg}");
    }
}
