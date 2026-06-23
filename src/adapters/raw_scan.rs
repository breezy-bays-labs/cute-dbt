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
//! never a picked offset). The mask (offset-preserving) blanks EVERY non-SQL-
//! structural lexical region — every `{%…%}` / `{{…}}` / `{#…#}` Jinja region,
//! every SQL comment (`--`-to-EOL, `/* … */`), every SQL **string literal**
//! (`'…'`, doubled-`''` escape — plus the backslash `\'` escape for prefixed
//! escape-strings `E'…'`/`U&'…'`), every **quoted identifier** (`"…"`, doubled-`""`
//! escape), and every duckdb **dollar-quoted string** (`$tag$…$tag$` / `$$…$$`).
//! After masking, the match / definition-site / boundary layers see ONLY live SQL
//! structure, so a CTE/column name that appears solely INSIDE Jinja, a comment, a
//! string literal, a quoted identifier, or a dollar-quoted string can never be
//! matched or anchored (a name living in any of those is NOT a live definition —
//! anchoring to it would be a false claim, cute-dbt#469). The complete lexical-
//! region exhaustiveness argument is in `mask_regions`. Malformed/unbalanced
//! Jinja ⇒ the model emits NOTHING (fail-closed, mirroring `locate_raw_zones`).
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

use crate::adapters::cte_engine::{ByteIndex, TERMINAL_NODE_NAME};
use crate::adapters::render::{byte_span, find_close, find_expr_close};
use crate::domain::source_map::{SourceMap, SpanRole};
use crate::domain::span::SourceSpan;
use sqlparser::dialect::GenericDialect;
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace};

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
/// Operates on `mask_regions` output, where EVERY non-SQL-structural lexical
/// region is already blanked to spaces — Jinja, SQL comments, **string literals**
/// (`'…'`), **quoted identifiers** (`"…"`), and **dollar-quoted strings**. So a
/// stray `(`/`)` inside ANY of those (e.g. `select ')' as x`, the `dim_date`
/// golden's `-- new year's day`, a `/* don't ) */` block) carries no live paren
/// and cannot desync the scan. The only `(`/`)` bytes that survive masking are
/// **live SQL parens**, so a plain depth count over the masked text is sound —
/// `balanced_close` no longer needs its own string-literal-awareness (that layer
/// moved up into [`mask_regions`], the single place the escape logic lives).
fn balanced_close(masked: &str, open: usize) -> Option<usize> {
    let bytes = masked.as_bytes();
    let n = bytes.len();
    let mut depth = 0usize;
    let mut i = open;
    while i < n {
        match bytes[i] {
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
        }
        i += 1;
    }
    None
}

/// What the Jinja masker recognizes at a LIVE-state byte `i` — the
/// classification of a single forward step over the RAW text. `Region { end }`
/// is a `{…}` Jinja region spanning `[i, end)` to blank; `Skip` is a byte that
/// opens no Jinja region (advance one). `Malformed` is an unterminated Jinja
/// region ⇒ the whole mask fails closed.
enum Step {
    /// Blank `[i, end)` and resume at `end`.
    Region { end: usize },
    /// Not a Jinja opener at `i`; advance one byte.
    Skip,
    /// An unterminated Jinja region ⇒ `mask_jinja` returns `None` (fail-closed).
    Malformed,
}

/// Classify the opener at LIVE byte `i` into a single [`Step`] for the Jinja
/// pass. The ONLY region kind this pass recognizes is a `{`-led Jinja tag
/// (`{{…}}` / `{%…%}` / `{#…#}`); SQL strings/comments/dollar-quotes are left for
/// the tokenizer pass. A SQL string's `'…'` is intentionally NOT honored here, so
/// a `{{` that lives inside a SQL string is still seen by the Jinja scanner —
/// which is sound: a `{{` in data is masked-or-fail-closed (over-mask), never
/// under-masked. (The tokenizer pass then re-masks the SQL string over the same
/// bytes; double-masking a region is idempotent.)
fn classify_opener(bytes: &[u8], i: usize) -> Step {
    let n = bytes.len();
    if bytes[i] == b'{' && i + 1 < n {
        return match jinja_close(bytes, i) {
            JinjaOpen::Close(end) => Step::Region { end },
            JinjaOpen::NotAnOpener => Step::Skip,
            JinjaOpen::Unterminated => Step::Malformed,
        };
    }
    Step::Skip
}

