//! Parsed PR-diff facts + the single normalization authority
//! (cute-dbt#96).
//!
//! A **path-aware leaf**: it owns the parsed-diff POD ([`PrDiff`] /
//! [`FileHunks`] / [`Hunk`]) and [`NormalizedDiffIndex`], the one place
//! that normalizes paths for both the diff-side file keyset (with the
//! project-root strip) and the declaring-side hunk lookup (with `None`).
//! Putting the index here — rather than in `scope` or a standalone
//! `path` leaf — keeps the module DAG acyclic: `scope → pr_diff → path`
//! (CAO plan-audit Decision 2). `scope` references the index; the index
//! references [`crate::domain::path::normalize_path`]; nothing points
//! back.
//!
//! cute-dbt never shells out to `git`. The workflow produces the diff
//! (`git diff --unified=0`); the `cli::pr_diff::parse_diff` value-parser
//! turns its text into a [`PrDiff`]; this module turns a [`PrDiff`] into the
//! facts scope-selection and (cute-dbt#96 concern 2) the inline YAML
//! diff consume. POD-only, `std` + `serde` derive: the report inlines
//! the parsed facts so `#98` (cell-level data-table diff) can reuse the
//! same POD.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain::path::normalize_path;

/// A parsed `git diff --unified=0`: the changed files and, per file, the
/// hunks that touch the **new** (post-change) side.
///
/// Additive POD (ADR-5). `Serialize`/`Deserialize` so the parsed facts
/// round-trip through the inlined report payload and `#98` can reuse the
/// shape.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PrDiff {
    /// One entry per changed file the diff names on its new side
    /// (`+++ b/<path>`). `/dev/null` (pure deletion of a whole file) is
    /// dropped by the parser.
    pub files: Vec<FileHunks>,
}

/// One changed file and its hunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHunks {
    /// The new-side repo-relative path (the `b/<path>` of `+++ b/<path>`,
    /// with the `b/` stripped). Repo-root-relative — a sub-directory dbt
    /// project carries the project-root prefix here; the manifest's
    /// `original_file_path` is project-relative.
    pub path: String,
    /// The hunks touching this file, in diff order.
    pub hunks: Vec<Hunk>,
}

/// One unified-diff hunk, retaining both sides' bodies.
///
/// `new_start` / `new_len` describe the hunk's footprint on the **new**
/// side (`@@ -old +new_start,new_len @@`). A pure-deletion hunk has
/// `new_len == 0` (a *point-touch* at `new_start` — cute-dbt#96 treats
/// the deletion site as touching the block). `removed_lines` /
/// `added_lines` retain the `-`/`+` bodies (no leading sigil): the
/// inline YAML diff (#96 concern 2) reconstructs from them and the
/// drift guard (`block_aligns_with_hunks`) compares `added_lines`
/// against the working-tree block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// 1-based first line of the hunk on the new side. For a
    /// pure-deletion hunk (`new_len == 0`) this is the line the deletion
    /// sits at/after.
    pub new_start: usize,
    /// Number of new-side lines the hunk spans. `0` for a pure deletion.
    pub new_len: usize,
    /// The `-` bodies (removed lines), sigil stripped, in diff order.
    pub removed_lines: Vec<String>,
    /// The `+` bodies (added lines), sigil stripped, in diff order.
    pub added_lines: Vec<String>,
}

/// The single normalization authority for a [`PrDiff`].
///
/// Built **once** (in the run loop's `resolve_scope_input`) and threaded
/// as one instance into scope selection (the changed-file keyset) and —
/// cute-dbt#96 concern 1/2 — the block-precise `changed` refinement and
/// the inline-diff reconstruction. The diff-side keys are normalized
/// **with** the project-root strip; the declaring-side lookups
/// ([`contains_changed`](Self::contains_changed) /
/// [`hunks_for`](Self::hunks_for)) normalize with `None`, because a
/// manifest `original_file_path` is already project-relative. Both sides
/// therefore resolve to the same key — the property the
/// `single_normalization_authority_*` tests pin.
#[derive(Debug, Clone)]
pub struct NormalizedDiffIndex {
    /// Normalized (strip-applied) declaring path → that file's hunks.
    by_path: HashMap<String, Vec<Hunk>>,
}

impl NormalizedDiffIndex {
    /// Build the index from a parsed diff, rebasing each diff-side path
    /// with `strip` (the dbt project root relative to the repo root, or
    /// `None` for an identity strip).
    #[must_use]
    pub fn new(diff: &PrDiff, strip: Option<&Path>) -> Self {
        let mut by_path: HashMap<String, Vec<Hunk>> = HashMap::new();
        for file in &diff.files {
            let key = normalize_path(&file.path, strip);
            by_path
                .entry(key)
                .or_default()
                .extend(file.hunks.iter().cloned());
        }
        Self { by_path }
    }

    /// The normalized changed-file paths (the diff-side keyset). Used by
    /// scope selection to identify path-modified models — the [`PrDiff`]
    /// analog of the baseline `modified_set`.
    pub fn changed_paths(&self) -> impl Iterator<Item = &str> {
        self.by_path.keys().map(String::as_str)
    }

