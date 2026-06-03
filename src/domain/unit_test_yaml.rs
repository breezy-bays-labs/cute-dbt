//! Pure line-based slicer for a `unit_test`'s authored YAML block.
//!
//! Locates a `unit_test` entry by name inside a dbt schema/model/`unit_test`
//! YAML file and returns the raw lines spanning the entry's body plus
//! its leading and trailing comments. The slicer is intentionally
//! line-and-indent-based rather than YAML-parsed — dbt's YAML
//! conventions are strict enough that a small state machine here is
//! correct and avoids pulling in a YAML parsing dependency at the
//! domain layer (which would also violate POD-only). Same spirit as
//! the hand-rolled RFC 4180 CSV parser in `templates/report.html`
//! (cute-dbt#66).
//!
//! ## Scope and structure
//!
//! cute-dbt encounters two common dbt conventions for storing unit
//! test YAML:
//!
//! 1. A schema-style combined file (`<directory>_models.yml`,
//!    `_schema.yml`) — has top-level `version`, `models`, `unit_tests`,
//!    possibly `sources` or `seeds`. The `unit_test` entries are list
//!    items under `unit_tests:`.
//! 2. A per-model file (`<model>_unit_tests.yml`) — has only
//!    `unit_tests:` at the top with that one model's tests.
//!
//! Both arrive at the slicer the same way: the dbt manifest's
//! `unit_tests.<id>.original_file_path` points at the source YAML, and
//! the slicer locates the named entry inside the file's `unit_tests:`
//! list. No structural difference between the two conventions from
//! this layer's perspective.
//!
//! ## Comment-bracketing rule (decision)
//!
//! - **Leading**: a contiguous block of `#`-comment lines at the same
//!   indent as `- name:`, immediately preceding the `- name:` line
//!   (no blank-line gap), is part of THIS test's slice.
//!
//! - **Trailing**: a contiguous block of `#`-comment lines at the
//!   same indent as `- name:`, immediately following the last content
//!   line of THIS test's body (no blank-line gap), is part of THIS
//!   test's slice **only if** the comment block is itself followed by
//!   a blank line or EOF. If the comment block butts directly against
//!   the next sibling `- name:`, the comments belong to that next
//!   test's leading (leading-wins tiebreaker). This keeps a single
//!   comment line between two tests unambiguous.
//!
//! - **Inline** comments inside the body (between fields, between
//!   given/expect entries) ride along with the body slice naturally —
//!   they are preserved as raw text.
//!
//! - Indentation in the leading/trailing match is column-exact against
//!   the `- name:` line's leading-space count. Comments at column 0
//!   (section-level) are NOT picked up; comments deeper than `- name:`
//!   indent are NOT picked up either.
//!
//! ## Failure modes
//!
//! - Test not found by name → `None`.
//! - File contains no `unit_tests:` top-level key → `None`.
//! - File can be parsed line-by-line but the slicer never finds an
//!   eligible `- name:` matching the requested test → `None`.
//!
//! Hard parse errors are not possible from this function — it never
//! interprets YAML semantics beyond list-item structure and `#`
//! comments.

use serde::{Deserialize, Serialize};

/// One `unit_test`'s authored YAML slice, ready to surface in the report.
///
/// `raw` is the verbatim text from the source file, including leading
/// comments (above `- name:`) and trailing comments (below the body),
/// joined with `\n` and ending without a trailing newline. The slice
/// preserves original indentation so a copy/paste back into the source
/// file is round-trip-safe.
///
/// `line_of_name` is the 1-based line number of the `- name: <test>`
/// line in the source file — load-bearing for any future "jump to
/// source" surface but not used by v0.1 rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitTestYamlBlock {
    /// Verbatim YAML slice for this `unit_test`, including leading and
    /// trailing comments per the bracketing rule above.
    pub raw: String,
    /// 1-based line number of the `- name: <test>` line in the source.
    pub line_of_name: usize,
    /// 1-based source line of the slice's FIRST line — the first leading
    /// comment, or the `- name:` line when there are none. The inclusive
    /// lower edge of the span cute-dbt#96 overlaps against a diff hunk to
    /// decide block-precise `changed`.
    pub block_start: usize,
    /// 1-based source line of the slice's LAST line — the last trailing
    /// comment, or the last body line. The inclusive upper edge.
    ///
    /// Two invariants hold by construction (pinned by the slicer tests):
    /// `block_start <= line_of_name <= block_end`, and
    /// `block_end - block_start + 1 == raw.split('\n').count()`.
    pub block_end: usize,
}

