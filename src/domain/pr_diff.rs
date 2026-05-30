//! Parsed PR-diff facts + the single normalization authority
//! (cute-dbt#96).
//!
//! It owns the parsed-diff POD ([`PrDiff`] / [`FileHunks`] / [`Hunk`]),
//! [`NormalizedDiffIndex`] — the one place that normalizes paths for both
//! the diff-side file keyset (with the project-root strip) and the
//! declaring-side hunk lookup (with `None`) — and (cute-dbt#96 Step 2) the
//! block-precise `changed` narrowing ([`hunk_touches_block`],
//! [`block_aligns_with_hunks`], [`block_changed_by_hunks`],
//! [`refine_changed_by_hunks`]).
//!
//! Keeping the index + the block-precise logic here — rather than in
//! `scope` or a standalone `path` leaf — keeps the module DAG acyclic:
//! `scope → pr_diff` (CAO plan-audit Decision 2). `scope` references this
//! module; nothing in this module points back at `scope`. Step 1 framed
//! `pr_diff` as a `path`-only leaf; Step 2's narrowing genuinely needs the
//! manifest (test id → declaring `original_file_path` → hunks), the
//! [`UnitTestYamlBlock`] span, and [`InScopeSet`], so the honest
//! intra-domain edge set is `pr_diff → {path, manifest, state,
//! unit_test_yaml}` — all downward (none of those import `pr_diff`), so
//! still acyclic. (The leaf direction is structurally unenforced —
//! `tests/domain_clean_arch.rs` greps only outward `adapters`/`cli`
//! imports — so it rides on review + the closeout decision-note.)
//!
//! cute-dbt never shells out to `git`. The workflow produces the diff
//! (`git diff --unified=0`); the `cli::pr_diff::parse_diff` value-parser
//! turns its text into a [`PrDiff`]; this module turns a [`PrDiff`] into the
//! facts scope-selection and (cute-dbt#96 concern 2) the inline YAML
//! diff consume. The POD is `std` + `serde` derive only, so the report
//! inlines the parsed facts and `#98` (cell-level data-table diff) can
//! reuse the same shape.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain::manifest::Manifest;
use crate::domain::path::normalize_path;
use crate::domain::state::InScopeSet;
use crate::domain::unit_test::UnitTest;
use crate::domain::unit_test_yaml::UnitTestYamlBlock;

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

// ---------------------------------------------------------------------
// Block-precise `changed` narrowing (cute-dbt#96 Step 2)
// ---------------------------------------------------------------------

/// Whether `hunk`'s new-side footprint overlaps the 1-based inclusive block
/// span `[block_start, block_end]`.
///
/// For an insertion/replacement (`new_len >= 1`) the hunk occupies new-side
/// lines `[new_start, new_start + new_len - 1]`, and this is a standard
/// closed-interval overlap: `new_start <= block_end && hunk_end >= block_start`.
///
/// For a **pure deletion** (`new_len == 0`) there are no new-side lines —
/// the removed content collapses to a gap immediately after new-side line
/// `new_start`. Modeling that gap as a point at `new_start + 0.5` and the
/// block as the closed real interval `[block_start − 0.5, block_end + 0.5]`,
/// point-in-interval reduces to `block_start − 1 <= new_start <= block_end`
/// (saturating at line 1). The lower edge `block_start − 1` is deliberate: a
/// deletion whose gap sits just before the block's first line removed
/// content at the block's leading edge and must count as touching it.
///
/// The edge bias is intentionally **over-inclusive**. [`refine_changed_by_hunks`]
/// only ever *removes* ids from `changed`, so a false `true` here keeps a
/// test at its pre-#96 file-granular `changed` label (safe), whereas a false
/// `false` would drop a genuinely-updated test to context (the dangerous
/// miss). At a boundary, keeping the test as updated is the correct
/// conservative direction.
#[must_use]
pub fn hunk_touches_block(block_start: usize, block_end: usize, hunk: &Hunk) -> bool {
    if hunk.new_len == 0 {
        // Pure deletion: gap at `new_start + 0.5`; block `[bs-0.5, be+0.5]`.
        block_start.saturating_sub(1) <= hunk.new_start && hunk.new_start <= block_end
    } else {
        let hunk_end = hunk.new_start + hunk.new_len - 1;
        hunk.new_start <= block_end && hunk_end >= block_start
    }
}

