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

/// The masking classification of a single SQL [`Token`]: either its byte span
/// is blanked ([`Mask`](MaskClass::Mask)) or it is left untouched in the live
/// text ([`Live`](MaskClass::Live)). A two-state result so [`classify_token`]
/// can be written as a TOTAL match — the compiler enforces exhaustiveness.
///
/// [`Mask`](MaskClass::Mask): every STRING literal form, every COMMENT, and
/// every QUOTED IDENTIFIER (`Word { quote_style: Some(_) }`). [`Live`](MaskClass::Live):
/// a bare `Word`, every operator/number/punctuation/placeholder, and
/// whitespace that is not a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaskClass {
    /// The token's byte span is blanked (non-SQL-structural name-bearing region).
    Mask,
    /// The token is left live (SQL structure / bare identifier — carries no
    /// hidden name).
    Live,
}

/// Whether `token`'s byte span must be blanked. Thin boolean adapter over
/// [`classify_token`]; the exhaustiveness guarantee lives in that match.
fn is_maskable_token(token: &Token) -> bool {
    matches!(classify_token(token), MaskClass::Mask)
}

/// Classify every sqlparser [`Token`] as [`Mask`](MaskClass::Mask) (blank its
/// byte span) or [`Live`](MaskClass::Live) (leave it in the live text).
///
/// ## Compile-time exhaustiveness (the dep-bump under-mask guard — cute-dbt#474)
///
/// This is a TOTAL `match` over `sqlparser::tokenizer::Token` with **NO
/// wildcard arm**. Every structural / number / punctuation / operator token is
/// classified [`Live`](MaskClass::Live) *explicitly*, never wildcard-defaulted.
/// The consequence is the whole point of the refactor: a future sqlparser bump
/// that adds a new `Token` variant — say a new string-literal form — makes this
/// match non-exhaustive and FAILS THE BUILD, forcing the maintainer to classify
/// the new variant by hand. The old `matches!(…)` with its implicit `_ => false`
/// wildcard would have silently defaulted such a variant to [`Live`](MaskClass::Live),
/// letting its interior leak as a live span — a FALSE ANCHOR that violates
/// never-a-false-claim. With this match, that defaulting is a compile error, not
/// a silent soundness hole. (The nested `Whitespace` match is total for the same
/// reason — a new `Whitespace` variant also forces a classify decision.)
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
// The length is INTRINSIC and load-bearing: one explicit arm per `Token` variant
// (no wildcard) is precisely the compile-time exhaustiveness guard this refactor
// exists to install. Splitting the match to satisfy `too_many_lines` would
// reintroduce a catch-all on one side and defeat the guard, so we allow the lint
// at this single site rather than weaken the invariant.
#[allow(clippy::too_many_lines)]
fn classify_token(token: &Token) -> MaskClass {
    use MaskClass::{Live, Mask};
    match token {
        // A quoted identifier is blanked (paren-balance soundness + behavior
        // parity, see doc); a bare identifier is the only live `Word`.
        Token::Word(w) => {
            if w.quote_style.is_some() {
                Mask
            } else {
                Live
            }
        }

        // ── Mask: every string-literal form, every comment ──────────────────
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
        | Token::HexStringLiteral(_) => Mask,

        // Whitespace splits: comments are name-bearing regions (Mask); real
        // whitespace is live SQL structure. Total match — a new `Whitespace`
        // variant forces a classify decision here too.
        Token::Whitespace(ws) => match ws {
            Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_) => Mask,
            Whitespace::Space | Whitespace::Newline | Whitespace::Tab => Live,
        },

        // ── Live: numbers, the EOF/Char lexer atoms, and every operator /
        // punctuation token. NONE carries a hidden interior name; each is
        // listed EXPLICITLY (no wildcard) so a new variant can't default here.
        Token::EOF
        | Token::Number(_, _)
        | Token::Char(_)
        | Token::Comma
        | Token::DoubleEq
        | Token::Eq
        | Token::Neq
        | Token::Lt
        | Token::Gt
        | Token::LtEq
        | Token::GtEq
        | Token::Spaceship
        | Token::Plus
        | Token::Minus
        | Token::Mul
        | Token::Div
        | Token::DuckIntDiv
        | Token::Mod
        | Token::StringConcat
        | Token::LParen
        | Token::RParen
        | Token::Period
        | Token::Colon
        | Token::DoubleColon
        | Token::Assignment
        | Token::SemiColon
        | Token::Backslash
        | Token::LBracket
        | Token::RBracket
        | Token::Ampersand
        | Token::Pipe
        | Token::Caret
        | Token::LBrace
        | Token::RBrace
        | Token::RArrow
        | Token::Sharp
        | Token::DoubleSharp
        | Token::Tilde
        | Token::TildeAsterisk
        | Token::ExclamationMarkTilde
        | Token::ExclamationMarkTildeAsterisk
        | Token::DoubleTilde
        | Token::DoubleTildeAsterisk
        | Token::ExclamationMarkDoubleTilde
        | Token::ExclamationMarkDoubleTildeAsterisk
        | Token::ShiftLeft
        | Token::ShiftRight
        | Token::Overlap
        | Token::ExclamationMark
        | Token::DoubleExclamationMark
        | Token::AtSign
        | Token::CaretAt
        | Token::PGSquareRoot
        | Token::PGCubeRoot
        | Token::Placeholder(_)
        | Token::Arrow
        | Token::LongArrow
        | Token::HashArrow
        | Token::AtDashAt
        | Token::QuestionMarkDash
        | Token::AmpersandLeftAngleBracket
        | Token::AmpersandRightAngleBracket
        | Token::AmpersandLeftAngleBracketVerticalBar
        | Token::VerticalBarAmpersandRightAngleBracket
        | Token::TwoWayArrow
        | Token::LeftAngleBracketCaret
        | Token::RightAngleBracketCaret
        | Token::QuestionMarkSharp
        | Token::QuestionMarkDashVerticalBar
        | Token::QuestionMarkDoubleVerticalBar
        | Token::TildeEqual
        | Token::ShiftLeftVerticalBar
        | Token::VerticalBarShiftRight
        | Token::VerticalBarRightAngleBracket
        | Token::HashLongArrow
        | Token::AtArrow
        | Token::ArrowAt
        | Token::HashMinus
        | Token::AtQuestion
        | Token::AtAt
        | Token::Question
        | Token::QuestionAnd
        | Token::QuestionPipe
        | Token::CustomBinaryOperator(_) => Live,
    }
}