impl UnitTestYamlBlock {
    /// Construct from owned parts. Reserved for tests and the slicer.
    ///
    /// `block_start` / `block_end` are 1-based inclusive source-line
    /// bounds of `raw` (see the field docs for the two invariants).
    #[must_use]
    pub fn new(raw: String, line_of_name: usize, block_start: usize, block_end: usize) -> Self {
        Self {
            raw,
            line_of_name,
            block_start,
            block_end,
        }
    }
}

/// Extract the raw YAML slice for a `unit_test` named `test_name` from
/// the contents of a dbt YAML file. Returns `None` if the test is not
/// found.
///
/// The slice includes leading and trailing comment lines per the
/// bracketing rule documented in the module comment. No YAML semantics
/// are interpreted beyond list-item indentation and `#` comments.
#[must_use]
pub fn extract_unit_test_block(file_contents: &str, test_name: &str) -> Option<UnitTestYamlBlock> {
    let lines: Vec<&str> = file_contents.split('\n').collect();

    // Locate the `unit_tests:` top-level key. dbt allows it at column
    // 0 only — any deeper indent would not be a top-level YAML key.
    let unit_tests_idx = lines.iter().position(|l| l.starts_with("unit_tests:"))?;

    // Find the first sibling `- name:` after `unit_tests:` to pin the
    // list-item indent (the column where each `-` sits). Once we know
    // that indent, sibling matching and same-indent comment matching
    // are well-defined.
    let (list_indent, _first_name_idx) = find_list_item_indent(&lines, unit_tests_idx + 1)?;

    // Scan for `- name: <test_name>` at the list-item indent. dbt
    // allows quoted or unquoted scalars for the name — accept either.
    let name_idx = find_named_list_item(&lines, unit_tests_idx + 1, list_indent, test_name)?;

    // Body end = last line that belongs to THIS test's entry. The
    // entry ends at the next sibling `- ` at the same indent, or at
    // a line at indent <= list_indent that is not blank (i.e., end of
    // the `unit_tests:` section), or at EOF.
    let body_end = find_body_end(&lines, name_idx, list_indent);

    // Walk back from the `- name:` line, collecting contiguous same-
    // indent `#`-comment lines. Stop at blank or non-comment.
    let leading_start = find_leading_start(&lines, name_idx, list_indent);

    // Walk forward from body_end + 1, collecting candidate trailing
    // comment lines. Apply the leading-wins tiebreaker: if the
    // candidate block butts directly against the next sibling
    // `- name:`, attribute them to that next test's leading instead.
    let trailing_end = find_trailing_end(&lines, body_end, list_indent);

    let raw = lines[leading_start..=trailing_end].join("\n");
    // 1-based, inclusive: the slice spans source lines
    // `leading_start..=trailing_end` (0-based), so the block bounds are
    // those + 1. cute-dbt#96 overlaps [block_start, block_end] against the
    // diff's hunks to decide block-precise `changed`.
    Some(UnitTestYamlBlock::new(
        raw,
        name_idx + 1,
        leading_start + 1,
        trailing_end + 1,
    ))
}

/// Locate the first `- ` list item at any indent on or after `start_idx`.
/// Returns `(indent, line_idx)`. Used to pin the canonical list-item
/// indent — the column where each `-` sits under `unit_tests:`.
fn find_list_item_indent(lines: &[&str], start_idx: usize) -> Option<(usize, usize)> {
    for (offset, line) in lines.iter().enumerate().skip(start_idx) {
        let trimmed = line.trim_start();
        // Must start with a dash followed by space — `- name: ...`.
        if trimmed.starts_with("- ") {
            let indent = line.len() - trimmed.len();
            return Some((indent, offset));
        }
        // A non-blank, non-comment line at column 0 before any list
        // item means we've fallen out of `unit_tests:` — no list.
        if !trimmed.is_empty() && !trimmed.starts_with('#') && !line.starts_with(' ') {
            return None;
        }
    }
    None
}