/// The Jinja-opener outcome at a `{` lead byte: a resolved close offset, "not a
/// Jinja opener" (a bare `{X`), or an unterminated region (fail-closed).
enum JinjaOpen {
    Close(usize),
    NotAnOpener,
    Unterminated,
}

/// Classify a `{`-led Jinja opener at `i` and find its close via render.rs's
/// vetted scanners (so the raw-span path and the zone path agree on Jinja
/// boundaries, including the backslash string-escape). `{#…#}` comment, `{{…}}`
/// variable, `{%…%}` block; anything else is `NotAnOpener`.
fn jinja_close(bytes: &[u8], i: usize) -> JinjaOpen {
    let close = match bytes[i + 1] {
        b'#' => find_close(bytes, i + 2, b'#'),
        b'{' => find_expr_close(bytes, i + 2, b'}'),
        b'%' => find_expr_close(bytes, i + 2, b'%'),
        _ => return JinjaOpen::NotAnOpener,
    };
    match close {
        Some(end) => JinjaOpen::Close(end),
        None => JinjaOpen::Unterminated,
    }
}

/// Return a copy of `raw` with EVERY non-SQL-structural lexical region replaced
/// by spaces, BYTE-FOR-BYTE (offsets preserved, so a span over the masked text
/// indexes the same region of the original `raw`). Returns `None` on a
/// malformed/unterminated Jinja region OR a `Tokenizer` error (fail-closed —
/// mirrors `scan_block_tags`).
///
/// Masking (not deletion) is the load-bearing honesty primitive: a CTE/column
/// name that only appears INSIDE one of these regions is blanked out, so it can
/// never be matched as a verbatim raw anchor OR a `name AS (` definition site — a
/// name in commented-out code, a string literal, or a templated region is not a
/// live definition (never-a-false-claim, cute-dbt#469).
///
/// ## Two passes (Jinja, then SQL-lexer)
///
/// 1. **Jinja pass** ([`mask_jinja`]) — blanks every `{{…}}` / `{%…%}` / `{#…#}`
///    region with render.rs's vetted close-finders (`find_close` /
///    `find_expr_close`), the SAME scanners the zone path uses, so both agree on
///    Jinja boundaries (including the backslash string-escape) on the SAME model.
///    sqlparser does NOT understand Jinja — masking it first hides every
///    template fragment from the tokenizer. An unterminated Jinja region ⇒ the
///    WHOLE mask fails closed (`None`).
/// 2. **SQL-lexer pass** ([`mask_sql_tokens`]) — tokenizes the Jinja-masked text
///    with `sqlparser::tokenizer::Tokenizer` under the `GenericDialect` (the SAME
///    dialect `cte_engine` parses with, so raw and compiled never diverge on SQL
///    lexing), and blanks the byte span of every STRING / COMMENT / DOLLAR-QUOTE
///    token. A `TokenizerError` ⇒ the WHOLE text is blanked (maximal over-mask =
///    fail closed; nothing can leak).
///
/// ## SOUNDNESS INVARIANT (the never-a-false-claim keystone)
///
/// `mask_regions` never leaves a name-bearing non-SQL-structural region live; on
/// uncertainty it OVER-masks (honest omission), never under-masks (a false
/// anchor). The masker's only error direction is masking too much:
///
/// - The SQL-lexer pass blanks EVERY string / comment / dollar-quote token the
///   tokenizer recognizes, with the tokenizer's own escape handling
///   (`''`/`""`/`\'`, dollar-tags, prefixed `E'…'`/`U&'…'`, `N'…'`, `X'…'`,
///   `B'…'`, …). The single home of the SQL string-escape logic is now the
///   tokenizer itself (no hand-rolled escape code to drift).
/// - A **quoted identifier** (`"my col"`, `` `my col` ``, `")"`) is `Token::Word`
///   with a `quote_style`. Though technically a live name, it is BLANKED in the
///   over-mask (honest) direction — both for paren-balance soundness (a `)` inside
///   `")"` is not a structural paren) and behavior-parity with the old masker
///   (such names are already honestly omitted at the fill layer). See
///   [`is_maskable_token`] for the full reasoning. An UNQUOTED `Word` (a bare
///   identifier) is the only `Word` form left live — the only one carrying a
///   matchable name.
/// - A `TokenizerError` (e.g. an unterminated string, a malformed `U&'…\\…'`
///   unicode escape) ⇒ the WHOLE text is blanked = the maximal over-mask = sound.
/// - A malformed `{`-led Jinja region ⇒ the WHOLE mask fails closed (`None` ⇒ the
///   model emits nothing) = the maximal over-mask = sound.
/// - A lone `$` / `$1` positional param tokenizes as a `Placeholder` (not a
///   dollar-quoted string), so it is left live — it carries no identifier bytes
///   past itself and cannot hide a name, so leaving it live is sound.
///
/// Because every uncertain case extends the masked region (or fails the whole
/// model closed) and the only live-left forms (live SQL structure, bare
/// identifiers, lone `$`) provably carry no hidden name, under-masking is
/// impossible — the soundness invariant holds.
///
/// ## Lexical-region exhaustiveness (the regions a name can hide in)
///
/// A name can falsely hide in EXACTLY these region kinds in the SQL + Jinja
/// grammar a `raw_code` model body draws from. Each is masked, so after both
/// passes the only bytes a name can match against are LIVE SQL structure:
///
/// | Region kind | Token / opener | Masked by |
/// |---|---|---|
/// | Jinja variable tag | `{{` … `}}` | `mask_jinja` (`render::find_expr_close`) |
/// | Jinja block tag | `{%` … `%}` | `mask_jinja` (`render::find_expr_close`) |
/// | Jinja comment | `{#` … `#}` | `mask_jinja` (`render::find_close`) |
/// | SQL line comment | `Whitespace::SingleLineComment` | `mask_sql_tokens` |
/// | SQL block comment | `Whitespace::MultiLineComment` | `mask_sql_tokens` |
/// | SQL string literal | `SingleQuotedString`, `EscapedStringLiteral`, `NationalStringLiteral`, `HexStringLiteral`, byte/raw/triple variants, … | `mask_sql_tokens` |
/// | Unicode string literal | `UnicodeStringLiteral` | `mask_sql_tokens` |
/// | Dollar-quoted string | `DollarQuotedString` | `mask_sql_tokens` |
/// | SQL quoted identifier | `Word { quote_style: Some(_) }` | `mask_sql_tokens` (over-mask; see [`is_maskable_token`]) |
///
/// Note `DoubleQuotedString` is enumerated too: under `GenericDialect` a `"…"`
/// tokenizes as a quoted IDENTIFIER (`Word`), so `DoubleQuotedString` does not
/// arise for `"`; it is included for completeness/robustness against any dialect
/// configuration where `"…"` is a string literal (masking it is sound either
/// way).
pub(crate) fn mask_regions(raw: &str) -> Option<String> {
    // Pass 1 — Jinja (sqlparser does not understand it). Fail-closed on a
    // malformed region (the whole model emits nothing).
    let jinja_masked = mask_jinja(raw)?;
    // Pass 2 — SQL strings/comments/dollar-quotes via the shared tokenizer.
    Some(mask_sql_tokens(&jinja_masked))
}

