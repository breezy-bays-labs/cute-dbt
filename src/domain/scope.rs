//! Scope-selection (cute-dbt#81 — Shape E Phase 1).
//!
//! Defines the in-scope set the run loop (PR-pdiff-1b) renders, from one
//! of two sources (resolved at CLI parse time):
//!
//! - [`ScopeInput::Baseline`] — the v0.1 `--baseline-manifest` path.
//!   Delegates to [`StateComparator::from_selectors`] — the always-on
//!   body checksum plus any opt-in `state:modified` sub-selectors
//!   (cute-dbt#160); with no sub-selectors the existing dbt
//!   `state:modified.body` semantics flow through unchanged.
//! - [`ScopeInput::PrDiff`] — the `--pr-diff` path (cute-dbt#85 renamed
//!   from `--scope-from-pr-diff` at cute-dbt#96). Carries a
//!   [`NormalizedDiffIndex`] built once from the parsed
//!   `git diff --unified=0`; the index is the single normalization
//!   authority that matches changed-file paths against
//!   [`crate::domain::manifest::Node::original_file_path`] and
//!   [`crate::domain::unit_test::UnitTest::original_file_path`].
//!
//! Two scope sources is a deliberate ADR-1 judgment call: free function
//! over trait until a third source arrives (a v0.2+ refactor moment).
//!
//! Path normalization lives in the [`crate::domain::path`] leaf and is
//! owned end-to-end by [`NormalizedDiffIndex`] (module DAG
//! `scope → pr_diff → path`) — `scope` no longer normalizes paths
//! directly, so the diff-side keyset and the declaring-side lookup
//! cannot diverge. Git-detected renames (cute-dbt#80) are handled inside
//! the index: both sides of every `rename from`/`rename to` pair join
//! the changed-file keyset, so a **pure** rename (which carries no
//! `+++` header and no hunks) still scopes the current node at its new
//! path — no scope-level code is rename-aware.

use std::collections::{BTreeSet, HashMap};

use crate::domain::manifest::{Manifest, NodeId};
use crate::domain::pr_diff::NormalizedDiffIndex;
use crate::domain::state::{
    InScopeSet, ModelInScopeSet, ModifierKind, StateComparator, resolve_tested_model,
};

/// Source of the in-scope set: either a baseline manifest (dbt
/// `state:modified` semantics) or a parsed PR diff (CI/PR-review path).
#[derive(Debug, Clone)]
pub enum ScopeInput {
    /// Compare against a baseline manifest — v0.1 default, ADR-2 +
    /// ADR-3 semantics unchanged.
    Baseline {
        /// Already-parsed baseline manifest (Stage-1 pre-flight ran in
        /// the adapter). Boxed (`clippy::large_enum_variant`): a parsed
        /// `Manifest` is hundreds of bytes inline and grows with every
        /// ingestion wave (cute-dbt#256 added exposures/groups maps);
        /// the `PrDiff` arm is ~48 bytes — boxing keeps the enum small
        /// where it is moved through the run loop.
        manifest: Box<Manifest>,
        /// Opt-in `state:modified` sub-selector kinds composed alongside
        /// the always-on body checksum (cute-dbt#160 — the CLI
        /// `--modified-selectors` wiring). Empty is the body-only v0.1
        /// default, byte-identical to the pre-flag behavior.
        sub_selectors: Vec<ModifierKind>,
    },
    /// Scope to nodes whose `original_file_path` appears in the PR's
    /// parsed diff. CI/PR-review path — no baseline needed.
    PrDiff {
        /// The single normalization authority, built once from the
        /// parsed `git diff --unified=0` and the `--project-root` strip.
        /// Owns the changed-file keyset (diff-side, strip-applied) and
        /// the per-file hunks (consumed by cute-dbt#96's block-precise
        /// `changed` refinement and inline YAML diff). Typically the
        /// diff includes non-dbt files (README, workflow YAML,
        /// `dbt_project.yml`) which silently miss.
        index: NormalizedDiffIndex,
    },
}

/// The resolved scope selection: the in-scope unit tests, the in-scope
/// models, and the **changed** (PR-updated) subset of the in-scope tests.
///
/// `changed` is the per-test "this PR updated this test" signal the report
/// foregrounds (cute-dbt#91). It is a strict subset of `in_scope`
/// (`changed ⊆ in_scope`) by construction in both arms:
///
/// - **`Baseline`** — `changed` is [`StateComparator::changed_unit_tests`]
///   (the precise `UnitTest` struct diff); a changed test is always in
///   scope via the `target_modified || test_changed` union. The changed
///   subset is modifier-independent: opt-in sub-selectors widen
///   `in_scope`, never `changed`.
/// - **`PrDiff`** — `changed` is the tests whose declaring YAML file
///   appears in the diff (file-granular here; cute-dbt#96 refines it to
///   block-precise as a post-scope run-loop narrowing). Collected in the
///   same traversal as `in_scope`, so the subset relation cannot drift.
///
/// Additive POD (ADR-5): the existing `InScopeSet` / `ModelInScopeSet`
/// types and their semantics are unchanged — this struct only *surfaces*
/// the label both arms already compute.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopeSelection {
    /// Unit-test ids the report renders (selection semantics unchanged).
    pub in_scope: InScopeSet,
    /// Model node ids the report renders (explorer-mode union, unchanged).
    pub models_in_scope: ModelInScopeSet,
    /// The subset of `in_scope` whose definition this diff updated — the
    /// report's "updated" tests (the rest are "context").
    pub changed: InScopeSet,
}

