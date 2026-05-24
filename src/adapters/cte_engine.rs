//! CTE engine — a `sqlparser-rs` 0.62 parser-AST pass that extracts a
//! [`CteGraph`] (CTE dependency graph + edge-type-classified edges) from a
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
//! ## The v0.1 edge model — a *structural* graph
//!
//! [`EdgeType`] covers all structural relationships between CTEs:
//!
//! - **`From`** — a plain `FROM <cte>` reference with no join operator.
//!   Every base relation and every join-free CTE reference emits a `From`
//!   edge into its consumer.
//! - **`Inner` / `Left` / `Right` / `Full` / `Cross`** — the five SQL
//!   join kinds. The joined relation (right-hand side of the join) takes
//!   the specific join type; the base relation of a join chain always
//!   takes `From`.
//! - **`UnionAll`** — a `UNION ALL` arm reference: the CTE appearing as
//!   the direct `FROM` source of a join-free UNION arm.
//! - **`UnionDistinct`** — a `UNION` / `UNION DISTINCT` arm reference
//!   (plain `UNION` is semantically distinct). `SetQuantifier::None`
//!   maps here.
//!
//! The base relation of a JOIN chain inside a UNION arm gets `From`, not
//! the union type — only join-free arm sources get the union type.
//!
//! Join kinds outside the five-kind vocabulary (`SEMI` / `ANTI` / `ASOF`
//! / `APPLY` / `ARRAY JOIN`, …) are not classified and emit no edge.
//! Non-`UNION` set operations (`EXCEPT` / `INTERSECT` / `MINUS`) are
//! recursed into — JOIN edges inside them still emit — but no union-type
//! edge is emitted for the arms themselves.
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

