//! Comment → rendered-diff-line anchoring (cute-dbt#418, epic #353).
//!
//! The crux of the PR-comments epic: take a [`PrCommentThread`] (the
//! ingested GitHub review thread, cute-dbt#395) and resolve it onto the
//! report's **rendered diff** for that file — or report an explicit
//! [`ThreadAnchor::Outdated`] when the comment's line has moved since it
//! was written (GitHub's outdated-comment case). The report renders a
//! `--pr-diff` as hunks ([`NormalizedDiffIndex`]); this module is the
//! pure join that places a thread's `(path, line, diff_side, commit_oid,
//! diff_hunk)` against those hunks.
//!
//! ## Why an explicit outdated state (never a mis-anchor)
//!
//! GitHub keeps a review comment around after the diff changes underneath
//! it, marking it `isOutdated` and nulling its current `line`
//! ([`PrCommentThread::line`] is `None` then;
//! [`PrCommentThread::original_line`] persists).
//! A naive anchor would either drop such a comment (losing reviewer
//! context) or pin it to the wrong line (worse — a false claim). This
//! resolver instead returns a first-class [`ThreadAnchor::Outdated`] so a
//! later render slice can surface the comment honestly ("this comment
//! refers to a line that changed"), exactly as GitHub's own UI does. The
//! never-a-false-claim posture mirrors `finding_anchor` (cute-dbt#393):
//! anchor only when an honest line exists, otherwise say so explicitly.
//!
//! ## The commit-basis check
//!
//! A comment is written against a specific commit
//! ([`PrCommentThread::commit_oid`]). When
//! the report's diff is built from a *different* basis (the PR advanced
//! since the comment), the comment's line numbers describe a revision the
//! report does not render. Even if GitHub has not yet flagged the thread
//! `isOutdated`, a basis mismatch means the rendered new-side line numbers
//! are not the comment's — so the resolver treats a known mismatch as
//! [`ThreadAnchor::Outdated`] rather than risk a stale anchor. The basis
//! is optional: callers that cannot determine the report's basis pass
//! `None` and fall back to GitHub's `isOutdated` flag and a within-hunk
//! check alone.
//!
//! ## Resolution states (exhaustive)
//!
//! - [`ThreadAnchor::Resolved`] — the thread's line maps to a concrete
//!   line in the rendered diff for its file; [`ResolvedThread::within_hunk`]
//!   records whether that line actually falls inside a changed hunk (the
//!   common case for a review comment) or merely in the file (a comment on
//!   an unchanged context line the report still shows).
//! - [`ThreadAnchor::Outdated`] — GitHub flagged the thread outdated, or
//!   its commit basis differs from the report's, or its current `line` is
//!   `None`. Carries the original-commit line so a consumer can still
//!   show *where it used to be*.
//! - [`ThreadAnchor::PathNotInDiff`] — the thread's file is not in the
//!   report's rendered diff at all (a comment on a file the report does
//!   not show). Distinct from outdated: the file, not the line, is absent.
//!
//! Pure domain (std + serde only): the resolver borrows the already-parsed
//! [`NormalizedDiffIndex`]; it does no I/O and never re-reads the diff.

use serde::{Deserialize, Serialize};

use crate::domain::pr_comment::{DiffSide, PrCommentThread};
use crate::domain::pr_diff::{Hunk, NormalizedDiffIndex};

/// A thread's line resolved against the report's rendered diff.
///
/// `line` is 1-based, on the side the thread anchors to
/// ([`side`](Self::side)): a
/// `Right`-side comment's `line` is a new-side line number; a `Left`-side
/// comment's is an old-side line number. `within_hunk` distinguishes a
/// comment that lands inside a changed hunk (the usual review case) from
/// one on a context line the report still renders. Owned POD — a later
/// render slice serializes it into the report payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedThread {
    /// The file the thread anchors to, normalized the same way the diff
    /// index keys its files (repo-relative, project-root strip already
    /// applied at index-build time).
    pub path: String,
    /// 1-based line on the thread's [`side`](Self::side).
    pub line: u32,
    /// Which side of the diff the line lives on.
    pub side: DiffSide,
    /// Whether [`line`](Self::line) falls inside a changed hunk (`true`)
    /// or only in the file's rendered context (`false`).
    pub within_hunk: bool,
}