/// Whether the diff's hunks still line up with the working-tree block — the
/// revision-alignment (N7b) drift guard.
///
/// cute-dbt's block span comes from the working tree (the #69 slicer); the
/// hunks come from an externally-produced `git diff`. They are only
/// comparable if both describe the same revision. This checks that every
/// added (`+`) line a hunk claims for a new-side position inside the block
/// matches the working-tree line at that position. A mismatch means the diff
/// is stale relative to the working tree, so the caller degrades gracefully
/// (keeps the test file-granular, drops the inline diff) rather than
/// misclassifying.
///
/// Trailing `\r` is trimmed on both sides: the diff parser reads via
/// `str::lines` (strips `\r`) while the slicer reads via `split('\n')`
/// (keeps it), so a CRLF working tree would otherwise report a spurious
/// mismatch on content that is byte-identical apart from line endings.
/// Added lines whose new-side position falls outside the block are ignored
/// (a hunk may straddle the block edge); a hunk with no `+` lines (a pure
/// deletion) is vacuously aligned.
#[must_use]
pub fn block_aligns_with_hunks(block: &UnitTestYamlBlock, hunks: &[Hunk]) -> bool {
    let block_lines: Vec<&str> = block.raw.split('\n').collect();
    for hunk in hunks {
        for (k, added) in hunk.added_lines.iter().enumerate() {
            let file_line = hunk.new_start + k; // 1-based new-side line
            if file_line < block.block_start || file_line > block.block_end {
                continue; // outside this block — not this block's concern
            }
            let offset = file_line - block.block_start; // 0-based into raw
            let Some(actual) = block_lines.get(offset) else {
                return false; // claims a line the block doesn't have
            };
            if actual.trim_end_matches('\r') != added.trim_end_matches('\r') {
                return false; // stale: diff content != working-tree content
            }
        }
    }
    true
}

/// The pure per-test decision: should a `changed` test STAY changed after
/// block-precise narrowing?
///
/// Keep (`true`) when any of:
/// - the block is **absent** (`None`) — the slicer couldn't locate it (file
///   missing, name not found, or the whole block was deleted); without a
///   span there is nothing to narrow against, so keep conservatively;
/// - the block is present but the hunks **don't align** ([`block_aligns_with_hunks`]
///   is false) — the diff is stale, so degrade to the file-granular label;
/// - the block is present, aligned, and **some hunk touches it** — a genuine
///   in-block edit.
///
/// Drop (`false`) only when the block is present, aligned, and **no hunk
/// touches it** — the confident case where the change lives entirely outside
/// this test's definition (a sibling test, or the surrounding `models:`
/// region). This is the sole narrowing path, so `changed′ ⊆ changed` holds.
///
/// This is the manifest-free core the boundary tables exercise;
/// [`refine_changed_by_hunks`] is the thin loop that resolves each id's
/// block + hunks and applies it.
#[must_use]
pub fn block_changed_by_hunks(block: Option<&UnitTestYamlBlock>, hunks: &[Hunk]) -> bool {
    let Some(block) = block else {
        return true; // absent block → conservative keep
    };
    if !block_aligns_with_hunks(block, hunks) {
        return true; // stale diff (N7b mismatch) → conservative keep
    }
    hunks
        .iter()
        .any(|hunk| hunk_touches_block(block.block_start, block.block_end, hunk))
}

