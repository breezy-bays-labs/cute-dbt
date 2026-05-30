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

// ---------------------------------------------------------------------
// Inline YAML block diff reconstruction (cute-dbt#96 concern 2)
// ---------------------------------------------------------------------

/// The kind of a reconstructed diff line.
///
/// Serializes lowercase (`"context"` / `"removed"` / `"added"`) — the exact
/// tokens the report's `renderYamlDiff` JS switches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffLineKind {
    /// Unchanged line, present on both sides of the edit.
    Context,
    /// Removed line (`-`), present only on the pre-edit side.
    Removed,
    /// Added line (`+`), present only on the post-edit (working-tree) side.
    Added,
}

/// One line of a reconstructed inline YAML-block diff.
///
/// Additive POD (ADR-5), `Serialize`/`Deserialize` so it inlines into the
/// report payload alongside [`crate::domain::unit_test_yaml::UnitTestYamlBlock`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    /// Whether the line is context, removed, or added.
    pub kind: DiffLineKind,
    /// The line text — `\r`-trimmed, no `\n`, no `+`/`-` sigil. Offsets in
    /// [`emphasis`](Self::emphasis) are codepoint indices into this string,
    /// so the report's JS must slice on codepoints (`Array.from`), not
    /// UTF-16 units, to stay aligned.
    pub text: String,
    /// Optional intra-line emphasis: the codepoint `[start, end)` range of
    /// [`text`](Self::text) that actually changed in a single-line
    /// replacement (common-prefix/suffix trimmed). `None` (serialized as
    /// JSON `null`) for context lines, multi-line edits, and the unchanged
    /// side of a pure insertion/deletion. Serialized as a two-element array
    /// `[start, end]` otherwise — the `<strong>` span `renderYamlDiff` wraps.
    pub emphasis: Option<(usize, usize)>,
}

/// A reconstructed inline diff of one `unit_test`'s authored YAML block.
///
/// Additive POD (ADR-5). The full block is rendered as ordered
/// [`DiffLine`]s (context + added from the working-tree block, removed
/// spliced in from the diff hunks), so the drawer can show the edit in
/// place rather than just the post-edit text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YamlBlockDiff {
    /// The block's diff lines, top to bottom.
    pub lines: Vec<DiffLine>,
}

/// The codepoint `[start, end)` span of `line` that differs from `other`,
/// found by trimming the common prefix and common suffix.
///
/// Returns `None` when the two are equal, or when `line` contributes no
/// changed codepoints (e.g. it is the shorter side of a pure
/// insertion/deletion — all of its content is shared affix). Symmetric in
/// its inputs only up to the start offset: `intra_line_span(a, b)` and
/// `intra_line_span(b, a)` share the prefix but end at each argument's own
/// length-minus-suffix, so a 1:1 replacement calls it once per side.
///
/// Callers pass `\r`-trimmed strings (the working-tree block line and the
/// diff body): the block slicer keeps a trailing `\r` (`split('\n')`) while
/// the diff parser strips it (`str::lines`), and an untrimmed `\r` on one
/// side alone would shrink the common suffix and inflate the span by one.
#[must_use]
pub fn intra_line_span(line: &str, other: &str) -> Option<(usize, usize)> {
    if line == other {
        return None;
    }
    let a: Vec<char> = line.chars().collect();
    let b: Vec<char> = other.chars().collect();
    let mut prefix = 0;
    while prefix < a.len() && prefix < b.len() && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let (start, end) = (prefix, a.len() - suffix);
    (start != end).then_some((start, end))
}

