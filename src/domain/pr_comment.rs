//! GitHub PR review-thread PODs (cute-dbt#395, epic #353).
//!
//! A reviewer's comments on a pull request are an input cute-dbt can
//! surface alongside the diff it already renders. GitHub's GraphQL API
//! exposes two kinds:
//!
//! - **review threads** — line-anchored conversations on the diff
//!   (`PullRequestReviewThread`): a `path`, an anchor `line` /
//!   `originalLine`, a `diffSide` (`LEFT`/`RIGHT`), `isResolved` /
//!   `isOutdated` state, and one or more comments. These are what a
//!   later slice will *anchor* into the rendered diff (anchoring is
//!   blocked on #393's shared anchor primitive — this slice ingests
//!   only).
//! - **general PR comments** — the issue-level conversation
//!   (`IssueComment`): an `author` + `body`, not tied to any line.
//!
//! [`PrCommentThread`] and [`PrComment`] are the owned carriers the
//! gen-time adapter ([`crate::adapters::pr_comments`]) fills from the
//! `gh api graphql` response, and that a later render slice consumes.
//!
//! POD-only (ADR-2): owned data, `serde` derive, `std` + `serde` only —
//! the domain-purity invariant (`tests/domain_clean_arch.rs`). No I/O,
//! no GraphQL knowledge, no clap. The adapter owns the wire shape; the
//! domain owns the normalized facts. Every field is plain owned data so
//! the carrier crosses serde cleanly and stays additive (ADR-5): a new
//! GraphQL fact arrives as a new field, never a restructure.

use serde::{Deserialize, Serialize};

/// Which side of the diff a review thread is anchored to — GitHub's
/// `DiffSide` enum (`LEFT` = the base/old side, `RIGHT` = the head/new
/// side). A thread on a freshly-added line is `Right`; one on a deleted
/// line is `Left`.
///
/// [`DiffSide::Unknown`] preserves any value the API adds in the future
/// (its raw spelling) rather than dropping the thread — the same
/// fail-soft posture the whole rung takes. The wire form is a SCREAMING
/// enum; serde maps the two known spellings and falls back to
/// [`DiffSide::Unknown`] for anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffSide {
    /// The base (old) side of the diff — `LEFT` in the GraphQL enum.
    Left,
    /// The head (new) side of the diff — `RIGHT` in the GraphQL enum.
    Right,
    /// A spelling cute-dbt does not (yet) model — carried verbatim so an
    /// API addition never silently drops a thread.
    Unknown(String),
}

impl DiffSide {
    /// Map a raw GraphQL `DiffSide` token onto the POD. `LEFT`/`RIGHT`
    /// are the only two values the schema defines today; anything else
    /// is carried in [`DiffSide::Unknown`].
    #[must_use]
    pub fn from_wire(raw: &str) -> Self {
        match raw {
            "LEFT" => Self::Left,
            "RIGHT" => Self::Right,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

/// One comment within a review thread (a `PullRequestReviewComment`) or
/// a general PR comment (an `IssueComment`).
///
/// Only the facts a reviewer-context surface needs are carried: who
/// wrote it ([`Self::author`]) and what they said ([`Self::body`]). The
/// author is `None` for a deleted/ghost account (GitHub returns a null
/// `author` then) — a truthful absence, never an empty string standing
/// in for a real login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrComment {
    /// The comment author's GitHub login (e.g. `octocat`). `None` when
    /// the account was deleted (GraphQL `author: null`).
    pub author: Option<String>,
    /// The comment body, verbatim (GitHub-flavored Markdown source).
    pub body: String,
}

/// One line-anchored review thread on a pull request (a
/// `PullRequestReviewThread`).
///
/// The anchor fields ([`Self::path`], [`Self::line`],
/// [`Self::original_line`], [`Self::diff_side`]) are what a later
/// anchoring slice maps onto the rendered diff; this slice ingests them
/// as owned facts only. [`Self::line`] is the thread's position in the
/// **current** diff and is `None` once the thread is outdated (the line
/// no longer exists in the latest diff); [`Self::original_line`] is its
/// position at the comment's original commit and persists even when
/// outdated — so the two together let a consumer place a thread whether
/// or not it has gone stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrCommentThread {
    /// The file the thread is anchored to, repo-relative (GraphQL
    /// `path`, a non-null `String`).
    pub path: String,
    /// The thread's line in the **current** diff. `None` when the thread
    /// is outdated and the line no longer maps (GraphQL `line`, a
    /// nullable `Int`).
    pub line: Option<u32>,
    /// The thread's line at its original commit — present even when the
    /// thread is outdated (GraphQL `originalLine`, a nullable `Int`).
    pub original_line: Option<u32>,
    /// Which side of the diff the thread anchors to (GraphQL
    /// `diffSide`).
    pub diff_side: DiffSide,
    /// Whether the thread has been marked resolved (GraphQL
    /// `isResolved`).
    pub is_resolved: bool,
    /// Whether the thread is outdated — its anchored line has changed
    /// since the comment was written (GraphQL `isOutdated`).
    pub is_outdated: bool,
    /// The commit the first comment was written against (GraphQL
    /// `comments.nodes[0].commit.oid`, a 40-char SHA). `None` when the
    /// thread carries no commit (an empty/degenerate thread).
    pub commit_oid: Option<String>,
    /// The most-recent diff hunk the thread is anchored to (GraphQL
    /// `comments.nodes[0].diffHunk`) — the few lines of unified-diff
    /// context GitHub shows above a review comment. `None` when absent.
    pub diff_hunk: Option<String>,
    /// The thread's comments, in API order (oldest first).
    pub comments: Vec<PrComment>,
}

/// The full PR-comment ingestion result: the line-anchored review
/// threads plus the general (issue-level) PR comments.
///
/// A single owned carrier so the adapter returns one value and a later
/// render slice consumes one value. Both vectors are empty on the
/// fail-soft path (no `gh`, no PR, unparseable response) — never an
/// error: PR comments are context cute-dbt surfaces when present, never
/// a dependency it requires.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrComments {
    /// The line-anchored review threads (`reviewThreads.nodes`).
    pub threads: Vec<PrCommentThread>,
    /// The general issue-level PR comments (`comments.nodes`).
    pub general: Vec<PrComment>,
}

