//! The canonical unit-test fixture IR — rows × typed cells, format- and
//! engine-normalized (cute-dbt#98).
//!
//! This is the IR construction layer of the cell-level data-table diff. It
//! turns a unit test's `given`/`expect` fixture rows — from either the
//! CURRENT manifest (the NEW side) or a reconstructed OLD-side YAML region
//! (the diff-sourced side) — into one [`FixtureTable`]: ordered columns
//! plus ordered [`TableRow`]s of semantically-[typed](CellValue) cells. The
//! cell-diff algorithm (`domain::cell_diff`, File 2, a later PR) consumes
//! this IR; it is **not** part of this module.
//!
//! ## The headline guarantee: format & engine convergence
//!
//! Both entry shapes — already-typed `serde_json::Value` (NEW) and raw
//! scalar tokens (OLD) — terminate in the same [`CellValue`]
//! canonicalization, so the *same logical data* expressed in different
//! source formats (manifest-dict vs reconstructed-YAML, dbt-core's
//! csv-as-array-of-string-dicts vs dbt-fusion's csv-as-raw-string) yields
//! an **equal** [`FixtureTable`]. A format-only or engine-only difference
//! is therefore a zero-diff at the table level — the property cute-dbt#66
//! already promises the report's two CSV views, lifted into the diff IR.
//!
//! ## Three typing behaviors, not two
//!
//! Equality is *semantic*, so cells are typed at construction. There are
//! three entry points, and **`format` is the only discriminator** between the
//! dict path and the csv path — a `Value::String "1"` infers `Number` under
//! `format: csv` but stays `Str` under `format: dict` (cute-dbt#127):
//!
//! 1. [`type_cell_value`] — the NEW side **dict** path, an already-typed JSON
//!    `Value`. A `Value::String` is a *deliberate* string (a dict author's
//!    quoted `"1"` is a string), so it stays [`CellValue::Str`] verbatim —
//!    never re-coerced.
//! 2. [`type_cell_scalar`] — OLD-side **dict** tokens (block-dict +
//!    inline-flow YAML): quote-stripped tokens stay `Str`; otherwise
//!    `true`/`false` → [`Bool`](CellValue::Bool), `null`/`~` →
//!    [`Null`](CellValue::Null), a fully-numeric token →
//!    [`Number`](CellValue::Number), else `Str`. Symmetric with
//!    `type_cell_value`'s dict-number typing.
//! 3. [`type_csv_token`] — csv cells on **both** engine encodings (fusion's
//!    raw-string body AND dbt-core's pre-parsed string dicts): a csv field is
//!    **value-inferred** with fusion's warehouse-numeric ladder (empty →
//!    `Null`; numeric → `Number`; case-insensitive `true`/`false` → `Bool`;
//!    else `Str`). This makes a dict↔csv reformat with equal values a zero
//!    data diff. The csv-format `Value::Array` path routes its string cells
//!    through [`type_csv_token`] too (via a format-aware `type_fn` thread in
//!    the array normalizer), so dbt-core's string-dicts and dbt-fusion's
//!    raw-string body converge to the same typed table.
//!
//! ## Domain purity
//!
//! `std` + `serde` (derive) + `serde_json::Value` only — the same
//! dependency surface `unit_test.rs` and `pr_diff.rs` already use. No I/O,
//! no parser libraries, no `clap`/`askama`. A leaf within `domain`: nothing
//! in `domain` points back at this module (the cell-diff in File 2 imports
//! *downward* into it). Per ADR-1 the hand-rolled RFC 4180 CSV parser
//! ([`parse_csv_rows`]) is mandated over the `csv` crate — and precedented
//! by the JS twin in `templates/report.html` (cute-dbt#66).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::unit_test_yaml::parse_yaml_scalar;

// ---------------------------------------------------------------------
// The IR PODs
// ---------------------------------------------------------------------

/// A format- and engine-normalized fixture table: ordered columns + ordered
/// typed rows.
///
/// Additive POD (ADR-5). `Serialize`/`Deserialize` so the pre-diff IR can
/// cross to JS for the "Current" table view (Workflow 2). `columns` is the
/// first-seen column order — csv header order, or the union of keys across
/// dict rows in first-seen order. A column present in only one row still
/// appears here; rows that lack it carry [`CellValue::Absent`] at that
/// position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureTable {
    /// First-seen column order (csv header order, or union-of-keys across
    /// dict rows). A column present in only one row still appears here.
    pub columns: Vec<String>,
    /// The rows, in source order. Each row's cells are positionally aligned
    /// to [`columns`](Self::columns).
    pub rows: Vec<TableRow>,
}

impl FixtureTable {
    /// Construct from owned parts.
    #[must_use]
    pub fn new(columns: Vec<String>, rows: Vec<TableRow>) -> Self {
        Self { columns, rows }
    }
}

impl Default for FixtureTable {
    /// The empty table (`columns = []`, `rows = []`) — what a `Value::Null`
    /// or empty-array fixture normalizes to, and the `unwrap_or_default`
    /// stand-in the File-2 diff uses for an absent OLD/NEW side.
    fn default() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
        }
    }
}

/// One row: cells positionally aligned to [`FixtureTable::columns`]. A
/// column the row lacks is `Cell { value: CellValue::Absent }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRow {
    /// The row's cells, in column order.
    pub cells: Vec<Cell>,
}

impl TableRow {
    /// Construct from owned cells.
    #[must_use]
    pub fn new(cells: Vec<Cell>) -> Self {
        Self { cells }
    }
}

/// One cell — split into its two axes (cute-dbt#138):
///
/// - [`display`](Self::display) — the **authored token** (truth), rendered in
///   BOTH the Current and Diff views. A csv `1.00` shows `1.00`, not the
///   normalized `1`.
/// - [`key`](Self::key) — the canonical [`CellValue`], the **equality** axis,
///   used ONLY for the diff's change decision and row alignment. `1`, `1.0`,
///   `1.00` all key to `Number("1")`, so a format-only reformat is not a diff.
///
/// Shipping both axes to JS is the foundation the settings normalize-toggle
/// (cute-dbt#139) builds on — it re-flags client-side between `key` (normalized)
/// and `display` (strict) without a Rust round-trip.
///
/// Kept a struct (not a bare `CellValue`) so the diff can hang per-cell render
/// hints off it without a wire-shape break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// The authored token, rendered verbatim in both views. For an
    /// [`Absent`](CellValue::Absent) or [`Null`](CellValue::Null) key the
    /// display is the empty string (the renderer derives the NULL/absent
    /// affordance from `key.t`, never from `display`).
    pub display: String,
    /// The cell's semantically-typed equality key.
    pub key: CellValue,
}

impl Cell {
    /// Construct from a typed value, deriving the display from the key (the
    /// canonical form). Use this when there is no distinct authored token —
    /// e.g. test literals, projected diff cells, and `Absent` placeholders.
    #[must_use]
    pub fn new(key: CellValue) -> Self {
        Self {
            display: display_from_key(&key),
            key,
        }
    }

    /// Construct from a distinct authored `display` token plus its canonical
    /// `key`. This is the **fidelity** path: the normalizers capture the raw
    /// token here so the Current/Diff views render what the author wrote, not
    /// the canonicalized form.
    #[must_use]
    pub fn with_display(display: String, key: CellValue) -> Self {
        Self { display, key }
    }
}

/// The display string derived from a canonical [`CellValue`] when there is no
/// distinct authored token: the value rendered as the author would have seen
/// it. [`Null`](CellValue::Null) and [`Absent`](CellValue::Absent) yield the
/// empty string — the renderer supplies their NULL/blank affordance from
/// `key.t`, never from the display text.
#[must_use]
pub fn display_from_key(key: &CellValue) -> String {
    match key {
        CellValue::Null | CellValue::Absent => String::new(),
        CellValue::Bool(b) => b.to_string(),
        CellValue::Number(n) => n.clone(),
        CellValue::Str(s) => s.clone(),
    }
}

/// Semantically-typed cell value — the equality axis of the cell diff.
///
/// Adjacently tagged (`{"t": <type>, "v": <value>}`) so the JS branches on
/// the type tag AND a `Str "1"` never collides on the wire with a
/// `Number "1"`. Unit variants serialize as `{"t": "absent"}` (no `"v"`).
///
/// [`Eq`] is derivable because [`Number`](Self::Number) holds a *canonical
/// decimal `String`*, not an `f64` — no `NaN`, exact, and `Hash`-clean for
/// the File-2 LCS row keying. `1`, `1.0`, and `1.00` all canonicalize to
/// `Number("1")`, so a format-only numeric difference is semantically
/// equal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "t", content = "v")]
pub enum CellValue {
    /// JSON `null`, an empty csv field `""`, or a YAML `null` / `~` token
    /// (the dbt empty-equals-null convention). Distinct from
    /// [`Absent`](Self::Absent).
    Null,
    /// A boolean — JSON `true`/`false`, or a lowercase `true`/`false`
    /// dict token. `True`/`TRUE` stay [`Str`](Self::Str) (conservative,
    /// documented boundary).
    Bool(bool),
    /// A number, held as its **canonical decimal string** (not `f64`):
    /// `Eq` + `Hash` free, exact on large integers. `1` / `1.0` / `1.00`
    /// → `"1"`; `1.50` → `"1.5"`; `-0` → `"0"`.
    Number(String),
    /// A string — verbatim. A csv field, a quoted scalar, or a manifest
    /// `Value::String` (a *deliberate* string, never re-coerced).
    Str(String),
    /// The column does not exist for this row (sparse dict, or a
    /// column added/removed in the diff). Distinct from [`Null`](Self::Null):
    /// a cell going `Absent → Null` IS a change (the column was added).
    Absent,
}

/// A fixture's dbt `format`: `dict`, `csv`, or `sql`.
///
/// `sql` is a raw `SELECT` string. cute-dbt#137 tabulates the **literal-row**
/// subset (`SELECT lit AS col … UNION ALL …`) via [`parse_sql_literal_rows`];
/// a non-literal sql (any clause/operator/cast/function/bare-word ref) is
/// opaque, so the [normalizers](table_from_manifest_rows) return `None` and
/// the view falls back to the cute-dbt#96 YAML/sql text diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixtureFormat {
    /// `format: dict` — row maps (or csv pre-parsed to row maps by core).
    Dict,
    /// `format: csv` — a raw csv body (fusion) or pre-parsed string dicts
    /// (core); either way the cells are value-inferred (cute-dbt#127) so the
    /// two engine encodings converge.
    Csv,
    /// `format: sql` — a raw `SELECT` string. Opaque: no cells.
    Sql,
}

impl FixtureFormat {
    /// Parse a dbt `format:` string. `None` (the dbt default) maps to
    /// [`Dict`](Self::Dict). An unrecognized non-empty value also maps to
    /// `Dict` (tolerant per ADR-5 — the normalizer still inspects the
    /// `rows` shape and degrades gracefully).
    #[must_use]
    pub fn from_opt(format: Option<&str>) -> Self {
        match format {
            Some("csv") => Self::Csv,
            Some("sql") => Self::Sql,
            _ => Self::Dict,
        }
    }
}

// ---------------------------------------------------------------------
// Cell typing — the three entry points converging on CellValue
// ---------------------------------------------------------------------

/// Type a NEW-side **dict** cell from an already-typed JSON `Value` (the
/// `format: dict` manifest path).
///
/// - `Null` → [`CellValue::Null`]
/// - `Bool(b)` → [`CellValue::Bool`]
/// - `Number(n)` → [`CellValue::Number`] (canonicalized)
/// - `String(s)` → [`CellValue::Str`] **verbatim** — a dict-format manifest
///   string is a deliberate string (a dict author's quoted `"1"` is a
///   string), so it is NOT re-coerced to a number/bool. The csv path infers
///   instead (see [`type_csv_token`]); `format` is the discriminator
///   (cute-dbt#127).
/// - `Array`/`Object` → [`CellValue::Str`] of the value's compact JSON
///   (nested values are opaque, rare, and must never panic).
#[must_use]
pub fn type_cell_value(v: &Value) -> CellValue {
    match v {
        Value::Null => CellValue::Null,
        Value::Bool(b) => CellValue::Bool(*b),
        Value::Number(n) => CellValue::Number(canonicalize_json_number(n)),
        Value::String(s) => CellValue::Str(s.clone()),
        Value::Array(_) | Value::Object(_) => {
            CellValue::Str(serde_json::to_string(v).unwrap_or_default())
        }
    }
}

/// Type an OLD-side **dict** token (block-dict or inline-flow YAML).
///
/// - A token wrapped in a matching pair of single OR double quotes →
///   [`CellValue::Str`] of the inner text, with **no** further coercion (a
///   quoted `'1'` / `"1"` is a deliberate string). Quote-stripping reuses
///   the YAML scalar reader `unit_test_yaml::parse_yaml_scalar` (crate-private).
/// - Else the trimmed, unquoted token: `""` → [`Null`](CellValue::Null);
///   exactly `true`/`false` (lowercase) → [`Bool`](CellValue::Bool);
///   `null`/`~` → `Null`; a fully-numeric token →
///   [`Number`](CellValue::Number) (canonicalized); otherwise →
///   [`Str`](CellValue::Str).
#[must_use]
pub fn type_cell_scalar(token: &str) -> CellValue {
    let trimmed = token.trim();
    if is_quoted(trimmed) {
        // Reuse the YAML scalar reader's quote-stripping (it reads up to the
        // matching closing quote). A quoted token stays a deliberate string.
        return CellValue::Str(parse_yaml_scalar(trimmed));
    }
    if trimmed.is_empty() {
        return CellValue::Null;
    }
    match trimmed {
        "true" => return CellValue::Bool(true),
        "false" => return CellValue::Bool(false),
        "null" | "~" => return CellValue::Null,
        _ => {}
    }
    if let Some(canon) = canonicalize_str_number(trimmed) {
        return CellValue::Number(canon);
    }
    CellValue::Str(trimmed.to_owned())
}