/// Pass 1: blank every `{{…}}` / `{%…%}` / `{#…#}` Jinja region of `raw`,
/// offset-preserving. Returns `None` on a malformed/unterminated Jinja region
/// (fail-closed). render.rs's vetted close-finders are reused so the raw-span
/// path and the zone path agree on Jinja boundaries on the SAME model.
fn mask_jinja(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let n = bytes.len();
    // Start from the original bytes; blank every Jinja region in place. We only
    // ever overwrite ASCII bytes with ASCII spaces, so the result stays valid
    // UTF-8 and every byte offset is preserved.
    let mut out = raw.as_bytes().to_vec();
    let mut i = 0usize;
    while i < n {
        match classify_opener(bytes, i) {
            Step::Region { end } => {
                blank(&mut out, i, end);
                i = end;
            }
            Step::Skip => i += 1,
            // Unterminated Jinja ⇒ the whole model emits nothing (fail-closed).
            Step::Malformed => return None,
        }
    }
    String::from_utf8(out).ok()
}

/// Pass 2: blank the byte span of every STRING / COMMENT / DOLLAR-QUOTE token of
/// `jinja_masked` via `sqlparser::tokenizer::Tokenizer` under the SAME
/// `GenericDialect` `cte_engine` parses with (so raw & compiled never diverge on
/// SQL lexing). On a `TokenizerError` the WHOLE text is blanked (fail-closed —
/// nothing can leak). Offset-preserving: the tokenizer's `Location` line/col
/// endpoints are converted to byte offsets via the shared [`ByteIndex`].
///
/// A quoted identifier (`Token::Word { quote_style: Some(_) }`, e.g. `"my col"`,
/// `` `c` ``, `")"`) is ALSO blanked here — see [`is_maskable_token`] for the
/// soundness reasoning (behavior-parity with the old hand-rolled masker + the
/// paren-balance invariant).
fn mask_sql_tokens(jinja_masked: &str) -> String {
    let Ok(tokens) = Tokenizer::new(&GenericDialect {}, jinja_masked).tokenize_with_location()
    else {
        // FAIL CLOSED — a tokenizer error (unterminated string, malformed unicode
        // escape, …) blanks everything so no interior name can ever leak.
        return " ".repeat(jinja_masked.len());
    };
    let bytes = jinja_masked.as_bytes();
    let mut out = bytes.to_vec();
    let index = ByteIndex::new(jinja_masked);
    for tok in &tokens {
        if !is_maskable_token(&tok.token) {
            continue;
        }
        let start = index.byte_of(jinja_masked, tok.span.start);
        let mut end = index.byte_of(jinja_masked, tok.span.end);
        // A `SingleLineComment` token's span INCLUDES its terminating newline.
        // Blanking that `\n` would shift the line/col of every downstream span
        // (`byte_span` counts `\n` over the masked text), making the emitted
        // line/col disagree with the TRUE raw source — a location regression
        // even though the byte offset stays correct. Preserve a single trailing
        // `\n` (the line comment's own terminator) so masked-text line structure
        // matches the raw exactly. Only line comments end in `\n`; every other
        // maskable token blanks its full span. (The `\n`'s liveness is harmless:
        // a newline carries no name and no paren.)
        if matches!(
            tok.token,
            Token::Whitespace(Whitespace::SingleLineComment { .. })
        ) && end > start
            && bytes[end - 1] == b'\n'
        {
            end -= 1;
        }
        blank(&mut out, start, end);
    }
    // We only replaced bytes with ASCII spaces over token spans, so the result is
    // valid UTF-8. Fall back to the input on the impossible failure (never lossy).
    String::from_utf8(out).unwrap_or_else(|_| jinja_masked.to_owned())
}

