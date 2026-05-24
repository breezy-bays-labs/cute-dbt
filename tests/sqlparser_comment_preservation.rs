//! Regression gates locking the sqlparser 0.62 behavior that motivated
//! cute-dbt's span-based slicing of `compiled_code` (cute-dbt#31).
//!
//! Empirical finding: sqlparser 0.62's `Display` impl drops every SQL
//! comment shape — `--` line comments outside or inside a CTE, `/* */`
//! block comments, trailing line comments, mid-select block comments —
//! through the `parse → to_string()` roundtrip. The CTE engine therefore
//! slices each CTE's `raw_sql` from the original `compiled_code`
//! directly, preserving SQL comments faithfully. See
//! `cte_engine::build_nodes` and `cte_engine::slice_or_fallback`.
//!
//! If either test below ever fails (sqlparser starts preserving comments
//! via `Display`), the slicing layer's failure mode shifts — re-read
//! cute-dbt#31 to confirm slicing is still the right call.

use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

/// The canonical roundtrip cute-dbt performs on every compiled model.
fn roundtrip(sql: &str) -> String {
    let statements = Parser::parse_sql(&GenericDialect, sql)
        .expect("test fixtures must be valid SQL under GenericDialect");
    statements
        .into_iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Hard regression gate over a representative SQL — one tight assert
/// that's the first place to look when sqlparser bumps.
#[test]
fn sqlparser_062_drops_sql_comments() {
    let rendered = roundtrip(
        "WITH stg AS (\n  -- intentional comment\n  SELECT id FROM users\n)\nSELECT id FROM stg",
    );
    assert!(
        !rendered.contains("intentional comment"),
        "sqlparser appears to preserve comments now — revisit cute-dbt#31. Got: {rendered}"
    );
}

/// Shape-coverage gate: every comment shape we care about (line/block,
/// inside/outside CTE, mid-expression) is dropped. Each case carries
/// an explicit sentinel so the assertion is robust — no fragile keyword
/// extraction. If any case starts surviving, the message names the
/// shape so the diagnosis is one-step.
#[test]
fn sqlparser_062_drops_every_tested_comment_shape() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "line comment outside CTE",
            "-- TOP_SENTINEL\nSELECT 1",
            "TOP_SENTINEL",
        ),
        (
            "line comment inside CTE body",
            "WITH stg AS (\n  -- INSIDE_LINE_SENTINEL\n  SELECT id FROM users\n)\nSELECT id FROM stg",
            "INSIDE_LINE_SENTINEL",
        ),
        (
            "block comment inside CTE body",
            "WITH stg AS (\n  /* INSIDE_BLOCK_SENTINEL */\n  SELECT id FROM users\n)\nSELECT id FROM stg",
            "INSIDE_BLOCK_SENTINEL",
        ),
        (
            "trailing line comment on SELECT",
            "WITH stg AS (\n  SELECT id FROM users -- TRAILING_SENTINEL\n)\nSELECT id FROM stg",
            "TRAILING_SENTINEL",
        ),
        (
            "block comment mid-expression",
            "SELECT id /* MID_EXPR_SENTINEL */ FROM users",
            "MID_EXPR_SENTINEL",
        ),
    ];
    for (label, sql, sentinel) in cases {
        let rendered = roundtrip(sql);
        assert!(
            !rendered.contains(sentinel),
            "[{label}] sentinel `{sentinel}` survived sqlparser's parse → Display roundtrip — \
             revisit cute-dbt#31. Got:\n{rendered}"
        );
    }
}