    /// `true` when `original_file_path` (a project-relative manifest
    /// path, normalized with `None`) is among the changed files.
    #[must_use]
    pub fn contains_changed(&self, original_file_path: &str) -> bool {
        self.by_path
            .contains_key(&normalize_path(original_file_path, None))
    }

    /// The hunks touching `original_file_path`'s declaring file (empty
    /// when the file is not in the diff). Normalizes the manifest-side
    /// path with `None` — the declaring-side half of the single
    /// normalization authority.
    #[must_use]
    pub fn hunks_for(&self, original_file_path: &str) -> &[Hunk] {
        self.by_path
            .get(&normalize_path(original_file_path, None))
            .map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(new_start: usize, new_len: usize) -> Hunk {
        Hunk {
            new_start,
            new_len,
            removed_lines: Vec::new(),
            added_lines: Vec::new(),
        }
    }

    // ----- POD serde round-trip -----

    #[test]
    fn pr_diff_round_trips_through_json() {
        let diff = PrDiff {
            files: vec![FileHunks {
                path: "models/marts/core/_core__models.yml".to_owned(),
                hunks: vec![Hunk {
                    new_start: 5,
                    new_len: 2,
                    removed_lines: vec!["    rows: []".to_owned()],
                    added_lines: vec!["    rows:".to_owned(), "      - {id: 1}".to_owned()],
                }],
            }],
        };
        let json = serde_json::to_string(&diff).expect("serialize");
        let back: PrDiff = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(diff, back);
    }

    #[test]
    fn empty_pr_diff_round_trips() {
        let diff = PrDiff::default();
        let json = serde_json::to_string(&diff).expect("serialize");
        let back: PrDiff = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(diff, back);
        assert!(back.files.is_empty());
    }

    // ----- NormalizedDiffIndex: keyset + lookup -----

    #[test]
    fn index_changed_paths_lists_normalized_file_set() {
        let diff = PrDiff {
            files: vec![
                FileHunks {
                    path: "./models/a.sql".to_owned(),
                    hunks: vec![hunk(1, 1)],
                },
                FileHunks {
                    path: "models/b.yml".to_owned(),
                    hunks: vec![hunk(3, 1)],
                },
            ],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        let mut paths: Vec<&str> = index.changed_paths().collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["models/a.sql", "models/b.yml"]);
    }

    #[test]
    fn index_contains_changed_matches_normalized_manifest_path() {
        let diff = PrDiff {
            files: vec![FileHunks {
                path: "models/marts/dim_payers.sql".to_owned(),
                hunks: vec![hunk(1, 1)],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        assert!(index.contains_changed("models/marts/dim_payers.sql"));
        assert!(!index.contains_changed("models/staging/stg_customers.sql"));
    }

    #[test]
    fn index_hunks_for_returns_the_files_hunks() {
        let diff = PrDiff {
            files: vec![FileHunks {
                path: "models/_ut.yml".to_owned(),
                hunks: vec![hunk(5, 2), hunk(12, 1)],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        assert_eq!(index.hunks_for("models/_ut.yml").len(), 2);
        assert_eq!(index.hunks_for("models/absent.yml"), &[]);
    }

    // ----- The single-normalization-authority property (advisor) -----

    #[test]
    fn single_normalization_authority_resolves_both_sides_to_same_key() {
        // The diff-side path carries the repo-root prefix
        // ("dbt_project/…"); the manifest's original_file_path is
        // project-relative ("models/…"). The strip is applied to the
        // diff side at build time; the declaring side is normalized with
        // `None` at lookup time. Both must resolve to the SAME key —
        // this is the "one normalization authority" claim cashed out
        // (BDD non-identity-strip scenario, at the unit level).
        let diff = PrDiff {
            files: vec![FileHunks {
                path: "dbt_project/models/marts/core/_core__models.yml".to_owned(),
                hunks: vec![hunk(7, 1)],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, Some(Path::new("dbt_project")));

        // Declaring side (manifest, project-relative) resolves.
        assert!(index.contains_changed("models/marts/core/_core__models.yml"));
        assert_eq!(
            index.hunks_for("models/marts/core/_core__models.yml").len(),
            1
        );
        // And the keyset itself is the project-relative key.
        let paths: Vec<&str> = index.changed_paths().collect();
        assert_eq!(paths, vec!["models/marts/core/_core__models.yml"]);
    }

    #[test]
    fn index_merges_hunks_when_a_path_appears_twice() {
        // Defensive: a malformed diff naming the same file twice merges
        // its hunks rather than dropping one.
        let diff = PrDiff {
            files: vec![
                FileHunks {
                    path: "models/_ut.yml".to_owned(),
                    hunks: vec![hunk(1, 1)],
                },
                FileHunks {
                    path: "models/_ut.yml".to_owned(),
                    hunks: vec![hunk(9, 1)],
                },
            ],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        assert_eq!(index.hunks_for("models/_ut.yml").len(), 2);
    }
}
