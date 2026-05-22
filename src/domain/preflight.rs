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

use crate::domain::manifest::Manifest;
use crate::domain::state::{InScopeSet, resolve_target_model};

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

/// Stage-2 of the two-stage fail-closed contract (ADR-2): verify every
/// in-scope unit test's target model carries compiled SQL.
///
/// Runs in the run loop **after** `StateComparator` selects the in-scope
/// set. For each in-scope unit test the target model is resolved from
/// its bare `model:` name; if that model is present and its
/// `compiled_code` is `None`, the manifest came from `dbt parse` (not
/// `dbt compile` / `dbt run`) and cannot be honestly visualised — the
/// run loop fails closed before any HTML is written.
///
/// Only in-scope models are inspected: an out-of-scope model with no
/// compiled SQL is not a fail condition (it is never rendered). That is
/// what makes the two-stage split worthwhile — a manifest valid for the
/// diff-scoped subset is not rejected over an irrelevant uncompiled
/// node.
///
/// An in-scope unit test whose target model cannot be resolved at all
/// (absent from `nodes`) is **skipped**, not failed: the in-scope
/// selection can place a unit test in scope via its own definition
/// change while the target model is missing from the manifest. Stage-2
/// inspects compiled-SQL presence *on a model*, and there is no model
/// to inspect — so it is not a Stage-2 concern, and deliberately not a
/// fifth [`PreflightError`] variant.
///
/// The first offender in deterministic [`InScopeSet`] order is reported.
///
/// # Errors
///
/// [`PreflightError::NotCompiled`] — the first in-scope unit test whose
/// resolvable target model has `compiled_code: None`.
pub fn preflight_compiled(current: &Manifest, in_scope: &InScopeSet) -> Result<(), PreflightError> {
    for unit_test_id in in_scope.iter() {
        let Some(unit_test) = current.unit_test(unit_test_id) else {
            // An in-scope id always keys `current.unit_tests()` — that
            // is where the in-scope selection draws it from. The guard
            // keeps the function total without relying on that.
            continue;
        };
        let Some(model) = resolve_target_model(current, unit_test.model()) else {
            // Unresolved target model — skip (see the doc comment).
            continue;
        };
        if model.compiled_code().is_none() {
            return Err(PreflightError::NotCompiled {
                node_id: model.id().as_str().to_owned(),
                unit_test: unit_test.name().to_owned(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{Checksum, DependsOn, ManifestMetadata, Node, NodeId};
    use crate::domain::unit_test::{UnitTest, UnitTestExpect};
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

    // ===== preflight_compiled (Stage-2) =====

    /// A `model` node with the given full id; `compiled` is its
    /// `compiled_code` (`None` models the `dbt parse` case).
    fn model(full_id: &str, compiled: Option<&str>) -> Node {
        Node::new(
            NodeId::new(full_id),
            "model",
            Checksum::new("sha256", "c"),
            compiled.map(str::to_owned),
            DependsOn::default(),
        )
    }

    /// A unit test named `name` targeting the bare model name `model_bare`.
    fn unit_test_for(name: &str, model_bare: &str) -> UnitTest {
        UnitTest::new(
            name,
            NodeId::new(model_bare),
            Vec::new(),
            UnitTestExpect::new(serde_json::Value::Null, None),
            None,
            DependsOn::default(),
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

    #[test]
    fn an_empty_in_scope_set_passes() {
        let m = manifest(vec![], vec![]);
        assert!(preflight_compiled(&m, &InScopeSet::new()).is_ok());
    }

    #[test]
    fn an_in_scope_test_whose_model_has_compiled_sql_passes() {
        let m = manifest(
            vec![model("model.shop.stg_orders", Some("select 1"))],
            vec![("unit_test.shop.t", unit_test_for("t", "stg_orders"))],
        );
        let in_scope = InScopeSet::from_iter(["unit_test.shop.t".to_owned()]);
        assert!(preflight_compiled(&m, &in_scope).is_ok());
    }

    #[test]
    fn an_in_scope_test_whose_model_lacks_compiled_sql_fails_closed() {
        let m = manifest(
            vec![model("model.shop.stg_orders", None)],
            vec![(
                "unit_test.shop.t",
                unit_test_for("test_dedup", "stg_orders"),
            )],
        );
        let in_scope = InScopeSet::from_iter(["unit_test.shop.t".to_owned()]);
        match preflight_compiled(&m, &in_scope) {
            Err(PreflightError::NotCompiled { node_id, unit_test }) => {
                assert_eq!(node_id, "model.shop.stg_orders");
                assert_eq!(unit_test, "test_dedup");
            }
            other => panic!("expected NotCompiled, got {other:?}"),
        }
    }

    #[test]
    fn an_out_of_scope_uncompiled_model_does_not_trigger_fail_closed() {
        // The two-stage split's whole point: `other` has no compiled SQL
        // but is out of scope, so Stage-2 ignores it. The in-scope test
        // targets `stg_orders`, which IS compiled.
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
        // Only stg_orders' test is in scope; `other_t` is not.
        let in_scope = InScopeSet::from_iter(["unit_test.shop.t".to_owned()]);
        assert!(preflight_compiled(&m, &in_scope).is_ok());
    }

    #[test]
    fn an_in_scope_test_with_an_unresolvable_target_is_skipped() {
        // The finding routed to PR 6: the in-scope selection can place a
        // unit test in scope via its own definition change while the
        // target model is absent from `nodes`. Stage-2 skips it — no
        // model to inspect — rather than failing closed or adding a
        // fifth PreflightError variant.
        let m = manifest(
            vec![],
            vec![(
                "unit_test.shop.ghost",
                unit_test_for("ghost", "missing_model"),
            )],
        );
        let in_scope = InScopeSet::from_iter(["unit_test.shop.ghost".to_owned()]);
        assert!(preflight_compiled(&m, &in_scope).is_ok());
    }

    #[test]
    fn an_unresolved_target_does_not_short_circuit_a_later_offender() {
        // Ordering kill: the unresolved-target test sorts BEFORE the
        // uncompiled-target test in InScopeSet (BTreeSet) order. The
        // `continue` after an unresolved target must keep iterating, not
        // return Ok — so the later uncompiled model is still caught.
        let m = manifest(
            vec![model("model.shop.real", None)],
            vec![
                (
                    "unit_test.shop.a_ghost",
                    unit_test_for("ghost", "missing_model"),
                ),
                ("unit_test.shop.b_real", unit_test_for("real_t", "real")),
            ],
        );
        let in_scope = InScopeSet::from_iter([
            "unit_test.shop.a_ghost".to_owned(),
            "unit_test.shop.b_real".to_owned(),
        ]);
        match preflight_compiled(&m, &in_scope) {
            Err(PreflightError::NotCompiled { node_id, .. }) => {
                assert_eq!(node_id, "model.shop.real");
            }
            other => panic!("expected NotCompiled for the later test, got {other:?}"),
        }
    }

    #[test]
    fn an_in_scope_id_absent_from_the_unit_tests_map_is_skipped() {
        // Defensive: an in-scope id that is not a key of
        // `current.unit_tests()` is skipped. Ordered before a genuine
        // offender so a `continue -> return Ok` mutation is caught.
        let m = manifest(
            vec![model("model.shop.real", None)],
            vec![("unit_test.shop.b_real", unit_test_for("real_t", "real"))],
        );
        let in_scope = InScopeSet::from_iter([
            "unit_test.shop.a_absent".to_owned(),
            "unit_test.shop.b_real".to_owned(),
        ]);
        match preflight_compiled(&m, &in_scope) {
            Err(PreflightError::NotCompiled { node_id, .. }) => {
                assert_eq!(node_id, "model.shop.real");
            }
            other => panic!("expected NotCompiled, got {other:?}"),
        }
    }

    #[test]
    fn the_first_offender_in_in_scope_order_is_reported() {
        // Two in-scope tests, both targeting uncompiled models. The
        // offender reported is the one whose in-scope id sorts first
        // (BTreeSet order) — deterministic regardless of HashMap order.
        let m = manifest(
            vec![model("model.shop.aaa", None), model("model.shop.zzz", None)],
            vec![
                ("unit_test.shop.a", unit_test_for("a_t", "aaa")),
                ("unit_test.shop.z", unit_test_for("z_t", "zzz")),
            ],
        );
        let in_scope =
            InScopeSet::from_iter(["unit_test.shop.a".to_owned(), "unit_test.shop.z".to_owned()]);
        match preflight_compiled(&m, &in_scope) {
            Err(PreflightError::NotCompiled { unit_test, .. }) => {
                assert_eq!(unit_test, "a_t", "the first in-scope id's test wins");
            }
            other => panic!("expected NotCompiled, got {other:?}"),
        }
    }
}
