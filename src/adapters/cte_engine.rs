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
    ColumnEdge, ColumnEdgeConfidence, ColumnEdgeKind, ColumnRef, ColumnSpan, CteEdge, CteGraph,
    CteNode, EdgeType, JoinKeyPair, LeftJoinFact, SourcePos, SourceSpan, SubqueryFact,
    SubqueryKind,
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
    // CLL-2 (cute-dbt#447): the projection-provenance pass rides this SAME
    // single parse — never a second one. It walks every body's projection,
    // emits intra-model column edges (pass-through / rename) + per-column
    // compiled spans, and the domain `SourceMap` folds the spans into
    // `SpanRole::Column` entries. Both arms (WITH-bearing + WITH-less) run it.
    let lineage = collect_column_lineage(compiled_sql, ctes, query);
    if ctes.is_empty() {
        // No CTE structure to visualise — but the body's LEFT JOIN
        // facts still surface (cute-dbt#173): the catalog C4 canonical
        // shape is a WITH-less `… FROM a LEFT JOIN b …` model. The
        // cute-dbt#196 subquery facts ride the same path (the WITH-less
        // `… WHERE NOT EXISTS (…)` anti-join is just as canonical).
        let facts = compute_shape_facts(TERMINAL_NODE_NAME, &query.body);
        return CteGraph::default()
            .with_left_join_facts(facts.left_joins)
            .with_subquery_facts(facts.subqueries)
            .with_column_edges(lineage.edges)
            .with_column_spans(lineage.spans);
    }
    let recursive = query.with.as_ref().is_some_and(|with| with.recursive);
    let (nodes, left_joins, subqueries) = build_nodes(compiled_sql, ctes, query);
    let index = name_index(ctes);
    let edges = build_edges(ctes, query, &index);
    let graph = CteGraph::new(nodes, edges)
        .with_left_join_facts(left_joins)
        .with_subquery_facts(subqueries)
        .with_column_edges(lineage.edges)
        .with_column_spans(lineage.spans);
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

// ---------------------------------------------------------------------
// Intra-model column lineage — the projection-provenance pass
// (cute-dbt#447, CLL-2).
//
// Tier-1 MVP = PASS-THROUGH (`c.email AS email`) + RENAME
// (`c.email AS contact_email`) ONLY — the cases the existing projection
// walker reaches (`Identifier` / aliased `CompoundIdentifier`). Expression
// provenance (`coalesce(a.x, b.y) AS z`) is CLL-3: those items emit NO
// `Derived` edge here (honest absence — never a fabricated edge).
//
// Confidence tracks SQL EXPLICITNESS (never-a-false-claim):
//   - Resolved  — qualified ref, or single-source unqualified (sole_relation_leaf).
//   - Ambiguous — unqualified column under a multi-relation FROM → FAN OUT
//                 to every candidate source (never dropped).
//   - Opaque    — `SELECT *` / `q.*` over an UNKNOWN external relation → a
//                 virtual `*→*` edge, badged. A star over a known intra-model
//                 CTE is NOT auto-Opaque: the design (column-lineage-feasibility
//                 §3 Tier-1) scopes intra-model star-EXPANSION into CLL-2 — the
//                 upstream CTE's output columns were resolved when the pass
//                 walked that CTE's own projection, so a downstream
//                 `select * from <known_cte>` EXPANDS into one Resolved
//                 PassThrough edge per upstream output column. Opaque is
//                 reserved STRICTLY for a star over an UNKNOWN external relation
//                 (an undocumented `source()` / a leaf the resolver cannot
//                 expand). Cross-model star expansion (over a `ref()` boundary)
//                 stays CLL-4.
// ---------------------------------------------------------------------

/// The output of the projection-provenance pass: the intra-model column
/// edges and the per-output-column compiled spans, both written back onto
/// the [`CteGraph`].
struct ColumnLineage {
    edges: Vec<ColumnEdge>,
    spans: Vec<ColumnSpan>,
}

/// A CTE's resolved output-column list (lowercased, in projection order) when
/// the body is FULLY enumerable, else `None`. A `None` body — one whose
/// projection carries an Opaque star or a positionally-anonymous expression —
/// cannot have its star expanded downstream (we would have to fabricate the
/// missing names), so a `select * from <that-cte>` honestly stays Opaque.
type KnownCteColumns = HashMap<String, Option<Vec<String>>>;

/// Walk every body's `select.projection` (each CTE + the terminal select),
/// emitting pass-through / rename [`ColumnEdge`]s and per-column
/// [`ColumnSpan`]s. Rides the single parse — never re-parses.
///
/// CTEs are walked in DECLARATION order so that by the time a body does
/// `select * from <upstream_cte>` the upstream CTE's resolved output columns
/// are already recorded in `known_cte_columns` — the design's intra-model
/// star-EXPANSION (CLL-2), keyed off the columns the pass already resolved.
fn collect_column_lineage(compiled_sql: &str, ctes: &[Cte], query: &Query) -> ColumnLineage {
    let index = ByteIndex::new(compiled_sql);
    let mut edges: Vec<ColumnEdge> = Vec::new();
    let mut spans: Vec<ColumnSpan> = Vec::new();
    let mut known_cte_columns: KnownCteColumns = HashMap::new();
    for cte in ctes {
        let name = cte_name(cte);
        let outputs = collect_body_column_lineage(
            name,
            &cte.query.body,
            compiled_sql,
            &index,
            &known_cte_columns,
            &mut edges,
            &mut spans,
        );
        known_cte_columns.insert(name.to_ascii_lowercase(), outputs);
    }
    collect_body_column_lineage(
        TERMINAL_NODE_NAME,
        &query.body,
        compiled_sql,
        &index,
        &known_cte_columns,
        &mut edges,
        &mut spans,
    );
    ColumnLineage { edges, spans }
}

/// Column lineage for ONE body. Only a top-level `SetExpr::Select` is
/// analysed — a `UNION`/`EXCEPT`/parenthesised query is not a single
/// projection list (UNION-by-position is a deferred refinement), so it
/// emits no edges (honest absence, never a fabricated claim).
///
/// Returns the body's resolved output-column descriptor (see
/// [`KnownCteColumns`]) so the caller can register it for downstream star
/// expansion. A non-`Select` body is non-enumerable (`None`).
fn collect_body_column_lineage(
    node_id: &str,
    body: &SetExpr,
    sql: &str,
    index: &ByteIndex,
    known_cte_columns: &KnownCteColumns,
    edges: &mut Vec<ColumnEdge>,
    spans: &mut Vec<ColumnSpan>,
) -> Option<Vec<String>> {
    let SetExpr::Select(select) = body else {
        return None;
    };
    let aliases = select_alias_map(select);
    let sole_leaf = sole_relation_leaf(select);
    // Accumulate the body's output columns; `enumerable` flips to false the
    // moment one item has no statically-knowable output name (an Opaque star
    // or an anonymous expression) — then the whole body is non-enumerable.
    let mut outputs: Vec<String> = Vec::new();
    let mut enumerable = true;
    for item in &select.projection {
        let item_outputs = resolve_projection_item(
            node_id,
            item,
            &aliases,
            sole_leaf.as_deref(),
            known_cte_columns,
            sql,
            index,
            edges,
            spans,
        );
        match item_outputs {
            Some(cols) => outputs.extend(cols),
            None => enumerable = false,
        }
    }
    enumerable.then_some(outputs)
}

/// Resolve ONE `SelectItem` into its output column(s) + provenance edge(s) +
/// span. Tier-1: pass-through / rename direct refs; a star over a KNOWN
/// intra-model CTE EXPANDS into one Resolved edge per upstream output column;
/// a star over an UNKNOWN external relation degrades honestly to `Opaque`;
/// expressions (functions/CASE/…) emit a span but NO edge (CLL-3).
///
/// Returns this item's contribution to the body's output-column list:
/// `Some(cols)` = the statically-knowable output name(s); `None` = the item
/// has no enumerable output name (an Opaque star or an anonymous expression),
/// which makes the owning body non-enumerable for downstream star expansion.
#[allow(clippy::too_many_arguments)]
fn resolve_projection_item(
    node_id: &str,
    item: &SelectItem,
    aliases: &HashMap<String, String>,
    sole_leaf: Option<&str>,
    known_cte_columns: &KnownCteColumns,
    sql: &str,
    index: &ByteIndex,
    edges: &mut Vec<ColumnEdge>,
    spans: &mut Vec<ColumnSpan>,
) -> Option<Vec<String>> {
    match item {
        // `expr AS alias` — the output name is the alias; the input is the
        // column inside `expr`. Pass-through when name==col, else rename.
        SelectItem::ExprWithAlias { expr, alias } => {
            let output = alias.value.to_ascii_lowercase();
            if let Some(input) = direct_column_ref(expr, aliases, sole_leaf) {
                let kind = if input.column == output {
                    ColumnEdgeKind::PassThrough
                } else {
                    ColumnEdgeKind::Renamed
                };
                push_edges(node_id, &output, input, kind, edges);
                push_span(node_id, &output, item, sql, index, spans);
            } else {
                // An expression (coalesce/CASE/func/arithmetic) — CLL-3
                // (cute-dbt#449): collect every input column it reads and emit
                // one `Derived` edge per input (many-to-one), depth-capped and
                // degrading to Ambiguous honestly. Never a fabricated edge:
                // an expression with no recoverable column ref emits none.
                let collected = collect_derived_refs(expr);
                for input in derived_input_cols(collected, aliases, sole_leaf) {
                    push_edges(node_id, &output, input, ColumnEdgeKind::Derived, edges);
                }
                push_span(node_id, &output, item, sql, index, spans);
            }
            // The output NAME is known regardless of whether an edge was
            // emitted — the alias is explicit in the SQL.
            Some(vec![output])
        }
        // A bare `col` or `q.col` projection — the output name is the column
        // itself; always a pass-through.
        SelectItem::UnnamedExpr(expr) => {
            if let Some(input) = direct_column_ref(expr, aliases, sole_leaf) {
                let output = input.column.clone();
                push_edges(node_id, &output, input, ColumnEdgeKind::PassThrough, edges);
                push_span(node_id, &output, item, sql, index, spans);
                Some(vec![output])
            } else {
                // A bare non-column expression with no alias has no stable
                // output name — no edge, no span, nothing to anchor — and it
                // makes the body non-enumerable (we cannot name this column).
                None
            }
        }
        // `SELECT *` — over a KNOWN intra-model CTE this EXPANDS into one
        // Resolved PassThrough edge per upstream output column (the design's
        // CLL-2 intra-model star expansion). Over an UNKNOWN external relation
        // (or a CTE whose own output is non-enumerable) it degrades honestly to
        // a single virtual `*→*` Opaque edge — never a fabricated column list.
        SelectItem::Wildcard(_) => resolve_star(
            node_id,
            sole_leaf,
            known_cte_columns,
            item,
            sql,
            index,
            edges,
            spans,
        ),
        // `q.*` — a qualified star over relation `q`. Same expansion/degrade
        // rule, sourced from `q`'s resolved leaf.
        SelectItem::QualifiedWildcard(kind, _) => {
            let leaf = qualified_wildcard_leaf(kind, aliases);
            resolve_star(
                node_id,
                leaf.as_deref(),
                known_cte_columns,
                item,
                sql,
                index,
                edges,
                spans,
            )
        }
        // Spark's `expr AS (a, b, …)` multi-alias form — not a direct column
        // reference; no Tier-1 edge (honest absence). No single stable output
        // name ⇒ the body is non-enumerable.
        SelectItem::ExprWithAliases { .. } => None,
    }
}

