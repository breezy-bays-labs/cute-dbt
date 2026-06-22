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
    BinaryOperator, Cte, Expr, JoinConstraint, JoinOperator, Query, Select, SelectItem,
    SelectItemQualifiedWildcardKind, SetExpr, SetOperator, SetQuantifier, Spanned, Statement,
    TableFactor, TableWithJoins,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Location, Span};

use crate::domain::{
    CteEdge, CteGraph, CteNode, EdgeType, JoinKeyPair, LeftJoinFact, SourcePos, SourceSpan,
    SubqueryFact, SubqueryKind,
};

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
    Ok(build_graph(compiled_sql, &query))
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
fn build_graph(compiled_sql: &str, query: &Query) -> CteGraph {
    let ctes = cte_tables(query);
    if ctes.is_empty() {
        // No CTE structure to visualise — but the body's LEFT JOIN
        // facts still surface (cute-dbt#173): the catalog C4 canonical
        // shape is a WITH-less `… FROM a LEFT JOIN b …` model. The
        // cute-dbt#196 subquery facts ride the same path (the WITH-less
        // `… WHERE NOT EXISTS (…)` anti-join is just as canonical).
        let facts = compute_shape_facts(TERMINAL_NODE_NAME, &query.body);
        return CteGraph::default()
            .with_left_join_facts(facts.left_joins)
            .with_subquery_facts(facts.subqueries);
    }
    let recursive = query.with.as_ref().is_some_and(|with| with.recursive);
    let (nodes, left_joins, subqueries) = build_nodes(compiled_sql, ctes, query);
    let index = name_index(ctes);
    let edges = build_edges(ctes, query, &index);
    let graph = CteGraph::new(nodes, edges)
        .with_left_join_facts(left_joins)
        .with_subquery_facts(subqueries);
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
/// Per-CTE `raw_sql` is sliced from the original `compiled_code` via
/// each CTE's [`Spanned::span()`] (alias-start through close-paren).
/// This preserves SQL comments authored in the CTE body that sqlparser
/// drops through the `parse → Display` roundtrip (cute-dbt#31).
/// Falls back to AST `to_string()` when the span is empty (defensive —
/// sqlparser 0.62 populates spans for every CTE we've observed).
///
/// Each node also carries engine-computed structural facts derived
/// from the parsed body — `is_simple_from_shape` and
/// `body_leaf_table_refs` (cute-dbt#40). The renderer reads these
/// directly via the POD accessors; it never re-parses the slice.
///
/// The second and third return values aggregate every body's
/// per-LEFT-JOIN facts (cute-dbt#173) and per-negated-subquery facts
/// (cute-dbt#196), each tagged with its consumer body's node name —
/// they hang off the [`CteGraph`] for the domain check detectors.
fn build_nodes(
    compiled_sql: &str,
    ctes: &[Cte],
    query: &Query,
) -> (Vec<CteNode>, Vec<LeftJoinFact>, Vec<SubqueryFact>) {
    let byte_index = ByteIndex::new(compiled_sql);
    let mut left_joins: Vec<LeftJoinFact> = Vec::new();
    let mut subqueries: Vec<SubqueryFact> = Vec::new();
    let mut nodes: Vec<CteNode> = ctes
        .iter()
        .map(|cte| {
            let (raw, source_span) =
                slice_or_fallback(compiled_sql, &byte_index, cte.span(), || {
                    cte.query.to_string()
                });
            let facts = compute_shape_facts(cte_name(cte), &cte.query.body);
            left_joins.extend(facts.left_joins);
            subqueries.extend(facts.subqueries);
            CteNode::new(cte_name(cte), source_span, Some(raw), None)
                .with_shape_facts(facts.is_simple, facts.leaf_refs)
        })
        .collect();
    let (terminal_raw, terminal_span) = match slice_terminal(compiled_sql, &byte_index, ctes) {
        Some((raw, span)) => (raw, Some(span)),
        None => (query.body.to_string(), None),
    };
    let terminal_facts = compute_shape_facts(TERMINAL_NODE_NAME, &query.body);
    left_joins.extend(terminal_facts.left_joins);
    subqueries.extend(terminal_facts.subqueries);
    nodes.push(
        CteNode::new(TERMINAL_NODE_NAME, terminal_span, Some(terminal_raw), None)
            .with_shape_facts(terminal_facts.is_simple, terminal_facts.leaf_refs),
    );
    (nodes, left_joins, subqueries)
}

/// Engine-computed structural facts about a query body
/// (cute-dbt#40 Option C; `left_joins` added by cute-dbt#173;
/// `subqueries` by cute-dbt#196).
struct BodyShapeFacts {
    is_simple: bool,
    leaf_refs: Vec<String>,
    left_joins: Vec<LeftJoinFact>,
    subqueries: Vec<SubqueryFact>,
}

/// Compute the structural facts the renderer needs to classify a CTE
/// node's role and resolve import-CTE body matches — without ever
/// exposing the AST outside the adapter layer. The per-LEFT-JOIN facts
/// (cute-dbt#173) and per-negated-subquery facts (cute-dbt#196) ride
/// the same pass over the same parsed AST: never a second parse.
/// `consumer` is the body's node name — a CTE name or
/// [`TERMINAL_NODE_NAME`] — tagged onto each fact.
fn compute_shape_facts(consumer: &str, body: &SetExpr) -> BodyShapeFacts {
    let mut leaf_refs = Vec::new();
    collect_leaf_table_refs(body, &mut leaf_refs);
    let mut left_joins = Vec::new();
    collect_left_join_facts(consumer, body, &mut left_joins);
    let mut subqueries = Vec::new();
    collect_subquery_facts(consumer, body, &mut subqueries);
    BodyShapeFacts {
        is_simple: is_body_simple_from_select(body),
        leaf_refs,
        left_joins,
        subqueries,
    }
}

// ---------------------------------------------------------------------
// Per-LEFT-JOIN facts (cute-dbt#173 — catalog class C4).
// ---------------------------------------------------------------------

/// Walk `body` recursively (the [`collect_leaf_table_refs`] descent
/// shape), appending one [`LeftJoinFact`] per `LEFT [OUTER] JOIN` whose
/// right side is a plain named table factor. Derived-table right sides
/// emit no fact (a declared check exclusion).
fn collect_left_join_facts(consumer: &str, body: &SetExpr, facts: &mut Vec<LeftJoinFact>) {
    match body {
        SetExpr::Select(select) => select_left_join_facts(consumer, select, facts),
        SetExpr::Query(inner) => collect_left_join_facts(consumer, &inner.body, facts),
        SetExpr::SetOperation { left, right, .. } => {
            collect_left_join_facts(consumer, left, facts);
            collect_left_join_facts(consumer, right, facts);
        }
        _ => {}
    }
}

/// Facts for every LEFT JOIN in one `SELECT`: the joined relation's
/// leaf, the `ON` equi-key pairs, the right-qualified `IS NULL`
/// top-level `WHERE` conjunct columns (the anti-join where-predicate
/// fact), whether right-side columns provably reach the projection, and
/// whether the `SELECT` dedups with `DISTINCT`.
fn select_left_join_facts(consumer: &str, select: &Select, facts: &mut Vec<LeftJoinFact>) {
    let aliases = select_alias_map(select);
    for table in &select.from {
        for join in &table.joins {
            let (JoinOperator::Left(constraint) | JoinOperator::LeftOuter(constraint)) =
                &join.join_operator
            else {
                continue;
            };
            let Some((right_qualifier, right_leaf)) = factor_qualifier_and_leaf(&join.relation)
            else {
                continue;
            };
            facts.push(LeftJoinFact::new(
                consumer,
                right_leaf,
                on_equi_key_pairs(constraint, &right_qualifier, &aliases),
                where_is_null_columns(select.selection.as_ref(), &right_qualifier),
                projection_carries_qualifier(&select.projection, &right_qualifier),
                select.distinct.is_some(),
            ));
        }
    }
}

/// Lowercased qualifier (alias if present, else leaf name) → lowercased
/// leaf name, for every plain named table factor in the `SELECT`'s
/// `FROM` chains. Resolves the left side of an `ON` equi-predicate to
/// the relation it reads.
fn select_alias_map(select: &Select) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for table in &select.from {
        insert_factor_alias(&table.relation, &mut map);
        for join in &table.joins {
            insert_factor_alias(&join.relation, &mut map);
        }
    }
    map
}

/// Insert one factor's `(qualifier, leaf)` pair into `map` when the
/// factor is a plain named table.
fn insert_factor_alias(factor: &TableFactor, map: &mut HashMap<String, String>) {
    if let Some((qualifier, leaf)) = factor_qualifier_and_leaf(factor) {
        map.insert(qualifier, leaf);
    }
}

