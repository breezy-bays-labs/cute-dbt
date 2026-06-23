//! `src/adapters/raw_scan.rs` — the raw-source CTE/column boundary scanner
//! (cute-dbt#469, S1).
//!
//! ADAPTER (cute-dbt#40 pattern): parses the model's `raw_code` and writes POD
//! facts BACK onto the domain [`SourceMap`] spine — it fills the reserved
//! per-entry `raw: Option<SourceSpan>` slot for the **non-zone** `CteBody` /
//! `Column` entries. Zones already carry their raw span (cute-dbt#448); this
//! fills the rest. The domain stays std+serde-only; all Jinja-awareness lives
//! here.
//!
//! NORTH STAR — never-a-false-claim. A raw span is emitted ONLY when the
//! CTE/column name resolves to EXACTLY ONE lexical anchor in the **masked** raw
//! text. Zero or multiple matches ⇒ the key is OMITTED (an unanchored region is
//! never a picked offset). The mask (offset-preserving) blanks BOTH every
//! `{%…%}` / `{{…}}` / `{#…#}` Jinja region AND every SQL comment (`--`-to-EOL,
//! `/* … */`), so a name that only appears INSIDE Jinja OR inside a SQL comment
//! is never matched or anchored (a CTE/column name living in commented-out code
//! is NOT a live definition — anchoring to it would be a false claim,
//! cute-dbt#469). Malformed/unbalanced Jinja ⇒ the model emits NOTHING
//! (fail-closed, mirroring `locate_raw_zones`).
//!
//! The mask is for MATCHING + boundary-finding ONLY: the final emitted SPAN is
//! still over the ORIGINAL raw bytes (a string literal or comment INSIDE a live
//! CTE body stays part of that body — masking only blanks the matched tokens'
//! own region, and the span is taken over the same offset range of the source).
//!
//! Jinja-region close-finding REUSES render.rs's vetted scanners
//! (`render::find_close` / `render::find_expr_close`) rather than a divergent
//! local copy — the two paths (zone scan + raw span) must agree on Jinja
//! boundaries on the SAME model, including the backslash string-escape
//! (cute-dbt#469).
//!
//! The two render projections that read the filled `e.raw` —
//! `code_map.raw_node_spans` / `code_map.raw_column_spans` — live in `render.rs`
//! beside their compiled twins (`node_spans` / `column_spans`).

use crate::adapters::cte_engine::TERMINAL_NODE_NAME;
use crate::adapters::render::{byte_span, find_close, find_expr_close};
use crate::domain::source_map::{SourceMap, SpanRole};
use crate::domain::span::SourceSpan;

/// Fill the reserved `raw` slot of every NON-zone `CteBody` / `Column` entry of
/// `sm` whose name resolves to a UNIQUE lexical anchor in the masked `raw` text.
/// Entries that already carry a `raw` span (the zone path, cute-dbt#448) are left
/// untouched. A name matching zero or multiple times is OMITTED (left `None`).
///
/// Fail-closed (never-a-false-claim): if `raw` contains malformed/unbalanced
/// Jinja, [`mask_regions`] returns `None` and this fills NOTHING — never a
/// partial guess over a stream it could not safely mask.
pub(crate) fn fill_raw_spans(sm: &mut SourceMap, raw: &str) {
    let Some(masked) = mask_regions(raw) else {
        // Malformed Jinja ⇒ emit nothing for this model (fail-closed).
        return;
    };
    for entry in &mut sm.entries {
        // Never overwrite a raw span the zone path already filled.
        if entry.raw.is_some() {
            continue;
        }
        let span = match &entry.role {
            SpanRole::CteBody { node_id } => cte_raw_span(&masked, node_id),
            SpanRole::Column { column, .. } => column_raw_span(&masked, column),
            // Zone entries carry their own raw span (cute-dbt#448); never touched.
            _ => None,
        };
        if let Some(span) = span {
            entry.raw = Some(span);
        }
    }
}

