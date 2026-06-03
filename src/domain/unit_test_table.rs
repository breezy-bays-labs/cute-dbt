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
//! three entry points, deliberately:
//!
//! 1. [`type_cell_value`] — the NEW side, an already-typed JSON `Value`. A
//!    `Value::String` is a *deliberate* string (dbt-core ships csv cells as
//!    JSON strings `"1"`; a dict author's quoted `"1"` is a string too), so
//!    it stays [`CellValue::Str`] verbatim — never re-coerced to a number.
//! 2. [`type_cell_scalar`] — OLD-side **dict** tokens (block-dict +
//!    inline-flow YAML): quote-stripped tokens stay `Str`; otherwise
//!    `true`/`false` → [`Bool`](CellValue::Bool), `null`/`~` →
//!    [`Null`](CellValue::Null), a fully-numeric token →
//!    [`Number`](CellValue::Number), else `Str`. Symmetric with
//!    `type_cell_value`'s dict-number typing.
//! 3. [`type_csv_token`] — csv cells on **both** engine encodings (fusion's
//!    raw-string body AND dbt-core's pre-parsed string dicts): a csv field
//!    stays `Str` (empty → `Null`), with **no** numeric/bool coercion. This
//!    is what keeps csv `Str`-on-both-sides so the cross-engine table is
//!    byte-identical.
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

/// One cell — a thin newtype over its [typed value](CellValue).
///
/// Kept a struct (not a bare `CellValue`) so the File-2 diff can later hang
/// per-cell render hints off it without a wire-shape break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// The cell's semantically-typed value.
    pub value: CellValue,
}