/// Type a csv field token (fusion's raw-string body AND core's pre-parsed
/// string dicts).
///
/// csv equality is **warehouse-numeric**, not textual: dbt-fusion parses a
/// csv field with a typed ladder (empty→null; numeric→number; case-insensitive
/// `true`/`false`→bool; else string), renders it to a SQL literal, then
/// compares *after a warehouse `CAST`* — so `1`≡`1.0`≡`1.00`, `1.50`≡`1.5`,
/// `1e3`≡`1000`, `-0`≡`0`. cute-dbt mirrors that ladder here, terminating in
/// [`CellValue`] (cute-dbt#127), so a dict↔csv reformat with equal values is
/// a zero data diff:
///
/// 1. `""` → [`CellValue::Null`] (the dbt empty-equals-null convention; the
///    JS twin half-implements it via the `hi < row.length ? … : ""` fill).
/// 2. a fully-numeric token → [`CellValue::Number`] (canonicalized
///    `i128`-first, so it is **strictly more exact than fusion's lossy
///    `f64`** on big integers; we deliberately do NOT mirror fusion's
///    `Number::to_string()`, which would split `1.0` from `1`).
/// 3. case-insensitive `true`/`false` → [`CellValue::Bool`].
/// 4. otherwise → [`CellValue::Str`] **verbatim** — no trim, no quote-strip
///    (the RFC 4180 [`parse_csv_rows`] already handled quoting/whitespace).
///
/// ## Documented divergences from dbt-fusion (accepted, not bugs; cute-dbt#127)
///
/// 1. **`"null"`/`"NULL"` text stays `Str`.** Fusion coerces the literal text
///    `null`/`NULL` to SQL NULL in any format (`create_values`); cute-dbt
///    keeps it as a [`CellValue::Str`] so a diff cell can render the literal
///    word "null". (The common empty-field→`Null` case is still zero-diff.)
/// 2. **Ragged rows are tolerated, not an error.** Fusion's `csv` crate
///    (`flexible=false`) errors on a row with the wrong field count; cute-dbt
///    pads a short row (`""`→`Null`) and drops long extras — correct for a
///    render-not-execute diff tool ([`parse_csv_rows`]).
/// 3. **`i128` vs `f64` wide-integer reach.** The numeric path uses `i128`
///    (exact to ~1.7e38); a dict integer `> u64::MAX` falls to `f64` — a
///    known limitation, not a tested-equal case.
#[must_use]
pub fn type_csv_token(token: &str) -> CellValue {
    if token.is_empty() {
        return CellValue::Null;
    }
    if let Some(canon) = canonicalize_str_number(token) {
        return CellValue::Number(canon);
    }
    if token.eq_ignore_ascii_case("true") {
        return CellValue::Bool(true);
    }
    if token.eq_ignore_ascii_case("false") {
        return CellValue::Bool(false);
    }
    CellValue::Str(token.to_owned())
}

// ---------------------------------------------------------------------
// Authored-token cell builders — display (truth) + key (equality)
// ---------------------------------------------------------------------

/// Build a NEW-side **dict** [`Cell`] from an already-typed JSON `Value`,
/// capturing the authored token as the display (cute-dbt#138).
///
/// `key` is [`type_cell_value`]'s canonical value; `display` is the value as
/// authored: a string verbatim, a bool's `true`/`false`, a number's authored
/// digits (`Value::Number`'s own `to_string`, which — absent `serde_json`'s
/// `arbitrary_precision` — is the f64 round-trip, so a dict-numeric `1.00`
/// already arrived as `1.0` upstream of this layer), or compact JSON for a
/// nested value. A `Null` displays empty (the renderer styles it from `key.t`).
#[must_use]
pub fn cell_from_value(v: &Value) -> Cell {
    let key = type_cell_value(v);
    let display = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(v).unwrap_or_default(),
    };
    Cell::with_display(display, key)
}

/// Build an OLD-side **dict** [`Cell`] from a YAML scalar token, capturing the
/// authored token as the display (cute-dbt#138).
///
/// `key` is [`type_cell_scalar`]'s canonical value. `display` is the authored
/// token as a reader sees it: a quoted scalar's inner text (quote-stripped via
/// the same `parse_yaml_scalar` path that `type_cell_scalar` uses), else the
/// trimmed token. A token whose key is [`Null`](CellValue::Null) (`""`,
/// `null`, `~`) displays empty so the renderer styles it as NULL from `key.t`.
#[must_use]
pub fn cell_from_scalar(token: &str) -> Cell {
    let key = type_cell_scalar(token);
    let trimmed = token.trim();
    let display = if is_quoted(trimmed) {
        parse_yaml_scalar(trimmed)
    } else if matches!(key, CellValue::Null) {
        String::new()
    } else {
        trimmed.to_owned()
    };
    Cell::with_display(display, key)
}

/// Build a csv [`Cell`] from a field token, capturing the raw token as the
/// display (cute-dbt#138).
///
/// `key` is [`type_csv_token`]'s value-inferred canonical value; `display` is
/// the raw csv token **verbatim** (so a csv `1.00` renders as `1.00` even
/// though its key is `Number("1")` — the headline fidelity fix). An empty
/// field (`key == Null`) displays empty.
#[must_use]
pub fn cell_from_csv_token(token: &str) -> Cell {
    let key = type_csv_token(token);
    let display = if token.is_empty() {
        String::new()
    } else {
        token.to_owned()
    };
    Cell::with_display(display, key)
}

/// The csv-format NEW-side `Value` cell builder (cute-dbt#138): a
/// [`Value::String`] routes through [`cell_from_csv_token`] (raw token kept as
/// display, value-inferred key); any other shape falls back to
/// [`cell_from_value`]. The `cell_fn` analogue of the dict-path
/// [`cell_from_value`] — `format` is the only discriminator.
fn cell_from_csv_value(v: &Value) -> Cell {
    match v {
        Value::String(s) => cell_from_csv_token(s),
        other => cell_from_value(other),
    }
}

/// Whether `s` is wrapped in a matching pair of single or double quotes
/// (length ≥ 2, same quote char at both ends).
fn is_quoted(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
}

/// Canonicalize a JSON number to a decimal string. Integers route through
/// the exact `i128`/`u128` path (no `f64` — large integers like
/// `9007199254740993` survive precision-intact); only genuine decimals go
/// through `f64` shortest-round-trip formatting.
fn canonicalize_json_number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return canonicalize_int(i128::from(i));
    }
    if let Some(u) = n.as_u64() {
        return canonicalize_int(i128::from(u));
    }
    // A non-integer JSON number (or one beyond i64/u64 range that serde
    // still parsed). Fall through to the string canonicalizer, which tries
    // i128 first (catching the wide-integer case) then f64.
    canonicalize_str_number(&n.to_string()).unwrap_or_else(|| n.to_string())
}

/// Canonicalize a numeric *string token* to a decimal string, or `None`
/// when it is not fully numeric.
///
/// Integer path first: a token that fully parses as `i128` is rendered via
/// `i128::to_string` — exact, no `f64` precision loss. Only a genuine
/// decimal (the `i128` parse fails but `f64` succeeds AND is finite) is
/// `f64`-formatted shortest-round-trip with trailing zeros and a trailing
/// `.` stripped. `1`/`1.0`/`1.00` → `"1"`; `1.50` → `"1.5"`; `0.85` →
/// `"0.85"`; `1000.0` → `"1000"`; `-0` → `"0"`. A non-finite `f64`
/// (`NaN`/`inf`, unreachable from valid JSON) yields `None`, keeping the
/// caller total.
fn canonicalize_str_number(token: &str) -> Option<String> {
    if let Ok(i) = token.parse::<i128>() {
        return Some(canonicalize_int(i));
    }
    let f = token.parse::<f64>().ok()?;
    if !f.is_finite() {
        return None;
    }
    Some(canonicalize_float(f))
}

/// `i128` → decimal string, mapping `-0` (impossible for `i128` but kept for
/// symmetry with the float path's intent) and rendering exactly.
fn canonicalize_int(i: i128) -> String {
    i.to_string()
}

/// Shortest-round-trip `f64` formatting with trailing zeros + a trailing
/// `.` stripped, and `-0` normalized to `0`. Only called for genuine
/// decimals (the `i128` parse already failed).
fn canonicalize_float(f: f64) -> String {
    // `{}` on f64 is shortest-round-trip in Rust. For a whole-valued float
    // (e.g. `1000.0` that slipped past the i128 parse because the token had
    // a `.`) this prints "1000"; for "1.50" it prints "1.5".
    let s = format!("{f}");
    let trimmed = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    };
    if trimmed == "-0" {
        "0".to_owned()
    } else {
        trimmed
    }
}

// ---------------------------------------------------------------------
// Parsers (csv / block-dict / inline-flow) — header-keyed string rows
// ---------------------------------------------------------------------

/// Hand-rolled RFC 4180 csv parser — a faithful Rust port of
/// `templates/report.html`'s `parseCsvRows` (cute-dbt#66).
///
/// Returns header-keyed rows as `Vec<Vec<(column, value)>>`, preserving
/// header order per row. Serves BOTH the fusion-csv NEW side
/// ([`table_from_manifest_rows`] on a `Value::String`) and the csv OLD side
/// ([`table_from_yaml_fragment`] on a dedented `rows: |` body).
///
/// Behavior (since cute-dbt#138 this is the SOLE RFC 4180 implementation — the
/// JS `parseCsvRows` twin was retired when the Current view started rendering
/// the Rust-computed [`FixtureTable`] POD; the `g22`–`g26` unit tests own its
/// correctness): strip exactly one trailing `\n` (and a preceding `\r`);
/// quoted fields; `""` → a literal `"`; CRLF as one terminator;
/// commas/newlines inside quotes verbatim; the first row is the header; fewer
/// than two rows (empty or header-only) → `[]`; an unterminated final row is
/// accepted; a row shorter than the header fills the missing trailing columns
/// with `""` (which [`type_csv_token`] then maps to `Null`).
#[must_use]
pub fn parse_csv_rows(text: &str) -> Vec<Vec<(String, String)>> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = strip_one_trailing_newline(text);
    if body.is_empty() {
        return Vec::new();
    }
    let matrix = scan_csv_matrix(body);
    if matrix.len() < 2 {
        // Empty or header-only → no data rows.
        return Vec::new();
    }
    key_rows_by_header(&matrix)
}

/// Strip exactly one trailing newline (LF, or a CRLF as a unit). A second
/// trailing newline (a genuine blank final line) survives.
fn strip_one_trailing_newline(text: &str) -> &str {
    text.strip_suffix('\n')
        .map_or(text, |s| s.strip_suffix('\r').unwrap_or(s))
}

/// The mutable scan state for the RFC 4180 char loop. Splitting the branch
/// set off `parse_csv_rows` into [`feed`](CsvScanner::feed) keeps each
/// function's cyclomatic complexity in the strict-gate band.
#[derive(Default)]
struct CsvScanner {
    matrix: Vec<Vec<String>>,
    fields: Vec<String>,
    field: String,
    in_quotes: bool,
}

impl CsvScanner {
    /// Feed one character with a one-char lookahead. Returns how many
    /// characters were consumed (1, or 2 for a `""` escape / a CRLF pair).
    fn feed(&mut self, c: char, next: Option<char>) -> usize {
        if self.in_quotes {
            return self.feed_in_quotes(c, next);
        }
        self.feed_unquoted(c, next)
    }

    /// Inside a quoted field: a `"` either escapes (`""` → literal `"`) or
    /// closes the quote; any other char is literal content.
    fn feed_in_quotes(&mut self, c: char, next: Option<char>) -> usize {
        if c == '"' {
            if next == Some('"') {
                self.field.push('"');
                return 2;
            }
            self.in_quotes = false;
            return 1;
        }
        self.field.push(c);
        1
    }

    /// Outside quotes: an opening `"` (only at field start), a field
    /// separator `,`, a row terminator (LF / CR / CRLF), or literal content.
    fn feed_unquoted(&mut self, c: char, next: Option<char>) -> usize {
        match c {
            '"' if self.field.is_empty() => {
                self.in_quotes = true;
                1
            }
            ',' => {
                self.end_field();
                1
            }
            '\n' | '\r' => {
                self.end_row();
                // CRLF: consume the paired \n as part of this terminator.
                if c == '\r' && next == Some('\n') {
                    2
                } else {
                    1
                }
            }
            _ => {
                self.field.push(c);
                1
            }
        }
    }

    /// Close the current field.
    fn end_field(&mut self) {
        self.fields.push(std::mem::take(&mut self.field));
    }

    /// Close the current field AND the current row.
    fn end_row(&mut self) {
        self.end_field();
        self.matrix.push(std::mem::take(&mut self.fields));
    }

    /// Flush the final (unterminated) field + row and yield the matrix.
    fn finish(mut self) -> Vec<Vec<String>> {
        self.end_row();
        self.matrix
    }
}

/// Scan a csv body (trailing newline already stripped) into a row × field
/// string matrix. The first row is the header.
fn scan_csv_matrix(body: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = body.chars().collect();
    let mut scanner = CsvScanner::default();
    let mut i = 0;
    while i < chars.len() {
        i += scanner.feed(chars[i], chars.get(i + 1).copied());
    }
    scanner.finish()
}

/// Key each data row (`matrix[1..]`) by the header row (`matrix[0]`),
/// filling a missing trailing field with `""`. Caller guarantees
/// `matrix.len() >= 2`.
fn key_rows_by_header(matrix: &[Vec<String>]) -> Vec<Vec<(String, String)>> {
    let headers = &matrix[0];
    matrix[1..]
        .iter()
        .map(|row| {
            headers
                .iter()
                .enumerate()
                .map(|(hi, header)| (header.clone(), row.get(hi).cloned().unwrap_or_default()))
                .collect()
        })
        .collect()
}

/// Parse a block-style dict `rows:` region into header-keyed string rows.
///
/// A new `- ` at the row indent starts a new row; each subsequent
/// deeper-indented `key: value` line within that row contributes a column.
/// The value is split on the FIRST `:` and trimmed; the raw value token is
/// kept (quote-stripping happens later in [`type_cell_scalar`]). Columns
/// accrue in first-seen order across rows (the [normalizer](table_from_yaml_fragment)
/// unions them). An inline-flow `- { … }` row (the `- ` line contains `{`)
/// is routed to [`parse_inline_flow_row`].
///
/// `rows_region` is the text *under* the `rows:` key (the `rows:` line
/// itself excluded), with consistent leading indentation — the
/// [normalizer](table_from_yaml_fragment) slices it out of the OLD-side
/// YAML by indentation.
#[must_use]
pub fn parse_block_dict_rows(rows_region: &str) -> Vec<Vec<(String, String)>> {
    let mut acc = BlockDictAcc::default();
    for line in rows_region.lines() {
        acc.feed_line(line);
    }
    acc.finish()
}

