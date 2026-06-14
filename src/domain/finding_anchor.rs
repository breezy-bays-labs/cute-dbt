//! Finding → `(path, line)` anchor resolution (cute-dbt#393).
//!
//! The shared review-UX primitive: a pure domain fn that projects a
//! [`Finding`] onto the source `(path, line)` a GitHub review affordance
//! pins it to. Two consumers ride the same resolver — the workflow-command
//! annotation emit (`::warning file=,line=`, cute-dbt#393) and the
//! findings-envelope `anchor` slot (cute-dbt#388 / #353 follow-up): build
//! it once so a finding's rendered line is computed in exactly one place.
//!
//! ## Relationship to the envelope's reserved anchor (cute-dbt#388)
//!
//! cute-dbt#388 reserves an `anchor` slot on each envelope finding via its
//! own `findings_envelope::FindingAnchor` — a **wire DTO** whose every
//! field is `Option` (it serializes a *reserved-but-empty* anchor
//! byte-identically to a bare finding). This module is the *resolver* that
//! computes the populated value. They are deliberately distinct: this
//! returns a fully-resolved [`ResolvedAnchor`] (no `Option` fields — it
//! only ever returns `Some` when every part resolved), and the deferred
//! envelope-population follow-up (after #388 merges) maps a
//! [`ResolvedAnchor`] into the envelope's reserved wire slot
//! (`Some(path)` / `Some(line as u32)` / `Some(side)` + the `anchor_hash`
//! drift guard). One resolver, two projections — the annotation emit here
//! and the envelope slot there.
//!
//! ## What a finding carries (and what it doesn't)
//!
//! A [`Finding`] names a `model_id` + a `construct` + a `verdict` —
//! **never a line**. The model's declaring-file path is on its
//! [`Node`](crate::domain::manifest::Node)
//! ([`original_file_path`](crate::domain::manifest::Node::original_file_path),
//! cute-dbt#81), and the `--pr-diff` hunks are already parsed into a
//! [`NormalizedDiffIndex`]. The new work is the join: resolve the
//! finding's model node, look up that file's hunks, and pick the line a
//! reviewer should land on.
//!
//! ## Line-selection policy (the node-anchored fallback)
//!
//! A coverage finding on a model has **no single source line** — the gap
//! is a property of the whole model, not one statement. So the resolver
//! anchors a finding to the model file's **first changed line** (the
//! earliest new-side line any hunk touches), which is where a reviewer's
//! eye already is on the Files-changed tab. When the model's declaring
//! file is not in the diff at all (a finding on a context model that
//! rode into scope without its own `.sql` changing), there is no honest
//! line to pin → the resolver returns `None` and the finding stays
//! summary-only (it still rides the check-run roll-up, just without an
//! inline annotation). This is the cute-dbt#393 Discovery call: a
//! conservative "first changed line, else summary-only" — never a
//! fabricated line.
//!
//! ## Diff context
//!
//! [`AnchorSide`] (Plannotator's vocabulary) records whether the anchored
//! line sits in an `Added`, `Removed`, or `Modified` region, derived from
//! the touching hunk's `+`/`-` body composition: both sides present ⇒
//! `Modified`; only `+` ⇒ `Added`; only `-` ⇒ `Removed`. It is advisory
//! context for the consumer (an annotation can colour-code; the envelope
//! records provenance) — it never changes the `(path, line)` itself.
//!
//! Pure domain (std + serde only): the resolver borrows the already-parsed
//! [`Manifest`] and [`NormalizedDiffIndex`] — it does no I/O and never
//! re-reads the diff.

use serde::Serialize;

use crate::domain::checks::{CheckId, Finding};
use crate::domain::manifest::Manifest;
use crate::domain::pr_diff::{Hunk, NormalizedDiffIndex};

/// Where an anchored line sits relative to the change (Plannotator's
/// `added`/`removed`/`modified` vocabulary).
///
/// Advisory provenance for the consumer — a workflow-command annotation
/// can colour-code by it, the findings envelope records it — but it never
/// alters the resolved `(path, line)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorSide {
    /// The hunk at the anchored line is a pure insertion (only `+`
    /// lines).
    Added,
    /// The hunk at the anchored line is a pure deletion (only `-`
    /// lines) — the anchored line is the deletion site on the new side.
    Removed,
    /// The hunk at the anchored line carries both `+` and `-` bodies (an
    /// edit-in-place).
    Modified,
}

impl AnchorSide {
    /// The wire/string form (`added` / `removed` / `modified`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Modified => "modified",
        }
    }

    /// Classify a hunk by its `+`/`-` body composition.
    fn classify(hunk: &Hunk) -> Self {
        match (hunk.added_lines.is_empty(), hunk.removed_lines.is_empty()) {
            (false, false) => Self::Modified,
            (false, true) => Self::Added,
            // Pure deletion (`+` empty, `-` present) — and the degenerate
            // empty hunk, which never occurs in a real diff; classifying
            // it as `Removed` is inert (a hunk with no `+` lines anchors
            // at its deletion site).
            (true, _) => Self::Removed,
        }
    }
}

