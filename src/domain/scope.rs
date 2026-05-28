//! Scope-selection (cute-dbt#81 — Shape E Phase 1).
//!
//! Defines the in-scope set the run loop (PR-pdiff-1b) renders, from one
//! of two sources (resolved at CLI parse time):
//!
//! - [`ScopeInput::Baseline`] — the v0.1 `--baseline-manifest` path.
//!   Delegates to [`StateComparator::body_only`] so the existing dbt
//!   `state:modified` semantics flow through unchanged.
//! - [`ScopeInput::PrDiff`] — the new `--scope-from-pr-diff` path.
//!   Matches the changed-files list (typically a PR's `git diff
//!   --name-only`) against [`crate::domain::manifest::Node::original_file_path`]
//!   and [`crate::domain::unit_test::UnitTest::original_file_path`].
//!
//! Two scope sources is a deliberate ADR-1 judgment call: free function
//! over trait until a third source arrives (a v0.2+ refactor moment).
//!
//! Path normalization: leading `./` is stripped; an optional
//! `project_root` prefix is stripped from changed paths (a dbt sub-tree
//! workflow lives under `<repo-root>/dbt_project/`, the manifest
//! records `models/...` relative to `dbt_project/`); double slashes
//! collapse. Windows-style `\` separators are explicitly **not**
//! supported in v0.1 — dbt manifests on macOS/Linux emit forward
//! slashes. Promoting to cross-platform path-set semantics is a v0.2+
//! follow-up.
// tracked: cute-dbt#80 — git-rename detection layer on top of `git diff
// --name-only` (a rename appears as one deleted path + one added path
// today; the deleted path maps to no current-manifest node).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::domain::manifest::{Manifest, NodeId};
use crate::domain::state::{InScopeSet, ModelInScopeSet, StateComparator, resolve_target_model};

/// Source of the in-scope set: either a baseline manifest (dbt
/// `state:modified` semantics) or a PR-diff file list (CI/PR-review path).
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
    /// changed-file list. CI/PR-review path — no baseline needed.
    PrDiff {
        /// Paths from `git diff --name-only ${base.sha}...${head.sha}`
        /// or equivalent. Typically includes non-dbt files (README,
        /// workflow YAML, `dbt_project.yml`) which silently miss.
        changed_files: Vec<String>,
        /// dbt project root relative to the repo root, used to rebase
        /// changed paths against manifest `original_file_path` (which
        /// is project-relative). `None` when `project_root` ==
        /// `repo_root`.
        project_root_strip: Option<PathBuf>,
    },
}

/// Resolve the (unit-test in-scope, model in-scope) pair for the
/// current manifest and the given [`ScopeInput`].
///
/// - [`ScopeInput::Baseline`] delegates to [`StateComparator::body_only`].
/// - [`ScopeInput::PrDiff`] matches changed-file paths against
///   `original_file_path`.
#[must_use]
pub fn select_in_scope(current: &Manifest, input: &ScopeInput) -> (InScopeSet, ModelInScopeSet) {
    match input {
        ScopeInput::Baseline { manifest: baseline } => {
            let cmp = StateComparator::body_only();
            (
                cmp.in_scope_unit_tests(current, baseline),
                cmp.models_in_scope(current, baseline),
            )
        }
        ScopeInput::PrDiff {
            changed_files,
            project_root_strip,
        } => select_in_scope_pr_diff(current, changed_files, project_root_strip.as_deref()),
    }
}