/// Find the list item under `unit_tests:` whose `name:` field matches
/// `test_name`. Returns the 0-based line index of the `- ` line that
/// starts the item, or `None`.
///
/// YAML allows dict keys in any order inside a list item, so `name:`
/// may appear on the `- ` line itself (`- name: foo`) or on any
/// subsequent line at the canonical field-indent for that item
/// (Gemini code-review on cute-dbt#70 surfaced this — the previous
/// impl required `- name:` to be the first field, which is convention
/// in dbt YAML but not enforced). Block-scalar continuation lines
/// (e.g. lines under `description: |-`) are skipped because their
/// indent exceeds the field-indent.
///
/// Accepts the name unquoted (`name: foo`), single-quoted
/// (`name: 'foo'`), or double-quoted (`name: "foo"`).
fn find_named_list_item(
    lines: &[&str],
    start_idx: usize,
    list_indent: usize,
    test_name: &str,
) -> Option<usize> {
    for (offset, line) in lines.iter().enumerate().skip(start_idx) {
        let trimmed = line.trim_start();
        let cur_indent = line.len() - trimmed.len();

        // A line at indent < list_indent that is non-blank, non-comment
        // means we've left the `unit_tests:` list — stop.
        if cur_indent < list_indent && !trimmed.is_empty() && !trimmed.starts_with('#') {
            return None;
        }

        // Only consider `- ` list items at the canonical list indent.
        if cur_indent != list_indent || !trimmed.starts_with("- ") {
            continue;
        }

        if list_item_name_matches(lines, offset, list_indent, test_name) {
            return Some(offset);
        }
    }
    None
}

/// Whether the `unit_test` list item starting at `item_start` carries a
/// `name:` field matching `test_name`. The field can be on the `- `
/// line itself or on any subsequent line at the item's canonical
/// field-indent. `item_start` MUST be a line whose `trim_start()`
/// begins with `"- "`.
fn list_item_name_matches(
    lines: &[&str],
    item_start: usize,
    list_indent: usize,
    test_name: &str,
) -> bool {
    let first = lines[item_start];
    let trimmed = first.trim_start();
    // Safe because caller verified `starts_with("- ")`.
    let after_dash_full = &trimmed[2..];
    let after_dash = after_dash_full.trim_start();
    // The canonical field-indent for this item is the column where
    // the first field after the dash starts. Equivalent to:
    // list_indent + 2 (for "- ") + any extra spaces between the dash
    // and the first non-space character on the same line.
    let field_indent = list_indent + 2 + (after_dash_full.len() - after_dash.len());

    // Case 1: `name:` is the first field on the `- ` line.
    if let Some(rest) = after_dash.strip_prefix("name:") {
        if parse_yaml_scalar(rest) == test_name {
            return true;
        }
    }

    // Case 2: `name:` is on a subsequent line at the field-indent.
    // Stop scanning when we hit the next list item (indent <=
    // list_indent) or the end of the file.
    for line in lines.iter().skip(item_start + 1) {
        let t = line.trim_start();
        let ci = line.len() - t.len();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if ci <= list_indent {
            break;
        }
        // Lines deeper than field_indent are nested values (block
        // scalars, given/expect bodies). They cannot carry a top-level
        // field of THIS item.
        if ci != field_indent {
            continue;
        }
        if let Some(rest) = t.strip_prefix("name:") {
            return parse_yaml_scalar(rest) == test_name;
        }
    }

    false
}

