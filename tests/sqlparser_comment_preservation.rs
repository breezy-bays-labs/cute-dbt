//! Regression gate: documents the sqlparser 0.62 behavior that
//! motivated cute-dbt's span-based slicing of `compiled_code`
//! (cute-dbt#31).
//!
//! Empirical finding: sqlparser 0.62's `Display` impl drops every
//! SQL comment shape — `--` line comments outside or inside a CTE,
//! `/* */` block comments, trailing line comments — through the
//! `parse → to_string()` roundtrip. The CTE engine therefore slices
//! each CTE's `raw_sql` from the original `compiled_code` directly,
//! preserving SQL comments faithfully. See `cte_engine::build_nodes`
//! and `cte_engine::slice_or_fallback`.
//!
//! `sqlparser_062_drops_sql_comments` is the **hard regression gate**.
//! If it ever fails (sqlparser starts preserving comments via
//! `Display`), the slicing layer's failure mode shifts — re-read
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

#[test]
fn sqlparser_062_drops_sql_comments() {
    // Hard regression gate. If sqlparser ever starts preserving SQL
    // comments through Display, the cte_engine's span-slicing layer
    // becomes belt-and-suspenders rather than load-bearing — revisit
    // cute-dbt#31 to decide whether to drop the slice in favor of the
    // simpler AST roundtrip.
    let rendered = roundtrip(
        "WITH stg AS (\n  -- intentional comment\n  SELECT id FROM users\n)\nSELECT id FROM stg",
    );
    assert!(
        !rendered.contains("intentional comment"),
        "sqlparser appears to preserve comments now — revisit cute-dbt#31. Got: {rendered}"
    );
}

#[test]
fn line_comment_outside_cte_body() {
    let sql = "\
-- top-of-file comment
SELECT 1";
    let rendered = roundtrip(sql);
    let preserved = rendered.contains("top-of-file comment");
    println!("[line_comment_outside] preserved={preserved}\n--- rendered ---\n{rendered}\n---");
    // Assertion is documentary — we want the test to record what
    // sqlparser actually does, not to enforce a behavior we don't
    // control.  The decision (keep / slice / accept-limit) is wired
    // off the printed output captured by `cargo test -- --nocapture`.
}

#[test]
fn line_comment_inside_cte_body() {
    let sql = "\
WITH stg AS (
    -- pulled from stg.users; do not filter rows here
    SELECT id, email FROM users
)
SELECT id FROM stg";
    let rendered = roundtrip(sql);
    let preserved = rendered.contains("do not filter rows here");
    println!("[line_comment_inside_cte] preserved={preserved}\n--- rendered ---\n{rendered}\n---");
}

#[test]
fn block_comment_inside_cte_body() {
    let sql = "\
WITH stg AS (
    /* intentional commentary about why this CTE exists */
    SELECT id FROM users
)
SELECT id FROM stg";
    let rendered = roundtrip(sql);
    let preserved = rendered.contains("intentional commentary about why this CTE exists");
    println!("[block_comment_inside_cte] preserved={preserved}\n--- rendered ---\n{rendered}\n---");
}

#[test]
fn trailing_line_comment_on_select() {
    let sql = "\
WITH stg AS (
    SELECT id FROM users -- only id; email omitted on purpose
)
SELECT id FROM stg";
    let rendered = roundtrip(sql);
    let preserved = rendered.contains("only id; email omitted on purpose");
    println!("[trailing_line_comment] preserved={preserved}\n--- rendered ---\n{rendered}\n---");
}

#[test]
fn block_comment_between_select_and_from() {
    let sql = "\
SELECT id /* primary key */ FROM users";
    let rendered = roundtrip(sql);
    let preserved = rendered.contains("primary key");
    println!("[block_comment_between] preserved={preserved}\n--- rendered ---\n{rendered}\n---");
}

#[test]
fn comment_survives_decision_record() {
    // This is the consolidated documentary test: if ANY of the four
    // comment shapes above survive the roundtrip, the engine could be
    // kept as-is for those shapes; if NONE survive, the v0.1 fidelity
    // limit is real and the decision is between raw-text slicing and
    // accepting the limit.
    let shapes = [
        ("-- top-of-file", "-- top-of-file comment\nSELECT 1"),
        (
            "-- inside CTE body",
            "WITH stg AS (\n  -- inside\n  SELECT id FROM users\n)\nSELECT id FROM stg",
        ),
        (
            "/* inside CTE body */",
            "WITH stg AS (\n  /* inside */\n  SELECT id FROM users\n)\nSELECT id FROM stg",
        ),
        (
            "-- trailing on SELECT",
            "WITH stg AS (\n  SELECT id FROM users -- trailing\n)\nSELECT id FROM stg",
        ),
        ("/* mid-select */", "SELECT id /* mid-select */ FROM users"),
    ];
    let mut summary = String::new();
    let mut any_preserved = false;
    for (label, sql) in shapes {
        let rendered = roundtrip(sql);
        let preserved = rendered.contains("inside")
            || rendered.contains("trailing")
            || rendered.contains("top-of-file")
            || rendered.contains("mid-select");
        if preserved {
            any_preserved = true;
        }
        summary.push_str(&format!("{label}: preserved={preserved}\n"));
    }
    println!(
        "\n=== sqlparser 0.62 comment preservation summary ===\n{summary}any_preserved={any_preserved}\n"
    );
}