/// Narrow a file-granular `changed` set to block precision (cute-dbt#96).
///
/// For each test id in `changed`, look up its sliced YAML block (`blocks` —
/// the run loop's `authoring_yaml` map, keyed by test id) and the hunks
/// touching its declaring file (via the [`NormalizedDiffIndex`], resolved
/// from the manifest's `original_file_path`), then apply
/// [`block_changed_by_hunks`]. An id is retained iff that predicate holds.
///
/// A post-scope run-loop *narrowing*: it only ever removes ids, so
/// `changed′ ⊆ changed ⊆ in_scope` is preserved. Called only on the
/// [`crate::domain::scope::ScopeInput::PrDiff`] arm — baseline-mode `changed`
/// is already precise (a [`crate::domain::state::StateComparator`] struct
/// diff) and must never reach here.
///
/// The run loop always passes the default-hasher `authoring_yaml` map, so
/// generalizing `blocks` over the hasher (`clippy::implicit_hasher`) buys
/// nothing — same rationale as `render_report`.
#[allow(clippy::implicit_hasher)]
#[must_use]
pub fn refine_changed_by_hunks(
    current: &Manifest,
    changed: &InScopeSet,
    blocks: &HashMap<String, UnitTestYamlBlock>,
    index: &NormalizedDiffIndex,
) -> InScopeSet {
    changed
        .iter()
        .filter(|id| {
            let hunks = current
                .unit_test(id)
                .and_then(UnitTest::original_file_path)
                .map_or(&[][..], |ofp| index.hunks_for(ofp));
            block_changed_by_hunks(blocks.get(*id), hunks)
        })
        .map(str::to_owned)
        .collect()
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

    // =================================================================
    // Block-precise narrowing (cute-dbt#96 Step 2)
    // =================================================================

    // A pure-deletion hunk (point-touch): no new-side lines, no `+` body.
    fn del(new_start: usize) -> Hunk {
        Hunk {
            new_start,
            new_len: 0,
            removed_lines: vec!["removed".to_owned()],
            added_lines: Vec::new(),
        }
    }

    // A replacement hunk carrying `added` as its new-side body (so
    // `new_len == added.len()`), used for both touch and alignment tests.
    fn repl(new_start: usize, added: &[&str]) -> Hunk {
        Hunk {
            new_start,
            new_len: added.len(),
            removed_lines: Vec::new(),
            added_lines: added.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    // A block whose span is derived from `raw`'s line count (line_of_name
    // is irrelevant to the overlap math, so it's pinned to block_start).
    fn block_at(raw: &str, block_start: usize) -> UnitTestYamlBlock {
        let n = raw.split('\n').count();
        UnitTestYamlBlock::new(
            raw.to_owned(),
            block_start,
            block_start,
            block_start + n - 1,
        )
    }

    // ----- hunk_touches_block: replacement boundary table -----

    #[test]
    fn hunk_touches_block_replacement_boundary_table() {
        let (bs, be) = (10usize, 18usize);
        // (new_start, new_len, expected, label)
        let cases = [
            (5usize, 3usize, false, "fully before (5..=7)"),
            (8, 3, true, "touches bs exactly (8..=10)"),
            (8, 2, false, "ends one line before bs (8..=9)"),
            (12, 2, true, "interior (12..=13)"),
            (18, 1, true, "touches be exactly (18..=18)"),
            (19, 1, false, "starts one line after be (19..=19)"),
            (5, 20, true, "spans the whole block (5..=24)"),
            (10, 1, true, "single line at bs (10..=10)"),
        ];
        for (new_start, new_len, expected, label) in cases {
            assert_eq!(
                hunk_touches_block(bs, be, &hunk(new_start, new_len)),
                expected,
                "replacement case: {label}",
            );
        }
    }

    // ----- hunk_touches_block: zero-count deletion point-touch table -----

    #[test]
    fn hunk_touches_block_deletion_point_touch_table() {
        let (bs, be) = (10usize, 18usize);
        // Deletion gap at `new_start + 0.5`; touches iff new_start ∈ [bs-1, be].
        let cases = [
            (8usize, false, "two before bs"),
            (9, true, "one before bs — leading-edge deletion touches"),
            (10, true, "at bs"),
            (14, true, "interior"),
            (18, true, "at be"),
            (19, false, "one after be"),
        ];
        for (new_start, expected, label) in cases {
            assert_eq!(
                hunk_touches_block(bs, be, &del(new_start)),
                expected,
                "deletion case: {label}",
            );
        }
    }

    #[test]
    fn hunk_touches_block_deletion_saturates_at_line_one() {
        // Block at the very top of the file: `bs - 1` must not underflow.
        assert!(
            hunk_touches_block(1, 5, &del(0)),
            "del(0) touches a block at line 1"
        );
        assert!(hunk_touches_block(1, 5, &del(1)));
        assert!(
            !hunk_touches_block(1, 5, &del(6)),
            "del past be does not touch"
        );
    }

    // ----- block_aligns_with_hunks: the N7b drift guard -----

    const ALIGN_RAW: &str = "  - name: t\n    model: m\n    given: []"; // lines bs..bs+2

    #[test]
    fn block_aligns_when_added_lines_match_working_tree() {
        let block = block_at(ALIGN_RAW, 2); // [2, 4]
        // Replace line 3 with the line that is actually there → aligned.
        assert!(block_aligns_with_hunks(
            &block,
            &[repl(3, &["    model: m"])]
        ));
    }

    #[test]
    fn block_misaligns_when_added_line_offset_is_wrong() {
        let block = block_at(ALIGN_RAW, 2); // [2, 4]
        // Claims "    model: m" at line 2, but line 2 is "  - name: t".
        assert!(!block_aligns_with_hunks(
            &block,
            &[repl(2, &["    model: m"])]
        ));
    }

    #[test]
    fn block_misaligns_when_added_line_content_is_corrupted() {
        let block = block_at(ALIGN_RAW, 2); // [2, 4]
        assert!(!block_aligns_with_hunks(
            &block,
            &[repl(3, &["    model: CORRUPTED"])]
        ));
    }

    #[test]
    fn block_aligns_ignores_added_lines_outside_the_block() {
        let block = block_at("  - name: t\n    model: m", 8); // [8, 9]
        // A hunk in the file header — entirely above the block → ignored.
        assert!(block_aligns_with_hunks(&block, &[repl(1, &["version: 3"])]));
    }

    #[test]
    fn block_aligns_vacuously_for_pure_deletion_hunks() {
        let block = block_at("  - name: t\n    model: m", 2);
        // A deletion carries no `+` lines → nothing to verify → aligned.
        assert!(block_aligns_with_hunks(&block, &[del(2)]));
    }

    #[test]
    fn block_aligns_normalizes_crlf_line_endings() {
        // The slicer keeps `\r` (split('\n')); the parser strips it
        // (str::lines). A CRLF working tree must not read as stale.
        let block = block_at("  - name: t\r\n    model: m\r", 2); // ["  - name: t\r", "    model: m\r"]
        assert!(block_aligns_with_hunks(
            &block,
            &[repl(3, &["    model: m"])]
        ));
    }

    // ----- block_changed_by_hunks: the four-branch decision -----

    const FOUR_BRANCH_RAW: &str = "  - name: t\n    model: m\n    given: []"; // [8, 10]

    #[test]
    fn block_changed_keeps_when_block_is_absent() {
        // Conservative keep: no span to narrow against.
        assert!(block_changed_by_hunks(None, &[del(5)]));
        assert!(block_changed_by_hunks(None, &[]));
    }

    #[test]
    fn block_changed_keeps_when_present_aligned_and_a_hunk_touches() {
        let block = block_at(FOUR_BRANCH_RAW, 8); // [8, 10]
        // Aligned (line 9 == block line 2) and inside the block → keep.
        assert!(block_changed_by_hunks(
            Some(&block),
            &[repl(9, &["    model: m"])]
        ));
    }

    #[test]
    fn block_changed_drops_when_present_aligned_and_no_hunk_touches() {
        // The narrowing path: the only edit is outside this test's block.
        let block = block_at(FOUR_BRANCH_RAW, 8); // [8, 10]
        assert!(!block_changed_by_hunks(
            Some(&block),
            &[repl(1, &["version: 3"])]
        ));
    }

    #[test]
    fn block_changed_keeps_when_present_but_hunks_are_stale() {
        // N7b mismatch → degrade to file-granular (keep), even though the
        // stale hunk's position would otherwise touch the block.
        let block = block_at(FOUR_BRANCH_RAW, 8); // [8, 10]
        assert!(block_changed_by_hunks(
            Some(&block),
            &[repl(9, &["    model: COMPLETELY_DIFFERENT"])],
        ));
    }

    // ----- refine_changed_by_hunks: the thin loop + invariants -----

    use crate::domain::manifest::{DependsOn, ManifestMetadata, NodeId};
    use crate::domain::unit_test::{UnitTest, UnitTestExpect};

    fn ut(name: &str, ofp: &str) -> UnitTest {
        UnitTest::new(
            name.to_owned(),
            NodeId::new("model.shop.m"),
            Vec::new(),
            UnitTestExpect::new(serde_json::Value::Null, None),
            None,
            DependsOn::default(),
            None,
            None,
            Some(ofp.to_owned()),
        )
    }

    #[test]
    fn refine_narrows_drops_keeps_and_preserves_the_subset_invariant() {
        // Three file-granular `changed` tests:
        //   test_in      — _a.yml, a hunk lands inside its block        → KEEP
        //   test_out     — _a.yml, the only hunk is outside its block   → DROP
        //   test_noblock — _b.yml, the slicer produced no block         → KEEP (conservative)
        let in_id = "unit_test.shop.m.test_in";
        let out_id = "unit_test.shop.m.test_out";
        let noblock_id = "unit_test.shop.m.test_noblock";

        let mut tests = HashMap::new();
        tests.insert(in_id.to_owned(), ut("test_in", "models/_a.yml"));
        tests.insert(out_id.to_owned(), ut("test_out", "models/_a.yml"));
        tests.insert(noblock_id.to_owned(), ut("test_noblock", "models/_b.yml"));
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            tests,
            HashMap::new(),
        );

        // _a.yml: one hunk replacing line 2 with test_in's body line. It
        // touches test_in's block [1,3] but not test_out's block [5,7].
        let diff = PrDiff {
            files: vec![
                FileHunks {
                    path: "models/_a.yml".to_owned(),
                    hunks: vec![repl(2, &["    model: m"])],
                },
                FileHunks {
                    path: "models/_b.yml".to_owned(),
                    hunks: vec![repl(1, &["    name: test_noblock"])],
                },
            ],
        };
        let index = NormalizedDiffIndex::new(&diff, None);

        let mut blocks = HashMap::new();
        blocks.insert(
            in_id.to_owned(),
            block_at("  - name: test_in\n    model: m\n    given: []", 1), // [1,3]
        );
        blocks.insert(
            out_id.to_owned(),
            block_at("  - name: test_out\n    model: m\n    given: []", 5), // [5,7]
        );
        // test_noblock: intentionally NO entry — the slicer soft-failed.

        let changed: InScopeSet = [in_id.to_owned(), out_id.to_owned(), noblock_id.to_owned()]
            .into_iter()
            .collect();

        let refined = refine_changed_by_hunks(&current, &changed, &blocks, &index);

        assert!(refined.contains(in_id), "in-block hunk keeps test_in");
        assert!(
            !refined.contains(out_id),
            "outside-only hunk narrows test_out to context"
        );
        assert!(
            refined.contains(noblock_id),
            "absent block keeps conservatively"
        );

        // changed′ ⊆ changed — the narrowing only ever removes ids.
        for id in refined.iter() {
            assert!(
                changed.contains(id),
                "refined id {id} must be in changed (changed′ ⊆ changed)",
            );
        }
    }

    #[test]
    fn refine_correspondence_every_changed_test_resolves_to_at_least_one_hunk() {
        // The structural correspondence the narrowing rests on: every
        // file-granular `changed` id's declaring file is a key in the diff
        // index and resolves to ≥1 hunk — so refine always has the right
        // hunks to consult (no silent file-granular revert from a lookup miss).
        let a_id = "unit_test.shop.m.test_a";
        let b_id = "unit_test.shop.m.test_b";
        let mut tests = HashMap::new();
        tests.insert(a_id.to_owned(), ut("test_a", "models/_a.yml"));
        tests.insert(b_id.to_owned(), ut("test_b", "models/_b.yml"));
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            tests,
            HashMap::new(),
        );

        let diff = PrDiff {
            files: vec![
                FileHunks {
                    path: "models/_a.yml".to_owned(),
                    hunks: vec![repl(2, &["    model: m"])],
                },
                FileHunks {
                    path: "models/_b.yml".to_owned(),
                    hunks: vec![repl(4, &["    model: m"]), repl(9, &["    given: []"])],
                },
            ],
        };
        let index = NormalizedDiffIndex::new(&diff, None);

        let changed: InScopeSet = [a_id.to_owned(), b_id.to_owned()].into_iter().collect();
        for id in changed.iter() {
            let ofp = current
                .unit_test(id)
                .and_then(UnitTest::original_file_path)
                .expect("a changed test always has a declaring original_file_path");
            assert!(
                index.contains_changed(ofp),
                "changed id {id}'s declaring file {ofp} must be in the diff index",
            );
            assert!(
                !index.hunks_for(ofp).is_empty(),
                "changed id {id} must resolve to ≥1 hunk via hunks_for",
            );
        }
    }
}
