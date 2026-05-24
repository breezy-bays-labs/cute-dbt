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

use crate::domain::AnalysisConfig;

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
}

/// Load + parse the operator-supplied TOML config.
///
/// Reads `path` as UTF-8, then deserializes into [`AnalysisConfig`].
///
/// # Errors
///
/// Returns [`ConfigLoadError::Io`] when the file cannot be read,
/// [`ConfigLoadError::Toml`] when the content is not valid TOML or does
/// not match the schema.
pub fn load_config(path: &Path) -> Result<AnalysisConfig, ConfigLoadError> {
    let path_str = path.display().to_string();
    let bytes = fs::read_to_string(path).map_err(|source| ConfigLoadError::Io {
        path: path_str.clone(),
        source,
    })?;
    toml::from_str(&bytes).map_err(|source| ConfigLoadError::Toml {
        path: path_str,
        source,
    })
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
            .map(|d| d.as_micros())
            .unwrap_or(0);
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
            other => panic!("expected Io error, got {other:?}"),
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
            other => panic!("expected Toml error, got {other:?}"),
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
