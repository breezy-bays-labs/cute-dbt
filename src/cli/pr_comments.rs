//! The `--pr-comments` value source: a `gh api graphql` `reviewThreads`
//! JSON payload (cute-dbt#419–#422, epic #353).
//!
//! The report can inline a PR's GitHub review comments alongside the diff
//! it renders. Two surfaces produce the same [`PrComments`] domain POD:
//!
//! - the **live** gen-time fetch —
//!   [`fetch_pr_comments`](crate::adapters::pr_comments::fetch_pr_comments)
//!   shells `gh api graphql` for the PR's review threads (the adapter,
//!   cute-dbt#395), and
//! - the **file** seam (this module) — `--pr-comments @<path>` reads a
//!   committed JSON fixture of that same payload. This is the
//!   deterministic injection point the comments-showcase golden + the BDD
//!   / headless suites use: a synthetic fixture stands in for the network
//!   `gh` call, so the committed example is reproducible without auth.
//!
//! `@file` is the canonical form (the only real usage); a value without a
//! leading `@` is parsed as the literal JSON (the unit-test form). The
//! ingestion is **fail-soft by contract** — any payload the parser cannot
//! use degrades to [`PrComments::default`] (an empty result), exactly the
//! adapter's posture: PR comments are context cute-dbt surfaces when
//! present, never a dependency it requires. So the *only* clap usage error
//! this value-parser raises is an unreadable / non-UTF-8 `@file` (a bad
//! operator path, the `--pr-diff @file` precedent); a present-but-malformed
//! payload is **not** an error — it yields the empty `PrComments`.

use std::fs;
use std::path::Path;

use crate::adapters::pr_comments::parse_pr_comments;
use crate::domain::PrComments;

/// clap value-parser for `--pr-comments`.
///
/// `@<path>` reads the JSON from a file; any other value is parsed as
/// literal JSON. The result is the parsed [`PrComments`] — an empty result
/// when the payload is present but malformed (the fail-soft contract).
///
/// # Errors
///
/// Only an `@file` that cannot be read or is not valid UTF-8 (a clap usage
/// error, exit 2 — the `--pr-diff @file` precedent). A readable-but-
/// malformed payload is **not** an error: it degrades to
/// [`PrComments::default`].
pub fn parse_pr_comments_arg(s: &str) -> Result<PrComments, String> {
    if let Some(file) = s.strip_prefix('@') {
        let contents = fs::read_to_string(Path::new(file))
            .map_err(|err| format!("could not read --pr-comments file at {file}: {err}"))?;
        return Ok(parse_pr_comments(&contents));
    }
    Ok(parse_pr_comments(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "data": { "repository": { "pullRequest": {
        "reviewThreads": { "nodes": [
          { "isResolved": false, "isOutdated": false,
            "path": "models/orders.sql", "line": 5, "originalLine": 5,
            "diffSide": "RIGHT",
            "comments": { "nodes": [
              { "author": { "login": "rev" }, "body": "nit",
                "originalLine": 5, "diffHunk": "@@ -1 +1 @@",
                "commit": { "oid": "abc" } }
            ] } }
        ] },
        "comments": { "nodes": [] }
      } } }
    }"#;

    #[test]
    fn literal_json_parses_to_the_review_threads() {
        let parsed = parse_pr_comments_arg(SAMPLE).expect("literal JSON parses");
        assert_eq!(parsed.threads.len(), 1);
        assert_eq!(parsed.threads[0].path, "models/orders.sql");
        assert_eq!(parsed.threads[0].line, Some(5));
    }

    #[test]
    fn at_file_reads_and_parses_the_payload() {
        // Std-only temp path (the codebase posture — no tempfile dev-dep).
        let pid = std::process::id();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path = std::env::temp_dir().join(format!("cute-dbt-prcomments-{pid}-{nonce}.json"));
        fs::write(&path, SAMPLE).expect("write fixture");
        let arg = format!("@{}", path.display());
        let parsed = parse_pr_comments_arg(&arg).expect("@file parses");
        assert_eq!(parsed.threads.len(), 1);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_missing_at_file_is_a_usage_error() {
        let err = parse_pr_comments_arg("@/no/such/path/comments.json")
            .expect_err("a missing @file is a usage error");
        assert!(err.contains("could not read --pr-comments file"), "{err}");
    }

    #[test]
    fn a_malformed_payload_degrades_to_empty_never_errors() {
        // The fail-soft contract: present-but-unusable JSON is "no comments",
        // not a clap usage error (PR comments are context, never a dependency).
        let parsed = parse_pr_comments_arg("not json at all").expect("malformed degrades");
        assert!(parsed.is_empty());
        let parsed_empty = parse_pr_comments_arg("").expect("empty degrades");
        assert!(parsed_empty.is_empty());
    }
}
