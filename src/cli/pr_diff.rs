//! The `--pr-diff` value source: a raw `git diff --unified=0` patch.
//!
//! cute-dbt's CI/PR-review path scopes the report to a PR's diff instead
//! of a baseline manifest. The workflow (or the Marketplace Action)
//! computes the diff — `git diff --unified=0 ${base.sha}...${head.sha} >
//! diff.patch` — and hands cute-dbt the file via `--pr-diff @diff.patch`.
//! cute-dbt parses the diff itself: the changed-file set comes from each
//! `+++ b/<path>` header, and the per-file hunks (with their `+`/`-`
//! bodies) drive both block-precise `updated` detection and the inline
//! YAML diff (cute-dbt#96).
//!
//! Renamed from `--scope-from-pr-diff` at cute-dbt#96. The old flag took
//! a *changed-file list*; the new one takes the *raw diff*, so cute-dbt
//! can see *which lines* changed, not just which files. cute-dbt still
//! never shells out to `git` — the workflow owns "how to get the diff";
//! cute-dbt owns "given the diff, render the report."
//!
//! `@file` is the canonical form (and the only one real CLI usage takes
//! — a multi-line diff is not a sane inline argument). A leading `@`
//! reads the diff from the file at the remaining path; otherwise the
//! value itself is parsed as raw diff text (used by the unit suite). A
//! bad `@file` (missing / non-UTF-8) or a value that is not a unified
//! diff is a clap usage error (exit 2), never a
//! [`crate::domain::PreflightError`] — the same precedent as `--config`
//! (PR 14) and `--baseline-manifest` (ADR-2).

use std::fs;
use std::path::Path;

use crate::domain::pr_diff::{FileHunks, Hunk, PrDiff};

/// clap value-parser for `--pr-diff`.
///
/// `@<path>` reads the diff from a file; any other value is parsed as raw
/// diff text. The result is a [`PrDiff`] of the changed files and their
/// new-side hunks.
///
/// # Errors
///
/// - An `@file` that cannot be read or is not valid UTF-8.
/// - A non-empty value that is not a recognizable unified diff (no diff
///   headers / hunks), or a hunk header that cannot be parsed.
///
/// An empty (or whitespace-only) diff is **not** an error — it parses to
/// a [`PrDiff`] with zero files (a zero-scope report).
pub fn parse_diff(s: &str) -> Result<PrDiff, String> {
    if let Some(file) = s.strip_prefix('@') {
        let contents = fs::read_to_string(Path::new(file))
            .map_err(|err| format!("could not read --pr-diff file at {file}: {err}"))?;
        return parse_unified_diff(&contents);
    }
    parse_unified_diff(s)
}

/// Error message for an input that is not a recognizable unified diff.
fn not_a_diff(detail: &str) -> String {
    format!("the --pr-diff value could not be parsed as a unified diff: {detail}")
}

/// Parse `git diff --unified=0` text into a [`PrDiff`].
///
/// A small line-oriented state machine — no diff-parsing dependency at
/// this layer (same spirit as the hand-rolled CSV parser, cute-dbt#66).
/// Lines are classified relative to a single `in_hunk` flag so a `+++`
/// **added body line** inside a hunk is never confused with a `+++ b/…`
/// **file header** (headers only appear when `in_hunk` is false, after a
/// `--- ` / `diff --git`).
fn parse_unified_diff(s: &str) -> Result<PrDiff, String> {
    let mut files: Vec<FileHunks> = Vec::new();
    let mut current: Option<FileHunks> = None;
    let mut in_hunk = false;
    let mut saw_structure = false;

    for line in s.lines() {
        // `diff --git` — start of a new file's header block.
        if line.starts_with("diff --git") {
            saw_structure = true;
            in_hunk = false;
            if let Some(f) = current.take() {
                files.push(f);
            }
            continue;
        }
        // Hunk header — `@@ -old[,c] +new[,c] @@ [section]`.
        if let Some(rest) = line.strip_prefix("@@") {
            saw_structure = true;
            let hunk = parse_hunk_header(rest)?;
            in_hunk = true;
            if let Some(f) = current.as_mut() {
                f.hunks.push(hunk);
            }
            continue;
        }
        if in_hunk {
            // Inside a hunk body: `+`/`-` are added/removed lines (sigil
            // stripped); `\ No newline at end of file` is ignored; any
            // other line ends the body.
            if line.starts_with('\\') {
                continue;
            }
            if let Some(body) = line.strip_prefix('+') {
                if let Some(h) = current.as_mut().and_then(|f| f.hunks.last_mut()) {
                    h.added_lines.push(body.to_owned());
                }
                continue;
            }
            if let Some(body) = line.strip_prefix('-') {
                if let Some(h) = current.as_mut().and_then(|f| f.hunks.last_mut()) {
                    h.removed_lines.push(body.to_owned());
                }
                continue;
            }
            // Not a body line — the hunk ended.
            in_hunk = false;
        }
        // Header block (in_hunk == false).
        if let Some(rest) = line.strip_prefix("--- ") {
            saw_structure = true;
            let _ = rest; // old-side path — ignored.
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            saw_structure = true;
            if let Some(f) = current.take() {
                files.push(f);
            }
            current = parse_plus_path(rest).map(|path| FileHunks {
                path,
                hunks: Vec::new(),
            });
        }
        // `index`, mode, `rename from/to`, `similarity`, blank — ignored.
    }
    if let Some(f) = current.take() {
        files.push(f);
    }

    if !s.trim().is_empty() && !saw_structure {
        return Err(not_a_diff("no diff headers or hunks found"));
    }
    Ok(PrDiff { files })
}