/// The mutable accumulator for the block-dict line walk. Distributing the
/// new-row / field-line branches onto methods keeps each function's
/// cyclomatic complexity in the strict-gate band.
#[derive(Default)]
struct BlockDictAcc {
    out: Vec<Vec<(String, String)>>,
    current: Option<Vec<(String, String)>>,
    row_indent: Option<usize>,
}

impl BlockDictAcc {
    /// Classify one source line: a blank/comment is skipped; a `- ` opens a
    /// new row (block-style or inline-flow); a deeper line is a field of the
    /// current row.
    fn feed_line(&mut self, line: &str) {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return; // blank / comment — not a row or a field
        }
        let indent = line.len() - trimmed.len();
        if let Some(after_dash) = trimmed.strip_prefix("- ") {
            self.start_row(line, after_dash, indent);
        } else {
            self.append_field(trimmed, indent);
        }
    }

    /// Open a new row at `indent`. Pins the row indent on first sight,
    /// flushes any in-progress row, then routes an inline-flow `- { … }`
    /// line to [`parse_inline_flow_row`] or starts a block-style row whose
    /// `- ` line may itself carry the first `key: value`.
    fn start_row(&mut self, line: &str, after_dash: &str, indent: usize) {
        self.row_indent.get_or_insert(indent);
        self.flush_current();
        let after = after_dash.trim_start();
        if after.starts_with('{') {
            self.out.push(parse_inline_flow_row(line));
            self.current = None;
            return;
        }
        let mut row: Vec<(String, String)> = Vec::new();
        if let Some(kv) = split_key_value(after) {
            row.push(kv);
        }
        self.current = Some(row);
    }

    /// Append a `key: value` field to the current row, but only when the
    /// line is deeper than the pinned row indent (a sibling/shallower line
    /// is ignored — the OLD-side region only ever holds rows).
    fn append_field(&mut self, trimmed: &str, indent: usize) {
        let Some(ri) = self.row_indent else { return };
        if indent <= ri {
            return;
        }
        if let (Some(row), Some(kv)) = (self.current.as_mut(), split_key_value(trimmed)) {
            row.push(kv);
        }
    }

    /// Push the in-progress row (if any) into the output.
    fn flush_current(&mut self) {
        if let Some(row) = self.current.take() {
            self.out.push(row);
        }
    }

    /// Flush the final row and yield the parsed rows.
    fn finish(mut self) -> Vec<Vec<(String, String)>> {
        self.flush_current();
        self.out
    }
}

/// Parse one inline-flow row (`- {k: v, k2: 'a, b'}`) into header-keyed
/// string values.
///
/// Detects the `{ … }` payload after the `- `, then splits the inner text
/// on commas that are NOT inside quotes (a quote-state-aware split, so
/// `'a, b'` stays one value), and splits each entry on its FIRST `:`. Quote
/// stripping happens later in [`type_cell_scalar`].
#[must_use]
pub fn parse_inline_flow_row(line: &str) -> Vec<(String, String)> {
    let trimmed = line.trim_start();
    let after_dash = trimmed.strip_prefix("- ").unwrap_or(trimmed).trim_start();
    // Slice the {...} payload (first `{` to the matching/last `}`).
    let Some(open) = after_dash.find('{') else {
        return Vec::new();
    };
    let inner_start = open + 1;
    let inner_end = after_dash.rfind('}').unwrap_or(after_dash.len());
    if inner_end <= inner_start {
        return Vec::new();
    }
    let inner = &after_dash[inner_start..inner_end];

    let mut out: Vec<(String, String)> = Vec::new();
    for entry in split_quote_aware(inner, ',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((k, v)) = split_key_value(entry) {
            out.push((k, v));
        }
    }
    out
}

/// Split `key: value` on the FIRST `:`, returning trimmed `(key, value)`.
/// `None` when there is no `:` (a malformed line — skipped).
fn split_key_value(s: &str) -> Option<(String, String)> {
    let idx = s.find(':')?;
    let key = s[..idx].trim().to_owned();
    let value = s[idx + 1..].trim().to_owned();
    Some((key, value))
}

/// Split `s` on `sep`, but only when `sep` is NOT inside a single- or
/// double-quoted run (a quote-state-aware split for inline-flow rows).
///
/// Honors YAML's two intra-string quote escapes so a `sep` (or a quote) that
/// is part of an escaped value never prematurely closes the run:
/// - inside a **double-quoted** run, a backslash escapes the next char, so
///   `"a\", b"` stays one value;
/// - inside a **single-quoted** run, a doubled quote (`''`) is a literal
///   single quote, so `'it''s, ok'` stays one value.
///
/// (The backslash / doubled-quote bytes are kept verbatim here; the later
/// [`type_cell_scalar`] quote-stripping pass owns unescaping.)
fn split_quote_aware(s: &str, sep: char) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some('"') if c == '\\' => {
                // Backslash escape: keep both bytes, never toggle on the next.
                cur.push(c);
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            Some('\'') if c == '\'' && chars.peek() == Some(&'\'') => {
                // Doubled single-quote = a literal quote; stay in quoted mode.
                cur.push(c);
                cur.push(chars.next().expect("peeked quote"));
            }
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                    cur.push(c);
                } else if c == sep {
                    parts.push(std::mem::take(&mut cur));
                } else {
                    cur.push(c);
                }
            }
        }
    }
    parts.push(cur);
    parts
}

// ---------------------------------------------------------------------
// SQL literal-row parser (cute-dbt#137) — conservative-reject
// ---------------------------------------------------------------------

/// Parse a `format: sql` fixture body into header-keyed string rows, but
/// ONLY when it is a literal-row `SELECT ... UNION ALL SELECT ...` shape —
/// the static subset a render-not-execute diff tool can tabulate without a
/// warehouse. Returns `None` ("conservative reject") for anything that needs
/// a query engine to evaluate, so the caller falls back to the cute-dbt#96
/// SQL/text view rather than ever showing a partial or wrong table.
///
/// ## Accept grammar
///
/// ```text
/// query := arm ( "UNION ALL" arm )*
/// arm   := SELECT proj ( "," proj )*
/// proj  := literal [ "AS" ] alias
/// literal := number | 'single-quoted string' | TRUE | FALSE | NULL
/// alias   := bare-word identifier
/// ```
///
/// Columns are the FIRST arm's aliases (UNION ALL is positional in SQL);
/// every later arm must have the same projection count (its own aliases are
/// ignored — positional). `AS` is optional (dbt's canonical fixture writes
/// `select 1 as id`, but `select 1 id` is also valid SQL); the keywords
/// `SELECT`/`AS`/`UNION ALL`/`TRUE`/`FALSE`/`NULL` are case-insensitive.
///
/// ## Reject (→ `None` → cute-dbt#96 fallback)
///
/// Any top-level clause (`FROM`/`WHERE`/`JOIN`/`GROUP`/`ORDER`/`LIMIT`/…);
/// any set-op except `UNION ALL` (`UNION`, `INTERSECT`, `EXCEPT`); any
/// non-literal projection (operators `1+1`, casts `1::int`/`CAST(...)`,
/// function calls `now()`, bare-word column refs, **double-quoted**
/// identifiers, `*`, subqueries, `CASE`); a missing alias; a projection-count
/// mismatch across arms.
///
/// ## Comments
///
/// `--`-to-EOL and `/* … */` comments are **stripped quote-awarely** (a `--`
/// or `/*` inside a single-quoted string literal — and a `''` escape within
/// it — is preserved) and then ignored — a comment never causes a reject
/// (cute-dbt#137, Christopher's call).
///
/// Each cell carries its own authored [`Cell`] (`display` = the literal's
/// faithful token — a string's unescaped inner text, a number/bool/null's
/// verbatim token-case; `key` = the canonical [`CellValue`]). The literal
/// *kind* is preserved per cell (a `'1'` string literal stays
/// [`CellValue::Str`]; a bare `1` is [`CellValue::Number`]), which is why the
/// parser builds [`Cell`]s directly instead of routing display strings back
/// through a type-erasing string cell-fn.
#[must_use]
pub fn parse_sql_literal_rows(sql: &str) -> Option<FixtureTable> {
    let stripped = strip_sql_comments(sql);
    let arms = split_union_all_arms(&stripped)?;
    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<TableRow> = Vec::new();
    for (ai, arm) in arms.iter().enumerate() {
        let cells = parse_select_arm(arm)?;
        if ai == 0 {
            columns = cells.iter().map(|(alias, _)| alias.clone()).collect();
        } else if cells.len() != columns.len() {
            return None; // mismatched arm width — positional union impossible
        }
        // UNION ALL is positional: every arm's values align to the FIRST
        // arm's aliases (a later arm's own alias text is ignored).
        let row_cells = cells.into_iter().map(|(_, cell)| cell).collect();
        rows.push(TableRow::new(row_cells));
    }
    Some(FixtureTable::new(columns, rows))
}

/// Strip `--`-to-EOL and `/* … */` SQL comments, quote-awarely: a `--` or
/// `/*` appearing inside a single-quoted string literal (honoring the `''`
/// escape) is NOT a comment and is preserved verbatim. Returns the
/// comment-free SQL (comment runs replaced by a single space so two tokens a
/// comment separated do not fuse).
///
/// The char-loop body distributes onto [`SqlCommentStripper`]'s four
/// single-responsibility handlers (in-string, line comment, block comment,
/// ordinary char) so each function's cyclomatic complexity stays in the
/// strict-gate band.
fn strip_sql_comments(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut s = SqlCommentStripper {
        out: String::with_capacity(sql.len()),
        i: 0,
        in_string: false,
    };
    while s.i < chars.len() {
        s.step(&chars);
    }
    s.out
}

/// The quote-aware SQL comment-stripping scan state. Each branch of the char
/// loop is one method so [`strip_sql_comments`]'s body stays a thin dispatch.
struct SqlCommentStripper {
    out: String,
    i: usize,
    in_string: bool,
}

impl SqlCommentStripper {
    /// Advance one step over `chars`, emitting kept characters and skipping
    /// comment runs (replaced by a single space).
    fn step(&mut self, chars: &[char]) {
        if self.in_string {
            self.in_string(chars);
            return;
        }
        let c = chars[self.i];
        if c == '\'' {
            self.in_string = true;
            self.out.push(c);
            self.i += 1;
        } else if c == '-' && chars.get(self.i + 1) == Some(&'-') {
            self.skip_line_comment(chars);
        } else if c == '/' && chars.get(self.i + 1) == Some(&'*') {
            self.skip_block_comment(chars);
        } else {
            self.out.push(c);
            self.i += 1;
        }
    }

    /// Inside a single-quoted string: copy verbatim, honoring the `''` escape
    /// (which stays in-string), and close on a lone `'`.
    fn in_string(&mut self, chars: &[char]) {
        let c = chars[self.i];
        self.out.push(c);
        if c == '\'' {
            if chars.get(self.i + 1) == Some(&'\'') {
                self.out.push('\''); // `''` escape — stay in the string
                self.i += 2;
                return;
            }
            self.in_string = false;
        }
        self.i += 1;
    }

    /// Skip a `--`-to-EOL line comment, leaving a single separating space.
    fn skip_line_comment(&mut self, chars: &[char]) {
        while self.i < chars.len() && chars[self.i] != '\n' {
            self.i += 1;
        }
        self.out.push(' ');
    }

    /// Skip a `/* … */` block comment (or to EOF), leaving a single space.
    fn skip_block_comment(&mut self, chars: &[char]) {
        self.i += 2;
        while self.i < chars.len() && !(chars[self.i] == '*' && chars.get(self.i + 1) == Some(&'/'))
        {
            self.i += 1;
        }
        self.i += 2; // consume the `*/` (saturates past EOF harmlessly)
        self.out.push(' ');
    }
}

/// Split a comment-free query into its `UNION ALL` arms, quote-awarely and
/// case-insensitively. Returns `None` if any OTHER top-level set operator
/// (`UNION` without `ALL`, `INTERSECT`, `EXCEPT`, `MINUS`) appears, or there
/// is no arm at all.
fn split_union_all_arms(sql: &str) -> Option<Vec<String>> {
    let words = tokenize_sql_words(sql);
    // Reject any disallowed top-level set op before splitting.
    if has_disallowed_set_op(&words) {
        return None;
    }
    let mut arms: Vec<String> = Vec::new();
    let mut cur: Vec<SqlWord> = Vec::new();
    let mut idx = 0;
    while idx < words.len() {
        if is_union_all_at(&words, idx) {
            arms.push(render_words(&cur));
            cur.clear();
            idx += 2; // consume UNION ALL
            continue;
        }
        cur.push(words[idx].clone());
        idx += 1;
    }
    arms.push(render_words(&cur));
    if arms.iter().any(|a| a.trim().is_empty()) {
        return None; // a dangling `UNION ALL` with an empty arm
    }
    Some(arms)
}

/// Whether the word at `idx` begins a `UNION ALL` (case-insensitive).
fn is_union_all_at(words: &[SqlWord], idx: usize) -> bool {
    words.get(idx).is_some_and(|w| w.eq_kw("union"))
        && words.get(idx + 1).is_some_and(|w| w.eq_kw("all"))
}

/// Whether the token stream carries a top-level set operator cute-dbt cannot
/// tabulate: a bare `UNION` not followed by `ALL`, or `INTERSECT` / `EXCEPT`
/// / `MINUS`. (`UNION ALL` is the only accepted set op.)
fn has_disallowed_set_op(words: &[SqlWord]) -> bool {
    words.iter().enumerate().any(|(i, w)| {
        if w.eq_kw("intersect") || w.eq_kw("except") || w.eq_kw("minus") {
            return true;
        }
        w.eq_kw("union") && !words.get(i + 1).is_some_and(|n| n.eq_kw("all"))
    })
}