/// Normalize a file path for matching:
/// - Strip leading `./`.
/// - Strip `strip_prefix` (with optional trailing slash) if the path
///   starts with it.
/// - Collapse runs of `/` into a single `/`.
///
/// Returns the normalized path as a `String` (cheap — most fixtures are
/// short). Windows-style `\` separators are passed through unchanged
/// (v0.1 limitation; tracked: cute-dbt#80 deferred follow-ups).
#[must_use]
pub fn normalize_path(p: &str, strip_prefix: Option<&Path>) -> String {
    let mut remaining = p;

    // Step 1: strip leading "./".
    while let Some(rest) = remaining.strip_prefix("./") {
        remaining = rest;
    }

    // Step 2: strip the configured project-root prefix, if present.
    if let Some(prefix) = strip_prefix {
        let prefix_str = prefix.to_string_lossy();
        let prefix_str = prefix_str.trim_end_matches('/');
        if !prefix_str.is_empty() {
            if let Some(rest) = remaining.strip_prefix(prefix_str) {
                remaining = rest.strip_prefix('/').unwrap_or(rest);
            }
        }
    }

    // Step 3: collapse "//" runs into "/".
    if remaining.contains("//") {
        let mut out = String::with_capacity(remaining.len());
        let mut prev_slash = false;
        for ch in remaining.chars() {
            if ch == '/' {
                if !prev_slash {
                    out.push('/');
                }
                prev_slash = true;
            } else {
                out.push(ch);
                prev_slash = false;
            }
        }
        return out;
    }

    remaining.to_owned()
}

/// `true` when `manifest_path` (after normalization) equals any of
/// `changed_paths` (after the same normalization with `project_root_strip`
/// applied). The manifest path is project-root-relative; the changed
/// paths are repo-root-relative — `project_root_strip` bridges the gap.
///
/// Designed for callers that need the boolean without first materializing
/// the normalized change set. For bulk lookups, prefer building a
/// `HashSet<String>` of normalized changed paths via [`normalize_path`]
/// once and consulting it directly.
#[must_use]
pub fn match_changed_path(
    manifest_path: &str,
    changed_paths: &[String],
    project_root_strip: Option<&Path>,
) -> bool {
    let manifest_norm = normalize_path(manifest_path, None);
    changed_paths
        .iter()
        .any(|changed| normalize_path(changed, project_root_strip) == manifest_norm)
}

// ---------------------------------------------------------------------
// PrDiff arm
// ---------------------------------------------------------------------

