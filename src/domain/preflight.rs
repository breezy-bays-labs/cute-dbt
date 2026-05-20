//! `PreflightError` — the two-stage fail-closed currency (ADR-2).
//!
//! Four variants, no more (per the locked baseline-required policy at
//! the /plan gate — missing `--baseline-manifest` is a clap usage error
//! raised **before** this enum is reachable, not a fifth variant):
//!
//! - [`PreflightError::Unreadable`] — Stage-1 (adapter): the manifest
//!   bytes were not valid JSON or were missing structurally required
//!   keys. Raised by `adapters/manifest.rs` (PR 4b) during
//!   deserialization.
//! - [`PreflightError::SchemaUnsupported`] — Stage-1 (adapter):
//!   `metadata.dbt_schema_version` is below the dbt ≥1.8 floor.
//! - [`PreflightError::NotCompiled`] — Stage-2 (domain): an **in-scope**
//!   unit test's target model has `compiled_code: null`. Raised by the
//!   preflight pass in the run loop **after** `StateComparator` selects
//!   the in-scope set (PR 6). The remediation message lives in the
//!   `cli` exit-code mapping per ADR-2.
//! - [`PreflightError::BaselineUnusable`] — Stage-1 (adapter): the
//!   `--baseline-manifest` was supplied (it is required per the locked
//!   policy) but could not be read or did not parse against the schema.
//!
//! `#[non_exhaustive]` per the enums-yes-structs-no rule: consumers
//! pattern-match this enum and future fail-closed reasons are additive.
//! Pairing with `thiserror::Error` gives a `Display` impl with
//! deterministic remediation-friendly wording.

use thiserror::Error;

/// Domain-level fail-closed currency for the two-stage preflight check.
///
/// See module-level docs for the per-variant stage assignment.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum PreflightError {
    /// Stage-1: manifest bytes were unreadable / not JSON / missing
    /// structurally required keys. `detail` is the underlying
    /// adapter-level error message verbatim (the adapter chooses the
    /// wording).
    #[error("manifest unreadable: {detail}")]
    Unreadable {
        /// Adapter-supplied detail (`serde_json` error, `io::Error`,
        /// etc.).
        detail: String,
    },

    /// Stage-1: `metadata.dbt_schema_version` below the dbt ≥1.8 floor.
    /// `found` is the verbatim version string from the manifest;
    /// `minimum` is a `&'static str` baked into the enum so the
    /// supported floor cannot drift between the error message and the
    /// adapter's check.
    #[error("dbt schema {found} is below minimum {minimum}")]
    SchemaUnsupported {
        /// Verbatim `metadata.dbt_schema_version` value.
        found: String,
        /// Minimum supported dbt schema version (compile-time constant).
        minimum: &'static str,
    },

    /// Stage-2: an in-scope unit test references a model whose
    /// `compiled_code` is `None`. The error names **both** the offending
    /// model node id and the unit-test name so the remediation message
    /// can tell the user exactly which `dbt compile`/`dbt run` to run.
    #[error("unit test `{unit_test}` references model `{node_id}` which has no compiled_code")]
    NotCompiled {
        /// Manifest node id of the uncompiled target model.
        node_id: String,
        /// Name of the unit test that referenced it.
        unit_test: String,
    },

    /// Stage-1: `--baseline-manifest` was supplied but the file could
    /// not be read or parsed. `detail` is the underlying error message
    /// verbatim.
    #[error("baseline manifest unusable: {detail}")]
    BaselineUnusable {
        /// Adapter-supplied detail.
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_display_includes_detail() {
        let err = PreflightError::Unreadable {
            detail: "expected ident at line 3 column 5".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "manifest unreadable: expected ident at line 3 column 5"
        );
    }

    #[test]
    fn schema_unsupported_display_includes_both_versions() {
        let err = PreflightError::SchemaUnsupported {
            found: "v6".to_owned(),
            minimum: "v12",
        };
        assert_eq!(err.to_string(), "dbt schema v6 is below minimum v12");
    }

    #[test]
    fn not_compiled_display_names_both_ids() {
        let err = PreflightError::NotCompiled {
            node_id: "model.shop.stg_orders".to_owned(),
            unit_test: "test_stg_orders_dedup".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "unit test `test_stg_orders_dedup` references model \
             `model.shop.stg_orders` which has no compiled_code"
        );
    }

    #[test]
    fn baseline_unusable_display_includes_detail() {
        let err = PreflightError::BaselineUnusable {
            detail: "no such file".to_owned(),
        };
        assert_eq!(err.to_string(), "baseline manifest unusable: no such file");
    }

    /// Pattern-match every variant — this test breaks the build when a
    /// fifth variant is added without an explicit decision. The match
    /// arms are deliberately exhaustive (no `_` fallback): adding a
    /// variant inside this crate surfaces here as a non-exhaustive
    /// match error, which is exactly the build-break the test exists
    /// to provoke.
    #[test]
    fn the_four_locked_variants_are_pattern_matchable() {
        let errors = [
            PreflightError::Unreadable {
                detail: String::new(),
            },
            PreflightError::SchemaUnsupported {
                found: String::new(),
                minimum: "v12",
            },
            PreflightError::NotCompiled {
                node_id: String::new(),
                unit_test: String::new(),
            },
            PreflightError::BaselineUnusable {
                detail: String::new(),
            },
        ];
        let mut seen = [false; 4];
        for err in &errors {
            // No `_` arm: `#[non_exhaustive]` does not require exhaustive
            // arms in the **defining** crate; clippy::pedantic flags an
            // `_` arm as unreachable. Adding a fifth variant inside this
            // crate will surface here as a non-exhaustive match error —
            // exactly the build-break this test exists to provoke.
            match err {
                PreflightError::Unreadable { .. } => seen[0] = true,
                PreflightError::SchemaUnsupported { .. } => seen[1] = true,
                PreflightError::NotCompiled { .. } => seen[2] = true,
                PreflightError::BaselineUnusable { .. } => seen[3] = true,
            }
        }
        assert!(seen.iter().all(|&hit| hit));
    }

    #[test]
    fn errors_implement_std_error() {
        // Walking the source chain is a real consumer use case — make
        // sure the trait impl actually compiles.
        fn assert_error<E: std::error::Error>(_: &E) {}
        assert_error(&PreflightError::Unreadable {
            detail: String::new(),
        });
    }
}
