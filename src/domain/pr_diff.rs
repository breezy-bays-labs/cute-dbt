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

use crate::domain::manifest::{Manifest, Node};
use crate::domain::model_yaml::ModelYamlOutcome;
use crate::domain::path::normalize_path;
use crate::domain::state::{InScopeSet, ModelInScopeSet};
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
    /// Git-detected renames — the `rename from <old>` / `rename to <new>`
    /// extended-header pairs (cute-dbt#80). A **pure** rename (100%
    /// similarity) carries no `---`/`+++` headers and no hunks, so it
    /// appears here and NOT in [`files`](Self::files); a rename **with**
    /// edits appears in both (the new path's [`FileHunks`] entry carries
    /// the hunks). `#[serde(default)]` keeps pre-rename payloads
    /// deserializable; `skip_serializing_if` keeps rename-free payloads
    /// byte-identical to the pre-#80 shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub renames: Vec<RenamePair>,
}

/// One git-detected rename, as named by a `rename from`/`rename to`
/// extended-header pair (cute-dbt#80).
///
/// Both paths are repo-relative and verbatim — git emits them with no
/// `a/`/`b/` prefix (unlike the `---`/`+++` headers), unquoted even when
/// they contain spaces. Paths with non-ASCII / control characters are
/// C-quoted by git (`core.quotePath`) and are **not** dequoted here — the
/// same fidelity level as the `+++ b/<path>` parser.
///
/// Additive POD (ADR-5). `Serialize`/`Deserialize` so the pairing survives
/// the payload round-trip — the rename *lineage* (old name ↔ new name) is
/// what a future report affordance would surface; the scope keyset alone
/// flattens it away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenamePair {
    /// The old (pre-rename) repo-relative path.
    pub from: String,
    /// The new (post-rename) repo-relative path.
    pub to: String,
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
    ///
    /// Rename pairs (cute-dbt#80) put **both** sides into the changed-file
    /// keyset: scope selection matches the **current** manifest's
    /// `original_file_path`, so post-rename only the new path resolves to
    /// a node — without this a *pure* rename (no `+++` header, no hunks)
    /// would scope nothing. The old path is kept too, conservatively: it
    /// matches no current node after a clean rename, so it is at worst
    /// inert. Hunks are never attached via the rename (a rename-with-edit
    /// already carries them on its new-path [`FileHunks`] entry, which
    /// `or_default` merges with rather than clobbers).
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
        for rename in &diff.renames {
            by_path
                .entry(normalize_path(&rename.from, strip))
                .or_default();
            by_path
                .entry(normalize_path(&rename.to, strip))
                .or_default();
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

    /// How many hunks across all files are **not** `--unified=0`-shaped —
    /// i.e. carry context lines (cute-dbt#111).
    ///
    /// A hunk's `new_len` (from the `@@` range) equals its recorded `+`
    /// body count (`added_lines.len()`) iff the diff was produced with
    /// `--unified=0`: pure insertion `N == N`, pure deletion `0 == 0`,
    /// replacement `N == N` all hold. A default (context-bearing) `git
    /// diff` drops its ` `-prefixed context lines at parse time, so
    /// `new_len > added_lines.len()`. This counts exactly those hunks.
    ///
    /// Pure computation over the parsed index (std-only, no I/O). The CLI
    /// reads it once on the `PrDiff` arm to emit a single stderr note when a
    /// user forgot `--unified=0` — inline diffs degrade to the plain view
    /// (`reconstruct_one`'s `hunk_is_unified_zero` guard), and this makes
    /// that silent degrade visible.
    #[must_use]
    pub fn context_bearing_hunk_count(&self) -> usize {
        self.by_path
            .values()
            .flatten()
            .filter(|h| h.new_len != h.added_lines.len())
            .count()
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

/// A contiguous source block addressed for diff reconstruction: the verbatim
/// text plus its 1-based inclusive new-side line span.
///
/// A borrowed function-argument helper (NOT owned/serialized POD — it never
/// enters the domain model or the wire payload, so it does not breach the
/// POD-only-domain rule). Its purpose is purely to bind `raw`,
/// `block_start`, and `block_end` into one value so the two widened
/// reconstruction entry points ([`block_aligns_with_hunks`] /
/// `reconstruct_one`) can't transpose the two adjacent `usize` bounds at a
/// call site. The `unit_test` YAML block (#96) builds it from
/// `(&block.raw, block.block_start, block.block_end)`; a model's SQL
/// `raw_code` (#111) from the whole-file span. `hunk_touches_block` keeps
/// its `(start, end, hunk)` arity (it takes no `raw`).
#[derive(Debug, Clone, Copy)]
pub struct BlockSpan<'a> {
    /// The block's verbatim text (the slicer keeps `\r`; callers pass it as
    /// authored, `split('\n')`-framed).
    pub raw: &'a str,
    /// 1-based inclusive first new-side line of the block.
    pub start: usize,
    /// 1-based inclusive last new-side line of the block.
    pub end: usize,
}

impl<'a> BlockSpan<'a> {
    /// Construct from the verbatim text and its 1-based inclusive span.
    #[must_use]
    pub fn new(raw: &'a str, start: usize, end: usize) -> Self {
        Self { raw, start, end }
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
///
/// The block is addressed by a [`BlockSpan`] — the content-agnostic shape
/// (cute-dbt#111), so the same drift guard serves the `unit_test` YAML
/// block (#96) and a model's SQL `raw_code` (#111). YAML callers build
/// `BlockSpan::new(&block.raw, block.block_start, block.block_end)`.
///
/// **Whitespace-EXACT by design.** This is the N7b revision-staleness
/// check (does the diff's new side still describe the working tree?), so a
/// whitespace divergence here is *genuine* drift and must NOT be
/// normalized. Whitespace-insensitivity ([`ws_equal`]) applies only
/// downstream of N7b, to the removed-vs-added change test in
/// `reconstruct_one`.
#[must_use]
pub fn block_aligns_with_hunks(span: &BlockSpan, hunks: &[Hunk]) -> bool {
    let block_lines: Vec<&str> = span.raw.split('\n').collect();
    for hunk in hunks {
        for (k, added) in hunk.added_lines.iter().enumerate() {
            let file_line = hunk.new_start + k; // 1-based new-side line
            if file_line < span.start || file_line > span.end {
                continue; // outside this block — not this block's concern
            }
            let offset = file_line - span.start; // 0-based into raw
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

/// Whether two lines carry identical non-whitespace content — the
/// `git --ignore-all-space` change test (cute-dbt#111).
///
/// `true` when the two lines tokenize to the same sequence of
/// whitespace-separated runs, so a re-indentation, a trailing-whitespace
/// edit, or a blank-line churn that leaves the substantive content
/// untouched is NOT treated as a change. Applied ONLY to the
/// removed-vs-added comparison inside `reconstruct_one` (downstream of
/// the whitespace-EXACT N7b guard, [`block_aligns_with_hunks`]), so a
/// removed/added pair that is `ws_equal` renders as Context — no removed
/// splice, no intra-line emphasis. std-only.
#[must_use]
pub fn ws_equal(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
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
    let span = BlockSpan::new(&block.raw, block.block_start, block.block_end);
    if !block_aligns_with_hunks(&span, hunks) {
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

/// A reconstructed inline diff of one contiguous source block.
///
/// Content-agnostic by construction — the lines are plain `&str`
/// [`DiffLine`]s, so the same POD serves both the `unit_test` authored
/// YAML block (cute-dbt#96) and a changed model's SQL `raw_code`
/// (cute-dbt#111). The JSON field that carries it on a `unit_test` is
/// still named `yaml_diff` (the #96 wire contract is unchanged); a model
/// carries it under `sql_diff`. Renamed from `YamlBlockDiff` at
/// cute-dbt#111 — the rename is internal (the lib is internal-only in
/// v0.x) and the wire shape is identical.
///
/// Additive POD (ADR-5). The full block is rendered as ordered
/// [`DiffLine`]s (context + added from the working-tree block, removed
/// spliced in from the diff hunks), so the view can show the edit in
/// place rather than just the post-edit text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockDiff {
    /// The block's diff lines, top to bottom.
    pub lines: Vec<DiffLine>,
}

impl BlockDiff {
    /// Whether this diff carries at least one substantive (non-context)
    /// line — an Added or Removed [`DiffLine`].
    ///
    /// A reconstruction whose touching hunks were all whitespace-only
    /// (cute-dbt#111) leaves an all-Context diff: the callers
    /// ([`reconstruct_block_diffs`] / [`reconstruct_model_sql_diffs`]) test
    /// this and emit no `BlockDiff` for it, so the view falls back to plain
    /// text rather than showing an identical-looking "diff".
    #[must_use]
    pub fn has_real_change(&self) -> bool {
        self.lines.iter().any(|l| l.kind != DiffLineKind::Context)
    }
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
///
/// Mutation note (classified-equivalent): replacing `prefix += 1` /
/// `suffix += 1` with `*= ` makes the counter stick at 0, so the scan loop
/// never terminates — an infinite loop, not a wrong answer. A hang is not a
/// behavioral difference a test can assert, so these two `cargo mutants`
/// survivors are equivalent by construction (the `+`/`<=` bound mutants ARE
/// killed — see `intra_line_span_suffix_stops_at_the_prefix_boundary`).
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
/// Emit a [`BlockDiff`] **only** when the block is present, the hunks
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
) -> HashMap<String, BlockDiff> {
    let mut out = HashMap::new();
    for id in changed.iter() {
        let Some(block) = blocks.get(id) else {
            continue; // no slice → plain drawer
        };
        let hunks = current
            .unit_test(id)
            .and_then(UnitTest::original_file_path)
            .map_or(&[][..], |ofp| index.hunks_for(ofp));
        let span = BlockSpan::new(&block.raw, block.block_start, block.block_end);
        if !block_aligns_with_hunks(&span, hunks) {
            continue; // stale diff → plain drawer
        }
        let touching: Vec<&Hunk> = hunks
            .iter()
            .filter(|h| hunk_touches_block(block.block_start, block.block_end, h))
            .collect();
        if touching.is_empty() {
            continue; // change is elsewhere in the file → plain drawer
        }
        let diff = reconstruct_one(&span, &touching);
        // A purely-whitespace edit (every touching hunk dropped by
        // `reconstruct_one`) leaves an all-Context diff → no drawer diff
        // (cute-dbt#111). The change-pair still kept the test `updated` via
        // `refine_changed_by_hunks` (N7b is whitespace-EXACT); only the
        // inline highlight is suppressed.
        if diff.has_real_change() {
            out.insert(id.to_owned(), diff);
        }
    }
    out
}

/// Reconstruct an inline SQL diff of each in-scope model whose `.sql`
/// changed in the PR diff (cute-dbt#111).
///
/// The sibling of [`reconstruct_block_diffs`] for a model's RAW Jinja
/// `raw_code` (the diffable source — compiled SQL is generated and
/// un-diffable). For each model in `models_in_scope` that carries
/// `raw_code` + `original_file_path`:
///
/// 1. Normalize `raw_code` to git's line frame — strip **exactly one**
///    trailing `\n` ([`str::strip_suffix`], not `trim_end_matches`). dbt
///    engines diverge here (verified 2026-05-31): dbt-core ships
///    `raw_code` already stripped, dbt-fusion ships it byte-identical but
///    retaining the file's trailing `\n`. Stripping one terminator
///    normalizes both to the same content frame, so `raw.split('\n')`
///    lines up with the diff's new-side numbering and the reconstruction
///    is engine-independent. A real blank line at EOF (`"a\n\n"`) survives
///    — only the single terminator is removed.
/// 2. Take the whole-file span `(raw, 1, raw.split('\n').count())` and the
///    hunks touching the model's declaring file
///    ([`NormalizedDiffIndex::hunks_for`]).
/// 3. Emit a [`BlockDiff`] (keyed by the model's full node id) **only**
///    when the diff still aligns ([`block_aligns_with_hunks`]), at least
///    one hunk touches the file ([`hunk_touches_block`]), and the
///    reconstruction carries a substantive change
///    ([`BlockDiff::has_real_change`] — a whitespace-only edit emits
///    nothing, cute-dbt#111). A model in scope only via a changed *test*
///    (its own `.sql` untouched) resolves to no hunks → no entry → the
///    template shows the plain SQL view.
///
/// Runs only on the [`crate::domain::scope::ScopeInput::PrDiff`] arm —
/// baseline mode has no hunks to reconstruct from (the cli threads
/// `HashMap::new()` there). ADR-3 scope selection is untouched: this only
/// *reads* `models_in_scope` and adds a render-payload field.
///
/// The run loop passes the default-hasher render set, so generalizing over
/// the hasher (`clippy::implicit_hasher`) buys nothing — same rationale as
/// [`reconstruct_block_diffs`].
#[must_use]
pub fn reconstruct_model_sql_diffs(
    current: &Manifest,
    models_in_scope: &ModelInScopeSet,
    index: &NormalizedDiffIndex,
) -> HashMap<String, BlockDiff> {
    let mut out = HashMap::new();
    for model_id in models_in_scope.iter() {
        if let Some(diff) = current
            .node(model_id)
            .and_then(|model| model_sql_diff(model, index))
        {
            out.insert(model_id.as_str().to_owned(), diff);
        }
    }
    out
}

/// The inline SQL diff for one model, or `None` when there is nothing to
/// show (no `raw_code`, no declaring path, the `.sql` not in the diff, a
/// stale diff, or a whitespace-only change). See [`reconstruct_model_sql_diffs`].
fn model_sql_diff(model: &Node, index: &NormalizedDiffIndex) -> Option<BlockDiff> {
    // Empty `raw_code` (some node types ship `raw_code: ""`) is treated as
    // absent — matches `build_model_payload`'s `raw_sql` filter, so we never
    // compute a diff the template would not show.
    let raw_code = model.raw_code().filter(|s| !s.is_empty())?;
    let ofp = model.original_file_path()?;
    // Strip git's single trailing terminator (engine-divergent — see the
    // module-level fn doc). A real blank line at EOF survives.
    let raw = raw_code.strip_suffix('\n').unwrap_or(raw_code);
    // Whole-file span: 1..=line count.
    let span = BlockSpan::new(raw, 1, raw.split('\n').count());

    let hunks = index.hunks_for(ofp);
    if !block_aligns_with_hunks(&span, hunks) {
        return None; // stale diff (N7b) → plain SQL view
    }
    let touching: Vec<&Hunk> = hunks
        .iter()
        .filter(|h| hunk_touches_block(span.start, span.end, h))
        .collect();
    if touching.is_empty() {
        return None; // .sql not touched (model in scope via a changed test)
    }
    let diff = reconstruct_one(&span, &touching);
    // Whitespace-only model edit → all-Context → no diff (plain SQL).
    diff.has_real_change().then_some(diff)
}

/// Attach an inline diff to each gathered Model-YAML block the PR diff
/// genuinely edited (cute-dbt#247).
///
/// The [`reconstruct_block_diffs`] sibling for the Model-YAML drawer,
/// operating **in place** on the cli's `gather_model_yaml` outcome map
/// (keyed by model node id). For each [`ModelYamlOutcome::Found`] entry,
/// resolve the hunks touching its schema file — the entry's `path` is the
/// scheme-stripped manifest `patch_path`, project-relative exactly like
/// an `original_file_path`, so [`NormalizedDiffIndex::hunks_for`] keys it
/// identically — and attach a [`BlockDiff`] **only** when the sliced
/// block is aligned ([`block_aligns_with_hunks`]), touched
/// ([`hunk_touches_block`]), and the reconstruction carries a substantive
/// change ([`BlockDiff::has_real_change`] — a whitespace-only edit
/// attaches nothing, cute-dbt#111). Every other entry (degrade variants,
/// untouched/stale/whitespace-only blocks) passes through unchanged, so
/// the section falls back to the plain File view.
///
/// Runs only on the [`crate::domain::scope::ScopeInput::PrDiff`] arm —
/// baseline mode has no hunks to reconstruct from (the cli skips the
/// call there).
///
/// The run loop passes the default-hasher gather map, so generalizing
/// over the hasher (`clippy::implicit_hasher`) buys nothing — same
/// rationale as [`reconstruct_block_diffs`].
#[allow(clippy::implicit_hasher)]
pub fn attach_model_yaml_diffs(
    model_yaml: &mut HashMap<String, ModelYamlOutcome>,
    index: &NormalizedDiffIndex,
) {
    for outcome in model_yaml.values_mut() {
        let ModelYamlOutcome::Found { path, block, diff } = outcome else {
            continue; // degrade variants have no block to diff
        };
        let hunks = index.hunks_for(path);
        let span = BlockSpan::new(&block.raw, block.block_start, block.block_end);
        if !block_aligns_with_hunks(&span, hunks) {
            continue; // stale diff (N7b) → plain File view
        }
        let touching: Vec<&Hunk> = hunks
            .iter()
            .filter(|h| hunk_touches_block(block.block_start, block.block_end, h))
            .collect();
        if touching.is_empty() {
            continue; // change is elsewhere in the file → plain File view
        }
        let reconstructed = reconstruct_one(&span, &touching);
        // Whitespace-only edit → all-Context → no diff (cute-dbt#111).
        if reconstructed.has_real_change() {
            *diff = Some(reconstructed);
        }
    }
}

/// Trim a trailing `\r` (CRLF tolerance — see [`block_aligns_with_hunks`]).
fn trim_cr(s: &str) -> String {
    s.trim_end_matches('\r').to_owned()
}

/// Within an aligned 1:1 replacement hunk (`removed.len() == added.len()`),
/// the new-side line offset `i` (0-based) is whitespace-only when
/// `removed[i]` is [`ws_equal`] to `added[i]` (cute-dbt#111).
///
/// Whitespace-insensitivity is **per line-pair**, not per-hunk: under
/// `git --unified=0` a re-indent and a real edit on *adjacent* lines arrive
/// as ONE hunk, so dropping the whole hunk would either leak the re-indent
/// as a change or hide the real edit. Pairing line-for-line, a re-indent /
/// trailing-whitespace / blank-churn pair renders as Context (no removed
/// splice, no emphasis) while a substantive pair in the same hunk still
/// diffs. Only meaningful for equal-count hunks; a genuine insertion /
/// deletion (unequal counts) has no pairing and is always a real change.
fn pair_is_ws_only(h: &Hunk, i: usize) -> bool {
    // No `\r`-trim needed: `ws_equal` compares `split_whitespace()` token
    // sequences, which already ignore a trailing `\r` (cute-dbt#113 review).
    h.removed_lines.len() == h.added_lines.len() && ws_equal(&h.removed_lines[i], &h.added_lines[i])
}

/// The new-side edits a set of touching hunks impose on a block: which
/// removed-line groups to splice before each 1-based new-side line, which
/// new-side lines are genuine (non-whitespace) additions, and the per-pair
/// intra-line emphasis spans for an aligned replacement's added lines.
#[derive(Default)]
struct HunkEdits {
    splice_before: HashMap<usize, Vec<DiffLine>>,
    added_real: std::collections::HashSet<usize>,
    added_emphasis: HashMap<usize, (usize, usize)>,
}

/// Whether a hunk is an ALIGNED replacement: equal, non-empty removed/added
/// line counts (cute-dbt#132, generalizing the former 1:1-only gate). Only
/// then is there a sound positional pairing `removed[i] ↔ added[i]` for
/// per-line intra-line emphasis. The `removed.len() == added.len()` equality
/// is load-bearing: a malformed diff can declare `new_len: N` with a shorter
/// `+` body, and the emphasis branch indexes both `removed_lines[i]` AND
/// `added_lines[i]` — equal, non-empty counts keep every index in bounds
/// (cute-dbt#110 review: cute-dbt never panics on a bad diff). A pure
/// insertion/deletion (unequal counts) has no pairing, hence no emphasis.
fn is_aligned_replacement(h: &Hunk) -> bool {
    !h.removed_lines.is_empty() && h.removed_lines.len() == h.added_lines.len()
}

/// The removed [`DiffLine`]s a hunk contributes, with ws-only pairs
/// dropped (cute-dbt#111) and each surviving removed line of an aligned
/// replacement carrying its per-pair intra-line emphasis (cute-dbt#132).
fn removed_diff_lines(h: &Hunk) -> Vec<DiffLine> {
    let aligned = is_aligned_replacement(h);
    h.removed_lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !pair_is_ws_only(h, *i))
        .map(|(i, r)| DiffLine {
            kind: DiffLineKind::Removed,
            text: trim_cr(r),
            // cute-dbt#132: each surviving removed line of an aligned
            // replacement carries the intra-line span against its positional
            // partner `added[i]` (was 1:1-only, indexing `added[0]`). ws-only
            // pairs are already filtered out above, so `i` here is a
            // substantive pair and `added_lines[i]` is in bounds (equal counts).
            emphasis: if aligned {
                // `intra_line_span` only needs `&str`; trim with
                // `trim_end_matches` (no allocation) rather than the owned
                // `trim_cr` (gemini review on cute-dbt#132).
                intra_line_span(
                    r.trim_end_matches('\r'),
                    h.added_lines[i].trim_end_matches('\r'),
                )
            } else {
                None
            },
        })
        .collect()
}

/// Whether a hunk has the `--unified=0` shape the reconstruction is
/// contracted on: its new-side footprint (`new_len`, from the `@@` range)
/// equals its recorded `+` body count (`added_lines.len()`).
///
/// Under `--unified=0` this always holds — pure insertion `N == N`, pure
/// deletion `0 == 0`, replacement `N == N`. A default (context-bearing)
/// `git diff` violates it: the `cli::pr_diff` parser drops the ` `-prefixed
/// context lines, so `new_len > added_lines.len()`. cute-dbt is spec'd on
/// `--unified=0`; a context-bearing hunk is not a trustworthy line-precise
/// diff, so [`reconstruct_one`] degrades the whole block to the plain view
/// rather than panic on the body/footprint mismatch or mislabel
/// the uncovered new-side lines as Added ("cute-dbt never panics on a bad
/// diff"). The check is the per-hunk basis of that whole-block degrade.
fn hunk_is_unified_zero(h: &Hunk) -> bool {
    h.new_len == h.added_lines.len()
}

/// Fold one hunk's edits into `edits`. The removed bodies splice before the
/// hunk's anchor (clamped into the block); each covered new-side line is a
/// real addition unless it is the new side of a ws-only pair (cute-dbt#111,
/// then it stays Context); an aligned replacement records each added line's
/// per-pair intra-line emphasis (cute-dbt#132). The caller has already verified every touching hunk
/// is [`hunk_is_unified_zero`], so iterating `0..h.new_len` indexes
/// `added_lines`/`removed_lines` only within bounds.
fn fold_hunk_edits(edits: &mut HunkEdits, h: &Hunk, bs: usize, be: usize) {
    let anchor = if h.new_len == 0 {
        h.new_start + 1
    } else {
        h.new_start
    }
    .clamp(bs, be + 1);
    let removed = removed_diff_lines(h);
    if !removed.is_empty() {
        edits
            .splice_before
            .entry(anchor)
            .or_default()
            .extend(removed);
    }
    for k in 0..h.new_len {
        if !pair_is_ws_only(h, k) {
            edits.added_real.insert(h.new_start + k);
        }
    }
    // cute-dbt#132: record per-pair intra-line emphasis for every substantive
    // (non-ws-only) pair of an aligned replacement that falls within the block
    // (was 1:1-only, indexing `added_lines[0]` at `new_start`).
    if is_aligned_replacement(h) {
        for k in 0..h.added_lines.len() {
            let line_no = h.new_start + k;
            if !pair_is_ws_only(h, k) && (bs..=be).contains(&line_no) {
                // `&str` trim (no allocation) — see removed_diff_lines
                // (gemini review on cute-dbt#132).
                if let Some(e) = intra_line_span(
                    h.added_lines[k].trim_end_matches('\r'),
                    h.removed_lines[k].trim_end_matches('\r'),
                ) {
                    edits.added_emphasis.insert(line_no, e);
                }
            }
        }
    }
}

/// Classify one new-side block line into its rendered [`DiffLine`].
fn block_line_diff(edits: &HunkEdits, line_no: usize, text: String) -> DiffLine {
    if edits.added_real.contains(&line_no) {
        DiffLine {
            kind: DiffLineKind::Added,
            text,
            emphasis: edits.added_emphasis.get(&line_no).copied(),
        }
    } else {
        DiffLine {
            kind: DiffLineKind::Context,
            text,
            emphasis: None,
        }
    }
}

/// Reconstruct one block's inline diff from its source slice + the hunks
/// touching it (cute-dbt#96, generalized to any source block at
/// cute-dbt#111).
///
/// The block is addressed by a [`BlockSpan`] — the content-agnostic shape,
/// so the same reconstruction serves the `unit_test` YAML block (#96,
/// `BlockSpan::new(&block.raw, block.block_start, block.block_end)`) and a
/// model's SQL `raw_code` (#111, the whole-file span). Callers pass `raw`
/// already trailing-`\n`-normalized to git's line frame so `raw.split('\n')`
/// lines up with the hunks' new-side numbering.
///
/// **Whitespace-only line-pairs render as Context** ([`pair_is_ws_only`]):
/// a re-indentation / trailing-whitespace / blank-churn edit is not a
/// change (cute-dbt#111, `git --ignore-all-space` semantics) — no removed
/// splice, no emphasis. When every change in the block is whitespace-only
/// the result is all-Context; the caller tests [`BlockDiff::has_real_change`]
/// and emits no [`BlockDiff`] so the view falls back to plain text.
///
/// **Degrades to the plain view on a non-`--unified=0` hunk**
/// ([`hunk_is_unified_zero`]): cute-dbt is contracted on `--unified=0`, but
/// the parser accepts a default-context `git diff` (`new_len >
/// added_lines.len()`). Rather than panic on the body/footprint mismatch or
/// mislabel uncovered new-side lines as Added, an all-Context diff is
/// returned (→ `has_real_change()` is false → caller shows plain text),
/// consistent with the stale-diff degrade.
fn reconstruct_one(span: &BlockSpan, touching: &[&Hunk]) -> BlockDiff {
    let block_lines: Vec<String> = span.raw.split('\n').map(trim_cr).collect();
    let (bs, be) = (span.start, span.end);

    // Contract guard: every touching hunk must be `--unified=0`-shaped
    // (`new_len == added_lines.len()`). A context-bearing / malformed hunk
    // makes the line-precise reconstruction untrustworthy (and would index
    // `added_lines` out of bounds), so degrade the whole block to the plain
    // view — render every line as Context.
    let trustworthy = touching.iter().all(|h| hunk_is_unified_zero(h));

    // Removed-line groups to splice immediately before a given new-side
    // line; `added_real` marks genuine (non-whitespace) additions;
    // `added_emphasis` carries the clean-1:1 intra-line span (cute-dbt#111).
    let mut edits = HunkEdits::default();
    if trustworthy {
        for h in touching {
            fold_hunk_edits(&mut edits, h, bs, be);
        }
    }

    let mut lines: Vec<DiffLine> = Vec::new();
    for (i, text) in block_lines.into_iter().enumerate() {
        let l = bs + i; // 1-based new-side line number
        if let Some(group) = edits.splice_before.remove(&l) {
            lines.extend(group);
        }
        lines.push(block_line_diff(&edits, l, text));
    }
    // Trailing deletions clamped to just past the block's last line.
    if let Some(group) = edits.splice_before.remove(&(be + 1)) {
        lines.extend(group);
    }

    BlockDiff { lines }
}

/// Reconstruct one block's inline diff — the crate-internal wrapper the
/// cell-table diff ([`crate::domain::cell_diff::reconstruct_table_diffs`],
/// cute-dbt#98) calls to obtain a complete OLD-side block (Context + Removed
/// lines) without re-implementing the splice algorithm. Delegates verbatim
/// to the private `reconstruct_one`; the duplication this avoids is the
/// single largest silent-drift risk between the #96 line diff and the #98
/// table diff.
#[must_use]
pub(crate) fn block_diff_for(span: &BlockSpan, touching: &[&Hunk]) -> BlockDiff {
    reconstruct_one(span, touching)
}

// ---------------------------------------------------------------------
// Reverse application — reconstruct a file's OLD side (cute-dbt#266)
// ---------------------------------------------------------------------

/// Why [`reverse_apply`] refused to reconstruct the old side.
///
/// Every variant is a **fail-closed degrade signal**: the caller falls
/// back to the Shape-A raw-diff row ("could not reconstruct the previous
/// version") — never a silently wrong old text. `new_start` names the
/// offending hunk's 1-based new-side anchor for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReverseApplyError {
    /// A hunk's recorded `+` body does not match the supplied new text at
    /// its claimed footprint — the diff is stale relative to the working
    /// tree (the same drift notion as [`block_aligns_with_hunks`]).
    Drift {
        /// The drifting hunk's 1-based new-side start line.
        new_start: usize,
    },
    /// A hunk is not `--unified=0`-shaped (`new_len != added_lines.len()`,
    /// see [`NormalizedDiffIndex::context_bearing_hunk_count`]) — its `+`
    /// body under-describes its footprint, so reversal is untrustworthy.
    ContextBearing {
        /// The context-bearing hunk's 1-based new-side start line.
        new_start: usize,
    },
    /// A hunk's new-side footprint lies (partly) outside the supplied
    /// text — the diff describes a different revision.
    OutOfBounds {
        /// The out-of-bounds hunk's 1-based new-side start line.
        new_start: usize,
    },
    /// Two hunks overlap (or regress) on the new side — a malformed diff
    /// `git diff` never emits.
    Overlapping {
        /// The overlapping hunk's 1-based new-side start line.
        new_start: usize,
    },
}

/// Reverse-apply `--unified=0` hunks to a file's NEW text, reconstructing
/// the OLD (pre-change) text (cute-dbt#266).
///
/// The inverse of `git apply`: each hunk's new-side footprint
/// (`new_start..new_start + new_len`, 1-based) is replaced by its
/// `removed_lines`; a pure-deletion hunk (`new_len == 0`) re-inserts its
/// removed lines immediately **after** new-side line `new_start`
/// (`new_start == 0` ⇒ before line 1 — the `@@ -1,N +0,0 @@` shape).
/// A file-creation diff (one hunk covering the whole file, no removed
/// lines) reverses to the empty string — the caller treats empty old
/// text as "the file did not exist".
///
/// **Drift-guarded, fail-closed**: before anything is applied, every
/// hunk's `added_lines` must match the new text at its claimed footprint
/// (trailing `\r` trimmed on both sides — the parser strips it, a CRLF
/// working tree keeps it, same tolerance as [`block_aligns_with_hunks`]).
/// Any mismatch, context-bearing hunk, out-of-bounds footprint, or
/// overlap returns an [`Err`] and the caller degrades to the raw-diff
/// fallback — NEVER a silently wrong old text.
///
/// Trailing-newline framing: the old text inherits the new text's
/// trailing-newline presence (the `\ No newline at end of file` marker is
/// dropped at parse time, so the old framing is unrecoverable; for the
/// YAML consumer the framing is semantically inert). Properties pinned by
/// the unit suite: `reverse_apply(t, &[]) == t` (empty-hunks identity)
/// and forward∘reverse == identity over a structured edit-script pool.
///
/// # Errors
///
/// See [`ReverseApplyError`] — every arm is a degrade signal, never a
/// panic ("cute-dbt never panics on a bad diff").
pub fn reverse_apply(new_text: &str, hunks: &[Hunk]) -> Result<String, ReverseApplyError> {
    let had_trailing_newline = new_text.ends_with('\n');
    let mut lines: Vec<&str> = if new_text.is_empty() {
        Vec::new() // an empty file has zero lines, not one phantom ""
    } else {
        new_text.split('\n').collect()
    };
    if had_trailing_newline {
        lines.pop(); // drop the terminator's empty tail
    }

    let mut sorted: Vec<&Hunk> = hunks.iter().collect();
    sorted.sort_by_key(|h| (h.new_start, h.new_len));
    validate_reversible(&lines, &sorted)?;

    let mut old_lines: Vec<&str> = Vec::new();
    let mut cursor = 1usize; // next 1-based new-side line to copy
    for h in &sorted {
        // Copy untouched new-side lines up to the hunk's anchor: a pure
        // deletion re-inserts AFTER line `new_start` (inclusive copy); a
        // replacement/insertion starts AT `new_start` (exclusive copy).
        let copy_through = if h.new_len == 0 {
            h.new_start
        } else {
            h.new_start - 1
        };
        while cursor <= copy_through {
            old_lines.push(lines[cursor - 1]);
            cursor += 1;
        }
        old_lines.extend(h.removed_lines.iter().map(String::as_str));
        cursor += h.new_len; // skip the hunk's added lines
    }
    while cursor <= lines.len() {
        old_lines.push(lines[cursor - 1]);
        cursor += 1;
    }

    // A pure file-creation reversal leaves zero old lines: the join of an
    // empty vec is "" and the trailing terminator would fabricate a blank
    // line — return the canonical empty string instead.
    if old_lines.is_empty() {
        return Ok(String::new());
    }
    let mut old = old_lines.join("\n");
    if had_trailing_newline {
        old.push('\n');
    }
    Ok(old)
}

/// The [`reverse_apply`] pre-flight: every hunk `--unified=0`-shaped, in
/// bounds, non-overlapping (in `(new_start, new_len)` order), and its `+`
/// body matching the new text at its footprint (`\r`-trimmed both sides).
fn validate_reversible(lines: &[&str], sorted: &[&Hunk]) -> Result<(), ReverseApplyError> {
    // The highest new-side line already claimed by an earlier hunk (0 ⇒
    // none). A replacement must start strictly after it; a pure deletion
    // anchors after `new_start`, so it may equal it.
    let mut claimed_through = 0usize;
    for h in sorted {
        let new_start = h.new_start;
        if h.new_len != h.added_lines.len() {
            return Err(ReverseApplyError::ContextBearing { new_start });
        }
        if h.new_len == 0 {
            if new_start > lines.len() {
                return Err(ReverseApplyError::OutOfBounds { new_start });
            }
            if new_start < claimed_through {
                return Err(ReverseApplyError::Overlapping { new_start });
            }
            claimed_through = claimed_through.max(new_start);
            continue;
        }
        if new_start == 0 || new_start + h.new_len - 1 > lines.len() {
            return Err(ReverseApplyError::OutOfBounds { new_start });
        }
        if new_start <= claimed_through {
            return Err(ReverseApplyError::Overlapping { new_start });
        }
        for (k, added) in h.added_lines.iter().enumerate() {
            let actual = lines[new_start - 1 + k];
            if actual.trim_end_matches('\r') != added.trim_end_matches('\r') {
                return Err(ReverseApplyError::Drift { new_start });
            }
        }
        claimed_through = new_start + h.new_len - 1;
    }
    Ok(())
}

/// Render a file's hunks directly as raw diff lines — removed (`-`) then
/// added (`+`) bodies per hunk, in diff order, `\r`-trimmed, no context,
/// no emphasis (cute-dbt#266).
///
/// The Shape-A fallback row's content: when the semantic `dbt_project.yml`
/// categorization degrades (parse failure / un-reconstructable old side /
/// unreadable working-tree file) the panel still shows the *diff itself*,
/// truthfully, without needing the working-tree text or any alignment
/// guarantee — the bodies come straight from the parsed hunks.
#[must_use]
pub fn raw_hunk_lines(hunks: &[Hunk]) -> Vec<DiffLine> {
    let mut out = Vec::new();
    for h in hunks {
        out.extend(h.removed_lines.iter().map(|l| DiffLine {
            kind: DiffLineKind::Removed,
            text: trim_cr(l),
            emphasis: None,
        }));
        out.extend(h.added_lines.iter().map(|l| DiffLine {
            kind: DiffLineKind::Added,
            text: trim_cr(l),
            emphasis: None,
        }));
    }
    out
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
            renames: Vec::new(),
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

    // ----- rename serde compatibility (cute-dbt#80) -----

    #[test]
    fn rename_free_pr_diff_serializes_without_a_renames_field() {
        // `skip_serializing_if` keeps the rename-free wire shape
        // byte-identical to the pre-#80 POD — no `"renames":[]` noise.
        let diff = PrDiff::default();
        let json = serde_json::to_string(&diff).expect("serialize");
        assert!(
            !json.contains("renames"),
            "an empty renames vec must not serialize: {json}",
        );
    }

    #[test]
    fn pre_rename_payload_without_a_renames_field_deserializes() {
        // `#[serde(default)]` — a payload from before the field existed.
        let back: PrDiff = serde_json::from_str(r#"{"files":[]}"#).expect("deserialize");
        assert!(back.renames.is_empty());
    }

    #[test]
    fn rename_carrying_pr_diff_round_trips_through_json() {
        let diff = PrDiff {
            renames: vec![RenamePair {
                from: "models/dim_a.sql".to_owned(),
                to: "models/dim_b.sql".to_owned(),
            }],
            files: Vec::new(),
        };
        let json = serde_json::to_string(&diff).expect("serialize");
        let back: PrDiff = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(diff, back);
    }

    // ----- NormalizedDiffIndex: keyset + lookup -----

    #[test]
    fn index_changed_paths_lists_normalized_file_set() {
        let diff = PrDiff {
            renames: Vec::new(),
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
            renames: Vec::new(),
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
            renames: Vec::new(),
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
            renames: Vec::new(),
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
            renames: Vec::new(),
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

    // ----- NormalizedDiffIndex: rename pairs (cute-dbt#80) -----

    #[test]
    fn index_includes_both_sides_of_a_pure_rename_in_the_changed_keyset() {
        // A pure rename has NO file entry — only the pair. Both the old
        // and the new path must land in the changed-file keyset, so the
        // current manifest's node at the NEW path scopes (the old path
        // maps to no current node and is inert).
        let diff = PrDiff {
            renames: vec![RenamePair {
                from: "models/marts/dim_a.sql".to_owned(),
                to: "models/marts/dim_b.sql".to_owned(),
            }],
            files: Vec::new(),
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        assert!(index.contains_changed("models/marts/dim_a.sql"));
        assert!(index.contains_changed("models/marts/dim_b.sql"));
        assert!(
            index.hunks_for("models/marts/dim_b.sql").is_empty(),
            "a pure rename carries no hunks",
        );
    }

    #[test]
    fn index_applies_the_project_root_strip_to_both_rename_sides() {
        // Rename paths are repo-relative like every other diff-side path,
        // so the strip applies to both sides — the same key the manifest's
        // project-relative original_file_path resolves to.
        let diff = PrDiff {
            renames: vec![RenamePair {
                from: "dbt_project/models/dim_a.sql".to_owned(),
                to: "dbt_project/models/dim_b.sql".to_owned(),
            }],
            files: Vec::new(),
        };
        let index = NormalizedDiffIndex::new(&diff, Some(Path::new("dbt_project")));
        assert!(index.contains_changed("models/dim_a.sql"));
        assert!(index.contains_changed("models/dim_b.sql"));
    }

    #[test]
    fn index_keeps_hunks_when_rename_to_coincides_with_a_file_entry() {
        // Rename WITH edits: the new path has a real file entry (with
        // hunks) AND appears as a rename `to`. The rename insertion must
        // not clobber the hunks.
        let diff = PrDiff {
            renames: vec![RenamePair {
                from: "models/dim_a.sql".to_owned(),
                to: "models/dim_b.sql".to_owned(),
            }],
            files: vec![FileHunks {
                path: "models/dim_b.sql".to_owned(),
                hunks: vec![hunk(3, 1)],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        assert_eq!(
            index.hunks_for("models/dim_b.sql").len(),
            1,
            "the new path's hunks survive the rename-side insertion",
        );
        assert!(index.contains_changed("models/dim_a.sql"));
    }

    /// The property under test (cute-dbt#80): the index keyset is
    /// **exactly** the normalized union of the file paths and BOTH sides
    /// of every rename pair — no supplied path can drop out, and nothing
    /// extra can appear. Coverage is exhaustive over a structured input
    /// space (every file-count × rename-count × strip combination from a
    /// fixed stem pool) rather than randomized — the house property-test
    /// style (no proptest dev-dependency; see
    /// `adapters::manifest`'s tolerant-deserialization property).
    #[test]
    fn index_keyset_is_exactly_the_union_of_files_and_both_rename_sides() {
        let file_pool = ["alpha", "bravo", "carol"];
        let rename_pool = [("delta", "delta_v2"), ("echo", "foxtrot"), ("golf", "golf")];

        for n_files in 0..=file_pool.len() {
            for n_renames in 0..=rename_pool.len() {
                for strip in [None, Some("dbt_project")] {
                    let prefix = strip.map_or(String::new(), |s| format!("{s}/"));
                    let files: Vec<FileHunks> = file_pool[..n_files]
                        .iter()
                        .map(|s| FileHunks {
                            path: format!("{prefix}models/{s}.sql"),
                            hunks: vec![hunk(1, 1)],
                        })
                        .collect();
                    let renames: Vec<RenamePair> = rename_pool[..n_renames]
                        .iter()
                        .map(|(f, t)| RenamePair {
                            from: format!("{prefix}models/{f}.sql"),
                            to: format!("{prefix}models/{t}.sql"),
                        })
                        .collect();
                    let diff = PrDiff { files, renames };
                    let index = NormalizedDiffIndex::new(&diff, strip.map(Path::new));

                    let expected: std::collections::HashSet<String> = file_pool[..n_files]
                        .iter()
                        .map(|s| format!("models/{s}.sql"))
                        .chain(rename_pool[..n_renames].iter().flat_map(|(f, t)| {
                            [format!("models/{f}.sql"), format!("models/{t}.sql")]
                        }))
                        .collect();
                    let actual: std::collections::HashSet<String> =
                        index.changed_paths().map(str::to_owned).collect();
                    assert_eq!(
                        actual, expected,
                        "keyset must equal the union (files={n_files}, \
                         renames={n_renames}, strip={strip:?})",
                    );
                }
            }
        }
    }

    #[test]
    fn context_bearing_hunk_count_counts_only_non_unified_zero_hunks() {
        // A `--unified=0` diff: replacement (new_len == added.len()) and a
        // pure deletion (new_len == 0 == added.len()) both satisfy the
        // predicate → count 0.
        let unified_zero = PrDiff {
            renames: Vec::new(),
            files: vec![
                FileHunks {
                    path: "models/a.sql".to_owned(),
                    hunks: vec![repl(3, &["new line"]), del(7)],
                },
                FileHunks {
                    path: "models/b.yml".to_owned(),
                    hunks: vec![repl(1, &["x", "y"])], // new_len 2 == added 2
                },
            ],
        };
        assert_eq!(
            NormalizedDiffIndex::new(&unified_zero, None).context_bearing_hunk_count(),
            0,
            "a pure --unified=0 diff (incl. a pure-deletion hunk) has zero context-bearing hunks",
        );

        // A context-bearing (default `git diff`) shape: `new_len` claims more
        // new-side lines than the recorded `+` bodies (context lines dropped).
        // Two such hunks across two files + one clean replacement → count 2.
        let context_bearing = |new_start, new_len, added: &[&str]| Hunk {
            new_start,
            new_len, // > added.len()
            removed_lines: vec!["was".to_owned()],
            added_lines: added.iter().map(|s| (*s).to_owned()).collect(),
        };
        let mixed = PrDiff {
            renames: Vec::new(),
            files: vec![
                FileHunks {
                    path: "models/a.sql".to_owned(),
                    hunks: vec![
                        context_bearing(1, 5, &["one +"]), // 5 != 1 → context-bearing
                        repl(20, &["clean replacement"]),  // 1 == 1 → unified-zero
                    ],
                },
                FileHunks {
                    path: "models/b.sql".to_owned(),
                    hunks: vec![context_bearing(1, 3, &["a", "b"])], // 3 != 2 → context-bearing
                },
            ],
        };
        assert_eq!(
            NormalizedDiffIndex::new(&mixed, None).context_bearing_hunk_count(),
            2,
            "counts exactly the hunks whose new_len != added_lines.len()",
        );
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
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
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
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &[repl(3, &["    model: m", "    given: []"])]
        ));
    }

    #[test]
    fn block_misaligns_when_added_line_offset_is_wrong() {
        let block = block_at(ALIGN_RAW, 2); // [2, 4]
        // Claims "    model: m" at line 2, but line 2 is "  - name: t".
        assert!(!block_aligns_with_hunks(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &[repl(2, &["    model: m"])]
        ));
    }

    #[test]
    fn block_misaligns_when_added_line_content_is_corrupted() {
        let block = block_at(ALIGN_RAW, 2); // [2, 4]
        assert!(!block_aligns_with_hunks(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &[repl(3, &["    model: CORRUPTED"])]
        ));
    }

    #[test]
    fn block_aligns_ignores_added_lines_outside_the_block() {
        let block = block_at("  - name: t\n    model: m", 8); // [8, 9]
        // A hunk in the file header — entirely above the block → ignored.
        assert!(block_aligns_with_hunks(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &[repl(1, &["version: 3"])]
        ));
    }

    #[test]
    fn block_aligns_vacuously_for_pure_deletion_hunks() {
        let block = block_at("  - name: t\n    model: m", 2);
        // A deletion carries no `+` lines → nothing to verify → aligned.
        assert!(block_aligns_with_hunks(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &[del(2)]
        ));
    }

    #[test]
    fn block_aligns_normalizes_crlf_line_endings() {
        // The slicer keeps `\r` (split('\n')); the parser strips it
        // (str::lines). A CRLF working tree must not read as stale.
        let block = block_at("  - name: t\r\n    model: m\r", 2); // ["  - name: t\r", "    model: m\r"]
        assert!(block_aligns_with_hunks(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &[repl(3, &["    model: m"])]
        ));
    }

    #[test]
    fn block_misaligns_when_hunk_claims_a_line_the_block_lacks() {
        // The span claims more lines (`block_end` = 5) than `raw` actually
        // has (2 lines, offsets 0..=1). A hunk whose added line lands at a
        // new-side position inside [bs, be] but past `raw`'s last line —
        // `block_lines.get(offset)` is None → stale (return false). Pins the
        // `(raw, start, end)` widening's out-of-range guard; with `split('\n')`
        // model `raw_code` spans this is the engine-mismatch safety net.
        let raw = "select 1\nfrom t"; // 2 lines, offsets 0..=1
        // block_end overstated to 5; the hunk claims line 4 (offset 3) which
        // raw doesn't have.
        assert!(
            !block_aligns_with_hunks(&BlockSpan::new(raw, 1, 5), &[repl(4, &["phantom"])]),
            "a hunk claiming a line beyond `raw` is stale, not aligned",
        );
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
            UnitTestExpect::new(serde_json::Value::Null, None, None),
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
            renames: Vec::new(),
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
            renames: Vec::new(),
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

    #[test]
    fn intra_line_span_suffix_stops_at_the_prefix_boundary() {
        // Shared prefix AND shared suffix with asymmetric middle lengths —
        // stresses BOTH suffix bounds (`suffix < a.len()-prefix` and
        // `suffix < b.len()-prefix`). The bounds keep the suffix scan from
        // overlapping the already-matched prefix on the shorter side.
        // Kills the suffix-bound mutants (`< → <=`, `a.len()-prefix →
        // a.len()+prefix`, `b.len()-prefix → b.len()+prefix`): each would
        // over-extend the suffix and shift the reported span.
        // prefix "a" (1) + suffix "c" (1); middles "X" vs "YYY".
        assert_eq!(intra_line_span("aXc", "aYYYc"), Some((1, 2))); // "X"
        assert_eq!(intra_line_span("aYYYc", "aXc"), Some((1, 4))); // "YYY"
        // The prefix-exhausts-the-short-side case: "ab" is entirely shared
        // prefix of "abab"; the suffix bound on the short side is 0, so the
        // suffix loop must NOT advance into the prefix (a `<=` or `+` mutant
        // would). Short side → None; long side → the trailing "ab".
        assert_eq!(intra_line_span("ab", "abab"), None);
        assert_eq!(intra_line_span("abab", "ab"), Some((2, 4))); // trailing "ab"
        // Suffix shares the WHOLE short side: "xy" is the suffix of "Axy".
        assert_eq!(intra_line_span("xy", "Axy"), None);
        assert_eq!(intra_line_span("Axy", "xy"), Some((0, 1))); // leading "A"
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
        let got = reconstruct_one(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &hunks.iter().collect::<Vec<_>>(),
        );
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
        let got = reconstruct_one(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &hunks.iter().collect::<Vec<_>>(),
        );
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
        let got = reconstruct_one(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &hunks.iter().collect::<Vec<_>>(),
        );
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
        let got = reconstruct_one(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &hunks.iter().collect::<Vec<_>>(),
        );
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
        let got = reconstruct_one(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &hunks.iter().collect::<Vec<_>>(),
        );
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
        let got = reconstruct_one(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &hunks.iter().collect::<Vec<_>>(),
        );
        assert_eq!(
            got.lines,
            vec![
                // cute-dbt#132: aligned 2↔2 → per-pair emphasis even when one
                // added partner (new9, line 9) is clamped above the block. Each
                // removed line and the in-block added line fully change vs their
                // positional partner, so each carries a whole-line span.
                rem_e("old8", (0, 4)),
                rem_e("old9", (0, 4)),
                add_e("    model: m2", (0, 13)), // line 10, inside the hunk's added range
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
        let got = reconstruct_one(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &hunks.iter().collect::<Vec<_>>(),
        );
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
    fn reconstruct_aligned_multi_line_replacement_carries_per_pair_emphasis() {
        // cute-dbt#132: an ALIGNED equal-count replacement (removed.len() ==
        // added.len()) now emphasizes each removed[i]↔added[i] pair, not just
        // the 1↔1 case. `old`→`new` share no characters, only the trailing
        // `A`/`B`, so each side's changed span is (0, 3). (Previously this
        // shape rendered line-level +/- with no <strong>.)
        let block = block_at("  - name: t\nnewA\nnewB\n    given: []", 10); // [10,13]
        let hunks = [replace(11, &["oldA", "oldB"], &["newA", "newB"])];
        let got = reconstruct_one(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &hunks.iter().collect::<Vec<_>>(),
        );
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: t"),
                rem_e("oldA", (0, 3)),
                rem_e("oldB", (0, 3)),
                add_e("newA", (0, 3)),
                add_e("newB", (0, 3)),
                ctx("    given: []"),
            ],
        );
    }

    #[test]
    fn reconstruct_aligned_multi_line_emphasis_skips_a_whitespace_only_pair() {
        // cute-dbt#132 boundary: in an aligned 2↔2 hunk, a whitespace-only pair
        // (a pure re-indent) must still render as Context with NO emphasis —
        // per-pair emphasis fires ONLY on substantive pairs. Pair 0
        // (`was`→`now`) is real; pair 1 (`rows: []` re-indented 6→4 spaces) is
        // whitespace-only. This pins that the per-pair loop honors
        // `pair_is_ws_only`, not just blanket-emphasizes every aligned pair.
        let block = block_at(
            "  - name: t\n    model: now\n    rows: []\n    given: x",
            10,
        ); // [10,13]
        let hunks = [replace(
            11,
            &["    model: was", "      rows: []"],
            &["    model: now", "    rows: []"],
        )];
        let got = reconstruct_one(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &hunks.iter().collect::<Vec<_>>(),
        );
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: t"),
                rem_e("    model: was", (11, 14)),
                add_e("    model: now", (11, 14)),
                ctx("    rows: []"), // ws-only pair → Context, no removed splice, no emphasis
                ctx("    given: x"),
            ],
        );
    }

    #[test]
    fn reconstruct_unequal_count_replacement_has_no_emphasis() {
        // cute-dbt#132: per-pair emphasis fires only on an ALIGNED replacement
        // (`is_aligned_replacement`: removed.len() == added.len()). A
        // `@@ -N,2 +N,1 @@` shape — TWO old lines collapse into ONE new line —
        // is a valid `--unified=0` hunk (new_len(1) == added.len(1)) but the
        // counts are UNEQUAL (removed.len()==2 != added.len()==1), so there is
        // no sound positional pairing and NO emphasis is emitted. A mutant
        // relaxing the equal-count guard would emit a spurious <strong> on
        // removed[0]/added[0] — this pins the count check (cargo mutants).
        let block = block_at("  - name: t\n    model: m\n    given: []", 10); // [10,12]
        let hunks = [replace(
            11,
            &["    model: was", "    extra: gone"],
            &["    model: m"],
        )];
        let got = reconstruct_one(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &hunks.iter().collect::<Vec<_>>(),
        );
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: t"),
                rem("    model: was"), // NO emphasis (not a clean 1:1)
                rem("    extra: gone"),
                add("    model: m"), // NO emphasis
                ctx("    given: []"),
            ],
            "a 2-removed/1-added replacement carries no intra-line emphasis",
        );
    }

    #[test]
    fn reconstruct_degrades_on_a_malformed_hunk_with_an_empty_added_body() {
        // A diff that declares `new_len: 1` yet carries no `+` body is
        // `new_len(1) != added_lines.len(0)` — the same non-`--unified=0`
        // mismatch as a context-bearing hunk, so it degrades the whole block
        // to the plain view (all Context). This both avoids the
        // `added_lines[0]` OOB panic the #110 review flagged AND is honest:
        // a hunk that claims a new-side line with no recorded `+` body is not
        // a trustworthy line-precise diff, so cute-dbt shows the plain text
        // rather than fabricate an Added line from the block content.
        let block = block_at("  - name: t\n    model: m\n    given: []", 10); // [10,12]
        let hunk = Hunk {
            new_start: 11,
            new_len: 1,
            removed_lines: vec!["    model: was".to_owned()],
            added_lines: Vec::new(),
        };
        let got = reconstruct_one(
            &BlockSpan::new(&block.raw, block.block_start, block.block_end),
            &[&hunk],
        );
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: t"),
                ctx("    model: m"),
                ctx("    given: []"),
            ],
        );
        assert!(
            !got.has_real_change(),
            "malformed hunk degrades to plain view"
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
            renames: Vec::new(),
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

    // ----- cute-dbt#247: attach_model_yaml_diffs -----
    //
    // The reconstruct_block_diffs sibling for the Model-YAML drawer:
    // operates in place on the gather map, attaching an inline diff to
    // each Found outcome whose sliced `models:` block the PR diff
    // genuinely edited. Same gate set as #96 (present + aligned +
    // touched + substantive) — pinned here against every outcome arm.

    #[test]
    fn attach_model_yaml_diffs_attaches_only_for_edited_aligned_blocks() {
        let diff = PrDiff {
            renames: Vec::new(),
            files: vec![
                // _a.yml: hunk replaces line 2 with the working-tree body
                // line → aligned + touches the edited block [1,3].
                FileHunks {
                    path: "models/_a.yml".to_owned(),
                    hunks: vec![replace(
                        2,
                        &["    description: was"],
                        &["    description: now"],
                    )],
                },
                // _c.yml: hunk's added body does NOT match the block line
                // at that position → misaligned (stale diff).
                FileHunks {
                    path: "models/_c.yml".to_owned(),
                    hunks: vec![replace(
                        2,
                        &["    description: was"],
                        &["    description: DRIFTED"],
                    )],
                },
                // _d.yml: the only hunk is outside the block [10,12].
                FileHunks {
                    path: "models/_d.yml".to_owned(),
                    hunks: vec![replace(
                        2,
                        &["    description: was"],
                        &["    description: now"],
                    )],
                },
            ],
        };
        let index = NormalizedDiffIndex::new(&diff, None);

        let found = |path: &str, raw: &str, start: usize| ModelYamlOutcome::Found {
            path: path.to_owned(),
            block: block_at(raw, start),
            diff: None,
        };
        let mut outcomes: HashMap<String, ModelYamlOutcome> = HashMap::new();
        outcomes.insert(
            "model.shop.edited".to_owned(),
            found(
                "models/_a.yml",
                "  - name: edited\n    description: now\n    columns: []",
                1, // [1,3]
            ),
        );
        outcomes.insert(
            "model.shop.stale".to_owned(),
            found(
                "models/_c.yml",
                "  - name: stale\n    description: now\n    columns: []",
                1, // [1,3]
            ),
        );
        outcomes.insert(
            "model.shop.untouched".to_owned(),
            found(
                "models/_d.yml",
                "  - name: untouched\n    description: x\n    columns: []",
                10, // [10,12]
            ),
        );
        attach_model_yaml_diffs(&mut outcomes, &index);

        let diff_of = |id: &str| match &outcomes[id] {
            ModelYamlOutcome::Found { diff, .. } => diff.clone(),
            other => panic!("outcome variant changed for {id}: {other:?}"),
        };
        let edited = diff_of("model.shop.edited").expect("edited block → diff attached");
        assert_eq!(
            edited.lines,
            vec![
                ctx("  - name: edited"),
                rem_e("    description: was", (17, 20)), // "was"
                add_e("    description: now", (17, 20)), // "now"
                ctx("    columns: []"),
            ],
        );
        assert!(
            diff_of("model.shop.stale").is_none(),
            "misaligned (stale) diff → no inline diff",
        );
        assert!(
            diff_of("model.shop.untouched").is_none(),
            "change outside the block → no inline diff",
        );
    }

    #[test]
    fn attach_model_yaml_diffs_passes_degrade_variants_through() {
        // Non-Found outcomes carry no block — they must pass through the
        // attach untouched even when their (unreadable) file has hunks.
        let diff = PrDiff {
            renames: Vec::new(),
            files: vec![FileHunks {
                path: "models/_zz.yml".to_owned(),
                hunks: vec![replace(1, &["old"], &["new"])],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        let mut outcomes: HashMap<String, ModelYamlOutcome> = HashMap::new();
        outcomes.insert(
            "model.shop.no_patch".to_owned(),
            ModelYamlOutcome::NoPatchPath,
        );
        outcomes.insert(
            "model.shop.missing".to_owned(),
            ModelYamlOutcome::FileMissing {
                path: "models/_zz.yml".to_owned(),
            },
        );

        attach_model_yaml_diffs(&mut outcomes, &index);

        assert_eq!(
            outcomes["model.shop.no_patch"],
            ModelYamlOutcome::NoPatchPath
        );
        assert_eq!(
            outcomes["model.shop.missing"],
            ModelYamlOutcome::FileMissing {
                path: "models/_zz.yml".to_owned(),
            },
        );
    }

    #[test]
    fn attach_model_yaml_diffs_skips_a_whitespace_only_edit() {
        // A re-indent inside the block reconstructs to all-Context
        // (cute-dbt#111 `ws_equal` pairing) → no diff attached, the
        // section keeps the plain File view.
        let diff = PrDiff {
            renames: Vec::new(),
            files: vec![FileHunks {
                path: "models/_a.yml".to_owned(),
                hunks: vec![replace(
                    2,
                    &["      description: d"],
                    &["    description: d"],
                )],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        let mut outcomes: HashMap<String, ModelYamlOutcome> = HashMap::new();
        outcomes.insert(
            "model.shop.ws".to_owned(),
            ModelYamlOutcome::Found {
                path: "models/_a.yml".to_owned(),
                block: block_at("  - name: ws\n    description: d", 1),
                diff: None,
            },
        );

        attach_model_yaml_diffs(&mut outcomes, &index);

        match &outcomes["model.shop.ws"] {
            ModelYamlOutcome::Found { diff, .. } => {
                assert!(diff.is_none(), "whitespace-only edit → no inline diff");
            }
            other => panic!("outcome variant changed: {other:?}"),
        }
    }

    // ----- BlockDiff serde: the exact JS wire shape -----

    #[test]
    fn block_diff_serializes_to_the_exact_renderblockdiff_contract() {
        let diff = BlockDiff {
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
        let back: BlockDiff = serde_json::from_str(&json).unwrap();
        assert_eq!(back, diff);
    }

    // =================================================================
    // Whitespace-insensitivity (cute-dbt#111): ws_equal +
    // hunk_is_whitespace_only + BlockDiff::has_real_change
    // =================================================================

    #[test]
    fn ws_equal_ignores_leading_trailing_and_interior_whitespace() {
        // Re-indentation, trailing whitespace, collapsed interior runs:
        // identical non-whitespace content ⇒ equal.
        assert!(ws_equal("    select id", "        select id"));
        assert!(ws_equal("select id ", "select id"));
        assert!(ws_equal("select  id", "select id"));
        assert!(ws_equal("", "   "));
        assert!(ws_equal("\tselect\tid\t", "select id"));
    }

    #[test]
    fn ws_equal_distinguishes_substantive_content() {
        assert!(!ws_equal("select id", "select uid"));
        assert!(!ws_equal("select id", "select id, name"));
        // Re-ordered tokens are a real change (token SEQUENCE, not set).
        assert!(!ws_equal("a b", "b a"));
    }

    #[test]
    fn pair_is_ws_only_true_per_aligned_pair() {
        // 1:1 re-indentation: each `+` is ws_equal to its paired `-`.
        let h = replace(
            5,
            &["select id", "  from t"],
            &["    select id", "        from t"],
        );
        assert!(pair_is_ws_only(&h, 0));
        assert!(pair_is_ws_only(&h, 1));
    }

    #[test]
    fn pair_is_ws_only_false_for_a_substantive_pair() {
        let h = replace(
            5,
            &["select id", "from t"],
            &["    select id", "from u"], // pair 1: t → u (real)
        );
        assert!(pair_is_ws_only(&h, 0)); // re-indent
        assert!(!pair_is_ws_only(&h, 1)); // value change
    }

    #[test]
    fn pair_is_ws_only_false_for_unequal_side_counts() {
        // A genuine insertion / deletion (unequal counts) has no pairing —
        // adding/removing a line is a real change even when blank. The
        // count guard short-circuits BEFORE indexing, so no panic.
        assert!(!pair_is_ws_only(&replace(5, &[], &["   "]), 0));
        assert!(!pair_is_ws_only(&delete(5, &["   "]), 0));
        assert!(!pair_is_ws_only(&replace(5, &["a"], &["   a   ", "b"]), 0));
    }

    #[test]
    fn reconstruct_one_mixed_adjacent_hunk_keeps_real_drops_ws_pair() {
        // The single-hunk adjacent case `git --unified=0` produces (advisor
        // 2026-05-31): line 1 is a pure re-indent (ws-only pair → Context),
        // line 2 a real value change (kept) — ALL IN ONE HUNK. A whole-hunk
        // filter would wrongly keep or drop both; per-pair handles it.
        let raw = "  select a\nfrom u"; // [1,2]
        let hunks = [replace(
            1,
            &["select a", "from t"],
            &["  select a", "from u"],
        )];
        let got = reconstruct_one(
            &BlockSpan::new(raw, 1, 2),
            &hunks.iter().collect::<Vec<_>>(),
        );
        // The whitespace goal is met: line 1 ("  select a") renders as
        // CONTEXT (not Added) and only the substantive "from t"→"from u"
        // pair diffs. The removed line splices at the hunk anchor (its
        // `new_start`, here clamped to the block top) — the pre-existing
        // #96 multi-line-replacement placement, not paired to its own
        // offset. Cosmetic ordering only; the change-set is correct.
        assert_eq!(
            got.lines,
            vec![
                // cute-dbt#132: the substantive pair now carries per-pair
                // emphasis ("t"→"u" at offset 5); the ws-only pair stays
                // Context with none.
                rem_e("from t", (5, 6)), // removed splices at the hunk anchor (line 1)
                ctx("  select a"),       // re-indented line: Context, NOT a change
                add_e("from u", (5, 6)), // the real new-side change
            ],
            "the re-indent stays Context; only the substantive pair diffs (now emphasised)",
        );
        // Critically: the re-indented line is NOT marked Added/Removed.
        assert!(
            got.lines
                .iter()
                .all(|l| !(l.text == "  select a" && l.kind != DiffLineKind::Context)),
            "the whitespace-only line must never render as a change",
        );
        assert!(got.has_real_change());
    }

    #[test]
    fn block_diff_has_real_change_only_with_added_or_removed() {
        let all_ctx = BlockDiff {
            lines: vec![ctx("a"), ctx("b")],
        };
        assert!(!all_ctx.has_real_change());
        assert!(
            BlockDiff {
                lines: vec![ctx("a"), add("b")]
            }
            .has_real_change()
        );
        assert!(
            BlockDiff {
                lines: vec![rem("a"), ctx("b")]
            }
            .has_real_change()
        );
    }

    #[test]
    fn reconstruct_one_renders_whitespace_only_change_as_context() {
        // A re-indentation of line 2 (working tree == `    model: m`, old
        // side `model: m`): ws_equal ⇒ no Removed splice, no emphasis, the
        // new-side line is Context. Frame: 3 context lines, no real change.
        let raw = "  - name: t\n    model: m\n    given: []"; // [10,12]
        let hunks = [replace(11, &["model: m"], &["    model: m"])];
        let got = reconstruct_one(
            &BlockSpan::new(raw, 10, 12),
            &hunks.iter().collect::<Vec<_>>(),
        );
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: t"),
                ctx("    model: m"),
                ctx("    given: []")
            ],
        );
        assert!(
            !got.has_real_change(),
            "ws-only change carries no real diff"
        );
    }

    #[test]
    fn reconstruct_one_keeps_real_change_adjacent_to_a_whitespace_only_change() {
        // git --unified=0 splits these into two hunks: line 11 is a pure
        // re-indent (dropped), line 12 is a real value change (kept).
        let raw = "  - name: t\n    model: m\n    given: [1]"; // [10,12]
        let hunks = [
            replace(11, &["model: m"], &["    model: m"]), // ws-only → dropped
            replace(12, &["    given: []"], &["    given: [1]"]), // real → kept
        ];
        let got = reconstruct_one(
            &BlockSpan::new(raw, 10, 12),
            &hunks.iter().collect::<Vec<_>>(),
        );
        // Common prefix "    given: [" (12); removed "]" has no own changed
        // span (it is the shorter side's shared suffix → None); added "1]"
        // contributes "1" at codepoint 12.
        assert_eq!(
            got.lines,
            vec![
                ctx("  - name: t"),
                ctx("    model: m"), // the re-indented line, as plain Context
                rem("    given: []"),
                add_e("    given: [1]", (12, 13)), // "1"
            ],
        );
        assert!(got.has_real_change());
    }

    // =================================================================
    // Model SQL reconstruction (cute-dbt#111):
    // reconstruct_model_sql_diffs + the trailing-newline frame
    // =================================================================

    use crate::domain::manifest::{Checksum, NodeConfig};

    /// A `model` node carrying `raw_code` + `original_file_path` — the two
    /// fields `reconstruct_model_sql_diffs` reads. `compiled` is irrelevant
    /// to SQL-diff reconstruction (the diff is over RAW Jinja).
    fn model_with_raw(full_id: &str, raw_code: &str, ofp: &str) -> Node {
        Node::new(
            NodeId::new(full_id),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            Some(raw_code.to_owned()),
            DependsOn::default(),
            Some(ofp.to_owned()),
            NodeConfig::default(),
            None,
            std::collections::BTreeMap::new(),
        )
    }

    fn manifest_with_models(models: Vec<Node>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            models.into_iter().map(|m| (m.id().clone(), m)).collect(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    /// The whole-file span of a `raw_code`, as `reconstruct_model_sql_diffs`
    /// computes it — trailing-`\n` stripped (git's line frame), then
    /// `(raw, 1, line_count)`. Pins the off-by-one trap.
    #[test]
    fn model_sql_frame_strips_exactly_one_trailing_newline() {
        // dbt-CORE shape: raw_code already trailing-newline-stripped.
        let core = "select 1\nfrom t"; // 2 content lines, no trailing \n
        // dbt-FUSION shape: byte-identical PLUS the trailing \n (verified
        // 2026-05-31 against a fusion-compiled playground manifest).
        let fusion = "select 1\nfrom t\n";

        let core_norm = core.strip_suffix('\n').unwrap_or(core);
        let fusion_norm = fusion.strip_suffix('\n').unwrap_or(fusion);
        assert_eq!(
            core_norm, fusion_norm,
            "both engines normalize to the same frame"
        );
        assert_eq!(
            core_norm.split('\n').count(),
            2,
            "git line frame = 2 content lines"
        );

        // A real blank line at EOF (`"a\n\n"`, git frame = 2) must be KEPT —
        // strip_suffix removes only the single terminator, not the blank.
        let blank_eof = "a\n\n";
        assert_eq!(
            blank_eof.strip_suffix('\n').unwrap_or(blank_eof),
            "a\n",
            "strip exactly one \\n; the real blank line survives",
        );
    }

    #[test]
    fn reconstruct_model_sql_diffs_identical_frame_across_engine_trailing_newline() {
        // The cross-engine guard: a core-shaped and a fusion-shaped
        // raw_code of the SAME model produce an IDENTICAL DiffLine frame
        // (AGENTS.md: reports look identical regardless of compiling
        // engine). Edit line 2 ("from t" → "from u"); whole-file hunk.
        let ofp = "models/m.sql";
        let core_raw = "select id\nfrom u"; // working tree (no trailing \n)
        let fusion_raw = "select id\nfrom u\n"; // same + trailing \n

        // The diff's `+` lines must match the (stripped) working tree at
        // their new-side positions for N7b to hold.
        let diff = PrDiff {
            renames: Vec::new(),
            files: vec![FileHunks {
                path: ofp.to_owned(),
                hunks: vec![replace(2, &["from t"], &["from u"])],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);

        let core_mf = manifest_with_models(vec![model_with_raw("model.s.m", core_raw, ofp)]);
        let fusion_mf = manifest_with_models(vec![model_with_raw("model.s.m", fusion_raw, ofp)]);
        let scope = ModelInScopeSet::from_iter([NodeId::new("model.s.m")]);

        let core_diffs = reconstruct_model_sql_diffs(&core_mf, &scope, &index);
        let fusion_diffs = reconstruct_model_sql_diffs(&fusion_mf, &scope, &index);

        let expected = vec![
            ctx("select id"),
            rem_e("from t", (5, 6)), // "t"
            add_e("from u", (5, 6)), // "u"
        ];
        assert_eq!(core_diffs["model.s.m"].lines, expected, "core frame");
        assert_eq!(
            fusion_diffs["model.s.m"].lines, expected,
            "fusion frame must be byte-identical to core (no phantom trailing line)",
        );
    }

    #[test]
    fn reconstruct_model_sql_diffs_emits_only_for_touched_aligned_models() {
        let ofp_edit = "models/edit.sql";
        let ofp_untouched = "models/untouched.sql";
        let ofp_stale = "models/stale.sql";
        let ofp_nodiff = "models/nodiff.sql"; // in scope but its file not in the diff

        let edit = model_with_raw("model.s.edit", "select a\nfrom edited", ofp_edit);
        let untouched = model_with_raw("model.s.untouched", "select a\nfrom u", ofp_untouched);
        let stale = model_with_raw("model.s.stale", "select a\nfrom s", ofp_stale);
        let nodiff = model_with_raw("model.s.nodiff", "select a\nfrom n", ofp_nodiff);
        let current = manifest_with_models(vec![edit, untouched, stale, nodiff]);

        let diff = PrDiff {
            renames: Vec::new(),
            files: vec![
                // edit.sql: line 2 replaced, `+` matches working tree → aligned + touched.
                FileHunks {
                    path: ofp_edit.to_owned(),
                    hunks: vec![replace(2, &["from was"], &["from edited"])],
                },
                // untouched.sql: change at line 1 only? No — make it a hunk that
                // does not overlap the file at all by pointing past EOF.
                FileHunks {
                    path: ofp_untouched.to_owned(),
                    hunks: vec![repl(9, &["zzz"])], // line 9 > 2-line file → no touch
                },
                // stale.sql: `+` body does not match the working tree → N7b fail.
                FileHunks {
                    path: ofp_stale.to_owned(),
                    hunks: vec![replace(2, &["from was"], &["from DRIFTED"])],
                },
                // nodiff.sql intentionally NOT in the diff.
            ],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        let scope = ModelInScopeSet::from_iter([
            NodeId::new("model.s.edit"),
            NodeId::new("model.s.untouched"),
            NodeId::new("model.s.stale"),
            NodeId::new("model.s.nodiff"),
        ]);

        let diffs = reconstruct_model_sql_diffs(&current, &scope, &index);

        assert!(
            diffs.contains_key("model.s.edit"),
            "edited model → SQL diff"
        );
        assert!(
            !diffs.contains_key("model.s.untouched"),
            "hunk outside the file frame → no diff (plain SQL)",
        );
        assert!(
            !diffs.contains_key("model.s.stale"),
            "stale (N7b-misaligned) diff → no diff",
        );
        assert!(
            !diffs.contains_key("model.s.nodiff"),
            "model whose .sql is not in the diff → no diff (in scope via a changed test)",
        );

        assert_eq!(
            diffs["model.s.edit"].lines,
            vec![
                ctx("select a"),
                rem_e("from was", (5, 8)),     // "was"
                add_e("from edited", (5, 11)), // "edited"
            ],
        );
    }

    #[test]
    fn reconstruct_model_sql_diffs_skips_a_whitespace_only_model_change() {
        // A re-indented model SQL: the whole-file hunk's `+` lines match the
        // working tree (N7b passes) and touch the block, but every pair is
        // ws_equal → has_real_change() is false → no BlockDiff (plain SQL).
        let ofp = "models/reindent.sql";
        let raw = "select id\n    from t"; // working tree: line 2 indented
        let m = model_with_raw("model.s.reindent", raw, ofp);
        let current = manifest_with_models(vec![m]);

        let diff = PrDiff {
            renames: Vec::new(),
            files: vec![FileHunks {
                path: ofp.to_owned(),
                hunks: vec![replace(2, &["from t"], &["    from t"])],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        let scope = ModelInScopeSet::from_iter([NodeId::new("model.s.reindent")]);

        let diffs = reconstruct_model_sql_diffs(&current, &scope, &index);
        assert!(
            !diffs.contains_key("model.s.reindent"),
            "whitespace-only model SQL change → no diff (plain view)",
        );
    }

    #[test]
    fn reconstruct_model_sql_diffs_skips_a_model_without_raw_code() {
        // Defensive: a model lacking raw_code (None) can carry no SQL diff.
        let ofp = "models/noraw.sql";
        let m = Node::new(
            NodeId::new("model.s.noraw"),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            None, // no raw_code
            DependsOn::default(),
            Some(ofp.to_owned()),
            NodeConfig::default(),
            None,
            std::collections::BTreeMap::new(),
        );
        let current = manifest_with_models(vec![m]);
        let diff = PrDiff {
            renames: Vec::new(),
            files: vec![FileHunks {
                path: ofp.to_owned(),
                hunks: vec![replace(1, &["old"], &["new"])],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        let scope = ModelInScopeSet::from_iter([NodeId::new("model.s.noraw")]);
        assert!(reconstruct_model_sql_diffs(&current, &scope, &index).is_empty());
    }

    #[test]
    fn reconstruct_model_sql_diffs_skips_a_model_with_empty_raw_code() {
        // A model shipping `raw_code: ""` (some node types do) is treated as
        // absent — matches `build_model_payload`'s `raw_sql` filter, so we
        // never compute a diff the template would not show.
        let ofp = "models/empty.sql";
        let m = model_with_raw("model.s.empty", "", ofp);
        let current = manifest_with_models(vec![m]);
        let diff = PrDiff {
            renames: Vec::new(),
            files: vec![FileHunks {
                path: ofp.to_owned(),
                hunks: vec![replace(1, &["old"], &["new"])],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        let scope = ModelInScopeSet::from_iter([NodeId::new("model.s.empty")]);
        assert!(reconstruct_model_sql_diffs(&current, &scope, &index).is_empty());
    }

    // ----- Non-`--unified=0` (context-bearing) hunk safety -----
    //
    // A hunk's `new_len` (from the `@@` header range) is independent of
    // `added_lines.len()` (the counted `+` bodies). cute-dbt is contracted on
    // `--unified=0` (`new_len == added_lines.len()`), but the parser ACCEPTS
    // a default-context `git diff` (`consume_body_line` drops context lines),
    // yielding `new_len > added_lines.len()`. Reconstruction must never panic
    // on that (the "cute-dbt never panics on a bad diff" contract) and must
    // not mislabel context lines as Added — it degrades the block to the
    // plain view, consistent with the stale→plain-view fallback.

    /// A context-bearing hunk: `new_len` spans more new-side lines than the
    /// `added_lines` body carries (the default-`git diff` shape the
    /// `--unified=0` contract excludes).
    fn context_bearing(new_start: usize, new_len: usize, removed: &[&str], added: &[&str]) -> Hunk {
        Hunk {
            new_start,
            new_len, // deliberately != added.len()
            removed_lines: removed.iter().map(|s| (*s).to_owned()).collect(),
            added_lines: added.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn reconstruct_one_degrades_on_a_context_bearing_hunk_without_panicking() {
        // `new_len = 3` but only one `+` body — a default-context git diff.
        // Must NOT panic (no `added_lines[k]` OOB) and must NOT label the
        // uncovered new-side lines as Added.
        let raw = "select a\nfrom t\nwhere x"; // [1,3]
        let hunks = [context_bearing(1, 3, &["from was"], &["from t"])];
        let got = reconstruct_one(
            &BlockSpan::new(raw, 1, 3),
            &hunks.iter().collect::<Vec<_>>(),
        );
        // Degrade to plain view: a context-bearing hunk is not a trustworthy
        // line-precise diff, so the block carries no Added/Removed lines.
        assert!(
            !got.has_real_change(),
            "a context-bearing hunk degrades to the plain view; got {:?}",
            got.lines,
        );
    }

    #[test]
    fn reconstruct_model_sql_diffs_degrades_on_a_context_bearing_hunk() {
        let ofp = "models/ctx.sql";
        let m = model_with_raw("model.s.ctx", "select a\nfrom t\nwhere x", ofp);
        let current = manifest_with_models(vec![m]);
        // A default-context git diff: the `@@` claims 3 new-side lines but
        // only one `+` body is recorded (parser drops context lines).
        let diff = PrDiff {
            renames: Vec::new(),
            files: vec![FileHunks {
                path: ofp.to_owned(),
                hunks: vec![context_bearing(1, 3, &["from was"], &["from t"])],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        let scope = ModelInScopeSet::from_iter([NodeId::new("model.s.ctx")]);
        // No panic; degrades to plain SQL view (no entry).
        assert!(reconstruct_model_sql_diffs(&current, &scope, &index).is_empty());
    }

    // =================================================================
    // reverse_apply (cute-dbt#266) — OLD-side reconstruction
    // =================================================================

    /// A fully-specified hunk (bodies + footprint).
    fn full_hunk(new_start: usize, removed: &[&str], added: &[&str]) -> Hunk {
        Hunk {
            new_start,
            new_len: added.len(),
            removed_lines: removed.iter().map(|s| (*s).to_owned()).collect(),
            added_lines: added.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    // ----- the two locked properties -----

    #[test]
    fn reverse_apply_with_empty_hunks_is_identity() {
        // Identity over every framing shape: trailing newline, none,
        // empty file, interior blank lines, CRLF.
        for text in [
            "a\nb\nc\n",
            "a\nb\nc",
            "",
            "\n",
            "a\n\nb\n",
            "a\r\nb\r\n",
            "single",
        ] {
            assert_eq!(
                reverse_apply(text, &[]).expect("empty hunks always apply"),
                text,
                "identity must hold for {text:?}",
            );
        }
    }

    /// One scripted edit over the property-test base file (1-based old
    /// lines). Declared at module-test level so the pool builder and the
    /// forward applier stay small, independently readable helpers
    /// (CRAP-gate decomposition, cute-dbt#266 review).
    #[derive(Clone, Copy, Debug)]
    enum Edit {
        /// Replace old line `at` with two new lines.
        Replace { at: usize },
        /// Insert one new line after old line `after` (0 ⇒ at top).
        Insert { after: usize },
        /// Delete old line `at`.
        Delete { at: usize },
    }

    /// The structured edit-script pool: every single edit at every base
    /// line, plus ordered pairs at non-adjacent old lines (1 and 3 /
    /// 1 and 4 / 2 and 4) so the constructed hunks are trivially
    /// non-overlapping.
    fn edit_script_pool(base_len: usize) -> Vec<Vec<Edit>> {
        let mut pool: Vec<Vec<Edit>> = (1..=base_len)
            .flat_map(|i| {
                [
                    vec![Edit::Replace { at: i }],
                    vec![Edit::Insert { after: i - 1 }],
                    vec![Edit::Delete { at: i }],
                ]
            })
            .collect();
        pool.extend([
            vec![Edit::Replace { at: 1 }, Edit::Replace { at: 3 }],
            vec![Edit::Delete { at: 1 }, Edit::Insert { after: 3 }],
            vec![Edit::Insert { after: 0 }, Edit::Delete { at: 4 }],
            vec![Edit::Replace { at: 1 }, Edit::Delete { at: 4 }],
            vec![Edit::Delete { at: 2 }, Edit::Replace { at: 4 }],
        ]);
        pool
    }

    /// Forward-apply one edit script over the OLD lines, returning the
    /// NEW lines plus the `--unified=0` hunks recorded against new-side
    /// numbering — exactly the `(new_text, hunks)` shape `git diff`
    /// would emit for that edit.
    fn forward_apply(base: &[&str], script: &[Edit]) -> (Vec<String>, Vec<Hunk>) {
        let mut new_lines: Vec<String> = Vec::new();
        let mut hunks: Vec<Hunk> = Vec::new();
        for (idx0, old_line) in base.iter().enumerate() {
            let at = idx0 + 1; // 1-based old line
            // Top-of-file insertion (after == 0) fires before line 1.
            if at == 1 && script_inserts_after(script, 0) {
                push_insertion(&mut new_lines, &mut hunks, "inserted-top: true");
            }
            match line_edit(script, at) {
                Some(Edit::Replace { .. }) => {
                    let r1 = format!("{old_line} # edited");
                    let r2 = "extra: line".to_owned();
                    hunks.push(full_hunk(new_lines.len() + 1, &[old_line], &[&r1, &r2]));
                    new_lines.push(r1);
                    new_lines.push(r2);
                }
                Some(Edit::Delete { .. }) => {
                    hunks.push(full_hunk(new_lines.len(), &[old_line], &[]));
                }
                _ => new_lines.push((*old_line).to_owned()),
            }
            if script_inserts_after(script, at) {
                push_insertion(
                    &mut new_lines,
                    &mut hunks,
                    &format!("inserted-after-{at}: true"),
                );
            }
        }
        (new_lines, hunks)
    }

    /// The Replace/Delete edit (if any) the script addresses at old line
    /// `at`. Insertions anchor between lines and are handled separately.
    fn line_edit(script: &[Edit], at: usize) -> Option<Edit> {
        script
            .iter()
            .find(|e| match e {
                Edit::Replace { at: a } | Edit::Delete { at: a } => *a == at,
                Edit::Insert { .. } => false,
            })
            .copied()
    }

    /// Whether the script inserts a new line after old line `after`.
    fn script_inserts_after(script: &[Edit], after: usize) -> bool {
        script
            .iter()
            .any(|e| matches!(e, Edit::Insert { after: a } if *a == after))
    }

    /// Record one pure-insertion hunk and its new-side line.
    fn push_insertion(new_lines: &mut Vec<String>, hunks: &mut Vec<Hunk>, line: &str) {
        hunks.push(full_hunk(new_lines.len() + 1, &[], &[line]));
        new_lines.push(line.to_owned());
    }

    /// forward∘reverse == identity, exercised exhaustively over a
    /// structured edit-script pool (the house property-test style — no
    /// proptest dep): every single edit and every ordered pair of
    /// non-overlapping edits over a fixed base file, with and without a
    /// trailing newline. The forward side is constructed BY the edit
    /// script ([`forward_apply`] — replace/insert/delete at a 1-based
    /// old line), so the test derives `(new_text, hunks)` pairs exactly
    /// shaped like `git diff --unified=0` output and asserts the
    /// reversal returns the original old text.
    #[test]
    fn reverse_apply_inverts_forward_application_over_edit_script_pool() {
        let base = ["name: playground", "version: '1.0'", "vars:", "  x: 1"];
        for trailing in [true, false] {
            for script in edit_script_pool(base.len()) {
                let (new_lines, hunks) = forward_apply(&base, &script);
                let frame = if trailing { "\n" } else { "" };
                let old_text = format!("{}{frame}", base.join("\n"));
                let new_text = format!("{}{frame}", new_lines.join("\n"));
                let reversed = reverse_apply(&new_text, &hunks)
                    .unwrap_or_else(|e| panic!("script {script:?} must reverse: {e:?}"));
                assert_eq!(
                    reversed, old_text,
                    "forward∘reverse must be identity for {script:?} (trailing={trailing})",
                );
            }
        }
    }

    // ----- drift guard: fail-closed, never silently wrong -----

    #[test]
    fn reverse_apply_rejects_a_hunk_whose_added_body_does_not_match() {
        let new_text = "a\nb\nc\n";
        let stale = full_hunk(2, &["was"], &["NOT b"]);
        assert_eq!(
            reverse_apply(new_text, &[stale]),
            Err(ReverseApplyError::Drift { new_start: 2 }),
            "a stale + body must refuse, never fabricate old text",
        );
    }

    #[test]
    fn reverse_apply_rejects_a_context_bearing_hunk() {
        // new_len 3 with one + body — the default-`git diff` shape.
        let h = Hunk {
            new_start: 1,
            new_len: 3,
            removed_lines: vec!["was".to_owned()],
            added_lines: vec!["a".to_owned()],
        };
        assert_eq!(
            reverse_apply("a\nb\nc\n", &[h]),
            Err(ReverseApplyError::ContextBearing { new_start: 1 }),
        );
    }

    #[test]
    fn reverse_apply_rejects_an_out_of_bounds_footprint() {
        let h = full_hunk(9, &[], &["z"]);
        assert_eq!(
            reverse_apply("a\nb\n", &[h]),
            Err(ReverseApplyError::OutOfBounds { new_start: 9 }),
        );
    }

    #[test]
    fn reverse_apply_rejects_overlapping_hunks() {
        let a = full_hunk(1, &["old-a"], &["a", "b"]);
        let b = full_hunk(2, &["old-b"], &["b", "c"]);
        assert_eq!(
            reverse_apply("a\nb\nc\n", &[a, b]),
            Err(ReverseApplyError::Overlapping { new_start: 2 }),
        );
    }

    // ----- edge cases: creation / deletion / trailing newline / CRLF -----

    #[test]
    fn reverse_apply_of_a_file_creation_yields_the_empty_string() {
        // `git diff` for a new file: one hunk covering the whole file,
        // no removed lines (`@@ -0,0 +1,N @@`). The caller treats empty
        // old text as "the file did not exist".
        let new_text = "name: playground\nversion: '1.0'\n";
        let h = full_hunk(1, &[], &["name: playground", "version: '1.0'"]);
        assert_eq!(reverse_apply(new_text, &[h]).expect("reverses"), "");
    }

    #[test]
    fn reverse_apply_reinserts_a_pure_deletion_after_its_anchor() {
        // Old: a / gone-1 / gone-2 / b — the deletion sits after new line 1.
        let h = full_hunk(1, &["gone-1", "gone-2"], &[]);
        assert_eq!(
            reverse_apply("a\nb\n", &[h]).expect("reverses"),
            "a\ngone-1\ngone-2\nb\n",
        );
    }

    #[test]
    fn reverse_apply_reinserts_a_top_of_file_deletion_before_line_one() {
        // `@@ -1,1 +0,0 @@` — new_start 0: the removed line led the file.
        let h = full_hunk(0, &["gone-first"], &[]);
        assert_eq!(
            reverse_apply("a\nb\n", &[h]).expect("reverses"),
            "gone-first\na\nb\n",
        );
    }

    #[test]
    fn reverse_apply_preserves_the_new_texts_trailing_newline_framing() {
        let h = full_hunk(2, &["was-b"], &["b"]);
        assert_eq!(
            reverse_apply("a\nb\n", std::slice::from_ref(&h)).unwrap(),
            "a\nwas-b\n",
        );
        assert_eq!(reverse_apply("a\nb", &[h]).unwrap(), "a\nwas-b");
    }

    #[test]
    fn reverse_apply_tolerates_a_crlf_working_tree() {
        // The diff parser strips `\r` from bodies; the working-tree text
        // keeps it. The drift guard must not report spurious drift.
        let h = full_hunk(1, &["old-a"], &["a"]);
        let reversed = reverse_apply("a\r\nb\r\n", &[h]).expect("CRLF must not read as drift");
        assert_eq!(reversed, "old-a\nb\r\n");
    }

    #[test]
    fn reverse_apply_emptying_a_file_restores_the_removed_lines() {
        // `@@ -1,2 +0,0 @@` against an empty new text.
        let h = full_hunk(0, &["a", "b"], &[]);
        assert_eq!(reverse_apply("", &[h]).expect("reverses"), "a\nb");
    }

    // ----- raw_hunk_lines (the Shape-A fallback content) -----

    #[test]
    fn raw_hunk_lines_renders_removed_then_added_per_hunk_in_order() {
        let hunks = vec![
            full_hunk(1, &["old-1"], &["new-1"]),
            full_hunk(5, &[], &["new-5\r"]),
        ];
        let lines = raw_hunk_lines(&hunks);
        assert_eq!(
            lines,
            vec![
                DiffLine {
                    kind: DiffLineKind::Removed,
                    text: "old-1".to_owned(),
                    emphasis: None,
                },
                DiffLine {
                    kind: DiffLineKind::Added,
                    text: "new-1".to_owned(),
                    emphasis: None,
                },
                DiffLine {
                    kind: DiffLineKind::Added,
                    text: "new-5".to_owned(),
                    emphasis: None,
                },
            ],
            "removed→added per hunk, \\r-trimmed, no emphasis",
        );
    }
}
