//! The `--scope-from-pr-diff` value source.
//!
//! cute-dbt's CI/PR-review path scopes the report to a PR's changed
//! files instead of a baseline manifest. The workflow (or the Marketplace
//! Action) computes the changed-file list — `git diff --name-only
//! ${base.sha}...${head.sha}` — and hands it to cute-dbt in one of two
//! forms (resolved here at clap parse time):
//!
//! - **Literal list** — `--scope-from-pr-diff models/a.sql,models/b.yml`
//!   (comma- *or* newline-separated; each entry trimmed; empties dropped).
//! - **File reference** — `--scope-from-pr-diff @changed.txt` (read the
//!   file, one path per line; each line trimmed; empties dropped).
//!
//! Deliberately NOT supported (per the plan's Decision 2): env-var
//! reading (`GITHUB_EVENT_PATH`), `git` shell-out, or `gh` CLI calls. The
//! workflow owns "how to get the file list"; cute-dbt owns "given facts,
//! render the report." A bad `@file` (missing / non-UTF-8) is a clap
//! usage error (exit 2), never a [`crate::domain::PreflightError`] — the
//! same precedent as `--config` (PR 14) and `--baseline-manifest` (ADR-2).

use std::fs;
use std::io;
use std::path::Path;

/// The changed-file paths a PR diff surfaced, as handed to
/// `--scope-from-pr-diff`.
///
/// A plain owned list (POD). Path normalization + manifest-node matching
/// happen later in the domain layer
/// ([`crate::domain::select_in_scope`]); this type only carries the raw,
/// trimmed, non-empty path strings.
#[derive(Debug, Clone)]
pub struct ChangedFiles {
    /// One repo-relative path per changed file (trimmed, never empty).
    pub paths: Vec<String>,
}

/// clap value-parser for `--scope-from-pr-diff`.
///
/// Resolves the two accepted forms:
/// - a leading `@` means "read the file at the remaining path, one path
///   per line";
/// - otherwise the value is a literal list split on `,` and `\n`.
///
/// In both forms each entry is trimmed and empty entries are dropped, so
/// trailing newlines, blank lines, and incidental whitespace are
/// tolerated.
///
/// # Errors
///
/// Returns a stringified error (for clap's usage-error path, exit 2) when
/// an `@file` cannot be read or is not valid UTF-8.
pub fn parse_arg_value(s: &str) -> Result<ChangedFiles, String> {
    if let Some(file) = s.strip_prefix('@') {
        let paths = read_file_list(Path::new(file))
            .map_err(|err| format!("could not read changed-files list at {file}: {err}"))?;
        Ok(ChangedFiles { paths })
    } else {
        Ok(ChangedFiles {
            paths: split_trim_drop(s, &[',', '\n']),
        })
    }
}

/// Read a changed-files list file: one path per line, each trimmed, with
/// empty lines dropped.
///
/// `\r\n` (CRLF) line endings are handled by [`str::lines`]; any residual
/// whitespace is removed by the per-line trim.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when the file cannot be read or
/// is not valid UTF-8.
pub fn read_file_list(path: &Path) -> io::Result<Vec<String>> {
    let contents = fs::read_to_string(path)?;
    // `lines()` splits on `\n` and strips a trailing `\r`, so CRLF is
    // handled; the per-line trim removes any residual whitespace.
    Ok(split_trim_drop(&contents, &['\n']))
}