/// The UNIQUE raw span of a `WITH`-defined CTE named `node_id` — the
/// `name AS ( … )` extent in the masked text, or `None` when not uniquely
/// anchored. The TERMINAL node has no verbatim name token in raw, so it is
/// always omitted in S1 (its sound raw origin is the terminal-synthesis of a
/// later slice — drop-not-fabricate).
fn cte_raw_span(masked: &str, node_id: &str) -> Option<SourceSpan> {
    if node_id == TERMINAL_NODE_NAME {
        return None;
    }
    // Find every CTE-definition site `<name> AS (` for this exact name.
    let mut hit: Option<(usize, usize)> = None;
    for (start, after_name) in name_occurrences(masked, node_id) {
        // The token must be FOLLOWED (skipping whitespace) by `as (` —
        // the CTE-definition boundary. A bare reference (`from <name>`) or an
        // alias elsewhere is NOT a definition site and must not match.
        let Some(open_paren) = cte_definition_open_paren(masked, after_name) else {
            continue;
        };
        // Balance parens from the opener to the matching close in masked text.
        let Some(close_end) = balanced_close(masked, open_paren) else {
            continue;
        };
        if hit.is_some() {
            // A second definition site ⇒ ambiguous ⇒ omit (never pick one).
            return None;
        }
        hit = Some((start, close_end));
    }
    let (start, end) = hit?;
    byte_span(masked, start, end)
}

/// The UNIQUE raw span of an output column named `column` — the column name
/// token, when it occurs EXACTLY ONCE as a whole-word identifier in masked raw.
/// Zero or multiple matches (templated / macro-expanded / a name reused across
/// CTEs) ⇒ `None` (omit the key — no sound raw region).
fn column_raw_span(masked: &str, column: &str) -> Option<SourceSpan> {
    let mut hits = name_occurrences(masked, column);
    let first = hits.next()?;
    if hits.next().is_some() {
        // Multiple occurrences ⇒ ambiguous ⇒ omit.
        return None;
    }
    let (start, after) = first;
    byte_span(masked, start, after)
}

/// Every whole-word occurrence of `name` in `text`, as `(start, end)` byte
/// offsets. "Whole-word" = the char before `start` and the char at `end` are
/// NOT identifier characters (`[A-Za-z0-9_]`), so `order` does not match inside
/// `orders` / `reorder`. ASCII-case-sensitive (SQL identifiers are folded to a
/// stable case by the engine before they reach the domain).
fn name_occurrences<'a>(text: &'a str, name: &'a str) -> impl Iterator<Item = (usize, usize)> + 'a {
    let nlen = name.len();
    text.match_indices(name).filter_map(move |(start, _)| {
        if nlen == 0 {
            return None;
        }
        let end = start + nlen;
        let before_ok = start == 0 || !is_ident_byte(text.as_bytes()[start - 1]);
        let after_ok = end >= text.len() || !is_ident_byte(text.as_bytes()[end]);
        (before_ok && after_ok).then_some((start, end))
    })
}

/// Whether `b` is an ASCII identifier byte (`[A-Za-z0-9_]`).
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Given the byte index just past a CTE name token, return the byte index of
/// the `(` that opens its body IFF the name is immediately followed (skipping
/// whitespace) by `as` (ASCII-case-insensitive, whole-word) then `(`. Returns
/// `None` when the token is NOT a `name AS (` CTE-definition site.
fn cte_definition_open_paren(masked: &str, after_name: usize) -> Option<usize> {
    let bytes = masked.as_bytes();
    let mut i = skip_ws(bytes, after_name);
    // Expect the `as` keyword (whole-word, case-insensitive).
    if i + 2 > bytes.len() {
        return None;
    }
    if !bytes[i].eq_ignore_ascii_case(&b'a') || !bytes[i + 1].eq_ignore_ascii_case(&b's') {
        return None;
    }
    // `as` must be a whole word — the next byte cannot be an identifier byte.
    if i + 2 < bytes.len() && is_ident_byte(bytes[i + 2]) {
        return None;
    }
    i = skip_ws(bytes, i + 2);
    if i < bytes.len() && bytes[i] == b'(' {
        Some(i)
    } else {
        None
    }
}