/// One lexically-explicit, unconditional raw CTE-to-CTE dependency
/// (cute-dbt#471, S3): the `to` CTE's body names the `from` CTE after a bare
/// `from`/`join` keyword in **masked** (un-templated) text. `(from, to)` are
/// raw-CTE node ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawEdge {
    /// The referenced sibling CTE (upstream).
    pub from: String,
    /// The CTE whose body names `from` (downstream).
    pub to: String,
}

/// Scan the model's `raw` for **lexically-explicit, unconditional**
/// CTE-to-CTE dependencies (cute-dbt#471, S3) — a bare `from <sibling>` /
/// `join <sibling>` inside one verbatim CTE's body, where `<sibling>` is ANOTHER
/// verbatim raw CTE of the same model. `raw_cte_spans` are the uniquely-anchored
/// raw CTE bodies (`SourceMap::raw_node_spans`), the ONLY sound raw nodes a
/// CTE-to-CTE edge can connect.
///
/// NEVER-A-FALSE-CLAIM (honesty principle 3): the scan runs over [`mask_regions`]
/// output, where every `{{ ref() }}` / `{% for %}` / macro call / string /
/// comment is already blanked to spaces. So a dependency mediated by any of those
/// leaves NO live `from <name>` token and produces NO edge — the pane shows it as
/// unresolved, never a guessed edge. Fail-closed: malformed Jinja ⇒ `mask_regions`
/// returns `None` and the scan emits NOTHING.
///
/// CONTROL-BLOCK CONDITIONALITY (cute-dbt#471, the honesty fix): masking blanks
/// Jinja control TAGS (`{% if … %}`, `{% endif %}`, `{% for … %}`, …) but leaves
/// the control-block BODY between them LIVE (S1/S2 anchor a real name in a
/// conditional body). For EDGE emission that live body is a TRAP: a literal
/// `from <sibling>` inside `{% if is_incremental() %} … {% endif %}` is a
/// CONDITIONAL dependency — on a full-refresh build the guard is pruned and
/// `base→derived` has NO compiled counterpart, so asserting a `resolved` edge
/// would be a FALSE claim. Edges therefore use a STRICTER exclusion than the
/// node/column path: a `from`-referent whose byte position falls inside ANY
/// `{% … %}…{% end… %}` control-block span ([`control_block_spans`]) is SUPPRESSED
/// (the pane shows it unresolved — never a guessed/conditional edge). Only a
/// TOP-LEVEL (depth-0, un-templated) `from <sibling>` is unconditional enough to
/// be an edge. `mask_regions` is left untouched (the node/column/zone-extent
/// behavior is unchanged); the exclusion is edge-local.
///
/// Each emitted edge is `confidence: resolved` at the call site — the masking
/// scan only ever yields a dependency it can prove lexically (a live, whole-word
/// `from`/`join` keyword immediately followed by a sibling CTE id) AND
/// unconditionally (outside every control block). A reference that is NOT a
/// sibling CTE id (an external relation, a `ref()`-mediated name masked away, a
/// column) is simply not emitted.
pub(crate) fn explicit_cte_edges(
    raw: &str,
    raw_cte_spans: &std::collections::BTreeMap<String, SourceSpan>,
) -> Vec<RawEdge> {
    let Some(masked) = mask_regions(raw) else {
        // Malformed Jinja ⇒ emit nothing (fail-closed).
        return Vec::new();
    };
    // The byte spans of every Jinja control-block body — a `from`-referent inside
    // any of these is CONDITIONAL and must NOT emit an edge (the stricter,
    // edge-local exclusion; see the fn doc). Fail-closed: a malformed/unbalanced
    // control-block stream yields a span covering the rest of the source, so a
    // `from` after the break is treated as inside-a-block (never under-suppressed).
    let block_spans = control_block_spans(raw);
    let mut edges: Vec<RawEdge> = Vec::new();
    for (to_id, body_span) in raw_cte_spans {
        let range = body_span.byte_range();
        let Some(body) = masked.get(range.clone()) else {
            continue;
        };
        // `body` is a sub-slice of `masked` starting at `range.start` in raw
        // coordinates; a referent's position in `body` maps to `range.start + pos`.
        let body_base = range.start;
        for (referenced, body_pos) in from_join_referents(body) {
            // CONTROL-BLOCK EXCLUSION: a referent inside any `{% … %}…{% end… %}`
            // body is conditional ⇒ no edge (never a false claim).
            let abs_pos = body_base + body_pos;
            if position_in_any_span(abs_pos, &block_spans) {
                continue;
            }
            // Only a reference to ANOTHER verbatim raw CTE (a sound raw node) is
            // an edge — a self-reference, an external relation, or a name not in
            // the raw-CTE set is not emitted. (A CTE cannot depend on itself in
            // standard SQL; the self-reference guard inside the case-insensitive
            // resolve makes that explicit.)
            //
            // CASE-FOLDING (cute-dbt#478): `referenced` is an UNQUOTED bare
            // identifier (a quoted `"Base"` referent was blanked by `mask_regions`
            // — see `classify_token` — so a quoted case-mismatch never reaches
            // here, honoring dbt-ident quoting for free). dbt/warehouse fold
            // unquoted identifier case, so `from Base` referencing `with base`
            // is a genuine sibling reference and must resolve to the `base` key
            // case-INSENSITIVELY. We resolve against the actual key so the edge's
            // `from` carries the canonical CTE-name (the map key), not the
            // referent's source casing.
            if let Some(canonical) = resolve_sibling_cte(&referenced, to_id, raw_cte_spans) {
                let edge = RawEdge {
                    from: canonical,
                    to: to_id.clone(),
                };
                // De-dupe (a body may name the same sibling in both a FROM and a
                // JOIN; the DAG carries one edge per ordered pair).
                if !edges.contains(&edge) {
                    edges.push(edge);
                }
            }
        }
    }
    edges
}

/// Whether byte offset `pos` falls inside any half-open `[start, end)` span of
/// `spans` (the control-block body extents). A `from`-referent at `pos` inside one
/// is a CONDITIONAL dependency ⇒ suppressed from edges.
fn position_in_any_span(pos: usize, spans: &[(usize, usize)]) -> bool {
    spans.iter().any(|&(start, end)| pos >= start && pos < end)
}

