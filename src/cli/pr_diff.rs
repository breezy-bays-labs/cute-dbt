//! The `--pr-diff` value source: a raw `git diff --unified=0` patch.
//!
//! cute-dbt's CI/PR-review path scopes the report to a PR's diff instead
//! of a baseline manifest. The workflow (or the Marketplace Action)
//! computes the diff — `git diff --unified=0 ${base.sha}...${head.sha} >
//! diff.patch` — and hands cute-dbt the file via `--pr-diff @diff.patch`.
//! cute-dbt parses the diff itself: the changed-file set comes from each
//! `+++ b/<path>` header, and the per-file hunks (with their `+`/`-`
//! bodies) drive both block-precise `updated` detection and the inline
//! YAML diff (cute-dbt#96). Git-detected renames — the `rename from` /
//! `rename to` extended-header pairs `git diff` emits by default since
//! git 2.9 — are collected too (cute-dbt#80), so a **pure** rename
//! (100% similarity, no `+++` header, no hunks) still names both of its
//! paths to scope selection.
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

use crate::domain::pr_diff::{FileHunks, Hunk, PrDiff, RenamePair};

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
///
/// Rename detection (cute-dbt#80): the `rename from <old>` /
/// `rename to <new>` extended-header pair (emitted by `git diff`'s
/// default rename detection, on since git 2.9) is collected into
/// [`PrDiff::renames`]. A **pure** rename (100% similarity) emits *only*
/// that header block — no `---`/`+++`, no hunks — so without this the
/// renamed file would vanish from the diff entirely. The paths are taken
/// verbatim (git emits them un-prefixed and unquoted-for-spaces;
/// C-quoted non-ASCII paths are not dequoted — parity with the
/// `+++ b/<path>` parser).
fn parse_unified_diff(s: &str) -> Result<PrDiff, String> {
    let mut files: Vec<FileHunks> = Vec::new();
    let mut renames: Vec<RenamePair> = Vec::new();
    let mut pending_rename_from: Option<String> = None;
    let mut current: Option<FileHunks> = None;
    let mut in_hunk = false;
    let mut saw_structure = false;

    for line in s.lines() {
        // `diff --git` — start of a new file's header block.
        if line.starts_with("diff --git") {
            saw_structure = true;
            in_hunk = false;
            // A dangling `rename from` never happens in real git output
            // (the pair is adjacent); dropping it here keeps a malformed
            // header from leaking across files.
            pending_rename_from = None;
            flush(&mut current, &mut files);
            continue;
        }
        // Hunk header — `@@ -old[,c] +new[,c] @@ [section]`.
        if let Some(rest) = line.strip_prefix("@@") {
            saw_structure = true;
            open_hunk(rest, current.as_mut(), &mut in_hunk)?;
            continue;
        }
        // Inside a hunk body, `consume_body_line` appends `+`/`-` bodies (and
        // ignores `\ No newline…`); a non-body line ends the hunk and falls
        // through to the header checks below.
        if in_hunk {
            if consume_body_line(line, current.as_mut()) {
                continue;
            }
            in_hunk = false;
        }
        // Header block (in_hunk == false). `--- `/`+++ ` are the path
        // headers; `rename from`/`rename to` pair into a RenamePair
        // (cute-dbt#80). `index`, mode, `similarity …`, blank → ignored.
        if consume_path_header(line, &mut current, &mut files) {
            saw_structure = true;
            continue;
        }
        if consume_rename_header(line, &mut pending_rename_from, &mut renames) {
            saw_structure = true;
        }
    }
    flush(&mut current, &mut files);

    if !s.trim().is_empty() && !saw_structure {
        return Err(not_a_diff("no diff headers or hunks found"));
    }
    Ok(PrDiff { files, renames })
}

/// Parse a `@@ … @@` header (everything after the `@@`) and open the hunk
/// on the current file, flagging `in_hunk` so subsequent `+`/`-` lines
/// attach to it. A malformed header is a usage error (propagated).
fn open_hunk(
    rest: &str,
    current: Option<&mut FileHunks>,
    in_hunk: &mut bool,
) -> Result<(), String> {
    let hunk = parse_hunk_header(rest)?;
    *in_hunk = true;
    if let Some(f) = current {
        f.hunks.push(hunk);
    }
    Ok(())
}

/// Classify one header-territory path-header line (`in_hunk == false`).
/// A `--- ` (old-side path) is consumed as a no-op; a `+++ ` starts a new
/// file via [`start_file`]. Returns `true` when the line was one of the
/// two (the caller marks `saw_structure` and moves on); `false` for any
/// other header line.
fn consume_path_header(
    line: &str,
    current: &mut Option<FileHunks>,
    files: &mut Vec<FileHunks>,
) -> bool {
    if line.starts_with("--- ") {
        return true;
    }
    if let Some(rest) = line.strip_prefix("+++ ") {
        start_file(rest, current, files);
        return true;
    }
    false
}