/// Whether `token` is a non-SQL-structural region whose byte span must be
/// blanked: every STRING literal form, every COMMENT, the DOLLAR-QUOTE string,
/// and every QUOTED IDENTIFIER (`Word { quote_style: Some(_) }`). Enumerated
/// exhaustively over the sqlparser 0.62 `Token` variants so a new string form
/// cannot silently slip through unmasked.
///
/// ## Why quoted identifiers are masked (soundness)
///
/// A quoted identifier (`"my col"`, `` `c` ``, `")"`) is technically a LIVE name,
/// but it is blanked here for two reasons, both biased toward the honest
/// (over-mask) direction:
///
/// 1. **Paren-balance soundness.** `cte_raw_span` finds a CTE body's `( … )`
///    extent by balancing parens over the masked text ([`balanced_close`]). A
///    `)` *inside* a quoted identifier (`select ")" as x`) is NOT a structural
///    paren — leaving it live would let it close the balance early and emit a
///    TRUNCATED span (`stg as (select ")`), a FALSE claim about where the CTE
///    ends. Blanking the whole quoted-identifier token removes its interior
///    parens from the structural count, so the balance stays sound.
/// 2. **Behavior parity.** The old hand-rolled masker blanked `"…"` quoted
///    identifiers too, and quoted-identifier CTE/column names are already
///    honestly OMITTED at the fill layer (their domain `node_id` is the unquoted
///    content — e.g. `my col` — whose space breaks the whole-word match the
///    scanner uses; see `quoted_identifier_cte_is_honestly_omitted`). So masking
///    them changes nothing observable except keeping the masker
///    behavior-identical to the pre-refactor path (hence goldens byte-stable).
///
/// Masking a live quoted identifier is over-masking = the honest failure
/// direction; it can only ever DROP a (already-dropped) anchor, never fabricate
/// one. An UNQUOTED `Word` (a real bare identifier) is left LIVE — that is the
/// only `Word` form that carries a matchable name.
fn is_maskable_token(token: &Token) -> bool {
    if let Token::Word(w) = token {
        // A quoted identifier is blanked (see doc); a bare identifier is live.
        return w.quote_style.is_some();
    }
    matches!(
        token,
        Token::SingleQuotedString(_)
            | Token::DoubleQuotedString(_)
            | Token::TripleSingleQuotedString(_)
            | Token::TripleDoubleQuotedString(_)
            | Token::DollarQuotedString(_)
            | Token::SingleQuotedByteStringLiteral(_)
            | Token::DoubleQuotedByteStringLiteral(_)
            | Token::TripleSingleQuotedByteStringLiteral(_)
            | Token::TripleDoubleQuotedByteStringLiteral(_)
            | Token::SingleQuotedRawStringLiteral(_)
            | Token::DoubleQuotedRawStringLiteral(_)
            | Token::TripleSingleQuotedRawStringLiteral(_)
            | Token::TripleDoubleQuotedRawStringLiteral(_)
            | Token::NationalStringLiteral(_)
            | Token::QuoteDelimitedStringLiteral(_)
            | Token::NationalQuoteDelimitedStringLiteral(_)
            | Token::EscapedStringLiteral(_)
            | Token::UnicodeStringLiteral(_)
            | Token::HexStringLiteral(_)
            | Token::Whitespace(
                Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_)
            )
    )
}

