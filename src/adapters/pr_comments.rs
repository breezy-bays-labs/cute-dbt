//! GitHub PR review-thread ingestion — `gh api graphql` →
//! [`PrComments`] (cute-dbt#395, epic #353).
//!
//! A **gen-time** adapter: when cute-dbt is reviewing an open pull
//! request it can pull that PR's review threads and general comments via
//! the GitHub CLI and carry them as domain PODs alongside the diff it
//! already renders. This module ingests only — anchoring the threads
//! into the rendered diff is a later slice (blocked on #393's shared
//! anchor primitive).
//!
//! **Fail-soft by contract**, mirroring the existing gh rung in
//! `cli::review` (its `PrInfo` / `run_gh_pr_view` — a private module, so
//! not an intra-doc link): `gh` is a convenience, never a dependency. A
//! missing `gh` on PATH, a non-zero exit (no auth, no PR, network), or
//! an unparseable response all degrade to [`PrComments::default`] (an
//! empty result) — never a panic, never an error that blocks report
//! generation. PR comments are context cute-dbt surfaces when present.
//!
//! **Never in `report.html`**: this is a gen-time spawn, exactly like
//! the `review` verb's `gh pr view` rung — the generated report still
//! makes zero outbound requests when opened (the zero-egress gate is
//! about the rendered artifact, not the gen-time porcelain).
//!
//! **The pinned GraphQL query** (field names confirmed against GitHub's
//! live GraphQL schema via introspection, 2026-06-13):
//!
//! ```graphql
//! query($owner: String!, $repo: String!, $number: Int!) {
//!   repository(owner: $owner, name: $repo) {
//!     pullRequest(number: $number) {
//!       reviewThreads(first: 100) {
//!         nodes {
//!           isResolved isOutdated path line originalLine diffSide
//!           comments(first: 100) {
//!             nodes {
//!               author { login } body originalLine diffHunk
//!               commit { oid }
//!             }
//!           }
//!         }
//!       }
//!       comments(first: 100) {
//!         nodes { author { login } body url }
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! `reviewThreads` is a `PullRequestReviewThread` connection;
//! `comments` (on the PR itself) is an `IssueComment` connection — the
//! general, non-line-anchored conversation. The first page (100 of
//! each) is taken; deeper pagination is deferred (a PR with >100 review
//! threads is well past the point a reviewer reads them inline). The
//! per-thread `commit.oid` and `diffHunk` come from the thread's
//! **first** comment (the anchoring comment).

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::domain::{DiffSide, PrComment, PrCommentThread, PrComments};

/// The pinned GraphQL query (see the module docs). `gh api graphql`
/// substitutes `-F owner=… -F repo=… -F number=…` for the variables.
const REVIEW_THREADS_QUERY: &str = "\
query($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewThreads(first: 100) {
        nodes {
          isResolved
          isOutdated
          path
          line
          originalLine
          diffSide
          comments(first: 100) {
            nodes {
              author { login }
              body
              originalLine
              diffHunk
              commit { oid }
            }
          }
        }
      }
      comments(first: 100) {
        nodes {
          author { login }
          body
          url
        }
      }
    }
  }
}";

/// Fetch a PR's review threads + general comments via `gh api graphql`,
/// returning the parsed [`PrComments`] — or [`PrComments::default`] (an
/// empty result) on **any** failure (`gh` missing, non-zero exit,
/// unparseable JSON). Fail-soft by contract: PR comments are context
/// surfaced when present, never a dependency.
///
/// `cwd` is the directory the `gh` call runs from (the repo checkout, so
/// `gh` resolves the right repository); `owner` / `repo` / `number`
/// identify the PR. The hang bound is structural, not a wall-clock
/// timer: `stdin(Stdio::null())` denies `gh` an interactive prompt, so
/// it fails fast (non-zero exit) when it cannot answer — the same
/// guarantee `review`'s `run_gh_pr_view` relies on.
#[must_use]
pub fn fetch_pr_comments(cwd: &Path, owner: &str, repo: &str, number: u64) -> PrComments {
    let result = Command::new("gh")
        .args(["api", "graphql", "-f"])
        .arg(format!("query={REVIEW_THREADS_QUERY}"))
        .arg("-F")
        .arg(format!("owner={owner}"))
        .arg("-F")
        .arg(format!("repo={repo}"))
        .arg("-F")
        .arg(format!("number={number}"))
        .current_dir(cwd)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output();
    let Ok(output) = result else {
        // `gh` not on PATH (or another spawn error) — degrade to empty.
        return PrComments::default();
    };
    if !output.status.success() {
        return PrComments::default();
    }
    parse_pr_comments(&String::from_utf8_lossy(&output.stdout))
}