/// Resolve a star (`*` or `q.*`) sourced from `source_leaf`. When the leaf
/// names a KNOWN intra-model CTE with a fully-enumerable output-column list,
/// EXPAND: emit one `PassThrough`/`Resolved` edge per upstream column
/// (`<leaf>.<col> → <node_id>.<col>`) and return those columns. Otherwise
/// (unknown external relation, or a CTE whose own projection was
/// non-enumerable) degrade to a single virtual `*→*` Opaque edge and return
/// `None` — the never-a-false-claim honest gap. The `*` column span is
/// recorded either way so the column→code sync can flash the projection.
#[allow(clippy::too_many_arguments)]
fn resolve_star(
    node_id: &str,
    source_leaf: Option<&str>,
    known_cte_columns: &KnownCteColumns,
    item: &SelectItem,
    sql: &str,
    index: &ByteIndex,
    edges: &mut Vec<ColumnEdge>,
    spans: &mut Vec<ColumnSpan>,
) -> Option<Vec<String>> {
    push_span(node_id, "*", item, sql, index, spans);
    if let Some(columns) = known_cte_output_columns(source_leaf, known_cte_columns) {
        let leaf = source_leaf.expect("a known CTE was found, so the leaf is Some");
        for column in &columns {
            edges.push(ColumnEdge::new(
                ColumnRef::intra(leaf, column.clone()),
                ColumnRef::intra(node_id, column.clone()),
                ColumnEdgeKind::PassThrough,
                ColumnEdgeConfidence::Resolved,
            ));
        }
        Some(columns)
    } else {
        push_star_edge(node_id, source_leaf, edges);
        None
    }
}

/// The fully-enumerable output columns of the CTE named `source_leaf`, when
/// that leaf names a KNOWN intra-model CTE whose own projection was
/// enumerable. `None` for an unknown external relation, an unqualified star
/// (`source_leaf` is `None`), or a known CTE whose output was non-enumerable
/// (it carried its own Opaque star / anonymous expression).
fn known_cte_output_columns(
    source_leaf: Option<&str>,
    known_cte_columns: &KnownCteColumns,
) -> Option<Vec<String>> {
    let leaf = source_leaf?;
    known_cte_columns.get(leaf).cloned().flatten()
}

/// A resolved input column reference for the Tier-1 cases — the qualifier
/// (when present) maps through `aliases`; an unqualified column resolves
/// against the sole relation, or fans out (handled by the caller via
/// [`InputCol::candidates`]).
struct InputCol {
    /// Candidate source node ids the column could come from. One ⇒ Resolved;
    /// many ⇒ Ambiguous (fan out). The qualified case has exactly one.
    candidates: Vec<String>,
    column: String,
    confidence: ColumnEdgeConfidence,
}