use sqlparser::ast::{
    Cte, JoinOperator, Query, SetExpr, SetOperator, SetQuantifier, Statement, TableFactor,
    TableWithJoins,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::domain::{CteEdge, CteGraph, CteNode, EdgeType};

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
/// to visualise). See the module docs for the structural edge model.
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
///
/// `WITH RECURSIVE` queries are flagged via [`CteGraph::with_recursive`]
/// so the renderer can surface a banner. The acyclicity invariant (`from
/// < to`) already drops self-referencing edges, keeping the edge list
/// DAG-safe regardless.
fn build_graph(query: &Query) -> CteGraph {
    let ctes = cte_tables(query);
    if ctes.is_empty() {
        return CteGraph::default();
    }
    let recursive = query.with.as_ref().is_some_and(|with| with.recursive);
    let nodes = build_nodes(ctes, query);
    let index = name_index(ctes);
    let edges = build_edges(ctes, query, &index);
    let graph = CteGraph::new(nodes, edges);
    if recursive {
        graph.with_recursive()
    } else {
        graph
    }
}

/// The `WITH` clause's CTEs, or an empty slice when there is no `WITH`.
fn cte_tables(query: &Query) -> &[Cte] {
    query
        .with
        .as_ref()
        .map_or([].as_slice(), |with| with.cte_tables.as_slice())
}

/// One [`CteNode`] per CTE in declaration order, then the terminal node.
///
/// tracked: cute-dbt#45 — `Display::to_string()` on the parsed AST is
/// the v0.1 source of `raw_sql`, which sqlparser 0.62 emits without
/// the original SQL comments (cute-dbt#31 confirmed). The v0.2
/// widening will swap this for span-based slicing of `compiled_code`.
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

/// Every edge from every consumer body — each CTE body plus the terminal
/// `SELECT`.
fn build_edges(ctes: &[Cte], query: &Query, index: &HashMap<String, usize>) -> Vec<CteEdge> {
    let mut edges: Vec<CteEdge> = Vec::new();
    for (consumer_idx, cte) in ctes.iter().enumerate() {
        collect_edges(consumer_idx, &cte.query.body, index, None, &mut edges);
    }
    collect_edges(ctes.len(), &query.body, index, None, &mut edges);
    edges
}

/// Walk a query body's `FROM` clauses, descending through parenthesised
/// subqueries and set operations, appending each edge it finds.
///
/// `union_type` is `Some(EdgeType)` when this body is a direct arm of a
/// `UNION ALL` / `UNION DISTINCT` operation; plain join-free `FROM`
/// references in that arm get the union type instead of `From`.
fn collect_edges(
    consumer_idx: usize,
    body: &SetExpr,
    index: &HashMap<String, usize>,
    union_type: Option<EdgeType>,
    edges: &mut Vec<CteEdge>,
) {
    match body {
        SetExpr::Select(select) => {
            for table in &select.from {
                edges_from_join_chain(consumer_idx, table, index, union_type, edges);
            }
        }
        SetExpr::Query(inner) => {
            collect_edges(consumer_idx, &inner.body, index, union_type, edges);
        }
        SetExpr::SetOperation {
            op,
            set_quantifier,
            left,
            right,
        } => {
            // Only UNION arms get a union-type override; EXCEPT/INTERSECT/
            // MINUS recurse without one (joins inside still emit normally).
            let arm_type = if *op == SetOperator::Union {
                Some(classify_union_quantifier(*set_quantifier))
            } else {
                None
            };
            collect_edges(consumer_idx, left, index, arm_type, edges);
            collect_edges(consumer_idx, right, index, arm_type, edges);
        }
        _ => {}
    }
}

/// Emit an edge for every CTE reference in one `FROM` join chain.
///
/// - If the chain has **no joins** (`table.joins` is empty), the base
///   relation gets the `union_type` override when present, or `From`.
/// - If the chain **has joins**, the base relation always gets `From`;
///   each joined relation gets its specific join type.
fn edges_from_join_chain(
    consumer_idx: usize,
    table: &TableWithJoins,
    index: &HashMap<String, usize>,
    union_type: Option<EdgeType>,
    edges: &mut Vec<CteEdge>,
) {
    if table.joins.is_empty() {
        // Plain FROM reference — use union context if present, else From.
        let base_type = union_type.unwrap_or(EdgeType::From);
        push_edge(&table.relation, consumer_idx, base_type, index, edges);
    } else {
        // JOIN chain: base gets From; each joined relation gets its type.
        push_edge(&table.relation, consumer_idx, EdgeType::From, index, edges);
        for join in &table.joins {
            if let Some(join_type) = classify_join(&join.join_operator) {
                push_edge(&join.relation, consumer_idx, join_type, index, edges);
            }
        }
    }
}

/// Append a `referenced_cte → consumer` edge when `factor` resolves to an
/// earlier-declared CTE. Drops external tables, self-references, forward
/// references, and duplicates.
fn push_edge(
    factor: &TableFactor,
    consumer_idx: usize,
    edge_type: EdgeType,
    index: &HashMap<String, usize>,
    edges: &mut Vec<CteEdge>,
) {
    let Some(source_idx) = resolve_factor(factor, index) else {
        return;
    };
    if source_idx >= consumer_idx {
        return;
    }
    let edge = CteEdge::new(source_idx, consumer_idx, edge_type);
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

/// Classify a `sqlparser` join operator into the [`EdgeType`] join
/// vocabulary, or `None` for join kinds outside it.
///
/// The catch-all arm is load-bearing: it keeps the engine forward
/// compatible with future `sqlparser` releases that add `JoinOperator`
/// variants, and it is where every non-vocabulary join (`SEMI` / `ANTI` /
/// `ASOF` / `APPLY` / `ARRAY JOIN`) lands.
fn classify_join(operator: &JoinOperator) -> Option<EdgeType> {
    use JoinOperator as Op;
    match operator {
        Op::Join(_) | Op::Inner(_) => Some(EdgeType::Inner),
        Op::Left(_) | Op::LeftOuter(_) => Some(EdgeType::Left),
        Op::Right(_) | Op::RightOuter(_) => Some(EdgeType::Right),
        Op::FullOuter(_) => Some(EdgeType::Full),
        Op::CrossJoin(_) => Some(EdgeType::Cross),
        _ => None,
    }
}

/// Classify a `UNION` set quantifier into [`EdgeType`].
///
/// All six sqlparser 0.62 variants collapse onto the two-kind vocabulary:
///
/// | Variant | `EdgeType` | Rationale |
/// |---|---|---|
/// | `All` | `UnionAll` | direct |
/// | `Distinct` | `UnionDistinct` | direct |
/// | `None` | `UnionDistinct` | plain `UNION` is semantically DISTINCT per SQL spec |
/// | `ByName` | `UnionDistinct` | by-name does not change graph topology |
/// | `AllByName` | `UnionAll` | all-row semantics; positional vs by-name is irrelevant to topology |
/// | `DistinctByName` | `UnionDistinct` | distinct-row semantics, same collapse as `Distinct` |
///
/// `SetQuantifier` is exhaustive in sqlparser 0.62 (no `#[non_exhaustive]`),
/// so all 6 variants are listed explicitly. A future sqlparser version that
/// adds new variants will produce a compile error here, which is the desired
/// behavior: each new variant needs a deliberate topology decision.
fn classify_union_quantifier(quantifier: SetQuantifier) -> EdgeType {
    match quantifier {
        SetQuantifier::All | SetQuantifier::AllByName => EdgeType::UnionAll,
        SetQuantifier::Distinct
        | SetQuantifier::None
        | SetQuantifier::ByName
        | SetQuantifier::DistinctByName => EdgeType::UnionDistinct,
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
    fn has_edge(g: &CteGraph, from: usize, to: usize, edge_type: EdgeType) -> bool {
        g.edges()
            .iter()
            .any(|e| e.from() == from && e.to() == to && e.edge_type() == edge_type)
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
    fn a_plain_from_emits_a_from_edge() {
        // `SELECT * FROM a` references the CTE `a` with no join —
        // the v0.1 model now carries a From edge for plain pass-through.
        let g = graph("WITH a AS (SELECT 1 AS id) SELECT * FROM a");
        assert_eq!(g.edges().len(), 1, "a plain FROM reference is a From edge");
        assert!(has_edge(&g, 0, 1, EdgeType::From), "a->terminal is From");
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
    fn each_join_keyword_classifies_its_joined_edge() {
        // The base relation `a` gets From; the joined relation `b` gets
        // its specific join type.
        let cases = [
            ("a JOIN b ON a.id = b.id", EdgeType::Inner),
            ("a INNER JOIN b ON a.id = b.id", EdgeType::Inner),
            ("a LEFT JOIN b ON a.id = b.id", EdgeType::Left),
            ("a RIGHT JOIN b ON a.id = b.id", EdgeType::Right),
            ("a FULL JOIN b ON a.id = b.id", EdgeType::Full),
            ("a CROSS JOIN b", EdgeType::Cross),
        ];
        for (from_clause, joined_type) in cases {
            let sql = format!(
                "WITH a AS (SELECT 1 AS id), b AS (SELECT 1 AS id) \
                 SELECT * FROM {from_clause}"
            );
            let g = graph(&sql);
            assert!(
                has_edge(&g, 0, 2, EdgeType::From),
                "`{from_clause}`: base `a` → terminal must be From",
            );
            assert!(
                has_edge(&g, 1, 2, joined_type),
                "`{from_clause}`: joined `b` → terminal must be {joined_type:?}",
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
        assert!(
            has_edge(&left, 0, 2, EdgeType::From),
            "base a->terminal is From"
        );
        assert!(has_edge(&left, 1, 2, EdgeType::Left), "b->terminal is Left");
        let right = graph(
            "WITH a AS (SELECT 1 AS id), b AS (SELECT 1 AS id) \
             SELECT * FROM a RIGHT OUTER JOIN b ON a.id = b.id",
        );
        assert!(
            has_edge(&right, 0, 2, EdgeType::From),
            "base a->terminal is From"
        );
        assert!(
            has_edge(&right, 1, 2, EdgeType::Right),
            "b->terminal is Right"
        );
    }

    #[test]
    fn base_gets_from_and_joined_relations_keep_their_join_type() {
        // Replaces `the_base_relation_takes_the_first_joins_type`:
        // `c` joins `a LEFT JOIN b` — base `a` gets From, joined `b` gets Left.
        let g = graph(
            "WITH a AS (SELECT 1 AS id), \
                  b AS (SELECT 1 AS id), \
                  c AS (SELECT * FROM a LEFT JOIN b ON a.id = b.id) \
             SELECT * FROM c",
        );
        assert!(has_edge(&g, 0, 2, EdgeType::From), "base a->c is From");
        assert!(has_edge(&g, 1, 2, EdgeType::Left), "joined b->c is Left");
        // The terminal SELECT * FROM c also emits a From edge.
        assert!(has_edge(&g, 2, 3, EdgeType::From), "c->terminal is From");
    }

    #[test]
    fn external_tables_are_not_nodes_and_not_edges() {
        let g = graph(
            "WITH a AS (SELECT * FROM raw_source JOIN other_raw ON 1 = 1) \
             SELECT * FROM a",
        );
        assert_eq!(g.nodes().len(), 2, "only the CTE and terminal are nodes");
        assert_eq!(g.edges().len(), 1, "only the From edge a->terminal");
        assert!(has_edge(&g, 0, 1, EdgeType::From), "a->terminal is From");
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
        // `selfish` is the base but it's a self-reference; `a` is the
        // joined relation with Inner type. Base-is-From rule still
        // applies to the base slot, but acyclicity drops the self-edge.
        assert!(
            has_edge(&g, 0, 1, EdgeType::Inner),
            "a->selfish resolves as Inner"
        );
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
        assert!(
            has_edge(&g, 0, 2, EdgeType::From),
            "orders (base) -> joined is From"
        );
        assert!(
            has_edge(&g, 1, 2, EdgeType::Inner),
            "customers -> joined is Inner"
        );
    }

    #[test]
    fn a_repeated_plain_from_reference_is_deduplicated() {
        // `FROM a JOIN a t2` — two references to the same CTE:
        // the base slot resolves to (a → terminal, From)
        // the joined slot resolves to (a → terminal, Inner).
        // They are DISTINCT edges (different edge_type), so both exist.
        let g = graph("WITH a AS (SELECT 1 AS id) SELECT * FROM a JOIN a t2 ON 1 = 1");
        assert_eq!(
            g.edges().len(),
            2,
            "From + Inner edges both exist (different edge_type)"
        );
        assert!(
            has_edge(&g, 0, 1, EdgeType::From),
            "base a->terminal is From"
        );
        assert!(
            has_edge(&g, 0, 1, EdgeType::Inner),
            "joined a->terminal is Inner"
        );
    }

    #[test]
    fn union_all_arms_emit_union_all_edges() {
        // A CTE body that is `SELECT * FROM a UNION ALL SELECT * FROM b` —
        // both arm references emit UnionAll.
        let g = graph(
            "WITH a AS (SELECT 1 AS id), \
                  b AS (SELECT 1 AS id), \
                  u AS (SELECT * FROM a UNION ALL SELECT * FROM b) \
             SELECT * FROM u",
        );
        assert!(has_edge(&g, 0, 2, EdgeType::UnionAll), "a->u is UnionAll");
        assert!(has_edge(&g, 1, 2, EdgeType::UnionAll), "b->u is UnionAll");
        // Terminal references u via a plain FROM.
        assert!(has_edge(&g, 2, 3, EdgeType::From), "u->terminal is From");
    }

    #[test]
    fn union_distinct_arms_emit_union_distinct_edges() {
        // Plain UNION and UNION DISTINCT both map to UnionDistinct.
        let g_plain = graph(
            "WITH a AS (SELECT 1 AS id), \
                  b AS (SELECT 1 AS id), \
                  u AS (SELECT * FROM a UNION SELECT * FROM b) \
             SELECT * FROM u",
        );
        assert!(
            has_edge(&g_plain, 0, 2, EdgeType::UnionDistinct),
            "a->u plain UNION is UnionDistinct"
        );
        assert!(
            has_edge(&g_plain, 1, 2, EdgeType::UnionDistinct),
            "b->u plain UNION is UnionDistinct"
        );
        let g_distinct = graph(
            "WITH a AS (SELECT 1 AS id), \
                  b AS (SELECT 1 AS id), \
                  u AS (SELECT * FROM a UNION DISTINCT SELECT * FROM b) \
             SELECT * FROM u",
        );
        assert!(
            has_edge(&g_distinct, 0, 2, EdgeType::UnionDistinct),
            "a->u UNION DISTINCT is UnionDistinct"
        );
        assert!(
            has_edge(&g_distinct, 1, 2, EdgeType::UnionDistinct),
            "b->u UNION DISTINCT is UnionDistinct"
        );
    }

    #[test]
    fn join_inside_union_arm_keeps_join_semantics_base_gets_from() {
        // `FROM a JOIN b UNION ALL SELECT * FROM c`:
        // left arm: base `a` gets From, joined `b` gets Inner
        // right arm: `c` gets UnionAll
        let g = graph(
            "WITH a AS (SELECT 1 AS id), \
                  b AS (SELECT 1 AS id), \
                  c AS (SELECT 1 AS id), \
                  u AS (SELECT * FROM a JOIN b ON 1 = 1 \
                        UNION ALL SELECT * FROM c) \
             SELECT * FROM u",
        );
        assert!(
            has_edge(&g, 0, 3, EdgeType::From),
            "left arm base a->u is From"
        );
        assert!(
            has_edge(&g, 1, 3, EdgeType::Inner),
            "left arm joined b->u is Inner"
        );
        assert!(
            has_edge(&g, 2, 3, EdgeType::UnionAll),
            "right arm c->u is UnionAll"
        );
    }

    #[test]
    fn non_union_set_operations_recurse_without_union_classification() {
        // EXCEPT: joins inside the arms still emit edges; no UnionAll/UnionDistinct.
        let g = graph(
            "WITH a AS (SELECT 1 AS id), \
                  b AS (SELECT 1 AS id), \
                  c AS (SELECT 1 AS id), \
                  ex AS (SELECT * FROM a JOIN b ON 1 = 1 \
                         EXCEPT SELECT * FROM c) \
             SELECT * FROM ex",
        );
        assert!(
            has_edge(&g, 0, 3, EdgeType::From),
            "a is base -> From in EXCEPT arm"
        );
        assert!(
            has_edge(&g, 1, 3, EdgeType::Inner),
            "b joins in EXCEPT arm -> Inner"
        );
        assert!(
            has_edge(&g, 2, 3, EdgeType::From),
            "c is plain FROM in EXCEPT arm -> From"
        );
        // No UnionAll or UnionDistinct edges.
        assert!(
            !g.edges()
                .iter()
                .any(|e| matches!(e.edge_type(), EdgeType::UnionAll | EdgeType::UnionDistinct)),
            "EXCEPT does not produce union-type edges",
        );
    }

    #[test]
    fn set_operations_in_a_body_are_walked() {
        // A CTE whose body is `… UNION ALL …` has both arms scanned;
        // joins inside the left arm retain their semantics.
        let g = graph(
            "WITH a AS (SELECT 1 AS id), \
                  b AS (SELECT 1 AS id), \
                  u AS (SELECT * FROM a JOIN b ON 1 = 1 \
                        UNION ALL SELECT * FROM a) \
             SELECT * FROM u",
        );
        // Left arm: a (base→From), b (joined→Inner).
        assert!(
            has_edge(&g, 0, 2, EdgeType::From),
            "left arm: base a->u is From"
        );
        assert!(
            has_edge(&g, 1, 2, EdgeType::Inner),
            "left arm: b->u is Inner"
        );
        // Right arm: a (plain FROM, union context) → UnionAll.
        assert!(
            has_edge(&g, 0, 2, EdgeType::UnionAll),
            "right arm: a->u is UnionAll"
        );
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
        assert!(
            has_edge(&g, 0, 2, EdgeType::From),
            "a (base) -> wrapped is From"
        );
        assert!(has_edge(&g, 1, 2, EdgeType::Inner), "b -> wrapped is Inner");
    }

    #[test]
    fn classify_join_maps_the_full_vocabulary() {
        use sqlparser::ast::JoinConstraint::None as NoConstraint;
        assert_eq!(
            classify_join(&JoinOperator::Inner(NoConstraint)),
            Some(EdgeType::Inner),
        );
        assert_eq!(
            classify_join(&JoinOperator::Left(NoConstraint)),
            Some(EdgeType::Left),
        );
        assert_eq!(
            classify_join(&JoinOperator::Right(NoConstraint)),
            Some(EdgeType::Right),
        );
        assert_eq!(
            classify_join(&JoinOperator::FullOuter(NoConstraint)),
            Some(EdgeType::Full),
        );
        assert_eq!(
            classify_join(&JoinOperator::CrossJoin(NoConstraint)),
            Some(EdgeType::Cross),
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
    fn classify_union_quantifier_maps_all_six_variants() {
        // The three direct variants.
        assert_eq!(
            classify_union_quantifier(SetQuantifier::All),
            EdgeType::UnionAll,
        );
        assert_eq!(
            classify_union_quantifier(SetQuantifier::Distinct),
            EdgeType::UnionDistinct,
        );
        assert_eq!(
            classify_union_quantifier(SetQuantifier::None),
            EdgeType::UnionDistinct,
            "plain UNION (no quantifier) is semantically DISTINCT per SQL spec",
        );
        // The three ByName variants collapse to the same topology as their
        // positional counterparts — by-name vs positional does not affect the
        // CTE dependency graph.
        assert_eq!(
            classify_union_quantifier(SetQuantifier::AllByName),
            EdgeType::UnionAll,
            "UNION ALL BY NAME collapses to UnionAll",
        );
        assert_eq!(
            classify_union_quantifier(SetQuantifier::ByName),
            EdgeType::UnionDistinct,
            "UNION BY NAME collapses to UnionDistinct",
        );
        assert_eq!(
            classify_union_quantifier(SetQuantifier::DistinctByName),
            EdgeType::UnionDistinct,
            "UNION DISTINCT BY NAME collapses to UnionDistinct",
        );
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
        // Enumerated property test: for every sample, node count matches
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

    #[test]
    fn a_standard_with_clause_is_not_recursive() {
        let g = graph("WITH a AS (SELECT 1 AS id) SELECT * FROM a");
        assert!(
            !g.is_recursive(),
            "a standard WITH clause is not flagged as recursive",
        );
    }

    #[test]
    fn with_recursive_sets_the_is_recursive_flag() {
        // sqlparser parses WITH RECURSIVE correctly; the engine sets the
        // flag so the renderer can surface a banner. The self-referencing
        // edge (n → n) is dropped by the from < to acyclicity guard.
        let g = graph(
            "WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t WHERE n < 10) \
             SELECT * FROM t",
        );
        assert!(
            g.is_recursive(),
            "WITH RECURSIVE query sets the is_recursive flag",
        );
        // The recursive CTE appears as a node, but the self-referencing
        // edge is dropped (n.from < n.to invariant). Nodes: t + terminal.
        assert_eq!(g.nodes().len(), 2, "one CTE node plus the terminal");
        for edge in g.edges() {
            assert!(
                edge.from() < edge.to(),
                "all emitted edges remain acyclic even in recursive queries",
            );
        }
    }
}
