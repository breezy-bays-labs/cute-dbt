//! CTE engine — a `sqlparser-rs` 0.62 parser-AST pass that extracts a
//! [`CteGraph`] (CTE dependency graph + join-type-classified edges) from a
//! dbt model's compiled SQL.
//!
//! ## What it produces
//!
//! Given the `compiled_code` of a model, [`parse_cte_graph`] returns one
//! [`CteNode`] per `WITH` common-table-expression (in declaration order)
//! plus a single terminal node for the final `SELECT`. A model with no
//! `WITH` clause has no CTE structure to visualise and yields an empty
//! [`CteGraph`].
//!
//! ## The v0.1 edge model — a *join* graph
//!
//! [`JoinType`] is a closed vocabulary of five SQL join kinds, so every
//! emitted edge *is* a join. The engine walks each query body's `FROM`
//! clause and, for each join chain `base [JOIN r1] [JOIN r2] …`, emits a
//! `referenced_cte → consumer` edge coloured by the join that introduces
//! the reference: each joined relation is introduced by its own join
//! operator; the base relation is introduced by the **first** join in the
//! chain. A plain single-table `FROM cte` (no join) carries no join type
//! and therefore emits no edge — so a CTE referenced only by pass-through,
//! and a terminal `SELECT * FROM last_cte`, appear as nodes with no
//! incoming edge. v0.1 visualises join structure; capturing plain
//! pass-through references is a future widening of the vocabulary.
//!
//! Joins outside the five-kind vocabulary (`SEMI` / `ANTI` / `ASOF` /
//! `APPLY` / `ARRAY JOIN`, …) are not classified and emit no edge.
//!
//! ## Acyclicity
//!
//! Edges are emitted only when the referenced CTE is declared *earlier*
//! than the consumer (`from < to`). Standard non-recursive SQL only ever
//! references earlier CTEs, so this drops nothing legitimate while making
//! the graph acyclic by construction: a self-reference (`WITH RECURSIVE`)
//! or a forward reference is silently not an edge.
//!
//! No tokenizer or comment pass — `-- @desc` per-CTE descriptions are a
//! v0.2 concern and are not designed ahead here.

use std::collections::HashMap;

use sqlparser::ast::{Cte, JoinOperator, Query, SetExpr, Statement, TableFactor, TableWithJoins};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::domain::{CteEdge, CteGraph, CteNode, JoinType};

/// Display name of the synthetic terminal node — the final `SELECT` that
/// follows a model's `WITH` clause. The compiled SQL does not carry the
/// model's own name, so the engine names the terminal consistently; the
/// renderer keys off this exact string.
pub const TERMINAL_NODE_NAME: &str = "(final select)";

/// Failure modes of [`parse_cte_graph`].
///
/// `#[non_exhaustive]` per the enums-yes-structs-no rule — consumers
/// pattern-match this, and new failure modes are additive.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CteError {
    /// `compiled_code` is not valid SQL under the generic dialect.
    #[error("compiled SQL failed to parse: {0}")]
    Parse(String),
    /// `compiled_code` parsed, but its first statement is not a `SELECT`
    /// query (a compiled dbt model is always a query).
    #[error("compiled SQL did not contain a top-level SELECT query")]
    NotAQuery,
}

/// Extract a [`CteGraph`] from a dbt model's compiled SQL.
///
/// A model with no `WITH` clause yields an empty graph (no CTE structure
/// to visualise). See the module docs for the join-graph edge model.
///
/// # Errors
///
/// Returns [`CteError::Parse`] when `compiled_sql` is not valid SQL under
/// the generic dialect, and [`CteError::NotAQuery`] when it parses but the
/// first statement is not a `SELECT` query.
pub fn parse_cte_graph(compiled_sql: &str) -> Result<CteGraph, CteError> {
    let query = parse_query(compiled_sql)?;
    Ok(build_graph(&query))
}

/// Parse `compiled_sql` and return its first statement as a [`Query`].
///
/// dbt emits exactly one statement per model; if a manifest ever carries
/// several, the first is authoritative.
fn parse_query(compiled_sql: &str) -> Result<Query, CteError> {
    let statements = Parser::parse_sql(&GenericDialect, compiled_sql)
        .map_err(|err| CteError::Parse(err.to_string()))?;
    match statements.into_iter().next() {
        Some(Statement::Query(query)) => Ok(*query),
        _ => Err(CteError::NotAQuery),
    }
}