/// Extract the direct input column of a projection expression for the
/// Tier-1 cases ONLY: a bare `Identifier` or an aliased/qualified
/// `CompoundIdentifier`. Returns `None` for any expression
/// (function/CASE/binary-op/…) — those are CLL-3's honest absence.
fn direct_column_ref(
    expr: &Expr,
    aliases: &HashMap<String, String>,
    sole_leaf: Option<&str>,
) -> Option<InputCol> {
    match expr {
        // `col` — unqualified. Single-source ⇒ Resolved against the sole
        // leaf; multi-source ⇒ Ambiguous fan-out to every candidate.
        Expr::Identifier(ident) => {
            let column = ident.value.to_ascii_lowercase();
            if let Some(leaf) = sole_leaf {
                return Some(InputCol {
                    candidates: vec![leaf.to_owned()],
                    column,
                    confidence: ColumnEdgeConfidence::Resolved,
                });
            }
            // Multi-source (or zero-source) ⇒ fan out to every candidate
            // relation, all Ambiguous. De-duplicated + ordered (BTreeSet) so
            // the emitted edge order is deterministic for goldens.
            let candidates: Vec<String> = aliases
                .values()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if candidates.is_empty() {
                None
            } else {
                Some(InputCol {
                    candidates,
                    column,
                    confidence: ColumnEdgeConfidence::Ambiguous,
                })
            }
        }
        // `q.col` — qualified. The qualifier resolves through `aliases` to a
        // source leaf ⇒ Resolved. An unresolvable qualifier still yields the
        // edge against the bare qualifier (Resolved — the SQL was explicit).
        Expr::CompoundIdentifier(_) => {
            let (qualifier, column) = qualified_column(expr)?;
            let source = aliases.get(&qualifier).cloned().unwrap_or(qualifier);
            Some(InputCol {
                candidates: vec![source],
                column,
                confidence: ColumnEdgeConfidence::Resolved,
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------
// CLL-3 (cute-dbt#449) — expression provenance (`Derived` edges).
//
// `coalesce(a.x, b.y) AS z`, `CASE … END AS z`, `a.x + b.y AS s` — the
// projection item is an EXPRESSION, not a direct column reference, so
// `direct_column_ref` returns `None`. The CLL-3 walker collects every
// column reference the expression reads and emits ONE `Derived` edge per
// input (many-to-one).
//
// Honesty floor (never-a-false-claim):
//   - A qualified input (`a.x`) ⇒ Resolved (the SQL named the relation).
//   - An unqualified input (`x`) over a SOLE relation ⇒ Resolved; over a
//     multi-relation FROM ⇒ fan out to every candidate, all Ambiguous.
//   - The walk is DEPTH/COMPLEXITY-CAPPED. An AST past the cap (deeply
//     nested parens/functions, `CASE WHEN EXISTS (…)`, …) degrades the
//     WHOLE column's edges to Ambiguous and fans out the refs it COULD
//     reach before the cap — NEVER a panic, NEVER a fabricated Resolved,
//     NEVER a silent total drop. The cap mirrors the engine's posture:
//     when the transform is too complex to attribute exactly, we still
//     name the visible inputs but stop claiming the SQL was explicit.
//
// This is intra-model only (the same scope as CLL-2's pass-through edges);
// cross-model trace-to-source is CLL-4 (#450).
// ---------------------------------------------------------------------

/// Maximum AST descent depth for the expression-provenance walk. A
/// realistic dbt projection expression is a handful of nodes deep; this
/// bound is generous for honest SQL yet finite, so a pathological /
/// adversarial AST (deeply nested parens, recursive functions) cannot blow
/// the stack or spin — it trips the cap and degrades. Chosen well above any
/// hand-written transform and well below a stack-overflow risk.
const DERIVED_DEPTH_CAP: u32 = 64;

/// Maximum number of distinct input refs collected from one expression
/// before the walk declares the expression too complex to attribute
/// exactly. A wide `coalesce`/`CASE` is fine; a fan-out into hundreds of
/// refs is a complexity signal that degrades to Ambiguous.
const DERIVED_REF_CAP: usize = 64;

/// One input column reference collected from a projection expression,
/// either qualified (`a.x`) or bare (`x`).
enum ExprRef {
    /// `q.col` — the qualifier resolves through the alias map.
    Qualified { qualifier: String, column: String },
    /// `col` — unqualified; resolved against the sole relation or fanned out.
    Bare { column: String },
}

/// The refs an expression reads, plus whether the descent hit the
/// depth/complexity cap (which forces every resulting edge to Ambiguous).
struct DerivedRefs {
    refs: Vec<ExprRef>,
    capped: bool,
}

/// Walk a projection EXPRESSION, collecting every column reference it reads
/// (qualified or bare), depth-capped. Pure: no I/O, no shared mutation —
/// it owns its accumulator and returns it. The `capped` flag rides out so
/// the caller can degrade confidence honestly.
fn collect_derived_refs(expr: &Expr) -> DerivedRefs {
    let mut out = DerivedRefs {
        refs: Vec::new(),
        capped: false,
    };
    walk_derived_refs(expr, 0, &mut out);
    out
}

/// The recursive descent behind [`collect_derived_refs`]. Appends a ref for
/// every `Identifier` / `CompoundIdentifier` reached; descends the same
/// expression shapes as [`collect_qualified_refs`] PLUS the bare-identifier
/// case. Once `depth` exceeds [`DERIVED_DEPTH_CAP`] or the ref count exceeds
/// [`DERIVED_REF_CAP`], it sets `capped` and stops descending that branch —
/// it never recurses past the cap (no stack-overflow), and it keeps what it
/// already saw (no silent total drop).
fn walk_derived_refs(expr: &Expr, depth: u32, out: &mut DerivedRefs) {
    if depth > DERIVED_DEPTH_CAP || out.refs.len() > DERIVED_REF_CAP {
        out.capped = true;
        return;
    }
    let next = depth + 1;
    match expr {
        // A qualified leaf — `a.x`.
        Expr::CompoundIdentifier(_) => {
            if let Some((qualifier, column)) = qualified_column(expr) {
                out.refs.push(ExprRef::Qualified { qualifier, column });
            }
        }
        // A bare leaf — `x` (CLL-3 reaches this; the correlation walker does
        // not, since a bare identifier carries no qualifier).
        Expr::Identifier(ident) => {
            out.refs.push(ExprRef::Bare {
                column: ident.value.to_ascii_lowercase(),
            });
        }
        // An `EXISTS`/`IN`-subquery, OR a SCALAR `(SELECT …)` projection
        // subquery, is a shape we do NOT attribute semantically — a
        // complexity signal that trips the cap (degrade to Ambiguous) rather
        // than silently claim full resolution. Refs already seen are kept
        // (honest absence, never a silent total drop).
        //
        // A scalar `Expr::Subquery` is the load-bearing case (cute-dbt#449,
        // CodeRabbit/verifier): its value comes from ANOTHER relation through
        // the subquery — an honestly UNKNOWN intra-model source, NOT an input
        // the projection reads. Descending its body (its correlated `WHERE` or
        // its inner projection) would FABRICATE provenance: it would emit a
        // Resolved edge from a PREDICATE column (`p.order_id`, never the value
        // column `p.paid_at`) attributed to a PHANTOM relation that is not an
        // intra-model DAG node. So we treat it EXACTLY like EXISTS/IN — never
        // collect any of its internals. A pure-subquery projection
        // (`(SELECT …) AS m`) thus yields NO intra-model Derived edge (Opaque /
        // honest absence); a mixed projection (`coalesce(a.x, (SELECT …))`)
        // trips the cap, degrading every produced edge — fanned out from ONLY
        // the other top-level visible refs (`a.x`) — to Ambiguous. The
        // correlation walker ([`collect_qualified_refs`]) still descends
        // `Expr::Subquery` for its own (different) outer-correlation purpose;
        // only THIS projection-provenance walker caps it. Cross-model trace
        // into the inner relation is CLL-4 (#450).
        Expr::Exists { .. } | Expr::InSubquery { .. } | Expr::Subquery(_) => {
            out.capped = true;
        }
        // Every other shape descends over its shared child set (the SAME
        // descent the correlation walker uses), depth-bounded.
        _ => {
            for child in expr_children(expr) {
                walk_derived_refs(child, next, out);
            }
        }
    }
}

/// Resolve the collected expression refs into `Derived` edges' [`InputCol`]s.
/// Qualified refs resolve through `aliases`; bare refs resolve against the
/// sole relation (Resolved) or fan out across every candidate (Ambiguous).
/// When the walk was `capped`, EVERY produced edge degrades to Ambiguous —
/// we still list the visible inputs (honest absence) but stop claiming the
/// SQL was explicit. De-duplicated + deterministically ordered for goldens.
fn derived_input_cols(
    collected: DerivedRefs,
    aliases: &HashMap<String, String>,
    sole_leaf: Option<&str>,
) -> Vec<InputCol> {
    // (source_node, column) → confidence, de-duplicated + ordered.
    let mut by_target: std::collections::BTreeMap<(String, String), ColumnEdgeConfidence> =
        std::collections::BTreeMap::new();
    let all_candidates: Vec<String> = aliases
        .values()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let degrade = |c: ColumnEdgeConfidence| {
        if collected.capped {
            ColumnEdgeConfidence::Ambiguous
        } else {
            c
        }
    };
    for r in collected.refs {
        match r {
            ExprRef::Qualified { qualifier, column } => {
                let source = aliases.get(&qualifier).cloned().unwrap_or(qualifier);
                let conf = degrade(ColumnEdgeConfidence::Resolved);
                merge_max(&mut by_target, (source, column), conf);
            }
            ExprRef::Bare { column } => {
                if let Some(leaf) = sole_leaf {
                    let conf = degrade(ColumnEdgeConfidence::Resolved);
                    merge_max(&mut by_target, (leaf.to_owned(), column), conf);
                } else {
                    // Multi-source (or zero-source) ⇒ fan out, Ambiguous.
                    for cand in &all_candidates {
                        merge_max(
                            &mut by_target,
                            (cand.clone(), column.clone()),
                            ColumnEdgeConfidence::Ambiguous,
                        );
                    }
                }
            }
        }
    }
    by_target
        .into_iter()
        .map(|((source, column), confidence)| InputCol {
            candidates: vec![source],
            column,
            confidence,
        })
        .collect()
}

/// Insert/merge one resolved target, keeping the WEAKER (more honest)
/// confidence when the same `(source, column)` is reached twice — a column
/// that appears once qualified and once unqualified-ambiguous stays
/// Ambiguous, never silently upgraded to Resolved.
fn merge_max(
    map: &mut std::collections::BTreeMap<(String, String), ColumnEdgeConfidence>,
    key: (String, String),
    conf: ColumnEdgeConfidence,
) {
    let entry = map.entry(key).or_insert(conf);
    if confidence_rank(conf) > confidence_rank(*entry) {
        *entry = conf;
    }
}

/// Honesty ordering: a HIGHER rank is the MORE-degraded (weaker) claim, so
/// `merge_max` keeps the weakest. Opaque is weakest, then Ambiguous, then
/// Resolved. Exhaustive on purpose (no wildcard): a future
/// `ColumnEdgeConfidence` variant MUST be consciously ranked here — the
/// compile error is the reminder, never a silent maximal-degrade default.
fn confidence_rank(c: ColumnEdgeConfidence) -> u8 {
    match c {
        ColumnEdgeConfidence::Resolved => 0,
        ColumnEdgeConfidence::Ambiguous => 1,
        ColumnEdgeConfidence::Opaque => 2,
    }
}

/// Push one edge per candidate source (fan-out for the Ambiguous case;
/// exactly one for the Resolved case). Each edge is intra-model.
fn push_edges(
    node_id: &str,
    output: &str,
    input: InputCol,
    kind: ColumnEdgeKind,
    edges: &mut Vec<ColumnEdge>,
) {
    for source in input.candidates {
        edges.push(ColumnEdge::new(
            ColumnRef::intra(source, input.column.clone()),
            ColumnRef::intra(node_id, output),
            kind,
            input.confidence,
        ));
    }
}

/// Push the virtual `*→*` Opaque edge for a `SELECT *` / `q.*` over an
/// UNKNOWN external relation (the only case that reaches here — a star over a
/// known intra-model CTE is expanded by [`resolve_star`] instead). The source
/// node is the relation's leaf when known, else a synthetic `*` source.
fn push_star_edge(node_id: &str, source_leaf: Option<&str>, edges: &mut Vec<ColumnEdge>) {
    let source = source_leaf.unwrap_or("*");
    edges.push(ColumnEdge::new(
        ColumnRef::intra(source, "*"),
        ColumnRef::intra(node_id, "*"),
        ColumnEdgeKind::Source,
        ColumnEdgeConfidence::Opaque,
    ));
}

/// The leaf relation of a `q.*` qualified wildcard, resolved through the
/// alias map. `None` for an expression-qualified wildcard.
fn qualified_wildcard_leaf(
    kind: &SelectItemQualifiedWildcardKind,
    aliases: &HashMap<String, String>,
) -> Option<String> {
    let SelectItemQualifiedWildcardKind::ObjectName(name) = kind else {
        return None;
    };
    let qualifier = name.0.last()?.as_ident()?.value.to_ascii_lowercase();
    Some(aliases.get(&qualifier).cloned().unwrap_or(qualifier))
}

/// Record the compiled span of one projection item as a [`ColumnSpan`] for
/// `column` under `node_id` — a sub-range of the owning body. Degraded
/// spans (empty / out-of-bounds) are dropped (degrade, not lie), so the
/// `SpanRole::Column` entry is simply absent.
fn push_span(
    node_id: &str,
    column: &str,
    item: &SelectItem,
    sql: &str,
    index: &ByteIndex,
    spans: &mut Vec<ColumnSpan>,
) {
    let Some(source_span) = span_to_source_span(item.span(), sql, index) else {
        return;
    };
    spans.push(ColumnSpan::new(
        node_id,
        column.to_ascii_lowercase(),
        source_span,
    ));
}

/// Convert a sqlparser [`Span`] into a domain [`SourceSpan`], or `None` when
/// the span is degraded — a line-0 sentinel (`start.line == 0 ||
/// end.line == 0`, sqlparser's "no span") or an invalid byte range
/// (`start > end || end > sql.len()`). The degrade-not-lie boundary: a
/// fabricated span would point the column→code sync at the wrong bytes, so an
/// absent span is the honest answer. Split out of [`push_span`] so the guard
/// is unit-testable with synthetic spans (no parser round-trip needed).
fn span_to_source_span(span: Span, sql: &str, index: &ByteIndex) -> Option<SourceSpan> {
    if span.start.line == 0 || span.end.line == 0 {
        return None;
    }
    let start = index.byte_of(sql, span.start);
    let end = index.byte_of(sql, span.end);
    if start > end || end > sql.len() {
        return None;
    }
    Some(SourceSpan {
        start: loc_to_pos(span.start, start),
        end: loc_to_pos(span.end, end),
    })
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
/// LIKE, CAST, parens, unary NOT) plus — since CLL-3 (cute-dbt#449) —
/// `Function`/`Case`/`Subquery` argument expressions. A qualified ref
/// nested inside a function or CASE branch is now FOUND (strictly more
/// correlation evidence, never less). Bare `Identifier`s carry no
/// qualifier, so they remain invisible to THIS walker — correlation
/// scoping needs a qualifier; the CLL-3 projection walker
/// ([`collect_derived_refs`]) captures the unqualified case separately.
/// Unknown variants are still not descended (silence, never
/// misclassification). This walker is depth-unbounded by construction:
/// it only ever appends already-validated qualified pairs and is used
/// for bounded predicate trees; the CLL-3 projection walker applies the
/// explicit depth cap.
fn collect_qualified_refs(expr: &Expr, refs: &mut Vec<(String, String)>) {
    if let Expr::CompoundIdentifier(_) = expr {
        refs.extend(qualified_column(expr));
        return;
    }
    // Bare `Identifier`s carry no qualifier — invisible to this walker by
    // design (correlation needs a qualifier). Every other shape recurses
    // over its child sub-expressions via the shared descent.
    for child in expr_children(expr) {
        collect_qualified_refs(child, refs);
    }
}

/// The child sub-expressions to descend into for both ref walkers — the
/// SINGLE place that knows the predicate/expression shapes cute-dbt models
/// (AND/OR trees, comparisons, IS \[NOT\] NULL, BETWEEN, IN lists, LIKE,
/// CAST, parens, unary NOT) plus, since CLL-3 (cute-dbt#449),
/// `Function` args, `Case` branches, the special-syntax exprs sqlparser
/// models as their own variants rather than `Expr::Function`
/// (`Extract`/`Ceil`/`Floor`/`Convert`/`Collate`/`Substring`/`Trim`/
/// `Position`/`Overlay`), and a scalar `Subquery`'s correlated `WHERE`.
/// Sharing this between [`collect_qualified_refs`] and [`walk_derived_refs`]
/// keeps the two walkers thin (one descent shape, not two) and fully
/// exercised — EXCEPT the `Subquery` arm, which only the correlation walker
/// reaches: [`walk_derived_refs`] short-circuits `Expr::Subquery` as a cap
/// signal BEFORE calling this (a scalar subquery is not an intra-model
/// input — descending it would fabricate provenance; see that walker).
/// `Identifier` / `CompoundIdentifier` leaves
/// have NO children — the callers handle them. Unknown variants yield no
/// children: silence, never misclassification.
fn expr_children(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::BinaryOp { left, right, .. } => vec![left, right],
        Expr::UnaryOp { expr: inner, .. }
        | Expr::Nested(inner)
        | Expr::IsNull(inner)
        | Expr::IsNotNull(inner)
        | Expr::Cast { expr: inner, .. }
        // Special-syntax single-operand exprs (cute-dbt#449,
        // CodeRabbit/verifier): sqlparser models `extract(field FROM a.x)`,
        // `ceil(a.x)`, `floor(a.x)`, `convert(a.x, …)`, and `a.x COLLATE c`
        // as DISTINCT `Expr` variants rather than `Expr::Function`, so their
        // operand refs were silently dropped. Descending them collects the
        // operand (honest-direction — ADDS a correct `Derived` edge, never
        // fabricates one). The non-`Expr` fields (datetime field, target
        // type, collation name) carry no column refs.
        | Expr::Extract { expr: inner, .. }
        | Expr::Ceil { expr: inner, .. }
        | Expr::Floor { expr: inner, .. }
        | Expr::Convert { expr: inner, .. }
        | Expr::Collate { expr: inner, .. } => vec![inner],
        Expr::Between {
            expr: inner,
            low,
            high,
            ..
        } => vec![inner, low, high],
        Expr::InList {
            expr: inner, list, ..
        } => {
            let mut out = vec![inner.as_ref()];
            out.extend(list.iter());
            out
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
        } => vec![inner, pattern],
        // Multi-operand special-syntax exprs (cute-dbt#449,
        // CodeRabbit/verifier): `substring(a.x FROM b.lo FOR b.len)`,
        // `trim(b.ch FROM a.x)`, `position(a.x IN b.hay)`,
        // `overlay(a.x PLACING b.p FROM b.f)`. Each is its OWN `Expr`
        // variant (not `Expr::Function`), so every column operand was
        // dropped. Collect ALL the `Expr`-typed operands (the `Option`
        // ones only when present). Honest-direction: ADDS correct
        // `Derived` edges, never fabricates one.
        Expr::Substring {
            expr: inner,
            substring_from,
            substring_for,
            ..
        } => {
            let mut out = vec![inner.as_ref()];
            out.extend(substring_from.as_deref());
            out.extend(substring_for.as_deref());
            out
        }
        Expr::Trim {
            expr: inner,
            trim_what,
            trim_characters,
            ..
        } => {
            let mut out = vec![inner.as_ref()];
            out.extend(trim_what.as_deref());
            if let Some(chars) = trim_characters {
                out.extend(chars.iter());
            }
            out
        }
        Expr::Position { expr: inner, r#in } => vec![inner, r#in],
        Expr::Overlay {
            expr: inner,
            overlay_what,
            overlay_from,
            overlay_for,
        } => {
            let mut out = vec![inner.as_ref(), overlay_what.as_ref(), overlay_from.as_ref()];
            out.extend(overlay_for.as_deref());
            out
        }
        Expr::Function(func) => function_arg_exprs(func),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let mut out: Vec<&Expr> = Vec::new();
            out.extend(operand.as_deref());
            for when in conditions {
                out.push(&when.condition);
                out.push(&when.result);
            }
            out.extend(else_result.as_deref());
            out
        }
        Expr::Subquery(query) => subquery_where(query).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// The correlated `WHERE` of a scalar subquery, when its body is a plain
/// `SELECT`. The inner relation's own columns are NOT this model's inputs —
/// only the correlated outer refs the `WHERE` carries — so the projection is
/// deliberately not descended (conservative). `None` for a set-operation /
/// `WITH`-bearing / where-less subquery.
fn subquery_where(query: &Query) -> Option<&Expr> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    select.selection.as_ref()
}

/// The argument expressions of a function call — the `Expr` inside each
/// positional / named arg, PLUS the column-bearing exprs of the trailing
/// clauses (cute-dbt#480): the `FILTER (WHERE …)` predicate, the `OVER (…)`
/// window spec (`PARTITION BY` + `ORDER BY` exprs), and the `WITHIN GROUP
/// (ORDER BY …)` ordering exprs. Before #480 only `func.args` was descended,
/// so a column referenced ONLY in one of those clauses
/// (`row_number() OVER (PARTITION BY a.id)`, `sum(a.x) FILTER (WHERE b.flag)`)
/// was silently dropped from lineage. Honest-direction: this ADDS the
/// `Derived` edges for refs genuinely present in the clause — it never
/// fabricates one. A `NamedWindow` reference (`OVER w`) names a window
/// defined elsewhere in the `SELECT` and carries no inline exprs here, so it
/// contributes nothing (honest absence). Wildcard args (`count(*)`, `f(t.*)`)
/// carry no column expression and contribute nothing. Used by both the
/// qualified-ref walker (correlation) and the CLL-3 projection walker; the
/// CLL-3 walker's depth/complexity cap applies to these clause exprs exactly
/// as it does to ordinary args.
fn function_arg_exprs(func: &sqlparser::ast::Function) -> Vec<&Expr> {
    let mut out: Vec<&Expr> = Vec::new();
    if let sqlparser::ast::FunctionArguments::List(list) = &func.args {
        out.extend(list.args.iter().filter_map(|arg| match arg {
            sqlparser::ast::FunctionArg::Unnamed(fae)
            | sqlparser::ast::FunctionArg::Named { arg: fae, .. }
            | sqlparser::ast::FunctionArg::ExprNamed { arg: fae, .. } => match fae {
                sqlparser::ast::FunctionArgExpr::Expr(e) => Some(e),
                _ => None,
            },
        }));
    }
    // `FILTER (WHERE <predicate>)` — the predicate's column refs are real
    // inputs to the aggregate (`sum(a.x) FILTER (WHERE b.flag)` reads b.flag).
    if let Some(filter) = &func.filter {
        out.push(filter.as_ref());
    }
    // `OVER (PARTITION BY … ORDER BY …)` — an inline window spec's
    // `PARTITION BY` and `ORDER BY` exprs reference real columns
    // (`row_number() OVER (PARTITION BY a.id ORDER BY a.ts)`). A bare named
    // window reference has no inline exprs.
    if let Some(sqlparser::ast::WindowType::WindowSpec(spec)) = &func.over {
        out.extend(spec.partition_by.iter());
        out.extend(spec.order_by.iter().map(|obe| &obe.expr));
    }
    // `WITHIN GROUP (ORDER BY …)` — ordered-set aggregate ordering keys
    // (`percentile_cont(0.5) WITHIN GROUP (ORDER BY a.x)` reads a.x).
    out.extend(func.within_group.iter().map(|obe| &obe.expr));
    out
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
///
/// `pub(crate)` so the raw-source scanner (`raw_scan`) can convert the
/// tokenizer's `Location` line/col endpoints to byte offsets with the SAME
/// helper the compiled side uses — one shared line/col↔byte convention, no
/// divergence (cute-dbt#473).
pub(crate) struct ByteIndex {
    /// `line_starts[i]` = byte offset of the start of line `i + 1`.
    line_starts: Vec<usize>,
}

impl ByteIndex {
    pub(crate) fn new(sql: &str) -> Self {
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
    pub(crate) fn byte_of(&self, sql: &str, loc: Location) -> usize {
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

    // -----------------------------------------------------------------
    // CLL-2 (cute-dbt#447) — intra-model column edges (pass-through /
    // rename) + the `SpanRole::Column` sub-range spans.
    // -----------------------------------------------------------------

    /// Find every edge whose `to_col` is `(node_id, column)` in declaration
    /// order — the resolver's output for one projected column.
    fn edges_to<'a>(g: &'a CteGraph, node_id: &str, column: &str) -> Vec<&'a ColumnEdge> {
        g.column_edges()
            .iter()
            .filter(|e| e.to_col == ColumnRef::intra(node_id, column))
            .collect()
    }

    /// The intra-model source node id of an edge's `from_col`.
    fn source_node(e: &ColumnEdge) -> String {
        match &e.from_col.scope {
            crate::domain::ColumnScope::Intra { node_id } => node_id.clone(),
            crate::domain::ColumnScope::Cross { .. } => unreachable!("intra-only in CLL-3"),
        }
    }

    /// Parse a bare SQL expression for the qualified-ref walker tests.
    fn parse_expr(sql: &str) -> Expr {
        let mut stmts = Parser::parse_sql(&GenericDialect {}, &format!("SELECT {sql}"))
            .unwrap_or_else(|e| panic!("`{sql}` should parse as an expr: {e:?}"));
        let stmt = stmts.pop().expect("one statement");
        let Statement::Query(query) = stmt else {
            panic!("expected a query");
        };
        let SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected a select");
        };
        match &select.projection[0] {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e.clone(),
            other => panic!("expected an expr projection, got {other:?}"),
        }
    }

    #[test]
    fn column_lineage_pass_through_resolved() {
        // `c.email AS email` — output name == input column ⇒ PassThrough,
        // qualified ⇒ Resolved; the qualifier resolves through select_alias_map.
        let g = graph(
            "WITH customers AS (SELECT 1 AS email) \
             SELECT c.email AS email FROM customers c",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "email");
        assert_eq!(edges.len(), 1, "exactly one resolved input");
        let e = edges[0];
        assert_eq!(e.from_col, ColumnRef::intra("customers", "email"));
        assert_eq!(e.kind, ColumnEdgeKind::PassThrough);
        assert_eq!(e.confidence, ColumnEdgeConfidence::Resolved);
    }

    #[test]
    fn column_lineage_rename_resolved() {
        // `c.email AS contact_email` — output != input ⇒ Renamed/Resolved.
        let g = graph(
            "WITH customers AS (SELECT 1 AS email) \
             SELECT c.email AS contact_email FROM customers c",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "contact_email");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from_col, ColumnRef::intra("customers", "email"));
        assert_eq!(edges[0].kind, ColumnEdgeKind::Renamed);
        assert_eq!(edges[0].confidence, ColumnEdgeConfidence::Resolved);
    }

    #[test]
    fn column_lineage_single_source_unqualified_resolved() {
        // Unqualified `email` over a SOLE relation ⇒ Resolved (sole_relation_leaf).
        let g = graph(
            "WITH customers AS (SELECT 1 AS email) \
             SELECT email FROM customers",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "email");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from_col, ColumnRef::intra("customers", "email"));
        assert_eq!(edges[0].kind, ColumnEdgeKind::PassThrough);
        assert_eq!(edges[0].confidence, ColumnEdgeConfidence::Resolved);
    }

    #[test]
    fn column_lineage_ambiguous_fan_out_never_dropped() {
        // Unqualified `status` under a multi-relation FROM ⇒ fan out to EVERY
        // candidate source, all Ambiguous, never dropped.
        let g = graph(
            "WITH orders AS (SELECT 1 AS status), refunds AS (SELECT 2 AS status) \
             SELECT status AS status FROM orders, refunds",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "status");
        assert_eq!(edges.len(), 2, "fanned out to both candidate sources");
        let froms: std::collections::BTreeSet<_> =
            edges.iter().map(|e| e.from_col.column.clone()).collect();
        assert_eq!(froms, ["status".to_owned()].into_iter().collect());
        let sources: std::collections::BTreeSet<_> = edges
            .iter()
            .map(|e| match &e.from_col.scope {
                crate::domain::ColumnScope::Intra { node_id } => node_id.clone(),
                crate::domain::ColumnScope::Cross { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(
            sources,
            ["orders".to_owned(), "refunds".to_owned()]
                .into_iter()
                .collect()
        );
        assert!(
            edges
                .iter()
                .all(|e| e.confidence == ColumnEdgeConfidence::Ambiguous),
            "every fan-out edge is Ambiguous"
        );
    }

    #[test]
    fn column_lineage_opaque_star_over_unknown_external() {
        // `SELECT *` over an undocumented external source ⇒ a virtual `*→*`
        // edge, Opaque, never a fabricated column list.
        let g = graph("SELECT * FROM raw_external_thing");
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "*");
        assert_eq!(edges.len(), 1, "one virtual star edge");
        let e = edges[0];
        assert_eq!(e.from_col.column, "*");
        assert_eq!(e.to_col.column, "*");
        assert_eq!(e.confidence, ColumnEdgeConfidence::Opaque);
        assert_eq!(e.kind, ColumnEdgeKind::Source);
        assert_eq!(
            e.from_col,
            ColumnRef::intra("raw_external_thing", "*"),
            "the star sources from the (unknown) external relation leaf"
        );
    }

    #[test]
    fn column_lineage_star_over_known_cte_expands_not_opaque() {
        // never-a-false-claim: `select * from <known_cte>` is NOT Opaque — the
        // upstream CTE's output columns were resolved when the pass walked it,
        // so the star EXPANDS into one Resolved PassThrough edge per column.
        // This mirrors jaffle-shop's compiled `stg_customers`
        // (`… renamed as (select id as customer_id, …) select * from renamed`).
        let g = graph(
            "WITH renamed AS ( \
                SELECT id AS customer_id, \
                       trim(first_name) AS first_name, \
                       trim(last_name) AS last_name \
                FROM source \
             ) \
             SELECT * FROM renamed",
        );
        // The false `renamed.* → (final select).*` Opaque edge is GONE.
        let star_edges = edges_to(&g, TERMINAL_NODE_NAME, "*");
        assert!(
            star_edges.is_empty(),
            "no virtual `*→*` edge for a star over a known CTE — it expands"
        );
        assert!(
            g.column_edges()
                .iter()
                .all(|e| e.confidence != ColumnEdgeConfidence::Opaque
                    || e.from_col.column != "*"
                    || !matches!(
                        &e.from_col.scope,
                        crate::domain::ColumnScope::Intra { node_id } if node_id == "renamed"
                    )),
            "the renamed.* Opaque edge must not exist (never-a-false-claim)"
        );
        // One Resolved PassThrough edge per upstream output column.
        for column in ["customer_id", "first_name", "last_name"] {
            let edges = edges_to(&g, TERMINAL_NODE_NAME, column);
            assert_eq!(
                edges.len(),
                1,
                "expanded `{column}` has exactly one upstream edge"
            );
            let e = edges[0];
            assert_eq!(
                e.from_col,
                ColumnRef::intra("renamed", column),
                "`{column}` traces back to the renamed CTE's same-named column"
            );
            assert_eq!(e.kind, ColumnEdgeKind::PassThrough);
            assert_eq!(
                e.confidence,
                ColumnEdgeConfidence::Resolved,
                "a star over a KNOWN CTE is Resolved, never Opaque"
            );
        }
    }

    #[test]
    fn column_lineage_qualified_star_over_known_cte_expands() {
        // `q.*` over a known CTE expands the same way (sourced through the
        // alias map), Resolved per-column — never Opaque.
        let g = graph(
            "WITH renamed AS (SELECT 1 AS a, 2 AS b) \
             SELECT r.* FROM renamed r",
        );
        assert!(
            edges_to(&g, TERMINAL_NODE_NAME, "*").is_empty(),
            "no virtual `*→*` edge for a qualified star over a known CTE"
        );
        for column in ["a", "b"] {
            let edges = edges_to(&g, TERMINAL_NODE_NAME, column);
            assert_eq!(edges.len(), 1, "expanded `{column}`");
            assert_eq!(edges[0].from_col, ColumnRef::intra("renamed", column));
            assert_eq!(edges[0].kind, ColumnEdgeKind::PassThrough);
            assert_eq!(edges[0].confidence, ColumnEdgeConfidence::Resolved);
        }
    }

    #[test]
    fn column_lineage_star_over_cte_with_opaque_star_stays_opaque() {
        // The honest gap: a star over a CTE whose OWN projection is a star over
        // an unknown external relation is non-enumerable — we cannot list its
        // columns, so the downstream star degrades to Opaque (never fabricated).
        // This mirrors jaffle-shop's `source as (select * from raw_customers)`.
        let g = graph(
            "WITH source AS (SELECT * FROM raw_external_thing) \
             SELECT * FROM source",
        );
        // source's own star over the unknown external stays Opaque.
        let source_star = edges_to(&g, "source", "*");
        assert_eq!(source_star.len(), 1);
        assert_eq!(source_star[0].confidence, ColumnEdgeConfidence::Opaque);
        assert_eq!(
            source_star[0].from_col,
            ColumnRef::intra("raw_external_thing", "*")
        );
        // The terminal star over `source` cannot expand (source is
        // non-enumerable) ⇒ it ALSO stays an honest Opaque `*→*` edge.
        let term_star = edges_to(&g, TERMINAL_NODE_NAME, "*");
        assert_eq!(term_star.len(), 1, "one honest Opaque star edge");
        assert_eq!(term_star[0].confidence, ColumnEdgeConfidence::Opaque);
        assert_eq!(term_star[0].from_col, ColumnRef::intra("source", "*"));
    }

    #[test]
    fn column_lineage_derived_function_resolved() {
        // CLL-3 (cute-dbt#449): `coalesce(a.x, b.y) AS z` is an Expr::Function
        // — the extended walker collects BOTH qualified args ⇒ two `Derived`
        // edges, both `Resolved` (the SQL named the relation explicitly).
        let g = graph(
            "WITH a AS (SELECT 1 AS x), b AS (SELECT 2 AS y) \
             SELECT coalesce(a.x, b.y) AS z FROM a, b",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        assert_eq!(edges.len(), 2, "one Derived edge per collected input ref");
        assert!(
            edges.iter().all(|e| e.kind == ColumnEdgeKind::Derived),
            "an expression input is a Derived edge"
        );
        assert!(
            edges
                .iter()
                .all(|e| e.confidence == ColumnEdgeConfidence::Resolved),
            "both args are qualified ⇒ Resolved"
        );
        let from: std::collections::BTreeSet<_> = edges
            .iter()
            .map(|&e| (source_node(e), e.from_col.column.clone()))
            .collect();
        assert_eq!(
            from,
            [
                ("a".to_owned(), "x".to_owned()),
                ("b".to_owned(), "y".to_owned())
            ]
            .into_iter()
            .collect(),
            "fans out to BOTH expression inputs, never a silent drop"
        );
        // The derived column also carries a span for the sync layer.
        assert!(
            g.column_spans()
                .iter()
                .any(|cs| cs.node_id == TERMINAL_NODE_NAME && cs.column == "z"),
            "the derived column still carries a span"
        );
    }

    #[test]
    fn column_lineage_derived_case_collects_each_branch() {
        // A searched CASE — refs come from every WHEN condition, every THEN
        // result, and the ELSE result (no operand here).
        let g = graph(
            "WITH a AS (SELECT 1 AS x, 2 AS flag), b AS (SELECT 3 AS y) \
             SELECT CASE WHEN a.flag > 0 THEN a.x ELSE b.y END AS z FROM a, b",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        let from: std::collections::BTreeSet<_> = edges
            .iter()
            .map(|&e| (source_node(e), e.from_col.column.clone()))
            .collect();
        assert_eq!(
            from,
            [
                ("a".to_owned(), "flag".to_owned()),
                ("a".to_owned(), "x".to_owned()),
                ("b".to_owned(), "y".to_owned()),
            ]
            .into_iter()
            .collect(),
            "WHEN condition + THEN result + ELSE result all contribute refs"
        );
        assert!(
            edges.iter().all(|e| e.kind == ColumnEdgeKind::Derived),
            "a CASE projection is Derived"
        );
    }

    #[test]
    fn column_lineage_derived_simple_case_operand() {
        // A simple CASE — the operand expression ALSO contributes a ref.
        let g = graph(
            "WITH a AS (SELECT 1 AS x, 2 AS k), b AS (SELECT 3 AS y) \
             SELECT CASE a.k WHEN 1 THEN a.x ELSE b.y END AS z FROM a, b",
        );
        let from: std::collections::BTreeSet<_> = edges_to(&g, TERMINAL_NODE_NAME, "z")
            .iter()
            .map(|&e| (source_node(e), e.from_col.column.clone()))
            .collect();
        assert!(
            from.contains(&("a".to_owned(), "k".to_owned())),
            "the simple-CASE operand a.k is collected (not dropped): {from:?}"
        );
    }

    #[test]
    fn column_lineage_derived_bare_identifier_reached() {
        // CLL-3: a bare unqualified Identifier inside an expression is now
        // reached (CLL-2 ignored it). Single-source ⇒ Resolved.
        let g = graph(
            "WITH a AS (SELECT 1 AS x) \
             SELECT upper(x) AS z FROM a",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        assert_eq!(edges.len(), 1, "bare identifier collected, single-source");
        assert_eq!(edges[0].from_col, ColumnRef::intra("a", "x"));
        assert_eq!(edges[0].kind, ColumnEdgeKind::Derived);
        assert_eq!(edges[0].confidence, ColumnEdgeConfidence::Resolved);
    }

    #[test]
    fn column_lineage_derived_unqualified_multi_source_ambiguous() {
        // A bare identifier under a multi-relation FROM ⇒ fan out to every
        // candidate, all Ambiguous, never dropped, never a wrong Resolved.
        let g = graph(
            "WITH a AS (SELECT 1 AS status), b AS (SELECT 2 AS other) \
             SELECT lower(status) AS z FROM a, b",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        assert_eq!(edges.len(), 2, "fanned out to both candidate sources");
        assert!(
            edges
                .iter()
                .all(|e| e.confidence == ColumnEdgeConfidence::Ambiguous),
            "unqualified-under-multi-source ⇒ Ambiguous"
        );
        assert!(edges.iter().all(|e| e.kind == ColumnEdgeKind::Derived));
        let sources: std::collections::BTreeSet<_> =
            edges.iter().map(|&e| source_node(e)).collect();
        assert_eq!(
            sources,
            ["a".to_owned(), "b".to_owned()].into_iter().collect()
        );
    }

    #[test]
    fn column_lineage_derived_cast_still_works() {
        // CAST inside the expression still resolves its inner column.
        let g = graph(
            "WITH a AS (SELECT 1 AS x) \
             SELECT cast(a.x AS varchar) || 'z' AS z FROM a",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from_col, ColumnRef::intra("a", "x"));
        assert_eq!(edges[0].kind, ColumnEdgeKind::Derived);
    }

    #[test]
    fn column_lineage_derived_between_collects_all_three_operands() {
        // A BETWEEN projection expression — the tested expr + low + high all
        // contribute refs (pins the `expr_children` Between arm).
        let g = graph(
            "WITH a AS (SELECT 1 AS x), b AS (SELECT 2 AS lo, 3 AS hi) \
             SELECT (a.x BETWEEN b.lo AND b.hi) AS z FROM a, b",
        );
        let from: std::collections::BTreeSet<_> = edges_to(&g, TERMINAL_NODE_NAME, "z")
            .iter()
            .map(|&e| (source_node(e), e.from_col.column.clone()))
            .collect();
        assert_eq!(
            from,
            [
                ("a".to_owned(), "x".to_owned()),
                ("b".to_owned(), "lo".to_owned()),
                ("b".to_owned(), "hi".to_owned()),
            ]
            .into_iter()
            .collect(),
            "BETWEEN's expr + low + high operands all contribute Derived refs"
        );
    }

    #[test]
    fn column_lineage_derived_inlist_and_like_collect_refs() {
        // IN-list + LIKE projection expressions — pin the `expr_children`
        // InList and Like arms (the list items + the pattern operand).
        let g_in = graph(
            "WITH a AS (SELECT 1 AS x), b AS (SELECT 2 AS v) \
             SELECT (a.x IN (b.v, 3)) AS z FROM a, b",
        );
        let in_from: std::collections::BTreeSet<_> = edges_to(&g_in, TERMINAL_NODE_NAME, "z")
            .iter()
            .map(|&e| (source_node(e), e.from_col.column.clone()))
            .collect();
        assert!(
            in_from.contains(&("a".to_owned(), "x".to_owned()))
                && in_from.contains(&("b".to_owned(), "v".to_owned())),
            "IN-list collects the tested expr AND the list refs: {in_from:?}"
        );

        let g_like = graph(
            "WITH a AS (SELECT 1 AS x), b AS (SELECT 'p' AS pat) \
             SELECT (a.x LIKE b.pat) AS z FROM a, b",
        );
        let like_from: std::collections::BTreeSet<_> = edges_to(&g_like, TERMINAL_NODE_NAME, "z")
            .iter()
            .map(|&e| (source_node(e), e.from_col.column.clone()))
            .collect();
        assert!(
            like_from.contains(&("a".to_owned(), "x".to_owned()))
                && like_from.contains(&("b".to_owned(), "pat".to_owned())),
            "LIKE collects the tested expr AND the pattern ref: {like_from:?}"
        );
    }

    #[test]
    fn column_lineage_derived_binaryop_two_inputs() {
        // `a.x + b.y AS s` — a BinaryOp expression, two qualified inputs.
        let g = graph(
            "WITH a AS (SELECT 1 AS x), b AS (SELECT 2 AS y) \
             SELECT a.x + b.y AS s FROM a, b",
        );
        let from: std::collections::BTreeSet<_> = edges_to(&g, TERMINAL_NODE_NAME, "s")
            .iter()
            .map(|&e| (source_node(e), e.from_col.column.clone()))
            .collect();
        assert_eq!(
            from,
            [
                ("a".to_owned(), "x".to_owned()),
                ("b".to_owned(), "y".to_owned())
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn column_lineage_derived_pure_scalar_subquery_emits_no_fabricated_edge() {
        // cute-dbt#449 (CodeRabbit/verifier) — the DECISIVE honesty fix. A
        // projection that is JUST a scalar correlated subquery
        // (`(SELECT … WHERE p.k = o.id) AS m`) has its value come from ANOTHER
        // relation through the subquery — an honestly UNKNOWN intra-model
        // source, NOT an input this model reads. The OLD code descended the
        // subquery's WHERE and emitted a Resolved edge from the PREDICATE
        // column (`a.id`/`b.aid`), never the value column (`b.amt`), against a
        // relation that is not even an intra-model DAG node — a false claim on
        // BOTH prongs (wrong column + phantom source). The cap fix makes a pure
        // subquery a complexity signal (like EXISTS/IN): NO intra-model Derived
        // edge at all (Opaque / honest absence). Cross-model trace is CLL-4.
        let g = graph(
            "WITH a AS (SELECT 1 AS id), b AS (SELECT 2 AS aid, 3 AS amt) \
             SELECT (SELECT max(b.amt) FROM b WHERE b.aid = a.id) AS m FROM a",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "m");
        // No fabricated source: never the predicate col, never any inner col.
        let from: std::collections::BTreeSet<_> = edges
            .iter()
            .map(|&e| (source_node(e), e.from_col.column.clone()))
            .collect();
        assert!(
            !from.contains(&("a".to_owned(), "id".to_owned())),
            "the predicate col a.id must NOT be fabricated as a value source: {from:?}"
        );
        assert!(
            from.iter().all(|(node, _)| node != "b"),
            "no inner-relation (b.*) col is fabricated as an intra-model source: {from:?}"
        );
        // A pure-subquery projection yields no intra-model Derived edge at all
        // (its source is honestly unknown — never a fabricated Resolved).
        assert!(
            edges.is_empty(),
            "a pure scalar-subquery projection has no fabricated intra-model edge: {:?}",
            edges
                .iter()
                .map(|e| (source_node(e), e.from_col.column.clone(), e.confidence))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn column_lineage_derived_subquery_verifier_repro_last_paid() {
        // The verifier's EXACT first repro. `last_paid` is a pure scalar
        // subquery over `raw_pay p` (not an intra-model DAG node). NO edge with
        // a `p.*` source (neither the predicate `p.order_id` nor the value
        // `p.paid_at`), and NO Resolved edge for `last_paid` from a non-DAG
        // node — its intra-model source is honestly unknown.
        let g = graph(
            "WITH orders AS (SELECT id, amount FROM raw_orders) \
             SELECT o.id AS id, \
                    (SELECT max(p.paid_at) FROM raw_pay p WHERE p.order_id = o.id) AS last_paid \
             FROM orders o",
        );
        let last_paid = edges_to(&g, TERMINAL_NODE_NAME, "last_paid");
        assert!(
            last_paid
                .iter()
                .all(|e| source_node(e) != "p" && source_node(e) != "raw_pay"),
            "no p.*/raw_pay source is fabricated for last_paid: {:?}",
            last_paid
                .iter()
                .map(|&e| (source_node(e), e.from_col.column.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            !last_paid
                .iter()
                .any(|e| e.confidence == ColumnEdgeConfidence::Resolved),
            "last_paid carries no Resolved edge from a non-DAG node: {:?}",
            last_paid.iter().map(|e| e.confidence).collect::<Vec<_>>()
        );
        // The direct sibling column `id` is unaffected (still resolves).
        let id_edges = edges_to(&g, TERMINAL_NODE_NAME, "id");
        assert!(
            id_edges
                .iter()
                .any(|&e| source_node(e) == "orders" && e.from_col.column == "id"),
            "the plain sibling column o.id still resolves through `orders`: {:?}",
            id_edges
                .iter()
                .map(|&e| (source_node(e), e.from_col.column.clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn column_lineage_derived_subquery_verifier_repro_no_phantom_inner_refs() {
        // The verifier's second repro: a scalar subquery whose WHERE references
        // ONLY its own inner relation (`b.x = b.y`, no outer correlation). The
        // OLD code would still descend and fabricate `b.x`/`b.y` edges; the cap
        // emits none. (`m` is a pure subquery ⇒ no intra-model edge.)
        let g = graph("SELECT (SELECT max(b.z) FROM t2 b WHERE b.x = b.y) AS m FROM t1 a");
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "m");
        assert!(
            edges
                .iter()
                .all(|e| source_node(e) != "b" && source_node(e) != "t2"),
            "no fabricated b.x/b.y/b.z inner refs: {:?}",
            edges
                .iter()
                .map(|&e| (source_node(e), e.from_col.column.clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn column_lineage_derived_mixed_subquery_degrades_visible_ref_to_ambiguous() {
        // The mixed case: `coalesce(a.x, (SELECT …)) AS z` over a SOLE/known
        // source. The top-level visible ref `a.x` IS collected, but the scalar
        // subquery trips the cap, so `a.x` degrades to Ambiguous — never a
        // Resolved fabrication, and NONE of the subquery's internals (`b.*`)
        // are collected.
        let g = graph(
            "WITH a AS (SELECT 1 AS x, 2 AS k), b AS (SELECT 3 AS z, 4 AS k) \
             SELECT coalesce(a.x, (SELECT max(b.z) FROM b WHERE b.k = a.k)) AS z FROM a",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        // a.x is present (visible top-level ref, never dropped)…
        assert!(
            edges
                .iter()
                .any(|&e| source_node(e) == "a" && e.from_col.column == "x"),
            "the visible top-level ref a.x is collected: {:?}",
            edges
                .iter()
                .map(|&e| (source_node(e), e.from_col.column.clone()))
                .collect::<Vec<_>>()
        );
        // …but degraded to Ambiguous by the subquery cap (never Resolved).
        assert!(
            edges
                .iter()
                .all(|e| e.confidence == ColumnEdgeConfidence::Ambiguous),
            "the subquery cap degrades every edge to Ambiguous, never a \
             fabricated Resolved: {:?}",
            edges.iter().map(|e| e.confidence).collect::<Vec<_>>()
        );
        // No subquery internals (b.z / b.k) are ever collected.
        assert!(
            edges.iter().all(|e| source_node(e) != "b"),
            "the subquery's inner refs (b.*) are never collected: {:?}",
            edges
                .iter()
                .map(|&e| (source_node(e), e.from_col.column.clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn column_lineage_derived_special_syntax_exprs_collect_operand() {
        // cute-dbt#449 (CodeRabbit/verifier) — the SECONDARY honest-direction
        // fix. sqlparser models `substring`/`trim`/`extract`/`position` as
        // DISTINCT `Expr` variants (not `Expr::Function`), so their operand
        // refs were silently dropped (no edge). Now each descends, collecting
        // its column operand as a `Derived` edge. Single-source ⇒ Resolved.
        let cases = [
            ("substring(a.x FROM 1 FOR 3)", "y"),
            ("trim(a.x)", "y"),
            ("extract(year FROM a.x)", "y"),
            ("position('z' IN a.x)", "y"),
            // OVERLAY parses to its OWN `Expr::Overlay` variant; the target
            // operand a.x is collected (kills the `delete Expr::Overlay arm`
            // mutant in `expr_children`).
            ("overlay(a.x PLACING 'q' FROM 2)", "y"),
        ];
        for (proj, out_col) in cases {
            let sql = format!("WITH a AS (SELECT 1 AS x) SELECT {proj} AS {out_col} FROM a");
            let g = graph(&sql);
            let edges = edges_to(&g, TERMINAL_NODE_NAME, out_col);
            assert!(
                edges
                    .iter()
                    .any(|&e| source_node(e) == "a" && e.from_col.column == "x"),
                "`{proj}` must collect the operand a.x as a Derived edge: {:?}",
                edges
                    .iter()
                    .map(|&e| (source_node(e), e.from_col.column.clone()))
                    .collect::<Vec<_>>()
            );
            assert!(
                edges.iter().all(|e| e.kind == ColumnEdgeKind::Derived),
                "`{proj}` is a Derived projection"
            );
            assert!(
                edges
                    .iter()
                    .all(|e| e.confidence == ColumnEdgeConfidence::Resolved),
                "`{proj}`'s single-source operand is Resolved"
            );
        }
    }

    #[test]
    fn column_lineage_window_partition_by_collects_ref() {
        // cute-dbt#480: `row_number() OVER (PARTITION BY a.id) AS z`. Before
        // #480 the window spec was never descended, so a.id was silently
        // dropped. Now the PARTITION BY expr's ref is a Resolved Derived edge.
        let g = graph(
            "WITH a AS (SELECT 1 AS id) \
             SELECT row_number() OVER (PARTITION BY a.id) AS z FROM a",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        assert_eq!(edges.len(), 1, "the PARTITION BY ref a.id is collected");
        assert_eq!(edges[0].from_col, ColumnRef::intra("a", "id"));
        assert_eq!(edges[0].kind, ColumnEdgeKind::Derived);
        assert_eq!(
            edges[0].confidence,
            ColumnEdgeConfidence::Resolved,
            "qualified window ref ⇒ Resolved"
        );
    }

    #[test]
    fn column_lineage_window_order_by_collects_ref() {
        // cute-dbt#480: the OVER (… ORDER BY …) ordering exprs also carry
        // real column refs — `lead(a.v) OVER (ORDER BY a.ts)` reads a.ts.
        let g = graph(
            "WITH a AS (SELECT 1 AS v, 2 AS ts) \
             SELECT lead(a.v) OVER (ORDER BY a.ts) AS z FROM a",
        );
        let from: std::collections::BTreeSet<_> = edges_to(&g, TERMINAL_NODE_NAME, "z")
            .iter()
            .map(|&e| (source_node(e), e.from_col.column.clone()))
            .collect();
        assert_eq!(
            from,
            [
                ("a".to_owned(), "v".to_owned()),
                ("a".to_owned(), "ts".to_owned()),
            ]
            .into_iter()
            .collect(),
            "the function arg a.v AND the ORDER BY ref a.ts both contribute"
        );
    }

    #[test]
    fn column_lineage_filter_where_collects_ref() {
        // cute-dbt#480: `sum(a.x) FILTER (WHERE b.flag) AS z` reads BOTH the
        // aggregate arg a.x AND the FILTER predicate column b.flag. Before
        // #480 b.flag was silently dropped.
        let g = graph(
            "WITH a AS (SELECT 1 AS x), b AS (SELECT 2 AS flag) \
             SELECT sum(a.x) FILTER (WHERE b.flag > 0) AS z FROM a, b",
        );
        let from: std::collections::BTreeSet<_> = edges_to(&g, TERMINAL_NODE_NAME, "z")
            .iter()
            .map(|&e| (source_node(e), e.from_col.column.clone()))
            .collect();
        assert_eq!(
            from,
            [
                ("a".to_owned(), "x".to_owned()),
                ("b".to_owned(), "flag".to_owned()),
            ]
            .into_iter()
            .collect(),
            "the aggregate arg a.x AND the FILTER predicate b.flag both contribute"
        );
        assert!(
            edges_to(&g, TERMINAL_NODE_NAME, "z")
                .iter()
                .all(|e| e.kind == ColumnEdgeKind::Derived),
            "a FILTER-bearing aggregate projection is Derived"
        );
    }

    #[test]
    fn column_lineage_within_group_collects_ref() {
        // cute-dbt#480: an ordered-set aggregate's WITHIN GROUP (ORDER BY …)
        // ordering keys are real inputs — `percentile_cont(0.5) WITHIN GROUP
        // (ORDER BY a.x)` reads a.x.
        let g = graph(
            "WITH a AS (SELECT 1 AS x) \
             SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY a.x) AS z FROM a",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        assert!(
            edges
                .iter()
                .any(|&e| source_node(e) == "a" && e.from_col.column == "x"),
            "the WITHIN GROUP ordering key a.x is collected: {:?}",
            edges
                .iter()
                .map(|&e| (source_node(e), e.from_col.column.clone()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            edges
                .iter()
                .find(|e| e.from_col.column == "x")
                .unwrap()
                .kind,
            ColumnEdgeKind::Derived
        );
    }

    #[test]
    fn column_lineage_window_bare_ref_under_multi_source_is_ambiguous() {
        // cute-dbt#480: the honesty floor holds inside window clauses too. A
        // BARE ref in PARTITION BY under a multi-relation FROM fans out to
        // every candidate, all Ambiguous — never a wrong Resolved, never a
        // silent drop.
        let g = graph(
            "WITH a AS (SELECT 1 AS grp), b AS (SELECT 2 AS other) \
             SELECT row_number() OVER (PARTITION BY grp) AS z FROM a, b",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        assert_eq!(edges.len(), 2, "bare window ref fans out to both sources");
        assert!(
            edges
                .iter()
                .all(|e| e.confidence == ColumnEdgeConfidence::Ambiguous),
            "a bare/ambiguous window ref degrades to Ambiguous, never a wrong Resolved"
        );
        let sources: std::collections::BTreeSet<_> =
            edges.iter().map(|&e| source_node(e)).collect();
        assert_eq!(
            sources,
            ["a".to_owned(), "b".to_owned()].into_iter().collect()
        );
    }

    #[test]
    fn column_lineage_named_window_reference_contributes_nothing() {
        // cute-dbt#480: a bare named-window reference (`OVER w`, defined by a
        // `WINDOW` clause elsewhere) carries no inline exprs here — honest
        // absence, never a fabricated edge. The function arg a.v still resolves.
        let g = graph(
            "WITH a AS (SELECT 1 AS v, 2 AS p) \
             SELECT sum(a.v) OVER w AS z FROM a WINDOW w AS (PARTITION BY a.p)",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        // Only the direct arg a.v contributes here; the named window's own
        // PARTITION BY (a.p) lives on the WINDOW definition, not this call site.
        assert!(
            edges
                .iter()
                .any(|&e| source_node(e) == "a" && e.from_col.column == "v"),
            "the function arg a.v still resolves: {:?}",
            edges
                .iter()
                .map(|&e| (source_node(e), e.from_col.column.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            edges.iter().all(|e| e.from_col.column != "p"),
            "a bare named-window reference fabricates no edge from the window def: {:?}",
            edges
                .iter()
                .map(|&e| (source_node(e), e.from_col.column.clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn column_lineage_complexity_cap_exists_degrades_to_ambiguous_never_panics() {
        // The issue's canonical complexity trigger: `CASE WHEN EXISTS (…)`.
        // An EXISTS subquery is a shape the walker deliberately does NOT model
        // semantically — it trips the cap, so the whole column's visible refs
        // degrade to Ambiguous. Never a panic, never a fabricated Resolved.
        let g = graph(
            "WITH a AS (SELECT 1 AS x), b AS (SELECT 2 AS bid) \
             SELECT CASE WHEN EXISTS (SELECT 1 FROM b WHERE b.bid = a.x) \
                    THEN a.x ELSE 0 END AS z FROM a, b",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        // a.x is still VISIBLE (the THEN branch) so it is never dropped.
        assert!(!edges.is_empty(), "the cap NEVER drops everything visible");
        assert!(
            edges.iter().all(|e| e.kind == ColumnEdgeKind::Derived),
            "still Derived"
        );
        assert!(
            edges
                .iter()
                .all(|e| e.confidence == ColumnEdgeConfidence::Ambiguous),
            "an EXISTS-bearing expression degrades every edge to Ambiguous, \
             never a fabricated Resolved: {:?}",
            edges.iter().map(|e| e.confidence).collect::<Vec<_>>()
        );
        // The visible THEN ref is still present (honest absence floor).
        assert!(
            edges
                .iter()
                .any(|&e| source_node(e) == "a" && e.from_col.column == "x"),
            "the visible THEN ref a.x is listed, never silently dropped"
        );
    }

    #[test]
    fn collect_derived_refs_depth_cap_triggers_without_panic() {
        // Defense-in-depth: a synthetically deep AST (built directly, past the
        // descent cap) trips `capped` and STOPS recursing — no stack overflow,
        // no panic — while keeping whatever it saw above the cap. This guards
        // the stack even if the parser's own recursion limit changes; in
        // practice sqlparser fails-closed at its 50-deep parse limit first
        // (see open_concerns), so this is the belt to that suspenders.
        let mut expr = Expr::CompoundIdentifier(vec![
            sqlparser::ast::Ident::new("a"),
            sqlparser::ast::Ident::new("x"),
        ]);
        // Wrap DERIVED_DEPTH_CAP + 10 levels of Nested(...) around it.
        for _ in 0..(DERIVED_DEPTH_CAP + 10) {
            expr = Expr::Nested(Box::new(expr));
        }
        let collected = collect_derived_refs(&expr); // must not overflow/panic
        assert!(
            collected.capped,
            "an AST past the depth cap sets the capped flag"
        );
        // Resolving it degrades to Ambiguous, never Resolved.
        let mut aliases = HashMap::new();
        aliases.insert("a".to_owned(), "a".to_owned());
        let cols = derived_input_cols(collected, &aliases, None);
        assert!(
            cols.iter()
                .all(|c| c.confidence == ColumnEdgeConfidence::Ambiguous),
            "a capped collection degrades to Ambiguous, never a wrong Resolved"
        );
    }

    #[test]
    fn collect_derived_refs_ref_cap_degrades_wide_expression() {
        // A pathologically WIDE but SHALLOW expression (one function, more
        // args than DERIVED_REF_CAP) is a complexity signal that trips
        // `capped` via the ref cap (not the depth cap). Built directly.
        let args: Vec<sqlparser::ast::FunctionArg> = (0..(DERIVED_REF_CAP + 5))
            .map(|i| {
                sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(
                    Expr::CompoundIdentifier(vec![
                        sqlparser::ast::Ident::new("a"),
                        sqlparser::ast::Ident::new(format!("c{i}")),
                    ]),
                ))
            })
            .collect();
        let expr = Expr::Function(sqlparser::ast::Function {
            name: sqlparser::ast::ObjectName(vec![sqlparser::ast::ObjectNamePart::Identifier(
                sqlparser::ast::Ident::new("coalesce"),
            )]),
            uses_odbc_syntax: false,
            parameters: sqlparser::ast::FunctionArguments::None,
            args: sqlparser::ast::FunctionArguments::List(sqlparser::ast::FunctionArgumentList {
                duplicate_treatment: None,
                args,
                clauses: Vec::new(),
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: Vec::new(),
        });
        let collected = collect_derived_refs(&expr);
        assert!(collected.capped, "more refs than the ref cap sets capped");
    }

    /// `a.x` wrapped in `n` levels of `Nested(...)` — a leaf at AST depth `n`.
    fn nested_to_depth(n: u32) -> Expr {
        let mut e = Expr::CompoundIdentifier(vec![
            sqlparser::ast::Ident::new("a"),
            sqlparser::ast::Ident::new("x"),
        ]);
        for _ in 0..n {
            e = Expr::Nested(Box::new(e));
        }
        e
    }

    #[test]
    fn collect_derived_refs_depth_cap_boundary_is_exact() {
        // Boundary precision (kills off-by-one cap mutants `> -> >=` / `> -> ==`):
        // a leaf reached at depth == DERIVED_DEPTH_CAP is the LAST level that is
        // NOT capped (the check is `depth > DERIVED_DEPTH_CAP`). One level
        // deeper IS capped. The walk increments depth by exactly one per level.
        let at_cap = collect_derived_refs(&nested_to_depth(DERIVED_DEPTH_CAP));
        assert!(
            !at_cap.capped,
            "a leaf at exactly the cap depth is NOT capped"
        );
        assert_eq!(at_cap.refs.len(), 1, "and its ref IS collected");

        let past_cap = collect_derived_refs(&nested_to_depth(DERIVED_DEPTH_CAP + 1));
        assert!(
            past_cap.capped,
            "one level past the cap depth IS capped (no leaf collected)"
        );
        assert!(
            past_cap.refs.is_empty(),
            "the buried leaf past the cap is not reached"
        );
    }

    #[test]
    fn collect_derived_refs_ref_cap_boundary_is_exact() {
        // Boundary precision for the ref cap (`refs.len() > DERIVED_REF_CAP`,
        // kills `> -> ==` at 647:52): collecting exactly DERIVED_REF_CAP + 1
        // refs does NOT cap (the last leaf is reached when len == REF_CAP, which
        // is not `> REF_CAP`); collecting one more DOES cap on the next entry.
        // Built as a flat function arg list (shallow, so only the ref cap can
        // fire, never the depth cap).
        let make = |count: usize| {
            let args: Vec<sqlparser::ast::FunctionArg> = (0..count)
                .map(|i| {
                    sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(
                        Expr::CompoundIdentifier(vec![
                            sqlparser::ast::Ident::new("a"),
                            sqlparser::ast::Ident::new(format!("c{i}")),
                        ]),
                    ))
                })
                .collect();
            Expr::Function(sqlparser::ast::Function {
                name: sqlparser::ast::ObjectName(vec![sqlparser::ast::ObjectNamePart::Identifier(
                    sqlparser::ast::Ident::new("coalesce"),
                )]),
                uses_odbc_syntax: false,
                parameters: sqlparser::ast::FunctionArguments::None,
                args: sqlparser::ast::FunctionArguments::List(
                    sqlparser::ast::FunctionArgumentList {
                        duplicate_treatment: None,
                        args,
                        clauses: Vec::new(),
                    },
                ),
                filter: None,
                null_treatment: None,
                over: None,
                within_group: Vec::new(),
            })
        };
        // Exactly REF_CAP + 1 leaves: the last leaf is entered with
        // len == REF_CAP, which is NOT `> REF_CAP`, so it is collected and the
        // walk never trips `capped`.
        let at_cap = collect_derived_refs(&make(DERIVED_REF_CAP + 1));
        assert!(
            !at_cap.capped,
            "exactly REF_CAP + 1 refs does not trip the ref cap"
        );
        assert_eq!(at_cap.refs.len(), DERIVED_REF_CAP + 1);
        // One more leaf: the (REF_CAP + 2)-th entry sees len == REF_CAP + 1,
        // which IS `> REF_CAP`, so it caps.
        let past_cap = collect_derived_refs(&make(DERIVED_REF_CAP + 2));
        assert!(past_cap.capped, "REF_CAP + 2 refs trips the ref cap");
    }

    #[test]
    fn collect_qualified_refs_descends_into_function_args() {
        // The shared correlation walker now descends Function/Case/Subquery —
        // a qualified ref hidden inside a function is FOUND (strictly more
        // correlation evidence, never less). Bare identifiers stay invisible
        // to this walker (they carry no qualifier — correlation needs one).
        let expr = parse_expr("coalesce(o.id, lower(c.name))");
        let mut refs = Vec::new();
        collect_qualified_refs(&expr, &mut refs);
        let got: std::collections::BTreeSet<_> = refs.into_iter().collect();
        assert_eq!(
            got,
            [
                ("o".to_owned(), "id".to_owned()),
                ("c".to_owned(), "name".to_owned())
            ]
            .into_iter()
            .collect(),
            "qualified refs nested inside functions are now collected"
        );
    }

    #[test]
    fn collect_qualified_refs_descends_into_case_and_subquery() {
        let expr = parse_expr(
            "CASE WHEN o.flag THEN o.a ELSE (SELECT max(r.b) FROM r WHERE r.k = o.k) END",
        );
        let mut refs = Vec::new();
        collect_qualified_refs(&expr, &mut refs);
        let got: std::collections::BTreeSet<_> = refs.into_iter().collect();
        assert!(
            got.contains(&("o".to_owned(), "flag".to_owned()))
                && got.contains(&("o".to_owned(), "a".to_owned()))
                && got.contains(&("o".to_owned(), "k".to_owned())),
            "CASE branches + subquery correlation refs are collected: {got:?}"
        );
    }

    #[test]
    fn column_lineage_derived_same_source_keeps_weaker_confidence() {
        // Honesty floor through `merge_max` (kills the `> -> ==` mutant at
        // the rank comparison): when the SAME `(source, column)` is reached
        // once qualified (Resolved) AND once via an unqualified multi-source
        // fan-out (Ambiguous), the edge MUST end Ambiguous — never silently
        // upgraded back to a false Resolved. `coalesce(a.x, x)` over a
        // multi-relation FROM reaches `(a, x)` both ways.
        let g = graph(
            "WITH a AS (SELECT 1 AS x), b AS (SELECT 2 AS y) \
             SELECT coalesce(a.x, x) AS z FROM a, b",
        );
        let edges = edges_to(&g, TERMINAL_NODE_NAME, "z");
        let a_x: Vec<_> = edges
            .iter()
            .filter(|e| source_node(e) == "a" && e.from_col.column == "x")
            .collect();
        assert_eq!(a_x.len(), 1, "(a, x) is merged to a single edge");
        assert_eq!(
            a_x[0].confidence,
            ColumnEdgeConfidence::Ambiguous,
            "the weaker Ambiguous claim wins — never upgraded to a false Resolved"
        );
    }

    #[test]
    fn column_lineage_downstream_reverse_index() {
        // B (downstream impact): a reverse index over the SAME edge set — no
        // new parse. Changing `customers.email` reaches `contact_email`.
        let g = graph(
            "WITH customers AS (SELECT 1 AS email) \
             SELECT c.email AS contact_email FROM customers c",
        );
        let downstream: Vec<_> = g
            .column_edges()
            .iter()
            .filter(|e| e.from_col == ColumnRef::intra("customers", "email"))
            .map(|e| e.to_col.clone())
            .collect();
        assert_eq!(
            downstream,
            vec![ColumnRef::intra(TERMINAL_NODE_NAME, "contact_email")]
        );
    }

    #[test]
    fn column_lineage_upstream_trace_dead_ends_at_ref_boundary() {
        // C (intra-model upstream trace): walk an output column backward over
        // the edge set; it dead-ends at the first leaf-table boundary (the
        // `ref()` leaf has no intra-model upstream edge).
        let g = graph(
            "WITH stg AS (SELECT id FROM raw_source) \
             SELECT s.id AS id FROM stg s",
        );
        // terminal.id <- stg.id
        let up1: Vec<_> = g
            .column_edges()
            .iter()
            .filter(|e| e.to_col == ColumnRef::intra(TERMINAL_NODE_NAME, "id"))
            .map(|e| e.from_col.clone())
            .collect();
        assert_eq!(up1, vec![ColumnRef::intra("stg", "id")]);
        // stg.id <- raw_source.id (the leaf boundary). No edge whose source is
        // raw_source.id (it is outside the model — dead end).
        let beyond_leaf: Vec<_> = g
            .column_edges()
            .iter()
            .filter(|e| e.to_col == ColumnRef::intra("raw_source", "id"))
            .collect();
        assert!(
            beyond_leaf.is_empty(),
            "upstream trace dead-ends at the ref boundary"
        );
    }

    #[test]
    fn column_spans_are_subranges_of_owning_cte_body() {
        // The `SpanRole::Column` span MUST be contained within the owning
        // CteBody node's span (contains_range), so the column anchor resolves
        // under the node anchor.
        let sql = "WITH customers AS (SELECT 1 AS email) \
                   SELECT c.email AS contact_email FROM customers c";
        let g = graph(sql);
        let term_span = g
            .nodes()
            .iter()
            .find(|n| n.name() == TERMINAL_NODE_NAME)
            .and_then(|n| n.source_span())
            .copied()
            .expect("terminal node has a span");
        let col = g
            .column_spans()
            .iter()
            .find(|cs| cs.node_id == TERMINAL_NODE_NAME && cs.column == "contact_email")
            .expect("contact_email has a column span");
        assert!(
            term_span.contains_range(&col.span),
            "column span {:?} must be a sub-range of the owning CteBody {:?}",
            col.span,
            term_span
        );
        // And the span byte-slices to the projection-item text.
        let slice = &sql[col.span.start.byte as usize..col.span.end.byte as usize];
        assert_eq!(slice, "c.email AS contact_email");
    }

    #[test]
    fn degraded_spans_are_dropped_not_fabricated() {
        // The degrade-not-lie boundary guard in `span_to_source_span` (the
        // column-span path, distinct from `slice_or_fallback`'s node-span
        // path): an empty (line-0 sentinel) or inverted byte range yields NO
        // span — never a fabricated one that would mis-anchor the column→code
        // sync. Kills the boundary-guard mutant on this strict-CRAP module by
        // driving each REACHABLE degrade path plus a valid control. (The
        // `end > sql.len()` clause is a belt-and-braces guard: `byte_of` itself
        // already clamps every offset to `sql.len()`, so it is unreachable
        // through this caller — exercised here only via the valid control's
        // upper-bound equality.)
        let sql = "select a from t"; // 15 bytes, single line
        let index = ByteIndex::new(sql);
        let loc = |line, col| Location::new(line, col);

        // Line-0 sentinel on either endpoint ⇒ dropped.
        assert!(
            span_to_source_span(Span::new(loc(0, 1), loc(1, 4)), sql, &index).is_none(),
            "a line-0 start (sqlparser's no-span) is dropped"
        );
        assert!(
            span_to_source_span(Span::new(loc(1, 1), loc(0, 1)), sql, &index).is_none(),
            "a line-0 end is dropped"
        );

        // Inverted byte range (start > end) ⇒ dropped. col 10 > col 2 on the
        // same line, so byte(start) > byte(end).
        assert!(
            span_to_source_span(Span::new(loc(1, 10), loc(1, 2)), sql, &index).is_none(),
            "an inverted byte range is dropped"
        );

        // Control: a valid in-bounds, ordered span IS retained and byte-slices
        // back to the source — proving the guard rejects only the degrades, and
        // that a span ending exactly at sql.len() is NOT dropped (the
        // `end > sql.len()` clause is `>`, not `>=`).
        let ok = span_to_source_span(Span::new(loc(1, 1), loc(1, 16)), sql, &index)
            .expect("a valid span is retained");
        let slice = &sql[ok.start.byte as usize..ok.end.byte as usize];
        assert_eq!(
            slice, "select a from t",
            "the retained span (ending at sql.len()) slices to the whole source"
        );
        assert_eq!(
            ok.end.byte as usize,
            sql.len(),
            "an end exactly at sql.len() is retained (guard is `>`, not `>=`)"
        );
    }

    #[test]
    fn column_lineage_canonical_vocab_is_stable_snake_case() {
        // The Recce 5-way vocabulary serializes to its stable wire strings —
        // goldens depend on these never drifting.
        assert_eq!(
            serde_json::to_string(&ColumnEdgeKind::PassThrough).unwrap(),
            "\"pass_through\""
        );
        assert_eq!(
            serde_json::to_string(&ColumnEdgeKind::Renamed).unwrap(),
            "\"renamed\""
        );
        assert_eq!(
            serde_json::to_string(&ColumnEdgeKind::Derived).unwrap(),
            "\"derived\""
        );
        assert_eq!(
            serde_json::to_string(&ColumnEdgeKind::Source).unwrap(),
            "\"source\""
        );
        assert_eq!(
            serde_json::to_string(&ColumnEdgeKind::JoinKey).unwrap(),
            "\"join_key\""
        );
        assert_eq!(
            serde_json::to_string(&ColumnEdgeConfidence::Resolved).unwrap(),
            "\"resolved\""
        );
        assert_eq!(
            serde_json::to_string(&ColumnEdgeConfidence::Ambiguous).unwrap(),
            "\"ambiguous\""
        );
        assert_eq!(
            serde_json::to_string(&ColumnEdgeConfidence::Opaque).unwrap(),
            "\"opaque\""
        );
    }
}