/// `(qualifier, leaf)` of a plain named table factor, both lowercased:
/// the qualifier is the alias when present (`stg_customers AS c` → `c`),
/// else the leaf name itself. `None` for derived tables / table
/// functions / nested joins.
fn factor_qualifier_and_leaf(factor: &TableFactor) -> Option<(String, String)> {
    let TableFactor::Table { name, alias, .. } = factor else {
        return None;
    };
    let leaf = name.0.last()?.as_ident()?.value.to_ascii_lowercase();
    let qualifier = alias
        .as_ref()
        .map_or_else(|| leaf.clone(), |a| a.name.value.to_ascii_lowercase());
    Some((qualifier, leaf))
}

/// The equi-key pairs of one `ON` constraint: each top-level `AND`
/// conjunct of the form `<q1>.<col1> = <q2>.<col2>` where exactly one
/// side is qualified by the joined relation. `USING` / `NATURAL` /
/// missing constraints and non-equi or unqualified predicates yield no
/// pairs (the join key is then not statically recoverable — a declared
/// check exclusion that degrades to UNKNOWN, never UNCOVERED).
fn on_equi_key_pairs(
    constraint: &JoinConstraint,
    right_qualifier: &str,
    aliases: &HashMap<String, String>,
) -> Vec<JoinKeyPair> {
    let JoinConstraint::On(expr) = constraint else {
        return Vec::new();
    };
    let mut conjuncts = Vec::new();
    collect_and_conjuncts(expr, &mut conjuncts);
    conjuncts
        .iter()
        .filter_map(|conjunct| equi_key_pair(conjunct, right_qualifier, aliases))
        .collect()
}

/// Flatten an expression's top-level `AND` tree (descending through
/// parentheses) into its conjuncts. An `OR` is a single conjunct — its
/// branches are deliberately not decomposed (different semantics).
fn collect_and_conjuncts<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    match expr {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => {
            collect_and_conjuncts(left, out);
            collect_and_conjuncts(right, out);
        }
        Expr::Nested(inner) => collect_and_conjuncts(inner, out),
        other => out.push(other),
    }
}

/// `(qualifier, column)` of a qualified column reference, lowercased —
/// the last two parts of a `CompoundIdentifier` (`o.customer_id`,
/// `"db"."schema"."t".col`). `None` for unqualified identifiers and
/// non-column expressions.
fn qualified_column(expr: &Expr) -> Option<(String, String)> {
    let Expr::CompoundIdentifier(parts) = expr else {
        return None;
    };
    if parts.len() < 2 {
        return None;
    }
    let column = parts.last()?.value.to_ascii_lowercase();
    let qualifier = parts[parts.len() - 2].value.to_ascii_lowercase();
    Some((qualifier, column))
}

/// Classify one `ON` conjunct as an equi-key pair for the join
/// qualified by `right_qualifier`: `=` between two qualified columns,
/// exactly one side carrying the right qualifier. The left side's
/// qualifier resolves to its relation leaf via `aliases` (or `None`
/// when unresolvable).
fn equi_key_pair(
    expr: &Expr,
    right_qualifier: &str,
    aliases: &HashMap<String, String>,
) -> Option<JoinKeyPair> {
    let Expr::BinaryOp {
        left,
        op: BinaryOperator::Eq,
        right,
    } = expr
    else {
        return None;
    };
    let a = qualified_column(left)?;
    let b = qualified_column(right)?;
    let (right_side, left_side) = if a.0 == right_qualifier && b.0 != right_qualifier {
        (a, b)
    } else if b.0 == right_qualifier && a.0 != right_qualifier {
        (b, a)
    } else {
        return None;
    };
    Some(JoinKeyPair::new(
        aliases.get(&left_side.0).cloned(),
        left_side.1,
        right_side.1,
    ))
}

/// Lowercased columns qualified by `right_qualifier` appearing under
/// `IS NULL` in the `WHERE` clause's top-level `AND` conjuncts — the
/// anti-join where-predicate fact (cute-dbt#173). An `IS NULL` inside
/// an `OR` or on an unqualified column is never recorded.
fn where_is_null_columns(selection: Option<&Expr>, right_qualifier: &str) -> Vec<String> {
    let Some(expr) = selection else {
        return Vec::new();
    };
    let mut conjuncts = Vec::new();
    collect_and_conjuncts(expr, &mut conjuncts);
    let mut columns: Vec<String> = Vec::new();
    for conjunct in conjuncts {
        let Expr::IsNull(inner) = conjunct else {
            continue;
        };
        let Some((qualifier, column)) = qualified_column(inner) else {
            continue;
        };
        if qualifier == right_qualifier && !columns.contains(&column) {
            columns.push(column);
        }
    }
    columns
}

// ---------------------------------------------------------------------
// Per-negated-subquery facts (cute-dbt#196 — the correlated-subquery
// evidence family, v1).
// ---------------------------------------------------------------------

/// Walk `body` recursively (the [`collect_left_join_facts`] descent
/// shape), appending one [`SubqueryFact`] per detected negated
/// subquery in a `SELECT`'s top-level `WHERE` `AND` conjuncts. Negated
/// forms anywhere else (OR branches, HAVING, JOIN ON, projections) and
/// non-negated `EXISTS` / `IN` (semi-join / membership — future
/// consumers) are deliberately NOT extracted.
fn collect_subquery_facts(consumer: &str, body: &SetExpr, facts: &mut Vec<SubqueryFact>) {
    match body {
        SetExpr::Select(select) => select_subquery_facts(consumer, select, facts),
        SetExpr::Query(inner) => collect_subquery_facts(consumer, &inner.body, facts),
        SetExpr::SetOperation { left, right, .. } => {
            collect_subquery_facts(consumer, left, facts);
            collect_subquery_facts(consumer, right, facts);
        }
        _ => {}
    }
}

/// Facts for every conforming `NOT EXISTS` / `NOT IN` subquery in one
/// `SELECT`'s top-level `WHERE` `AND` conjuncts.
fn select_subquery_facts(consumer: &str, select: &Select, facts: &mut Vec<SubqueryFact>) {
    let Some(selection) = select.selection.as_ref() else {
        return;
    };
    let outer_aliases = select_alias_map(select);
    let mut conjuncts = Vec::new();
    collect_and_conjuncts(selection, &mut conjuncts);
    for conjunct in conjuncts {
        let fact = match conjunct {
            Expr::Exists {
                subquery,
                negated: true,
            } => not_exists_fact(consumer, subquery, &outer_aliases),
            Expr::InSubquery {
                expr,
                subquery,
                negated: true,
            } => not_in_fact(consumer, expr, subquery, &outer_aliases, select),
            _ => None,
        };
        facts.extend(fact);
    }
}

/// The single plain named inner relation of a conforming subquery —
/// `(inner Select, inner leaf)` — or `None` when the subquery carries
/// its own `WITH` clause, is a set operation, reads more than one
/// relation, joins, or reads anything but a plain named table (derived
/// tables, table functions): all declared exclusions, silent by
/// construction.
fn conforming_inner(subquery: &Query) -> Option<(&Select, String)> {
    if subquery.with.is_some() {
        return None;
    }
    let SetExpr::Select(inner) = subquery.body.as_ref() else {
        return None;
    };
    if inner.from.len() != 1 || !inner.from[0].joins.is_empty() {
        return None;
    }
    let (_, inner_leaf) = factor_qualifier_and_leaf(&inner.from[0].relation)?;
    Some((inner, inner_leaf))
}

/// Classify one `NOT EXISTS (SELECT … FROM <inner> WHERE …)` conjunct
/// (cute-dbt#196). A fact is emitted only for a **correlated** subquery
/// over a conforming single-relation inner: at least one inner-`WHERE`
/// qualified reference must resolve in the OUTER alias map (and not in
/// the inner one — SQL scoping: the innermost binding wins). The
/// `equi_keys` are the inner-`WHERE` top-level `AND` equi-conjuncts
/// between one inner-resolvable and one outer-resolvable column,
/// normalized OUTER-side-left. Resolvable correlation with NO
/// resolvable equi pair yields a fact with EMPTY keys (the downstream
/// bind fails → honest UNKNOWN). An uncorrelated `NOT EXISTS` is not a
/// keyed anti-join — no fact, silence.
fn not_exists_fact(
    consumer: &str,
    subquery: &Query,
    outer_aliases: &HashMap<String, String>,
) -> Option<SubqueryFact> {
    let (inner, inner_leaf) = conforming_inner(subquery)?;
    let inner_where = inner.selection.as_ref()?;
    let inner_aliases = select_alias_map(inner);
    let mut refs = Vec::new();
    collect_qualified_refs(inner_where, &mut refs);
    let correlated = refs
        .iter()
        .any(|(qualifier, _)| resolves_outer(qualifier, &inner_aliases, outer_aliases));
    if !correlated {
        return None;
    }
    let mut conjuncts = Vec::new();
    collect_and_conjuncts(inner_where, &mut conjuncts);
    let pairs = conjuncts
        .iter()
        .filter_map(|conjunct| correlated_equi_pair(conjunct, &inner_aliases, outer_aliases))
        .collect();
    Some(SubqueryFact::new(
        SubqueryKind::NotExists,
        consumer,
        inner_leaf,
        pairs,
    ))
}