/// Assemble the [`CteGraph`] from a parsed [`Query`].
fn build_graph(query: &Query) -> CteGraph {
    let ctes = cte_tables(query);
    if ctes.is_empty() {
        return CteGraph::default();
    }
    let nodes = build_nodes(ctes, query);
    let index = name_index(ctes);
    let edges = build_edges(ctes, query, &index);
    CteGraph::new(nodes, edges)
}

/// The `WITH` clause's CTEs, or an empty slice when there is no `WITH`.
fn cte_tables(query: &Query) -> &[Cte] {
    query
        .with
        .as_ref()
        .map_or([].as_slice(), |with| with.cte_tables.as_slice())
}

/// One [`CteNode`] per CTE in declaration order, then the terminal node.
fn build_nodes(ctes: &[Cte], query: &Query) -> Vec<CteNode> {
    let mut nodes: Vec<CteNode> = ctes
        .iter()
        .map(|cte| CteNode::new(cte_name(cte), None, Some(cte.query.to_string()), None))
        .collect();
    nodes.push(CteNode::new(
        TERMINAL_NODE_NAME,
        None,
        Some(query.body.to_string()),
        None,
    ));
    nodes
}

/// Map each CTE name (lowercased — SQL identifiers are case-insensitive)
/// to its declaration index.
fn name_index(ctes: &[Cte]) -> HashMap<String, usize> {
    ctes.iter()
        .enumerate()
        .map(|(idx, cte)| (cte_name(cte).to_ascii_lowercase(), idx))
        .collect()
}

/// Every join edge from every consumer body — each CTE body plus the
/// terminal `SELECT`.
fn build_edges(ctes: &[Cte], query: &Query, index: &HashMap<String, usize>) -> Vec<CteEdge> {
    let mut edges: Vec<CteEdge> = Vec::new();
    for (consumer_idx, cte) in ctes.iter().enumerate() {
        collect_edges(consumer_idx, &cte.query.body, index, &mut edges);
    }
    collect_edges(ctes.len(), &query.body, index, &mut edges);
    edges
}

/// Walk a query body's `FROM` clauses, descending through parenthesised
/// subqueries and set operations, appending each join edge it finds.
fn collect_edges(
    consumer_idx: usize,
    body: &SetExpr,
    index: &HashMap<String, usize>,
    edges: &mut Vec<CteEdge>,
) {
    match body {
        SetExpr::Select(select) => {
            for table in &select.from {
                edges_from_join_chain(consumer_idx, table, index, edges);
            }
        }
        SetExpr::Query(inner) => collect_edges(consumer_idx, &inner.body, index, edges),
        SetExpr::SetOperation { left, right, .. } => {
            collect_edges(consumer_idx, left, index, edges);
            collect_edges(consumer_idx, right, index, edges);
        }
        _ => {}
    }
}

/// Emit an edge for every CTE reference in one `FROM` join chain.
///
/// Each joined relation is introduced by its own join operator; the base
/// relation is introduced by the first join in the chain (and emits no
/// edge when the chain has no joins).
fn edges_from_join_chain(
    consumer_idx: usize,
    table: &TableWithJoins,
    index: &HashMap<String, usize>,
    edges: &mut Vec<CteEdge>,
) {
    if let Some(first) = table.joins.first() {
        if let Some(join_type) = classify_join(&first.join_operator) {
            push_edge(&table.relation, consumer_idx, join_type, index, edges);
        }
    }
    for join in &table.joins {
        if let Some(join_type) = classify_join(&join.join_operator) {
            push_edge(&join.relation, consumer_idx, join_type, index, edges);
        }
    }
}

/// Append a `referenced_cte → consumer` edge when `factor` resolves to an
/// earlier-declared CTE. Drops external tables, self-references, forward
/// references, and duplicates.
fn push_edge(
    factor: &TableFactor,
    consumer_idx: usize,
    join_type: JoinType,
    index: &HashMap<String, usize>,
    edges: &mut Vec<CteEdge>,
) {
    let Some(source_idx) = resolve_factor(factor, index) else {
        return;
    };
    if source_idx >= consumer_idx {
        return;
    }
    let edge = CteEdge::new(source_idx, consumer_idx, join_type);
    if !edges.contains(&edge) {
        edges.push(edge);
    }
}

/// Resolve a table reference to a CTE declaration index, or `None` when it
/// is not a plain named table or not a known CTE.
fn resolve_factor(factor: &TableFactor, index: &HashMap<String, usize>) -> Option<usize> {
    let TableFactor::Table { name, .. } = factor else {
        return None;
    };
    let leaf = name.0.last()?.as_ident()?;
    index.get(&leaf.value.to_ascii_lowercase()).copied()
}