/// Blank `out[from..to]` with ASCII spaces (offset-preserving). **Total**: a
/// malformed span — `to` past the end, or an inverted `from >= to` — blanks
/// nothing rather than panicking. The masker must fail closed (over-mask or
/// omit), never crash the render on a manifest-derived span. Today's callers
/// pass codepoint-aligned, ordered, in-bounds offsets (`ByteIndex::byte_of`
/// clamps to `len`; token spans are ordered), so the guards are belt-and-braces
/// against any future caller that isn't — not a reachable path now.
fn blank(out: &mut [u8], from: usize, to: usize) {
    let end = to.min(out.len());
    if from >= end {
        return;
    }
    for b in &mut out[from..end] {
        *b = b' ';
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::source_map::{SourceMapEntry, ZoneKind};

    #[test]
    fn blank_is_total_on_malformed_spans() {
        // The masker must never panic on a malformed span — it fails closed.
        // Valid range blanks exactly that range.
        let mut valid = b"abcdef".to_vec();
        blank(&mut valid, 2, 4);
        assert_eq!(&valid, b"ab  ef");
        // `to` past the end clamps to len (blanks the tail), never out-of-bounds.
        let mut clamped = b"abcdef".to_vec();
        blank(&mut clamped, 2, 999);
        assert_eq!(&clamped, b"ab    ");
        // Empty range (from == to) is a no-op.
        let mut empty = b"abcdef".to_vec();
        blank(&mut empty, 3, 3);
        assert_eq!(&empty, b"abcdef");
        // Inverted range (from > to) is a no-op, NOT a panic.
        let mut inverted = b"abcdef".to_vec();
        blank(&mut inverted, 4, 2);
        assert_eq!(&inverted, b"abcdef");
        // `from` past the end is a no-op, NOT a panic.
        let mut past_end = b"abcdef".to_vec();
        blank(&mut past_end, 10, 999);
        assert_eq!(&past_end, b"abcdef");
    }

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
        // The line comment's own terminating `\n` is PRESERVED (not blanked):
        // the tokenizer's `SingleLineComment` span includes it, but masking it
        // would shift every downstream span's line/col away from the true raw
        // source, so the trailing `\n` is kept live (it carries no name).
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

    #[test]
    fn mask_blanks_sql_string_literal_offset_preserving() {
        // A live SQL string literal is blanked; surrounding live SQL survives.
        let raw = "select 'customer_id desc' as d from raw";
        let masked = mask_regions(raw).expect("well-formed");
        assert_eq!(masked.len(), raw.len(), "offsets preserved");
        assert!(masked.starts_with("select "), "live SQL before survives");
        assert!(
            masked.ends_with(" as d from raw"),
            "live SQL after survives"
        );
        assert!(
            !masked.contains("customer_id"),
            "name inside the string literal is blanked"
        );
    }

    #[test]
    fn mask_blanks_double_quoted_identifier() {
        let raw = "select x as \"my col\" from raw";
        let masked = mask_regions(raw).expect("well-formed");
        assert_eq!(masked.len(), raw.len(), "offsets preserved");
        assert!(!masked.contains("my col"), "quoted identifier is blanked");
        assert!(masked.ends_with(" from raw"), "live SQL after survives");
    }

    #[test]
    fn mask_honours_sql_doubled_quote_escape_in_string() {
        // `''` inside a `'…'` string is an escaped quote, NOT a close-then-reopen,
        // so the whole `'a''b'` literal is one region and the trailing live SQL is
        // not swallowed.
        let raw = "select 'a''b' as v, customer_id from raw";
        let masked = mask_regions(raw).expect("well-formed");
        assert!(
            !masked.contains("'a''b'") && !masked.contains("a''b"),
            "the doubled-quote string is fully blanked"
        );
        assert!(
            masked.contains("customer_id"),
            "live SQL after the escaped string survives (no early close)"
        );
    }

    #[test]
    fn mask_honours_doubled_quote_escape_in_identifier() {
        // `""` inside a `"…"` identifier is an escaped quote; the live tail must
        // survive (no early close on the doubled quote).
        let raw = "select x as \"a\"\"b\", customer_id from raw";
        let masked = mask_regions(raw).expect("well-formed");
        assert!(
            !masked.contains("a\"\"b"),
            "the doubled-quote ident is blanked"
        );
        assert!(
            masked.contains("customer_id"),
            "live SQL after the escaped identifier survives"
        );
    }

    #[test]
    fn mask_precedence_brace_inside_string_is_not_a_jinja_opener() {
        // A `{{` inside a SQL string must NOT be read as a Jinja variable tag —
        // the string region is claimed first (single forward pass). Without this,
        // an unterminated-looking `{{` in data could fail-close a valid model.
        let raw = "select '{{ not jinja }}' as v from raw";
        let masked = mask_regions(raw).expect("string claims the braces, not Jinja");
        assert_eq!(masked.len(), raw.len(), "offsets preserved");
        assert!(
            masked.ends_with(" as v from raw"),
            "live SQL after survives"
        );
    }

    #[test]
    fn mask_precedence_quote_inside_jinja_is_not_a_sql_string() {
        // A `'` inside a `{% … %}` tag is consumed by the Jinja region first; it
        // must NOT open a SQL string that swallows the live tail.
        let raw = "{% set x = 'a' %} select customer_id from raw";
        let masked = mask_regions(raw).expect("Jinja claims the quote, not SQL");
        assert!(!masked.contains("set"), "the block tag is blanked");
        assert!(
            masked.contains("customer_id"),
            "live SQL after the tag survives (the tag's quote did not open a string)"
        );
    }

    #[test]
    fn mask_blanks_dollar_quoted_string() {
        // duckdb `$tag$…$tag$` and `$$…$$` constants are blanked.
        let raw = "select $tag$customer_id$tag$ as a, $$other_id$$ as b from raw";
        let masked = mask_regions(raw).expect("well-formed");
        assert_eq!(masked.len(), raw.len(), "offsets preserved");
        assert!(!masked.contains("customer_id"), "$tag$ body blanked");
        assert!(!masked.contains("other_id"), "$$ body blanked");
        assert!(masked.ends_with(" from raw"), "live SQL after survives");
    }

    #[test]
    fn mask_lone_dollar_is_left_live() {
        // A lone `$` / positional `$1` is NOT a dollar-quote opener and is left
        // live (it cannot hide a name, so masking it would be wrong-but-harmless;
        // leaving it live is the honest no-op).
        let raw = "select a $ b, customer_id from raw";
        let masked = mask_regions(raw).expect("well-formed");
        assert!(
            masked.contains("customer_id"),
            "live SQL survives around a lone $"
        );
        assert!(masked.contains(" $ "), "the lone $ is left in place");
    }

    // ── prefixed escape-strings (E'…' / U&'…') — the SOUND over-mask (#469) ──

    #[test]
    fn mask_e_string_backslash_escape_does_not_close_early() {
        // THE PROBE (#469 blocker 1): a DuckDB escape-string `E'…'` uses BACKSLASH
        // escapes (`\'`). The naive doubled-quote-only scanner would close the
        // string at the `\'` and leak `customer_id` LIVE → a false anchor. With the
        // prefixed-escape rule the `\'` stays in-string, so the whole `E'pre \'
        // customer_id'` literal is one masked region and `customer_id` is OMITTED.
        let raw = "select E'pre \\' customer_id' as x from t";
        let masked = mask_regions(raw).expect("well-formed");
        assert_eq!(masked.len(), raw.len(), "offsets preserved");
        assert!(
            !masked.contains("customer_id"),
            "the E'…' escape-string is fully masked; the name does not leak live"
        );
        assert!(masked.ends_with(" as x from t"), "live SQL after survives");
    }

    #[test]
    fn e_string_name_omits_at_the_fill_layer() {
        // End-to-end: a column whose only would-be occurrence falls inside an
        // `E'…'` escape-string (after a `\'`) must be OMITTED — never anchored into
        // the string. This is the false-anchor the probe guards against.
        let raw = "select E'pre \\' customer_id' as x from t";
        let mut sm = sm_with(vec![column_entry("(final select)", "customer_id")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a name inside an E'…' escape-string is masked ⇒ omit (no false anchor)"
        );
    }

    #[test]
    fn mask_lowercase_e_string_backslash_escape() {
        // The prefix is case-insensitive at the lexer: `e'…'` behaves like `E'…'`.
        let raw = "select e'a \\' customer_id' as x from t";
        let masked = mask_regions(raw).expect("well-formed");
        assert!(
            !masked.contains("customer_id"),
            "e'…' escape-string masked; name does not leak"
        );
    }

    #[test]
    fn mask_unicode_escape_string_name_omits() {
        // A `U&'…'` Unicode-escape-string: the `&'` prefix marks it, so it honors
        // the backslash escape too. A name inside it ⇒ OMIT.
        let raw = "select U&'pre \\' customer_id' as x from t";
        let masked = mask_regions(raw).expect("well-formed");
        assert_eq!(masked.len(), raw.len(), "offsets preserved");
        assert!(
            !masked.contains("customer_id"),
            "the U&'…' Unicode-escape-string is masked; name does not leak"
        );

        let mut sm = sm_with(vec![column_entry("(final select)", "customer_id")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a name inside a U&'…' escape-string is masked ⇒ omit"
        );
    }

    #[test]
    fn bare_string_with_literal_backslash_still_resolves_following_live_name() {
        // A BARE `'…'` (no prefix) does NOT honor the backslash escape — standard
        // SQL treats `\` as a literal byte. So `'a\'` is the complete two-char
        // string `a\`, and the following live `customer_id` is correctly anchored.
        // (Over-mask tradeoff: NONE here — the bare string closes at its real
        // first-unescaped quote, exactly as standard SQL specifies; the live name
        // after it is honestly resolved.)
        let raw = "select 'a\\' as lit, customer_id from t";
        let masked = mask_regions(raw).expect("well-formed");
        assert!(
            masked.contains("customer_id"),
            "live name after a bare backslash-bearing string survives"
        );
        let mut sm = sm_with(vec![column_entry("(final select)", "customer_id")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("the live customer_id is the unique anchor");
        assert_eq!(raw_slice(raw, &span), "customer_id");
    }

    #[test]
    fn double_quoted_identifier_does_not_honor_backslash() {
        // A `"…"` quoted identifier is never a prefixed escape-string; it honors
        // ONLY the doubled-`""` escape. A `\"` inside it is a literal backslash +
        // closing quote — the identifier closes at the `"`. (DuckDB does not give
        // `"…"` C-style escapes.) The trailing live SQL must survive.
        let raw = "select x as \"a\\\" b from t";
        let masked = mask_regions(raw).expect("well-formed");
        // The identifier closes at the first un-doubled `"` (after `a\`), so the
        // live tail (` b from t`) survives — backslash is NOT an escape here.
        assert!(
            masked.contains("from t"),
            "live SQL after the ident survives"
        );
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

    // ── string-literal-anchored span is a FALSE CLAIM ⇒ OMIT (cute-dbt#469) ──

    #[test]
    fn column_only_inside_string_literal_omits() {
        // THE PROBE (design §4 Ask 1 — "collides with a string literal" = OMIT):
        // the column's only would-be live occurrence falls INSIDE a SQL string
        // literal (`'customer_id desc'`); its real value is templated by
        // `{{ quote('customer_id') }}`. After masking, BOTH occurrences are gone
        // (Jinja + string), so the name is unanchored ⇒ omit. Anchoring into the
        // string would be a false claim.
        let raw = "select {{ quote('customer_id') }}, 'customer_id desc' as d from raw";
        let mut sm = sm_with(vec![column_entry("(final select)", "customer_id")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a column whose only occurrence is inside a string literal ⇒ omit"
        );
    }

    #[test]
    fn cte_def_only_inside_string_literal_omits() {
        // The ONLY `stg as (` text lives inside a SQL string literal (e.g. a
        // dynamic-SQL string being assembled). Masking blanks it ⇒ no live
        // definition site ⇒ omit.
        let raw = "select 'with stg as (select 1)' as generated_sql from raw";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a CTE def inside a string literal is not a live definition ⇒ omit"
        );
    }

    #[test]
    fn name_around_doubled_quote_escape_inside_string_omits() {
        // The name sits inside a string that ALSO contains the `''` doubled-quote
        // escape — the whole `'…''…'` literal is one masked region, so the name is
        // blanked and omitted (the escape must not split the string and leak the
        // name as live).
        let raw = "select 'a customer_id ''quoted'' tail' as note from raw";
        let mut sm = sm_with(vec![column_entry("(final select)", "customer_id")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a name inside a string with a '' escape is masked ⇒ omit"
        );
    }

    #[test]
    fn column_only_inside_quoted_identifier_omits() {
        // The name appears only as a double-quoted identifier string `"my col"`;
        // masking blanks it ⇒ omit (the quotes break the live identifier run).
        let raw = "select x as \"my col\" from raw";
        let mut sm = sm_with(vec![column_entry("(final select)", "my col")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a name only inside a quoted identifier is masked ⇒ omit"
        );
    }

    #[test]
    fn column_live_once_outside_any_string_still_anchors() {
        // Regression guard: a column that legitimately appears ONCE outside any
        // string (the same name also occurring inside a masked string region)
        // still anchors to the LIVE occurrence — masking strings must not break
        // honest, well-anchored columns.
        let raw = "select customer_id from raw where note = 'customer_id is legacy'";
        let mut sm = sm_with(vec![column_entry("(final select)", "customer_id")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("the live occurrence outside the string is the unique anchor");
        assert_eq!(raw_slice(raw, &span), "customer_id");
        assert!(
            (span.start.byte as usize) < raw.find("where").unwrap(),
            "anchored to the live occurrence, not the one inside the string"
        );
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

    // ── tokenizer-pass soundness contracts (cute-dbt#473) ────────────────────

    #[test]
    fn tokenizer_error_fails_closed_blanks_everything() {
        // A `U&'…\\…'` Unicode escape-string is a TokenizerError under
        // GenericDialect (the `\'` is not a valid hex escape). The tokenizer pass
        // must FAIL CLOSED — blank the ENTIRE text so no interior name can ever
        // leak as a false anchor. (Empirically verified: this exact input raises
        // `TokenizerError` rather than a maskable token span.)
        let raw = "select U&'pre \\' customer_id' as x from t";
        let masked = mask_regions(raw).expect("Jinja pass is clean ⇒ Some(...)");
        assert_eq!(masked.len(), raw.len(), "offsets preserved");
        assert!(
            masked.trim().is_empty(),
            "a tokenizer error blanks the WHOLE text (fail-closed)"
        );
        assert!(
            !masked.contains("customer_id") && !masked.contains("from"),
            "nothing survives a fail-closed tokenizer error"
        );
    }

    #[test]
    fn tokenizer_error_omits_every_name_at_fill_layer() {
        // End-to-end: when the tokenizer fails closed, EVERY name is omitted — a
        // live `stg` definition included. Fail-closed never fabricates an anchor.
        let raw = "with stg as (select U&'\\' x) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        assert!(
            sm.entries[0].raw.is_none(),
            "a tokenizer error blanks everything ⇒ no anchor (fail-closed)"
        );
    }

    #[test]
    fn paren_inside_quoted_identifier_does_not_truncate_cte_span() {
        // THE PROBE (cute-dbt#473): a `)` inside a quoted IDENTIFIER (`")"`, a
        // column literally named `)`) is NOT a structural paren. The tokenizer
        // surfaces `")"` as a quoted-identifier `Word`, which the masker blanks —
        // so its interior `)` is removed from the paren balance and the CTE span
        // covers its FULL extent (never the truncated `stg as (select ")`).
        let raw = "with stg as (select \")\" as x) select * from stg";
        let mut sm = sm_with(vec![cte_entry("stg")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0]
            .raw
            .expect("paren inside a quoted identifier is not a real close");
        assert_eq!(raw_slice(raw, &span), "stg as (select \")\" as x)");
    }

    #[test]
    fn unquoted_word_identifier_stays_live_and_anchors() {
        // A BARE (unquoted) identifier `Word` is the ONE `Word` form left live —
        // it carries the matchable name. A column uniquely named once anchors.
        let raw = "select net_amount from raw";
        let mut sm = sm_with(vec![column_entry("(final select)", "net_amount")]);
        fill_raw_spans(&mut sm, raw);
        let span = sm.entries[0].raw.expect("bare identifier anchors");
        assert_eq!(raw_slice(raw, &span), "net_amount");
    }

    #[test]
    fn dialect_matches_cte_engine_generic() {
        // GUARD: the tokenizer pass must use the SAME dialect the CTE engine
        // parses with, so raw & compiled never diverge on SQL lexing. A `$$…$$`
        // dollar-quoted constant (a GenericDialect-recognized form) must be masked
        // here exactly as cte_engine would lex it.
        let raw = "select $$customer_id$$ as a, net from raw";
        let masked = mask_regions(raw).expect("well-formed");
        assert!(!masked.contains("customer_id"), "dollar-quote masked");
        assert!(masked.contains("net"), "live SQL survives");
    }
}