/// Classify one `<col> NOT IN (SELECT <col> FROM <inner>)` conjunct
/// (cute-dbt#196). The inner must be a conforming single-relation
/// subquery projecting EXACTLY one column; the outer expression must
/// itself be a column reference. The membership pair (outer column ↔
/// inner projected column) is the single equi key. An outer column
/// whose side does not resolve (unknown qualifier, or unqualified over
/// a multi-relation outer `FROM`) yields a fact with EMPTY keys (→
/// UNKNOWN); a non-column outer expression or non-conforming inner
/// yields no fact.
fn not_in_fact(
    consumer: &str,
    outer_expr: &Expr,
    subquery: &Query,
    outer_aliases: &HashMap<String, String>,
    outer_select: &Select,
) -> Option<SubqueryFact> {
    let (inner, inner_leaf) = conforming_inner(subquery)?;
    let inner_aliases = select_alias_map(inner);
    let inner_column = single_projected_column(&inner.projection, &inner_aliases)?;
    let (outer_qualifier, outer_column) = match outer_expr {
        Expr::Identifier(ident) => (None, ident.value.to_ascii_lowercase()),
        Expr::CompoundIdentifier(_) => {
            let (qualifier, column) = qualified_column(outer_expr)?;
            (Some(qualifier), column)
        }
        _ => return None,
    };
    let left_leaf = match outer_qualifier {
        Some(qualifier) => outer_aliases.get(&qualifier).cloned(),
        // An unqualified column resolves only when the outer FROM reads
        // exactly one relation.
        None => sole_relation_leaf(outer_select),
    };
    let pairs = left_leaf.map_or_else(Vec::new, |leaf| {
        vec![JoinKeyPair::new(Some(leaf), outer_column, inner_column)]
    });
    Some(SubqueryFact::new(
        SubqueryKind::NotIn,
        consumer,
        inner_leaf,
        pairs,
    ))
}

/// `true` when `qualifier` resolves in the OUTER alias map and not in
/// the inner one — the correlation-evidence test (SQL scoping: a
/// qualifier bound by the inner `FROM` shadows the outer binding).
fn resolves_outer(
    qualifier: &str,
    inner_aliases: &HashMap<String, String>,
    outer_aliases: &HashMap<String, String>,
) -> bool {
    !inner_aliases.contains_key(qualifier) && outer_aliases.contains_key(qualifier)
}

/// Classify one inner-`WHERE` conjunct as a correlated equi-key pair:
/// `=` between two qualified columns, one resolving in the INNER alias
/// map and the other in the OUTER one. Normalized OUTER-side-left —
/// the inner relation plays the LEFT JOIN's right-leaf role, so the
/// pair binds through the existing key-match machinery unchanged.
fn correlated_equi_pair(
    expr: &Expr,
    inner_aliases: &HashMap<String, String>,
    outer_aliases: &HashMap<String, String>,
) -> Option<JoinKeyPair> {
    let Expr::BinaryOp {
        left,
        op: BinaryOperator::Eq,
        right,
    } = expr
    else {
        return None;
    };
    let a = qualified_column(left)?;
    let b = qualified_column(right)?;
    let a_inner = inner_aliases.contains_key(&a.0);
    let b_inner = inner_aliases.contains_key(&b.0);
    let (inner_side, outer_side) = match (a_inner, b_inner) {
        (true, false) if resolves_outer(&b.0, inner_aliases, outer_aliases) => (a, b),
        (false, true) if resolves_outer(&a.0, inner_aliases, outer_aliases) => (b, a),
        _ => return None,
    };
    Some(JoinKeyPair::new(
        outer_aliases.get(&outer_side.0).cloned(),
        outer_side.1,
        inner_side.1,
    ))
}

/// Append every qualified column reference under `expr`, descending
/// the predicate shapes correlated anti-join `WHERE`s realistically
/// use (AND/OR trees, comparisons, IS \[NOT\] NULL, BETWEEN, IN lists,
/// LIKE, CAST, parens, unary NOT). Unknown variants are deliberately
/// not descended: a correlation reference hidden in an exotic shape
/// yields no evidence, so the fact is not emitted — silence, never
/// misclassification.
fn collect_qualified_refs(expr: &Expr, refs: &mut Vec<(String, String)>) {
    match expr {
        Expr::CompoundIdentifier(_) => refs.extend(qualified_column(expr)),
        Expr::BinaryOp { left, right, .. } => {
            collect_qualified_refs(left, refs);
            collect_qualified_refs(right, refs);
        }
        Expr::UnaryOp { expr: inner, .. }
        | Expr::Nested(inner)
        | Expr::IsNull(inner)
        | Expr::IsNotNull(inner)
        | Expr::Cast { expr: inner, .. } => collect_qualified_refs(inner, refs),
        Expr::Between {
            expr: inner,
            low,
            high,
            ..
        } => {
            collect_qualified_refs(inner, refs);
            collect_qualified_refs(low, refs);
            collect_qualified_refs(high, refs);
        }
        Expr::InList {
            expr: inner, list, ..
        } => {
            collect_qualified_refs(inner, refs);
            for item in list {
                collect_qualified_refs(item, refs);
            }
        }
        Expr::Like {
            expr: inner,
            pattern,
            ..
        }
        | Expr::ILike {
            expr: inner,
            pattern,
            ..
        } => {
            collect_qualified_refs(inner, refs);
            collect_qualified_refs(pattern, refs);
        }
        _ => {}
    }
}

/// The single projected column of a `NOT IN` inner subquery — lowercased,
/// the underlying column the membership values come from (an alias
/// renames the output; the given mocks the relation's raw columns).
/// `None` unless the projection is exactly one plain or aliased column
/// reference whose qualifier (when present) resolves in the inner alias
/// map.
fn single_projected_column(
    projection: &[SelectItem],
    inner_aliases: &HashMap<String, String>,
) -> Option<String> {
    let [item] = projection else {
        return None;
    };
    let (SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. }) = item else {
        return None;
    };
    match expr {
        Expr::Identifier(ident) => Some(ident.value.to_ascii_lowercase()),
        Expr::CompoundIdentifier(_) => {
            let (qualifier, column) = qualified_column(expr)?;
            inner_aliases.contains_key(&qualifier).then_some(column)
        }
        _ => None,
    }
}

/// The leaf of the `SELECT`'s sole relation — `None` unless the `FROM`
/// reads exactly one table factor of any kind AND that factor is a
/// plain named table (an unqualified outer column over several
/// relations, or over a derived table, is ambiguous — unresolvable).
fn sole_relation_leaf(select: &Select) -> Option<String> {
    let mut factors = select.from.iter().flat_map(|table| {
        std::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation))
    });
    let first = factors.next()?;
    if factors.next().is_some() {
        return None;
    }
    factor_qualifier_and_leaf(first).map(|(_, leaf)| leaf)
}

/// `true` when the projection **provably** carries columns of the
/// relation referred to by `qualifier`: a bare `*`, a `<qualifier>.*`
/// qualified wildcard, or a direct `<qualifier>.<column>` item. Columns
/// reaching the output only through expressions (`COALESCE(q.col, …)`,
/// `CASE …`) deliberately do not count — the conservative direction the
/// left-null-propagation check's exclusion documents.
fn projection_carries_qualifier(projection: &[SelectItem], qualifier: &str) -> bool {
    projection.iter().any(|item| match item {
        SelectItem::Wildcard(_) => true,
        SelectItem::QualifiedWildcard(kind, _) => match kind {
            SelectItemQualifiedWildcardKind::ObjectName(name) => name
                .0
                .last()
                .and_then(|part| part.as_ident())
                .is_some_and(|ident| ident.value.eq_ignore_ascii_case(qualifier)),
            SelectItemQualifiedWildcardKind::Expr(_) => false,
        },
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
            qualified_column(expr).is_some_and(|(item_qualifier, _)| item_qualifier == qualifier)
        }
        // Spark's `expr AS (a, b, …)` multi-alias form — not a direct
        // column reference; never provably right-qualified.
        SelectItem::ExprWithAliases { .. } => false,
    })
}

