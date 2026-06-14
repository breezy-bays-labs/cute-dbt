//! Integration contract for the GitHub PR review-thread ingestion
//! adapter (cute-dbt#395, epic #353).
//!
//! Reads the committed **synthetic** `gh api graphql` response fixture
//! from disk (`tests/fixtures/pr-review-threads.json`) and parses it
//! through the public adapter into the domain POD, asserting the
//! thread/comment facts an anchoring slice will later consume. The
//! parse's exact edge behavior (null author, null line, unknown diff
//! side, malformed-response degrade) is pinned by the adapter's own unit
//! suite; this binary pins the end-to-end fixture-file → POD shape and
//! that the synthetic fixture is itself ingestible.
//!
//! Disk-level only — it does NOT spawn `gh` (the fail-soft spawn rung is
//! exercised by the adapter unit tests' degrade paths; a live `gh` is
//! never a test dependency).

use std::fs;
use std::path::{Path, PathBuf};

use cute_dbt::adapters::pr_comments::parse_pr_comments;
use cute_dbt::domain::DiffSide;

/// Absolute path to a committed fixture under `tests/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Read and parse the committed synthetic review-thread fixture.
fn parse_synthetic_fixture() -> cute_dbt::domain::PrComments {
    let path = fixture("pr-review-threads.json");
    let json =
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    parse_pr_comments(&json)
}

#[test]
fn synthetic_fixture_yields_three_threads_and_two_general_comments() {
    let parsed = parse_synthetic_fixture();
    assert_eq!(parsed.threads.len(), 3, "fixture has three review threads");
    assert_eq!(parsed.general.len(), 2, "fixture has two general comments");
    assert!(!parsed.is_empty());
}

#[test]
fn the_open_thread_carries_its_anchor_and_both_comments() {
    let parsed = parse_synthetic_fixture();
    let open = &parsed.threads[0];
    assert_eq!(open.path, "models/staging/stg_orders.sql");
    assert_eq!(open.line, Some(14));
    assert_eq!(open.original_line, Some(14));
    assert_eq!(open.diff_side, DiffSide::Right);
    assert!(!open.is_resolved);
    assert!(!open.is_outdated);
    assert_eq!(
        open.commit_oid.as_deref(),
        Some("1111111111111111111111111111111111111111"),
        "the anchoring (first) comment's commit oid",
    );
    assert!(
        open.diff_hunk
            .as_deref()
            .is_some_and(|h| h.starts_with("@@ -10,4 +10,6 @@")),
        "the anchoring comment's diff hunk",
    );
    assert_eq!(open.comments.len(), 2);
    assert_eq!(
        open.comments[0].author.as_deref(),
        Some("synthetic-reviewer")
    );
    assert_eq!(open.comments[1].author.as_deref(), Some("synthetic-author"));
}

#[test]
fn the_resolved_outdated_thread_has_null_line_left_side() {
    let parsed = parse_synthetic_fixture();
    let outdated = &parsed.threads[1];
    assert_eq!(outdated.path, "models/marts/fct_orders.sql");
    assert_eq!(outdated.line, None, "outdated ⇒ current line is null");
    assert_eq!(outdated.original_line, Some(8), "original line persists");
    assert_eq!(outdated.diff_side, DiffSide::Left);
    assert!(outdated.is_resolved);
    assert!(outdated.is_outdated);
}

#[test]
fn a_deleted_account_author_is_none_in_thread_and_general_comments() {
    let parsed = parse_synthetic_fixture();
    // The third thread's only comment has a null author.
    let ghost_review = &parsed.threads[2].comments[0];
    assert_eq!(ghost_review.author, None);
    assert!(ghost_review.body.contains("deleted account"));
    // The second general comment also has a null author.
    let ghost_general = &parsed.general[1];
    assert_eq!(ghost_general.author, None);
    assert!(ghost_general.body.contains("ghost account"));
}

#[test]
fn the_first_general_comment_carries_login_and_body() {
    let parsed = parse_synthetic_fixture();
    let first = &parsed.general[0];
    assert_eq!(first.author.as_deref(), Some("synthetic-reviewer"));
    assert!(first.body.contains("looks solid"));
}