/// Classify a `sqlparser` join operator into the v0.1 [`JoinType`]
/// vocabulary, or `None` for join kinds outside it.
///
/// The catch-all arm is load-bearing: it keeps the engine forward
/// compatible with future `sqlparser` releases that add `JoinOperator`
/// variants, and it is where every non-vocabulary join (`SEMI` / `ANTI` /
/// `ASOF` / `APPLY` / `ARRAY JOIN`) lands.
fn classify_join(operator: &JoinOperator) -> Option<JoinType> {
    use JoinOperator as Op;
    match operator {
        Op::Join(_) | Op::Inner(_) => Some(JoinType::Inner),
        Op::Left(_) | Op::LeftOuter(_) => Some(JoinType::Left),
        Op::Right(_) | Op::RightOuter(_) => Some(JoinType::Right),
        Op::FullOuter(_) => Some(JoinType::Full),
        Op::CrossJoin(_) => Some(JoinType::Cross),
        _ => None,
    }
}

/// A CTE's declared name (the alias before `AS`).
fn cte_name(cte: &Cte) -> &str {
    &cte.alias.name.value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `sql` into a graph, panicking with context on failure.
    fn graph(sql: &str) -> CteGraph {
        parse_cte_graph(sql).unwrap_or_else(|err| panic!("`{sql}` should parse: {err:?}"))
    }

    /// `true` when `g` carries an edge with exactly these endpoints/kind.
    fn has_edge(g: &CteGraph, from: usize, to: usize, join_type: JoinType) -> bool {
        g.edges()
            .iter()
            .any(|e| e.from() == from && e.to() == to && e.join_type() == join_type)
    }

    #[test]
    fn a_query_with_no_with_clause_yields_an_empty_graph() {
        let g = graph("SELECT 1");
        assert!(g.is_empty(), "no CTEs means no graph to render");
        assert!(g.nodes().is_empty());
        assert!(g.edges().is_empty());
    }

    #[test]
    fn one_cte_produces_that_cte_plus_a_terminal_node() {
        let g = graph("WITH a AS (SELECT 1 AS id) SELECT * FROM a");
        assert_eq!(g.nodes().len(), 2, "one CTE node plus the terminal node");
        assert_eq!(g.nodes()[0].name(), "a");
        assert_eq!(g.nodes()[1].name(), TERMINAL_NODE_NAME);
    }

    #[test]
    fn a_plain_pass_through_from_emits_no_edge() {
        // `SELECT * FROM a` references the CTE `a` but does not join it —
        // the v0.1 join-graph model carries no edge for a pass-through.
        let g = graph("WITH a AS (SELECT 1 AS id) SELECT * FROM a");
        assert!(g.edges().is_empty(), "a non-join reference is not an edge");
    }

    #[test]
    fn ctes_keep_declaration_order_and_the_terminal_is_last() {
        let g = graph(
            "WITH first AS (SELECT 1 AS id), \
                  second AS (SELECT 2 AS id), \
                  third AS (SELECT 3 AS id) \
             SELECT * FROM third",
        );
        let names: Vec<&str> = g.nodes().iter().map(CteNode::name).collect();
        assert_eq!(names, ["first", "second", "third", TERMINAL_NODE_NAME]);
    }

    #[test]
    fn each_join_keyword_classifies_its_edge() {
        // Maps `cte_rendering.feature`'s Scenario Outline: every join
        // variant produces a correctly classified edge. `a` is the base
        // and `b` the joined relation, so both edges carry the join type.
        let cases = [
            ("a JOIN b ON a.id = b.id", JoinType::Inner),
            ("a INNER JOIN b ON a.id = b.id", JoinType::Inner),
            ("a LEFT JOIN b ON a.id = b.id", JoinType::Left),
            ("a RIGHT JOIN b ON a.id = b.id", JoinType::Right),
            ("a FULL JOIN b ON a.id = b.id", JoinType::Full),
            ("a CROSS JOIN b", JoinType::Cross),
        ];
        for (from_clause, expected) in cases {
            let sql = format!(
                "WITH a AS (SELECT 1 AS id), b AS (SELECT 1 AS id) \
                 SELECT * FROM {from_clause}"
            );
            let g = graph(&sql);
            assert!(
                has_edge(&g, 0, 2, expected),
                "`{from_clause}`: base edge a->terminal should be {expected:?}",
            );
            assert!(
                has_edge(&g, 1, 2, expected),
                "`{from_clause}`: joined edge b->terminal should be {expected:?}",
            );
        }
    }

    #[test]
    fn outer_keyword_variants_classify_to_the_same_join_type() {
        // `LEFT OUTER` / `RIGHT OUTER` are distinct `JoinOperator`
        // variants from `LEFT` / `RIGHT`; both must classify.
        let left = graph(
            "WITH a AS (SELECT 1 AS id), b AS (SELECT 1 AS id) \
             SELECT * FROM a LEFT OUTER JOIN b ON a.id = b.id",
        );
        assert!(has_edge(&left, 1, 2, JoinType::Left));
        let right = graph(
            "WITH a AS (SELECT 1 AS id), b AS (SELECT 1 AS id) \
             SELECT * FROM a RIGHT OUTER JOIN b ON a.id = b.id",
        );
        assert!(has_edge(&right, 1, 2, JoinType::Right));
    }

    #[test]
    fn the_base_relation_takes_the_first_joins_type() {
        // `c` joins `a LEFT JOIN b`: both the base `a` and the joined `b`
        // become LEFT-coloured edges into `c`.
        let g = graph(
            "WITH a AS (SELECT 1 AS id), \
                  b AS (SELECT 1 AS id), \
                  c AS (SELECT * FROM a LEFT JOIN b ON a.id = b.id) \
             SELECT * FROM c",
        );
        assert!(has_edge(&g, 0, 2, JoinType::Left), "base a->c is LEFT");
        assert!(has_edge(&g, 1, 2, JoinType::Left), "joined b->c is LEFT");
    }

    #[test]
    fn external_tables_are_not_nodes_and_not_edges() {
        let g = graph(
            "WITH a AS (SELECT * FROM raw_source JOIN other_raw ON 1 = 1) \
             SELECT * FROM a",
        );
        assert_eq!(g.nodes().len(), 2, "only the CTE and terminal are nodes");
        assert!(g.edges().is_empty(), "raw_source/other_raw are not CTEs");
    }

    #[test]
    fn a_self_reference_is_not_an_edge() {
        // `selfish` joins itself and `a`: the self-reference is dropped
        // (acyclic by construction); the earlier CTE `a` still resolves.
        let g = graph(
            "WITH a AS (SELECT 1 AS id), \
                  selfish AS (SELECT * FROM selfish JOIN a ON 1 = 1) \
             SELECT * FROM selfish",
        );
        assert!(has_edge(&g, 0, 1, JoinType::Inner), "a->selfish resolves");
        assert!(
            !g.edges().iter().any(|e| e.from() == e.to()),
            "no edge points a node at itself",
        );
    }

    #[test]
    fn cte_names_resolve_case_insensitively() {
        let g = graph(
            "WITH Orders AS (SELECT 1 AS id), \
                  Customers AS (SELECT 2 AS id), \
                  joined AS (SELECT * FROM orders JOIN customers ON 1 = 1) \
             SELECT * FROM joined",
        );
        assert!(has_edge(&g, 0, 2, JoinType::Inner), "orders -> joined");
        assert!(has_edge(&g, 1, 2, JoinType::Inner), "customers -> joined");
    }

    #[test]
    fn a_repeated_reference_is_deduplicated() {
        // `a JOIN a` resolves the base and the joined relation to the
        // same CTE under the same join type — one edge, not two.
        let g = graph("WITH a AS (SELECT 1 AS id) SELECT * FROM a JOIN a t2 ON 1 = 1");
        assert_eq!(g.edges().len(), 1, "identical edges collapse to one");
        assert!(has_edge(&g, 0, 1, JoinType::Inner));
    }

    #[test]
    fn set_operations_in_a_body_are_walked() {
        // A CTE whose body is `… UNION ALL …` has both arms scanned.
        let g = graph(
            "WITH a AS (SELECT 1 AS id), \
                  b AS (SELECT 1 AS id), \
                  u AS (SELECT * FROM a JOIN b ON 1 = 1 \
                        UNION ALL SELECT * FROM a) \
             SELECT * FROM u",
        );
        assert!(has_edge(&g, 0, 2, JoinType::Inner), "left arm: a->u");
        assert!(has_edge(&g, 1, 2, JoinType::Inner), "left arm: b->u");
    }

    #[test]
    fn a_parenthesized_subquery_body_is_walked() {
        // A CTE body wrapped in parentheses parses as `SetExpr::Query`;
        // the engine descends through it to reach the join chain.
        let g = graph(
            "WITH a AS (SELECT 1 AS id), b AS (SELECT 1 AS id), \
                  wrapped AS ((SELECT * FROM a JOIN b ON 1 = 1)) \
             SELECT * FROM wrapped",
        );
        assert!(has_edge(&g, 0, 2, JoinType::Inner), "a -> wrapped");
        assert!(has_edge(&g, 1, 2, JoinType::Inner), "b -> wrapped");
    }

    #[test]
    fn classify_join_maps_the_full_vocabulary() {
        use sqlparser::ast::JoinConstraint::None as NoConstraint;
        assert_eq!(
            classify_join(&JoinOperator::Inner(NoConstraint)),
            Some(JoinType::Inner),
        );
        assert_eq!(
            classify_join(&JoinOperator::Left(NoConstraint)),
            Some(JoinType::Left),
        );
        assert_eq!(
            classify_join(&JoinOperator::Right(NoConstraint)),
            Some(JoinType::Right),
        );
        assert_eq!(
            classify_join(&JoinOperator::FullOuter(NoConstraint)),
            Some(JoinType::Full),
        );
        assert_eq!(
            classify_join(&JoinOperator::CrossJoin(NoConstraint)),
            Some(JoinType::Cross),
        );
    }

    #[test]
    fn classify_join_rejects_joins_outside_the_vocabulary() {
        // Exotic joins (here MSSQL `CROSS APPLY`) carry no v0.1 colour
        // and so produce no edge.
        assert_eq!(classify_join(&JoinOperator::CrossApply), None);
        assert_eq!(classify_join(&JoinOperator::OuterApply), None);
    }

    #[test]
    fn invalid_sql_is_a_parse_error() {
        let err = parse_cte_graph("this is not sql ((").unwrap_err();
        assert!(matches!(err, CteError::Parse(_)), "got {err:?}");
    }

    #[test]
    fn a_non_query_statement_is_rejected() {
        let err = parse_cte_graph("CREATE TABLE t (id INT)").unwrap_err();
        assert_eq!(err, CteError::NotAQuery);
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(parse_cte_graph("").is_err(), "no statement is not a query");
    }

    #[test]
    fn the_first_statement_wins() {
        // `SELECT 1` (no CTEs) is taken; the trailing `WITH` is ignored.
        let g = graph("SELECT 1; WITH a AS (SELECT 1 AS id) SELECT * FROM a");
        assert!(g.is_empty(), "only the first statement is analysed");
    }

    #[test]
    fn nodes_carry_their_raw_sql() {
        let g = graph("WITH a AS (SELECT 1 AS id) SELECT * FROM a");
        assert!(
            g.nodes()[0].raw_sql().is_some_and(|s| s.contains("SELECT")),
            "a CTE node carries its body SQL",
        );
        assert!(
            g.nodes()[1].raw_sql().is_some(),
            "the terminal node carries the final SELECT",
        );
    }

    #[test]
    fn structural_invariants_hold_across_varied_sql() {
        // Enumerated property test (no proptest dep, per the per-PR
        // dependency discipline): for every sample, node count matches
        // the CTE count and every edge is acyclic and in range.
        let samples: [(&str, usize); 5] = [
            ("SELECT 1", 0),
            ("WITH a AS (SELECT 1 AS id) SELECT * FROM a", 1),
            (
                "WITH a AS (SELECT 1 AS id), b AS (SELECT 1 AS id) \
                 SELECT * FROM a JOIN b ON a.id = b.id",
                2,
            ),
            (
                "WITH a AS (SELECT 1 AS id), b AS (SELECT 1 AS id), \
                      c AS (SELECT * FROM a LEFT JOIN b ON a.id = b.id) \
                 SELECT * FROM c",
                3,
            ),
            (
                "WITH a AS (SELECT 1 AS id), b AS (SELECT 1 AS id), \
                      c AS (SELECT 1 AS id), \
                      d AS (SELECT * FROM a \
                            JOIN b ON 1 = 1 \
                            RIGHT JOIN c ON 1 = 1) \
                 SELECT * FROM d",
                4,
            ),
        ];
        for (sql, cte_count) in samples {
            let g = graph(sql);
            let expected_nodes = if cte_count == 0 { 0 } else { cte_count + 1 };
            assert_eq!(
                g.nodes().len(),
                expected_nodes,
                "`{sql}`: node count is CTE count plus the terminal",
            );
            for edge in g.edges() {
                assert!(edge.from() < edge.to(), "`{sql}`: edge is acyclic");
                assert!(
                    edge.to() < g.nodes().len(),
                    "`{sql}`: edge endpoint is in range",
                );
            }
        }
    }
}