/// `true` when `body` is a single `SELECT … FROM <Table>` with no joins
/// — the import-CTE shape. The top-level form must be `SetExpr::Select`;
/// a `UNION`, parenthesised query, or anything else is not "simple"
/// even if the AST walk would eventually find a single source. The
/// renderer's `NodeRole::Import` classification keys off this fact.
fn is_body_simple_from_select(body: &SetExpr) -> bool {
    let SetExpr::Select(select) = body else {
        return false;
    };
    select.from.len() == 1
        && select.from[0].joins.is_empty()
        && matches!(select.from[0].relation, TableFactor::Table { .. })
}

/// Walk `body` recursively, appending the lowercased leaf identifier of
/// every `TableFactor::Table` reference in any `FROM` or `JOIN` to
/// `refs`. Descends through parenthesised subqueries, derived tables
/// (`(SELECT …) AS alias`), and set operations so the renderer's
/// import-CTE body match sees every reachable leaf.
fn collect_leaf_table_refs(body: &SetExpr, refs: &mut Vec<String>) {
    match body {
        SetExpr::Select(select) => {
            for table in &select.from {
                collect_leaves_from_join_chain(table, refs);
            }
        }
        SetExpr::Query(inner) => collect_leaf_table_refs(&inner.body, refs),
        SetExpr::SetOperation { left, right, .. } => {
            collect_leaf_table_refs(left, refs);
            collect_leaf_table_refs(right, refs);
        }
        _ => {}
    }
}

/// Collect leaves from one `FROM` join chain — the base relation plus
/// every joined relation. Derived tables (subqueries with an alias)
/// recurse into [`collect_leaf_table_refs`] so subquery leaves surface.
fn collect_leaves_from_join_chain(table: &TableWithJoins, refs: &mut Vec<String>) {
    push_leaf(&table.relation, refs);
    for join in &table.joins {
        push_leaf(&join.relation, refs);
    }
}

/// Append the lowercased leaf identifier of `factor` to `refs` when
/// `factor` is a plain named table; recurse into a derived subquery's
/// body when it is a `TableFactor::Derived`; otherwise drop.
fn push_leaf(factor: &TableFactor, refs: &mut Vec<String>) {
    match factor {
        TableFactor::Table { name, .. } => {
            if let Some(ident) = name.0.last().and_then(|p| p.as_ident()) {
                refs.push(ident.value.to_ascii_lowercase());
            }
        }
        TableFactor::Derived { subquery, .. } => {
            collect_leaf_table_refs(&subquery.body, refs);
        }
        _ => {}
    }
}

/// Slice `sql` over `span` (start inclusive, end exclusive per sqlparser
/// 0.62 token spans — `closing_paren_token` reports
/// `(Location(L, C), Location(L, C+1))` for the single-character `)`).
///
/// Returns `(text, Some(source_span))` on success, where `source_span`
/// carries the 1-based line/col endpoints (from the sqlparser `Span`) plus
/// the 0-based UTF-8 byte offsets (from [`ByteIndex`]) — the retained fact
/// `build_nodes` attaches to the `CteNode` (cute-dbt#444). `sql[span.byte_range()]`
/// byte-equals the returned text by construction.
///
/// Returns `(fallback(), None)` when the span is empty or yields an
/// out-of-bounds range (defensive — sqlparser populates spans for every CTE
/// we've observed, but the engine must never panic on a fixture, and a
/// degraded span is `None` rather than a lie).
fn slice_or_fallback<F: FnOnce() -> String>(
    sql: &str,
    index: &ByteIndex,
    span: Span,
    fallback: F,
) -> (String, Option<SourceSpan>) {
    if span.start.line == 0 || span.end.line == 0 {
        return (fallback(), None);
    }
    let start = index.byte_of(sql, span.start);
    let end = index.byte_of(sql, span.end);
    if start > end || end > sql.len() {
        return (fallback(), None);
    }
    // The non-terminal `CteBody` span is the untrimmed `name AS ( … )`
    // range; its line/col endpoints come straight from the sqlparser `Span`.
    let source_span = SourceSpan {
        start: loc_to_pos(span.start, start),
        end: loc_to_pos(span.end, end),
    };
    (sql[start..end].to_owned(), Some(source_span))
}

/// Slice the terminal `SELECT` — everything after the last CTE's
/// closing paren to end-of-`sql`.
///
/// Starts at the byte position immediately past `)` so any comments or
/// whitespace between the `WITH` clause and the final `SELECT` survive.
/// Returns `Some((text, source_span))` where `source_span.start.byte` is
/// advanced PAST the same leading-trim prefix the text strips
/// (`[',', '\n', '\r', ' ', '\t']`, cute-dbt#444 Blocker-1) so
/// `sql[source_span.byte_range()]` byte-equals the trimmed text — NOT the
/// raw closing-paren-end offset. The end endpoint is end-of-`sql`. Returns
/// `None` when no CTEs are present (the caller falls back to the AST
/// roundtrip, though `build_nodes` only fires this branch when at least one
/// CTE exists).
fn slice_terminal(sql: &str, index: &ByteIndex, ctes: &[Cte]) -> Option<(String, SourceSpan)> {
    let last = ctes.last()?;
    let close_end = last.closing_paren_token.0.span.end;
    if close_end.line == 0 {
        return None;
    }
    let raw_start = index.byte_of(sql, close_end);
    if raw_start > sql.len() {
        return None;
    }
    let tail = &sql[raw_start..];
    let trimmed = tail.trim_start_matches([',', '\n', '\r', ' ', '\t']);
    // The post-trim start byte: raw_start advanced by the stripped prefix
    // length (Blocker-1). `trimmed` is a suffix of `tail`, so the prefix
    // length is `tail.len() - trimmed.len()`.
    let start_byte = raw_start + (tail.len() - trimmed.len());
    let source_span = SourceSpan {
        start: index.pos_at(sql, start_byte),
        end: index.pos_at(sql, sql.len()),
    };
    Some((trimmed.to_owned(), source_span))
}

/// The guarded `→ u32` narrowing at the ONE ingestion boundary where a span
/// byte/line/col value becomes a [`SourcePos`] field (cute-dbt#444, Part I §5
/// FIX 6) — fusion widened minijinja `u16 → u32` for exactly this
/// silent-truncation class, so cute-dbt asserts the cast rather than
/// trusting it. `u32::MAX` is the fail-closed fallback in release (a 4 GB
/// model — or a 4-billion-line one — is not real); `debug_assert!` trips in
/// tests. ALL THREE axes (byte, line, col) route through this one helper so
/// the narrowing is uniform + debug-asserted (no bare saturating
/// `unwrap_or(u32::MAX)` left on the line/col axes).
fn clamp_u32<T: TryInto<u32>>(value: T, axis: &'static str) -> u32 {
    value.try_into().unwrap_or_else(|_| {
        debug_assert!(false, "span {axis} exceeds u32::MAX");
        u32::MAX
    })
}

/// Build a [`SourcePos`] from a sqlparser [`Location`] (1-based line/col)
/// and an already-computed 0-based byte offset. Used for endpoints whose
/// line/col the parser already reports.
fn loc_to_pos(loc: Location, byte: usize) -> SourcePos {
    SourcePos {
        line: clamp_u32(loc.line, "line"),
        col: clamp_u32(loc.column, "col"),
        byte: clamp_u32(byte, "byte offset"),
    }
}

/// Maps `(line, column)` to byte offsets in a SQL source string.
///
/// `line` and `column` are 1-indexed per sqlparser's convention.
/// Columns are character indices (codepoints), not byte indices —
/// matters whenever a SQL comment carries non-ASCII text.
struct ByteIndex {
    /// `line_starts[i]` = byte offset of the start of line `i + 1`.
    line_starts: Vec<usize>,
}