/// Parse a `+++ ` header's path: strip an optional `b/` prefix and a
/// trailing `\t<timestamp>` section. `/dev/null` (a deleted file's new
/// side) yields `None`.
fn parse_plus_path(rest: &str) -> Option<String> {
    let path = rest.split('\t').next().unwrap_or(rest).trim_end();
    if path == "/dev/null" {
        return None;
    }
    Some(path.strip_prefix("b/").unwrap_or(path).to_owned())
}

/// Parse the new-side range from a hunk header's text (everything after
/// the leading `@@`): `-A[,B] +C[,D] @@ …` → `(C, D)` with `D` defaulting
/// to `1` when the count is omitted (a single-line hunk).
fn parse_hunk_header(rest: &str) -> Result<Hunk, String> {
    let body = rest.trim_start();
    let mut tokens = body.split_whitespace();
    let old = tokens
        .next()
        .ok_or_else(|| not_a_diff("hunk header missing the old-side range"))?;
    let new = tokens
        .next()
        .ok_or_else(|| not_a_diff("hunk header missing the new-side range"))?;
    if !old.starts_with('-') || !new.starts_with('+') {
        return Err(not_a_diff(&format!("malformed hunk header: @@{rest}")));
    }
    let (new_start, new_len) = parse_range(&new[1..])?;
    Ok(Hunk {
        new_start,
        new_len,
        removed_lines: Vec::new(),
        added_lines: Vec::new(),
    })
}