/// Resolve an unquoted `from`/`join` referent to the CANONICAL sibling-CTE name
/// (the `raw_cte_spans` key) it references, or `None` if it is not a sibling
/// (cute-dbt#478). `referent` is always an unquoted bare identifier — a quoted
/// `"Base"` was blanked by `mask_regions` before this point — so we mirror
/// dbt-ident's UNQUOTED-identifier semantics: case is folded
/// (`Base` == `base` == `BASE`). The match is ASCII-case-insensitive against
/// every key EXCEPT `self_id` (a CTE cannot reference itself in standard SQL).
///
/// NEVER-A-FALSE-EDGE (honesty principle 3): case-folding only ever resolves a
/// referent whose folded form EQUALS a real sibling CTE key — it never invents a
/// key, and a quoted referent (already masked away) never reaches here, so a
/// quoted `"Base"` does NOT fold onto `base`. The returned `String` is the
/// MAP KEY (canonical CTE-name), never the referent's source casing, so the
/// emitted edge endpoint matches the node id the rest of the DAG uses.
fn resolve_sibling_cte(
    referent: &str,
    self_id: &str,
    raw_cte_spans: &std::collections::BTreeMap<String, SourceSpan>,
) -> Option<String> {
    raw_cte_spans
        .keys()
        .find(|key| key.as_str() != self_id && key.eq_ignore_ascii_case(referent))
        .cloned()
}

/// Every identifier that immediately follows a whole-word `from` / `join`
/// keyword in the masked CTE body `body` (cute-dbt#471, S3), paired with the
/// referent's BYTE START in `body` (so the edge caller can map it to a raw
/// coordinate and apply the control-block exclusion). The body is already masked
/// (no strings/comments/Jinja), so every `from`/`join` here is a LIVE SQL keyword
/// and every following word is a LIVE relation reference. ASCII case-insensitive
/// keyword match; the referent is returned verbatim in its SOURCE CASING (it is
/// always an unquoted bare identifier — quoted referents were blanked by
/// `mask_regions`). The call site folds case ASCII-case-insensitively against the
/// raw-CTE keys (`resolve_sibling_cte`, cute-dbt#478) to mirror how dbt/the
/// warehouse fold UNQUOTED identifier case — so `from Base` resolves to a `base`
/// CTE while a quoted case-mismatch (already masked away) never matches.
fn from_join_referents(body: &str) -> Vec<(String, usize)> {
    let bytes = body.as_bytes();
    let n = bytes.len();
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut i = 0usize;
    while i < n {
        // Find the start of the next identifier-or-keyword word.
        if !is_ident_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && is_ident_byte(bytes[i]) {
            i += 1;
        }
        let word = &body[start..i];
        if word.eq_ignore_ascii_case("from") || word.eq_ignore_ascii_case("join") {
            // The next word (skipping whitespace) is the referenced relation. A
            // qualified `db.schema.rel` reference is NOT a bare CTE name, so only
            // an immediately-following bare identifier with no `.` qualifier is a
            // sibling-CTE candidate; the call site's raw-CTE-set membership check
            // rejects anything else.
            let ws_end = skip_ws(bytes, i);
            if ws_end < n && is_ident_byte(bytes[ws_end]) {
                let rstart = ws_end;
                let mut rend = ws_end;
                while rend < n && is_ident_byte(bytes[rend]) {
                    rend += 1;
                }
                // Reject a qualified reference (`rel.col` / `schema.rel`): a `.`
                // immediately after the word means it is not a bare CTE name.
                let qualified = rend < n && bytes[rend] == b'.';
                if !qualified {
                    out.push((body[rstart..rend].to_owned(), rstart));
                }
                i = rend;
            }
        }
    }
    out
}

/// The half-open byte spans (in `raw` coordinates) of every Jinja control-block
/// BODY — the live text BETWEEN a `{% <opener> … %}` and its depth-matched
/// `{% end<opener> %}` (cute-dbt#471, the honesty fix). A `from <sibling>`
/// referent whose position lands inside one of these is a CONDITIONAL dependency
/// (pruned away on the wrong build) and must NOT emit an edge.
///
/// Recognized openers — every Jinja construct with an `end…` closer a `raw_code`
/// model body can carry: `if`, `for`, `block`, `macro`, `call`, `filter`, `with`,
/// `autoescape`, `raw`, `trans`, `apply`, and the BLOCK form of `set`
/// (`{% set x %}…{% endset %}`, distinguished from the inline `{% set x = … %}` by
/// the absence of an `=` before the tag close). Each opener pushes a depth-stack
/// frame keyed by its keyword; the matching `end<keyword>` pops it and records the
/// body span `[opener_close, closer_open)`. Mid dividers (`else`/`elif`) are
/// ignored (they do not change the enclosing block's body extent). Variable tags
/// `{{…}}` and comments `{#…#}` are skipped wholesale (an inner `{% %}` inside a
/// comment never counts).
///
/// FAIL-CLOSED (never under-suppress): the instant the scan hits an UNBALANCE — an
/// unterminated tag, an `end<x>` with no matching opener, or a mismatched
/// `end<x>` — it abandons structural matching and returns a single span covering
/// `[break_point, raw.len())`, so every `from` from the break to EOF is treated as
/// inside-a-block. Likewise any opener left UNCLOSED at EOF contributes a span
/// `[its_body_start, raw.len())`. The exclusion can only ever DROP a (possibly
/// real) edge, never fabricate one — the honest failure direction.
fn control_block_spans(raw: &str) -> Vec<(usize, usize)> {
    let bytes = raw.as_bytes();
    let n = bytes.len();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    // The depth stack: (opener keyword lowercased, body-start byte = past the
    // opener's `%}`).
    let mut stack: Vec<(String, usize)> = Vec::new();
    let mut i = 0usize;
    while i < n {
        if bytes[i] != b'{' || i + 1 >= n {
            i += 1;
            continue;
        }
        // Delegate the per-`{`-opener handling (skip a `{#…#}`/`{{…}}` region, or
        // classify + pair a `{%…%}` block tag) to keep this loop low-complexity.
        match step_control_scan(raw, i, &mut spans, &mut stack) {
            ScanStep::Advance(next) => i = next,
            // Malformed / unbalanced ⇒ fail-closed from `i` to EOF.
            ScanStep::FailClosed => return fail_closed_from(i, n, &stack),
        }
    }
    // Any opener left unclosed at EOF: its body runs to EOF (fail-closed).
    for (_, body_start) in &stack {
        spans.push((*body_start, n));
    }
    spans
}

/// The outcome of processing the `{`-led region at byte `i` in
/// [`control_block_spans`]: advance to a resumption index, or fail closed.
enum ScanStep {
    /// Resume the scan at this byte index.
    Advance(usize),
    /// A malformed/unbalanced tag ⇒ the caller fails closed from `i` to EOF.
    FailClosed,
}