/// Split `s` on any of `seps`, trim each segment, and drop empties.
///
/// Shared by both `--scope-from-pr-diff` forms: the literal list splits
/// on `,` and `\n`; the `@file` form splits on `\n` only (commas are
/// valid inside a single path on a line).
fn split_trim_drop(s: &str, seps: &[char]) -> Vec<String> {
    s.split(seps)
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
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
        std::env::temp_dir().join(format!("cute-dbt-prdiff-{pid}-{micros}-{nonce}-{stem}.txt"))
    }

    fn write_fixture(stem: &str, content: &str) -> std::path::PathBuf {
        let path = unique_temp_path(stem);
        let mut f = std::fs::File::create(&path).expect("create temp fixture");
        f.write_all(content.as_bytes()).expect("write temp fixture");
        path
    }

    // ----- parse_arg_value: literal list form -----

    #[test]
    fn literal_single_path_parses() {
        let cf = parse_arg_value("models/a.sql").expect("a literal path parses");
        assert_eq!(cf.paths, vec!["models/a.sql"]);
    }

    #[test]
    fn literal_comma_separated_list_parses() {
        let cf = parse_arg_value("models/a.sql,models/b.yml").expect("comma list parses");
        assert_eq!(cf.paths, vec!["models/a.sql", "models/b.yml"]);
    }

    #[test]
    fn literal_newline_separated_list_parses() {
        let cf = parse_arg_value("models/a.sql\nmodels/b.yml").expect("newline list parses");
        assert_eq!(cf.paths, vec!["models/a.sql", "models/b.yml"]);
    }

    #[test]
    fn literal_trims_each_entry() {
        let cf = parse_arg_value("  models/a.sql ,\tmodels/b.yml  ").expect("trims");
        assert_eq!(cf.paths, vec!["models/a.sql", "models/b.yml"]);
    }

    #[test]
    fn literal_drops_empty_segments() {
        let cf = parse_arg_value("models/a.sql,,\n  \n,models/b.yml").expect("drops empties");
        assert_eq!(cf.paths, vec!["models/a.sql", "models/b.yml"]);
    }

    // ----- parse_arg_value: @file form -----

    #[test]
    fn at_file_reads_one_path_per_line() {
        let path = write_fixture("list", "models/a.sql\nmodels/b.yml\n");
        let arg = format!("@{}", path.display());
        let cf = parse_arg_value(&arg).expect("@file reads");
        assert_eq!(cf.paths, vec!["models/a.sql", "models/b.yml"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn at_file_handles_crlf_line_endings() {
        let path = write_fixture("crlf", "models/a.sql\r\nmodels/b.yml\r\n");
        let arg = format!("@{}", path.display());
        let cf = parse_arg_value(&arg).expect("@file CRLF reads");
        assert_eq!(cf.paths, vec!["models/a.sql", "models/b.yml"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn at_file_trims_and_drops_blank_lines() {
        let path = write_fixture("blanks", "  models/a.sql  \n\n   \nmodels/b.yml\n");
        let arg = format!("@{}", path.display());
        let cf = parse_arg_value(&arg).expect("@file trims");
        assert_eq!(cf.paths, vec!["models/a.sql", "models/b.yml"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn at_file_with_unicode_paths_parses() {
        let path = write_fixture("unicode", "models/café_revenue.sql\nmodels/naïve.yml\n");
        let arg = format!("@{}", path.display());
        let cf = parse_arg_value(&arg).expect("@file unicode reads");
        assert_eq!(
            cf.paths,
            vec!["models/café_revenue.sql", "models/naïve.yml"]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn at_empty_file_parses_to_empty_list() {
        let path = write_fixture("empty", "");
        let arg = format!("@{}", path.display());
        let cf = parse_arg_value(&arg).expect("@empty file is Ok, not an error");
        assert!(cf.paths.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn at_missing_file_is_an_error() {
        let path = unique_temp_path("does-not-exist");
        // Deliberately do NOT create the file.
        let arg = format!("@{}", path.display());
        let err = parse_arg_value(&arg).expect_err("@missing file is an error");
        assert!(
            err.contains("could not read"),
            "error explains the read failure: {err}"
        );
    }

    // ----- read_file_list (direct) -----

    #[test]
    fn read_file_list_trims_and_drops_empties() {
        let path = write_fixture("direct", " models/a.sql \n\n  \nmodels/b.yml\n");
        let lines = read_file_list(&path).expect("reads the list");
        assert_eq!(lines, vec!["models/a.sql", "models/b.yml"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_file_list_missing_is_an_io_error() {
        let path = unique_temp_path("missing");
        let err = read_file_list(&path).expect_err("missing file is an io error");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