/// Classify one header-territory rename-header line (cute-dbt#80,
/// `in_hunk == false`). `rename from <old>` stages the old path;
/// `rename to <new>` pairs it into a [`RenamePair`]. A stray `rename to`
/// with no pending `rename from` (never emitted by git) is consumed but
/// ignored — the same lenience as other unrecognized header lines.
/// Returns `true` when the line was one of the two (the caller marks
/// `saw_structure`); `false` otherwise.
fn consume_rename_header(
    line: &str,
    pending_from: &mut Option<String>,
    renames: &mut Vec<RenamePair>,
) -> bool {
    if let Some(rest) = line.strip_prefix("rename from ") {
        *pending_from = Some(rest.trim_end().to_owned());
        return true;
    }
    if let Some(rest) = line.strip_prefix("rename to ") {
        if let Some(from) = pending_from.take() {
            renames.push(RenamePair {
                from,
                to: rest.trim_end().to_owned(),
            });
        }
        return true;
    }
    false
}

/// Flush the in-progress file and begin a new one from a `+++ ` header
/// (`/dev/null` — a deleted file's new side — yields no new file).
fn start_file(rest: &str, current: &mut Option<FileHunks>, files: &mut Vec<FileHunks>) {
    flush(current, files);
    *current = parse_plus_path(rest).map(|path| FileHunks {
        path,
        hunks: Vec::new(),
    });
}

/// Move `current` (if any) onto `files`, leaving `current` empty.
fn flush(current: &mut Option<FileHunks>, files: &mut Vec<FileHunks>) {
    if let Some(f) = current.take() {
        files.push(f);
    }
}

