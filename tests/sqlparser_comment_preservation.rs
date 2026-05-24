//! Empirical probe: does sqlparser 0.62's `Display` impl preserve SQL
//! `--` line comments and `/* */` block comments through the
//! `parse → to_string` roundtrip that `cte_engine.rs` uses?
//!
//! Context: `CteNode::raw_sql` is sourced from `cte.query.to_string()`
//! (cte_engine.rs:146) and `query.body.to_string()` (cte_engine.rs:151).
//! If sqlparser drops comments at parse time, the compiled-SQL drawer
//! shown by the renderer is comment-stripped — a DevX loss for users
//! who author intentional commentary in their CTEs.
//!
//! Empirical finding (cute-dbt#31, 2026-05-23): sqlparser 0.62 drops
//! every comment shape tested (line/block, inside/outside CTE,
//! mid-select). The v0.1 fidelity limit was accepted; v0.2+ widening
//! is tracked in cute-dbt#45.
//!
//! The `sqlparser_062_drops_sql_comments` test below is the hard
//! regression gate — if it ever fails (sqlparser starts preserving),
//! revisit the cute-dbt#31 decision and the cute-dbt#45 follow-up.

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
    // Hard regression gate locking the v0.1 fidelity limit decision
    // (cute-dbt#31). If this assertion ever fires, sqlparser has
    // started preserving comments and the cute-dbt#45 widening can
    // either ship for free or change shape.
    let rendered = roundtrip(
        "WITH stg AS (\n  -- intentional comment\n  SELECT id FROM users\n)\nSELECT id FROM stg",
    );
    assert!(
        !rendered.contains("intentional comment"),
        "sqlparser appears to preserve comments now — revisit cute-dbt#31 / cute-dbt#45. Got: {rendered}"
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