/// Resolve the [`ScopeSelection`] for the current manifest and the given
/// [`ScopeInput`].
///
/// - [`ScopeInput::Baseline`] delegates to
///   [`StateComparator::from_selectors`] (the body checksum plus any
///   opt-in `sub_selectors` — empty is the body-only default) for the
///   in-scope/model sets and to
///   [`StateComparator::changed_unit_tests`] for the changed subset.
/// - [`ScopeInput::PrDiff`] matches changed-file paths against
///   `original_file_path` via the [`NormalizedDiffIndex`], collecting the
///   in-scope and changed sets in one pass. The `PrDiff` arm never
///   constructs a [`StateComparator`], so sub-selectors are structurally
///   meaningless here — the CLI rejects `--modified-selectors` with
///   `--pr-diff` at parse time (cute-dbt#160).
#[must_use]
pub fn select_in_scope(current: &Manifest, input: &ScopeInput) -> ScopeSelection {
    match input {
        ScopeInput::Baseline {
            manifest: baseline,
            sub_selectors,
        } => {
            let cmp = StateComparator::from_selectors(sub_selectors);
            ScopeSelection {
                in_scope: cmp.in_scope_unit_tests(current, baseline),
                models_in_scope: cmp.models_in_scope(current, baseline),
                changed: StateComparator::changed_unit_tests(current, baseline),
            }
        }
        ScopeInput::PrDiff { index } => select_in_scope_pr_diff(current, index),
    }
}

/// The full-manifest model scope (cute-dbt#100 — the `explore` verb's
/// `all_models` seam): every `model` node in the manifest, no baseline,
/// no diff.
///
/// Non-model resource types (`test`, `seed`, `snapshot`, …) are
/// excluded — the same `resource_type == "model"` filter both
/// diff-scoping arms apply (cute-dbt#167), so a generic test node can
/// never surface as a model card on the explore pages either.
/// Compiled-ness is deliberately **not** consulted here: explore is
/// fail-open on uncompiled models (they render as "not compiled"), so
/// the seam returns them like any other model. The returned
/// [`ModelInScopeSet`] iterates in deterministic node-id order
/// (`BTreeSet`).
#[must_use]
pub fn all_models(current: &Manifest) -> ModelInScopeSet {
    current
        .nodes()
        .iter()
        .filter(|(_, node)| node.resource_type() == "model")
        .map(|(id, _)| id.clone())
        .collect()
}

/// The models whose source files a PR diff changed — the `explore`
/// verb's **change-context** seam (cute-dbt#106).
///
/// Matches each `model` node's `original_file_path` against the
/// [`NormalizedDiffIndex`] changed-file keyset — the exact cute-dbt#81
/// matching the report's `PrDiff` arm applies (the private
/// `select_in_scope_pr_diff` consumes this same function, so the two
/// verbs cannot disagree about which models a diff touched). Git renames
/// (cute-dbt#80) are handled inside the index: both sides of every
/// rename pair join the keyset, so a **pure** rename still marks the
/// current node at its new path. Non-`model` resource types never mark
/// (the cute-dbt#167 filter).
///
/// Change context **never narrows scope**: explore renders the full
/// manifest regardless; this set only decorates the changed nodes with
/// the "changed" context treatment.
#[must_use]
pub fn changed_models(current: &Manifest, index: &NormalizedDiffIndex) -> ModelInScopeSet {
    current
        .nodes()
        .iter()
        .filter(|(_, node)| node.resource_type() == "model")
        .filter(|(_, node)| {
            node.original_file_path()
                .is_some_and(|ofp| index.contains_changed(ofp))
        })
        .map(|(id, _)| id.clone())
        .collect()
}

// ---------------------------------------------------------------------
// PrDiff arm
// ---------------------------------------------------------------------