/// Parse a `gh api graphql` `reviewThreads` response into
/// [`PrComments`]. The pure half of the rung — unit-tested over the
/// synthetic fixture and edge shapes. Any shape the parser cannot use
/// (not an object, missing keys, wrong types) degrades to
/// [`PrComments::default`] rather than erroring: a malformed response is
/// "no comments to surface", not a fatal condition.
#[must_use]
pub fn parse_pr_comments(json: &str) -> PrComments {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return PrComments::default();
    };
    let pull_request = value
        .pointer("/data/repository/pullRequest")
        .unwrap_or(&Value::Null);
    PrComments {
        threads: parse_threads(pull_request),
        general: parse_general_comments(pull_request),
    }
}

/// Extract the line-anchored review threads from a `pullRequest` value.
fn parse_threads(pull_request: &Value) -> Vec<PrCommentThread> {
    thread_nodes(pull_request)
        .iter()
        .map(parse_thread)
        .collect()
}

/// The `reviewThreads.nodes` array, or an empty slice when absent.
fn thread_nodes(pull_request: &Value) -> &[Value] {
    pull_request
        .pointer("/reviewThreads/nodes")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// One `reviewThreads.nodes[i]` → a [`PrCommentThread`]. Missing scalars
/// degrade to sensible defaults (empty path, `None` line, `Unknown`
/// side, `false` flags) so a partial node still ingests rather than
/// dropping the thread.
fn parse_thread(node: &Value) -> PrCommentThread {
    let comments = comment_nodes(node);
    let parsed_comments: Vec<PrComment> = comments.iter().map(parse_comment).collect();
    // The anchoring (first) comment carries the commit + diff hunk.
    let first = comments.first();
    PrCommentThread {
        path: str_field(node, "path").unwrap_or_default(),
        line: u32_field(node, "line"),
        original_line: u32_field(node, "originalLine"),
        diff_side: str_field(node, "diffSide").map_or_else(
            || DiffSide::Unknown(String::new()),
            |s| DiffSide::from_wire(&s),
        ),
        is_resolved: bool_field(node, "isResolved"),
        is_outdated: bool_field(node, "isOutdated"),
        commit_oid: first
            .and_then(|c| str_field(c.pointer("/commit").unwrap_or(&Value::Null), "oid")),
        diff_hunk: first.and_then(|c| str_field(c, "diffHunk")),
        comments: parsed_comments,
    }
}

/// The `comments.nodes` array on a thread node, or an empty slice.
fn comment_nodes(node: &Value) -> &[Value] {
    node.pointer("/comments/nodes")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Extract the general (issue-level) PR comments from a `pullRequest`
/// value.
fn parse_general_comments(pull_request: &Value) -> Vec<PrComment> {
    pull_request
        .pointer("/comments/nodes")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |nodes| nodes.iter().map(parse_comment).collect())
}

/// One comment node (`PullRequestReviewComment` or `IssueComment`) →
/// [`PrComment`]. A `null` / absent `author` (deleted account) becomes
/// `None`, never an empty-string login.
fn parse_comment(node: &Value) -> PrComment {
    PrComment {
        author: node
            .pointer("/author/login")
            .and_then(Value::as_str)
            .map(str::to_owned),
        body: str_field(node, "body").unwrap_or_default(),
    }
}

/// Read a string field, `None` when absent or not a string.
fn str_field(node: &Value, key: &str) -> Option<String> {
    node.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Read a `bool` field, `false` when absent or not a bool.
fn bool_field(node: &Value, key: &str) -> bool {
    node.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Read a non-negative integer field as `u32`. `None` when absent, null,
/// not an integer, or out of `u32` range (a line number never exceeds
/// it, but be defensive about the wire).
fn u32_field(node: &Value, key: &str) -> Option<u32> {
    node.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed but faithful slice of the synthetic fixture: an open
    /// RIGHT-side thread with two comments and a resolved+outdated
    /// LEFT-side thread whose `line` is null, plus one general comment.
    const SAMPLE: &str = r#"{
      "data": { "repository": { "pullRequest": {
        "reviewThreads": { "nodes": [
          {
            "isResolved": false, "isOutdated": false,
            "path": "models/a.sql", "line": 14, "originalLine": 14,
            "diffSide": "RIGHT",
            "comments": { "nodes": [
              { "author": { "login": "rev" }, "body": "first",
                "originalLine": 14, "diffHunk": "@@ -1 +1 @@",
                "commit": { "oid": "abc123" } },
              { "author": { "login": "auth" }, "body": "reply",
                "originalLine": 14, "diffHunk": "@@ -1 +1 @@",
                "commit": { "oid": "abc123" } }
            ] }
          },
          {
            "isResolved": true, "isOutdated": true,
            "path": "models/b.sql", "line": null, "originalLine": 8,
            "diffSide": "LEFT",
            "comments": { "nodes": [
              { "author": null, "body": "ghost",
                "originalLine": 8, "diffHunk": "@@ -6 +6 @@",
                "commit": { "oid": "def456" } }
            ] }
          }
        ] },
        "comments": { "nodes": [
          { "author": { "login": "rev" }, "body": "LGTM",
            "url": "https://example.invalid/pr/1#c-1" }
        ] }
      } } }
    }"#;

    #[test]
    fn parses_both_threads_and_one_general_comment() {
        let parsed = parse_pr_comments(SAMPLE);
        assert_eq!(parsed.threads.len(), 2);
        assert_eq!(parsed.general.len(), 1);
        assert!(!parsed.is_empty());
    }

    #[test]
    fn parses_the_open_right_side_thread_facts() {
        let parsed = parse_pr_comments(SAMPLE);
        let open = &parsed.threads[0];
        assert_eq!(open.path, "models/a.sql");
        assert_eq!(open.line, Some(14));
        assert_eq!(open.original_line, Some(14));
        assert_eq!(open.diff_side, DiffSide::Right);
        assert!(!open.is_resolved);
        assert!(!open.is_outdated);
        assert_eq!(open.commit_oid.as_deref(), Some("abc123"));
        assert_eq!(open.diff_hunk.as_deref(), Some("@@ -1 +1 @@"));
        assert_eq!(open.comments.len(), 2);
        assert_eq!(open.comments[0].author.as_deref(), Some("rev"));
        assert_eq!(open.comments[0].body, "first");
        assert_eq!(open.comments[1].author.as_deref(), Some("auth"));
    }

    #[test]
    fn outdated_thread_has_null_line_but_keeps_original_line() {
        let parsed = parse_pr_comments(SAMPLE);
        let outdated = &parsed.threads[1];
        assert_eq!(
            outdated.line, None,
            "outdated thread's current line is null"
        );
        assert_eq!(outdated.original_line, Some(8), "original line persists");
        assert!(outdated.is_resolved);
        assert!(outdated.is_outdated);
        assert_eq!(outdated.diff_side, DiffSide::Left);
    }

    #[test]
    fn a_deleted_account_author_ingests_as_none() {
        let parsed = parse_pr_comments(SAMPLE);
        let ghost_comment = &parsed.threads[1].comments[0];
        assert_eq!(ghost_comment.author, None);
        assert_eq!(ghost_comment.body, "ghost");
    }

    #[test]
    fn general_comment_facts_are_carried() {
        let parsed = parse_pr_comments(SAMPLE);
        assert_eq!(parsed.general[0].author.as_deref(), Some("rev"));
        assert_eq!(parsed.general[0].body, "LGTM");
    }

    #[test]
    fn empty_response_degrades_to_empty() {
        let empty_pr = r#"{"data":{"repository":{"pullRequest":{
            "reviewThreads":{"nodes":[]},"comments":{"nodes":[]}}}}}"#;
        let parsed = parse_pr_comments(empty_pr);
        assert!(parsed.is_empty());
    }

    #[test]
    fn non_json_response_degrades_to_empty_never_panics() {
        assert!(parse_pr_comments("not json at all").is_empty());
        assert!(parse_pr_comments("").is_empty());
    }

    #[test]
    fn graphql_errors_envelope_degrades_to_empty() {
        // gh prints a JSON object with `errors` and a null `data` on a
        // GraphQL-level failure; that has no pullRequest → empty.
        let errors = r#"{"data":null,"errors":[{"message":"Could not resolve to a Repository"}]}"#;
        assert!(parse_pr_comments(errors).is_empty());
    }

    #[test]
    fn thread_with_no_comments_still_ingests() {
        let no_comments = r#"{"data":{"repository":{"pullRequest":{
            "reviewThreads":{"nodes":[
              {"isResolved":false,"isOutdated":false,"path":"models/c.sql",
               "line":1,"originalLine":1,"diffSide":"RIGHT",
               "comments":{"nodes":[]}}
            ]},
            "comments":{"nodes":[]}}}}}"#;
        let parsed = parse_pr_comments(no_comments);
        assert_eq!(parsed.threads.len(), 1);
        let thread = &parsed.threads[0];
        assert!(thread.comments.is_empty());
        assert_eq!(thread.commit_oid, None, "no first comment ⇒ no commit oid");
        assert_eq!(thread.diff_hunk, None);
    }

    #[test]
    fn unknown_diff_side_is_preserved_verbatim() {
        let weird = r#"{"data":{"repository":{"pullRequest":{
            "reviewThreads":{"nodes":[
              {"isResolved":false,"isOutdated":false,"path":"x.sql",
               "line":1,"originalLine":1,"diffSide":"BOTH",
               "comments":{"nodes":[]}}
            ]},
            "comments":{"nodes":[]}}}}}"#;
        let parsed = parse_pr_comments(weird);
        assert_eq!(
            parsed.threads[0].diff_side,
            DiffSide::Unknown("BOTH".to_owned())
        );
    }

    #[test]
    fn the_pinned_query_names_every_field_the_pod_carries() {
        // Guard against a query that silently stops requesting a field
        // the POD reads (the parse would then always see it absent).
        for field in [
            "reviewThreads",
            "isResolved",
            "isOutdated",
            "originalLine",
            "diffSide",
            "diffHunk",
            "oid",
            "comments",
        ] {
            assert!(
                REVIEW_THREADS_QUERY.contains(field),
                "pinned query is missing `{field}`"
            );
        }
    }
}
