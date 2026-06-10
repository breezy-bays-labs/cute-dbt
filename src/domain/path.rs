//! Path normalization + matching — the shared leaf for scope selection
//! and PR-diff overlap (cute-dbt#96).
//!
//! Extracted from `domain::scope` (cute-dbt#81) so both `scope` and the
//! `pr_diff` overlap engine can depend on it without a `scope ⇄ pr_diff`
//! cycle. The module DAG is `scope → pr_diff → path`: `pr_diff`'s
//! [`crate::domain::pr_diff::NormalizedDiffIndex`] is the single
//! normalization authority and consults [`normalize_path`] for both the
//! diff-side keyset (with the project-root strip) and the declaring-side
//! lookup (with `None`). `tests/domain_clean_arch.rs` greps for outward
//! `use crate::adapters` imports only — it cannot see an intra-domain
//! cycle, so the leaf direction is a structural decision recorded here
//! and in the closeout decision record.
//!
//! Path normalization: Windows-style `\` separators canonicalize to `/`
//! (cute-dbt#183); leading `./` is stripped; an optional `project_root`
//! prefix is stripped from changed paths (a dbt sub-tree workflow lives
//! under `<repo-root>/dbt_project/`, the manifest records `models/...`
//! relative to `dbt_project/`); separator runs collapse.
//!
//! The `\` canonicalization closes a one-sided gap (cute-dbt#183,
//! verified against source 2026-06-10): a dbt manifest compiled **on
//! Windows** serializes `original_file_path` from a native `PathBuf`
//! verbatim — `\`-separated — with no slash normalization (dbt-fusion
//! `crates/dbt-schemas/src/schemas/manifest/manifest_nodes.rs` +
//! `crates/dbt-schemas/src/schemas/nodes.rs` `CommonAttributes`, both at
//! commit `9977b6cbb1b761065536300037560d8e3c037011`; fusion's own
//! `dbt_common::path::path_separator_eq` treats `/` and `\` as
//! equivalent for exactly this reason). `git diff`, by contrast, emits
//! `/`-separated repo paths on **every** platform (index/tree entry path
//! names are stored with `/` — `git/git`
//! `Documentation/gitformat-index.adoc` at `1ff279f34`). Without
//! canonicalization a Windows-compiled manifest never matched the diff
//! keyset and scoping silently missed. Treating `\` as a separator
//! mirrors fusion's equivalence semantics; a Unix filename containing a
//! *literal* `\` is therefore misread, accepted deliberately — fusion's
//! own path equality already conflates the two.

use std::borrow::Cow;
use std::path::Path;

/// Canonicalize Windows-style `\` separators to `/` (cute-dbt#183),
/// borrowing when the input carries none.
fn canonicalize_separators(s: &str) -> Cow<'_, str> {
    if s.contains('\\') {
        Cow::Owned(s.replace('\\', "/"))
    } else {
        Cow::Borrowed(s)
    }
}

/// Normalize a file path for matching:
/// - Canonicalize Windows-style `\` separators to `/` (cute-dbt#183 —
///   a Windows-compiled manifest emits `\`-separated
///   `original_file_path`; see the module docs for the source-pinned
///   evidence). Applied to the path **and** to `strip_prefix`, before
///   every other step, so a fully Windows-shaped input still strips and
///   matches.
/// - Strip leading `./`.
/// - Strip `strip_prefix` (with optional trailing slash) if the path
///   starts with it.
/// - Collapse runs of `/` into a single `/`.
///
/// Returns the normalized path as a `String` (cheap — most fixtures are
/// short).
#[must_use]
pub fn normalize_path(p: &str, strip_prefix: Option<&Path>) -> String {
    // Step 0: canonicalize `\` → `/` so every later step sees one
    // separator vocabulary (`.\x` strips, `dbt_project\x` matches the
    // prefix, `\\` runs collapse).
    let canonical = canonicalize_separators(p);
    let mut remaining: &str = &canonical;

    // Step 1: strip leading "./".
    while let Some(rest) = remaining.strip_prefix("./") {
        remaining = rest;
    }

    // Step 2: strip the configured project-root prefix, if present.
    // Match must be segment-aware (`prefix` or `prefix/…`, never
    // mid-segment) so `dbt_project_notes/x.sql` is NOT stripped when the
    // prefix is `dbt_project` — bot-review finding on cute-dbt#86.
    if let Some(prefix) = strip_prefix {
        let prefix_lossy = prefix.to_string_lossy();
        let prefix_canonical = canonicalize_separators(&prefix_lossy);
        let prefix_str = prefix_canonical.trim_end_matches('/');
        if !prefix_str.is_empty() {
            if remaining == prefix_str {
                remaining = "";
            } else if let Some(rest) = remaining.strip_prefix(prefix_str) {
                if let Some(after_slash) = rest.strip_prefix('/') {
                    remaining = after_slash;
                }
                // else: prefix matches at position 0 but is followed by
                // a non-`/` character (e.g. `dbt_project_notes/...`) —
                // not a real path-component match, leave `remaining`
                // unchanged.
            }
        }
    }

    // Step 3: collapse "//" runs into "/".
    if remaining.contains("//") {
        let mut out = String::with_capacity(remaining.len());
        let mut prev_slash = false;
        for ch in remaining.chars() {
            if ch == '/' {
                if !prev_slash {
                    out.push('/');
                }
                prev_slash = true;
            } else {
                out.push(ch);
                prev_slash = false;
            }
        }
        return out;
    }

    remaining.to_owned()
}