/// Parse one `SELECT proj (, proj)*` arm into `(alias, cell)` pairs, or
/// `None` if it is not a literal-only projection list. Rejects an arm that
/// does not start with `SELECT`, or carries any top-level clause keyword
/// after the projection list (`FROM`/`WHERE`/`JOIN`/…) — such a clause makes
/// some projection fail [`parse_projection`].
fn parse_select_arm(arm: &str) -> Option<Vec<(String, Cell)>> {
    let arm = arm.trim();
    // Must begin with the SELECT keyword (case-insensitive), followed by a
    // word boundary.
    let rest = strip_leading_keyword(arm, "select")?;
    if rest.trim().is_empty() {
        return None; // SELECT with no projections
    }
    let mut out: Vec<(String, Cell)> = Vec::new();
    for proj in split_quote_aware(rest, ',') {
        out.push(parse_projection(proj.trim())?);
    }
    Some(out)
}

/// Strip a leading SQL keyword (case-insensitive) from `s`, requiring a word
/// boundary after it (whitespace or EOF). `None` if `s` does not begin with
/// the keyword as a whole word.
fn strip_leading_keyword<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    let s = s.trim_start();
    // `s.get(..kw.len())` is byte-safe: it returns `None` when `kw.len()`
    // exceeds `s` OR lands inside a multi-byte char, so a unicode-leading SQL
    // string can never panic the keyword check (`s[..kw.len()]` would).
    let head = s.get(..kw.len())?;
    if !head.eq_ignore_ascii_case(kw) {
        return None;
    }
    let after = &s[kw.len()..];
    // Word boundary: the keyword must be followed by whitespace (or be the
    // whole token, handled by callers that allow an empty rest).
    if after.is_empty() || after.starts_with(char::is_whitespace) {
        Some(after)
    } else {
        None
    }
}

/// Parse one projection `literal [AS] alias` into `(alias, cell)`. The cell
/// carries the literal's faithful display plus its canonical typed key.
/// `None` for any non-literal projection, a missing alias, a double-quoted
/// alias, or extra trailing tokens.
fn parse_projection(proj: &str) -> Option<(String, Cell)> {
    let (literal_tok, alias_region) = split_literal_token(proj)?;
    let cell = literal_cell(&literal_tok)?;
    // The alias region may be `AS alias` or just `alias`.
    let alias_region = alias_region.trim();
    let alias_str = strip_leading_keyword(alias_region, "as")
        .map_or(alias_region, str::trim_start)
        .trim();
    let alias = parse_bare_alias(alias_str)?;
    Some((alias, cell))
}

/// Split a projection into its leading literal token and the trailing alias
/// region. The literal is one of: a single-quoted string (consuming `''`
/// escapes), or a contiguous non-whitespace run (number / keyword literal).
/// `None` if the projection is empty. A double-quoted leading token is
/// returned as a token that [`literal_cell`] then rejects.
fn split_literal_token(proj: &str) -> Option<(String, &str)> {
    let proj = proj.trim_start();
    if proj.is_empty() {
        return None;
    }
    let bytes = proj.as_bytes();
    if bytes[0] == b'\'' {
        // Single-quoted string literal: read to the matching unescaped `'`.
        let end = single_quote_end(proj)?;
        return Some((proj[..end].to_owned(), &proj[end..]));
    }
    // Otherwise the literal is a whitespace-delimited token.
    let end = proj.find(char::is_whitespace).unwrap_or(proj.len());
    Some((proj[..end].to_owned(), &proj[end..]))
}

/// The byte index just past the closing quote of a single-quoted string
/// starting at byte 0 of `s` (which begins with `'`), honoring the `''`
/// escape. `None` if the string is never closed.
fn single_quote_end(s: &str) -> Option<usize> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 1; // past the opening quote
    let mut byte = '\''.len_utf8();
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            if chars.get(i + 1) == Some(&'\'') {
                byte += 2 * '\''.len_utf8();
                i += 2;
                continue;
            }
            return Some(byte + c.len_utf8());
        }
        byte += c.len_utf8();
        i += 1;
    }
    None
}

/// Convert a SQL literal token to a typed [`Cell`] (`display` = the literal's
/// faithful authored token; `key` = the canonical [`CellValue`]), or `None`
/// if it is not one of the accepted literal kinds. SQL-literal typing is its
/// own ladder — deliberately NOT the dict/csv typers, which only honor
/// *lowercase* `true`/`false`/`null` and apply YAML quoting semantics:
///
/// - `'…'` single-quoted string → [`CellValue::Str`] of its unescaped inner
///   text (`''` → `'`); display = that inner text. A `'1'` literal stays a
///   string (never re-coerced to a number).
/// - `"…"` double-quoted → an identifier → **reject**.
/// - case-insensitive `TRUE` / `FALSE` → [`CellValue::Bool`]; `NULL` →
///   [`CellValue::Null`]. Display keeps the authored case (`TRUE`/`true`).
/// - a numeric token (`-1`, `1.5`, `1e3`) → [`CellValue::Number`]
///   (canonicalized key); display = the authored digits.
/// - anything else (operators, casts, function calls, bare words, `*`,
///   `CAST`, `CASE`, a subquery `(`) → reject.
fn literal_cell(token: &str) -> Option<Cell> {
    let t = token.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(rest) = t.strip_prefix('\'') {
        // A well-formed single-quoted string (its end was found by
        // split_literal_token, so the trailing `'` is present here).
        let inner = rest.strip_suffix('\'')?.replace("''", "'");
        return Some(Cell::with_display(inner.clone(), CellValue::Str(inner)));
    }
    if t.starts_with('"') {
        return None; // double-quoted = identifier
    }
    if t.eq_ignore_ascii_case("true") {
        return Some(Cell::with_display(t.to_owned(), CellValue::Bool(true)));
    }
    if t.eq_ignore_ascii_case("false") {
        return Some(Cell::with_display(t.to_owned(), CellValue::Bool(false)));
    }
    if t.eq_ignore_ascii_case("null") {
        // A NULL literal keys to Null; per the Cell contract a Null cell
        // displays empty (the renderer styles it from `key.t`).
        return Some(Cell::with_display(String::new(), CellValue::Null));
    }
    // A numeric literal — the only remaining accepted kind. The canonical key
    // strips format (`1.00` → `1`); the display keeps the authored digits.
    canonicalize_str_number(t)
        .map(|canon| Cell::with_display(t.to_owned(), CellValue::Number(canon)))
}

/// Validate `s` as a bare-word SQL alias (an unquoted identifier: ASCII
/// letter/underscore start, then letters/digits/underscores; no trailing
/// tokens). `None` for an empty, quoted, dotted, or multi-token alias.
fn parse_bare_alias(s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    let mut chars = s.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None; // a space, dot, quote, or operator → not a bare alias
    }
    Some(s.to_owned())
}

/// A single SQL "word" produced by [`tokenize_sql_words`]: the raw slice plus
/// a flag marking a single-quoted string literal (so a keyword-looking word
/// inside a string never matches a set-op check).
#[derive(Debug, Clone)]
struct SqlWord {
    text: String,
    is_string: bool,
}

impl SqlWord {
    /// Whether this word equals `kw` case-insensitively AND is not a quoted
    /// string literal (so `'union'` the string never reads as the set op).
    fn eq_kw(&self, kw: &str) -> bool {
        !self.is_string && self.text.eq_ignore_ascii_case(kw)
    }
}

/// Tokenize comment-free SQL into whitespace-delimited words, keeping a
/// single-quoted string (with its `''` escapes) as ONE word. Punctuation
/// stays attached to its word — this tokenizer's only job is the set-op /
/// UNION-ALL boundary scan, not full lexing.
fn tokenize_sql_words(sql: &str) -> Vec<SqlWord> {
    let chars: Vec<char> = sql.chars().collect();
    let mut words: Vec<SqlWord> = Vec::new();
    let mut cur = String::new();
    let mut cur_has_string = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            cur_has_string = true;
            i = consume_quoted_run(&chars, i, &mut cur);
        } else if c.is_whitespace() {
            flush_word(&mut words, &mut cur, &mut cur_has_string);
            i += 1;
        } else {
            cur.push(c);
            i += 1;
        }
    }
    flush_word(&mut words, &mut cur, &mut cur_has_string);
    words
}

/// Append a single-quoted run (starting at the opening `'` at `start`) to
/// `cur`, honoring the `''` escape, and return the index just past the
/// closing quote (or EOF for an unterminated run). The opening and closing
/// quotes are kept verbatim in `cur` — this is a boundary tokenizer, not a
/// quote stripper.
fn consume_quoted_run(chars: &[char], start: usize, cur: &mut String) -> usize {
    cur.push(chars[start]); // opening quote
    let mut i = start + 1;
    while i < chars.len() {
        let d = chars[i];
        cur.push(d);
        if d == '\'' {
            if chars.get(i + 1) == Some(&'\'') {
                cur.push('\''); // `''` escape — stays in the run
                i += 2;
                continue;
            }
            return i + 1; // past the closing quote
        }
        i += 1;
    }
    i // unterminated run → EOF
}

/// Push the in-progress word (if non-empty) into `words`, resetting the
/// accumulator. A word is a string literal iff it contained a quoted run.
fn flush_word(words: &mut Vec<SqlWord>, cur: &mut String, has_string: &mut bool) {
    if !cur.is_empty() {
        words.push(SqlWord {
            text: std::mem::take(cur),
            is_string: *has_string,
        });
    }
    *has_string = false;
}