/// Classify one line while inside a hunk body. Appends a `+`/`-` body
/// (sigil stripped) to the current hunk and returns `true`; a
/// `\ No newline at end of file` marker, a ` `-prefixed context line, or a
/// blank line is a consumed no-op (`true`). Returns `false` only when the
/// line is not body-shaped — the hunk has ended and the caller
/// re-classifies it as a header.
///
/// Context/blank lines never occur in the documented `git diff --unified=0`
/// input, but consuming them (rather than ending the hunk) keeps the parser
/// robust if a non-zero-context diff is supplied: the interleaved context
/// lines are skipped and the hunk's `+`/`-` bodies still accumulate
/// correctly (cute-dbt#110 review).
fn consume_body_line(line: &str, current: Option<&mut FileHunks>) -> bool {
    if line.starts_with('\\') || line.starts_with(' ') || line.is_empty() {
        return true;
    }
    let Some(hunk) = current.and_then(|f| f.hunks.last_mut()) else {
        // No open hunk to attach to (defensive): a `+`/`-` is still body-
        // shaped and consumed; anything else ends the body.
        return line.starts_with('+') || line.starts_with('-');
    };
    if let Some(body) = line.strip_prefix('+') {
        hunk.added_lines.push(body.to_owned());
        return true;
    }
    if let Some(body) = line.strip_prefix('-') {
        hunk.removed_lines.push(body.to_owned());
        return true;
    }
    false
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
    // ----- AND carries the rename pair (cute-dbt#80) -----

    #[test]
    fn rename_with_content_change_parses_as_new_path_and_pair() {
        let diff = parse_diff(
            "diff --git a/old_name.yml b/new_name.yml\nsimilarity index 80%\nrename from old_name.yml\nrename to new_name.yml\n--- a/old_name.yml\n+++ b/new_name.yml\n@@ -1 +1 @@\n-a\n+b\n",
        )
        .expect("a rename-with-change diff parses");
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "new_name.yml");
        assert_eq!(diff.files[0].hunks[0].added_lines, vec!["b"]);
        assert_eq!(
            diff.renames,
            vec![RenamePair {
                from: "old_name.yml".to_owned(),
                to: "new_name.yml".to_owned(),
            }],
            "the rename pair is collected alongside the file entry",
        );
    }

    // ----- A pure rename (no hunks) emits ONLY the rename pair -----

    #[test]
    fn pure_rename_with_no_hunks_emits_a_rename_pair_and_no_file_entry() {
        let diff = parse_diff(
            "diff --git a/old.yml b/new.yml\nsimilarity index 100%\nrename from old.yml\nrename to new.yml\n",
        )
        .expect("a pure-rename diff parses");
        // No `+++ b/` header (no content change) → no file entry; the
        // rename pair carries both paths instead (cute-dbt#80 — was a
        // documented fidelity limit before rename detection landed).
        assert!(diff.files.is_empty());
        assert_eq!(
            diff.renames,
            vec![RenamePair {
                from: "old.yml".to_owned(),
                to: "new.yml".to_owned(),
            }],
        );
    }

    // ----- Rename paths are verbatim (spaces stay; verified vs git) -----

    #[test]
    fn rename_paths_with_spaces_are_taken_verbatim() {
        // Real `git diff` output (verified against git 2.51): `rename
        // from`/`rename to` paths are NOT quoted for spaces and carry no
        // `a/`/`b/` prefix.
        let diff = parse_diff(
            "diff --git a/models/dim c.sql b/models/dim d.sql\nsimilarity index 100%\nrename from models/dim c.sql\nrename to models/dim d.sql\n",
        )
        .expect("a spaced-path pure-rename diff parses");
        assert_eq!(
            diff.renames,
            vec![RenamePair {
                from: "models/dim c.sql".to_owned(),
                to: "models/dim d.sql".to_owned(),
            }],
        );
    }

    // ----- CRLF rename headers carry no trailing \r -----

    #[test]
    fn crlf_rename_headers_are_trimmed() {
        let diff = parse_diff(
            "diff --git a/old.yml b/new.yml\r\nsimilarity index 100%\r\nrename from old.yml\r\nrename to new.yml\r\n",
        )
        .expect("a CRLF pure-rename diff parses");
        assert_eq!(
            diff.renames,
            vec![RenamePair {
                from: "old.yml".to_owned(),
                to: "new.yml".to_owned(),
            }],
            "no trailing \\r in rename paths",
        );
    }

    // ----- Multiple renames in one diff are all collected -----

    #[test]
    fn multiple_renames_are_all_collected() {
        let diff = parse_diff(
            "diff --git a/a.sql b/b.sql\nsimilarity index 100%\nrename from a.sql\nrename to b.sql\ndiff --git a/c.yml b/d.yml\nsimilarity index 90%\nrename from c.yml\nrename to d.yml\n--- a/c.yml\n+++ b/d.yml\n@@ -1 +1 @@\n-x\n+y\n",
        )
        .expect("a two-rename diff parses");
        assert_eq!(diff.renames.len(), 2);
        assert_eq!(diff.renames[0].from, "a.sql");
        assert_eq!(diff.renames[0].to, "b.sql");
        assert_eq!(diff.renames[1].from, "c.yml");
        assert_eq!(diff.renames[1].to, "d.yml");
        // The rename-with-edit also has its file entry; the pure one doesn't.
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "d.yml");
    }

    // ----- An added body line that LOOKS like a rename header is body -----

    #[test]
    fn added_body_line_resembling_rename_header_is_not_a_rename() {
        // Inside a hunk, `+rename from sneaky` is an added body line whose
        // content is `rename from sneaky` — never a rename header (the
        // in_hunk disambiguation, same as the `+++` body case).
        let diff = parse_diff(
            "--- a/x.md\n+++ b/x.md\n@@ -1 +1,2 @@\n-old\n+rename from sneaky\n+rename to sneakier\n",
        )
        .expect("parses");
        assert!(diff.renames.is_empty(), "no spurious rename pair");
        assert_eq!(
            diff.files[0].hunks[0].added_lines,
            vec!["rename from sneaky", "rename to sneakier"],
        );
    }

    // ----- Defensive: unpaired rename header lines are ignored -----

    #[test]
    fn stray_rename_to_without_rename_from_is_ignored() {
        let diff = parse_diff(
            "diff --git a/x.yml b/x.yml\nrename to x.yml\n--- a/x.yml\n+++ b/x.yml\n@@ -1 +1 @@\n-a\n+b\n",
        )
        .expect("parses");
        assert!(diff.renames.is_empty());
    }

    #[test]
    fn dangling_rename_from_does_not_leak_across_files() {
        // A `rename from` never followed by `rename to` (not real git
        // output) is dropped at the next `diff --git`, so a later file's
        // `rename to` cannot pair with it.
        let diff = parse_diff(
            "diff --git a/a.yml b/a.yml\nrename from a.yml\ndiff --git a/b.yml b/c.yml\nrename to c.yml\n--- a/b.yml\n+++ b/c.yml\n@@ -1 +1 @@\n-a\n+b\n",
        )
        .expect("parses");
        assert!(diff.renames.is_empty(), "no cross-file rename pairing");
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

    #[test]
    fn context_lines_do_not_prematurely_end_a_hunk() {
        // Off-contract (a non-`--unified=0` diff): a ` `-context line between
        // the `-` and `+` bodies must NOT end the hunk — the `+` after it is
        // still captured (CodeRabbit #110). Without the fix `added_lines`
        // would be empty.
        // `concat!` (not a `\`-continued literal) so the ` context` line
        // keeps its leading space — continuation would eat it.
        let diff = concat!(
            "diff --git a/m.sql b/m.sql\n",
            "--- a/m.sql\n",
            "+++ b/m.sql\n",
            "@@ -1,3 +1,3 @@\n",
            "-old\n",
            " context\n",
            "+new\n",
        );
        let pr = parse_diff(diff).expect("parses");
        assert_eq!(pr.files.len(), 1);
        let h = &pr.files[0].hunks[0];
        assert_eq!(h.removed_lines, vec!["old".to_owned()]);
        assert_eq!(
            h.added_lines,
            vec!["new".to_owned()],
            "the `+new` after a context line is still captured",
        );
    }
}