/// The result of anchoring a [`PrCommentThread`] to the rendered diff.
///
/// Exhaustive over the cases a real GitHub review thread presents (see the
/// module docs). `#[non_exhaustive]` so a future signal (e.g. a recovered
/// line-moved offset) can become a new arm without a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ThreadAnchor {
    /// The thread anchors to a concrete rendered-diff line.
    Resolved(ResolvedThread),
    /// The thread cannot be honestly placed: GitHub flagged it outdated,
    /// its commit basis differs from the report's, or its current line is
    /// gone. `original_line` is the line it referred to at its original
    /// commit (the GitHub `originalLine`, `None` if also absent) so a
    /// consumer can still say *where it used to be*.
    Outdated {
        /// The file the (now-outdated) thread referred to.
        path: String,
        /// The thread's line at its original commit, if known.
        original_line: Option<u32>,
    },
    /// The thread's file is not in the report's rendered diff.
    PathNotInDiff {
        /// The absent file the thread referred to.
        path: String,
    },
}

/// Whether `line` (1-based, new side) falls inside any of `hunks`'
/// new-side footprints.
///
/// An insertion/replacement hunk spans new-side lines `[new_start,
/// new_start + new_len - 1]`. A pure-deletion hunk (`new_len == 0`) has no
/// new-side lines — a comment cannot land *inside* it — so it never
/// contains a new-side line (the deletion site is addressed Left-side).
fn line_in_new_side_hunk(line: u32, hunks: &[Hunk]) -> bool {
    let line = line as usize;
    hunks
        .iter()
        .any(|h| h.new_len > 0 && line >= h.new_start && line < h.new_start + h.new_len)
}

/// Whether `line` (1-based, old side) falls inside any of `hunks`'
/// old-side footprints.
///
/// The old-side footprint is reconstructed from the new-side anchor and
/// the removed-line count: a hunk's removed block occupies old-side lines
/// `[old_start, old_start + removed_len - 1]`, where `old_start` aligns
/// with the hunk's new-side anchor (a pure deletion sits *at* `new_start`;
/// a replacement at `new_start`). cute-dbt's [`Hunk`] does not retain
/// `old_start` separately, so this uses `new_start` as the alignment point
/// — sound for the `--unified=0` diffs the report consumes (each hunk's
/// removed block is co-located with its new-side anchor). A hunk with no
/// removed lines (a pure insertion) has no old-side footprint and never
/// contains a Left-side line.
fn line_in_old_side_hunk(line: u32, hunks: &[Hunk]) -> bool {
    let line = line as usize;
    hunks.iter().any(|h| {
        let removed_len = h.removed_lines.len();
        if removed_len == 0 {
            return false;
        }
        let old_start = h.new_start.max(1);
        line >= old_start && line < old_start + removed_len
    })
}