/// Strip leading whitespace, optional quote, then read the scalar up to
/// the first end-of-scalar boundary (matching closing quote, or
/// whitespace / `#` for unquoted). Returns the scalar with surrounding
/// whitespace and quotes removed.
///
/// `pub(crate)` so the cell-table IR's block-dict parser
/// ([`crate::domain::unit_test_table::parse_block_dict_rows`], cute-dbt#98)
/// reuses the same quote-stripping semantics on the OLD-side YAML tokens
/// rather than re-deriving them.
pub(crate) fn parse_yaml_scalar(raw: &str) -> String {
    let s = raw.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return rest[..end].to_string();
        }
        return rest.to_string();
    }
    if let Some(rest) = s.strip_prefix('\'') {
        if let Some(end) = rest.find('\'') {
            return rest[..end].to_string();
        }
        return rest.to_string();
    }
    // Unquoted scalar: read up to whitespace or `#`.
    let end = s
        .find(|c: char| c.is_whitespace() || c == '#')
        .unwrap_or(s.len());
    s[..end].to_string()
}

/// Walk forward from the `- name:` line collecting all lines that
/// belong to this list entry. The body extends through any line at
/// indent > `list_indent`, plus blank lines as long as more body
/// content follows before any sibling/section boundary. Trailing
/// blank lines are NOT included in the body span (they belong to the
/// inter-test whitespace).
fn find_body_end(lines: &[&str], name_idx: usize, list_indent: usize) -> usize {
    let mut last_content_idx = name_idx;
    for (i, line) in lines.iter().enumerate().skip(name_idx + 1) {
        let trimmed = line.trim_start();
        let cur_indent = line.len() - trimmed.len();

        if trimmed.is_empty() {
            // Blank line — could be inside the block (between fields)
            // or could mark the end. Decide by peeking ahead: if any
            // following line is indented deeper than `list_indent`
            // before we hit a sibling/section boundary, the blank
            // line is still part of the block. Otherwise, the block
            // already ended at `last_content_idx`.
            continue;
        }
        if cur_indent <= list_indent {
            // Sibling `- ` or section boundary — block ends here.
            break;
        }
        // Deeper indent — part of this entry's body.
        last_content_idx = i;
    }
    last_content_idx
}

/// Walk backward from the `- name:` line collecting contiguous
/// `#`-comment lines at the SAME indent as `- name:`. Stop at the
/// first blank, non-comment, or differently-indented line.
fn find_leading_start(lines: &[&str], name_idx: usize, list_indent: usize) -> usize {
    let mut start = name_idx;
    let mut i = name_idx;
    while i > 0 {
        i -= 1;
        let line = lines[i];
        let trimmed = line.trim_start();
        let cur_indent = line.len() - trimmed.len();
        if trimmed.is_empty() {
            break;
        }
        if !trimmed.starts_with('#') {
            break;
        }
        if cur_indent != list_indent {
            break;
        }
        start = i;
    }
    start
}

/// Walk forward from `body_end + 1` collecting candidate trailing
/// `#`-comment lines at `list_indent`. Apply the leading-wins
/// tiebreaker: if the candidate block butts directly against a
/// sibling `- name:` (no blank-line gap), those comments belong to
/// the NEXT test's leading and are NOT in this test's trailing.
fn find_trailing_end(lines: &[&str], body_end: usize, list_indent: usize) -> usize {
    let mut end = body_end;
    let mut candidate_end = body_end;
    let mut have_candidates = false;

    for (i, line) in lines.iter().enumerate().skip(body_end + 1) {
        let trimmed = line.trim_start();
        let cur_indent = line.len() - trimmed.len();
        if trimmed.is_empty() {
            break;
        }
        if !trimmed.starts_with('#') {
            // Non-comment at any indent — stop. If this is the next
            // sibling `- ` at `list_indent`, the leading-wins rule
            // discards our candidates.
            if cur_indent == list_indent && trimmed.starts_with("- ") && have_candidates {
                return body_end;
            }
            break;
        }
        if cur_indent != list_indent {
            break;
        }
        candidate_end = i;
        have_candidates = true;
    }
    if have_candidates {
        end = candidate_end;
    }
    end
}

#[cfg(test)]
#[allow(clippy::pedantic, clippy::cargo)]
mod tests {
    use super::*;

    // Helper — most test fixtures are tiny inline YAML strings that
    // would otherwise be quoted with quoting noise. The helper trims
    // a single leading newline so test cases can start with a clean
    // multi-line string literal.
    fn yaml(s: &str) -> String {
        s.strip_prefix('\n').unwrap_or(s).to_string()
    }