/// A finding's resolved source anchor — the `(path, line)` a GitHub
/// review affordance pins it to, plus its [`AnchorSide`].
///
/// `path` is the model's declaring-file path verbatim from the manifest
/// (`original_file_path`, project-relative); `line` is 1-based on the
/// diff's new side. Owned POD (the envelope serializes it; the annotation
/// formatter borrows it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedAnchor {
    /// The declaring-file path the line lives in (project-relative,
    /// verbatim from `original_file_path`).
    pub path: String,
    /// 1-based new-side line the finding anchors to.
    pub line: usize,
    /// Whether the anchored line sits in an added / removed / modified
    /// region.
    pub diff_context: AnchorSide,
}

/// The earliest new-side line any hunk touches, paired with that hunk's
/// [`AnchorSide`].
///
/// "Touches" means: for an insertion/replacement hunk, its `new_start`
/// (the first new-side line it spans); for a pure-deletion hunk
/// (`new_len == 0`), its `new_start` (the deletion site on the new side,
/// clamped to ≥ 1 so a deletion before line 1 still anchors honestly).
/// Picking the minimum across all of the file's hunks gives the model
/// file's first changed line — the node-anchored fallback target.
fn first_changed_line(hunks: &[Hunk]) -> Option<(usize, AnchorSide)> {
    hunks
        .iter()
        .map(|hunk| (hunk.new_start.max(1), AnchorSide::classify(hunk)))
        .min_by_key(|&(line, _)| line)
}