/// Reconstruct an inline diff of each updated test's authored YAML block.
///
/// For every id in `changed`, resolve its sliced block (`blocks`) and the
/// hunks touching its declaring file ([`NormalizedDiffIndex::hunks_for`]).
/// Emit a [`YamlBlockDiff`] **only** when the block is present, the hunks
/// still align with it ([`block_aligns_with_hunks`]), and at least one hunk
/// touches it ([`hunk_touches_block`]) — i.e. this test's own definition was
/// edited in the diff. Absent / stale / untouched ids get no entry, so the
/// drawer falls back to the plain authored-YAML view (the entry's presence
/// is therefore exactly "this test's own block changed"). Runs only on the
/// `PrDiff` arm — baseline mode has no hunks to reconstruct from.
///
/// The run loop always passes the default-hasher `authoring_yaml` map, so
/// generalizing `blocks` over the hasher (`clippy::implicit_hasher`) buys
/// nothing — same rationale as [`refine_changed_by_hunks`].
#[allow(clippy::implicit_hasher)]
#[must_use]
pub fn reconstruct_block_diffs(
    current: &Manifest,
    changed: &InScopeSet,
    blocks: &HashMap<String, UnitTestYamlBlock>,
    index: &NormalizedDiffIndex,
) -> HashMap<String, YamlBlockDiff> {
    let mut out = HashMap::new();
    for id in changed.iter() {
        let Some(block) = blocks.get(id) else {
            continue; // no slice → plain drawer
        };
        let hunks = current
            .unit_test(id)
            .and_then(UnitTest::original_file_path)
            .map_or(&[][..], |ofp| index.hunks_for(ofp));
        if !block_aligns_with_hunks(block, hunks) {
            continue; // stale diff → plain drawer
        }
        let touching: Vec<&Hunk> = hunks
            .iter()
            .filter(|h| hunk_touches_block(block.block_start, block.block_end, h))
            .collect();
        if touching.is_empty() {
            continue; // change is elsewhere in the file → plain drawer
        }
        out.insert(id.to_owned(), reconstruct_one(block, &touching));
    }
    out
}