/// Process the single `{`-led region at byte `i` of `raw` for
/// [`control_block_spans`]: skip a `{#…#}` comment / `{{…}}` variable tag
/// wholesale, or classify + depth-pair a `{%…%}` block tag (pushing an opener,
/// popping + recording a matched closer's body span). Mutates `spans`/`stack` in
/// place and returns the resumption index, or [`ScanStep::FailClosed`] on a
/// malformed tag or a mismatched/orphan closer. The caller guarantees
/// `bytes[i] == b'{'` and `i + 1 < bytes.len()`.
fn step_control_scan(
    raw: &str,
    i: usize,
    spans: &mut Vec<(usize, usize)>,
    stack: &mut Vec<(String, usize)>,
) -> ScanStep {
    let bytes = raw.as_bytes();
    match bytes[i + 1] {
        // Comment `{#…#}` — skip wholesale; an inner `{% %}` never counts.
        b'#' => find_close(bytes, i + 2, b'#').map_or(ScanStep::FailClosed, ScanStep::Advance),
        // Variable tag `{{…}}` — skip wholesale (string-literal-aware).
        b'{' => find_expr_close(bytes, i + 2, b'}').map_or(ScanStep::FailClosed, ScanStep::Advance),
        // Block tag `{%…%}` — classify + pair.
        b'%' => {
            let Some(close) = find_expr_close(bytes, i + 2, b'%') else {
                return ScanStep::FailClosed;
            };
            match classify_control_tag(&raw[i..close]) {
                ControlTag::Open { keyword } => stack.push((keyword, close)),
                // The closer must match the innermost opener's keyword; a
                // mismatched / orphan `end<x>` ⇒ unbalanced ⇒ fail closed.
                ControlTag::End { keyword } => match stack.pop() {
                    Some((open_kw, body_start)) if open_kw == keyword => {
                        spans.push((body_start, i));
                    }
                    _ => return ScanStep::FailClosed,
                },
                // `else`/`elif` and every non-paired tag (`set` inline, …) do not
                // change the enclosing block's body extent.
                ControlTag::Skip => {}
            }
            ScanStep::Advance(close)
        }
        // A bare `{X` opens no Jinja region; advance one byte.
        _ => ScanStep::Advance(i + 1),
    }
}

/// The fail-closed result for [`control_block_spans`]: a span `[break, len)` (the
/// rest of the source is treated as inside-a-block), UNIONED with the body span of
/// every still-open frame on the stack (their bodies also run to EOF). This is the
/// never-under-suppress contract — on ANY malformed/unbalanced Jinja the edge
/// scanner suppresses every `from` from the break point onward.
fn fail_closed_from(
    break_point: usize,
    len: usize,
    stack: &[(String, usize)],
) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = stack
        .iter()
        .map(|&(_, body_start)| (body_start, len))
        .collect();
    spans.push((break_point, len));
    spans
}

/// Every Jinja construct that pairs with an `end…` closer and so bounds a body
/// extent (besides the block-form `set`, handled separately because it shares a
/// keyword with the inline `{% set x = … %}` assignment). The matching closer is
/// `end<keyword>`.
const BLOCK_OPENERS: &[&str] = &[
    "if",
    "for",
    "block",
    "macro",
    "call",
    "filter",
    "with",
    "autoescape",
    "raw",
    "trans",
    "apply",
    "embed",
];

/// The control-flow classification of a `{%…%}` tag for [`control_block_spans`].
enum ControlTag {
    /// A block opener (`if`/`for`/`block`/`macro`/…) keyed by its lowercased
    /// keyword; the matching closer is `end<keyword>`.
    Open { keyword: String },
    /// A block closer `end<keyword>` (the `end` prefix stripped).
    End { keyword: String },
    /// Any tag that does NOT bound a body extent (a mid divider `else`/`elif`, an
    /// inline `{% set x = … %}`, or any unrecognized tag).
    Skip,
}

/// Classify a full `{%…%}` tag slice `tag` into a [`ControlTag`] by its leading
/// keyword. An `end<x>` closer is `End { keyword: x }`. A known block opener is
/// `Open { keyword }`. The BLOCK form of `set` (`{% set x %}`, no `=` before the
/// close) opens; the INLINE form (`{% set x = … %}`) is `Skip`. Everything else
/// (`else`/`elif`/unrecognized) is `Skip`.
fn classify_control_tag(tag: &str) -> ControlTag {
    // Strip `{%`, an optional whitespace-control `-`, and leading whitespace;
    // strip the trailing `%}`/`-%}` and surrounding whitespace, to read the inner
    // statement (mirrors render::classify_block_tag's shape).
    let inner = tag
        .strip_prefix("{%")
        .unwrap_or(tag)
        .trim_start_matches('-')
        .trim_start();
    let inner = inner
        .strip_suffix("%}")
        .unwrap_or(inner)
        .trim_end()
        .trim_end_matches('-')
        .trim_end();
    let keyword = inner.split_whitespace().next().unwrap_or("");
    // A closer: `end` + a non-empty keyword (`endif`, `endfor`, `endmacro`, …). The
    // non-empty guard rejects a bare `end` (not a Jinja closer). Avoids a let-chain
    // (unstable on MSRV 1.88) by matching the post-strip result directly.
    match keyword.strip_prefix("end") {
        Some(closed) if !closed.is_empty() => {
            return ControlTag::End {
                keyword: closed.to_ascii_lowercase(),
            };
        }
        _ => {}
    }
    // The block-form `set` opens a body ONLY when it carries NO `=` (the inline
    // `{% set x = expr %}` form assigns and has no `{% endset %}`).
    if keyword == "set" {
        return if inner.contains('=') {
            ControlTag::Skip
        } else {
            ControlTag::Open {
                keyword: "set".to_owned(),
            }
        };
    }
    // Every other Jinja construct that pairs with an `end…` closer.
    if BLOCK_OPENERS.contains(&keyword) {
        ControlTag::Open {
            keyword: keyword.to_ascii_lowercase(),
        }
    } else {
        ControlTag::Skip
    }
}