impl Cell {
    /// Construct from a typed value.
    #[must_use]
    pub fn new(value: CellValue) -> Self {
        Self { value }
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
/// `sql` is opaque — a raw `SELECT` string has no cells, so the
/// [normalizers](table_from_manifest_rows) return `None` for it and the
/// view falls back to the cute-dbt#96 YAML text diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixtureFormat {
    /// `format: dict` — row maps (or csv pre-parsed to row maps by core).
    Dict,
    /// `format: csv` — a raw csv body (fusion) or pre-parsed string dicts
    /// (core); either way the cells stay `Str`.
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

/// Type a NEW-side cell from an already-typed JSON `Value` (the manifest
/// path).
///
/// - `Null` → [`CellValue::Null`]
/// - `Bool(b)` → [`CellValue::Bool`]
/// - `Number(n)` → [`CellValue::Number`] (canonicalized)
/// - `String(s)` → [`CellValue::Str`] **verbatim** — a manifest string is a
///   deliberate string (core ships csv cells as `"1"`; a dict author's
///   quoted `"1"` is a string), so it is NOT re-coerced to a number/bool.
///   This is the csv-`Str`-on-both-sides guarantee.
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
/// A csv field stays [`CellValue::Str`] — **no** numeric/bool coercion — so
/// csv is `Str`-on-both-engine-sides. An empty field maps to
/// [`CellValue::Null`] (the dbt empty-equals-null convention; the JS twin
/// half-implements it via the `hi < row.length ? … : ""` fill).
#[must_use]
pub fn type_csv_token(token: &str) -> CellValue {
    if token.is_empty() {
        CellValue::Null
    } else {
        CellValue::Str(token.to_owned())
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
/// Behavior, mirroring the JS twin's `tests/headless_csv_parser.rs` cases:
/// strip exactly one trailing `\n` (and a preceding `\r`); quoted fields;
/// `""` → a literal `"`; CRLF as one terminator; commas/newlines inside
/// quotes verbatim; the first row is the header; fewer than two rows
/// (empty or header-only) → `[]`; an unterminated final row is accepted;
/// a row shorter than the header fills the missing trailing columns with
/// `""` (which [`type_csv_token`] then maps to `Null`).
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
    for line in rows_region.split('\n') {
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
fn split_quote_aware(s: &str, sep: char) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
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
// Normalizers — the two terminus functions producing a FixtureTable
// ---------------------------------------------------------------------

/// Build the NEW-side [`FixtureTable`] from a manifest fixture's `rows`
/// `Value` + `format`. Returns `None` for an opaque/sql fixture (no cells).
///
/// Absorbs the dbt-core-vs-fusion csv divergence in ONE place:
/// - `format: sql` with a string `rows` → `None` (opaque → cute-dbt#96
///   fallback).
/// - `rows` is an `Array` (dict on both engines, csv-from-core, inline-flow
///   after serde): each element is an object; columns are the first-seen
///   union of keys; each field value routes through [`type_cell_value`]; a
///   key a row lacks → [`CellValue::Absent`].
/// - `rows` is a `String` AND `format: csv` (the fusion csv-as-raw-string
///   path): [`parse_csv_rows`] → header-keyed rows → each token through
///   [`type_csv_token`] (stays `Str`).
/// - `rows` is `Null` or an empty array → the empty [`FixtureTable`].
/// - any other shape → `None` (graceful; the IR is not a validator).
#[must_use]
pub fn table_from_manifest_rows(rows: &Value, format: Option<&str>) -> Option<FixtureTable> {
    let fmt = FixtureFormat::from_opt(format);
    match rows {
        Value::Null => Some(FixtureTable::default()),
        Value::Array(elems) => Some(table_from_value_objects(elems)),
        Value::String(s) => match fmt {
            FixtureFormat::Csv => Some(table_from_csv_text(s)),
            // A non-csv string `rows` (a raw sql SELECT, or a malformed
            // dict) is opaque — no cells.
            FixtureFormat::Sql | FixtureFormat::Dict => None,
        },
        // Object / Bool / Number `rows` — not a table.
        _ => None,
    }
}

/// Build the OLD-side [`FixtureTable`] from a reconstructed YAML `rows`
/// region + `format`. The diff-sourced sibling of [`table_from_manifest_rows`],
/// terminating in the same canonicalization so the two sides are
/// comparable.
///
/// - `format: sql` → `None` (opaque).
/// - `format: csv` → dedent then [`parse_csv_rows`] → [`type_csv_token`].
/// - `format: dict` (the default) → [`parse_block_dict_rows`] (which routes
///   inline-flow rows to [`parse_inline_flow_row`]) → [`type_cell_scalar`].
///
/// An empty region → the empty [`FixtureTable`].
#[must_use]
pub fn table_from_yaml_fragment(rows_region: &str, format: Option<&str>) -> Option<FixtureTable> {
    match FixtureFormat::from_opt(format) {
        FixtureFormat::Sql => None,
        FixtureFormat::Csv => {
            let dedented = dedent(rows_region);
            Some(table_from_csv_text(&dedented))
        }
        FixtureFormat::Dict => {
            let keyed = parse_block_dict_rows(rows_region);
            Some(table_from_keyed_rows(&keyed, type_cell_scalar))
        }
    }
}

/// Normalize an array of JSON objects (core dict / csv, fusion dict,
/// inline-flow-after-serde) into a [`FixtureTable`]. Columns are the
/// first-seen union of object keys; absent keys → [`CellValue::Absent`].
fn table_from_value_objects(elems: &[Value]) -> FixtureTable {
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
                .map(|col| {
                    let value = match elem {
                        Value::Object(map) => {
                            map.get(col).map_or(CellValue::Absent, type_cell_value)
                        }
                        _ => CellValue::Absent,
                    };
                    Cell::new(value)
                })
                .collect();
            TableRow::new(cells)
        })
        .collect();
    FixtureTable::new(columns, rows)
}

/// Normalize a raw csv body into a [`FixtureTable`] (cells stay `Str` via
/// [`type_csv_token`]). Shared by the fusion NEW side and the csv OLD side.
fn table_from_csv_text(text: &str) -> FixtureTable {
    let keyed = parse_csv_rows(text);
    table_from_keyed_rows(&keyed, type_csv_token)
}

/// Turn header-keyed string rows into a [`FixtureTable`], typing each token
/// through `type_fn`. Columns are the first-seen union of keys; a row that
/// lacks a column → [`CellValue::Absent`].
fn table_from_keyed_rows(
    keyed: &[Vec<(String, String)>],
    type_fn: fn(&str) -> CellValue,
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
                    let value = row
                        .iter()
                        .find(|(k, _)| k == col)
                        .map_or(CellValue::Absent, |(_, v)| type_fn(v));
                    Cell::new(value)
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
    fn a4_strings_stay_str_never_coerced_to_number() {
        // NEW Value::String("1") → Str("1"), NOT Number — the core-csv
        // string-stays-Str guarantee.
        assert_eq!(type_cell_value(&json!("1")), CellValue::Str("1".into()));
        // OLD quoted '1' / "1" → Str("1").
        assert_eq!(type_cell_scalar("'1'"), CellValue::Str("1".into()));
        assert_eq!(type_cell_scalar("\"1\""), CellValue::Str("1".into()));
        // csv token "1" stays Str.
        assert_eq!(type_csv_token("1"), CellValue::Str("1".into()));
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
    fn a8_absent_is_distinct_from_null() {
        // A sparse dict: row 1 has {a,b}, row 2 has only {a} → row 2's b is
        // Absent, NOT Null. And Absent != Null as CellValues.
        let elems = vec![json!({"a": 1, "b": 2}), json!({"a": 3})];
        let table = table_from_value_objects(&elems);
        assert_eq!(table.columns, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(table.rows[1].cells[1].value, CellValue::Absent);
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
        // identical data → EQUAL FixtureTable (cells Str on both sides).
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
        // And the cells are Str, not Number (the csv-Str guarantee).
        assert_eq!(
            fusion_tbl.rows[0].cells[0].value,
            CellValue::Str("1".into())
        );
    }

    // ----- G. Parsers -----

    /// Mirror tests/headless_csv_parser.rs's exact RFC 4180 cases.
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
        assert_eq!(table.rows[0].cells[0].value, CellValue::Number("-1".into()));
        assert_eq!(
            table.rows[0].cells[1].value,
            CellValue::Str("UNKNOWN".into())
        );
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
        assert_eq!(table.rows[0].cells[0].value, CellValue::Str("a, b".into()));
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
    fn i34_sql_format_manifest_rows_returns_none() {
        let sql = json!("SELECT 1 AS id, 'alice' AS name");
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
        assert_eq!(table.rows[0].cells[1].value, CellValue::Str("alice".into()));
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
}