impl ByteIndex {
    fn new(sql: &str) -> Self {
        let mut line_starts = Vec::with_capacity(sql.bytes().filter(|&b| b == b'\n').count() + 1);
        line_starts.push(0);
        for (i, byte) in sql.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts }
    }

    /// Byte offset of the character at `loc`. For end-exclusive spans
    /// (`Location(line, col + 1)` where `col` is the last character),
    /// this returns the byte immediately past the span — slice it as
    /// `sql[start..end]` directly.
    fn byte_of(&self, sql: &str, loc: Location) -> usize {
        if loc.line == 0 {
            return 0;
        }
        let line_idx = usize::try_from(loc.line - 1).unwrap_or(usize::MAX);
        let col_idx = usize::try_from(loc.column.saturating_sub(1)).unwrap_or(usize::MAX);
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or(sql.len());
        if col_idx == 0 {
            return line_start;
        }
        let next_line_start = self
            .line_starts
            .get(line_idx + 1)
            .copied()
            .unwrap_or(sql.len().saturating_add(1));
        let line_terminator = next_line_start.saturating_sub(1).min(sql.len());
        let line_text = &sql[line_start..line_terminator];
        for (chars_passed, (byte_off, _ch)) in line_text.char_indices().enumerate() {
            if chars_passed == col_idx {
                return line_start + byte_off;
            }
        }
        line_start + line_text.len()
    }

    /// Inverse of [`Self::byte_of`]: the 1-based `(line, col)` and 0-based
    /// `byte` [`SourcePos`] for a byte offset. Used for span endpoints the
    /// parser does NOT report a `Location` for — the terminal node's
    /// post-trim start (Blocker-1) and its end-of-`sql` endpoint
    /// (cute-dbt#444). `col` is a 1-based unicode-char column, matching the
    /// sqlparser `Location` convention `byte_of` consumes. A `byte` past
    /// end-of-`sql` (only the saturating fallbacks reach here) clamps to the
    /// last line; `byte` is recorded verbatim (guarded `usize → u32`).
    fn pos_at(&self, sql: &str, byte: usize) -> SourcePos {
        // `line_starts` is sorted ascending; the owning line is the last
        // start `<= byte`. `partition_point` gives the count of starts
        // `<= byte`; that count is the 1-based line number.
        let clamped = byte.min(sql.len());
        let line_no = self.line_starts.partition_point(|&s| s <= clamped).max(1);
        let line_start = self.line_starts[line_no - 1];
        // 1-based char column within the line.
        let col = sql[line_start..clamped].chars().count() + 1;
        SourcePos {
            line: clamp_u32(line_no, "line"),
            col: clamp_u32(col, "col"),
            byte: clamp_u32(byte, "byte offset"),
        }
    }
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
        let cte_sql = g.nodes()[0].raw_sql().expect("CTE carries SQL");
        // Wrapper-plus-body slice (cute-dbt#31): preserves the `name AS
        // (...)` extent so the compiled-SQL drawer can show the CTE as
        // the user authored it.
        assert!(cte_sql.contains("a AS ("), "CTE slice starts at alias");
        assert!(cte_sql.contains("SELECT 1 AS id"), "CTE slice covers body");
        assert!(cte_sql.trim_end().ends_with(')'), "CTE slice ends at )");
        assert!(
            g.nodes()[1].raw_sql().is_some(),
            "the terminal node carries the final SELECT",
        );
    }

    #[test]
    fn cte_slice_preserves_sql_comments() {
        // The point of the span-based slice (cute-dbt#31): SQL comments
        // authored in the compiled_code survive into raw_sql. dbt
        // preserves `--` and `/* */` in compiled_code; cute-dbt now
        // preserves them through to the compiled-SQL drawer.
        let sql = "with stg AS (\n    -- pulling from raw.users\n    /* note: id only */\n    select id from raw.users\n)\n-- final pass\nselect * from stg";
        let g = parse_cte_graph(sql).unwrap();
        let cte_body = g.nodes()[0].raw_sql().expect("CTE body sliced");
        assert!(
            cte_body.contains("-- pulling from raw.users"),
            "line comment preserved in CTE body, got:\n{cte_body}"
        );
        assert!(
            cte_body.contains("/* note: id only */"),
            "block comment preserved in CTE body, got:\n{cte_body}"
        );
        let terminal = g.nodes()[1].raw_sql().expect("terminal sliced");
        assert!(
            terminal.contains("-- final pass"),
            "comment between WITH-clause and final SELECT preserved, got:\n{terminal}"
        );
    }

    // ── cute-dbt#444: per-CTE SourceSpan retention ──────────────────────

    /// TDD 1 — slice fidelity: `compiled[node.source_span().byte_range()]`
    /// byte-equals the node's `raw_sql()` for EVERY node, pinned against the
    /// comment-preserving fixture (`cte_slice_preserves_sql_comments`).
    #[test]
    fn retained_span_slices_byte_equal_raw_sql() {
        let sql = "with stg AS (\n    -- pulling from raw.users\n    /* note: id only */\n    select id from raw.users\n)\n-- final pass\nselect * from stg";
        let g = parse_cte_graph(sql).unwrap();
        for node in g.nodes() {
            let span = node
                .source_span()
                .unwrap_or_else(|| panic!("node `{}` retains a span", node.name()));
            let raw = node.raw_sql().expect("node carries raw_sql");
            assert_eq!(
                &sql[span.byte_range()],
                raw,
                "compiled[span] byte-equals raw_sql for node `{}`",
                node.name()
            );
        }
    }

    /// TDD 1 (multi-CTE) — the slice-fidelity invariant holds across CTE
    /// bodies AND the terminal, with non-ASCII text in comments (the
    /// char-vs-byte column path).
    #[test]
    fn retained_span_slices_byte_equal_with_unicode() {
        let sql = "with a AS (\n  -- café ☕\n  select 1 AS id\n), b AS (select 2 AS id)\nselect * from a join b on a.id = b.id";
        let g = parse_cte_graph(sql).unwrap();
        assert_eq!(g.nodes().len(), 3, "two CTEs + terminal");
        for node in g.nodes() {
            let span = node.source_span().expect("span retained");
            assert_eq!(
                &sql[span.byte_range()],
                node.raw_sql().unwrap(),
                "node `{}` slice byte-equals raw_sql",
                node.name()
            );
        }
    }

    /// TDD 2 (Blocker-1) — the TERMINAL node's `start.byte` is the POST-trim
    /// offset: `compiled[span]` byte-equals the trimmed terminal text, NOT
    /// the raw closing-paren-end (which would include the `\n-- final pass\n`
    /// leading glue). The slice must START at the trimmed text's first byte.
    #[test]
    fn terminal_span_start_is_post_trim() {
        let sql =
            "with stg AS (\n    select id from raw.users\n)\n-- final pass\nselect * from stg";
        let g = parse_cte_graph(sql).unwrap();
        let terminal = &g.nodes()[1];
        let span = terminal.source_span().expect("terminal span retained");
        let raw = terminal.raw_sql().unwrap();
        // The retained text equals the trimmed terminal (begins at the
        // first non-trim char — here the `-- final pass` comment).
        assert_eq!(&sql[span.byte_range()], raw);
        assert!(
            raw.starts_with("-- final pass"),
            "terminal begins at the post-trim first byte, got:\n{raw}"
        );
        // The raw closing-paren-end byte (before trimming) points AT a
        // newline; the post-trim start must be strictly greater.
        let close_end = sql.find(")\n").unwrap() + 1; // byte just past `)`
        assert!(
            (span.start.byte as usize) > close_end,
            "post-trim start ({}) advanced past the raw close-end ({close_end})",
            span.start.byte
        );
        assert_eq!(
            sql.as_bytes()[close_end],
            b'\n',
            "the raw close-end sits on the leading-trim newline"
        );
        // Pin a `pos_at`-derived `col` to its EXACT 1-based char column so the
        // `let col = …chars().count() + 1` step is mutation-covered (a
        // `+1 → *1` off-by-one — 1-based → 0-based — survives otherwise; no
        // other test fixes a `pos_at` col ≥ 2). The terminal `end` reaches
        // end-of-`sql`, which here is the last byte of `select * from stg`
        // (17 chars, no trailing newline) on line 5 — so `end.col` is the
        // post-the-last-char column 18 (`17 + 1`), which `17 * 1 = 17` cannot
        // reproduce.
        assert_eq!(span.end.line, 5, "terminal end is on the last line (5)");
        assert_eq!(
            span.end.col, 18,
            "terminal end.col is the 1-based char column past `select * from stg` (17 chars + 1)"
        );
    }

    /// TDD 3 — the terminal node's `end` endpoint reaches end-of-`sql`; its
    /// `end.line` is the last line, derived by `ByteIndex::pos_at`.
    #[test]
    fn terminal_span_end_reaches_eof() {
        let sql = "with stg AS (\n  select 1 AS id\n)\nselect * from stg\n";
        let g = parse_cte_graph(sql).unwrap();
        let span = g.nodes()[1].source_span().expect("terminal span");
        assert_eq!(
            span.end.byte as usize,
            sql.len(),
            "terminal end is end-of-sql"
        );
        let last_line = u32::try_from(sql.matches('\n').count() + 1).unwrap();
        assert_eq!(
            span.end.line, last_line,
            "terminal end_line is the last line"
        );
    }

    /// TDD 1/3 — line/col endpoints agree with the byte endpoints: slicing
    /// from `pos_at(start.byte)` reproduces the same text and the recorded
    /// line/col round-trip through `byte_of`-style reconstruction.
    #[test]
    fn span_line_col_agree_with_bytes() {
        let sql = "with a AS (\n  select 1 AS id\n)\nselect * from a";
        let g = parse_cte_graph(sql).unwrap();
        let cte = &g.nodes()[0];
        let span = cte.source_span().unwrap();
        // start.line is 1 (the `with a AS (` is on line 1).
        assert_eq!(span.start.line, 1, "CTE body span starts on line 1");
        assert_eq!(span.start.col, 6, "CTE body span starts at the alias `a`");
        assert!(span.start.byte < span.end.byte, "half-open, non-empty");

        // Cross-check `start.col` against the BYTES under the §3.1 invariant
        // ("line/col endpoints agree with byte endpoints"). This endpoint's
        // col comes from the sqlparser-`Location` path (`loc_to_pos`); the
        // terminal-span test pins the `pos_at` path. Computing the col from
        // bytes independently of either source pins BOTH against the one
        // invariant. The line-1 byte offset of the line start is 0.
        let line_starts: Vec<usize> = std::iter::once(0)
            .chain(sql.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let start_line_start = line_starts[(span.start.line - 1) as usize];
        let col_from_bytes = sql[start_line_start..span.start.byte as usize]
            .chars()
            .count()
            + 1;
        assert_eq!(
            span.start.col as usize, col_from_bytes,
            "start.col agrees with the byte-derived char column"
        );
    }

    /// TDD 4 — fallback degrade: a node whose span the engine cannot soundly
    /// locate carries `None`, never a fabricated/wrong span. A no-`WITH`
    /// query produces no CTE nodes, so we exercise the empty-graph path and
    /// the `slice_or_fallback` empty-span branch directly.
    #[test]
    fn empty_or_oob_span_degrades_to_none() {
        // Empty span (line 0) ⇒ fallback text + None.
        let sql = "select 1";
        let idx = ByteIndex::new(sql);
        let empty = Span {
            start: Location { line: 0, column: 0 },
            end: Location { line: 0, column: 0 },
        };
        let (text, span) = slice_or_fallback(sql, &idx, empty, || "FALLBACK".to_owned());
        assert_eq!(text, "FALLBACK", "empty span yields the fallback text");
        assert!(span.is_none(), "degraded span is None, never a lie");

        // Inverted span (start strictly after end) ⇒ the `start > end`
        // guard fires the fallback + None.
        let inverted = Span {
            start: Location { line: 1, column: 8 },
            end: Location { line: 1, column: 2 },
        };
        let (text, span) = slice_or_fallback(sql, &idx, inverted, || "FB2".to_owned());
        assert_eq!(text, "FB2", "inverted span yields the fallback text");
        assert!(span.is_none(), "inverted span degrades to None");
    }

    /// TDD 7 — the `→ u32` guard caps at `u32::MAX` (the tested behavior). In
    /// a debug build `clamp_u32` would `debug_assert!`, so this exercises the
    /// in-range pass-through; the cap path is verified by construction (the
    /// `unwrap_or_else` returns `u32::MAX`). All three axes share the helper,
    /// so the byte axis exercises it for line/col too.
    #[test]
    fn clamp_u32_guard_passes_in_range() {
        assert_eq!(clamp_u32(0_usize, "byte offset"), 0);
        assert_eq!(clamp_u32(123_456_usize, "byte offset"), 123_456);
        assert_eq!(clamp_u32(u32::MAX as usize, "byte offset"), u32::MAX);
        // The line/col axes route through the same helper (u64 inputs from
        // the sqlparser `Location`).
        assert_eq!(clamp_u32(7_u64, "line"), 7);
        assert_eq!(clamp_u32(u64::from(u32::MAX), "col"), u32::MAX);
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

    // ===== shape facts (cute-dbt#40, Option C) =====

    #[test]
    fn import_cte_body_is_classified_simple_from_shape() {
        // Plain `SELECT * FROM <single relation>` — the import-CTE shape.
        let g = graph("WITH src AS (SELECT * FROM raw.users) SELECT * FROM src");
        let src = &g.nodes()[0];
        assert_eq!(src.name(), "src");
        assert!(
            src.is_simple_from_shape(),
            "single-source SELECT FROM is the import-CTE shape",
        );
        assert_eq!(
            src.body_leaf_table_refs(),
            &["users".to_owned()],
            "leaf table identifier extracted from schema-qualified ref",
        );
    }

    #[test]
    fn comma_cross_join_body_is_not_simple_from_shape() {
        // cute-dbt#40: the bug the AST refactor fixes. The whitespace
        // heuristic counted one `from` keyword and called this "simple";
        // the AST correctly identifies two source relations.
        let g = graph("WITH a AS (SELECT * FROM x, y) SELECT * FROM a");
        let a = &g.nodes()[0];
        assert_eq!(a.name(), "a");
        assert!(
            !a.is_simple_from_shape(),
            "comma cross-join is two sources — NOT the import-CTE shape",
        );
        let refs = a.body_leaf_table_refs();
        assert!(
            refs.iter().any(|t| t == "x"),
            "leaf x extracted, got {refs:?}"
        );
        assert!(
            refs.iter().any(|t| t == "y"),
            "leaf y extracted, got {refs:?}"
        );
    }

    #[test]
    fn join_body_is_not_simple_from_shape_and_carries_both_leaves() {
        // A JOIN body is structurally transform-shaped; both joined
        // leaves appear in body_leaf_table_refs.
        let g = graph(
            "WITH j AS (SELECT * FROM a JOIN b ON a.k = b.k) \
             SELECT * FROM j",
        );
        let j = &g.nodes()[0];
        assert_eq!(j.name(), "j");
        assert!(!j.is_simple_from_shape(), "JOIN body is transform-shaped");
        let refs = j.body_leaf_table_refs();
        assert!(
            refs.iter().any(|t| t == "a"),
            "leaf a extracted, got {refs:?}"
        );
        assert!(
            refs.iter().any(|t| t == "b"),
            "leaf b extracted, got {refs:?}"
        );
    }

    #[test]
    fn schema_qualified_refs_yield_lowercased_leaf_only() {
        // dbt's compiled SQL commonly produces `"db"."schema"."MODEL"`;
        // the engine extracts the leaf identifier and lowercases.
        let g = graph(
            "WITH src AS (SELECT * FROM \"db\".\"main\".\"RAW_CUSTOMERS\") \
             SELECT * FROM src",
        );
        let src = &g.nodes()[0];
        assert!(src.is_simple_from_shape());
        assert_eq!(src.body_leaf_table_refs(), &["raw_customers".to_owned()]);
    }

    #[test]
    fn terminal_node_carries_its_own_shape_facts() {
        // The terminal node's body is itself walked for shape facts —
        // useful for non-CTE models (empty graph), where the terminal
        // is the only node the renderer has to reason about.
        let g = graph("WITH src AS (SELECT * FROM raw.t) SELECT * FROM src");
        let terminal = &g.nodes()[1];
        assert_eq!(terminal.name(), TERMINAL_NODE_NAME);
        assert!(
            terminal.is_simple_from_shape(),
            "terminal `SELECT * FROM src` is single-source"
        );
        assert_eq!(terminal.body_leaf_table_refs(), &["src".to_owned()]);
    }

    #[test]
    fn select_only_body_is_not_simple_from_shape() {
        // `SELECT 1` (no FROM clause) — not the import-CTE shape; no
        // table refs to extract.
        let g = graph("WITH lit AS (SELECT 1 AS id) SELECT * FROM lit");
        let lit = &g.nodes()[0];
        assert!(
            !lit.is_simple_from_shape(),
            "SELECT without FROM is not single-source-FROM",
        );
        assert!(lit.body_leaf_table_refs().is_empty());
    }

    // ===== per-LEFT-JOIN facts (cute-dbt#173, catalog class C4) =====

    /// The graph-level left-join facts for a single-statement query.
    fn terminal_facts(sql: &str) -> Vec<crate::domain::LeftJoinFact> {
        graph(sql).left_join_facts().to_vec()
    }

    #[test]
    fn left_join_with_aliases_yields_a_fact_with_equi_keys() {
        let facts = terminal_facts(
            "SELECT o.order_id, c.email \
             FROM stg_orders o LEFT JOIN stg_customers c ON o.customer_id = c.id",
        );
        assert_eq!(facts.len(), 1, "one LEFT JOIN, one fact");
        let fact = &facts[0];
        assert_eq!(
            fact.consumer(),
            TERMINAL_NODE_NAME,
            "a WITH-less body's facts are tagged with the terminal name"
        );
        assert_eq!(fact.right_leaf(), "stg_customers");
        assert_eq!(fact.equi_keys().len(), 1);
        assert_eq!(fact.equi_keys()[0].left_leaf(), Some("stg_orders"));
        assert_eq!(fact.equi_keys()[0].left_column(), "customer_id");
        assert_eq!(fact.equi_keys()[0].right_column(), "id");
        assert!(fact.where_is_null_columns().is_empty());
        assert!(
            fact.projects_right_columns(),
            "c.email is a direct right-qualified projection item"
        );
        assert!(!fact.select_is_distinct());
    }

    #[test]
    fn left_outer_join_without_aliases_resolves_by_table_leaf() {
        // dbt's compiled SQL often schema-qualifies: the qualifier in ON
        // / projection is the table leaf when no alias is given.
        let facts = terminal_facts(
            "SELECT stg_orders.order_id, stg_customers.email \
             FROM \"db\".\"main\".\"stg_orders\" \
             LEFT OUTER JOIN \"db\".\"main\".\"stg_customers\" \
             ON stg_orders.customer_id = stg_customers.id",
        );
        assert_eq!(facts.len(), 1);
        let fact = &facts[0];
        assert_eq!(fact.right_leaf(), "stg_customers");
        assert_eq!(fact.equi_keys()[0].left_leaf(), Some("stg_orders"));
        assert!(fact.projects_right_columns());
    }

    #[test]
    fn where_right_key_is_null_is_captured_as_the_anti_join_fact() {
        let facts = terminal_facts(
            "SELECT * FROM stg_orders o \
             LEFT JOIN stg_returns r ON o.order_id = r.order_id \
             WHERE r.order_id IS NULL AND o.status = 'shipped'",
        );
        assert_eq!(facts.len(), 1);
        let fact = &facts[0];
        assert_eq!(fact.where_is_null_columns(), &["order_id".to_owned()]);
        assert!(
            fact.projects_right_columns(),
            "bare * provably includes right columns"
        );
    }

    #[test]
    fn is_null_inside_an_or_is_not_an_anti_join_conjunct() {
        // `WHERE r.k IS NULL OR …` does not exclude the matched class —
        // different semantics, never recorded.
        let facts = terminal_facts(
            "SELECT * FROM stg_orders o \
             LEFT JOIN stg_returns r ON o.order_id = r.order_id \
             WHERE r.order_id IS NULL OR o.status = 'shipped'",
        );
        assert!(facts[0].where_is_null_columns().is_empty());
    }

    #[test]
    fn unqualified_is_null_is_not_attributed_to_the_join() {
        let facts = terminal_facts(
            "SELECT * FROM stg_orders o \
             LEFT JOIN stg_returns r ON o.order_id = r.order_id \
             WHERE order_id IS NULL",
        );
        assert!(
            facts[0].where_is_null_columns().is_empty(),
            "an unqualified column cannot be attributed to the joined relation"
        );
    }

    #[test]
    fn projection_attribution_is_conservative() {
        // Right columns reaching the output only through expressions
        // (COALESCE) deliberately do not count; a right-qualified
        // wildcard does; a wildcard qualified by ANOTHER relation does
        // not.
        let coalesce_only = terminal_facts(
            "SELECT o.order_id, COALESCE(c.email, 'none') AS email \
             FROM stg_orders o LEFT JOIN stg_customers c ON o.customer_id = c.id",
        );
        assert!(
            !coalesce_only[0].projects_right_columns(),
            "expression-wrapped right columns are not provably attributed"
        );
        let right_wildcard = terminal_facts(
            "SELECT c.* FROM stg_orders o \
             LEFT JOIN stg_customers c ON o.customer_id = c.id",
        );
        assert!(right_wildcard[0].projects_right_columns());
        let other_wildcard = terminal_facts(
            "SELECT o.* FROM stg_orders o \
             LEFT JOIN stg_customers c ON o.customer_id = c.id",
        );
        assert!(!other_wildcard[0].projects_right_columns());
    }

    #[test]
    fn non_equi_and_using_constraints_yield_no_key_pairs() {
        let non_equi = terminal_facts(
            "SELECT * FROM stg_orders o \
             LEFT JOIN stg_rates r ON o.order_date >= r.valid_from",
        );
        assert!(non_equi[0].equi_keys().is_empty());
        let expr_key = terminal_facts(
            "SELECT * FROM stg_orders o \
             LEFT JOIN stg_customers c ON upper(o.customer_id) = c.id",
        );
        assert!(expr_key[0].equi_keys().is_empty());
        let using =
            terminal_facts("SELECT * FROM stg_orders LEFT JOIN stg_customers USING(customer_id)");
        assert!(using[0].equi_keys().is_empty());
    }

    #[test]
    fn compound_on_clause_yields_one_pair_per_equi_conjunct() {
        let facts = terminal_facts(
            "SELECT * FROM stg_orders o \
             LEFT JOIN stg_rates r \
             ON o.currency = r.currency AND o.order_date = r.rate_date",
        );
        let pairs = facts[0].equi_keys();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].left_column(), "currency");
        assert_eq!(pairs[0].right_column(), "currency");
        assert_eq!(pairs[1].left_column(), "order_date");
        assert_eq!(pairs[1].right_column(), "rate_date");
    }

    #[test]
    fn derived_table_right_side_emits_no_fact() {
        let facts = terminal_facts(
            "SELECT * FROM stg_orders o \
             LEFT JOIN (SELECT id FROM stg_customers) c ON o.customer_id = c.id",
        );
        assert!(
            facts.is_empty(),
            "a derived-table right side is a declared exclusion: {facts:?}"
        );
    }

    #[test]
    fn non_left_joins_emit_no_left_join_facts() {
        let facts = terminal_facts(
            "SELECT * FROM stg_orders o \
             INNER JOIN stg_payments p ON o.order_id = p.order_id \
             RIGHT JOIN stg_customers c ON o.customer_id = c.id",
        );
        assert!(facts.is_empty());
    }

    #[test]
    fn not_exists_anti_join_emits_no_left_join_fact() {
        // Family separation: a NOT EXISTS anti-join is never a
        // LeftJoinFact — since cute-dbt#196 it surfaces through the
        // SIBLING SubqueryFact family (tested below), not by
        // normalizing subqueries into join facts (the rejected
        // alternative: it would lie in the POD).
        let facts = terminal_facts(
            "SELECT * FROM stg_orders o \
             WHERE NOT EXISTS (SELECT 1 FROM stg_returns r WHERE r.order_id = o.order_id)",
        );
        assert!(facts.is_empty());
    }

    #[test]
    fn select_distinct_sets_the_dedup_fact() {
        let facts = terminal_facts(
            "SELECT DISTINCT o.order_id, c.segment \
             FROM stg_orders o LEFT JOIN stg_customers c ON o.customer_id = c.id",
        );
        assert!(facts[0].select_is_distinct());
    }

    #[test]
    fn left_joins_in_cte_bodies_and_union_arms_are_collected() {
        let g = graph(
            "WITH joined AS (\
                 SELECT a.id, b.v FROM ta a LEFT JOIN tb b ON a.id = b.id\
             ) \
             SELECT * FROM joined \
             UNION ALL \
             SELECT c.id, d.v FROM tc c LEFT JOIN td d ON c.id = d.id",
        );
        let facts = g.left_join_facts();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].consumer(), "joined");
        assert_eq!(facts[0].right_leaf(), "tb");
        assert_eq!(
            facts[1].consumer(),
            TERMINAL_NODE_NAME,
            "the union arm's LEFT JOIN is collected under the terminal body"
        );
        assert_eq!(facts[1].right_leaf(), "td");
    }

    #[test]
    fn two_left_joins_in_one_select_yield_two_facts_in_source_order() {
        let facts = terminal_facts(
            "SELECT p.patient_id, cs.first_date, cf.flag \
             FROM patients p \
             LEFT JOIN condition_stats cs ON p.patient_id = cs.patient_id \
             LEFT JOIN chronic_flags cf ON p.patient_id = cf.patient_id",
        );
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].right_leaf(), "condition_stats");
        assert_eq!(facts[1].right_leaf(), "chronic_flags");
    }

    // ===== per-negated-subquery facts (cute-dbt#196) =====

    /// The graph-level subquery facts for a single-statement query.
    fn subquery_facts(sql: &str) -> Vec<crate::domain::SubqueryFact> {
        graph(sql).subquery_facts().to_vec()
    }

    #[test]
    fn correlated_not_exists_yields_a_fact_with_one_equi_key() {
        let facts = subquery_facts(
            "SELECT * FROM stg_orders o \
             WHERE NOT EXISTS (SELECT 1 FROM stg_returns r WHERE r.order_id = o.order_id)",
        );
        assert_eq!(facts.len(), 1, "one NOT EXISTS, one fact");
        let fact = &facts[0];
        assert_eq!(fact.kind(), SubqueryKind::NotExists);
        assert_eq!(
            fact.consumer(),
            TERMINAL_NODE_NAME,
            "a WITH-less body's facts are tagged with the terminal name"
        );
        assert_eq!(fact.inner_leaf(), "stg_returns");
        assert_eq!(fact.equi_keys().len(), 1);
        assert_eq!(
            fact.equi_keys()[0].left_leaf(),
            Some("stg_orders"),
            "the OUTER side is the pair's left"
        );
        assert_eq!(fact.equi_keys()[0].left_column(), "order_id");
        assert_eq!(fact.equi_keys()[0].right_column(), "order_id");
    }

    #[test]
    fn multi_key_correlation_yields_one_pair_per_equi_conjunct() {
        let facts = subquery_facts(
            "SELECT * FROM stg_rates o \
             WHERE NOT EXISTS (SELECT 1 FROM stg_quotes q \
             WHERE q.currency = o.currency AND o.quote_date = q.rate_date)",
        );
        assert_eq!(facts.len(), 1);
        let pairs = facts[0].equi_keys();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].left_column(), "currency");
        assert_eq!(pairs[0].right_column(), "currency");
        assert_eq!(
            pairs[1].left_column(),
            "quote_date",
            "the outer side is normalized left regardless of source order"
        );
        assert_eq!(pairs[1].right_column(), "rate_date");
    }

    #[test]
    fn not_exists_alias_resolution_follows_both_alias_maps() {
        // dbt-style schema-qualified names with aliases on BOTH sides:
        // the outer qualifier resolves through the outer alias map, the
        // inner through the inner one.
        let facts = subquery_facts(
            "SELECT c.customer_id FROM \"db\".\"main\".\"stg_customers\" c \
             WHERE NOT EXISTS (SELECT 1 FROM \"db\".\"main\".\"stg_orders\" o \
             WHERE o.customer_id = c.customer_id)",
        );
        assert_eq!(facts.len(), 1);
        let fact = &facts[0];
        assert_eq!(fact.inner_leaf(), "stg_orders");
        assert_eq!(fact.equi_keys()[0].left_leaf(), Some("stg_customers"));
    }

    #[test]
    fn correlated_but_non_equi_not_exists_yields_a_fact_with_empty_keys() {
        // Correlation evidence (o.signup_date is outer-resolvable) but
        // no resolvable equi conjunct — the unrecoverable-key shape:
        // fact emitted, EMPTY keys, downstream bind degrades UNKNOWN.
        let facts = subquery_facts(
            "SELECT * FROM stg_customers o \
             WHERE NOT EXISTS (SELECT 1 FROM stg_orders r \
             WHERE r.order_date > o.signup_date)",
        );
        assert_eq!(facts.len(), 1);
        assert!(facts[0].equi_keys().is_empty());
    }

    #[test]
    fn uncorrelated_not_exists_emits_no_fact() {
        // Zero outer references — not a keyed anti-join: silence.
        let facts = subquery_facts(
            "SELECT * FROM stg_customers c \
             WHERE NOT EXISTS (SELECT 1 FROM stg_orders o WHERE o.status = 'void')",
        );
        assert!(facts.is_empty(), "{facts:?}");
    }

    #[test]
    fn or_branch_not_exists_emits_no_fact() {
        // An OR conjunct has different semantics and is never
        // decomposed — the negated subquery inside it is invisible.
        let facts = subquery_facts(
            "SELECT * FROM stg_customers c \
             WHERE c.is_active = true OR NOT EXISTS \
             (SELECT 1 FROM stg_orders o WHERE o.customer_id = c.customer_id)",
        );
        assert!(facts.is_empty(), "{facts:?}");
    }

    #[test]
    fn non_negated_exists_and_in_emit_no_facts() {
        // Semi-join + membership are FUTURE consumers — extracting them
        // now would be dead variants; pinned silent.
        let exists = subquery_facts(
            "SELECT * FROM stg_customers c \
             WHERE EXISTS (SELECT 1 FROM stg_orders o WHERE o.customer_id = c.customer_id)",
        );
        assert!(exists.is_empty(), "{exists:?}");
        let inn = subquery_facts(
            "SELECT * FROM stg_orders o \
             WHERE o.order_id IN (SELECT r.order_id FROM stg_refunds r)",
        );
        assert!(inn.is_empty(), "{inn:?}");
    }

    #[test]
    fn derived_table_inner_emits_no_fact() {
        let facts = subquery_facts(
            "SELECT * FROM stg_customers c \
             WHERE NOT EXISTS (SELECT 1 FROM (SELECT * FROM stg_orders) o \
             WHERE o.customer_id = c.customer_id)",
        );
        assert!(facts.is_empty(), "{facts:?}");
    }

    #[test]
    fn multi_table_inner_emits_no_fact() {
        let joined = subquery_facts(
            "SELECT * FROM stg_customers c \
             WHERE NOT EXISTS (SELECT 1 FROM stg_orders o \
             JOIN stg_payments p ON o.order_id = p.order_id \
             WHERE o.customer_id = c.customer_id)",
        );
        assert!(joined.is_empty(), "{joined:?}");
        let comma = subquery_facts(
            "SELECT * FROM stg_customers c \
             WHERE NOT EXISTS (SELECT 1 FROM stg_orders o, stg_payments p \
             WHERE o.customer_id = c.customer_id)",
        );
        assert!(comma.is_empty(), "{comma:?}");
    }

    #[test]
    fn not_in_happy_path_yields_the_membership_pair() {
        let facts = subquery_facts(
            "SELECT o.order_id FROM stg_orders o \
             WHERE o.order_id NOT IN (SELECT r.order_id FROM stg_refunds r)",
        );
        assert_eq!(facts.len(), 1);
        let fact = &facts[0];
        assert_eq!(fact.kind(), SubqueryKind::NotIn);
        assert_eq!(fact.inner_leaf(), "stg_refunds");
        assert_eq!(fact.equi_keys().len(), 1);
        assert_eq!(fact.equi_keys()[0].left_leaf(), Some("stg_orders"));
        assert_eq!(fact.equi_keys()[0].left_column(), "order_id");
        assert_eq!(fact.equi_keys()[0].right_column(), "order_id");
    }

    #[test]
    fn not_in_unqualified_outer_column_resolves_over_a_sole_relation() {
        // One outer relation: the unqualified membership column is
        // unambiguous. An ALIASED single projection keeps the SOURCE
        // column (the given mocks the relation's raw columns).
        let facts = subquery_facts(
            "SELECT order_id FROM stg_orders \
             WHERE order_id NOT IN (SELECT order_id AS refunded_id FROM stg_refunds)",
        );
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].equi_keys().len(), 1);
        assert_eq!(facts[0].equi_keys()[0].left_leaf(), Some("stg_orders"));
        assert_eq!(facts[0].equi_keys()[0].right_column(), "order_id");
    }

    #[test]
    fn not_in_unresolvable_outer_column_yields_empty_keys() {
        // Two outer relations + an unqualified column: the outer side
        // is present but ambiguous — fact with EMPTY keys (→ UNKNOWN).
        let facts = subquery_facts(
            "SELECT * FROM stg_orders o JOIN stg_shipments s ON o.order_id = s.order_id \
             WHERE order_id NOT IN (SELECT r.order_id FROM stg_refunds r)",
        );
        assert_eq!(facts.len(), 1);
        assert!(facts[0].equi_keys().is_empty());
    }

    #[test]
    fn not_in_multi_column_or_expression_projection_emits_no_fact() {
        let two_columns = subquery_facts(
            "SELECT * FROM stg_orders o \
             WHERE o.order_id NOT IN (SELECT r.order_id, r.refund_id FROM stg_refunds r)",
        );
        assert!(two_columns.is_empty(), "{two_columns:?}");
        let expression = subquery_facts(
            "SELECT * FROM stg_orders o \
             WHERE o.order_id NOT IN (SELECT abs(r.order_id) FROM stg_refunds r)",
        );
        assert!(expression.is_empty(), "{expression:?}");
        let non_column_outer = subquery_facts(
            "SELECT * FROM stg_orders o \
             WHERE upper(o.status) NOT IN (SELECT r.status FROM stg_refunds r)",
        );
        assert!(non_column_outer.is_empty(), "{non_column_outer:?}");
    }

    #[test]
    fn subquery_facts_in_cte_bodies_carry_their_consumer_name() {
        let g = graph(
            "WITH unrefunded AS (\
                 SELECT o.order_id FROM stg_orders o \
                 WHERE NOT EXISTS (SELECT 1 FROM stg_refunds r WHERE r.order_id = o.order_id)\
             ) \
             SELECT * FROM unrefunded \
             WHERE order_id NOT IN (SELECT v.order_id FROM stg_voids v)",
        );
        let facts = g.subquery_facts();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].kind(), SubqueryKind::NotExists);
        assert_eq!(facts[0].consumer(), "unrefunded");
        assert_eq!(facts[1].kind(), SubqueryKind::NotIn);
        assert_eq!(
            facts[1].consumer(),
            TERMINAL_NODE_NAME,
            "the terminal body's NOT IN is collected under the terminal name"
        );
        assert_eq!(
            facts[1].equi_keys()[0].left_leaf(),
            Some("unrefunded"),
            "the sole outer relation may itself be a CTE — the detector \
             binds it through the simple-FROM closure"
        );
    }
}