/// `true` when `manifest_path` (after normalization) equals any of
/// `changed_paths` (after the same normalization with `project_root_strip`
/// applied). The manifest path is project-root-relative; the changed
/// paths are repo-root-relative — `project_root_strip` bridges the gap.
///
/// Designed for callers that need the boolean without first materializing
/// the normalized change set. For bulk lookups, prefer building a
/// [`crate::domain::pr_diff::NormalizedDiffIndex`] once and consulting it
/// directly (the v0.1.x PR-diff path does exactly this).
#[must_use]
pub fn match_changed_path(
    manifest_path: &str,
    changed_paths: &[String],
    project_root_strip: Option<&Path>,
) -> bool {
    let manifest_norm = normalize_path(manifest_path, None);
    changed_paths
        .iter()
        .any(|changed| normalize_path(changed, project_root_strip) == manifest_norm)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- normalize_path -----

    #[test]
    fn normalize_path_strips_leading_dot_slash() {
        assert_eq!(normalize_path("./models/x.sql", None), "models/x.sql");
    }

    #[test]
    fn normalize_path_strips_repeated_leading_dot_slash() {
        assert_eq!(normalize_path("././models/x.sql", None), "models/x.sql");
    }

    #[test]
    fn normalize_path_strips_project_root_prefix() {
        assert_eq!(
            normalize_path("dbt_project/models/x.sql", Some(Path::new("dbt_project"))),
            "models/x.sql"
        );
    }

    #[test]
    fn normalize_path_strips_project_root_prefix_with_trailing_slash() {
        assert_eq!(
            normalize_path("dbt_project/models/x.sql", Some(Path::new("dbt_project/"))),
            "models/x.sql"
        );
    }

    #[test]
    fn normalize_path_collapses_double_slash() {
        assert_eq!(normalize_path("models//x.sql", None), "models/x.sql");
    }

    #[test]
    fn normalize_path_leaves_unrelated_paths_unchanged() {
        assert_eq!(normalize_path("README.md", None), "README.md");
    }

    #[test]
    fn normalize_path_does_not_strip_prefix_when_not_present() {
        assert_eq!(
            normalize_path("models/x.sql", Some(Path::new("dbt_project"))),
            "models/x.sql"
        );
    }

    #[test]
    fn normalize_path_does_not_strip_mid_segment_prefix_match() {
        // `dbt_project_notes/...` must NOT be stripped when the prefix is
        // `dbt_project` (segment-aware match — bot-review on cute-dbt#86).
        assert_eq!(
            normalize_path("dbt_project_notes/x.sql", Some(Path::new("dbt_project"))),
            "dbt_project_notes/x.sql"
        );
    }

    // ----- Windows-style `\` separators (cute-dbt#183) -----
    // Representative unit cases; the exhaustive mutation-kill parity
    // suite lives in `tests/path_matching.rs`.

    #[test]
    fn normalize_path_canonicalizes_backslash_separators() {
        assert_eq!(normalize_path("models\\x.sql", None), "models/x.sql");
    }

    #[test]
    fn normalize_path_strips_project_root_prefix_from_backslash_path() {
        assert_eq!(
            normalize_path("dbt_project\\models\\x.sql", Some(Path::new("dbt_project"))),
            "models/x.sql"
        );
    }

    #[test]
    fn normalize_path_backslash_segment_guard_still_holds() {
        // The segment-aware prefix guard survives canonicalization.
        assert_eq!(
            normalize_path("dbt_project_notes\\x.sql", Some(Path::new("dbt_project"))),
            "dbt_project_notes/x.sql"
        );
    }

    // ----- match_changed_path -----

    #[test]
    fn match_changed_path_finds_exact_match() {
        let changed = vec!["models/x.sql".to_owned()];
        assert!(match_changed_path("models/x.sql", &changed, None));
    }

    #[test]
    fn match_changed_path_finds_match_after_leading_dot_slash_strip() {
        let changed = vec!["./models/x.sql".to_owned()];
        assert!(match_changed_path("models/x.sql", &changed, None));
    }

    #[test]
    fn match_changed_path_finds_match_after_project_root_strip() {
        let changed = vec!["dbt_project/models/x.sql".to_owned()];
        assert!(match_changed_path(
            "models/x.sql",
            &changed,
            Some(Path::new("dbt_project"))
        ));
    }

    #[test]
    fn match_changed_path_no_match_for_unrelated_path() {
        let changed = vec!["README.md".to_owned()];
        assert!(!match_changed_path("models/x.sql", &changed, None));
    }

    #[test]
    fn match_changed_path_bridges_backslash_manifest_to_slash_diff() {
        // The cute-dbt#183 gap: a Windows-compiled manifest's
        // original_file_path is `\`-separated; the git diff is `/`.
        let changed = vec!["models/x.sql".to_owned()];
        assert!(match_changed_path("models\\x.sql", &changed, None));
    }
}