    #[test]
    fn returns_none_when_file_has_no_unit_tests_block() {
        let src = yaml(
            "
version: 2
models:
  - name: foo
    description: a model
",
        );
        assert_eq!(extract_unit_test_block(&src, "any_test"), None);
    }

    #[test]
    fn returns_none_when_test_name_not_found() {
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
",
        );
        assert_eq!(extract_unit_test_block(&src, "test_z"), None);
    }

    #[test]
    fn single_test_no_comments_returns_body_only() {
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
    given: []
    expect:
      rows: []
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        assert_eq!(block.line_of_name, 2);
        assert_eq!(
            block.raw,
            "  - name: test_a\n    model: foo\n    given: []\n    expect:\n      rows: []"
        );
    }

    #[test]
    fn leading_comments_at_same_indent_are_included() {
        let src = yaml(
            "
unit_tests:
  # leading comment one
  # leading comment two
  - name: test_a
    model: foo
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        assert_eq!(block.line_of_name, 4);
        assert_eq!(
            block.raw,
            "  # leading comment one\n  # leading comment two\n  - name: test_a\n    model: foo"
        );
    }

    #[test]
    fn leading_comments_at_column_zero_are_not_included() {
        let src = yaml(
            "
unit_tests:
# section-level comment at column 0
  - name: test_a
    model: foo
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        // The column-0 comment is section-level, not list-item-level
        // — it must not be in the slice.
        assert!(!block.raw.contains("section-level"));
        assert!(block.raw.starts_with("  - name: test_a"));
    }

    #[test]
    fn blank_line_terminates_leading_collection() {
        let src = yaml(
            "
unit_tests:
  # far-away comment

  # nearby comment
  - name: test_a
    model: foo
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        // Only the nearby comment is contiguous to `- name:`.
        assert!(block.raw.contains("nearby comment"));
        assert!(!block.raw.contains("far-away comment"));
    }

    #[test]
    fn trailing_comments_followed_by_blank_line_are_included() {
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
  # trailing of test_a

  - name: test_b
    model: bar
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        assert!(block.raw.contains("trailing of test_a"));
        assert!(!block.raw.contains("test_b"));
    }

    #[test]
    fn trailing_comments_butting_against_next_sibling_are_excluded() {
        // Leading-wins tiebreaker: a comment line directly above the
        // next `- name:` with no blank-line gap belongs to that
        // next test's leading, NOT this test's trailing.
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
  # this documents test_b, not test_a
  - name: test_b
    model: bar
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        assert!(
            !block.raw.contains("documents test_b"),
            "trailing should not steal test_b's leading; got: {:?}",
            block.raw
        );
    }

    #[test]
    fn next_test_picks_up_leading_when_previous_did_not_steal_it() {
        // Same fixture as the previous case, but from test_b's side —
        // it should claim the comment as its leading.
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
  # this documents test_b, not test_a
  - name: test_b
    model: bar
",
        );
        let block = extract_unit_test_block(&src, "test_b").expect("test_b present");
        assert!(
            block.raw.contains("documents test_b"),
            "test_b should pick up its leading comment; got: {:?}",
            block.raw
        );
    }

    #[test]
    fn multiple_tests_slice_only_the_requested_one() {
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
    given: []

  - name: test_b
    model: bar
    given: []
",
        );
        let block_a = extract_unit_test_block(&src, "test_a").expect("test_a present");
        let block_b = extract_unit_test_block(&src, "test_b").expect("test_b present");
        assert!(block_a.raw.contains("test_a"));
        assert!(!block_a.raw.contains("test_b"));
        assert!(block_b.raw.contains("test_b"));
        assert!(!block_b.raw.contains("test_a"));
    }

    #[test]
    fn inline_comments_between_fields_are_preserved_in_body() {
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
    # inline comment between fields
    given: []
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        assert!(block.raw.contains("inline comment between fields"));
    }

    #[test]
    fn body_continues_through_multi_line_block_scalars() {
        // `description: |` introduces a block scalar — the deeper-
        // indented lines below it are part of the description, not
        // sibling list items. The slicer treats them as body content
        // by virtue of their indent > list_indent.
        let src = yaml(
            "
unit_tests:
  - name: test_a
    description: |
      first description line
      second description line
    model: foo
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        assert!(block.raw.contains("first description line"));
        assert!(block.raw.contains("second description line"));
        assert!(block.raw.contains("model: foo"));
    }

    #[test]
    fn body_continues_through_internal_blank_lines() {
        let src = yaml(
            "
unit_tests:
  - name: test_a
    description: foo

    model: bar
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        assert!(block.raw.contains("model: bar"));
    }

    #[test]
    fn last_test_in_file_with_no_trailing_content() {
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
    given: []
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        assert!(block.raw.contains("given: []"));
        assert!(block.raw.starts_with("  - name: test_a"));
    }

    #[test]
    fn name_with_single_quotes_is_matched() {
        let src = yaml(
            "
unit_tests:
  - name: 'test_quoted'
    model: foo
",
        );
        let block = extract_unit_test_block(&src, "test_quoted").expect("test_quoted present");
        assert_eq!(block.line_of_name, 2);
    }

    #[test]
    fn name_with_double_quotes_is_matched() {
        let src = yaml(
            "
unit_tests:
  - name: \"test_dquoted\"
    model: foo
",
        );
        let block = extract_unit_test_block(&src, "test_dquoted").expect("test_dquoted present");
        assert_eq!(block.line_of_name, 2);
    }

    #[test]
    fn returns_none_for_unrelated_test_name_in_populated_file() {
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
  - name: test_b
    model: bar
",
        );
        assert_eq!(extract_unit_test_block(&src, "test_c"), None);
    }

    #[test]
    fn combined_schema_file_with_models_and_unit_tests() {
        // The dbt-playground convention — one file holds `models:`,
        // `unit_tests:`, and other top-level keys.
        let src = yaml(
            "
version: 2

models:
  - name: foo
    description: a model

unit_tests:
  - name: test_a
    model: foo
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        assert!(block.raw.contains("  - name: test_a"));
        // No bleed-over from the `models:` section.
        assert!(!block.raw.contains("description: a model"));
    }

    #[test]
    fn per_model_file_with_only_unit_tests() {
        // The `<model>_unit_tests.yml` convention.
        let src = yaml(
            "
unit_tests:
  - name: test_only
    model: foo
",
        );
        let block = extract_unit_test_block(&src, "test_only").expect("test_only present");
        assert!(block.raw.contains("test_only"));
    }

    #[test]
    fn comment_at_wrong_indent_is_not_included() {
        // A comment indented MORE than `- name:` is a field-level
        // comment inside a prior list entry, not this entry's leading.
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
      # over-indented comment
  - name: test_b
    model: bar
",
        );
        let block = extract_unit_test_block(&src, "test_b").expect("test_b present");
        assert!(!block.raw.contains("over-indented comment"));
    }

    // Regression: Gemini code-review on cute-dbt#70 flagged that the
    // initial slicer impl required `- name:` to be the first field of
    // a unit_test list item. YAML allows dict keys in any order, so
    // a user could write `- description:` (or any other key) first
    // and the slicer would fail to locate the test by name.
    #[test]
    fn name_field_not_on_first_line_is_still_found() {
        let src = yaml(
            "
unit_tests:
  - description: \"docs\"
    model: foo
    name: my_test
    given:
      - input: ref('raw_users')
        rows: []
    expect:
      rows: []
  - name: other
    model: bar
",
        );
        let block =
            extract_unit_test_block(&src, "my_test").expect("slicer must locate name-not-first");
        assert!(block.raw.contains("description: \"docs\""));
        assert!(block.raw.contains("name: my_test"));
        // And it must not bleed into the next item.
        assert!(!block.raw.contains("name: other"));
    }

    // Regression: a `name:` token at a deeper indent than the canonical
    // field-indent (e.g. inside a `description: |-` block scalar)
    // must NOT be treated as the item's name field — that would let
    // sentence text accidentally name a test.
    #[test]
    fn name_token_inside_block_scalar_is_ignored() {
        let src = yaml(
            "
unit_tests:
  - description: |-
      A long description that mentions name: not_a_test inside the prose.
    name: real_test
    model: foo
",
        );
        assert!(extract_unit_test_block(&src, "not_a_test").is_none());
        let block = extract_unit_test_block(&src, "real_test").expect("real_test present");
        assert!(block.raw.contains("name: real_test"));
    }

    // ----- cute-dbt#96: block_start / block_end exposure -----
    //
    // Every block-precise off-by-one in #96 Step 2 (`hunk_touches_block`,
    // the N7b alignment offset) is anchored on these two 1-based line
    // numbers, so pin them exactly — plus the span↔raw consistency
    // invariant — BEFORE any overlap logic layers on. A one-off here is
    // silently wrong-but-plausible downstream; caught here it's a single
    // arithmetic fix.

    #[test]
    fn block_span_pins_first_and_last_line_for_a_plain_block() {
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
    given: []
    expect:
      rows: []
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        // 1-based lines: 1 `unit_tests:` / 2 `- name:` / 3 model / 4 given
        // / 5 expect / 6 rows. No leading/trailing comments → the span is
        // the body, `- name:` through `rows: []`.
        assert_eq!(block.block_start, 2, "first slice line is `- name:`");
        assert_eq!(block.block_end, 6, "last slice line is `rows: []`");
        assert_eq!(block.line_of_name, 2);
    }

    #[test]
    fn block_span_includes_leading_comments_in_block_start() {
        let src = yaml(
            "
unit_tests:
  # leading comment one
  # leading comment two
  - name: test_a
    model: foo
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        // 1-based lines: 1 `unit_tests:` / 2 #c1 / 3 #c2 / 4 `- name:` / 5 model.
        assert_eq!(
            block.block_start, 2,
            "block_start is the first leading comment, not `- name:`",
        );
        assert_eq!(block.line_of_name, 4);
        assert_eq!(block.block_end, 5);
    }

    #[test]
    fn block_span_includes_trailing_comments_in_block_end() {
        let src = yaml(
            "
unit_tests:
  - name: test_a
    model: foo
  # trailing of test_a

  - name: test_b
    model: bar
",
        );
        let block = extract_unit_test_block(&src, "test_a").expect("test_a present");
        // 1-based: 1 `unit_tests:` / 2 `- name:` / 3 model / 4 #trailing /
        // 5 blank / 6 `- name: test_b`. The trailing comment (line 4) is
        // the last slice line; the blank gap stops the trailing scan.
        assert_eq!(block.block_start, 2);
        assert_eq!(
            block.block_end, 4,
            "the trailing comment is the last line of the slice",
        );
    }

    // The span↔raw consistency invariant the overlap math depends on:
    // `block_end - block_start + 1` equals the raw slice's line count, and
    // the name line lies within the span. Swept across representative
    // shapes (plain, leading comment, block scalar, combined schema file)
    // so a one-off in either edge is caught here, not downstream.
    #[test]
    fn block_span_is_consistent_with_raw_across_fixtures() {
        let fixtures = [
            "\nunit_tests:\n  - name: t\n    model: m\n",
            "\nunit_tests:\n  # lead\n  - name: t\n    model: m\n    given: []\n",
            "\nunit_tests:\n  - name: t\n    description: |\n      line one\n      line two\n    model: m\n",
            "\nversion: 2\n\nmodels:\n  - name: m\n\nunit_tests:\n  - name: t\n    model: m\n",
        ];
        for src in fixtures {
            let s = yaml(src);
            let block = extract_unit_test_block(&s, "t").expect("t present");
            let line_count = block.raw.split('\n').count();
            assert_eq!(
                block.block_end - block.block_start + 1,
                line_count,
                "span [{}, {}] must match the {line_count}-line raw slice for {src:?}",
                block.block_start,
                block.block_end,
            );
            assert!(
                block.block_start <= block.line_of_name && block.line_of_name <= block.block_end,
                "name line {} must lie within span [{}, {}]",
                block.line_of_name,
                block.block_start,
                block.block_end,
            );
        }
    }
}
