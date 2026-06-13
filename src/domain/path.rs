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
    remaining = strip_project_root(remaining, strip_prefix);

    // Step 3: collapse "//" runs into "/".
    collapse_slash_runs(remaining)
}

/// Strip the project-root `prefix` from `remaining` when it matches as a
/// whole path component — `prefix` itself (→ `""`) or `prefix/…` (→ the
/// tail). The match is **segment-aware**: a prefix that is only a leading
/// substring of the first segment (e.g. `dbt_project` vs
/// `dbt_project_notes/x.sql`) is NOT stripped (bot-review finding on
/// cute-dbt#86). `prefix` is `\`-canonicalized and trailing-`/`-trimmed
/// first so a Windows-shaped or slash-suffixed root still matches. Returns
/// `remaining` unchanged when there is no prefix, an empty prefix, or no
/// segment match.
fn strip_project_root<'a>(remaining: &'a str, strip_prefix: Option<&Path>) -> &'a str {
    let Some(prefix) = strip_prefix else {
        return remaining;
    };
    let prefix_lossy = prefix.to_string_lossy();
    let prefix_canonical = canonicalize_separators(&prefix_lossy);
    let prefix_str = prefix_canonical.trim_end_matches('/');
    if prefix_str.is_empty() {
        return remaining;
    }
    if remaining == prefix_str {
        return "";
    }
    // Segment-aware guard: a prefix match only counts as a real
    // path-component match when followed by `/`. If the next character is
    // anything else, the strip is skipped and `remaining` is unchanged.
    if let Some(rest) = remaining.strip_prefix(prefix_str)
        && let Some(after_slash) = rest.strip_prefix('/')
    {
        return after_slash;
    }
    remaining
}

/// Collapse runs of `/` into a single `/`. Borrows-then-clones only when
/// no run is present (the common case); allocates a compacted buffer
/// otherwise.
fn collapse_slash_runs(remaining: &str) -> String {
    if !remaining.contains("//") {
        return remaining.to_owned();
    }
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
    out
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
    fn normalize_path_keeps_single_slashes_while_collapsing_a_run() {
        // A path with BOTH a single separator and a `//` run: the single
        // `/` must survive, only the run collapses. Pins the collapse
        // loop's per-run dedup (`if !prev_slash`) — a path with a `//`
        // run but no lone separator (e.g. `models//x.sql`) cannot tell
        // the dedup apart from dropping every first-of-run slash.
        assert_eq!(normalize_path("a/b//c/d", None), "a/b/c/d");
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