/// Derive a `{% for %}` / `{% if %}` zone's compiled **EXTENT** by BOUNDARY-
/// ANCHORING (cute-dbt#471, S3) — the sound basis for `node_map.raw[zone]` (which
/// compiled CTEs fall inside the loop's fan-out). A zone's `entry.compiled` is a
/// single literal ANCHOR (`resolve_zone_compiled`), NOT its extent; the fanned
/// CTE names (`us_sales`, `eu_sales`) are NOT verbatim in raw (only the
/// `{{ r }}_sales` template is), so the extent must be located by the UNIQUE
/// literal text that BOUNDS the zone in the surrounding raw:
///
/// - `before` = the longest literal fragment in the raw text immediately BEFORE
///   the zone opener that occurs EXACTLY ONCE in `compiled` → the extent starts at
///   the END of that occurrence.
/// - `after` = the longest literal fragment immediately AFTER the zone closer that
///   occurs EXACTLY ONCE in `compiled` → the extent ends at the START of that
///   occurrence.
///
/// The extent is `[before.end, after.start)`. Sound because the boundary anchors
/// are literal text OUTSIDE the zone (unaffected by the loop expansion), so every
/// compiled CTE between them is the zone's observed fan-out by structural position.
///
/// OMIT-ON-AMBIGUOUS (never over-claim): if EITHER boundary fails to anchor
/// uniquely (zero or multiple occurrences), returns `None` — the call site then
/// OMITS the zone from `node_map.raw` rather than listing a CTE it cannot prove is
/// in the zone. A zone at the very start/end of the model (no before/after
/// literal) likewise has no anchor ⇒ `None`. A returned `Some((s, e))` with
/// `s == e` is an honest EMPTY extent (the zone compiled to nothing between its
/// anchors).
pub(crate) fn zone_compiled_extent(
    raw: &str,
    zone_raw_span: &SourceSpan,
    compiled: &str,
) -> Option<(u32, u32)> {
    let zr = zone_raw_span.byte_range();
    if zr.start > raw.len() || zr.end > raw.len() {
        return None;
    }
    // The raw text BEFORE the zone opener and AFTER the zone closer — the regions
    // the boundary anchors are drawn from. Use the MASKED raw so a Jinja/string/
    // comment fragment is never picked as a boundary anchor (it would not appear
    // verbatim in compiled). Fail-closed on malformed Jinja.
    let masked = mask_regions(raw)?;
    let before_region = masked.get(..zr.start)?;
    let after_region = masked.get(zr.end..)?;
    // before-anchor: the TIGHTEST (closest-to-zone) trailing literal run of the
    // before-region that occurs UNIQUELY in compiled — its END in compiled is the
    // extent start. after-anchor: the tightest leading literal run of the
    // after-region — its START in compiled is the extent end. "Tightest" so the
    // extent does not swallow compiled CTEs that belong to a SIBLING before/after
    // the zone (a farther anchor would over-claim membership).
    let extent_start = boundary_anchor_end(before_region, compiled)?;
    let extent_end = boundary_anchor_start(after_region, compiled)?;
    if extent_start > extent_end {
        // Inverted (the after-anchor landed before the before-anchor): the zone's
        // surrounding literals are not a clean bracket — omit rather than claim a
        // backwards extent.
        return None;
    }
    Some((extent_start, extent_end))
}

/// The compiled byte offset where a zone's extent STARTS: the END of the UNIQUE
/// occurrence in `compiled` of the TIGHTEST trailing literal run of the
/// before-region (the run closest to the zone opener that anchors uniquely). Walk
/// the before-region's trailing literal runs CLOSEST-FIRST; bind the first that
/// occurs exactly once in compiled. Returns `None` when none anchors uniquely
/// (omit-on-ambiguous).
fn boundary_anchor_end(region: &str, compiled: &str) -> Option<u32> {
    // Trailing literal runs of the masked region, closest-to-zone first. A "run"
    // is a maximal sequence of non-whitespace bytes joined by single spaces — the
    // shape a SQL fragment keeps after masking. We grow the candidate from the
    // region END leftward across runs, trying the closest (shortest) viable
    // candidate first, then widening, so the TIGHTEST unique anchor wins.
    let runs = literal_runs(region);
    // Build trailing candidates: [last], [last-1 .. last], … widening leftward.
    for take in 1..=runs.len() {
        let slice = &runs[runs.len() - take..];
        let candidate = join_runs(region, slice);
        if candidate.chars().filter(|c| !c.is_whitespace()).count() < 3 {
            continue;
        }
        if let Some(at) = unique_match(compiled, &candidate) {
            return u32::try_from(at + candidate.len()).ok();
        }
    }
    None
}

/// The compiled byte offset where a zone's extent ENDS: the START of the UNIQUE
/// occurrence in `compiled` of the TIGHTEST leading literal run of the
/// after-region. Walk the after-region's leading literal runs CLOSEST-FIRST.
/// Returns `None` when none anchors uniquely (omit-on-ambiguous).
fn boundary_anchor_start(region: &str, compiled: &str) -> Option<u32> {
    let runs = literal_runs(region);
    for take in 1..=runs.len() {
        let slice = &runs[..take];
        let candidate = join_runs(region, slice);
        if candidate.chars().filter(|c| !c.is_whitespace()).count() < 3 {
            continue;
        }
        if let Some(at) = unique_match(compiled, &candidate) {
            return u32::try_from(at).ok();
        }
    }
    None
}

/// The `(start, end)` byte ranges of every maximal NON-WHITESPACE run in `region`
/// (the masked text), in source order — a "literal run" is one contiguous
/// non-whitespace token-or-symbol sequence. A candidate boundary anchor is a
/// contiguous SLICE of these runs (the original text between the first run's start
/// and the last run's end, single spaces included), so it matches the compiled
/// text's own single-space glue.
fn literal_runs(region: &str) -> Vec<(usize, usize)> {
    let bytes = region.as_bytes();
    let n = bytes.len();
    let mut runs = Vec::new();
    let mut i = 0usize;
    while i < n {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        runs.push((start, i));
    }
    runs
}

/// The VERBATIM `region` substring spanning the first run's start through the
/// last run's end — interior whitespace PRESERVED exactly as in the masked raw.
/// dbt emits the literal text OUTSIDE a `{% for %}`/`{% if %}` zone verbatim into
/// compiled (only the loop body is re-expanded), so a verbatim boundary anchor of
/// the surrounding raw matches the compiled text byte-for-byte — collapsing
/// whitespace would BREAK that match against compiled's multi-line indentation.
/// `slice` is a contiguous, non-empty sub-slice of [`literal_runs`].
fn join_runs(region: &str, slice: &[(usize, usize)]) -> String {
    let first = slice.first().expect("non-empty slice").0;
    let last = slice.last().expect("non-empty slice").1;
    region[first..last].to_owned()
}