impl PrComments {
    /// Whether the ingestion found nothing to surface — both the threads
    /// and the general comments are empty. The fail-soft adapter path
    /// returns this; a render slice treats it as "no PR-comment section".
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.threads.is_empty() && self.general.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_side_maps_the_two_known_wire_spellings() {
        assert_eq!(DiffSide::from_wire("LEFT"), DiffSide::Left);
        assert_eq!(DiffSide::from_wire("RIGHT"), DiffSide::Right);
    }

    #[test]
    fn diff_side_preserves_an_unknown_spelling_verbatim() {
        assert_eq!(
            DiffSide::from_wire("BOTH"),
            DiffSide::Unknown("BOTH".to_owned())
        );
    }

    #[test]
    fn pr_comments_default_is_empty() {
        let comments = PrComments::default();
        assert!(comments.is_empty());
        assert!(comments.threads.is_empty());
        assert!(comments.general.is_empty());
    }

    #[test]
    fn pr_comments_is_empty_is_false_when_a_thread_is_present() {
        let comments = PrComments {
            threads: vec![PrCommentThread {
                path: "models/a.sql".to_owned(),
                line: Some(3),
                original_line: Some(3),
                diff_side: DiffSide::Right,
                is_resolved: false,
                is_outdated: false,
                commit_oid: None,
                diff_hunk: None,
                comments: vec![],
            }],
            general: vec![],
        };
        assert!(!comments.is_empty());
    }

    #[test]
    fn pr_comments_is_empty_is_false_when_only_a_general_comment_is_present() {
        let comments = PrComments {
            threads: vec![],
            general: vec![PrComment {
                author: Some("octocat".to_owned()),
                body: "looks good".to_owned(),
            }],
        };
        assert!(!comments.is_empty());
    }

    /// The carrier crosses serde round-trip unchanged (ADR-5: the
    /// pre-composed payload travels serde to the renderer).
    #[test]
    fn pr_comments_round_trips_through_serde_json() {
        let original = PrComments {
            threads: vec![PrCommentThread {
                path: "models/staging/stg_orders.sql".to_owned(),
                line: None,
                original_line: Some(12),
                diff_side: DiffSide::Left,
                is_resolved: true,
                is_outdated: true,
                commit_oid: Some("a".repeat(40)),
                diff_hunk: Some("@@ -1,3 +1,3 @@\n-old\n+new".to_owned()),
                comments: vec![
                    PrComment {
                        author: Some("reviewer-one".to_owned()),
                        body: "nit: rename this CTE".to_owned(),
                    },
                    PrComment {
                        author: None,
                        body: "agreed".to_owned(),
                    },
                ],
            }],
            general: vec![PrComment {
                author: Some("reviewer-two".to_owned()),
                body: "overall LGTM".to_owned(),
            }],
        };

        let json = serde_json::to_string(&original).expect("serialize PrComments");
        let restored: PrComments = serde_json::from_str(&json).expect("deserialize PrComments");

        assert_eq!(original, restored);
    }
}