/// Anchor a review thread to the report's rendered diff, or report an
/// explicit non-anchorable state.
///
/// `report_basis` is the commit SHA the report's diff was built against,
/// when the caller knows it (`None` to skip the basis check and rely on
/// GitHub's `isOutdated` flag plus the within-hunk test alone). The path
/// is normalized through the same [`NormalizedDiffIndex`] the report
/// renders, so a sub-directory dbt project's project-root strip is honored
/// automatically.
///
/// Resolution order (each step is fail-honest — never a fabricated line):
///
/// 1. **Outdated** when GitHub flagged the thread `isOutdated`, when its
///    `commit_oid` differs from `report_basis` (a known basis mismatch),
///    or when its current `line` is `None` (the line is gone). The
///    `original_line` rides along so a consumer can show where it was.
/// 2. **`PathNotInDiff`** when the thread's file is not in the rendered
///    diff (no hunks for it). The file, not the line, is absent.
/// 3. **Resolved** otherwise: the thread has a live `line` on a file the
///    report renders. `within_hunk` records whether that line is inside a
///    changed hunk (`Right` → new-side footprint, `Left` → old-side
///    footprint) or only in the rendered file context.
///
/// Pure: borrows the parsed diff index, does no I/O.
#[must_use]
pub fn anchor_comment_thread(
    thread: &PrCommentThread,
    index: &NormalizedDiffIndex,
    report_basis: Option<&str>,
) -> ThreadAnchor {
    let basis_differs = match (thread.commit_oid.as_deref(), report_basis) {
        (Some(thread_oid), Some(basis)) => thread_oid != basis,
        // No basis to compare against (or the thread carries no commit) →
        // defer to GitHub's flag + the live-line check.
        _ => false,
    };

    // Step 1: outdated — GitHub flag, basis mismatch, or no live line.
    // `let … else` binds the live line and folds the `line == None` case
    // into the same Outdated return as the flag / basis-mismatch cases.
    let Some(line) = thread
        .line
        .filter(|_| !thread.is_outdated && !basis_differs)
    else {
        return ThreadAnchor::Outdated {
            path: thread.path.clone(),
            original_line: thread.original_line,
        };
    };

    // Step 2: path not in the rendered diff at all.
    if !index.contains_changed(&thread.path) {
        return ThreadAnchor::PathNotInDiff {
            path: thread.path.clone(),
        };
    }

    // Step 3: resolve against the file's hunks, recording within-hunk.
    let hunks = index.hunks_for(&thread.path);
    let within_hunk = match thread.diff_side {
        DiffSide::Right => line_in_new_side_hunk(line, hunks),
        DiffSide::Left => line_in_old_side_hunk(line, hunks),
        // An unknown side cannot be placed inside a hunk honestly; the
        // line is still rendered, so it resolves with within_hunk = false.
        DiffSide::Unknown(_) => false,
    };

    ThreadAnchor::Resolved(ResolvedThread {
        path: thread.path.clone(),
        line,
        side: thread.diff_side.clone(),
        within_hunk,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pr_comment::PrComment;
    use crate::domain::pr_diff::{FileHunks, PrDiff};

    // ----- builders -------------------------------------------------

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
        };
        NormalizedDiffIndex::new(&diff, None)
    }

    /// A thread builder with sensible live-on-the-right defaults; tests
    /// override the one field they exercise.
    fn thread(path: &str, line: Option<u32>, side: DiffSide) -> PrCommentThread {
        PrCommentThread {
            path: path.to_owned(),
            line,
            original_line: line,
            diff_side: side,
            is_resolved: false,
            is_outdated: false,
            commit_oid: Some("a".repeat(40)),
            diff_hunk: Some("@@ -1,3 +1,3 @@\n-old\n+new".to_owned()),
            comments: vec![PrComment {
                author: Some("octocat".to_owned()),
                body: "comment".to_owned(),
            }],
        }
    }

    // ===== Exhaustive enumeration of the anchor cases ================
    //
    // The bounded case space is:
    //   {path in diff, path absent}
    //   × {line present, line None}
    //   × {is_outdated true/false}
    //   × {basis matches, basis differs, basis unknown}
    //   × {side Right, Left, Unknown}
    //   × {line inside a hunk, line outside every hunk}
    // The named tests below cover every distinct outcome of that cube.

    // ----- Case: exact match (live line inside a hunk) --------------

    #[test]
    fn right_side_line_inside_a_hunk_resolves_within_hunk() {
        // Hunk spans new-side lines 5..=6 (new_len 2). Line 5 is inside.
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        );
        let t = thread("models/orders.sql", Some(5), DiffSide::Right);
        assert_eq!(
            anchor_comment_thread(&t, &index, Some(&"a".repeat(40))),
            ThreadAnchor::Resolved(ResolvedThread {
                path: "models/orders.sql".to_owned(),
                line: 5,
                side: DiffSide::Right,
                within_hunk: true,
            })
        );
    }

    #[test]
    fn right_side_last_line_of_a_hunk_is_within_hunk() {
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        );
        let t = thread("models/orders.sql", Some(6), DiffSide::Right);
        let anchor = anchor_comment_thread(&t, &index, None);
        match anchor {
            ThreadAnchor::Resolved(r) => assert!(r.within_hunk),
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    // ----- Case: line moved / still present but outside the hunk ----

    #[test]
    fn right_side_line_in_file_but_outside_every_hunk_resolves_not_within_hunk() {
        // The file is in the diff (a hunk at 5..=6), but the comment is on
        // line 40 — a context line the report renders, not a changed line.
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        );
        let t = thread("models/orders.sql", Some(40), DiffSide::Right);
        assert_eq!(
            anchor_comment_thread(&t, &index, None),
            ThreadAnchor::Resolved(ResolvedThread {
                path: "models/orders.sql".to_owned(),
                line: 40,
                side: DiffSide::Right,
                within_hunk: false,
            })
        );
    }

    // ----- Case: outdated commit (GitHub flag) ----------------------

    #[test]
    fn github_outdated_flag_yields_outdated_carrying_original_line() {
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        );
        let mut t = thread("models/orders.sql", Some(5), DiffSide::Right);
        t.is_outdated = true;
        t.original_line = Some(12);
        assert_eq!(
            anchor_comment_thread(&t, &index, None),
            ThreadAnchor::Outdated {
                path: "models/orders.sql".to_owned(),
                original_line: Some(12),
            }
        );
    }

    #[test]
    fn outdated_takes_precedence_even_when_the_line_would_be_in_a_hunk() {
        // The line WOULD resolve within a hunk, but isOutdated wins — never
        // a mis-anchor.
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        );
        let mut t = thread("models/orders.sql", Some(5), DiffSide::Right);
        t.is_outdated = true;
        assert!(matches!(
            anchor_comment_thread(&t, &index, None),
            ThreadAnchor::Outdated { .. }
        ));
    }

    // ----- Case: commit basis differs -------------------------------

    #[test]
    fn commit_basis_mismatch_yields_outdated() {
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        );
        let t = thread("models/orders.sql", Some(5), DiffSide::Right); // oid = "aaaa…"
        // The report was built against a DIFFERENT commit.
        assert_eq!(
            anchor_comment_thread(&t, &index, Some(&"b".repeat(40))),
            ThreadAnchor::Outdated {
                path: "models/orders.sql".to_owned(),
                original_line: Some(5),
            }
        );
    }

    #[test]
    fn matching_commit_basis_does_not_force_outdated() {
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        );
        let t = thread("models/orders.sql", Some(5), DiffSide::Right);
        assert!(matches!(
            anchor_comment_thread(&t, &index, Some(&"a".repeat(40))),
            ThreadAnchor::Resolved(_)
        ));
    }

    #[test]
    fn unknown_basis_defers_to_the_flag_and_live_line() {
        // commit_oid present, report_basis None → no basis check; resolves.
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        );
        let t = thread("models/orders.sql", Some(5), DiffSide::Right);
        assert!(matches!(
            anchor_comment_thread(&t, &index, None),
            ThreadAnchor::Resolved(_)
        ));
    }

    #[test]
    fn thread_without_commit_oid_skips_the_basis_check() {
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        );
        let mut t = thread("models/orders.sql", Some(5), DiffSide::Right);
        t.commit_oid = None;
        // Even with a report basis present, a thread with no commit can't
        // mismatch → resolves on its live line.
        assert!(matches!(
            anchor_comment_thread(&t, &index, Some(&"b".repeat(40))),
            ThreadAnchor::Resolved(_)
        ));
    }

    // ----- Case: line None (the line is gone) -----------------------

    #[test]
    fn missing_current_line_yields_outdated() {
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        );
        let mut t = thread("models/orders.sql", None, DiffSide::Right);
        t.original_line = Some(9);
        assert_eq!(
            anchor_comment_thread(&t, &index, None),
            ThreadAnchor::Outdated {
                path: "models/orders.sql".to_owned(),
                original_line: Some(9),
            }
        );
    }

    // ----- Case: path not in the diff -------------------------------

    #[test]
    fn path_not_in_diff_yields_path_not_in_diff() {
        // The diff touches a DIFFERENT file.
        let index = index_for("models/customers.sql", vec![hunk(1, 1, &[], &["x"])]);
        let t = thread("models/orders.sql", Some(5), DiffSide::Right);
        assert_eq!(
            anchor_comment_thread(&t, &index, None),
            ThreadAnchor::PathNotInDiff {
                path: "models/orders.sql".to_owned(),
            }
        );
    }

    #[test]
    fn outdated_is_checked_before_path_presence() {
        // An outdated comment on a file NOT in the diff is Outdated, not
        // PathNotInDiff — the outdated state is the more specific truth and
        // carries the original line.
        let index = index_for("models/customers.sql", vec![hunk(1, 1, &[], &["x"])]);
        let mut t = thread("models/orders.sql", Some(5), DiffSide::Right);
        t.is_outdated = true;
        assert!(matches!(
            anchor_comment_thread(&t, &index, None),
            ThreadAnchor::Outdated { .. }
        ));
    }

    // ----- Case: Left-side (deletion) anchoring ---------------------

    #[test]
    fn left_side_line_inside_a_removed_block_is_within_hunk() {
        // A replacement hunk anchored at new-side line 5 removes 2 old
        // lines → old-side footprint 5..=6. A Left comment on old line 5
        // is within the hunk.
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 1, &["o1", "o2"], &["n1"])],
        );
        let t = thread("models/orders.sql", Some(5), DiffSide::Left);
        let anchor = anchor_comment_thread(&t, &index, None);
        match anchor {
            ThreadAnchor::Resolved(r) => {
                assert!(r.within_hunk);
                assert_eq!(r.side, DiffSide::Left);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn left_side_pure_deletion_hunk_anchors_within() {
        // A pure deletion (new_len 0) at new_start 8 removes one old line →
        // old-side footprint 8..=8.
        let index = index_for("models/orders.sql", vec![hunk(8, 0, &["dropped"], &[])]);
        let t = thread("models/orders.sql", Some(8), DiffSide::Left);
        let anchor = anchor_comment_thread(&t, &index, None);
        match anchor {
            ThreadAnchor::Resolved(r) => assert!(r.within_hunk),
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn left_side_line_outside_the_removed_block_is_not_within_hunk() {
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 1, &["o1", "o2"], &["n1"])],
        );
        let t = thread("models/orders.sql", Some(99), DiffSide::Left);
        let anchor = anchor_comment_thread(&t, &index, None);
        match anchor {
            ThreadAnchor::Resolved(r) => assert!(!r.within_hunk),
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn left_side_pure_insertion_hunk_has_no_old_footprint() {
        // A pure insertion (no removed lines) → a Left comment is never
        // within the hunk (there is no old-side content there).
        let index = index_for("models/orders.sql", vec![hunk(5, 1, &[], &["added"])]);
        let t = thread("models/orders.sql", Some(5), DiffSide::Left);
        let anchor = anchor_comment_thread(&t, &index, None);
        match anchor {
            ThreadAnchor::Resolved(r) => assert!(!r.within_hunk),
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    // ----- Case: Unknown side ---------------------------------------

    #[test]
    fn unknown_side_resolves_but_never_within_hunk() {
        let index = index_for(
            "models/orders.sql",
            vec![hunk(5, 2, &["old"], &["n1", "n2"])],
        );
        let t = thread(
            "models/orders.sql",
            Some(5),
            DiffSide::Unknown("BOTH".to_owned()),
        );
        let anchor = anchor_comment_thread(&t, &index, None);
        match anchor {
            ThreadAnchor::Resolved(r) => {
                assert!(!r.within_hunk);
                assert_eq!(r.side, DiffSide::Unknown("BOTH".to_owned()));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    // ----- New-side hunk membership: boundary enumeration -----------

    #[test]
    fn new_side_membership_is_inclusive_at_both_ends_and_excludes_neighbors() {
        let hunks = vec![hunk(5, 3, &[], &["a", "b", "c"])]; // new-side 5..=7
        assert!(!line_in_new_side_hunk(4, &hunks)); // just before
        assert!(line_in_new_side_hunk(5, &hunks)); // first
        assert!(line_in_new_side_hunk(6, &hunks)); // middle
        assert!(line_in_new_side_hunk(7, &hunks)); // last
        assert!(!line_in_new_side_hunk(8, &hunks)); // just after
    }

    #[test]
    fn new_side_membership_excludes_pure_deletion_hunks() {
        // A pure deletion (new_len 0) has no new-side lines.
        let hunks = vec![hunk(5, 0, &["gone"], &[])];
        assert!(!line_in_new_side_hunk(5, &hunks));
    }

    #[test]
    fn old_side_membership_is_inclusive_at_both_ends() {
        // Replacement at new_start 5 removing 3 old lines → old-side 5..=7.
        let hunks = vec![hunk(5, 1, &["o1", "o2", "o3"], &["n1"])];
        assert!(!line_in_old_side_hunk(4, &hunks));
        assert!(line_in_old_side_hunk(5, &hunks));
        assert!(line_in_old_side_hunk(7, &hunks));
        assert!(!line_in_old_side_hunk(8, &hunks));
    }

    // ----- POD: serde round-trip ------------------------------------

    #[test]
    fn resolved_anchor_round_trips_through_serde_json() {
        let anchor = ThreadAnchor::Resolved(ResolvedThread {
            path: "models/orders.sql".to_owned(),
            line: 7,
            side: DiffSide::Right,
            within_hunk: true,
        });
        let json = serde_json::to_string(&anchor).expect("serialize");
        let back: ThreadAnchor = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(anchor, back);
    }

    #[test]
    fn outdated_anchor_round_trips_through_serde_json() {
        let anchor = ThreadAnchor::Outdated {
            path: "models/orders.sql".to_owned(),
            original_line: Some(12),
        };
        let json = serde_json::to_string(&anchor).expect("serialize");
        let back: ThreadAnchor = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(anchor, back);
    }

    #[test]
    fn path_not_in_diff_anchor_round_trips_through_serde_json() {
        let anchor = ThreadAnchor::PathNotInDiff {
            path: "models/orders.sql".to_owned(),
        };
        let json = serde_json::to_string(&anchor).expect("serialize");
        let back: ThreadAnchor = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(anchor, back);
    }

    #[test]
    fn the_state_tag_serializes_snake_case() {
        let json = serde_json::to_string(&ThreadAnchor::PathNotInDiff {
            path: "x".to_owned(),
        })
        .unwrap();
        assert!(
            json.contains("\"state\":\"path_not_in_diff\""),
            "got {json}"
        );
    }
}