/// Resolve a finding to its source anchor, or `None` when no honest line
/// exists (summary-only).
///
/// The join: look up the finding's `model_id` in the manifest, read its
/// `original_file_path`, fetch that file's `--pr-diff` hunks
/// ([`NormalizedDiffIndex::hunks_for`]), and anchor at the file's first
/// changed line (the node-anchored fallback — see the module docs). Returns
/// `None` when the model is missing, has no `original_file_path`, or its
/// declaring file is not in the diff (no honest line to pin).
///
/// Pure: borrows the parsed manifest + diff index, does no I/O.
#[must_use]
pub fn resolve_finding_anchor<Id: CheckId>(
    finding: &Finding<Id>,
    manifest: &Manifest,
    index: &NormalizedDiffIndex,
) -> Option<ResolvedAnchor> {
    let node = manifest.nodes().get(&finding.model_id)?;
    let path = node.original_file_path()?;
    let hunks = index.hunks_for(path);
    let (line, diff_context) = first_changed_line(hunks)?;
    Some(ResolvedAnchor {
        path: path.to_owned(),
        line,
        diff_context,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::domain::checks::{HeuristicId, Verdict};
    use crate::domain::manifest::{
        Checksum, DependsOn, Manifest, ManifestMetadata, Node, NodeConfig, NodeId,
    };
    use crate::domain::pr_diff::{FileHunks, Hunk, PrDiff};

    // ----- builders -------------------------------------------------

    fn model_node(id: &str, original_file_path: Option<&str>) -> Node {
        Node::new(
            NodeId::new(id),
            "model",
            Checksum::new("sha256", "abc"),
            Some("select 1".to_owned()),
            Some("select 1".to_owned()),
            DependsOn::new(Vec::new(), Vec::new()),
            original_file_path.map(str::to_owned),
            NodeConfig::default(),
            None,
            std::collections::BTreeMap::new(),
        )
    }

    fn manifest_with(node: Node) -> Manifest {
        let mut nodes = HashMap::new();
        nodes.insert(node.id().clone(), node);
        Manifest::new(
            ManifestMetadata::new("https://schemas.getdbt.com/dbt/manifest/v12.json"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn hunk(new_start: usize, new_len: usize, removed: &[&str], added: &[&str]) -> Hunk {
        Hunk {
            new_start,
            new_len,
            removed_lines: removed.iter().map(|s| (*s).to_owned()).collect(),
            added_lines: added.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn index_for(path: &str, hunks: Vec<Hunk>) -> NormalizedDiffIndex {
        let diff = PrDiff {
            files: vec![FileHunks {
                path: path.to_owned(),
                hunks,
            }],
            renames: Vec::new(),
            deleted: Vec::new(),
            added: Vec::new(),
        };
        NormalizedDiffIndex::new(&diff, None)
    }

    fn finding(model_id: &str) -> Finding<HeuristicId> {
        Finding::new(
            HeuristicId::GrainUniqueKeyUnbacked,
            NodeId::new(model_id),
            "config.unique_key",
            Verdict::Uncovered,
            Vec::new(),
        )
    }

    // ----- AnchorSide::classify ------------------------------------

    #[test]
    fn classify_pure_insertion_is_added() {
        let h = hunk(10, 1, &[], &["new line"]);
        assert_eq!(AnchorSide::classify(&h), AnchorSide::Added);
    }

    #[test]
    fn classify_pure_deletion_is_removed() {
        let h = hunk(10, 0, &["gone"], &[]);
        assert_eq!(AnchorSide::classify(&h), AnchorSide::Removed);
    }

    #[test]
    fn classify_replacement_is_modified() {
        let h = hunk(10, 1, &["old"], &["new"]);
        assert_eq!(AnchorSide::classify(&h), AnchorSide::Modified);
    }

    #[test]
    fn diff_context_as_str_is_the_wire_form() {
        assert_eq!(AnchorSide::Added.as_str(), "added");
        assert_eq!(AnchorSide::Removed.as_str(), "removed");
        assert_eq!(AnchorSide::Modified.as_str(), "modified");
    }

    // ----- first_changed_line ---------------------------------------

    #[test]
    fn first_changed_line_picks_the_earliest_hunk() {
        let hunks = vec![
            hunk(30, 1, &[], &["c"]),
            hunk(10, 1, &[], &["a"]),
            hunk(20, 1, &[], &["b"]),
        ];
        assert_eq!(first_changed_line(&hunks), Some((10, AnchorSide::Added)));
    }

    #[test]
    fn first_changed_line_clamps_a_deletion_before_line_one_to_one() {
        let hunks = vec![hunk(0, 0, &["gone"], &[])];
        assert_eq!(first_changed_line(&hunks), Some((1, AnchorSide::Removed)));
    }

    #[test]
    fn first_changed_line_of_no_hunks_is_none() {
        assert_eq!(first_changed_line(&[]), None);
    }

    // ----- resolve_finding_anchor -----------------------------------

    #[test]
    fn resolves_to_the_model_files_first_changed_line() {
        let manifest = manifest_with(model_node("model.shop.orders", Some("models/orders.sql")));
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["new1", "new2"])],
        );
        let anchor = resolve_finding_anchor(&finding("model.shop.orders"), &manifest, &index);
        assert_eq!(
            anchor,
            Some(ResolvedAnchor {
                path: "models/orders.sql".to_owned(),
                line: 5,
                diff_context: AnchorSide::Modified,
            })
        );
    }

    #[test]
    fn anchors_at_the_earliest_line_across_multiple_hunks() {
        let manifest = manifest_with(model_node("model.shop.orders", Some("models/orders.sql")));
        let index = index_for(
            "models/orders.sql",
            vec![hunk(40, 1, &[], &["late"]), hunk(12, 1, &[], &["early"])],
        );
        let anchor =
            resolve_finding_anchor(&finding("model.shop.orders"), &manifest, &index).unwrap();
        assert_eq!(anchor.line, 12);
        assert_eq!(anchor.diff_context, AnchorSide::Added);
    }

    #[test]
    fn no_anchor_when_model_file_is_not_in_the_diff() {
        let manifest = manifest_with(model_node("model.shop.orders", Some("models/orders.sql")));
        // The diff touches a DIFFERENT file → no hunks for orders.sql →
        // summary-only.
        let index = index_for("models/customers.sql", vec![hunk(1, 1, &[], &["x"])]);
        assert_eq!(
            resolve_finding_anchor(&finding("model.shop.orders"), &manifest, &index),
            None
        );
    }

    #[test]
    fn no_anchor_when_model_missing_from_manifest() {
        let manifest = manifest_with(model_node("model.shop.other", Some("models/other.sql")));
        let index = index_for("models/orders.sql", vec![hunk(5, 1, &[], &["x"])]);
        assert_eq!(
            resolve_finding_anchor(&finding("model.shop.orders"), &manifest, &index),
            None
        );
    }

    #[test]
    fn no_anchor_when_model_has_no_original_file_path() {
        let manifest = manifest_with(model_node("model.shop.orders", None));
        let index = index_for("models/orders.sql", vec![hunk(5, 1, &[], &["x"])]);
        assert_eq!(
            resolve_finding_anchor(&finding("model.shop.orders"), &manifest, &index),
            None
        );
    }

    #[test]
    fn deletion_only_diff_anchors_at_the_deletion_site() {
        let manifest = manifest_with(model_node("model.shop.orders", Some("models/orders.sql")));
        let index = index_for("models/orders.sql", vec![hunk(8, 0, &["dropped"], &[])]);
        let anchor =
            resolve_finding_anchor(&finding("model.shop.orders"), &manifest, &index).unwrap();
        assert_eq!(anchor.line, 8);
        assert_eq!(anchor.diff_context, AnchorSide::Removed);
    }
}