/// Re-render a word slice into a space-joined arm string for the per-arm
/// `SELECT` parse. (The arm parser re-splits on commas quote-awarely, so the
/// single-space join is lossless for the literal-only grammar.)
fn render_words(words: &[SqlWord]) -> String {
    words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------
// Normalizers — the two terminus functions producing a FixtureTable
// ---------------------------------------------------------------------

/// Build the NEW-side [`FixtureTable`] from a manifest fixture's `rows`
/// `Value` + `format`. Returns `None` for an opaque fixture (no cells).
///
/// Absorbs the dbt-core-vs-fusion csv divergence in ONE place:
/// - `format: sql` with a string `rows` → the literal-row table when it parses
///   as `SELECT lit AS col … UNION ALL …` (cute-dbt#137,
///   [`parse_sql_literal_rows`]); `None` otherwise (non-literal sql → opaque →
///   cute-dbt#96 fallback).
/// - `rows` is an `Array` (dict on both engines, csv-from-core, inline-flow
///   after serde): each element is an object; columns are the first-seen
///   union of keys; a key a row lacks → [`CellValue::Absent`]. The per-field
///   typing is **format-aware** (cute-dbt#127): `format: dict` routes through
///   [`type_cell_value`] (a quoted `"1"` stays `Str`); `format: csv` (core's
///   array-of-string-dicts) routes its string cells through [`type_csv_token`]
///   so a `"1"` cell infers `Number`.
/// - `rows` is a `String` AND `format: csv` (the fusion csv-as-raw-string
///   path): [`parse_csv_rows`] → header-keyed rows → each token through
///   [`type_csv_token`] (value-inferred — see its doc for the ladder).
/// - `rows` is `Null` or an empty array → the empty [`FixtureTable`].
/// - any other shape → `None` (graceful; the IR is not a validator).
#[must_use]
pub fn table_from_manifest_rows(rows: &Value, format: Option<&str>) -> Option<FixtureTable> {
    let fmt = FixtureFormat::from_opt(format);
    match rows {
        Value::Null => Some(FixtureTable::default()),
        // The Array arm is FORMAT-AWARE (cute-dbt#127 DELTA 2, cute-dbt#138):
        // dbt-core encodes csv as a `Value::Array` of all-string dicts, so a
        // csv-format array threads the value-inferring `cell_from_csv_value`;
        // a dict-format array keeps `cell_from_value` verbatim (a
        // deliberately-quoted dict `'1'` stays `Str`). Each cell carries its
        // authored display + canonical key. `format` is the only discriminator.
        Value::Array(elems) => {
            let cell_fn: fn(&Value) -> Cell = match fmt {
                FixtureFormat::Csv => cell_from_csv_value,
                FixtureFormat::Dict | FixtureFormat::Sql => cell_from_value,
            };
            Some(table_from_value_objects(elems, cell_fn))
        }
        Value::String(s) => match fmt {
            FixtureFormat::Csv => Some(table_from_csv_text(s)),
            // A `format: sql` string `rows` tabulates IFF it is a literal-row
            // `SELECT … UNION ALL …` shape (cute-dbt#137); a non-literal sql
            // (any clause, operator, cast, function, bare-word ref) returns
            // `None` → the cute-dbt#96 sql/text fallback.
            FixtureFormat::Sql => parse_sql_literal_rows(s),
            // A non-csv/non-sql string `rows` (a malformed dict) is opaque.
            FixtureFormat::Dict => None,
        },
        // Object / Bool / Number `rows` — not a table.
        _ => None,
    }
}

/// Parse an external unit-test fixture **file** body into a [`FixtureTable`]
/// (cute-dbt#126).
///
/// dbt lets a `given`/`expect` source its rows from an external fixture
/// file (`fixture: <path>`) instead of an inline `rows:` block — the v12
/// manifest carries `rows: null` + the resolved path, so the data is read
/// from the working tree at render time (via the `ProjectFileReader`
/// port). This tabulates that file's raw text **exactly** as the same
/// `format`'s inline `rows:` String would, so an external fixture renders
/// identically to an inline one:
///
/// - `format: csv` → header-keyed value-inferred rows ([`parse_csv_rows`]
///   → [`type_csv_token`]);
/// - `format: sql` → the literal-row table, or `None` for a non-literal
///   `SELECT` (the cute-dbt#96/#137 sql code-block fallback);
/// - `format: dict` / unknown / absent → `None` (a *dict* fixture file is
///   not a dbt construct; the caller resolves the effective `format` from
///   the manifest field or the file extension before calling).
///
/// Delegates to [`table_from_manifest_rows`] over the file text wrapped as
/// a `Value::String`, so the external path can never diverge from the
/// inline csv/sql tabulation.
#[must_use]
pub fn external_fixture_table(text: &str, format: Option<&str>) -> Option<FixtureTable> {
    table_from_manifest_rows(&Value::String(text.to_owned()), format)
}

/// Build the OLD-side [`FixtureTable`] from a reconstructed YAML `rows`
/// region + `format`. The diff-sourced sibling of [`table_from_manifest_rows`],
/// terminating in the same canonicalization so the two sides are
/// comparable.
///
/// - `format: sql` → dedent then [`parse_sql_literal_rows`] (the literal-row
///   table, or `None` for non-literal sql → cute-dbt#96 fallback).
/// - `format: csv` → dedent then [`parse_csv_rows`] → [`type_csv_token`].
/// - `format: dict` (the default) → [`parse_block_dict_rows`] (which routes
///   inline-flow rows to [`parse_inline_flow_row`]) → [`type_cell_scalar`].
///
/// An empty region → the empty [`FixtureTable`].
#[must_use]
pub fn table_from_yaml_fragment(rows_region: &str, format: Option<&str>) -> Option<FixtureTable> {
    match FixtureFormat::from_opt(format) {
        // The OLD-side sql path mirrors the NEW side (cute-dbt#137): dedent
        // the reconstructed `rows:` region, then tabulate IFF it is a
        // literal-row SELECT; a non-literal sql → `None` → #96 fallback.
        FixtureFormat::Sql => {
            let dedented = dedent(rows_region);
            parse_sql_literal_rows(&dedented)
        }
        FixtureFormat::Csv => {
            let dedented = dedent(rows_region);
            Some(table_from_csv_text(&dedented))
        }
        FixtureFormat::Dict => {
            let keyed = parse_block_dict_rows(rows_region);
            Some(table_from_keyed_rows(&keyed, cell_from_scalar))
        }
    }
}

/// Normalize an array of JSON objects (core dict / csv, fusion dict,
/// inline-flow-after-serde) into a [`FixtureTable`], typing each field value
/// through `type_fn`. Columns are the first-seen union of object keys; absent
/// keys → [`CellValue::Absent`].
///
/// `cell_fn` is the **format discriminator** (cute-dbt#127 DELTA 2,
/// cute-dbt#138): the caller passes [`cell_from_value`] for `format: dict`
/// (a quoted `'1'` stays `Str`) and [`cell_from_csv_value`] for `format: csv`
/// (a `"1"` string cell infers `Number`). Each cell carries its authored
/// display plus the canonical key. Mirrors the `cell_fn` thread in
/// [`table_from_keyed_rows`].
fn table_from_value_objects(elems: &[Value], cell_fn: fn(&Value) -> Cell) -> FixtureTable {
    // First-seen union of keys across all object rows.
    let mut columns: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for elem in elems {
        if let Value::Object(map) = elem {
            for k in map.keys() {
                if seen.insert(k.clone()) {
                    columns.push(k.clone());
                }
            }
        }
    }
    let rows = elems
        .iter()
        .map(|elem| {
            let cells = columns
                .iter()
                .map(|col| match elem {
                    Value::Object(map) => map
                        .get(col)
                        .map_or_else(|| Cell::new(CellValue::Absent), cell_fn),
                    _ => Cell::new(CellValue::Absent),
                })
                .collect();
            TableRow::new(cells)
        })
        .collect();
    FixtureTable::new(columns, rows)
}

/// Normalize a raw csv body into a [`FixtureTable`] (cells value-inferred via
/// [`cell_from_csv_token`], raw token kept as display). Shared by the fusion
/// NEW side and the csv OLD side.
fn table_from_csv_text(text: &str) -> FixtureTable {
    let keyed = parse_csv_rows(text);
    table_from_keyed_rows(&keyed, cell_from_csv_token)
}

/// Turn header-keyed string rows into a [`FixtureTable`], building each cell
/// through `cell_fn` (authored display + canonical key). Columns are the
/// first-seen union of keys; a row that lacks a column → a `Cell` with an
/// [`Absent`](CellValue::Absent) key.
fn table_from_keyed_rows(
    keyed: &[Vec<(String, String)>],
    cell_fn: fn(&str) -> Cell,
) -> FixtureTable {
    let mut columns: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in keyed {
        for (k, _) in row {
            if seen.insert(k.clone()) {
                columns.push(k.clone());
            }
        }
    }
    let rows = keyed
        .iter()
        .map(|row| {
            let cells = columns
                .iter()
                .map(|col| {
                    row.iter()
                        .find(|(k, _)| k == col)
                        .map_or_else(|| Cell::new(CellValue::Absent), |(_, v)| cell_fn(v))
                })
                .collect();
            TableRow::new(cells)
        })
        .collect();
    FixtureTable::new(columns, rows)
}

/// Remove the common leading-whitespace prefix from every non-blank line of
/// a region (a csv `rows: |` block-scalar arrives indented). Blank lines are
/// preserved (they carry no indentation signal). Used only by the csv OLD
/// path so [`parse_csv_rows`] sees a flush-left body.
fn dedent(region: &str) -> String {
    let min_indent = region
        .split('\n')
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    region
        .split('\n')
        .map(|l| {
            if l.trim().is_empty() {
                l
            } else {
                &l[min_indent.min(l.len() - l.trim_start().len())..]
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[allow(clippy::pedantic, clippy::cargo)]
mod tests {
    use super::*;
    use serde_json::json;

    // ----- A. Cell typing / semantic equality (the chokepoint) -----

    #[test]
    fn a1_typed_one_is_number_one_across_both_entries() {
        // type_cell_value(json!(1)) and type_cell_scalar("1") agree.
        assert_eq!(type_cell_value(&json!(1)), CellValue::Number("1".into()));
        assert_eq!(type_cell_scalar("1"), CellValue::Number("1".into()));
    }

    #[test]
    fn a2_canonicalize_numbers_strip_trailing_zeros() {
        // 1.0 / 1 / 1.00 all → "1"; 1.50 → "1.5"; 0.85 → "0.85";
        // 1000.0 → "1000"; -1 → "-1".
        assert_eq!(type_cell_value(&json!(1.0)), CellValue::Number("1".into()));
        assert_eq!(type_cell_value(&json!(1)), CellValue::Number("1".into()));
        assert_eq!(type_cell_scalar("1.00"), CellValue::Number("1".into()));
        assert_eq!(type_cell_scalar("1.50"), CellValue::Number("1.5".into()));
        assert_eq!(type_cell_scalar("0.85"), CellValue::Number("0.85".into()));
        assert_eq!(
            type_cell_value(&json!(1000.0)),
            CellValue::Number("1000".into())
        );
        assert_eq!(type_cell_value(&json!(-1)), CellValue::Number("-1".into()));
        // The float-token path: a `.`-bearing token that is whole-valued.
        assert_eq!(type_cell_scalar("1000.0"), CellValue::Number("1000".into()));
        // -0 normalizes to 0.
        assert_eq!(type_cell_scalar("-0.0"), CellValue::Number("0".into()));
    }

    #[test]
    fn a2b_canonicalize_float_never_emits_scientific_notation() {
        // Defensive invariant (Gemini PR #130 review): `canonicalize_float`
        // builds on `format!("{f}")`, whose `f64` `Display` is ALWAYS
        // positional decimal — it NEVER emits an `e`/`E` exponent (unlike
        // `ryu` or the `{:e}` formatter). Therefore the `.contains('.')`-gated
        // `trim_end_matches('0')` can only ever strip trailing *decimal*
        // zeros; it can never truncate an exponent. These extreme magnitudes
        // would round-trip through scientific notation under `{:e}`, so they
        // pin that the positional-Display assumption holds.
        for f in [
            1e-300_f64,
            1e300_f64,
            1.5e-20_f64,
            -1e-300_f64,
            f64::MIN_POSITIVE,
        ] {
            let s = canonicalize_float(f);
            assert!(
                !s.contains('e') && !s.contains('E'),
                "canonicalize_float({f}) = {s:?} must not contain an exponent"
            );
            // And it must remain parseable as the same finite magnitude.
            let back: f64 = s.parse().expect("canonicalized float re-parses");
            assert_eq!(
                back, f,
                "canonicalize_float must not lose magnitude for {f}"
            );
        }
        // A genuinely large whole-valued float prints as a long integer with
        // no decimal point and no exponent.
        let whole = canonicalize_float(1e30_f64);
        assert!(!whole.contains('.') && !whole.contains('e') && !whole.contains('E'));
    }

    #[test]
    fn a3_large_integer_survives_without_f64_mangling() {
        // 9007199254740993 = 2^53 + 1 — unrepresentable exactly as f64.
        let big = 9_007_199_254_740_993_i64;
        assert_eq!(
            type_cell_value(&json!(big)),
            CellValue::Number("9007199254740993".into()),
            "integer path must preserve >2^53 ints exactly"
        );
        assert_eq!(
            type_cell_scalar("9007199254740993"),
            CellValue::Number("9007199254740993".into()),
        );
    }

    #[test]
    fn a4_dict_path_strings_stay_str_never_coerced_to_number() {
        // The DICT-path string-stays-Str guarantee (the format discriminator,
        // cute-dbt#127): a manifest `Value::String("1")` and a quoted YAML
        // dict scalar `'1'`/`"1"` are DELIBERATE strings — never re-coerced.
        // (The csv path now DOES infer — see a4b.)
        assert_eq!(type_cell_value(&json!("1")), CellValue::Str("1".into()));
        // OLD quoted '1' / "1" → Str("1").
        assert_eq!(type_cell_scalar("'1'"), CellValue::Str("1".into()));
        assert_eq!(type_cell_scalar("\"1\""), CellValue::Str("1".into()));
    }

    #[test]
    fn a4b_csv_token_infers_number_and_bool_not_str() {
        // cute-dbt#127: a csv field `1` is warehouse-numeric, so it now types
        // as Number("1") (NOT Str), matching fusion's csv parse-ladder. The
        // flipped canonical RED of the old a4.
        assert_eq!(type_csv_token("1"), CellValue::Number("1".into()));
        // Bool inference too (case-insensitive — see a6b).
        assert_eq!(type_csv_token("true"), CellValue::Bool(true));
        assert_eq!(type_csv_token("false"), CellValue::Bool(false));
        // A genuine non-numeric/non-bool csv string stays Str verbatim.
        assert_eq!(type_csv_token("alice"), CellValue::Str("alice".into()));
    }

    #[test]
    fn a5_empty_and_null_tokens_map_to_null() {
        assert_eq!(type_cell_scalar(""), CellValue::Null);
        assert_eq!(type_cell_value(&json!(null)), CellValue::Null);
        assert_eq!(type_cell_scalar("null"), CellValue::Null);
        assert_eq!(type_cell_scalar("~"), CellValue::Null);
        assert_eq!(type_csv_token(""), CellValue::Null);
        // A QUOTED "null" is the literal string, not Null.
        assert_eq!(type_cell_scalar("'null'"), CellValue::Str("null".into()));
        // DOCUMENTED DIVERGENCE 1 (cute-dbt#127): fusion coerces the csv text
        // "null"/"NULL" → SQL NULL; cute-dbt keeps it as the literal Str so a
        // diff cell can render the word "null". (The common empty-field=Null
        // case is still zero-diff.)
        assert_eq!(type_csv_token("null"), CellValue::Str("null".into()));
        assert_eq!(type_csv_token("NULL"), CellValue::Str("NULL".into()));
    }

    #[test]
    fn a6_bool_boundary_lowercase_only() {
        assert_eq!(type_cell_scalar("true"), CellValue::Bool(true));
        assert_eq!(type_cell_scalar("false"), CellValue::Bool(false));
        assert_eq!(type_cell_value(&json!(true)), CellValue::Bool(true));
        // "True" / "TRUE" stay Str (conservative documented boundary).
        assert_eq!(type_cell_scalar("True"), CellValue::Str("True".into()));
        assert_eq!(type_cell_scalar("TRUE"), CellValue::Str("TRUE".into()));
    }

    #[test]
    fn a7_partial_number_parse_stays_str() {
        assert_eq!(type_cell_scalar("1px"), CellValue::Str("1px".into()));
        assert_eq!(type_cell_scalar("1,000"), CellValue::Str("1,000".into()));
    }

    #[test]
    fn a7b_one_sided_leading_quote_is_not_a_quoted_scalar() {
        // A token with a quote on ONLY the leading end is NOT a matching
        // quote pair, so `is_quoted` is false and the token is NOT
        // quote-stripped — it stays the verbatim token (a non-numeric string
        // because the stray quote defeats the i128 parse). Pins BOTH `&&`
        // operators in `is_quoted`: were either flipped to `||`, a single
        // leading quote would falsely classify the token as quoted and
        // `parse_yaml_scalar` would strip it (`"5` → `5`, `'5` → `5`),
        // collapsing two distinct cell values to one (a silent miss).
        assert_eq!(type_cell_scalar("\"5"), CellValue::Str("\"5".into()));
        assert_eq!(type_cell_scalar("'5"), CellValue::Str("'5".into()));
    }

    #[test]
    fn a8_absent_is_distinct_from_null() {
        // A sparse dict: row 1 has {a,b}, row 2 has only {a} → row 2's b is
        // Absent, NOT Null. And Absent != Null as CellValues.
        let elems = vec![json!({"a": 1, "b": 2}), json!({"a": 3})];
        let table = table_from_value_objects(&elems, cell_from_value);
        assert_eq!(table.columns, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(table.rows[1].cells[1].key, CellValue::Absent);
        assert_ne!(CellValue::Absent, CellValue::Null);
    }

    #[test]
    fn array_and_object_cell_values_become_compact_json_str() {
        assert_eq!(
            type_cell_value(&json!([1, 2])),
            CellValue::Str("[1,2]".into())
        );
        assert_eq!(
            type_cell_value(&json!({"k": 1})),
            CellValue::Str("{\"k\":1}".into())
        );
    }

    // ----- B. Format-only / engine-only = EQUAL table (headline) -----

    #[test]
    fn b9_dict_value_vs_block_yaml_yields_equal_table() {
        // The same logical data as a manifest dict Value vs a reconstructed
        // block-style YAML region → EQUAL FixtureTable.
        let manifest = json!([
            {"id": 1, "name": "alice"},
            {"id": 2, "name": "bob"}
        ]);
        let new_tbl = table_from_manifest_rows(&manifest, Some("dict")).unwrap();

        let yaml_region =
            "      - id: 1\n        name: 'alice'\n      - id: 2\n        name: 'bob'";
        let old_tbl = table_from_yaml_fragment(yaml_region, Some("dict")).unwrap();

        assert_eq!(
            new_tbl, old_tbl,
            "dict-Value and block-YAML of the same data must be equal"
        );
    }

    #[test]
    fn b10_csv_fusion_string_vs_core_string_dicts_yields_equal_table() {
        // fusion csv-as-raw-string vs core csv-as-array-of-string-dicts of
        // identical data → EQUAL FixtureTable (cells value-inferred + equal on
        // both engine encodings; cute-dbt#127).
        let fusion = json!("id,name\n1,alice\n2,bob\n");
        let fusion_tbl = table_from_manifest_rows(&fusion, Some("csv")).unwrap();

        let core = json!([
            {"id": "1", "name": "alice"},
            {"id": "2", "name": "bob"}
        ]);
        let core_tbl = table_from_manifest_rows(&core, Some("csv")).unwrap();

        assert_eq!(
            fusion_tbl, core_tbl,
            "the two engine encodings of identical csv data must be equal"
        );
        // cute-dbt#127: the numeric `id` cell now CONVERGES to Number on both
        // engine encodings (fusion raw-string AND core string-dicts both go
        // through type_csv_token). The convergence is stronger than the old
        // both-Str: a format-only reformat is a true no-op.
        assert_eq!(
            fusion_tbl.rows[0].cells[0].key,
            CellValue::Number("1".into())
        );
    }

    // ----- B'. cute-dbt#127: dict↔csv reformat is a value no-op -----

    /// The headline acceptance matrix: a single-cell dict fixture and a
    /// single-cell csv fixture carrying the SAME logical value must produce
    /// the SAME [`CellValue`], so a dict→csv (or csv→dict) reformat with equal
    /// values is a zero data diff. Each case pairs a dict-side encoding (an
    /// already-typed JSON scalar, `format: dict`) against a csv-side encoding
    /// (an all-string dict, `format: csv`) and asserts both → the expected
    /// `CellValue`. The negative cases pin where the convergence MUST NOT hold
    /// (the format discriminator + the documented "null"-text divergence).
    #[test]
    fn b127_dict_csv_equality_matrix() {
        // Build a one-row, one-column table for `id` and pull out cell[0][0].
        fn cell(rows: &Value, fmt: &str) -> CellValue {
            let t = table_from_manifest_rows(rows, Some(fmt)).unwrap();
            t.rows[0].cells[0].key.clone()
        }
        // dict side: a typed JSON scalar value for `id`.
        fn dict(v: Value) -> CellValue {
            cell(&json!([{ "id": v }]), "dict")
        }
        // csv side: dbt-core encodes csv as an all-STRING dict.
        fn csv(token: &str) -> CellValue {
            cell(&json!([{ "id": token }]), "csv")
        }

        // 1–3: 1 / 1.0 / 1.00 all converge to Number("1") on BOTH formats.
        assert_eq!(dict(json!(1)), CellValue::Number("1".into()));
        assert_eq!(csv("1"), CellValue::Number("1".into()));
        assert_eq!(dict(json!(1.0)), CellValue::Number("1".into()));
        assert_eq!(csv("1.0"), CellValue::Number("1".into()));
        assert_eq!(csv("1.00"), CellValue::Number("1".into()));
        // 4–5: 1.50 / 1.5 → Number("1.5").
        assert_eq!(csv("1.50"), CellValue::Number("1.5".into()));
        assert_eq!(csv("1.5"), CellValue::Number("1.5".into()));
        assert_eq!(dict(json!(1.5)), CellValue::Number("1.5".into()));
        // 6: true / "true" (case-insensitive) → Bool(true) on both.
        assert_eq!(dict(json!(true)), CellValue::Bool(true));
        assert_eq!(csv("true"), CellValue::Bool(true));
        assert_eq!(csv("TRUE"), CellValue::Bool(true));
        assert_eq!(csv("True"), CellValue::Bool(true));
        assert_eq!(csv("false"), CellValue::Bool(false));
        assert_eq!(csv("FALSE"), CellValue::Bool(false));
        // 7: "" / null → Null on both (csv empty field; dict JSON null).
        assert_eq!(dict(json!(null)), CellValue::Null);
        assert_eq!(csv(""), CellValue::Null);
        // 8: 007 → Number("7") (leading zeros collapse via i128).
        assert_eq!(csv("007"), CellValue::Number("7".into()));
        // 9: 1e3 → Number("1000") (scientific notation, finite f64 path).
        assert_eq!(csv("1e3"), CellValue::Number("1000".into()));
        assert_eq!(dict(json!(1e3)), CellValue::Number("1000".into()));
        // 10: -0 → Number("0").
        assert_eq!(csv("-0"), CellValue::Number("0".into()));
        assert_eq!(csv("-0.0"), CellValue::Number("0".into()));
        // 11: 2^63 (i64::MAX + 1 = 9223372036854775808) survives via i128.
        assert_eq!(
            csv("9223372036854775808"),
            CellValue::Number("9223372036854775808".into()),
        );
        assert_eq!(
            dict(json!(9_223_372_036_854_775_808_u64)),
            CellValue::Number("9223372036854775808".into()),
        );
        // 12: a genuine string stays Str on both formats.
        assert_eq!(dict(json!("alice")), CellValue::Str("alice".into()));
        assert_eq!(csv("alice"), CellValue::Str("alice".into()));

        // 13a: NEGATIVE — the dict-quoted-numeric safe FALSE positive. A
        // DICT-format string `"1"` is a DELIBERATE string and must STAY Str —
        // it must NOT converge with csv Number("1"). Format is the only
        // discriminator.
        assert_eq!(dict(json!("1")), CellValue::Str("1".into()));
        assert_ne!(dict(json!("1")), csv("1"));
        // 13b: NEGATIVE — the "null"-text divergence (documented). csv text
        // "null" stays Str (so a diff cell can show the literal word); only an
        // empty field is Null. A dict JSON string "null" is likewise Str.
        assert_eq!(csv("null"), CellValue::Str("null".into()));
        assert_eq!(dict(json!("null")), CellValue::Str("null".into()));
    }

    /// The load-bearing format discriminator (cute-dbt#127, DELTA 2): the
    /// EXACT SAME `Value::Array` of string dicts (`[{"id":"1"}]`) routes
    /// through `table_from_value_objects` and types its `"1"` cell
    /// DIFFERENTLY by `format` alone — `Some("dict")` → `Str` (deliberate
    /// string), `Some("csv")` → `Number` (warehouse-numeric inference). Pins
    /// that the Array arm threads a format-aware `type_fn`, not a fixed one.
    #[test]
    fn b127_array_format_discriminator_dict_str_vs_csv_number() {
        let rows = json!([{ "id": "1" }]);
        let dict_tbl = table_from_manifest_rows(&rows, Some("dict")).unwrap();
        let csv_tbl = table_from_manifest_rows(&rows, Some("csv")).unwrap();
        assert_eq!(
            dict_tbl.rows[0].cells[0].key,
            CellValue::Str("1".into()),
            "a deliberately-quoted dict '1' stays Str"
        );
        assert_eq!(
            csv_tbl.rows[0].cells[0].key,
            CellValue::Number("1".into()),
            "the same array under csv format infers Number"
        );
        assert_ne!(dict_tbl, csv_tbl, "format is the only discriminator");
    }

    // ----- B''. cute-dbt#127 mutation kills (one per new branch) -----

    /// Kill the `canonicalize_str_number → Number` branch of the new
    /// type_csv_token ladder. Were the Number arm dropped, `"50.00"` would
    /// fall through to `Str("50.00")` and this miscompares.
    #[test]
    fn b127_kill_csv_number_branch() {
        assert_eq!(type_csv_token("50.00"), CellValue::Number("50".into()));
        assert_eq!(type_csv_token("0.85"), CellValue::Number("0.85".into()));
        assert_eq!(type_csv_token("-1"), CellValue::Number("-1".into()));
    }

    /// Kill the `true` Bool arm AND its `eq_ignore_ascii_case` (vs `==`).
    /// `"TRUE"` only types Bool if the compare is case-insensitive; were it
    /// `== "true"`, `"TRUE"` would wrongly stay Str.
    #[test]
    fn b127_kill_csv_bool_true_branch_is_case_insensitive() {
        assert_eq!(type_csv_token("true"), CellValue::Bool(true));
        assert_eq!(type_csv_token("TRUE"), CellValue::Bool(true));
        assert_eq!(type_csv_token("True"), CellValue::Bool(true));
        // A non-bool word stays Str (the arm is not over-greedy).
        assert_eq!(type_csv_token("truee"), CellValue::Str("truee".into()));
    }

    /// Kill the `false` Bool arm AND its `eq_ignore_ascii_case`. Distinct test
    /// from the `true` arm so dropping EITHER bool branch is caught.
    #[test]
    fn b127_kill_csv_bool_false_branch_is_case_insensitive() {
        assert_eq!(type_csv_token("false"), CellValue::Bool(false));
        assert_eq!(type_csv_token("FALSE"), CellValue::Bool(false));
        assert_eq!(type_csv_token("False"), CellValue::Bool(false));
        assert_eq!(type_csv_token("falsey"), CellValue::Str("falsey".into()));
    }

    /// Kill the empty-field `Null` arm: it must fire BEFORE the number ladder
    /// (an empty token is not numeric, but the early return is the documented
    /// empty==null rule and the JS-twin contract).
    #[test]
    fn b127_kill_csv_empty_null_branch() {
        assert_eq!(type_csv_token(""), CellValue::Null);
        assert_ne!(type_csv_token(""), CellValue::Str(String::new()));
    }

    /// Look up a row's cell by column name (serde_json's `Map` is a
    /// `BTreeMap`, so the column order is alphabetical, not insertion order).
    fn cell_by_col(t: &FixtureTable, row: usize, col: &str) -> CellValue {
        let idx = t.columns.iter().position(|c| c == col).expect("column");
        t.rows[row].cells[idx].key.clone()
    }

    /// Kill the csv-Array routing in `table_from_value_objects`: the threaded
    /// `type_csv_value` must apply `type_csv_token` to a string cell. Were the
    /// Array arm hardwired to `type_cell_value` (the pre-#127 behavior), a
    /// core-csv `"1"` string cell would stay `Str`. This drives the routing
    /// (not just `type_csv_token` in isolation), so a mutant that ignores the
    /// threaded `type_fn` is caught.
    #[test]
    fn b127_kill_csv_value_object_routing() {
        let core = json!([{ "n": "42", "flag": "true", "label": "ok" }]);
        let t = table_from_manifest_rows(&core, Some("csv")).unwrap();
        assert_eq!(cell_by_col(&t, 0, "n"), CellValue::Number("42".into()));
        assert_eq!(cell_by_col(&t, 0, "flag"), CellValue::Bool(true));
        assert_eq!(cell_by_col(&t, 0, "label"), CellValue::Str("ok".into()));
    }

    /// Kill the `type_csv_value` non-string fallback: a non-string cell inside
    /// a csv-format Array (a JSON number `1`, defensive — core csv ships
    /// strings, but the shim must not panic or mis-route) routes to
    /// `type_cell_value` → Number. Pins the `else` arm of the shim.
    #[test]
    fn b127_kill_csv_value_object_non_string_fallback() {
        let mixed = json!([{ "n": 1, "label": "ok" }]);
        let t = table_from_manifest_rows(&mixed, Some("csv")).unwrap();
        assert_eq!(cell_by_col(&t, 0, "n"), CellValue::Number("1".into()));
        assert_eq!(cell_by_col(&t, 0, "label"), CellValue::Str("ok".into()));
    }

    // ----- G. Parsers -----

    /// The canonical RFC 4180 cases (since cute-dbt#138 the Rust parser is the
    /// sole implementation — the retired JS twin's cases live here now).
    #[test]
    fn g22_parse_csv_mirrors_headless_edge_cases() {
        // Helper: parse and project to a simple Vec<Vec<(k,v)>> string form.
        fn p(s: &str) -> Vec<Vec<(String, String)>> {
            parse_csv_rows(s)
        }
        // empty + header-only → [].
        assert_eq!(p(""), Vec::<Vec<(String, String)>>::new());
        assert_eq!(p("id,name"), Vec::<Vec<(String, String)>>::new());
        assert_eq!(p("id,name\n"), Vec::<Vec<(String, String)>>::new());
        // single row, no trailing newline.
        assert_eq!(
            p("id,name\n1,alice"),
            vec![vec![
                ("id".into(), "1".into()),
                ("name".into(), "alice".into())
            ]]
        );
        // single row, trailing LF (no spurious empty row).
        assert_eq!(
            p("id,name\n1,alice\n"),
            vec![vec![
                ("id".into(), "1".into()),
                ("name".into(), "alice".into())
            ]]
        );
        // CRLF as one terminator.
        assert_eq!(
            p("id,name\r\n1,alice\r\n2,bob\r\n"),
            vec![
                vec![("id".into(), "1".into()), ("name".into(), "alice".into())],
                vec![("id".into(), "2".into()), ("name".into(), "bob".into())],
            ]
        );
    }

    #[test]
    fn g23_parse_csv_quoted_comma() {
        assert_eq!(
            parse_csv_rows("id,name\n1,\"alice, the brave\"\n"),
            vec![vec![
                ("id".into(), "1".into()),
                ("name".into(), "alice, the brave".into())
            ]]
        );
    }

    #[test]
    fn g24_parse_csv_embedded_newline_in_quoted_field() {
        assert_eq!(
            parse_csv_rows("id,memo\n1,\"line one\nline two\"\n"),
            vec![vec![
                ("id".into(), "1".into()),
                ("memo".into(), "line one\nline two".into())
            ]]
        );
    }

    #[test]
    fn g25_parse_csv_double_quote_escape() {
        assert_eq!(
            parse_csv_rows("id,note\n1,\"she said \"\"hello\"\"\"\n"),
            vec![vec![
                ("id".into(), "1".into()),
                ("note".into(), "she said \"hello\"".into())
            ]]
        );
    }

    #[test]
    fn g26_parse_csv_short_row_fills_empty() {
        // A row shorter than the header fills missing trailing columns "".
        assert_eq!(
            parse_csv_rows("a,b,c\n1,2\n"),
            vec![vec![
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
                ("c".into(), "".into())
            ]]
        );
    }

    #[test]
    fn g26b_quote_only_opens_at_field_start_not_mid_field() {
        // A `"` is a quote-open ONLY at the START of a field (per RFC 4180);
        // a `"` mid-field is a literal character. Pins the
        // `self.field.is_empty()` match guard in feed_unquoted: were it
        // replaced with `true`, the mid-field `"` would open a quoted run,
        // swallow the rest of the field/line, and silently mangle the cell
        // value (here `a"b` would collapse to `b`).
        assert_eq!(
            parse_csv_rows("note\na\"b"),
            vec![vec![("note".into(), "a\"b".into())]],
            "a mid-field double-quote is literal, not a quote-open"
        );
    }

    #[test]
    fn g26c_lone_cr_terminates_one_row_each_not_a_crlf_pair() {
        // A lone CR (old-Mac line ending) NOT followed by LF terminates a
        // row consuming ONE char; only a true `\r\n` consumes two. Pins the
        // `c == '\r' && next == Some('\n')` CRLF check: were the `&&`
        // flipped to `||`, a lone `\r` (c == '\r', `||` short-circuits true)
        // would consume the FOLLOWING data char as part of the terminator,
        // dropping a cell — a silent miss.
        assert_eq!(
            parse_csv_rows("id\r1\r2"),
            vec![
                vec![("id".into(), "1".into())],
                vec![("id".into(), "2".into())],
            ],
            "lone CRs each terminate one row; no data char is eaten"
        );
    }

    #[test]
    fn g28a_comment_line_inside_a_row_is_skipped_not_a_field() {
        // A `#` comment line within a row region is skipped, NOT parsed as a
        // field — even when it superficially looks like `key: value`. Pins the
        // `trimmed.is_empty() || trimmed.starts_with('#')` skip-guard in
        // feed_line: were the `||` flipped to `&&`, the guard would never fire
        // (no string is both empty AND `#`-prefixed) and `# note: x` would be
        // appended as a spurious `# note` column — a phantom cell.
        let region = "        - id: 1\n          # note: ignore me\n          name: bob";
        let rows = parse_block_dict_rows(region);
        assert_eq!(rows.len(), 1, "one row");
        assert_eq!(
            rows[0],
            vec![("id".into(), "1".into()), ("name".into(), "bob".into())],
            "the comment line must not contribute a `# note` cell"
        );
    }

    #[test]
    fn g28b_field_attribution_uses_leading_indent_not_line_length() {
        // A field deeper than its row's `- ` line is attributed to that row;
        // the depth test is on LEADING-WHITESPACE width, not on line length.
        // Pins `indent = line.len() - trimmed.len()` in feed_line: were the
        // `-` flipped to `+`, a long row opener would get an inflated pseudo
        // "indent" exceeding a genuinely-deeper (but shorter) field line, so
        // `append_field`'s `indent <= ri` would wrongly drop the field — the
        // cell would silently vanish. The row opener is deliberately long and
        // the field line short so the length-based mutant inverts the test.
        let region = "- aaaaaaaaaa: 1\n  b: 2";
        let rows = parse_block_dict_rows(region);
        assert_eq!(rows.len(), 1, "one row");
        assert_eq!(
            rows[0],
            vec![("aaaaaaaaaa".into(), "1".into()), ("b".into(), "2".into())],
            "the deeper `b: 2` field must be attributed to the row"
        );
    }

    #[test]
    fn g29_parse_block_dict_two_rows_with_quotes_and_negative() {
        // The dim_payer-shape: `- payer_key: -1` + a sibling quoted key.
        let region = "      - payer_key: -1\n        payer_id: 'UNKNOWN'\n      - payer_key: 2\n        payer_id: 'ACME'";
        let rows = parse_block_dict_rows(region);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], ("payer_key".into(), "-1".into()));
        assert_eq!(rows[0][1], ("payer_id".into(), "'UNKNOWN'".into()));
        // Through the normalizer: -1 → Number, 'UNKNOWN' → Str (stripped).
        let table = table_from_yaml_fragment(region, Some("dict")).unwrap();
        assert_eq!(
            table.columns,
            vec!["payer_key".to_string(), "payer_id".to_string()]
        );
        assert_eq!(table.rows[0].cells[0].key, CellValue::Number("-1".into()));
        assert_eq!(table.rows[0].cells[1].key, CellValue::Str("UNKNOWN".into()));
    }

    #[test]
    fn g30_parse_inline_flow_protects_quoted_comma() {
        // `- {id: 1, name: alice}` → 2 cells.
        let row = parse_inline_flow_row("      - {id: 1, name: alice}");
        assert_eq!(
            row,
            vec![("id".into(), "1".into()), ("name".into(), "alice".into())]
        );
        // `- {note: 'a, b'}` → ONE cell whose value is the quoted "a, b".
        let row = parse_inline_flow_row("      - {note: 'a, b'}");
        assert_eq!(row, vec![("note".into(), "'a, b'".into())]);
        // Through the normalizer the quoted value strips to Str("a, b").
        let table = table_from_yaml_fragment("      - {note: 'a, b'}", Some("dict")).unwrap();
        assert_eq!(table.rows[0].cells[0].key, CellValue::Str("a, b".into()));
    }

    #[test]
    fn g30b_inline_flow_protects_escaped_quotes_inside_values() {
        // Regression (CodeRabbit PR #130): an escaped quote inside an
        // inline-flow value must NOT prematurely close the quoted run and
        // split the row at a following comma into phantom cells.

        // Doubled single-quote (`''` = a literal `'`): `'it''s, ok'` is ONE
        // value, not two cells.
        let row = parse_inline_flow_row("      - {note: 'it''s, ok'}");
        assert_eq!(row, vec![("note".into(), "'it''s, ok'".into())]);

        // Backslash-escaped double-quote: `"a\", b"` is ONE value.
        let row = parse_inline_flow_row("      - {note: \"a\\\", b\"}");
        assert_eq!(row, vec![("note".into(), "\"a\\\", b\"".into())]);

        // A sibling key after the escaped value still parses as its own cell.
        let row = parse_inline_flow_row("      - {note: 'it''s', id: 1}");
        assert_eq!(
            row,
            vec![("note".into(), "'it''s'".into()), ("id".into(), "1".into())]
        );
    }

    #[test]
    fn inline_flow_within_block_dict_parser_is_detected() {
        // parse_block_dict_rows routes a `- {` line to the inline parser.
        let region = "  - {id: 1, name: alice}\n  - {id: 2, name: bob}";
        let rows = parse_block_dict_rows(region);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1],
            vec![("id".into(), "2".into()), ("name".into(), "bob".into())]
        );
    }

    // ----- I. (IR half) sql-opaque yields no cells -----

    #[test]
    fn i34_non_literal_sql_format_manifest_rows_returns_none() {
        // A NON-literal sql (a real FROM clause) is opaque → None → the
        // cute-dbt#96 sql/text fallback. (A literal-row SELECT now tabulates;
        // see the K-series.) cute-dbt#137 narrowed this from "all sql → None".
        let sql = json!("SELECT id, name FROM src");
        assert_eq!(table_from_manifest_rows(&sql, Some("sql")), None);
        // And the OLD-side sql path too.
        assert_eq!(table_from_yaml_fragment("anything", Some("sql")), None);
    }

    #[test]
    fn null_and_empty_array_rows_become_empty_table() {
        assert_eq!(
            table_from_manifest_rows(&json!(null), Some("dict")),
            Some(FixtureTable::default())
        );
        assert_eq!(
            table_from_manifest_rows(&json!([]), Some("dict")),
            Some(FixtureTable::new(Vec::new(), Vec::new()))
        );
    }

    #[test]
    fn non_csv_string_rows_is_opaque_none() {
        // A bare string with no/dict format is not a table.
        assert_eq!(table_from_manifest_rows(&json!("not a table"), None), None);
        assert_eq!(
            table_from_manifest_rows(&json!("not a table"), Some("dict")),
            None
        );
    }

    #[test]
    fn object_rows_value_is_graceful_none() {
        // rows: { ... } (not an array) → None, no panic.
        assert_eq!(
            table_from_manifest_rows(&json!({"id": 1}), Some("dict")),
            None
        );
    }

    #[test]
    fn csv_old_side_dedents_indented_block_scalar() {
        // An indented `rows: |` body dedents before the csv parse.
        let region = "        id,name\n        1,alice\n        2,bob";
        let table = table_from_yaml_fragment(region, Some("csv")).unwrap();
        assert_eq!(table.columns, vec!["id".to_string(), "name".to_string()]);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells[1].key, CellValue::Str("alice".into()));
    }

    #[test]
    fn dedent_strips_only_the_common_prefix_and_preserves_deeper_indent() {
        // `dedent` removes the SMALLEST leading-whitespace prefix shared by
        // all non-blank lines; lines indented deeper than that minimum keep
        // their residual indentation. Pins the indent arithmetic in `dedent`:
        // both `l.len() - l.trim_start().len()` subtractions compute a line's
        // leading-whitespace width. Were either flipped to `+`, `min_indent`
        // (or the per-line clamp) would be wildly inflated and every line
        // would be stripped to flush-left, dropping a deeper line's residual
        // indentation — a silent over-dedent.
        //
        // header at 2-space indent, data row at 4-space indent → common
        // prefix is 2; the data row must keep 2 residual leading spaces.
        let region = "  id\n    1";
        let out = dedent(region);
        assert_eq!(
            out, "id\n  1",
            "common prefix (2) stripped; 2 residual kept"
        );
        // A uniform region dedents fully (the min == each line's indent).
        assert_eq!(dedent("    a\n    b"), "a\nb");
        // A blank line carries no indent signal and is preserved verbatim.
        assert_eq!(dedent("    a\n\n    b"), "a\n\nb");
    }

    // ----- Cross-source symmetry guard (the headline kill) -----

    #[test]
    fn csv_dedented_old_equals_fusion_string_new() {
        // The csv OLD region (indented block-scalar) and the fusion NEW
        // string of the same data → EQUAL FixtureTable.
        let fusion = json!("id,name\n1,alice\n2,bob\n");
        let new_tbl = table_from_manifest_rows(&fusion, Some("csv")).unwrap();
        let region = "        id,name\n        1,alice\n        2,bob";
        let old_tbl = table_from_yaml_fragment(region, Some("csv")).unwrap();
        assert_eq!(new_tbl, old_tbl);
    }

    // ----- J. Wire-shape / serde round-trip -----

    #[test]
    fn j37_cellvalue_wire_tokens_are_exact() {
        // Adjacent tagging: {"t":"number","v":"1"}, {"t":"absent"}, etc.
        assert_eq!(
            serde_json::to_string(&CellValue::Number("1".into())).unwrap(),
            r#"{"t":"number","v":"1"}"#
        );
        assert_eq!(
            serde_json::to_string(&CellValue::Str("x".into())).unwrap(),
            r#"{"t":"str","v":"x"}"#
        );
        assert_eq!(
            serde_json::to_string(&CellValue::Bool(true)).unwrap(),
            r#"{"t":"bool","v":true}"#
        );
        assert_eq!(
            serde_json::to_string(&CellValue::Null).unwrap(),
            r#"{"t":"null"}"#
        );
        assert_eq!(
            serde_json::to_string(&CellValue::Absent).unwrap(),
            r#"{"t":"absent"}"#
        );
    }

    #[test]
    fn j37_fixtureformat_wire_tokens_are_lowercase() {
        assert_eq!(
            serde_json::to_string(&FixtureFormat::Dict).unwrap(),
            r#""dict""#
        );
        assert_eq!(
            serde_json::to_string(&FixtureFormat::Csv).unwrap(),
            r#""csv""#
        );
        assert_eq!(
            serde_json::to_string(&FixtureFormat::Sql).unwrap(),
            r#""sql""#
        );
    }

    #[test]
    fn j37_cellvalue_roundtrips() {
        for v in [
            CellValue::Null,
            CellValue::Bool(false),
            CellValue::Number("-42".into()),
            CellValue::Str("hi".into()),
            CellValue::Absent,
        ] {
            let back: CellValue =
                serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
            assert_eq!(back, v);
        }
    }

    #[test]
    fn j37_fixturetable_roundtrips() {
        let table = FixtureTable::new(
            vec!["id".into(), "name".into()],
            vec![
                TableRow::new(vec![
                    Cell::new(CellValue::Number("1".into())),
                    Cell::new(CellValue::Str("alice".into())),
                ]),
                TableRow::new(vec![
                    Cell::new(CellValue::Number("2".into())),
                    Cell::new(CellValue::Absent),
                ]),
            ],
        );
        let back: FixtureTable =
            serde_json::from_str(&serde_json::to_string(&table).unwrap()).unwrap();
        assert_eq!(back, table);
    }

    #[test]
    fn fixtureformat_from_opt_defaults_and_maps() {
        assert_eq!(FixtureFormat::from_opt(None), FixtureFormat::Dict);
        assert_eq!(FixtureFormat::from_opt(Some("dict")), FixtureFormat::Dict);
        assert_eq!(FixtureFormat::from_opt(Some("csv")), FixtureFormat::Csv);
        assert_eq!(FixtureFormat::from_opt(Some("sql")), FixtureFormat::Sql);
        // Unrecognized → Dict (tolerant).
        assert_eq!(FixtureFormat::from_opt(Some("yaml")), FixtureFormat::Dict);
    }

    // -----------------------------------------------------------------
    // K. SQL literal-row parser (cute-dbt#137) — the conservative-reject
    //    boundary table.
    // -----------------------------------------------------------------

    /// Read parsed cells as `(column, display, key)` triples for assertions.
    fn sql_cells(sql: &str) -> Vec<Vec<(String, String, CellValue)>> {
        let t = parse_sql_literal_rows(sql).expect("expected an accepted literal-row table");
        t.rows
            .iter()
            .map(|r| {
                t.columns
                    .iter()
                    .zip(r.cells.iter())
                    .map(|(c, cell)| (c.clone(), cell.display.clone(), cell.key.clone()))
                    .collect()
            })
            .collect()
    }

    // ----- K1. accept: single-arm, all literal kinds -----

    #[test]
    fn k1_single_arm_canonical_dbt_shape_accepts() {
        let cells = sql_cells("select 1 as id, 'alice' as name");
        assert_eq!(cells.len(), 1);
        assert_eq!(
            cells[0],
            vec![
                ("id".into(), "1".into(), CellValue::Number("1".into())),
                (
                    "name".into(),
                    "alice".into(),
                    CellValue::Str("alice".into())
                ),
            ]
        );
    }

    #[test]
    fn k1b_all_literal_kinds_type_correctly() {
        // number, single-quoted string, TRUE/FALSE (case-insensitive),
        // NULL, negative + decimal + scientific numbers.
        let cells = sql_cells(
            "SELECT 42 AS n, 'hi' AS s, TRUE AS t, false AS f, NULL AS z, -1 AS neg, 1.5 AS dec, 1e3 AS sci",
        );
        let row = &cells[0];
        assert_eq!(row[0].2, CellValue::Number("42".into()));
        assert_eq!(row[1].2, CellValue::Str("hi".into()));
        assert_eq!(row[2].2, CellValue::Bool(true));
        assert_eq!(row[3].2, CellValue::Bool(false));
        assert_eq!(row[4].2, CellValue::Null);
        assert_eq!(row[5].2, CellValue::Number("-1".into()));
        assert_eq!(row[6].2, CellValue::Number("1.5".into()));
        assert_eq!(row[7].2, CellValue::Number("1000".into()));
        // TRUE/false keep their authored display case.
        assert_eq!(row[2].1, "TRUE");
        assert_eq!(row[3].1, "false");
        // A NULL literal displays empty (renderer styles from key.t).
        assert_eq!(row[4].1, "");
    }

    #[test]
    fn k1c_alias_without_as_keyword_accepts() {
        // `AS` is optional: `select 1 id` is valid SQL.
        let cells = sql_cells("select 1 id, 2 qty");
        assert_eq!(
            cells[0],
            vec![
                ("id".into(), "1".into(), CellValue::Number("1".into())),
                ("qty".into(), "2".into(), CellValue::Number("2".into())),
            ]
        );
    }

    #[test]
    fn k1d_string_literal_1_stays_str_not_number() {
        // A SQL `'1'` is a deliberate string literal — never re-coerced.
        let cells = sql_cells("select '1' as code");
        assert_eq!(cells[0][0].2, CellValue::Str("1".into()));
        assert_eq!(cells[0][0].1, "1");
    }

    #[test]
    fn k1e_single_quote_escape_doubled_quote() {
        // `''` inside a single-quoted string is a literal `'`.
        let cells = sql_cells("select 'it''s' as note");
        assert_eq!(cells[0][0].2, CellValue::Str("it's".into()));
        assert_eq!(cells[0][0].1, "it's");
    }

    // ----- K2. accept: UNION ALL (positional) -----

    #[test]
    fn k2_union_all_multi_arm_accepts_positionally() {
        let cells = sql_cells("select 1 as id union all select 2 as id union all select 3 as id");
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0][0].2, CellValue::Number("1".into()));
        assert_eq!(cells[1][0].2, CellValue::Number("2".into()));
        assert_eq!(cells[2][0].2, CellValue::Number("3".into()));
    }

    #[test]
    fn k2b_union_all_is_case_insensitive() {
        let cells = sql_cells("select 1 as id UNION ALL select 2 as id");
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn k2c_union_all_uses_first_arm_aliases_positionally() {
        // A later arm's own alias text is ignored — columns come from arm 0.
        let t = parse_sql_literal_rows("select 1 as id union all select 2 as other").unwrap();
        assert_eq!(t.columns, vec!["id".to_string()]);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1].cells[0].key, CellValue::Number("2".into()));
    }

    #[test]
    fn k2d_union_all_mismatched_arm_width_rejects() {
        assert_eq!(
            parse_sql_literal_rows("select 1 as id union all select 2 as id, 3 as x"),
            None,
            "mismatched projection count across UNION ALL arms rejects"
        );
    }

    // ----- K3. comments (ignored, never reject) -----

    #[test]
    fn k3_line_comment_is_stripped_not_rejected() {
        let cells = sql_cells("select 1 as id -- trailing comment\nunion all select 2 as id");
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn k3b_block_comment_is_stripped_not_rejected() {
        let cells = sql_cells("select /* inline */ 1 as id, 2 as qty");
        assert_eq!(cells[0].len(), 2);
    }

    #[test]
    fn k3c_comment_marker_inside_string_literal_is_preserved() {
        // A `--` inside a single-quoted string is NOT a comment.
        let cells = sql_cells("select '-- not a comment' as note");
        assert_eq!(cells[0][0].2, CellValue::Str("-- not a comment".into()));
        // A `/*` inside a string is preserved too.
        let cells = sql_cells("select '/* literal */' as note");
        assert_eq!(cells[0][0].2, CellValue::Str("/* literal */".into()));
    }

    // ----- K4. reject: clauses -----

    #[test]
    fn k4_top_level_clauses_reject() {
        for sql in [
            "select id from src",
            "select 1 as id where id > 0",
            "select 1 as id group by id",
            "select 1 as id order by id",
            "select 1 as id limit 10",
            "select a.id from a join b on a.id = b.id",
            "select 1 as id having count(*) > 0",
        ] {
            assert_eq!(parse_sql_literal_rows(sql), None, "must reject: {sql}");
        }
    }

    // ----- K5. reject: non-UNION-ALL set ops -----

    #[test]
    fn k5_other_set_ops_reject() {
        for sql in [
            "select 1 as id union select 2 as id",
            "select 1 as id intersect select 2 as id",
            "select 1 as id except select 2 as id",
        ] {
            assert_eq!(
                parse_sql_literal_rows(sql),
                None,
                "must reject set op: {sql}"
            );
        }
    }

    // ----- K6. reject: non-literal projections -----

    #[test]
    fn k6_non_literal_projections_reject() {
        for sql in [
            "select 1 + 1 as x",                  // operator
            "select 1::int as x",                 // postgres cast
            "select cast(1 as int) as x",         // CAST(...)
            "select now() as x",                  // function call
            "select id as x",                     // bare-word column ref
            "select * ",                          // star
            "select \"quoted\" as x",             // double-quoted identifier
            "select (select 1) as x",             // subquery
            "select case when 1 then 2 end as x", // CASE
        ] {
            assert_eq!(
                parse_sql_literal_rows(sql),
                None,
                "must reject projection: {sql}"
            );
        }
    }

    // ----- K7. reject: missing alias / structural -----

    #[test]
    fn k7_missing_alias_rejects() {
        assert_eq!(parse_sql_literal_rows("select 1"), None, "no alias rejects");
        assert_eq!(
            parse_sql_literal_rows("select 1 as id, 2"),
            None,
            "one missing alias rejects the whole arm"
        );
    }

    #[test]
    fn k7b_empty_and_non_select_reject() {
        assert_eq!(parse_sql_literal_rows(""), None);
        assert_eq!(parse_sql_literal_rows("   "), None);
        assert_eq!(parse_sql_literal_rows("insert into t values (1)"), None);
        assert_eq!(
            parse_sql_literal_rows("select"),
            None,
            "SELECT with no proj"
        );
    }

    #[test]
    fn k7c_dotted_or_quoted_alias_rejects() {
        assert_eq!(parse_sql_literal_rows("select 1 as a.b"), None);
        assert_eq!(parse_sql_literal_rows("select 1 as \"id\""), None);
        assert_eq!(parse_sql_literal_rows("select 1 as 'id'"), None);
    }

    // ----- K8. the #40 scar re-check (must hold) -----

    #[test]
    fn k8_scar_select_quoted_from_tabulates() {
        // `select 'from' as col` — `'from'` is a quoted STRING literal, not
        // the FROM clause. Must tabulate.
        let cells = sql_cells("select 'from' as col");
        assert_eq!(cells[0][0].2, CellValue::Str("from".into()));
    }

    #[test]
    fn k8b_scar_from_clause_rejects() {
        // `from x, y` as a top-level clause → reject.
        assert_eq!(
            parse_sql_literal_rows("select c from x, y"),
            None,
            "a real FROM clause must reject"
        );
    }

    // ----- K9. normalizer wiring (both sides) -----

    #[test]
    fn k9_manifest_rows_sql_literal_tabulates() {
        let sql = json!("select 1 as id, 'alice' as name union all select 2 as id, 'bob' as name");
        let t = table_from_manifest_rows(&sql, Some("sql")).expect("literal sql tabulates");
        assert_eq!(t.columns, vec!["id".to_string(), "name".to_string()]);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1].cells[1].key, CellValue::Str("bob".into()));
    }

    #[test]
    fn k9b_manifest_rows_non_literal_sql_returns_none() {
        // A non-literal sql falls back (None) — the renderer shows the sql
        // code block.
        let sql = json!("select id, name from src");
        assert_eq!(table_from_manifest_rows(&sql, Some("sql")), None);
    }

    #[test]
    fn k9c_yaml_fragment_sql_literal_tabulates_dedented() {
        // The OLD-side path: an indented multi-line literal-row block scalar.
        let region = "          select\n            true as is_valid\n            , 1 as n";
        let t = table_from_yaml_fragment(region, Some("sql")).expect("literal sql tabulates");
        assert_eq!(t.columns, vec!["is_valid".to_string(), "n".to_string()]);
        assert_eq!(t.rows[0].cells[0].key, CellValue::Bool(true));
        assert_eq!(t.rows[0].cells[1].key, CellValue::Number("1".into()));
    }

    #[test]
    fn k9d_yaml_fragment_non_literal_sql_returns_none() {
        assert_eq!(
            table_from_yaml_fragment("select id from src", Some("sql")),
            None
        );
    }

    #[test]
    fn k9e_cross_source_sql_literal_equals_manifest_string() {
        // The OLD reconstructed region and the NEW manifest string of the
        // same literal-row SQL → EQUAL FixtureTable (cross-source symmetry).
        let new_tbl = table_from_manifest_rows(
            &json!("select 1 as id\nunion all select 2 as id"),
            Some("sql"),
        )
        .unwrap();
        let old_tbl = table_from_yaml_fragment(
            "          select 1 as id\n          union all select 2 as id",
            Some("sql"),
        )
        .unwrap();
        assert_eq!(new_tbl, old_tbl);
    }

    // ----- K10. external fixture FILE body (cute-dbt#126) -----

    #[test]
    fn external_fixture_csv_value_inferred_grid() {
        // A csv fixture file body tabulates header-keyed with the same
        // value inference as an inline csv String (#66/#127): the `amount`
        // tokens become Numbers, not Strs.
        let t = external_fixture_table("id,amount\n1,10\n2,20\n", Some("csv"))
            .expect("csv fixture tabulates");
        assert_eq!(t.columns, vec!["id".to_string(), "amount".to_string()]);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].cells[1].key, CellValue::Number("10".into()));
        assert_eq!(t.rows[1].cells[1].key, CellValue::Number("20".into()));
    }

    #[test]
    fn external_fixture_csv_empty_cell_is_null() {
        // dbt's empty-equals-null convention holds for file bodies too.
        let t = external_fixture_table("id,name\n1,\n", Some("csv")).unwrap();
        assert_eq!(t.rows[0].cells[1].key, CellValue::Null);
    }

    #[test]
    fn external_fixture_csv_header_only_is_empty_grid() {
        // A header row with no data rows → a real (empty) grid (`Some`, NOT
        // the affordance fallback). Columns are derived from the DATA rows
        // (the shared `parse_csv_rows` behavior — identical to an inline
        // header-only csv), so a zero-row fixture yields an empty table. The
        // load-bearing point is `Some(empty)`, not `None`: the renderer shows
        // an (empty) grid, never the "data in external file" affordance.
        let t =
            external_fixture_table("id,amount\n", Some("csv")).expect("header-only csv is Some");
        assert!(t.rows.is_empty());
        assert!(
            t.columns.is_empty(),
            "columns are derived from data rows; a header-only csv has none"
        );
    }

    #[test]
    fn external_fixture_sql_literal_tabulates() {
        let t = external_fixture_table(
            "select 1 as id, 'alice' as name union all select 2 as id, 'bob' as name",
            Some("sql"),
        )
        .expect("literal-row sql fixture tabulates");
        assert_eq!(t.columns, vec!["id".to_string(), "name".to_string()]);
        assert_eq!(t.rows.len(), 2);
    }

    #[test]
    fn external_fixture_sql_non_literal_returns_none() {
        // A non-literal sql fixture file is opaque → None → the renderer
        // shows the sql code block (AC#5, matching #98 inline-sql).
        assert_eq!(
            external_fixture_table("select id, name from src", Some("sql")),
            None
        );
    }

    #[test]
    fn external_fixture_unknown_or_dict_format_returns_none() {
        // A `dict` (or unknown/absent) format on a FILE body is not a dbt
        // construct → None (the affordance fallback). The caller resolves
        // the effective format (manifest field or extension) to csv/sql.
        assert_eq!(external_fixture_table("id,amount\n1,10\n", None), None);
        assert_eq!(
            external_fixture_table("id,amount\n1,10\n", Some("dict")),
            None
        );
    }

    #[test]
    fn external_fixture_equals_inline_string_of_same_format() {
        // The no-divergence contract: an external fixture file tabulates
        // EXACTLY as the same-format inline `rows:` String (#138 dual axis),
        // so external + inline fixtures render identically.
        for (text, fmt) in [
            ("id,amount\n1,10\n2,20\n", "csv"),
            ("select 1 as id union all select 2 as id", "sql"),
        ] {
            assert_eq!(
                external_fixture_table(text, Some(fmt)),
                table_from_manifest_rows(&Value::String(text.to_owned()), Some(fmt)),
                "external != inline String for format {fmt}"
            );
        }
    }
}