fn select_in_scope_pr_diff(
    current: &Manifest,
    changed_files: &[String],
    project_root_strip: Option<&Path>,
) -> (InScopeSet, ModelInScopeSet) {
    // Materialize the normalized change set once for O(1) lookup.
    let normalized_changes: HashSet<String> = changed_files
        .iter()
        .map(|p| normalize_path(p, project_root_strip))
        .collect();

    // Identify path-modified models — the PrDiff analog of the baseline
    // `modified_set`. Only `model` nodes participate (other resource
    // types do not host unit tests in v0.1).
    let path_modified_models: HashSet<NodeId> = current
        .nodes()
        .iter()
        .filter_map(|(id, node)| {
            if node.resource_type() != "model" {
                return None;
            }
            let ofp = node.original_file_path()?;
            if normalized_changes.contains(&normalize_path(ofp, None)) {
                Some(id.clone())
            } else {
                None
            }
        })
        .collect();

    // In-scope unit tests: a test is in scope when its target model is
    // path-modified OR its own `original_file_path` (the declaring YAML
    // file) is in the change set. Mirrors the dbt OR-semantics of the
    // baseline path.
    let in_scope: InScopeSet = current
        .unit_tests()
        .iter()
        .filter_map(|(test_id, ut)| {
            let target_path_modified = resolve_target_model(current, ut.model())
                .is_some_and(|model| path_modified_models.contains(model.id()));
            let test_yaml_changed = ut
                .original_file_path()
                .is_some_and(|p| normalized_changes.contains(&normalize_path(p, None)));
            if target_path_modified || test_yaml_changed {
                Some(test_id.clone())
            } else {
                None
            }
        })
        .collect();

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
    (in_scope, models_in_scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{Checksum, DependsOn, ManifestMetadata, Node};
    use crate::domain::unit_test::{UnitTest, UnitTestExpect};
    use std::collections::HashMap;

    // ----- normalize_path -----

    #[test]
    fn normalize_path_strips_leading_dot_slash() {
        assert_eq!(normalize_path("./models/x.sql", None), "models/x.sql");
    }

    #[test]
    fn normalize_path_strips_repeated_leading_dot_slash() {
        assert_eq!(normalize_path("././models/x.sql", None), "models/x.sql");
    }

    #[test]
    fn normalize_path_strips_project_root_prefix() {
        assert_eq!(
            normalize_path("dbt_project/models/x.sql", Some(Path::new("dbt_project"))),
            "models/x.sql"
        );
    }

    #[test]
    fn normalize_path_strips_project_root_prefix_with_trailing_slash() {
        assert_eq!(
            normalize_path("dbt_project/models/x.sql", Some(Path::new("dbt_project/"))),
            "models/x.sql"
        );
    }

    #[test]
    fn normalize_path_collapses_double_slash() {
        assert_eq!(normalize_path("models//x.sql", None), "models/x.sql");
    }

    #[test]
    fn normalize_path_leaves_unrelated_paths_unchanged() {
        assert_eq!(normalize_path("README.md", None), "README.md");
    }

    #[test]
    fn normalize_path_does_not_strip_prefix_when_not_present() {
        assert_eq!(
            normalize_path("models/x.sql", Some(Path::new("dbt_project"))),
            "models/x.sql"
        );
    }

    // ----- match_changed_path -----

    #[test]
    fn match_changed_path_finds_exact_match() {
        let changed = vec!["models/x.sql".to_owned()];
        assert!(match_changed_path("models/x.sql", &changed, None));
    }

    #[test]
    fn match_changed_path_finds_match_after_leading_dot_slash_strip() {
        let changed = vec!["./models/x.sql".to_owned()];
        assert!(match_changed_path("models/x.sql", &changed, None));
    }

    #[test]
    fn match_changed_path_finds_match_after_project_root_strip() {
        let changed = vec!["dbt_project/models/x.sql".to_owned()];
        assert!(match_changed_path(
            "models/x.sql",
            &changed,
            Some(Path::new("dbt_project"))
        ));
    }

    #[test]
    fn match_changed_path_no_match_for_unrelated_path() {
        let changed = vec!["README.md".to_owned()];
        assert!(!match_changed_path("models/x.sql", &changed, None));
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
        let (in_scope, models) = select_in_scope(&current, &input);

        assert!(in_scope.contains(test_id));
        assert!(models.contains(&modified_id));
        assert!(!models.contains(&unchanged_id));
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

        let input = ScopeInput::PrDiff {
            changed_files: vec!["models/marts/dim_payers.sql".to_owned()],
            project_root_strip: None,
        };
        let (in_scope, models) = select_in_scope(&current, &input);

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

        let input = ScopeInput::PrDiff {
            changed_files: vec![
                "README.md".to_owned(),
                ".github/workflows/ci.yml".to_owned(),
                "packages.yml".to_owned(),
                "dbt_project.yml".to_owned(),
                "models/deleted_model.sql".to_owned(),
            ],
            project_root_strip: None,
        };
        let (in_scope, models) = select_in_scope(&current, &input);

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
        let input = ScopeInput::PrDiff {
            changed_files: vec!["models/marts/_core__models.yml".to_owned()],
            project_root_strip: None,
        };
        let (in_scope, _models) = select_in_scope(&current, &input);

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

        let input = ScopeInput::PrDiff {
            changed_files: vec!["models/staging/stg_payments.sql".to_owned()],
            project_root_strip: None,
        };
        let (in_scope, models) = select_in_scope(&current, &input);

        assert_eq!(in_scope.len(), 0);
        assert!(models.contains(&stg_payments));
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

        let input = ScopeInput::PrDiff {
            changed_files: vec!["dbt_project/models/marts/dim_payers.sql".to_owned()],
            project_root_strip: Some(PathBuf::from("dbt_project")),
        };
        let (_in_scope, models) = select_in_scope(&current, &input);

        assert!(models.contains(&dim_payers));
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
        )
    }

    fn model_node_with_path(id: &NodeId, ck: &str, ofp: &str) -> Node {
        model_node(id, ck, Some(ofp))
    }

    fn test_for(name: &str, model_bare: &str) -> UnitTest {
        test_with_path(name, model_bare, None)
    }

    fn test_with_path(name: &str, model_bare: &str, ofp: Option<&str>) -> UnitTest {
        UnitTest::new(
            name.to_owned(),
            NodeId::new(model_bare),
            Vec::new(),
            UnitTestExpect::new(serde_json::Value::Null, None),
            None,
            DependsOn::default(),
            None,
            None,
            ofp.map(str::to_owned),
        )
    }
}