/// The byte offset of `needle` in `haystack` IFF it occurs EXACTLY ONCE; `None`
/// for zero or multiple occurrences (the ambiguity-safe bind the zone-anchor
/// resolution already uses, hoisted for the extent boundaries).
fn unique_match(haystack: &str, needle: &str) -> Option<usize> {
    let mut it = haystack.match_indices(needle);
    let first = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some(first.0)
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

    /// GUARD INTENT (cute-dbt#474): `classify_token` is a TOTAL match over
    /// `sqlparser::tokenizer::Token` with no wildcard arm, so a dep bump that
    /// adds a new `Token` variant FAILS THE BUILD instead of silently defaulting
    /// it to live (a potential false anchor). This test can't assert the compile
    /// error itself, but it PINS the classification of one token from each class
    /// so the behavior the guard protects is recorded — and so a maintainer who
    /// reclassifies a variant while fixing a future non-exhaustive-match error
    /// trips a red test if they get the Mask/Live direction wrong.
    #[test]
    fn classify_token_pins_mask_and_live_classes() {
        use sqlparser::ast::DollarQuotedString;
        use sqlparser::keywords::Keyword;
        use sqlparser::tokenizer::Word;

        let quoted_ident = Token::Word(Word {
            value: "my col".to_owned(),
            quote_style: Some('"'),
            keyword: Keyword::NoKeyword,
        });
        let bare_ident = Token::Word(Word {
            value: "customer_id".to_owned(),
            quote_style: None,
            keyword: Keyword::NoKeyword,
        });

        // Mask: every string-literal form, comments, and quoted identifiers.
        for tok in [
            Token::SingleQuotedString("x".to_owned()),
            Token::DollarQuotedString(DollarQuotedString {
                value: "x".to_owned(),
                tag: None,
            }),
            Token::EscapedStringLiteral("x".to_owned()),
            Token::UnicodeStringLiteral("x".to_owned()),
            Token::NationalStringLiteral("x".to_owned()),
            Token::HexStringLiteral("x".to_owned()),
            Token::Whitespace(Whitespace::SingleLineComment {
                comment: "c".to_owned(),
                prefix: "--".to_owned(),
            }),
            Token::Whitespace(Whitespace::MultiLineComment("c".to_owned())),
            quoted_ident,
        ] {
            assert_eq!(
                classify_token(&tok),
                MaskClass::Mask,
                "{tok:?} must be masked (name-bearing region)"
            );
            assert!(is_maskable_token(&tok), "{tok:?} maskable");
        }

        // Live: bare identifier, numbers, real whitespace, structure/operators.
        for tok in [
            bare_ident,
            Token::Number("42".to_owned(), false),
            Token::Whitespace(Whitespace::Space),
            Token::Whitespace(Whitespace::Newline),
            Token::Whitespace(Whitespace::Tab),
            Token::LParen,
            Token::RParen,
            Token::Comma,
            Token::Period,
            Token::Placeholder("$1".to_owned()),
            Token::Eq,
            Token::EOF,
        ] {
            assert_eq!(
                classify_token(&tok),
                MaskClass::Live,
                "{tok:?} must stay live (carries no hidden name)"
            );
            assert!(!is_maskable_token(&tok), "{tok:?} not maskable");
        }
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

    // ── cute-dbt#471 (S3): explicit_cte_edges + zone_compiled_extent ──────────

    /// Build the raw-CTE-span map (`SourceMap::raw_node_spans` shape) by running
    /// the real `fill_raw_spans` over `raw` for the given verbatim CTE ids.
    fn raw_cte_spans_for(
        raw: &str,
        ids: &[&str],
    ) -> std::collections::BTreeMap<String, SourceSpan> {
        let mut sm = sm_with(ids.iter().map(|id| cte_entry(id)).collect());
        fill_raw_spans(&mut sm, raw);
        sm.raw_node_spans()
    }

    /// A `SourceSpan` over `[start, end)` of `text` (line/col computed honestly).
    fn span_of(text: &str, start: u32, end: u32) -> SourceSpan {
        crate::adapters::render::byte_span(text, start as usize, end as usize)
            .expect("in-bounds, char-aligned span")
    }

    /// The first located zone's raw span (the real `locate_raw_zones` path, via
    /// the test-visible fuzz shim that returns `(kind, start, end, block_id)`).
    fn first_zone_span(raw: &str) -> SourceSpan {
        let (_, s, e, _) = crate::adapters::render::fuzz_locate_raw_zones(raw)
            .into_iter()
            .next()
            .expect("one zone located");
        span_of(raw, s, e)
    }

    #[test]
    fn explicit_cte_edges_emits_a_bare_from_sibling() {
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  select id from base\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert_eq!(
            edges,
            vec![RawEdge {
                from: "base".to_owned(),
                to: "derived".to_owned()
            }],
            "a bare `from base` inside `derived` is one explicit edge base→derived"
        );
    }

    #[test]
    fn explicit_cte_edges_ignores_a_ref_mediated_dependency() {
        // The dependency runs through `{{ ref('base') }}` (masked) ⇒ no edge.
        let raw = "with base as (\n  select 1 as id\n),\nderived as (\n  select id from {{ ref('base') }}\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert!(
            edges.is_empty(),
            "a ref()-mediated dependency is masked ⇒ NO edge"
        );
    }

    #[test]
    fn explicit_cte_edges_ignores_a_name_in_a_string_or_comment() {
        // `base` appears only inside a string and a comment in `derived` ⇒ no edge.
        let raw = "with base as (\n  select 1 as id\n),\nderived as (\n  select 'from base' as n -- from base\n  , id from ext\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert!(
            edges.is_empty(),
            "a name only inside a string/comment is masked ⇒ not an edge endpoint"
        );
    }

    #[test]
    fn explicit_cte_edges_ignores_a_qualified_or_external_reference() {
        // `from warehouse.base` (qualified) and `from external_rel` (not a sibling
        // CTE) both produce no edge — only a bare sibling-CTE name is an edge.
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  select id from warehouse.base\n  union all select id from external_rel\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert!(
            edges.is_empty(),
            "a qualified `warehouse.base` / an external relation is not a sibling-CTE edge"
        );
    }

    #[test]
    fn explicit_cte_edges_malformed_jinja_fails_closed() {
        // Unbalanced Jinja ⇒ mask_regions returns None ⇒ no edges (fail-closed).
        let raw = "with base as (select 1), derived as (select id from base {% if";
        let spans = std::collections::BTreeMap::from([
            ("base".to_owned(), span_of(raw, 5, 23)),
            ("derived".to_owned(), span_of(raw, 25, 56)),
        ]);
        let edges = explicit_cte_edges(raw, &spans);
        assert!(
            edges.is_empty(),
            "malformed Jinja ⇒ emit nothing (fail-closed)"
        );
    }

    // ── cute-dbt#471 honesty fix: control-block `from` is CONDITIONAL ⇒ no edge ──
    // The masker blanks the control TAGS but leaves the BODY live, so a literal
    // `from <sibling>` inside `{% if %}`/`{% for %}` previously emitted a `resolved`
    // edge — a FALSE claim (the dependency is pruned away on the wrong build). The
    // edge path now excludes any referent inside a control-block body. All built
    // through the REAL SourceMap path (`raw_cte_spans_for` runs `fill_raw_spans`).

    #[test]
    fn literal_from_inside_an_if_block_emits_no_edge() {
        // `from base` lives inside `{% if true %} … {% endif %}` ⇒ CONDITIONAL ⇒
        // NO edge (the pane shows it unresolved, never a guessed/conditional edge).
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  {% if true %}\n  select id from base\n  {% endif %}\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert!(
            edges.is_empty(),
            "a `from base` inside an `{{% if %}}` block is conditional ⇒ NO edge; got {edges:?}"
        );
    }

    #[test]
    fn literal_from_inside_an_incremental_guard_emits_no_edge_full_refresh() {
        // The canonical false-claim: `from base` inside `{% if is_incremental() %}`.
        // On a FULL-REFRESH build the guard is pruned and `from base` is ABSENT from
        // compiled, so a `resolved` base→derived edge would be a lie. NO edge.
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  {% if is_incremental() %}\n  select id from base\n  {% endif %}\n)\nselect * from derived";
        // Full-refresh compiled: the guarded `from base` is gone entirely.
        let compiled =
            "with base as (\n  select id from src\n),\nderived as (\n  \n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert!(
            edges.is_empty(),
            "a `from base` inside `{{% if is_incremental() %}}` is conditional ⇒ NO edge; got {edges:?}"
        );
        // The compiled text genuinely has no `from base` — the would-be edge has no
        // compiled counterpart, so asserting it would be a false claim.
        assert!(
            !compiled.contains("from base"),
            "full-refresh compiled has no `from base` — the suppressed edge is unbacked"
        );
    }

    #[test]
    fn literal_from_inside_a_for_loop_emits_no_edge() {
        // `from base` inside `{% for r in [...] %} … {% endfor %}` ⇒ CONDITIONAL on
        // the loop body being expanded ⇒ NO edge (even with a verbatim, un-templated
        // `from base` in the loop body).
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  {% for r in [1, 2] %}\n  select id from base\n  {% endfor %}\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert!(
            edges.is_empty(),
            "a `from base` inside a `{{% for %}}` loop is conditional ⇒ NO edge; got {edges:?}"
        );
    }

    #[test]
    fn top_level_from_outside_any_block_still_emits_a_resolved_edge() {
        // REGRESSION GUARD: a TOP-LEVEL (depth-0, un-templated) `from base` is
        // unconditional and MUST still emit a resolved edge — the exclusion is
        // strictly about control-block BODIES, not all `from`s.
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  select id from base\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert_eq!(
            edges,
            vec![RawEdge {
                from: "base".to_owned(),
                to: "derived".to_owned()
            }],
            "a top-level `from base` is unconditional ⇒ one resolved edge base→derived"
        );
    }

    #[test]
    fn top_level_from_with_a_sibling_if_block_still_emits_its_edge() {
        // A control block ELSEWHERE in the body must not suppress an UNCONDITIONAL
        // top-level `from` (the exclusion is position-scoped to block bodies, not
        // model-wide). Here `derived` has both a guarded `from other` (no edge) and
        // a top-level `from base` (edge) — only base→derived survives.
        let raw = "with base as (\n  select id from src\n),\nother as (\n  select 1 as id\n),\nderived as (\n  select id from base\n  {% if is_incremental() %}\n  union all select id from other\n  {% endif %}\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "other", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert_eq!(
            edges,
            vec![RawEdge {
                from: "base".to_owned(),
                to: "derived".to_owned()
            }],
            "top-level `from base` survives; the guarded `from other` is suppressed; got {edges:?}"
        );
    }

    #[test]
    fn nested_control_block_from_emits_no_edge() {
        // A `from base` nested TWO blocks deep (`{% for %}` inside `{% if %}`) is
        // still inside a control-block body ⇒ NO edge (depth-matched exclusion).
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  {% if is_incremental() %}\n  {% for r in [1] %}\n  select id from base\n  {% endfor %}\n  {% endif %}\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert!(
            edges.is_empty(),
            "a `from base` nested inside `{{% if %}}`→`{{% for %}}` is conditional ⇒ NO edge; got {edges:?}"
        );
    }

    // ── cute-dbt#478: case-insensitive raw-DAG edge matching (quote-aware) ──────
    // dbt/the warehouse fold UNQUOTED identifier case, so `from Base` referencing
    // `with base` is a genuine sibling reference and must emit the edge. A QUOTED
    // referent (`from "Base"`) preserves case (dbt-ident); it is blanked by
    // `mask_regions` before the scan, so a quoted case-mismatch never matches —
    // honoring quoting for free. The canonical edge endpoint is always the
    // raw-CTE map key (`base`), never the referent's source casing.

    #[test]
    fn unquoted_from_mixed_case_resolves_to_lowercase_sibling() {
        // `from Base` (mixed case) referencing `with base` ⇒ edge base→derived
        // (unquoted identifier case folds). The edge endpoint is the key `base`.
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  select id from Base\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert_eq!(
            edges,
            vec![RawEdge {
                from: "base".to_owned(),
                to: "derived".to_owned()
            }],
            "unquoted `from Base` folds to the `base` CTE ⇒ one edge base→derived; got {edges:?}"
        );
    }

    #[test]
    fn unquoted_from_uppercase_resolves_to_lowercase_sibling() {
        // `from BASE` (all caps) referencing `with base` ⇒ edge base→derived.
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  select id from BASE\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert_eq!(
            edges,
            vec![RawEdge {
                from: "base".to_owned(),
                to: "derived".to_owned()
            }],
            "unquoted `from BASE` folds to the `base` CTE ⇒ one edge base→derived; got {edges:?}"
        );
    }

    #[test]
    fn quoted_from_case_mismatch_emits_no_edge() {
        // `from "Base"` (QUOTED, case-PRESERVING per dbt-ident) referencing a
        // `with base` CTE is NOT a match — the quoted referent is blanked by
        // `mask_regions`, so it never reaches the resolve. NO false edge.
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  select id from \"Base\"\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert!(
            edges.is_empty(),
            "a QUOTED `\"Base\"` is case-sensitive and does NOT match the `base` CTE ⇒ NO edge; got {edges:?}"
        );
    }

    #[test]
    fn unquoted_exact_case_still_emits_edge_regression() {
        // REGRESSION GUARD: the common exact-case `from base` → `with base` path
        // is unchanged by the case-fold (a top-level unquoted edge still works).
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  select id from base\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert_eq!(
            edges,
            vec![RawEdge {
                from: "base".to_owned(),
                to: "derived".to_owned()
            }],
            "exact-case `from base` still emits base→derived; got {edges:?}"
        );
    }

    #[test]
    fn case_fold_never_fabricates_an_edge_for_an_unrelated_name() {
        // A coincidental case-fold of an UNRELATED relation must NOT become an
        // edge. `derived`'s only sibling reference is `from base`; `from EXTERNAL`
        // (not a sibling CTE, despite caps) produces no edge — the case-fold only
        // resolves a referent whose folded form equals a real sibling key.
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  select id from base\n  union all select id from EXTERNAL\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert_eq!(
            edges,
            vec![RawEdge {
                from: "base".to_owned(),
                to: "derived".to_owned()
            }],
            "only the real sibling `base` resolves; `EXTERNAL` is not folded into any CTE; got {edges:?}"
        );
    }

    #[test]
    fn case_fold_does_not_create_a_self_edge() {
        // The self-reference guard is case-insensitive too: a `derived` body that
        // names `from Derived` (own name, different case) must NOT self-edge.
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  select id from base\n  union all select 1 from Derived\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert_eq!(
            edges,
            vec![RawEdge {
                from: "base".to_owned(),
                to: "derived".to_owned()
            }],
            "a case-variant self-reference `from Derived` is not a self-edge; only base→derived; got {edges:?}"
        );
    }

    #[test]
    fn unbalanced_control_block_fails_closed_suppressing_following_from() {
        // An `{% if %}` opener with NO matching `{% endif %}` ⇒ unbalanced ⇒
        // fail-closed: every `from` from the opener body to EOF is suppressed
        // (never under-suppress). The control-block scanner's malformed path; the
        // tag stream is otherwise well-formed so `mask_regions` succeeds.
        let raw = "with base as (\n  select id from src\n),\nderived as (\n  {% if true %}\n  select id from base\n)\nselect * from derived";
        let spans = raw_cte_spans_for(raw, &["base", "derived"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert!(
            edges.is_empty(),
            "an unbalanced `{{% if %}}` ⇒ fail-closed ⇒ the trailing `from base` is suppressed; got {edges:?}"
        );
    }

    #[test]
    fn templated_from_inside_a_for_loop_still_emits_no_edge() {
        // The PRE-EXISTING guard (kept): a TEMPLATED `from {{ prev }}` is blanked by
        // masking, so even outside the control-block-body exclusion it produces no
        // edge. This is the original `for_loop_mediated_dependency_emits_no_edge`
        // shape, kept at the unit level to prove masking still covers it.
        let raw = "with base as (\n  select 1 as id\n),\nstep as (\n  {% for prev in ['base'] %}\n  select id from {{ prev }}\n  {% endfor %}\n)\nselect * from step";
        let spans = raw_cte_spans_for(raw, &["base", "step"]);
        let edges = explicit_cte_edges(raw, &spans);
        assert!(
            edges.is_empty(),
            "a templated `from {{{{ prev }}}}` is masked ⇒ NO edge; got {edges:?}"
        );
    }

    #[test]
    fn zone_compiled_extent_brackets_the_fanned_ctes_soundly() {
        // A loop bracketed by a verbatim `base` CTE (before) and `final` CTE
        // (after): the extent spans exactly the two fanned CTEs in compiled.
        let raw = "with base as (\n  select region from src\n),\n{% for region in ['us','eu'] %}\n{{ region }}_sales as (\n  select 1 from base\n),\n{% endfor %}\nfinal as (\n  select * from base\n)\nselect * from final";
        let compiled = "with base as (\n  select region from src\n),\nus_sales as (\n  select 1 from base\n),\neu_sales as (\n  select 1 from base\n),\nfinal as (\n  select * from base\n)\nselect * from final";
        // The zone raw span: locate it via the real zone scanner.
        let zone_span = first_zone_span(raw);
        let (ext_start, ext_end) =
            zone_compiled_extent(raw, &zone_span, compiled).expect("a sound extent");
        let extent = &compiled[ext_start as usize..ext_end as usize];
        // The extent contains BOTH fanned CTE bodies and NEITHER sibling.
        assert!(extent.contains("us_sales"), "extent covers us_sales");
        assert!(extent.contains("eu_sales"), "extent covers eu_sales");
        assert!(
            !extent.contains("final as"),
            "extent stops before the `final` sibling CTE (never swallows it)"
        );
    }

    #[test]
    fn zone_compiled_extent_omits_on_a_non_unique_after_anchor() {
        // OMIT-ON-AMBIGUOUS reached at the AFTER-anchor specifically. The
        // before-region carries a UNIQUE ≥3-char literal (`unique_prefix_xyz`)
        // that binds the extent START — so execution genuinely REACHES the
        // after-anchor (this is the cute-dbt#471 Finding B fix: the old fixture
        // omitted on the before-anchor `with`, never exercising this branch). The
        // after-region's leading literal (`repeated_tail`) appears TWICE in
        // compiled and its widened two-token form is absent, so NO after-candidate
        // binds uniquely ⇒ the extent END cannot anchor ⇒ None.
        let raw =
            "unique_prefix_xyz\n{% for x in [1] %}body{% endfor %}\nrepeated_tail repeated_tail";
        let compiled = "unique_prefix_xyz body repeated_tail then repeated_tail end";
        let zone_span = first_zone_span(raw);
        // MECHANICAL PROOF the after-anchor branch executes: reconstruct the
        // before/after regions exactly as `zone_compiled_extent` does (masked raw,
        // sliced at the zone span) and assert the before-anchor BINDS while the
        // after-anchor does NOT — so the function genuinely reaches and omits at
        // the after-anchor (the cute-dbt#471 Finding B fix: the old fixture's
        // before-anchor `with` was absent in compiled, omitting BEFORE this point).
        let zr = zone_span.byte_range();
        let masked = mask_regions(raw).expect("well-formed jinja masks");
        let before_region = &masked[..zr.start];
        let after_region = &masked[zr.end..];
        assert!(
            boundary_anchor_end(before_region, compiled).is_some(),
            "the before-anchor binds ⇒ the extent START resolves and execution \
             REACHES the after-anchor (not a vacuous before-anchor omit)"
        );
        assert!(
            boundary_anchor_start(after_region, compiled).is_none(),
            "the after-anchor does NOT bind ⇒ this is where the omit genuinely fires"
        );
        assert!(
            zone_compiled_extent(raw, &zone_span, compiled).is_none(),
            "a non-unique after-anchor (reached after a unique before-anchor) ⇒ None \
             (omit-on-ambiguous, never over-claim)"
        );
    }

    #[test]
    fn zone_compiled_extent_omits_when_a_boundary_region_is_empty() {
        // A zone at the very END of the model has an EMPTY after-region ⇒ no
        // after-anchor ⇒ None (omit-on-ambiguous), never a fabricated extent.
        let raw = "with base as (select 1)\n{% for x in [1] %}q{% endfor %}";
        let compiled = "with base as (select 1)";
        let zone_span = first_zone_span(raw);
        assert!(
            zone_compiled_extent(raw, &zone_span, compiled).is_none(),
            "an empty after-region ⇒ None (no fabricated extent)"
        );
    }
}