/// Reconstruct one block's diff from its working-tree slice + the hunks
/// already filtered to those touching it. See [`reconstruct_block_diffs`].
fn reconstruct_one(block: &UnitTestYamlBlock, touching: &[&Hunk]) -> YamlBlockDiff {
    fn trim_cr(s: &str) -> String {
        s.trim_end_matches('\r').to_owned()
    }
    let block_lines: Vec<String> = block.raw.split('\n').map(trim_cr).collect();
    let (bs, be) = (block.block_start, block.block_end);

    // Removed-line groups to splice immediately before a given new-side
    // line. Anchored before `new_start` for a replacement (new_len >= 1) and
    // after it for a pure deletion (new_len == 0), then clamped into the
    // block so leading/trailing edits render at the block's top/bottom.
    // Per-line intra-line emphasis for the single added line of a clean 1:1
    // replacement (one removed + one added line).
    let mut splice_before: HashMap<usize, Vec<DiffLine>> = HashMap::new();
    let mut added_emphasis: HashMap<usize, (usize, usize)> = HashMap::new();
    for h in touching {
        let clean_1to1 = h.new_len == 1 && h.removed_lines.len() == 1;
        let anchor = if h.new_len == 0 {
            h.new_start + 1
        } else {
            h.new_start
        }
        .clamp(bs, be + 1);
        let removed: Vec<DiffLine> = h
            .removed_lines
            .iter()
            .enumerate()
            .map(|(i, r)| DiffLine {
                kind: DiffLineKind::Removed,
                text: trim_cr(r),
                emphasis: if clean_1to1 && i == 0 {
                    intra_line_span(&trim_cr(r), &trim_cr(&h.added_lines[0]))
                } else {
                    None
                },
            })
            .collect();
        splice_before.entry(anchor).or_default().extend(removed);
        if clean_1to1 && (bs..=be).contains(&h.new_start) {
            if let Some(e) =
                intra_line_span(&trim_cr(&h.added_lines[0]), &trim_cr(&h.removed_lines[0]))
            {
                added_emphasis.insert(h.new_start, e);
            }
        }
    }

    // A new-side line is Added when some replacement/insertion hunk covers
    // it; otherwise it is Context.
    let is_added = |l: usize| {
        touching
            .iter()
            .any(|h| h.new_len >= 1 && h.new_start <= l && l < h.new_start + h.new_len)
    };

    let mut lines: Vec<DiffLine> = Vec::new();
    for (i, text) in block_lines.into_iter().enumerate() {
        let l = bs + i; // 1-based new-side line number
        if let Some(group) = splice_before.remove(&l) {
            lines.extend(group);
        }
        lines.push(if is_added(l) {
            DiffLine {
                kind: DiffLineKind::Added,
                text,
                emphasis: added_emphasis.get(&l).copied(),
            }
        } else {
            DiffLine {
                kind: DiffLineKind::Context,
                text,
                emphasis: None,
            }
        });
    }
    // Trailing deletions clamped to just past the block's last line.
    if let Some(group) = splice_before.remove(&(be + 1)) {
        lines.extend(group);
    }

    YamlBlockDiff { lines }
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
    fn block_aligns_checks_each_added_line_at_its_own_offset() {
        let block = block_at(ALIGN_RAW, 2); // [2, 4]
        // A two-line hunk whose added bodies match the working tree at
        // lines 3 AND 4. The second body is verified at offset `new_start +
        // 1`, not `new_start - 1` — pins the per-line index arithmetic
        // (`file_line = new_start + k`) so a `+`→`-` slip on `k` is caught.
        assert!(block_aligns_with_hunks(
            &block,
            &[repl(3, &["    model: m", "    given: []"])]
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

    // =================================================================
    // Inline YAML block diff reconstruction (cute-dbt#96 concern 2)
    // =================================================================

    // A replacement hunk carrying BOTH sides' bodies (the reconstruction
    // needs `removed_lines`, unlike the touch/alignment `repl` helper).
    fn replace(new_start: usize, removed: &[&str], added: &[&str]) -> Hunk {
        Hunk {
            new_start,
            new_len: added.len(),
            removed_lines: removed.iter().map(|s| (*s).to_owned()).collect(),
            added_lines: added.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    // A pure deletion at `new_start` carrying explicit removed bodies.
    fn delete(new_start: usize, removed: &[&str]) -> Hunk {
        Hunk {
            new_start,
            new_len: 0,
            removed_lines: removed.iter().map(|s| (*s).to_owned()).collect(),
            added_lines: Vec::new(),
        }
    }

    fn ctx(text: &str) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Context,
            text: text.to_owned(),
            emphasis: None,
        }
    }
    fn rem(text: &str) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Removed,
            text: text.to_owned(),
            emphasis: None,
        }
    }
    fn add(text: &str) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Added,
            text: text.to_owned(),
            emphasis: None,
        }
    }
    fn rem_e(text: &str, e: (usize, usize)) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Removed,
            text: text.to_owned(),
            emphasis: Some(e),
        }
    }
    fn add_e(text: &str, e: (usize, usize)) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Added,
            text: text.to_owned(),
            emphasis: Some(e),
        }
    }

    // ----- intra_line_span: common-affix codepoint range -----

    #[test]
    fn intra_line_span_identical_lines_have_no_emphasis() {
        assert_eq!(
            intra_line_span("    model: orders", "    model: orders"),
            None
        );
    }

    #[test]
    fn intra_line_span_marks_the_changed_middle_per_side() {
        // Shared prefix "    model: " (11) + shared suffix "s" (1).
        let removed = "    model: payments";
        let added = "    model: orders";
        // removed side: [11, 19-1) = "payment"
        assert_eq!(intra_line_span(removed, added), Some((11, 18)));
        // added side:   [11, 17-1) = "order"
        assert_eq!(intra_line_span(added, removed), Some((11, 16)));
    }

    #[test]
    fn intra_line_span_pure_append_emphasizes_only_the_longer_side() {
        assert_eq!(intra_line_span("abc", "abcd"), None); // shorter side: nothing of its own changed
        assert_eq!(intra_line_span("abcd", "abc"), Some((3, 4))); // the trailing "d"
    }

    #[test]
    fn intra_line_span_prepend_emphasizes_the_leading_diff() {
        assert_eq!(intra_line_span("abc", "bc"), Some((0, 1))); // leading "a"
    }

    #[test]
    fn intra_line_span_counts_codepoints_not_bytes() {
        // "café " is 5 codepoints (é is 2 bytes); the changed char is at 5.
        assert_eq!(intra_line_span("café x", "café y"), Some((5, 6)));
    }

    // ----- reconstruct_one: the ordered DiffLine table -----

    #[test]
    fn reconstruct_single_line_replacement_carries_intra_line_emphasis() {
        let block = block_at(
            "  - name: test_orders\n    model: payments\n    given: []",
            10,
        ); // [10,12]
        let hunks = [replace(
            11,
            &["    model: payments"],
            &["    model: orders"],
        )];
        // raw already holds the post-edit (working-tree) line at 11, so the
        // hunk's added body matches it. Old "payments" → new "orders".
        let block = UnitTestYamlBlock {
            raw: "  - name: test_orders\n    model: orders\n    given: []".to_owned(),
            ..block
        };
        let got = reconstruct_one(&block, &hunks.iter().collect::<Vec<_>>());
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: test_orders"),
                rem_e("    model: payments", (11, 18)),
                add_e("    model: orders", (11, 16)),
                ctx("    given: []"),
            ],
        );
    }

    #[test]
    fn reconstruct_trims_cr_on_both_sides_keeping_offsets_correct() {
        // CRLF working tree: the slicer keeps `\r` (split('\n')), the diff
        // parser strips it (str::lines). Without trimming BOTH, the added
        // line text would carry `\r` and the emphasis suffix would collapse
        // (off-by-one span). Block raw lines end in `\r`; hunk bodies don't.
        let block = UnitTestYamlBlock {
            raw: "  - name: t\r\n    model: orders\r\n    given: []\r".to_owned(),
            line_of_name: 20,
            block_start: 20,
            block_end: 22,
        };
        let hunks = [replace(
            21,
            &["    model: payments"],
            &["    model: orders"],
        )];
        let got = reconstruct_one(&block, &hunks.iter().collect::<Vec<_>>());
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: t"),
                rem_e("    model: payments", (11, 18)),
                add_e("    model: orders", (11, 16)),
                ctx("    given: []"),
            ],
            "text must be \\r-trimmed and emphasis offsets unaffected by line endings",
        );
    }

    #[test]
    fn reconstruct_pure_deletion_splices_removed_after_its_line() {
        let block = block_at("  - name: t\n    model: m\n    given: []", 10); // [10,12]
        // Deletion gap after new-side line 11 → removed renders between 11 and 12.
        let hunks = [delete(11, &["      # note"])];
        let got = reconstruct_one(&block, &hunks.iter().collect::<Vec<_>>());
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: t"),
                ctx("    model: m"),
                rem("      # note"),
                ctx("    given: []"),
            ],
        );
    }

    #[test]
    fn reconstruct_leading_edge_deletion_renders_removed_first() {
        // new_start = block_start - 1 → anchor clamps to the top.
        let block = block_at("  - name: t\n    model: m\n    given: []", 10); // [10,12]
        let hunks = [delete(9, &["  # leading"])];
        let got = reconstruct_one(&block, &hunks.iter().collect::<Vec<_>>());
        assert_eq!(
            got.lines,
            vec![
                rem("  # leading"),
                ctx("  - name: t"),
                ctx("    model: m"),
                ctx("    given: []"),
            ],
        );
    }

    #[test]
    fn reconstruct_trailing_edge_deletion_renders_removed_last() {
        // new_start = block_end → anchor clamps just past the last line.
        let block = block_at("  - name: t\n    model: m\n    given: []", 10); // [10,12]
        let hunks = [delete(12, &["  # trailing"])];
        let got = reconstruct_one(&block, &hunks.iter().collect::<Vec<_>>());
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: t"),
                ctx("    model: m"),
                ctx("    given: []"),
                rem("  # trailing"),
            ],
        );
    }

    #[test]
    fn reconstruct_replacement_straddling_the_top_edge_clamps_removed_to_top() {
        // new_start (9) < block_start (10) ≤ new_end (10): line 10 is Added,
        // the removed pair clamps to the block top.
        let block = block_at("    model: m2\n    given: []\n      rows: []", 10); // [10,12]
        let hunks = [replace(9, &["old8", "old9"], &["new9", "    model: m2"])];
        let got = reconstruct_one(&block, &hunks.iter().collect::<Vec<_>>());
        assert_eq!(
            got.lines,
            vec![
                rem("old8"),
                rem("old9"),
                add("    model: m2"), // line 10, inside the hunk's added range
                ctx("    given: []"),
                ctx("      rows: []"),
            ],
        );
    }

    #[test]
    fn reconstruct_two_hunks_in_one_block_render_in_ascending_order() {
        let block = block_at("  - name: t\nnewA\n    given: []\nnewB\n      rows: []", 10); // [10,14]
        // hunks_for returns diff order (ascending new_start); one forward pass.
        let hunks = [
            replace(11, &["oldA"], &["newA"]),
            replace(13, &["oldB"], &["newB"]),
        ];
        let got = reconstruct_one(&block, &hunks.iter().collect::<Vec<_>>());
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: t"),
                rem_e("oldA", (0, 3)),
                add_e("newA", (0, 3)),
                ctx("    given: []"),
                rem_e("oldB", (0, 3)),
                add_e("newB", (0, 3)),
                ctx("      rows: []"),
            ],
        );
    }

    #[test]
    fn reconstruct_multi_line_replacement_has_no_intra_line_emphasis() {
        // new_len = 2 (not a clean 1:1) → line-level +/- only, no <strong>.
        let block = block_at("  - name: t\nnewA\nnewB\n    given: []", 10); // [10,13]
        let hunks = [replace(11, &["oldA", "oldB"], &["newA", "newB"])];
        let got = reconstruct_one(&block, &hunks.iter().collect::<Vec<_>>());
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: t"),
                rem("oldA"),
                rem("oldB"),
                add("newA"),
                add("newB"),
                ctx("    given: []"),
            ],
        );
    }

    // ----- reconstruct_block_diffs: the gating (present AND aligned AND touched) -----

    #[test]
    fn reconstruct_block_diffs_emits_only_for_edited_own_blocks() {
        let edit_id = "unit_test.shop.m.t_edit";
        let absent_id = "unit_test.shop.m.t_absent";
        let stale_id = "unit_test.shop.m.t_stale";
        let untouched_id = "unit_test.shop.m.t_untouched";

        let mut tests = HashMap::new();
        tests.insert(edit_id.to_owned(), ut("t_edit", "models/_a.yml"));
        tests.insert(absent_id.to_owned(), ut("t_absent", "models/_b.yml"));
        tests.insert(stale_id.to_owned(), ut("t_stale", "models/_c.yml"));
        tests.insert(untouched_id.to_owned(), ut("t_untouched", "models/_d.yml"));
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            tests,
            HashMap::new(),
        );

        let diff = PrDiff {
            files: vec![
                // _a.yml: hunk replaces line 2 with t_edit's working-tree
                // body line → aligned + touches block [1,3].
                FileHunks {
                    path: "models/_a.yml".to_owned(),
                    hunks: vec![replace(2, &["    model: was"], &["    model: m"])],
                },
                // _b.yml: a hunk exists (so t_absent stays `changed`), but no
                // block is sliced for t_absent.
                FileHunks {
                    path: "models/_b.yml".to_owned(),
                    hunks: vec![replace(2, &["    model: was"], &["    model: m"])],
                },
                // _c.yml: hunk's added body does NOT match t_stale's block
                // line at that position → misaligned (stale diff).
                FileHunks {
                    path: "models/_c.yml".to_owned(),
                    hunks: vec![replace(2, &["    model: was"], &["    model: DRIFTED"])],
                },
                // _d.yml: the only hunk is outside t_untouched's block [10,12].
                FileHunks {
                    path: "models/_d.yml".to_owned(),
                    hunks: vec![replace(2, &["    model: was"], &["    model: m"])],
                },
            ],
        };
        let index = NormalizedDiffIndex::new(&diff, None);

        let mut blocks = HashMap::new();
        blocks.insert(
            edit_id.to_owned(),
            block_at("  - name: t_edit\n    model: m\n    given: []", 1), // [1,3]
        );
        // t_absent: intentionally NO block.
        blocks.insert(
            stale_id.to_owned(),
            block_at("  - name: t_stale\n    model: m\n    given: []", 1), // [1,3]
        );
        blocks.insert(
            untouched_id.to_owned(),
            block_at("  - name: t_untouched\n    model: m\n    given: []", 10), // [10,12]
        );

        let changed: InScopeSet = [
            edit_id.to_owned(),
            absent_id.to_owned(),
            stale_id.to_owned(),
            untouched_id.to_owned(),
        ]
        .into_iter()
        .collect();

        let diffs = reconstruct_block_diffs(&current, &changed, &blocks, &index);

        assert!(
            diffs.contains_key(edit_id),
            "edited own block → diff emitted"
        );
        assert!(
            !diffs.contains_key(absent_id),
            "absent block → no diff (plain drawer)"
        );
        assert!(
            !diffs.contains_key(stale_id),
            "misaligned (stale) diff → no diff"
        );
        assert!(
            !diffs.contains_key(untouched_id),
            "change outside the block → no diff",
        );

        // The emitted diff shows the in-place edit of line 2.
        assert_eq!(
            diffs[edit_id].lines,
            vec![
                ctx("  - name: t_edit"),
                rem_e("    model: was", (11, 14)), // "was"
                add_e("    model: m", (11, 12)),   // "m"
                ctx("    given: []"),
            ],
        );
    }

    // ----- YamlBlockDiff serde: the exact JS wire shape -----

    #[test]
    fn yaml_block_diff_serializes_to_the_exact_renderyamldiff_contract() {
        let diff = YamlBlockDiff {
            lines: vec![
                ctx("  - name: t"),
                rem_e("    model: payments", (11, 18)),
                add_e("    model: orders", (11, 16)),
            ],
        };
        let json = serde_json::to_string(&diff).unwrap();
        assert_eq!(
            json,
            r#"{"lines":[{"kind":"context","text":"  - name: t","emphasis":null},{"kind":"removed","text":"    model: payments","emphasis":[11,18]},{"kind":"added","text":"    model: orders","emphasis":[11,16]}]}"#,
        );
        // wire-shape round-trips back to the same POD.
        let back: YamlBlockDiff = serde_json::from_str(&json).unwrap();
        assert_eq!(back, diff);
    }
}
