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
//!   model has `compiled_code: null`. `unit_test` is `Some(name)` when the
//!   model was in scope because of a specific unit test; `None` when the
//!   model was in scope as a modified model with zero unit tests targeting
//!   it (explorer mode, PR C / #30). Raised by the preflight pass in the
//!   run loop **after** `StateComparator` selects the in-scope model set.
//!   The remediation message lives in the `cli` exit-code mapping per
//!   ADR-2.
//! - [`PreflightError::BaselineUnusable`] — Stage-1 (adapter): the
//!   `--baseline-manifest` was supplied (it is required per the locked
//!   policy) but could not be read or did not parse against the schema.
//!
//! `#[non_exhaustive]` per the enums-yes-structs-no rule: consumers
//! pattern-match this enum and future fail-closed reasons are additive.
//! Pairing with `thiserror::Error` gives a `Display` impl with
//! deterministic remediation-friendly wording.

use thiserror::Error;

use crate::domain::manifest::Manifest;
use crate::domain::state::{InScopeSet, ModelInScopeSet, resolve_tested_model};

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

    /// Stage-2: an in-scope model has `compiled_code: null`.
    ///
    /// `unit_test` is `Some(name)` when the model was in scope because
    /// a specific unit test targets it; `None` when the model was in
    /// scope as a modified model with zero unit tests (explorer mode).
    /// The Display impl produces variant wording for each shape.
    #[error("{}", NotCompiled::display_for(node_id, unit_test.as_deref()))]
    NotCompiled {
        /// Manifest node id of the uncompiled model.
        node_id: String,
        /// Name of the unit test that referenced it, or `None` when the
        /// model is in scope as a modified-with-zero-tests model.
        unit_test: Option<String>,
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

/// Display helper for `PreflightError::NotCompiled`.
///
/// Extracted as an associated function so the `#[error("...")]` attribute
/// can delegate to it without the noise of a custom `Display` impl on
/// the whole enum. Not part of the public API.
struct NotCompiled;

impl NotCompiled {
    fn display_for(node_id: &str, unit_test: Option<&str>) -> String {
        match unit_test {
            Some(name) => format!(
                "unit test `{name}` references model `{node_id}` which has no compiled_code"
            ),
            None => format!(
                "modified model `{node_id}` has no compiled_code; run `dbt compile` or `dbt run`"
            ),
        }
    }
}

/// Stage-2 of the two-stage fail-closed contract (ADR-2): verify every
/// in-scope model carries compiled SQL.
///
/// **Explorer-mode widening (PR C / #30)**: the check now iterates over
/// `models_in_scope` — the full union of models targeted by in-scope unit
/// tests plus modified models with zero unit tests — rather than
/// iterating unit tests individually. For each in-scope model, if
/// `compiled_code` is `None`, the run fails closed. The reported
/// `unit_test` field is:
///
/// - `Some(name)` — an in-scope unit test (`in_scope`) targets the model.
///   If multiple tests target the same model, the lexicographically first
///   test id's name is reported (deterministic).
/// - `None` — no in-scope unit test targets the model; the model was in
///   scope via the modified-with-zero-tests arm.
///
/// Runs in the run loop **after** `StateComparator` selects the in-scope
/// model set. Only in-scope models are inspected: an out-of-scope model
/// with no compiled SQL is not a fail condition.
///
/// The first offender in deterministic [`ModelInScopeSet`] (`BTreeSet` over
/// `NodeId`) order is reported.
///
/// # Errors
///
/// [`PreflightError::NotCompiled`] — the first in-scope model with
/// `compiled_code: None`.
pub fn preflight_compiled(
    current: &Manifest,
    in_scope: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
) -> Result<(), PreflightError> {
    for model_id in models_in_scope.iter() {
        // Resolve the model node by its full id. If the node is not
        // found, skip: the scope selection guarantees it came from a
        // manifest node, but be defensive.
        let Some(model) = current.node(model_id) else {
            continue;
        };
        if model.compiled_code().is_some() {
            continue;
        }
        // Model is uncompiled. Find the first in-scope unit test that
        // targets this model (lexicographic by test id, deterministic).
        let unit_test_name = first_in_scope_test_for_model(current, in_scope, model_id);
        return Err(PreflightError::NotCompiled {
            node_id: model_id.as_str().to_owned(),
            unit_test: unit_test_name,
        });
    }
    Ok(())
}

/// Return the name of the lexicographically first in-scope unit test that
/// resolves to `model_id`, or `None` if no in-scope test targets it.
fn first_in_scope_test_for_model(
    current: &Manifest,
    in_scope: &InScopeSet,
    model_id: &crate::domain::manifest::NodeId,
) -> Option<String> {
    // `in_scope` is backed by a BTreeSet, so `.iter()` is already sorted.
    for test_id in in_scope.iter() {
        let Some(unit_test) = current.unit_test(test_id) else {
            continue;
        };
        if let Some(resolved) = resolve_tested_model(current, unit_test)
            && resolved.id() == model_id
        {
            return Some(unit_test.name().to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{
        Checksum, DependsOn, ManifestMetadata, Node, NodeConfig, NodeId,
    };
    use crate::domain::state::ModelInScopeSet;
    use crate::domain::unit_test::{UnitTest, UnitTestExpect};
    use std::collections::BTreeMap;
    use std::collections::HashMap;

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
    fn not_compiled_display_with_unit_test_names_both_ids() {
        let err = PreflightError::NotCompiled {
            node_id: "model.shop.stg_orders".to_owned(),
            unit_test: Some("test_stg_orders_dedup".to_owned()),
        };
        assert_eq!(
            err.to_string(),
            "unit test `test_stg_orders_dedup` references model \
             `model.shop.stg_orders` which has no compiled_code"
        );
    }

    #[test]
    fn not_compiled_display_without_unit_test_names_model_only() {
        // The None shape must not contain "unit test" — the fail_closed.feature
        // scenario "stderr does not name a unit test" asserts this.
        let err = PreflightError::NotCompiled {
            node_id: "model.shop.stg_orders".to_owned(),
            unit_test: None,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("model.shop.stg_orders"),
            "names the node: {msg}"
        );
        assert!(
            !msg.contains("unit test"),
            "must not mention unit test: {msg}"
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
                unit_test: None,
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

    // ===== preflight_compiled (Stage-2) =====

    /// A `model` node with the given full id; `compiled` is its
    /// `compiled_code` (`None` models the `dbt parse` case).
    fn model(full_id: &str, compiled: Option<&str>) -> Node {
        Node::new(
            NodeId::new(full_id),
            "model",
            Checksum::new("sha256", "c"),
            compiled.map(str::to_owned),
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    /// A unit test named `name` targeting the bare model name `model_bare`.
    fn unit_test_for(name: &str, model_bare: &str) -> UnitTest {
        UnitTest::new(
            name,
            NodeId::new(model_bare),
            Vec::new(),
            UnitTestExpect::new(serde_json::Value::Null, None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
    }

    fn manifest(nodes: Vec<Node>, unit_tests: Vec<(&str, UnitTest)>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            unit_tests
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            HashMap::new(),
        )
    }

    /// Helper to build an `InScopeSet` + `ModelInScopeSet` from explicit
    /// test-id and model-id strings. Used for tests that control both sets
    /// independently of `StateComparator` to isolate preflight logic.
    fn scope(test_ids: &[&str], model_ids: &[&str]) -> (InScopeSet, ModelInScopeSet) {
        let in_scope = test_ids.iter().map(|s| (*s).to_string()).collect();
        let models = model_ids.iter().map(|s| NodeId::new(*s)).collect();
        (in_scope, models)
    }

    #[test]
    fn an_empty_models_in_scope_passes() {
        let m = manifest(vec![], vec![]);
        let (in_scope, models) = scope(&[], &[]);
        assert!(preflight_compiled(&m, &in_scope, &models).is_ok());
    }

    #[test]
    fn an_in_scope_model_with_compiled_sql_passes() {
        let m = manifest(
            vec![model("model.shop.stg_orders", Some("select 1"))],
            vec![("unit_test.shop.t", unit_test_for("t", "stg_orders"))],
        );
        let (in_scope, models) = scope(&["unit_test.shop.t"], &["model.shop.stg_orders"]);
        assert!(preflight_compiled(&m, &in_scope, &models).is_ok());
    }

    #[test]
    fn an_in_scope_model_lacking_compiled_sql_fails_closed_with_unit_test_name() {
        // The model was in scope via an in-scope unit test — unit_test
        // should be Some(name).
        let m = manifest(
            vec![model("model.shop.stg_orders", None)],
            vec![(
                "unit_test.shop.t",
                unit_test_for("test_dedup", "stg_orders"),
            )],
        );
        let (in_scope, models) = scope(&["unit_test.shop.t"], &["model.shop.stg_orders"]);
        match preflight_compiled(&m, &in_scope, &models) {
            Err(PreflightError::NotCompiled { node_id, unit_test }) => {
                assert_eq!(node_id, "model.shop.stg_orders");
                assert_eq!(unit_test, Some("test_dedup".to_owned()));
            }
            other => panic!("expected NotCompiled, got {other:?}"),
        }
    }

    #[test]
    fn a_modified_model_with_zero_unit_tests_and_no_compiled_sql_fails_closed() {
        // Explorer-mode case: the model is in models_in_scope but there
        // is no in-scope unit test targeting it. unit_test must be None.
        let m = manifest(
            vec![model("model.shop.stg_orders", None)],
            vec![], // zero unit tests
        );
        let (in_scope, models) = scope(
            &[], // no unit tests in scope
            &["model.shop.stg_orders"],
        );
        match preflight_compiled(&m, &in_scope, &models) {
            Err(PreflightError::NotCompiled { node_id, unit_test }) => {
                assert_eq!(node_id, "model.shop.stg_orders");
                assert!(
                    unit_test.is_none(),
                    "unit_test must be None for zero-test case"
                );
            }
            other => panic!("expected NotCompiled, got {other:?}"),
        }
    }

    #[test]
    fn a_modified_model_with_zero_unit_tests_and_compiled_sql_passes() {
        // Explorer-mode: a modified model in models_in_scope with
        // compiled_code present does NOT fail closed.
        let m = manifest(
            vec![model("model.shop.stg_orders", Some("select 1"))],
            vec![],
        );
        let (in_scope, models) = scope(&[], &["model.shop.stg_orders"]);
        assert!(preflight_compiled(&m, &in_scope, &models).is_ok());
    }

    #[test]
    fn an_out_of_scope_uncompiled_model_does_not_trigger_fail_closed() {
        // The two-stage split's whole point: `other` has no compiled SQL
        // but is NOT in models_in_scope. Only stg_orders is in scope and
        // it IS compiled.
        let m = manifest(
            vec![
                model("model.shop.stg_orders", Some("select 1")),
                model("model.shop.other", None),
            ],
            vec![
                ("unit_test.shop.t", unit_test_for("t", "stg_orders")),
                ("unit_test.shop.other_t", unit_test_for("other_t", "other")),
            ],
        );
        // Only stg_orders in models_in_scope; `other` is out of scope.
        let (in_scope, models) = scope(&["unit_test.shop.t"], &["model.shop.stg_orders"]);
        assert!(preflight_compiled(&m, &in_scope, &models).is_ok());
    }

    #[test]
    fn a_model_absent_from_nodes_in_models_in_scope_is_skipped() {
        // Defensive: a model id in models_in_scope that is not a key of
        // `current.nodes()` is skipped — no panic, no failure.
        let m = manifest(
            vec![model("model.shop.real", None)],
            vec![("unit_test.shop.b_real", unit_test_for("real_t", "real"))],
        );
        // ghost is in models_in_scope but missing from nodes; real
        // is uncompiled and in scope.
        let (in_scope, models) = scope(
            &["unit_test.shop.b_real"],
            &["model.shop.a_ghost", "model.shop.real"],
        );
        match preflight_compiled(&m, &in_scope, &models) {
            Err(PreflightError::NotCompiled { node_id, .. }) => {
                assert_eq!(node_id, "model.shop.real");
            }
            other => panic!("expected NotCompiled, got {other:?}"),
        }
    }

    #[test]
    fn the_first_offender_in_model_id_order_is_reported() {
        // Two in-scope models (both uncompiled). The offender reported is
        // the one whose NodeId sorts first (BTreeSet / lexicographic order).
        let m = manifest(
            vec![model("model.shop.aaa", None), model("model.shop.zzz", None)],
            vec![
                ("unit_test.shop.a", unit_test_for("a_t", "aaa")),
                ("unit_test.shop.z", unit_test_for("z_t", "zzz")),
            ],
        );
        let (in_scope, models) = scope(
            &["unit_test.shop.a", "unit_test.shop.z"],
            &["model.shop.aaa", "model.shop.zzz"],
        );
        match preflight_compiled(&m, &in_scope, &models) {
            Err(PreflightError::NotCompiled { node_id, .. }) => {
                assert_eq!(node_id, "model.shop.aaa", "first in model-id order wins");
            }
            other => panic!("expected NotCompiled, got {other:?}"),
        }
    }
}
