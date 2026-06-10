//! Scope-selection (cute-dbt#81 — Shape E Phase 1).
//!
//! Defines the in-scope set the run loop (PR-pdiff-1b) renders, from one
//! of two sources (resolved at CLI parse time):
//!
//! - [`ScopeInput::Baseline`] — the v0.1 `--baseline-manifest` path.
//!   Delegates to [`StateComparator::body_only`] so the existing dbt
//!   `state:modified` semantics flow through unchanged.
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

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::domain::manifest::{Manifest, NodeId};
use crate::domain::pr_diff::NormalizedDiffIndex;
use crate::domain::state::{InScopeSet, ModelInScopeSet, StateComparator, resolve_target_model};

/// Source of the in-scope set: either a baseline manifest (dbt
/// `state:modified` semantics) or a parsed PR diff (CI/PR-review path).
#[derive(Debug, Clone)]
pub enum ScopeInput {
    /// Compare against a baseline manifest — v0.1 default, ADR-2 +
    /// ADR-3 semantics unchanged.
    Baseline {
        /// Already-parsed baseline manifest (Stage-1 pre-flight ran in
        /// the adapter).
        manifest: Manifest,
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
///   scope via the `target_modified || test_changed` union.
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
/// - [`ScopeInput::Baseline`] delegates to [`StateComparator::body_only`]
///   for the in-scope/model sets and to
///   [`StateComparator::changed_unit_tests`] for the changed subset.
/// - [`ScopeInput::PrDiff`] matches changed-file paths against
///   `original_file_path` via the [`NormalizedDiffIndex`], collecting the
///   in-scope and changed sets in one pass.
#[must_use]
pub fn select_in_scope(current: &Manifest, input: &ScopeInput) -> ScopeSelection {
    match input {
        ScopeInput::Baseline { manifest: baseline } => {
            let cmp = StateComparator::body_only();
            ScopeSelection {
                in_scope: cmp.in_scope_unit_tests(current, baseline),
                models_in_scope: cmp.models_in_scope(current, baseline),
                changed: StateComparator::changed_unit_tests(current, baseline),
            }
        }
        ScopeInput::PrDiff { index } => select_in_scope_pr_diff(current, index),
    }
}

// ---------------------------------------------------------------------
// PrDiff arm
// ---------------------------------------------------------------------

fn select_in_scope_pr_diff(current: &Manifest, index: &NormalizedDiffIndex) -> ScopeSelection {
    // Identify path-modified models — the PrDiff analog of the baseline
    // `modified_set`. Only `model` nodes participate (other resource
    // types do not host unit tests in v0.1). The index owns the
    // changed-file keyset, so this consults it rather than normalizing
    // paths here (single normalization authority).
    let path_modified_models: HashSet<NodeId> = current
        .nodes()
        .iter()
        .filter_map(|(id, node)| {
            if node.resource_type() != "model" {
                return None;
            }
            let ofp = node.original_file_path()?;
            if index.contains_changed(ofp) {
                Some(id.clone())
            } else {
                None
            }
        })
        .collect();

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
        let target_path_modified = resolve_target_model(current, ut.model())
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
        .filter_map(|ut| resolve_target_model(current, ut.model()).map(|m| m.id().clone()))
        .fold(HashMap::new(), |mut acc, id| {
            *acc.entry(id).or_insert(0) += 1;
            acc
        });

    let mut model_ids: BTreeSet<NodeId> = BTreeSet::new();
    for test_id in in_scope.iter() {
        if let Some(ut) = current.unit_test(test_id) {
            if let Some(model) = resolve_target_model(current, ut.model()) {
                model_ids.insert(model.id().clone());
            }
        }
    }
    for model_id in &path_modified_models {
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

        let input = ScopeInput::Baseline { manifest: baseline };
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

        let input = ScopeInput::Baseline { manifest: baseline };
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