fn select_in_scope_pr_diff(current: &Manifest, index: &NormalizedDiffIndex) -> ScopeSelection {
    // Identify path-modified models — the PrDiff analog of the baseline
    // `modified_set`. Only `model` nodes participate (other resource
    // types do not host unit tests in v0.1). The index owns the
    // changed-file keyset, so this consults it rather than normalizing
    // paths here (single normalization authority). Shared with the
    // explore verb's change context (cute-dbt#106) via [`changed_models`]
    // — one matching authority for both verbs.
    let path_modified_models = changed_models(current, index);

    // In-scope unit tests + the changed subset, in ONE traversal so
    // `changed ⊆ in_scope` holds by construction (cute-dbt#91). A test is
    // in scope when its target model is path-modified OR its own
    // `original_file_path` (the declaring YAML file) is in the change set
    // (the dbt OR-semantics of the baseline path). It is *changed* when
    // that declaring YAML appears in the diff — file-granular here (a
    // changed multi-test YAML marks every test it declares; cute-dbt#96
    // narrows this to block-precise via diff-hunk overlap in a post-scope
    // run-loop step, leaving `changed ⊆ in_scope` intact).
    let mut in_scope_ids: Vec<String> = Vec::new();
    let mut changed_ids: Vec<String> = Vec::new();
    for (test_id, ut) in current.unit_tests() {
        let target_path_modified = resolve_tested_model(current, ut)
            .is_some_and(|model| path_modified_models.contains(model.id()));
        let test_yaml_changed = ut
            .original_file_path()
            .is_some_and(|p| index.contains_changed(p));
        if test_yaml_changed {
            changed_ids.push(test_id.clone());
        }
        if target_path_modified || test_yaml_changed {
            in_scope_ids.push(test_id.clone());
        }
    }
    let in_scope: InScopeSet = in_scope_ids.into_iter().collect();
    let changed: InScopeSet = changed_ids.into_iter().collect();

    // Models in scope — explorer-mode union:
    //   Arm 1: every model resolved from an in-scope unit test (so the
    //          renderer has the model context for the test).
    //   Arm 2: every path-modified model with zero unit tests targeting
    //          it (the "no tests wired" explorer signal).
    let tests_per_model: HashMap<NodeId, usize> = current
        .unit_tests()
        .values()
        .filter_map(|ut| resolve_tested_model(current, ut).map(|m| m.id().clone()))
        .fold(HashMap::new(), |mut acc, id| {
            *acc.entry(id).or_insert(0) += 1;
            acc
        });

    let mut model_ids: BTreeSet<NodeId> = BTreeSet::new();
    for test_id in in_scope.iter() {
        if let Some(ut) = current.unit_test(test_id)
            && let Some(model) = resolve_tested_model(current, ut)
        {
            model_ids.insert(model.id().clone());
        }
    }
    for model_id in path_modified_models.iter() {
        let has_tests = tests_per_model.get(model_id).copied().unwrap_or(0) > 0;
        if !has_tests {
            model_ids.insert(model_id.clone());
        }
    }

    let models_in_scope: ModelInScopeSet = model_ids.into_iter().collect();
    ScopeSelection {
        in_scope,
        models_in_scope,
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{Checksum, DependsOn, ManifestMetadata, Node, NodeConfig};
    use crate::domain::pr_diff::{FileHunks, Hunk, PrDiff};
    use crate::domain::unit_test::{UnitTest, UnitTestExpect};
    use std::collections::{BTreeMap, HashMap};
    use std::path::Path;

    // ----- PrDiff test builders -----

    /// Build a file-granular [`PrDiff`] from changed-file paths (one
    /// minimal hunk each — block precision is exercised separately by the
    /// `pr_diff` overlap tests; here only the changed-file keyset matters).
    fn prdiff_from_paths(paths: &[&str]) -> PrDiff {
        PrDiff {
            renames: Vec::new(),
            files: paths
                .iter()
                .map(|p| FileHunks {
                    path: (*p).to_owned(),
                    hunks: vec![Hunk {
                        new_start: 1,
                        new_len: 1,
                        removed_lines: Vec::new(),
                        added_lines: Vec::new(),
                    }],
                })
                .collect(),
        }
    }

    /// A [`ScopeInput::PrDiff`] wrapping the index built from `paths` and
    /// an optional project-root strip.
    fn pr_diff_input(paths: &[&str], strip: Option<&Path>) -> ScopeInput {
        ScopeInput::PrDiff {
            index: NormalizedDiffIndex::new(&prdiff_from_paths(paths), strip),
        }
    }

    // ----- select_in_scope: Baseline arm -----

    #[test]
    fn baseline_arm_matches_state_comparator_body_only() {
        // Two-model manifest: one modified (checksum diff), one unchanged.
        let modified_id = NodeId::new("model.shop.dim_payers");
        let unchanged_id = NodeId::new("model.shop.stg_customers");
        let mut current_nodes = HashMap::new();
        current_nodes.insert(
            modified_id.clone(),
            model_node(&modified_id, "ck-current", None),
        );
        current_nodes.insert(
            unchanged_id.clone(),
            model_node(&unchanged_id, "ck-same", None),
        );

        let mut baseline_nodes = HashMap::new();
        baseline_nodes.insert(
            modified_id.clone(),
            model_node(&modified_id, "ck-baseline", None),
        );
        baseline_nodes.insert(
            unchanged_id.clone(),
            model_node(&unchanged_id, "ck-same", None),
        );

        let test_id = "unit_test.shop.dim_payers.injects_unknown";
        let mut current_tests = HashMap::new();
        current_tests.insert(
            test_id.to_owned(),
            test_for("injects_unknown", "dim_payers"),
        );

        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            current_nodes,
            current_tests,
            HashMap::new(),
        );
        let baseline = Manifest::new(
            ManifestMetadata::new("v12"),
            baseline_nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = ScopeInput::Baseline {
            manifest: Box::new(baseline),
            sub_selectors: Vec::new(),
        };
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert!(in_scope.contains(test_id));
        assert!(models.contains(&modified_id));
        assert!(!models.contains(&unchanged_id));
    }

    #[test]
    fn baseline_arm_without_sub_selectors_keeps_a_config_only_change_out_of_scope() {
        // The byte-identical default (cute-dbt#160): no sub-selectors
        // opted in ⇒ a config-only change (identical body checksum) stays
        // out of scope, exactly as before the flag existed.
        let (current, baseline) = config_only_pair();
        let input = ScopeInput::Baseline {
            manifest: Box::new(baseline),
            sub_selectors: Vec::new(),
        };
        let selection = select_in_scope(&current, &input);
        assert!(selection.in_scope.is_empty());
        assert!(selection.models_in_scope.is_empty());
    }

    #[test]
    fn baseline_arm_with_configs_sub_selector_scopes_a_config_only_change() {
        // The opt-in widening (cute-dbt#160): the SAME config-only change
        // is in scope once `.configs` is composed into the comparator.
        let (current, baseline) = config_only_pair();
        let input = ScopeInput::Baseline {
            manifest: Box::new(baseline),
            sub_selectors: vec![ModifierKind::Configs],
        };
        let selection = select_in_scope(&current, &input);
        assert!(
            selection
                .in_scope
                .contains("unit_test.shop.dim_payers.injects_unknown"),
        );
        assert!(
            selection
                .models_in_scope
                .contains(&NodeId::new("model.shop.dim_payers")),
        );
        // The test definition itself is unchanged — sub-selectors widen
        // `in_scope`, never the `changed` subset (it stays a precise
        // UnitTest struct diff).
        assert!(selection.changed.is_empty());
    }

    /// A current/baseline pair where `dim_payers` differs ONLY in its
    /// resolved config (`materialized: table` vs `view`) — identical
    /// body checksum — and carries one unit test (identical in both).
    fn config_only_pair() -> (Manifest, Manifest) {
        let id = NodeId::new("model.shop.dim_payers");
        let test_id = "unit_test.shop.dim_payers.injects_unknown";

        let node_with = |materialized: &str| {
            let config: BTreeMap<String, serde_json::Value> = [(
                "materialized".to_owned(),
                serde_json::Value::from(materialized),
            )]
            .into_iter()
            .collect();
            Node::new(
                id.clone(),
                "model",
                checksum("ck-same"),
                Some("select 1".to_owned()),
                None,
                DependsOn::default(),
                None,
                NodeConfig::new(config, false),
                None,
                BTreeMap::new(),
            )
        };

        let manifest_with = |materialized: &str| {
            let mut nodes = HashMap::new();
            nodes.insert(id.clone(), node_with(materialized));
            let mut tests = HashMap::new();
            tests.insert(
                test_id.to_owned(),
                test_for("injects_unknown", "dim_payers"),
            );
            Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new())
        };

        (manifest_with("table"), manifest_with("view"))
    }

    #[test]
    fn baseline_arm_excludes_a_new_non_model_node_from_model_scope() {
        // Regression (cute-dbt#167, observed live on PR #166): a newly
        // added generic test node is `state:modified` (absent from the
        // baseline) and has zero unit tests targeting it, but it must NOT
        // surface as a model card in baseline mode.
        let stg_orders = NodeId::new("model.shop.stg_orders");
        let generic_test = NodeId::new("test.shop.not_null_stg_orders_id");

        let mut current_nodes = HashMap::new();
        current_nodes.insert(stg_orders.clone(), model_node(&stg_orders, "ck-same", None));
        current_nodes.insert(
            generic_test.clone(),
            typed_node(&generic_test, "test", "ck-new"),
        );

        let mut baseline_nodes = HashMap::new();
        baseline_nodes.insert(stg_orders.clone(), model_node(&stg_orders, "ck-same", None));

        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            current_nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let baseline = Manifest::new(
            ManifestMetadata::new("v12"),
            baseline_nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = ScopeInput::Baseline {
            manifest: Box::new(baseline),
            sub_selectors: Vec::new(),
        };
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert_eq!(in_scope.len(), 0);
        assert!(
            !models.contains(&generic_test),
            "a modified non-model node must not render as a model card",
        );
        assert_eq!(models.len(), 0);
    }

    // ----- select_in_scope: PrDiff arm -----

    #[test]
    fn pr_diff_arm_puts_modified_model_and_its_test_in_scope() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let stg_customers = NodeId::new("model.shop.stg_customers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        nodes.insert(
            stg_customers.clone(),
            model_node_with_path(&stg_customers, "ck2", "models/staging/stg_customers.sql"),
        );

        let test_id = "unit_test.shop.dim_payers.injects_unknown";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_for("injects_unknown", "dim_payers"),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input(&["models/marts/dim_payers.sql"], None);
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert!(in_scope.contains(test_id));
        assert!(models.contains(&dim_payers));
        assert!(!models.contains(&stg_customers));
    }

    #[test]
    fn pr_diff_arm_silently_skips_extraneous_paths() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(
            &[
                "README.md",
                ".github/workflows/ci.yml",
                "packages.yml",
                "dbt_project.yml",
                "models/deleted_model.sql",
            ],
            None,
        );
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert_eq!(in_scope.len(), 0);
        assert_eq!(models.len(), 0);
    }

    #[test]
    fn pr_diff_arm_picks_up_changed_unit_test_yaml() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );

        let test_id = "unit_test.shop.dim_payers.injects_unknown";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_with_path(
                "injects_unknown",
                "dim_payers",
                Some("models/marts/_core__models.yml"),
            ),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        // Only the YAML file changed — model SQL untouched.
        let input = pr_diff_input(&["models/marts/_core__models.yml"], None);
        let ScopeSelection { in_scope, .. } = select_in_scope(&current, &input);

        assert!(in_scope.contains(test_id));
    }

    #[test]
    fn pr_diff_arm_explorer_mode_for_modified_model_with_no_tests() {
        let stg_payments = NodeId::new("model.shop.stg_payments");
        let mut nodes = HashMap::new();
        nodes.insert(
            stg_payments.clone(),
            model_node_with_path(&stg_payments, "ck1", "models/staging/stg_payments.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(&["models/staging/stg_payments.sql"], None);
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert_eq!(in_scope.len(), 0);
        assert!(models.contains(&stg_payments));
    }

    #[test]
    fn pr_diff_arm_excludes_a_changed_non_model_node_from_model_scope() {
        // The PrDiff analog of the cute-dbt#167 baseline gap: a generic
        // test node whose declaring SQL file is in the diff must not
        // surface as a model card. Pins the existing `resource_type ==
        // "model"` filter in `select_in_scope_pr_diff` so the two arms
        // cannot drift apart.
        let generic_test = NodeId::new("test.shop.assert_positive_total");
        let mut nodes = HashMap::new();
        nodes.insert(
            generic_test.clone(),
            typed_node_with_path(
                &generic_test,
                "test",
                "ck1",
                "tests/assert_positive_total.sql",
            ),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(&["tests/assert_positive_total.sql"], None);
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert_eq!(in_scope.len(), 0);
        assert!(
            !models.contains(&generic_test),
            "a path-changed non-model node must not render as a model card",
        );
        assert_eq!(models.len(), 0);
    }

    // ----- select_in_scope: git renames (cute-dbt#80) -----

    /// A [`ScopeInput::PrDiff`] for a diff carrying rename pairs (and
    /// optionally plain changed files).
    fn pr_diff_input_with_renames(
        paths: &[&str],
        renames: &[(&str, &str)],
        strip: Option<&Path>,
    ) -> ScopeInput {
        let mut diff = prdiff_from_paths(paths);
        diff.renames = renames
            .iter()
            .map(|(f, t)| crate::domain::pr_diff::RenamePair {
                from: (*f).to_owned(),
                to: (*t).to_owned(),
            })
            .collect();
        ScopeInput::PrDiff {
            index: NormalizedDiffIndex::new(&diff, strip),
        }
    }

    #[test]
    fn pr_diff_arm_pure_rename_scopes_the_renamed_model_at_its_new_path() {
        // models/marts/dim_a.sql → models/marts/dim_b.sql, 100% similar:
        // the diff carries ONLY the rename pair (no file entry). The
        // current manifest (compiled at head) has the node at the NEW
        // path; it must scope, and its unit test is in scope as context
        // (the test's declaring YAML is untouched → not `changed`).
        let dim_b = NodeId::new("model.shop.dim_b");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_b.clone(),
            model_node_with_path(&dim_b, "ck1", "models/marts/dim_b.sql"),
        );

        let test_id = "unit_test.shop.dim_b.checks_rows";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_with_path("checks_rows", "dim_b", Some("models/marts/_models.yml")),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input_with_renames(
            &[],
            &[("models/marts/dim_a.sql", "models/marts/dim_b.sql")],
            None,
        );
        let selection = select_in_scope(&current, &input);

        assert!(
            selection.models_in_scope.contains(&dim_b),
            "the renamed model scopes under its NEW path",
        );
        assert!(selection.in_scope.contains(test_id));
        assert!(
            !selection.changed.contains(test_id),
            "a pure model rename does not mark the test's YAML changed",
        );
    }

    #[test]
    fn pr_diff_arm_rename_with_edit_scopes_the_model_once_not_twice() {
        // Rename + edit: the new path appears BOTH as a file entry (with
        // hunks) and as the rename `to`. The model must scope exactly
        // once, and nothing extra may enter the scope sets.
        let dim_b = NodeId::new("model.shop.dim_b");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_b.clone(),
            model_node_with_path(&dim_b, "ck1", "models/marts/dim_b.sql"),
        );

        let test_id = "unit_test.shop.dim_b.checks_rows";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_with_path("checks_rows", "dim_b", Some("models/marts/_models.yml")),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input_with_renames(
            &["models/marts/dim_b.sql"],
            &[("models/marts/dim_a.sql", "models/marts/dim_b.sql")],
            None,
        );
        let selection = select_in_scope(&current, &input);

        assert_eq!(
            selection.models_in_scope.len(),
            1,
            "the renamed-and-edited model scopes once, not twice",
        );
        assert!(selection.models_in_scope.contains(&dim_b));
        assert_eq!(selection.in_scope.len(), 1);
        assert!(selection.in_scope.contains(test_id));
    }

    #[test]
    fn pr_diff_arm_rename_old_path_matching_no_current_node_is_inert() {
        // The rename's old path maps to no current-manifest node (the
        // node moved). It must scope nothing — no phantom models, no
        // phantom tests.
        let unrelated = NodeId::new("model.shop.stg_x");
        let mut nodes = HashMap::new();
        nodes.insert(
            unrelated.clone(),
            model_node_with_path(&unrelated, "ck1", "models/staging/stg_x.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        // Neither rename side exists in the manifest (e.g. a non-dbt file
        // was renamed, or the manifest predates the new path).
        let input =
            pr_diff_input_with_renames(&[], &[("docs/old_readme.md", "docs/new_readme.md")], None);
        let selection = select_in_scope(&current, &input);

        assert_eq!(selection.in_scope.len(), 0);
        assert_eq!(selection.models_in_scope.len(), 0);
    }

    #[test]
    fn pr_diff_arm_pure_rename_of_declaring_yaml_marks_its_tests_in_scope() {
        // A purely renamed unit-test YAML: the test's current
        // original_file_path is the NEW path, which is in the rename
        // keyset → in scope AND file-granular `changed` (the post-scope
        // block-precise refinement then narrows it to context, since a
        // pure rename carries zero hunks — existing #96 machinery).
        let dim = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim.clone(),
            model_node_with_path(&dim, "ck1", "models/marts/dim_payers.sql"),
        );

        let test_id = "unit_test.shop.dim_payers.checks_rows";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_with_path(
                "checks_rows",
                "dim_payers",
                Some("models/marts/_renamed__models.yml"),
            ),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input_with_renames(
            &[],
            &[(
                "models/marts/_old__models.yml",
                "models/marts/_renamed__models.yml",
            )],
            None,
        );
        let selection = select_in_scope(&current, &input);

        assert!(selection.in_scope.contains(test_id));
        assert!(
            selection.changed.contains(test_id),
            "file-granular changed at scope level (refinement narrows later)",
        );
    }

    #[test]
    fn pr_diff_arm_rename_honors_project_root_strip_on_both_sides() {
        let dim_b = NodeId::new("model.shop.dim_b");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_b.clone(),
            model_node_with_path(&dim_b, "ck1", "models/marts/dim_b.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input_with_renames(
            &[],
            &[(
                "dbt_project/models/marts/dim_a.sql",
                "dbt_project/models/marts/dim_b.sql",
            )],
            Some(Path::new("dbt_project")),
        );
        let selection = select_in_scope(&current, &input);

        assert!(selection.models_in_scope.contains(&dim_b));
    }

    #[test]
    fn pr_diff_arm_honors_project_root_strip() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(
            &["dbt_project/models/marts/dim_payers.sql"],
            Some(Path::new("dbt_project")),
        );
        let ScopeSelection {
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert!(models.contains(&dim_payers));
    }

    // ----- select_in_scope: changed subset (cute-dbt#91) -----

    #[test]
    fn pr_diff_arm_changed_is_subset_and_distinguishes_updated_from_context() {
        // The load-bearing invariant for the PrDiff arm: `changed` is a
        // strict subset of `in_scope`, and it distinguishes updated tests
        // from context tests.
        //   - dim_payers.sql changed → its test (declaring YAML untouched)
        //     is in scope via target_path_modified, but NOT changed →
        //     context.
        //   - _changed.yml changed → stg_x's test is in scope AND changed
        //     (its declaring YAML is in the diff) even though stg_x.sql is
        //     untouched → updated.
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let stg_x = NodeId::new("model.shop.stg_x");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        nodes.insert(
            stg_x.clone(),
            model_node_with_path(&stg_x, "ck2", "models/staging/stg_x.sql"),
        );

        let ctx_id = "unit_test.shop.test_ctx";
        let upd_id = "unit_test.shop.test_upd";
        let mut tests = HashMap::new();
        tests.insert(
            ctx_id.to_owned(),
            test_with_path(
                "test_ctx",
                "dim_payers",
                Some("models/marts/_unchanged.yml"),
            ),
        );
        tests.insert(
            upd_id.to_owned(),
            test_with_path("test_upd", "stg_x", Some("models/marts/_changed.yml")),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input(
            &["models/marts/dim_payers.sql", "models/marts/_changed.yml"],
            None,
        );
        let selection = select_in_scope(&current, &input);

        // changed ⊆ in_scope — by construction (single traversal).
        for id in selection.changed.iter() {
            assert!(
                selection.in_scope.contains(id),
                "changed id {id:?} must be in scope (changed ⊆ in_scope)",
            );
        }
        assert!(selection.in_scope.contains(ctx_id));
        assert!(selection.in_scope.contains(upd_id));
        assert!(
            selection.changed.contains(upd_id),
            "test_upd is updated (its declaring YAML is in the diff)",
        );
        assert!(
            !selection.changed.contains(ctx_id),
            "test_ctx is context (in scope via its model's SQL, YAML unchanged)",
        );
    }

    // ----- changed_models (cute-dbt#106 — explore change context) -----

    #[test]
    fn changed_models_marks_exactly_the_path_modified_models() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let stg_customers = NodeId::new("model.shop.stg_customers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        nodes.insert(
            stg_customers.clone(),
            model_node_with_path(&stg_customers, "ck2", "models/staging/stg_customers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let index = NormalizedDiffIndex::new(
            &prdiff_from_paths(&["models/marts/dim_payers.sql", "README.md"]),
            None,
        );
        let changed = changed_models(&current, &index);

        assert!(changed.contains(&dim_payers));
        assert!(!changed.contains(&stg_customers));
        assert_eq!(changed.len(), 1, "extraneous diff paths mark nothing");
    }

    #[test]
    fn changed_models_excludes_non_model_nodes() {
        // A generic test node whose declaring SQL is in the diff must not
        // surface as a changed MODEL (the cute-dbt#167 filter, shared with
        // select_in_scope_pr_diff so the two consumers cannot drift).
        let generic_test = NodeId::new("test.shop.assert_positive_total");
        let mut nodes = HashMap::new();
        nodes.insert(
            generic_test.clone(),
            typed_node_with_path(
                &generic_test,
                "test",
                "ck1",
                "tests/assert_positive_total.sql",
            ),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let index = NormalizedDiffIndex::new(
            &prdiff_from_paths(&["tests/assert_positive_total.sql"]),
            None,
        );
        assert!(changed_models(&current, &index).is_empty());
    }

    #[test]
    fn changed_models_marks_a_purely_renamed_model_at_its_new_path() {
        // The cute-dbt#80 rename lineage: a pure rename carries no `+++`
        // header and no hunks — only the rename pair — and the current
        // manifest holds the node at the NEW path. The index keyset
        // carries both sides, so the model still marks as changed.
        let dim_b = NodeId::new("model.shop.dim_b");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_b.clone(),
            model_node_with_path(&dim_b, "ck1", "models/marts/dim_b.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let mut diff = prdiff_from_paths(&[]);
        diff.renames = vec![crate::domain::pr_diff::RenamePair {
            from: "models/marts/dim_a.sql".to_owned(),
            to: "models/marts/dim_b.sql".to_owned(),
        }];
        let index = NormalizedDiffIndex::new(&diff, None);
        let changed = changed_models(&current, &index);
        assert!(
            changed.contains(&dim_b),
            "the renamed model marks changed under its NEW path",
        );
    }

    #[test]
    fn changed_models_honors_the_project_root_strip() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let index = NormalizedDiffIndex::new(
            &prdiff_from_paths(&["dbt_project/models/marts/dim_payers.sql"]),
            Some(Path::new("dbt_project")),
        );
        assert!(changed_models(&current, &index).contains(&dim_payers));
    }

    #[test]
    fn changed_models_matches_the_pr_diff_arm_model_marking() {
        // The reuse property: explore's changed set is EXACTLY the PrDiff
        // arm's path-modified model marking (one matching authority — the
        // two verbs cannot disagree about which models a diff touched).
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let index =
            NormalizedDiffIndex::new(&prdiff_from_paths(&["models/marts/dim_payers.sql"]), None);
        let changed = changed_models(&current, &index);
        let selection = select_in_scope(
            &current,
            &ScopeInput::PrDiff {
                index: index.clone(),
            },
        );
        // dim_payers has zero unit tests, so the report arm surfaces it
        // via arm 2 — the same membership changed_models reports.
        assert_eq!(changed, selection.models_in_scope);
    }

    // ----- all_models (cute-dbt#100 — the explore verb's seam) -----

    #[test]
    fn all_models_returns_every_model_node_and_nothing_else() {
        // Three models (one of them UNCOMPILED — explore is fail-open, so
        // compiled-ness must not filter) plus a generic test node and a
        // seed node that must both stay out.
        let m1 = NodeId::new("model.shop.dim_payers");
        let m2 = NodeId::new("model.shop.stg_customers");
        let m3 = NodeId::new("model.shop.stg_uncompiled");
        let t1 = NodeId::new("test.shop.not_null_dim_payers_id");
        let s1 = NodeId::new("seed.shop.raw_payers");

        let mut nodes = HashMap::new();
        nodes.insert(m1.clone(), model_node(&m1, "ck1", None));
        nodes.insert(m2.clone(), model_node(&m2, "ck2", None));
        nodes.insert(m3.clone(), uncompiled_model_node(&m3, "ck3"));
        nodes.insert(t1.clone(), typed_node(&t1, "test", "ck4"));
        nodes.insert(s1.clone(), typed_node(&s1, "seed", "ck5"));
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let models = all_models(&current);

        assert_eq!(models.len(), 3, "exactly the three model nodes");
        assert!(models.contains(&m1));
        assert!(models.contains(&m2));
        assert!(
            models.contains(&m3),
            "an uncompiled model is still in the full-manifest scope (fail-open)",
        );
        assert!(!models.contains(&t1), "a generic test node is not a model");
        assert!(!models.contains(&s1), "a seed node is not a model");
    }

    #[test]
    fn all_models_of_an_empty_manifest_is_empty() {
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        assert!(all_models(&current).is_empty());
    }

    #[test]
    fn all_models_iterates_in_deterministic_node_id_order() {
        // BTreeSet-backed: insertion order (HashMap) must not leak into
        // iteration order — the rendered explore pages depend on it.
        let ids = ["model.shop.zeta", "model.shop.alpha", "model.shop.mid"];
        let mut nodes = HashMap::new();
        for id in ids {
            let node_id = NodeId::new(id);
            nodes.insert(node_id.clone(), model_node(&node_id, "ck", None));
        }
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let models = all_models(&current);
        let ordered: Vec<&str> = models.iter().map(NodeId::as_str).collect();
        assert_eq!(
            ordered,
            vec!["model.shop.alpha", "model.shop.mid", "model.shop.zeta"],
        );
    }

    /// A model node with `compiled_code: None` — the `dbt parse` shape
    /// explore renders fail-open as "not compiled".
    fn uncompiled_model_node(id: &NodeId, ck: &str) -> Node {
        Node::new(
            id.clone(),
            "model",
            checksum(ck),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    // ----- helpers -----

    fn checksum(value: &str) -> Checksum {
        Checksum::new("sha256", value)
    }

    fn model_node(id: &NodeId, ck: &str, ofp: Option<&str>) -> Node {
        Node::new(
            id.clone(),
            "model",
            checksum(ck),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            ofp.map(str::to_owned),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    fn model_node_with_path(id: &NodeId, ck: &str, ofp: &str) -> Node {
        model_node(id, ck, Some(ofp))
    }

    /// A node of an arbitrary `resource_type` (cute-dbt#167 — the arm-2
    /// resource-type filter regression tests).
    fn typed_node(id: &NodeId, resource_type: &str, ck: &str) -> Node {
        Node::new(
            id.clone(),
            resource_type,
            checksum(ck),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    /// A non-model node with an `original_file_path` (the `PrDiff` arm's
    /// path-matching input).
    fn typed_node_with_path(id: &NodeId, resource_type: &str, ck: &str, ofp: &str) -> Node {
        Node::new(
            id.clone(),
            resource_type,
            checksum(ck),
            None,
            None,
            DependsOn::default(),
            Some(ofp.to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    fn test_for(name: &str, model_bare: &str) -> UnitTest {
        test_with_path(name, model_bare, None)
    }

    fn test_with_path(name: &str, model_bare: &str, ofp: Option<&str>) -> UnitTest {
        UnitTest::new(
            name.to_owned(),
            NodeId::new(model_bare),
            Vec::new(),
            UnitTestExpect::new(serde_json::Value::Null, None, None),
            None,
            DependsOn::default(),
            None,
            None,
            ofp.map(str::to_owned),
        )
    }
}