/// Skip ASCII whitespace from `from` forward; return the first non-whitespace
/// byte index (or `bytes.len()`).
fn skip_ws(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Balance parentheses starting at the opener `open` (a `(`); return the byte
/// index PAST the matching `)`. `None` if unbalanced before EOF (fail-closed).
///
/// Operates on `mask_regions` output, so Jinja regions AND SQL comments are
/// already blanked to spaces — a stray `'`/`)`/`(` inside a `--` / `/* */`
/// comment (e.g. the `dim_date` golden's `-- new year's day`) carries no quote or
/// paren and cannot desync the scan. The only remaining lexical layer is the
/// **live SQL string literal**, which IS preserved in the mask (strings inside a
/// CTE body stay part of the body), so paren counting stays string-aware here:
///
/// - A stray `)` inside a quoted string (e.g. `select ')' as x`) does NOT close
///   the CTE body early — the span is CORRECT and complete, never truncated
///   mid-literal. The SQL doubled-quote escape is honoured: a `''` inside a `'…'`
///   string (or `""` inside a `"…"` string) stays in-string (the doubled quote is
///   an escaped quote char, not a close-then-reopen).
///
/// With Jinja + comments masked and live strings tracked, paren counting over
/// the remaining live SQL is sound.
fn balanced_close(masked: &str, open: usize) -> Option<usize> {
    let bytes = masked.as_bytes();
    let n = bytes.len();
    let mut depth = 0usize;
    let mut i = open;
    let mut quote: Option<u8> = None;
    while i < n {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    // A doubled quote (`''` / `""`) is an escaped quote that
                    // stays in-string: consume both and remain quoted.
                    if i + 1 < n && bytes[i + 1] == q {
                        i += 1;
                    } else {
                        quote = None;
                    }
                }
            }
            None => match b {
                b'\'' | b'"' => quote = Some(b),
                b'(' => depth += 1,
                b')' => {
                    // Fail-closed on an unbalanced `)` at depth 0 (a `)` with no
                    // matching `(`): `checked_sub` returns `None` rather than
                    // usize-underflow-panicking (cute-dbt#469 robustness nit).
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Return a copy of `raw` with every Jinja region — `{%…%}` block tags,
/// `{{…}}` variable tags, `{#…#}` comments — AND every SQL comment (`--`-to-EOL
/// line comments, `/* … */` block comments) replaced by spaces, BYTE-FOR-BYTE
/// (offsets preserved, so a span over the masked text indexes the same region of
/// the original `raw`). Returns `None` on a malformed/unterminated Jinja region
/// (fail-closed — mirrors `scan_block_tags`).
///
/// Masking (not deletion) is the load-bearing honesty primitive: a CTE/column
/// name that only appears INSIDE a Jinja region OR inside a SQL comment is
/// blanked out, so it can never be matched as a verbatim raw anchor OR a
/// `name AS (` definition site — a name in commented-out code is not a live
/// definition (never-a-false-claim, cute-dbt#469).
///
/// The Jinja close-finders are render.rs's vetted scanners
/// (`render::find_close` / `render::find_expr_close`), so the raw-span path and
/// the zone path (`scan_block_tags`) agree on Jinja boundaries — including the
/// backslash string-escape — on the SAME model (one shared implementation, no
/// divergence).
///
/// **Precedence.** A SQL comment opener (`--`/`/*`) is only honoured OUTSIDE a
/// Jinja region (the Jinja regions are consumed first); conversely a `{`-opener
/// is only honoured outside a SQL comment. This mirrors a lexer's single forward
/// pass — whichever opener is reached first claims its region.
pub(crate) fn mask_regions(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let n = bytes.len();
    // Start from the original bytes; blank Jinja + SQL-comment regions in place.
    // We only ever overwrite ASCII bytes with ASCII spaces, so the result stays
    // valid UTF-8 and every byte offset is preserved.
    let mut out = raw.as_bytes().to_vec();
    let mut i = 0usize;
    while i < n {
        if bytes[i] == b'{' && i + 1 < n {
            let close = match bytes[i + 1] {
                // Comment `{#…#}` — literal scan to `#}`.
                b'#' => find_close(bytes, i + 2, b'#')?,
                // Variable `{{…}}` — string-literal-aware scan to `}}`.
                b'{' => find_expr_close(bytes, i + 2, b'}')?,
                // Block `{%…%}` — string-literal-aware scan to `%}`.
                b'%' => find_expr_close(bytes, i + 2, b'%')?,
                _ => {
                    i += 1;
                    continue;
                }
            };
            blank(&mut out, i, close);
            i = close;
        } else if bytes[i] == b'-' && i + 1 < n && bytes[i + 1] == b'-' {
            // `--` line comment: blank to end-of-line (the `\n` stays live).
            let mut j = i + 2;
            while j < n && bytes[j] != b'\n' {
                j += 1;
            }
            blank(&mut out, i, j);
            i = j;
        } else if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            // `/* … */` block comment: blank to the closing `*/` (or EOF if
            // unterminated — an unterminated block comment swallows the rest,
            // mirroring how a SQL lexer treats it; never a false anchor).
            let mut j = i + 2;
            while j + 1 < n && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                j += 1;
            }
            let close = if j + 1 < n { j + 2 } else { n };
            blank(&mut out, i, close);
            i = close;
        } else {
            i += 1;
        }
    }
    // SAFETY-FREE: we only replaced ASCII bytes with ASCII spaces over byte
    // ranges that began at an ASCII opener (a char boundary), so the result is
    // valid UTF-8. Use the checked constructor regardless (no unsafe).
    String::from_utf8(out).ok()
}

/// Blank `out[from..to]` with ASCII spaces (offset-preserving).
fn blank(out: &mut [u8], from: usize, to: usize) {
    let end = to.min(out.len());
    for b in &mut out[from..end] {
        *b = b' ';
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::source_map::{SourceMapEntry, ZoneKind};

    fn cte_entry(node_id: &str) -> SourceMapEntry {
        SourceMapEntry {
            role: SpanRole::CteBody {
                node_id: node_id.to_owned(),
            },
            raw: None,
            compiled: None,
        }
    }

    fn column_entry(node_id: &str, column: &str) -> SourceMapEntry {
        SourceMapEntry {
            role: SpanRole::Column {
                node_id: node_id.to_owned(),
                column: column.to_owned(),
            },
            raw: None,
            compiled: None,
        }
    }

    fn sm_with(entries: Vec<SourceMapEntry>) -> SourceMap {
        SourceMap {
            compiled: String::new(),
            entries,
        }
    }

    /// The masked slice a filled raw span points at, for asserting on content.
    fn raw_slice<'a>(raw: &'a str, span: &SourceSpan) -> &'a str {
        &raw[span.byte_range()]
    }

    // ── mask_regions ─────────────────────────────────────────────────────────

    #[test]
    fn mask_blanks_all_three_jinja_region_kinds_offset_preserving() {
        let raw = "a {{ ref('x') }} b {% if z %} c {# note #} d";
        let masked = mask_regions(raw).expect("well-formed");
        assert_eq!(masked.len(), raw.len(), "offsets preserved");
        // Literal bytes survive; Jinja regions are blanked to spaces.
        assert!(masked.contains("a "));
        assert!(masked.contains(" b "));
        assert!(masked.contains(" c "));
        assert!(masked.contains(" d"));
        assert!(!masked.contains("ref"), "variable tag blanked");
        assert!(!masked.contains("if"), "block tag blanked");
        assert!(!masked.contains("note"), "comment blanked");
    }

    #[test]
    fn mask_fail_closed_on_unterminated_region() {
        assert!(
            mask_regions("select {{ ref('x')").is_none(),
            "unterminated variable tag"
        );
        assert!(
            mask_regions("select {% if z").is_none(),
            "unterminated block tag"
        );
        assert!(
            mask_regions("select {# note").is_none(),
            "unterminated comment"
        );
    }

    #[test]
    fn mask_string_literal_aware_close() {
        // A `}}` inside a quoted string does NOT close the variable tag early.
        let raw = "{{ foo('}}') }} rest";
        let masked = mask_regions(raw).expect("well-formed");
        assert!(masked.ends_with(" rest"));
        assert!(!masked.contains("foo"));
    }

    #[test]
    fn mask_jinja_backslash_string_escape_in_variable_tag() {
        // render::find_expr_close honours the Jinja backslash escape (`\'`): the
        // escaped quote does NOT end the string, so the inner `}}`-less `)` is
        // still in-string and the tag closes only at the REAL `}}`. The raw-span
        // path REUSES that exact scanner, so both paths agree on the boundary.
        let raw = "{{ foo('a\\'b') }} rest";
        let masked = mask_regions(raw).expect("backslash escape handled");
        assert!(masked.ends_with(" rest"), "tag closed at the real `}}`");
        assert!(!masked.contains("foo"), "whole variable tag blanked");
        assert_eq!(masked.len(), raw.len(), "offsets preserved");
    }

    #[test]
    fn mask_blanks_line_and_block_sql_comments() {
        let raw = "select 1 -- a line note\nfrom t /* block note */ x";
        let masked = mask_regions(raw).expect("well-formed");
        assert_eq!(masked.len(), raw.len(), "offsets preserved");
        assert!(masked.contains("select 1 "), "live SQL survives");
        assert!(masked.contains("\nfrom t "), "newline + live SQL survive");
        assert!(masked.ends_with(" x"), "trailing live SQL survives");
        assert!(!masked.contains("line note"), "line comment blanked");
        assert!(!masked.contains("block note"), "block comment blanked");
    }

    #[test]
    fn mask_block_tag_string_literal_with_close_delim() {
        // find_expr_close is used for BOTH {{ }} and {% %}; a `%}` inside a
        // string literal in a `{% set %}` must NOT close the block tag early.
        let raw = "{% set x = '%}' %} rest";
        let masked = mask_regions(raw).expect("string-aware block-tag close");
        assert!(
            masked.ends_with(" rest"),
            "block tag closed at the real closer"
        );
        assert!(!masked.contains("set"), "whole block tag blanked");
        assert!(!masked.contains("rest'"), "no leakage past the string");
    }

    // ── verbatim-unique-hit ─────────────────────────────────────────────────

    #[test]
    fn verbatim_unique_cte_gets_a_raw_span() {
        let raw = "with stg as (select 1) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0].raw.expect("verbatim unique CTE anchored");
        // The span covers the `stg as (select 1)` definition extent.
        assert_eq!(raw_slice(raw, &span), "stg as (select 1)");
    }

    #[test]
    fn verbatim_unique_column_gets_a_raw_span() {
        let raw = "select customer_id from raw";
        let mut sm = sm_with(vec![column_entry("(final select)", "customer_id")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0].raw.expect("unique column anchored");
        assert_eq!(raw_slice(raw, &span), "customer_id");
    }

    // ── ambiguous-name-omit ─────────────────────────────────────────────────

    #[test]
    fn ambiguous_cte_name_two_definitions_omits() {
        // The same CTE name defined twice (e.g. across {% if %}/{% else %}
        // branches, here literal for the test) ⇒ two definition sites ⇒ omit.
        let raw = "with dup as (select 1), dup as (select 2) select * from dup";
        let mut sm = sm_with(vec![cte_entry("dup")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "two definition sites ⇒ no raw span (never pick one)"
        );
    }

    #[test]
    fn ambiguous_column_name_appearing_twice_omits() {
        // A column name that appears more than once cannot be uniquely anchored.
        let raw = "select id, other.id from a join other on a.id = other.id";
        let mut sm = sm_with(vec![column_entry("(final select)", "id")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "name appearing multiply ⇒ omit"
        );
    }

    #[test]
    fn zero_match_name_omits() {
        let raw = "with stg as (select 1) select * from stg";
        let mut sm = sm_with(vec![
            cte_entry("absent_cte"),
            column_entry("x", "absent_col"),
        ]);
        fill_raw_spans(&mut sm, raw);
        assert!(sm.entries[0].raw.is_none(), "absent CTE name ⇒ omit");
        assert!(sm.entries[1].raw.is_none(), "absent column name ⇒ omit");
    }

    // ── templated-column-omit / Jinja-masking ───────────────────────────────

    #[test]
    fn templated_column_inside_jinja_is_not_matched() {
        // The column name appears ONLY inside a Jinja expression region → after
        // masking it is gone → no match → omit (never a fabricated span). Here
        // `amount` is the rendered output of `{{ col_name }}`, present only as a
        // var-tag arg, never as literal text.
        let raw = "select {{ render_col('amount') }}, 1 from t";
        let mut sm = sm_with(vec![column_entry("(final select)", "amount")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a name only inside Jinja is masked away ⇒ omit"
        );
    }

    #[test]
    fn cte_name_inside_jinja_is_not_matched() {
        // The CTE name token lives inside a comment; after masking the only
        // literal `stg` is the `from stg` reference — which is NOT a definition
        // site (no `as (`), so still omitted. Proves masking + definition-site.
        let raw = "{# stg as (select 1) #} select * from other";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a CTE definition inside a Jinja comment is masked ⇒ omit"
        );
    }

    #[test]
    fn bare_cte_reference_is_not_a_definition_site() {
        // `from stg` is a reference, never a `name AS (` definition → omit.
        let raw = "select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a bare reference is not a definition"
        );
    }

    // ── terminal CteBody omitted in S1 ──────────────────────────────────────

    #[test]
    fn terminal_node_is_omitted_in_s1() {
        let raw = "with stg as (select 1) select * from stg";
        let mut sm = sm_with(vec![cte_entry(TERMINAL_NODE_NAME)]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "the terminal node has no verbatim name token in raw ⇒ omitted in S1"
        );
    }

    // ── fail-closed ─────────────────────────────────────────────────────────

    #[test]
    fn malformed_jinja_fills_nothing() {
        // An unbalanced {% leaves the whole model unanchored (fail-closed).
        let raw = "with stg as (select {% if x select 1) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "malformed Jinja ⇒ emit nothing (fail-closed)"
        );
    }

    // ── drop-not-fabricate / zone entries untouched ─────────────────────────

    #[test]
    fn zone_entries_keep_their_existing_raw_span() {
        // A Zone entry already carries its raw span (cute-dbt#448); the scanner
        // must never overwrite it.
        use crate::domain::span::SourcePos;
        let existing = SourceSpan {
            start: SourcePos {
                line: 1,
                col: 1,
                byte: 0,
            },
            end: SourcePos {
                line: 1,
                col: 5,
                byte: 4,
            },
        };
        let mut sm = sm_with(vec![SourceMapEntry {
            role: SpanRole::Zone {
                kind: ZoneKind::IncrementalGuard,
            },
            raw: Some(existing),
            compiled: None,
        }]);
        fill_raw_spans(&mut sm, "with stg as (select 1) select * from stg");
        assert_eq!(
            sm.entries[0].raw,
            Some(existing),
            "zone raw span is preserved, never overwritten"
        );
    }

    #[test]
    fn case_insensitive_as_keyword() {
        let raw = "with stg AS (select 1) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0].raw.expect("uppercase AS still a definition");
        assert_eq!(raw_slice(raw, &span), "stg AS (select 1)");
    }

    #[test]
    fn whole_word_only_no_substring_match() {
        // `order` must not match inside `orders`.
        let raw = "with orders as (select 1) select * from orders";
        let mut sm = sm_with(vec![cte_entry("order")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "substring of a longer identifier is not a match"
        );
    }

    #[test]
    fn nested_parens_balance_correctly() {
        let raw = "with stg as (select coalesce(a, (b)) ) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0].raw.expect("nested parens balanced");
        assert_eq!(raw_slice(raw, &span), "stg as (select coalesce(a, (b)) )");
    }

    // ── string-literal-aware paren balancing (cute-dbt#469) ──────────────────

    #[test]
    fn stray_close_paren_inside_single_quoted_string_does_not_truncate() {
        // The CTE body contains a `)` inside a SQL '…' string literal. Without
        // string-awareness the balancer would close at that `)` and emit a
        // TRUNCATED span (a false claim about where the raw CTE ends). It must
        // skip in-string parens and span the FULL `stg as ( … )` region.
        let raw = "with stg as (select ')' as x) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("stray ) inside a string is not a real close");
        assert_eq!(raw_slice(raw, &span), "stg as (select ')' as x)");
    }

    #[test]
    fn stray_close_paren_inside_double_quoted_string_does_not_truncate() {
        // Same hazard via a double-quoted SQL identifier/string literal.
        let raw = "with stg as (select \")\" as x) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("stray ) inside a double-quoted literal is not a real close");
        assert_eq!(raw_slice(raw, &span), "stg as (select \")\" as x)");
    }

    #[test]
    fn stray_open_paren_inside_string_does_not_break_balance() {
        // A stray `(` inside a string literal must NOT inflate depth — otherwise
        // the body would never balance and the span would be omitted (an honest
        // omission today, but a string-aware balancer makes it a CORRECT span).
        let raw = "with stg as (select '(' as x) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("stray ( inside a string is not a real open");
        assert_eq!(raw_slice(raw, &span), "stg as (select '(' as x)");
    }

    #[test]
    fn doubled_quote_escape_keeps_balancer_in_string() {
        // The SQL doubled-quote escape: `''` inside a '…' string is an escaped
        // quote, NOT a close-then-reopen. The `)` that follows the doubled quote
        // is still INSIDE the string, so it must not truncate the span.
        let raw = "with stg as (select 'a''b)' as x) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("doubled-quote escape stays in-string");
        assert_eq!(raw_slice(raw, &span), "stg as (select 'a''b)' as x)");
    }

    #[test]
    fn apostrophe_in_line_comment_does_not_desync_quotes() {
        // A stray apostrophe in a `--` line comment (`year's`) must NOT be read
        // as a string-literal opener — otherwise it flips quote parity and the
        // body never balances. (This is the dim_date golden's exact hazard:
        // `-- new year's day`.) The body must span its full extent.
        let raw = "with stg as (\n  select 1 -- year's day\n) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("apostrophe in a line comment is not a string opener");
        assert_eq!(
            raw_slice(raw, &span),
            "stg as (\n  select 1 -- year's day\n)"
        );
    }

    #[test]
    fn paren_in_line_comment_does_not_break_balance() {
        // A stray `)` in a `--` comment must not be counted as a real close.
        let raw = "with stg as (\n  select 1 -- a ) paren\n) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("paren in a line comment is not a real close");
        assert_eq!(
            raw_slice(raw, &span),
            "stg as (\n  select 1 -- a ) paren\n)"
        );
    }

    #[test]
    fn apostrophe_and_paren_in_block_comment_do_not_desync() {
        // A `/* … */` block comment carrying both a stray apostrophe and a stray
        // `)` must be skipped wholesale.
        let raw = "with stg as (select 1 /* don't ) */ ) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("block comment contents are skipped");
        assert_eq!(raw_slice(raw, &span), "stg as (select 1 /* don't ) */ )");
    }

    // ── SQL-comment-anchored span is a FALSE CLAIM ⇒ OMIT (cute-dbt#469) ──────

    #[test]
    fn cte_def_inside_line_comment_is_not_a_definition_site() {
        // The ONLY `stg as (` text lives inside a `--` line comment (commented-out
        // code). Masking blanks it, so there is NO live definition site → omit.
        // Anchoring to commented-out code would be a false claim.
        let raw = "-- with stg as (select 1)\nselect * from other";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a CTE def inside a -- comment is not a live definition ⇒ omit"
        );
    }

    #[test]
    fn cte_def_in_trailing_line_comment_is_not_a_definition_site() {
        // A trailing `-- old: with stg as (...)` after live SQL.
        let raw = "select * from other -- old: with stg as (select 1)";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a CTE def in a trailing -- comment is masked ⇒ omit"
        );
    }

    #[test]
    fn cte_def_inside_block_comment_is_not_a_definition_site() {
        // The ONLY `stg as (` text lives inside a `/* … */` block comment.
        let raw = "/* with stg as (select 1) */ select * from other";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a CTE def inside a /* */ comment is masked ⇒ omit"
        );
    }

    #[test]
    fn column_live_once_and_in_comment_anchors_the_live_one() {
        // The column name appears LIVE once and again inside a `--` comment.
        // The commented occurrence is masked away, so the name is uniquely
        // anchored to the LIVE occurrence (not a false ambiguity, not the
        // comment).
        let raw = "select customer_id from raw -- legacy customer_id col";
        let mut sm = sm_with(vec![column_entry("(final select)", "customer_id")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("the live occurrence is the unique anchor");
        assert_eq!(raw_slice(raw, &span), "customer_id");
        // The anchored span is the LIVE one (before the comment), not the masked
        // occurrence inside the comment.
        assert!(
            (span.start.byte as usize) < raw.find("-- legacy").unwrap(),
            "anchored to the live occurrence, not the commented one"
        );
    }

    #[test]
    fn real_def_plus_commented_out_dup_def_anchors_the_real_one() {
        // The secondary false-ambiguity face: a real `dup as (…)` definition
        // alongside a commented-out duplicate. The commented one is masked, so
        // the def is UNIQUE → anchor the REAL one (NOT a false ambiguity that
        // drops the span).
        let raw = "-- with dup as (select 99)\nwith dup as (select 1) select * from dup";
        let mut sm = sm_with(vec![cte_entry("dup")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("commented-out dup is masked ⇒ the real def is unique");
        assert_eq!(raw_slice(raw, &span), "dup as (select 1)");
    }

    // ── council should-fix: completeness (cute-dbt#469) ──────────────────────

    #[test]
    fn nested_ctes_both_anchor() {
        // `with a as (with b as (select 1) select * from b) …` — both the outer
        // `a` and the inner `b` are unique definition sites and both anchor.
        let raw = "with a as (with b as (select 1) select * from b) select * from a";
        let mut sm = sm_with(vec![cte_entry("a"), cte_entry("b")]);
        fill_raw_spans(&mut sm, raw);
        let a = sm.entries[0].raw.expect("outer CTE anchors");
        let b = sm.entries[1].raw.expect("inner CTE anchors");
        assert_eq!(
            raw_slice(raw, &a),
            "a as (with b as (select 1) select * from b)"
        );
        assert_eq!(raw_slice(raw, &b), "b as (select 1)");
    }

    #[test]
    fn cte_name_used_as_column_alias_anchors_the_def_site_only() {
        // `total` is both a CTE definition (`total as (`) and a column alias
        // (`as total`). As a CTE entry, only the `total AS (` definition site is
        // a match (the alias `as total` is `name`-after-`as`, not `name`-before-
        // `as (`), so the CTE anchors its DEFINITION extent.
        let raw = "with total as (select sum(x) as total from t) select * from total";
        let mut sm = sm_with(vec![cte_entry("total")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0].raw.expect("CTE def site anchors");
        assert_eq!(
            raw_slice(raw, &span),
            "total as (select sum(x) as total from t)"
        );
    }

    #[test]
    fn quoted_identifier_cte_is_honestly_omitted() {
        // A quoted-identifier CTE `"my cte"` has a space and quotes — its domain
        // node_id is `my cte`, which never occurs as a whole-word run in the
        // masked text the way the scanner matches (the quotes break the
        // identifier boundary). Honest omission (never a fabricated span).
        let raw = "with \"my cte\" as (select 1) select * from \"my cte\"";
        let mut sm = sm_with(vec![cte_entry("my cte")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "quoted-identifier CTE is honestly omitted in S1"
        );
    }

    #[test]
    fn keyword_whole_word_collision_at_def_layer_omits() {
        // A CTE literally named `select` (pathological but legal-if-quoted) must
        // not match the `select` keyword tokens scattered through the body. With
        // many `select` occurrences and none a `select AS (` definition site,
        // the entry is omitted (no false definition anchor).
        let raw = "with stg as (select 1) select * from stg";
        let mut sm = sm_with(vec![cte_entry("select")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "keyword collisions are not definition sites ⇒ omit"
        );
    }

    // ── council should-fix: balanced_close underflow robustness (#469) ───────

    #[test]
    fn balanced_close_underflow_at_depth_zero_returns_none_not_panic() {
        // Direct exercise of the checked_sub guard: starting the balance at a
        // position whose first live paren is a `)` (depth 0) must fail-closed to
        // None — never a usize-underflow panic. (`balanced_close` is normally
        // entered at a `(` opener, so depth-0 close is the defense-in-depth path;
        // this calls it at an index pointing into a `)`-leading region.)
        let masked = "select )";
        let close_idx = masked.find(')').unwrap();
        assert_eq!(
            balanced_close(masked, close_idx),
            None,
            "a `)` at depth 0 fails closed to None, never underflow-panics"
        );
    }

    #[test]
    fn unbalanced_extra_close_paren_still_anchors_the_balanced_def() {
        // A trailing stray `)` after a balanced def must not perturb the scan:
        // the def balances at its own matching `)`, the stray is never reached by
        // balanced_close, and the span anchors correctly (no panic, no omission).
        let raw = "with stg as (select 1)) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("the balanced def anchors; trailing stray ) is harmless");
        assert_eq!(raw_slice(raw, &span), "stg as (select 1)");
    }
}