/// Parse a unified-diff range `C` or `C,D` into `(start, len)`. A bare
/// `C` means a single line (`len == 1`).
fn parse_range(s: &str) -> Result<(usize, usize), String> {
    let mut parts = s.splitn(2, ',');
    let start = parts
        .next()
        .unwrap_or("")
        .parse::<usize>()
        .map_err(|_| not_a_diff(&format!("bad hunk range start: {s:?}")))?;
    let len = match parts.next() {
        Some(d) => d
            .parse::<usize>()
            .map_err(|_| not_a_diff(&format!("bad hunk range length: {s:?}")))?,
        None => 1,
    };
    Ok((start, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_path(stem: &str) -> std::path::PathBuf {
        let nonce = COUNTER.fetch_add(1, Ordering::SeqCst);
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_micros());
        let pid = std::process::id();
        std::env::temp_dir().join(format!(
            "cute-dbt-prdiff-{pid}-{micros}-{nonce}-{stem}.patch"
        ))
    }

    fn write_fixture(stem: &str, content: &str) -> std::path::PathBuf {
        let path = unique_temp_path(stem);
        let mut f = std::fs::File::create(&path).expect("create temp fixture");
        f.write_all(content.as_bytes()).expect("write temp fixture");
        path
    }

    // ----- A realistic git diff --unified=0 (content, not just ranges) -----

    const REAL_DIFF: &str = "diff --git a/models/marts/core/_core__models.yml b/models/marts/core/_core__models.yml\n\
index 1111111..2222222 100644\n\
--- a/models/marts/core/_core__models.yml\n\
+++ b/models/marts/core/_core__models.yml\n\
@@ -5,1 +5,2 @@ unit_tests:\n\
-      rows: []\n\
+      rows:\n\
+        - {id: 1}\n\
diff --git a/models/marts/core/dim_payers.sql b/models/marts/core/dim_payers.sql\n\
index 3333333..4444444 100644\n\
--- a/models/marts/core/dim_payers.sql\n\
+++ b/models/marts/core/dim_payers.sql\n\
@@ -12,0 +13,1 @@\n\
+select 1 as added\n";

    #[test]
    fn parses_files_paths_ranges_and_bodies() {
        let diff = parse_diff(REAL_DIFF).expect("a real --unified=0 diff parses");
        assert_eq!(diff.files.len(), 2);

        let yaml = &diff.files[0];
        assert_eq!(yaml.path, "models/marts/core/_core__models.yml");
        assert_eq!(yaml.hunks.len(), 1);
        let h = &yaml.hunks[0];
        assert_eq!((h.new_start, h.new_len), (5, 2));
        // Content — not just ranges (advisor: Step 2's N7b + the
        // provenance rule key off added_lines being extracted right).
        assert_eq!(h.removed_lines, vec!["      rows: []"]);
        assert_eq!(h.added_lines, vec!["      rows:", "        - {id: 1}"]);

        let sql = &diff.files[1];
        assert_eq!(sql.path, "models/marts/core/dim_payers.sql");
        assert_eq!(sql.hunks[0].added_lines, vec!["select 1 as added"]);
    }

    // ----- Pure deletion: new_len == 0, point-touch -----

    #[test]
    fn pure_deletion_hunk_has_zero_new_len_and_removed_bodies() {
        let diff = parse_diff(
            "--- a/_ut.yml\n+++ b/_ut.yml\n@@ -5,3 +5,0 @@\n-line a\n-line b\n-line c\n",
        )
        .expect("pure-deletion diff parses");
        let h = &diff.files[0].hunks[0];
        assert_eq!((h.new_start, h.new_len), (5, 0));
        assert_eq!(h.removed_lines, vec!["line a", "line b", "line c"]);
        assert!(h.added_lines.is_empty());
    }

    // ----- `\ No newline at end of file` is ignored, not counted -----

    #[test]
    fn no_newline_marker_is_ignored() {
        let diff = parse_diff(
            "--- a/x.yml\n+++ b/x.yml\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n",
        )
        .expect("a diff with no-newline markers parses");
        let h = &diff.files[0].hunks[0];
        assert_eq!(h.removed_lines, vec!["old"]);
        assert_eq!(h.added_lines, vec!["new"]);
    }

    // ----- CRLF line endings: bodies carry no trailing \r -----

    #[test]
    fn crlf_line_endings_are_handled() {
        let diff = parse_diff("--- a/x.yml\r\n+++ b/x.yml\r\n@@ -1 +1 @@\r\n-old\r\n+new\r\n")
            .expect("a CRLF diff parses");
        assert_eq!(diff.files[0].path, "x.yml");
        let h = &diff.files[0].hunks[0];
        assert_eq!(h.removed_lines, vec!["old"]);
        assert_eq!(h.added_lines, vec!["new"], "no trailing \\r in bodies");
    }

    // ----- A rename WITH content changes parses as the new path -----

    #[test]
    fn rename_with_content_change_parses_as_new_path() {
        let diff = parse_diff(
            "diff --git a/old_name.yml b/new_name.yml\nsimilarity index 80%\nrename from old_name.yml\nrename to new_name.yml\n--- a/old_name.yml\n+++ b/new_name.yml\n@@ -1 +1 @@\n-a\n+b\n",
        )
        .expect("a rename-with-change diff parses");
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "new_name.yml");
        assert_eq!(diff.files[0].hunks[0].added_lines, vec!["b"]);
    }

    // ----- A pure rename (no hunks) names no changed-content file -----

    #[test]
    fn pure_rename_with_no_hunks_emits_no_file_entry() {
        let diff = parse_diff(
            "diff --git a/old.yml b/new.yml\nsimilarity index 100%\nrename from old.yml\nrename to new.yml\n",
        )
        .expect("a pure-rename diff parses");
        // No `+++ b/` header (no content change) → no file entry. dbt
        // rename handling is a documented fidelity limit (cute-dbt#80).
        assert!(diff.files.is_empty());
    }

    // ----- Multiple hunks in one file -----

    #[test]
    fn multiple_hunks_per_file_are_all_captured() {
        let diff = parse_diff(
            "--- a/_ut.yml\n+++ b/_ut.yml\n@@ -3 +3 @@\n-a\n+b\n@@ -10,0 +11,1 @@\n+c\n",
        )
        .expect("a multi-hunk diff parses");
        let hunks = &diff.files[0].hunks;
        assert_eq!(hunks.len(), 2);
        assert_eq!((hunks[0].new_start, hunks[0].new_len), (3, 1));
        assert_eq!((hunks[1].new_start, hunks[1].new_len), (11, 1));
        assert_eq!(hunks[1].added_lines, vec!["c"]);
    }

    // ----- Bare range (no count) defaults to length 1 -----

    #[test]
    fn bare_range_without_count_is_length_one() {
        let diff =
            parse_diff("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n").expect("bare range parses");
        let h = &diff.files[0].hunks[0];
        assert_eq!((h.new_start, h.new_len), (1, 1));
    }

    // ----- A `+++` ADDED BODY line is not confused with a file header -----

    #[test]
    fn plus_plus_plus_added_body_is_not_a_file_header() {
        // An added line whose content begins with `++ ` makes the diff
        // line `+++ …`. Inside a hunk it must be an added body line, not
        // a new-file header (the `in_hunk` disambiguation).
        let diff =
            parse_diff("--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n-old\n+normal\n+++ plus prefixed\n")
                .expect("parses");
        assert_eq!(diff.files.len(), 1, "no spurious second file");
        assert_eq!(
            diff.files[0].hunks[0].added_lines,
            vec!["normal", "++ plus prefixed"]
        );
    }

    // ----- New file (--- /dev/null) -----

    #[test]
    fn new_file_against_dev_null_parses_with_its_added_lines() {
        let diff = parse_diff(
            "diff --git a/n.yml b/n.yml\nnew file mode 100644\n--- /dev/null\n+++ b/n.yml\n@@ -0,0 +1,2 @@\n+line 1\n+line 2\n",
        )
        .expect("a new-file diff parses");
        assert_eq!(diff.files[0].path, "n.yml");
        assert_eq!(diff.files[0].hunks[0].added_lines.len(), 2);
    }

    // ----- Deleted file (+++ /dev/null) yields no new-side entry -----

    #[test]
    fn deleted_file_against_dev_null_emits_no_entry() {
        let diff = parse_diff(
            "diff --git a/d.yml b/d.yml\ndeleted file mode 100644\n--- a/d.yml\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-line 1\n-line 2\n",
        )
        .expect("a deleted-file diff parses");
        assert!(
            diff.files.is_empty(),
            "a file deleted on the new side names no changed-content path"
        );
    }

    // ----- Empty diff is valid (zero files, zero scope) -----

    #[test]
    fn empty_diff_is_a_valid_zero_file_diff() {
        assert!(parse_diff("").expect("empty parses").files.is_empty());
        assert!(
            parse_diff("   \n\n")
                .expect("whitespace parses")
                .files
                .is_empty(),
            "whitespace-only is an empty diff, not malformed"
        );
    }

    // ----- Malformed input is an error -----

    #[test]
    fn non_diff_text_is_an_error() {
        let err = parse_diff("this is not a diff at all\njust some prose\n")
            .expect_err("non-diff prose is malformed");
        assert!(
            err.contains("could not be parsed as a unified diff"),
            "error explains the parse failure: {err}"
        );
    }

    #[test]
    fn malformed_hunk_header_is_an_error() {
        let err = parse_diff("--- a/x\n+++ b/x\n@@ total garbage @@\n+a\n")
            .expect_err("a bad hunk header is malformed");
        assert!(
            err.contains("could not be parsed as a unified diff"),
            "error explains the parse failure: {err}"
        );
    }

    // ----- @file form -----

    #[test]
    fn at_file_reads_and_parses_the_diff() {
        let path = write_fixture("realdiff", REAL_DIFF);
        let arg = format!("@{}", path.display());
        let diff = parse_diff(&arg).expect("@file reads + parses");
        assert_eq!(diff.files.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn at_missing_file_is_an_error() {
        let path = unique_temp_path("does-not-exist");
        let arg = format!("@{}", path.display());
        let err = parse_diff(&arg).expect_err("@missing file is an error");
        assert!(
            err.contains("could not read"),
            "error explains the read failure: {err}"
        );
    }

    #[test]
    fn at_empty_file_is_a_valid_zero_file_diff() {
        let path = write_fixture("empty", "");
        let arg = format!("@{}", path.display());
        let diff = parse_diff(&arg).expect("@empty file is Ok, not an error");
        assert!(diff.files.is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
