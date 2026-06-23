//! `CteGraph` + `CteNode` + `CteEdge` + `EdgeType` — the AST output the
//! sqlparser CTE engine (PR 7) produces and the renderer (PR 8b)
//! consumes.
//!
//! Edges store endpoints as `usize` indices into the `nodes` vector —
//! the renderer needs a stable iteration order and Mermaid `graph LR`
//! syntax is index-friendly. The constructor takes ownership of the
//! `nodes` vector exactly once so indices remain valid for the lifetime
//! of the `CteGraph`.
//!
//! `EdgeType` is `#[non_exhaustive]` per the
//! [enums-yes-structs-no rule](https://github.com/cmbays/.claude/blob/main/rules/non-exhaustive.md):
//! consumers pattern-match this and new SQL structural kinds (e.g.
//! `LATERAL`) are additive.

use serde::{Deserialize, Serialize};

use crate::domain::manifest::NodeId;
use crate::domain::span::SourceSpan;

/// SQL edge kind classified by the CTE engine.
///
/// Covers all structural relationships that can appear between CTEs:
/// plain `FROM` references, the five join types, and the two UNION
/// variants. `#[non_exhaustive]` — adding a dialect-specific variant is
/// a v0.x additive change that consumers must opt into via `_` arms.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    /// Plain `FROM <cte>` reference (no join operator).
    From,
    /// `INNER JOIN`.
    Inner,
    /// `LEFT [OUTER] JOIN`.
    Left,
    /// `RIGHT [OUTER] JOIN`.
    Right,
    /// `FULL [OUTER] JOIN`.
    Full,
    /// `CROSS JOIN` / Cartesian product.
    Cross,
    /// `UNION ALL` arm reference.
    UnionAll,
    /// `UNION` / `UNION DISTINCT` arm reference.
    UnionDistinct,
}

/// One equi-join key pair recovered from a LEFT JOIN's `ON` clause —
/// `<left qualifier>.<left_column> = <right qualifier>.<right_column>`
/// where the right qualifier names the LEFT-JOINed relation
/// (cute-dbt#173, catalog class C4).
///
/// All identifiers are lowercased (SQL identifiers are
/// case-insensitive). `left_leaf` is the lowercased leaf name of the
/// relation the left-side qualifier resolved to within the same
/// `SELECT` (a CTE name or an external table leaf); `None` when the
/// qualifier did not resolve to a plain named table factor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinKeyPair {
    left_leaf: Option<String>,
    left_column: String,
    right_column: String,
}

impl JoinKeyPair {
    /// Canonical constructor.
    #[must_use]
    pub fn new(
        left_leaf: Option<String>,
        left_column: impl Into<String>,
        right_column: impl Into<String>,
    ) -> Self {
        Self {
            left_leaf,
            left_column: left_column.into(),
            right_column: right_column.into(),
        }
    }

    /// Lowercased leaf name of the relation the left-side qualifier
    /// resolved to, or `None` when not statically resolvable.
    #[must_use]
    pub fn left_leaf(&self) -> Option<&str> {
        self.left_leaf.as_deref()
    }

    /// Lowercased left-side key column.
    #[must_use]
    pub fn left_column(&self) -> &str {
        &self.left_column
    }

    /// Lowercased right-side key column (qualified by the LEFT-JOINed
    /// relation in the source SQL).
    #[must_use]
    pub fn right_column(&self) -> &str {
        &self.right_column
    }
}

/// Engine-computed structural facts about one `LEFT [OUTER] JOIN` in
/// one query body — the cute-dbt#40 additive-facts pattern extended
/// with the where-predicate fact the anti-join check needs
/// (cute-dbt#173).
///
/// Computed during the engine's existing single AST-parse pass — never
/// a second parse. Facts hang off the [`CteGraph`] (tagged with their
/// `consumer` body's node name) rather than off [`CteNode`]s, so a
/// model with **no** `WITH` clause — whose graph carries zero nodes —
/// still surfaces its LEFT JOINs (the catalog C4 canonical shape).
/// They are **render-pass internal** (consumed by the domain check
/// detectors only): the field is `#[serde(skip)]` on [`CteGraph`], so
/// the embedded report payload is byte-identical to the pre-#173 shape.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LeftJoinFact {
    consumer: String,
    right_leaf: String,
    equi_keys: Vec<JoinKeyPair>,
    where_is_null_columns: Vec<String>,
    projects_right_columns: bool,
    select_is_distinct: bool,
}

impl LeftJoinFact {
    /// Canonical constructor.
    #[must_use]
    pub fn new(
        consumer: impl Into<String>,
        right_leaf: impl Into<String>,
        equi_keys: Vec<JoinKeyPair>,
        where_is_null_columns: Vec<String>,
        projects_right_columns: bool,
        select_is_distinct: bool,
    ) -> Self {
        Self {
            consumer: consumer.into(),
            right_leaf: right_leaf.into(),
            equi_keys,
            where_is_null_columns,
            projects_right_columns,
            select_is_distinct,
        }
    }

    /// Name of the body the join appears in — a CTE name, or the
    /// engine's terminal-node name for the final `SELECT` (also the
    /// name used when the model has no `WITH` clause at all).
    #[must_use]
    pub fn consumer(&self) -> &str {
        &self.consumer
    }

    /// Lowercased leaf name of the LEFT-JOINed (right-side) relation.
    #[must_use]
    pub fn right_leaf(&self) -> &str {
        &self.right_leaf
    }

    /// Equi-join key pairs recovered from the `ON` clause's top-level
    /// `AND` conjuncts. Empty when the join key is not statically
    /// recoverable (non-equi predicates, expressions, `USING`,
    /// unqualified columns).
    #[must_use]
    pub fn equi_keys(&self) -> &[JoinKeyPair] {
        &self.equi_keys
    }

    /// Lowercased right-qualified columns appearing under `IS NULL` in
    /// the containing `SELECT`'s top-level `AND`ed `WHERE` conjuncts —
    /// the anti-join where-predicate fact. An `IS NULL` inside an `OR`
    /// (different semantics) or on an unqualified column (not
    /// attributable) is never recorded.
    #[must_use]
    pub fn where_is_null_columns(&self) -> &[String] {
        &self.where_is_null_columns
    }

    /// `true` when the containing `SELECT`'s projection **provably**
    /// carries right-side columns: a bare `*`, a `<right>.*` qualified
    /// wildcard, or a direct `<right>.<column>` reference. Right-side
    /// columns reaching the output only through expressions (e.g.
    /// wrapped in `COALESCE`) do not set this — the conservative,
    /// never-false-fire direction.
    #[must_use]
    pub fn projects_right_columns(&self) -> bool {
        self.projects_right_columns
    }

    /// `true` when the containing `SELECT` dedups its output
    /// (`SELECT DISTINCT` / `DISTINCT ON`) — the dedup-after-fan-out
    /// signal the instrument routing keys off (catalog C4/C10).
    #[must_use]
    pub fn select_is_distinct(&self) -> bool {
        self.select_is_distinct
    }
}

/// Which negated-subquery construct a [`SubqueryFact`] describes
/// (cute-dbt#196 — the correlated-subquery evidence family, v1).
///
/// `#[non_exhaustive]` per the enums-yes-structs-no rule: future
/// consumers (non-negated `EXISTS` semi-joins, `IN` membership, scalar
/// aggregates) arrive as additive variants — never extracted ahead of a
/// consumer.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubqueryKind {
    /// `WHERE NOT EXISTS (SELECT … FROM <inner> WHERE <correlation>)`.
    NotExists,
    /// `WHERE <col> NOT IN (SELECT <col> FROM <inner>)`.
    NotIn,
}

/// Engine-computed structural facts about one negated subquery in one
/// query body's top-level `WHERE` conjuncts — the cute-dbt#196
/// evidence family that lifts the cute-dbt#173 NOT EXISTS / NOT IN
/// anti-join exclusions.
///
/// Computed during the engine's existing single AST-parse pass — never
/// a second parse. The sibling of [`LeftJoinFact`] (the #191/#40
/// additive-facts pattern): facts hang off the [`CteGraph`] tagged with
/// their `consumer` body's node name, are **render-pass internal**
/// (`#[serde(skip)]` on [`CteGraph`]), and reuse the [`JoinKeyPair`]
/// key vocabulary — the OUTER side is the pair's "left", the inner
/// relation plays the LEFT JOIN's `right_leaf` role. Empty `equi_keys`
/// is the unrecoverable-key shape (downstream key binding fails and the
/// verdict degrades to honest UNKNOWN, mirroring the LEFT JOIN
/// degrade).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubqueryFact {
    kind: SubqueryKind,
    consumer: String,
    inner_leaf: String,
    equi_keys: Vec<JoinKeyPair>,
}

impl SubqueryFact {
    /// Canonical constructor.
    #[must_use]
    pub fn new(
        kind: SubqueryKind,
        consumer: impl Into<String>,
        inner_leaf: impl Into<String>,
        equi_keys: Vec<JoinKeyPair>,
    ) -> Self {
        Self {
            kind,
            consumer: consumer.into(),
            inner_leaf: inner_leaf.into(),
            equi_keys,
        }
    }

    /// Which negated-subquery construct this fact describes.
    #[must_use]
    pub fn kind(&self) -> SubqueryKind {
        self.kind
    }

    /// Name of the body the subquery's outer `SELECT` appears in — a
    /// CTE name, or the engine's terminal-node name for the final
    /// `SELECT` (also the name used when the model has no `WITH` clause
    /// at all).
    #[must_use]
    pub fn consumer(&self) -> &str {
        &self.consumer
    }

    /// Lowercased leaf name of the subquery's single plain named inner
    /// relation (the LEFT JOIN `right_leaf` analogue).
    #[must_use]
    pub fn inner_leaf(&self) -> &str {
        &self.inner_leaf
    }

    /// Outer↔inner key pairs, normalized so the OUTER side is the
    /// pair's "left" and the inner column the "right": the resolvable
    /// inner-`WHERE` equi-conjuncts for `NOT EXISTS`, or the single
    /// membership pair (outer column ↔ inner projected column) for
    /// `NOT IN`. Empty when the key is not statically recoverable —
    /// the honest-UNKNOWN degrade.
    #[must_use]
    pub fn equi_keys(&self) -> &[JoinKeyPair] {
        &self.equi_keys
    }
}

/// A node in the CTE dependency DAG — one `WITH name AS (...)` block.
///
/// `desc` is reserved for a future `-- @desc <text>` per-CTE comment
/// pass (deferred to v0.2 per ADR); v0.1 always emits `None`.
///
/// `is_simple_from_shape` and `body_leaf_table_refs` are structural
/// facts about the CTE body, populated by the CTE engine during the
/// existing single AST-parse pass and consumed by the renderer for
/// node-role classification and import-CTE body-match (cute-dbt#40).
/// The renderer never re-parses the slice; the engine is the single
/// source of truth for AST-derived structural facts. Defaults are
/// `false` and empty — a `CteNode` constructed without facts
/// classifies as `Transform`, the safer default. New facts of this
/// kind are additive POD fields with `#[serde(default)]`; no domain
/// layer ever pulls in `sqlparser`.
///
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CteNode {
    name: String,
    /// The retained byte/line range of this node's slice within the
    /// model's compiled SQL — the `name AS ( … )` extent for a CTE body,
    /// or the post-trim terminal-`SELECT` extent for the terminal node
    /// (cute-dbt#444). `None` when the engine could not soundly locate the
    /// span (empty / out-of-bounds → the same fallback path `raw_sql`
    /// degrades through). A FACT computed by the CTE engine during the
    /// single AST-parse pass, written back to this POD (the cute-dbt#40
    /// pattern); the domain never pulls in `sqlparser`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_span: Option<SourceSpan>,
    #[serde(default)]
    raw_sql: Option<String>,
    #[serde(default)]
    desc: Option<String>,
    /// `true` when the CTE body is a single `SELECT … FROM <relation>`
    /// with no joins and exactly one source — the import-CTE shape.
    /// Computed by the engine from the parsed AST; defaults to `false`.
    #[serde(default)]
    is_simple_from_shape: bool,
    /// Lowercased leaf table identifiers appearing in `FROM` / `JOIN`
    /// clauses of the CTE body. Computed by the engine from the parsed
    /// AST; defaults to empty.
    #[serde(default)]
    body_leaf_table_refs: Vec<String>,
}

impl CteNode {
    /// Canonical constructor.
    ///
    /// `source_span` is the retained compiled-SQL byte/line range of this
    /// node's slice (cute-dbt#444), or `None` when the engine could not
    /// soundly locate it. `is_simple_from_shape` defaults to `false` and
    /// `body_leaf_table_refs` to empty. Use [`Self::with_shape_facts`] to
    /// attach engine-computed structural facts.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        source_span: Option<SourceSpan>,
        raw_sql: Option<String>,
        desc: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            source_span,
            raw_sql,
            desc,
            is_simple_from_shape: false,
            body_leaf_table_refs: Vec::new(),
        }
    }

    /// Attach engine-computed structural facts about the CTE body.
    ///
    /// Returns `self` with `is_simple_from_shape` and
    /// `body_leaf_table_refs` set. Called by the CTE engine during
    /// `build_nodes` from the parsed AST.
    #[must_use]
    pub fn with_shape_facts(
        mut self,
        is_simple_from_shape: bool,
        body_leaf_table_refs: Vec<String>,
    ) -> Self {
        self.is_simple_from_shape = is_simple_from_shape;
        self.body_leaf_table_refs = body_leaf_table_refs;
        self
    }

    /// CTE name as declared in `WITH <name> AS (...)`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The retained compiled-SQL byte/line range of this node's slice —
    /// the `name AS ( … )` extent for a CTE body, the post-trim terminal-
    /// `SELECT` extent for the terminal node (cute-dbt#444). `None` when
    /// the engine could not soundly locate the span (the same fallback the
    /// raw-SQL slice degrades through). The `compiled[span.byte_range()]`
    /// slice byte-equals [`Self::raw_sql`] by construction.
    #[must_use]
    pub fn source_span(&self) -> Option<&SourceSpan> {
        self.source_span.as_ref()
    }

    /// Raw SQL body of the CTE.
    #[must_use]
    pub fn raw_sql(&self) -> Option<&str> {
        self.raw_sql.as_deref()
    }

    /// `-- @desc` comment (v0.2 feature); always `None` in v0.1.
    #[must_use]
    pub fn desc(&self) -> Option<&str> {
        self.desc.as_deref()
    }

    /// `true` when the engine classified the CTE body as a single
    /// `SELECT … FROM <relation>` with no joins (the import-CTE shape).
    /// `false` for transform-shaped bodies and for nodes constructed
    /// without engine-computed facts.
    #[must_use]
    pub fn is_simple_from_shape(&self) -> bool {
        self.is_simple_from_shape
    }

    /// Lowercased leaf table identifiers appearing in `FROM` / `JOIN`
    /// clauses of the CTE body, in source order. Empty when the engine
    /// found no table-factor references or the node was constructed
    /// without engine-computed facts.
    #[must_use]
    pub fn body_leaf_table_refs(&self) -> &[String] {
        &self.body_leaf_table_refs
    }
}

/// A directed edge between two CTE nodes in [`CteGraph`].
///
/// `from` and `to` are indices into the parent `CteGraph::nodes` vector;
/// `edge_type` classifies the SQL relationship the edge represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CteEdge {
    from: usize,
    to: usize,
    edge_type: EdgeType,
}

impl CteEdge {
    /// Canonical constructor.
    #[must_use]
    pub fn new(from: usize, to: usize, edge_type: EdgeType) -> Self {
        Self {
            from,
            to,
            edge_type,
        }
    }

    /// Index of the upstream (referenced) CTE in `CteGraph::nodes`.
    #[must_use]
    pub fn from(&self) -> usize {
        self.from
    }

    /// Index of the downstream (referencing) CTE in `CteGraph::nodes`.
    #[must_use]
    pub fn to(&self) -> usize {
        self.to
    }

    /// SQL edge kind classified by the CTE engine.
    #[must_use]
    pub fn edge_type(&self) -> EdgeType {
        self.edge_type
    }
}

// ---------------------------------------------------------------------
// Intra-model column lineage (cute-dbt#447, CLL-2).
//
// Domain POD facts — std + serde only, written BACK by the CTE engine
// (the cute-dbt#40 pattern; `sqlparser` is forbidden in `src/domain/` by
// `tests/domain_clean_arch.rs`). The projection-provenance AST walk lives
// in `cte_engine.rs` and rides the EXISTING single parse; it emits these
// PODs the renderer/context layer then projects. Mirrors Fusion's open
// `CllEdge { from_node, from_col, to_node, to_col, op }` flat directed-edge
// shape, but with cute-dbt's honest `confidence` axis the warehouse-backed
// Fusion engine does not need.
// ---------------------------------------------------------------------

/// Where a column lives. A [`ColumnRef`] is keyed by a TYPED node identity,
/// never a bare `String` (v2 keys every span by a typed node id — a
/// `SourceMapEntry`'s `node_id` is a `dag.nodes[].id`/[`NodeId`], not an
/// untyped string). The two cases are distinct variants so the field can
/// never silently mean two things:
///   - `Intra` — a CTE/terminal node *within* one model's DAG (a
///     `dag.nodes[].id`, i.e. a CTE alias or `TERMINAL_NODE_NAME`).
///   - `Cross` — an upstream MODEL, for the Tier-3 cross-model edge
///     (a manifest [`NodeId`]). Reserved; CLL-2 emits only `Intra` (the
///     intra-model variant — cross-model is CLL-4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnScope {
    /// A CTE/terminal node within one model's DAG (a `dag.nodes[].id`).
    Intra {
        /// The stable engine node id (a CTE alias or `TERMINAL_NODE_NAME`).
        node_id: String,
    },
    /// An upstream MODEL (cross-model, Tier 3 / CLL-4). Reserved.
    Cross {
        /// The upstream manifest model id.
        model: NodeId,
    },
}

/// A column within a [`ColumnScope`]. `scope` is the TYPED node identity —
/// the v2 `SpanRole::Column { node_id, column }` reserved slot rendered as
/// an edge endpoint, never an overloaded `String`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColumnRef {
    /// The node the column belongs to.
    pub scope: ColumnScope,
    /// The column name (lowercased to match the engine's case-folding).
    pub column: String,
}

impl ColumnRef {
    /// Construct an `Intra`-scoped column reference within one model's DAG.
    #[must_use]
    pub fn intra(node_id: impl Into<String>, column: impl Into<String>) -> Self {
        Self {
            scope: ColumnScope::Intra {
                node_id: node_id.into(),
            },
            column: column.into(),
        }
    }
}

/// Recce's dbt-proven 5-way transformation vocabulary (NOT Fusion's three
/// inconsistent layers). ONE canonical enum, stable for byte-identity
/// goldens. `#[non_exhaustive]` — every future kind is an additive variant.
///
/// CLL-2 (the deterministic MVP) emits `PassThrough` and `Renamed` only;
/// `Derived` is the deferred expression-walker extension (CLL-3 / #449),
/// and `Source`/`JoinKey` are the cross-model / predicate kinds (CLL-4).
/// They are defined now so the vocabulary is stable for goldens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ColumnEdgeKind {
    /// `c.email AS email` — a direct copy, output name == input name.
    PassThrough,
    /// `c.email AS contact_email` — a copy under a new name.
    Renamed,
    /// `coalesce(a.x, b.y) AS z` — many-to-one through an expression
    /// (DEFERRED to CLL-3; not produced by CLL-2).
    Derived,
    /// Terminates at a leaf table ref / `ref()` boundary (CLL-4).
    Source,
    /// Used in a join/filter predicate, not projected (CLL-4).
    JoinKey,
}

/// The honest no-catalog confidence (never-a-false-claim) — tracks SQL
/// EXPLICITNESS, not catalog presence. `#[non_exhaustive]` — additive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ColumnEdgeConfidence {
    /// Qualified, single-source (via `sole_relation_leaf`), or disambiguated
    /// via documented `columns[]` — the SQL states the input unambiguously.
    Resolved,
    /// Unqualified column under a multi-relation FROM → fanned out to all
    /// candidate sources (the `SQLLineage` two-edge trick). Render dotted.
    Ambiguous,
    /// `SELECT *` / `q.*` over an UNKNOWN external relation (undocumented
    /// `source()`), lateral / UNNEST / `json_extract` — the column list is not
    /// statically enumerable. Render badged with the WHY. NEVER dropped.
    Opaque,
}

/// ONE directed column-provenance edge: an output column `to_col` derives
/// from an input column `from_col`, classified by `kind` + `confidence`.
/// The bidirectional edge set serves all three affordances (context,
/// downstream impact, upstream trace) from one structure — downstream
/// impact is a reverse index over the set; upstream trace is a forward
/// walk. Written by the engine onto [`CteGraph::with_column_edges`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnEdge {
    /// Upstream (input) column.
    pub from_col: ColumnRef,
    /// Downstream (output) column.
    pub to_col: ColumnRef,
    /// The transformation kind.
    pub kind: ColumnEdgeKind,
    /// The honest confidence in this edge.
    pub confidence: ColumnEdgeConfidence,
}

impl ColumnEdge {
    /// Canonical constructor.
    #[must_use]
    pub fn new(
        from_col: ColumnRef,
        to_col: ColumnRef,
        kind: ColumnEdgeKind,
        confidence: ColumnEdgeConfidence,
    ) -> Self {
        Self {
            from_col,
            to_col,
            kind,
            confidence,
        }
    }
}

/// A column-defining span: the compiled byte range of one output column's
/// projection item within the owning CTE/terminal body. The domain fact
/// behind the v2 `SpanRole::Column { node_id, column }` `SourceMapEntry`
/// (a sub-range of the owning `CteBody` entry). Written by the engine onto
/// [`CteGraph::with_column_spans`] in the SAME retain-don't-recompute AST
/// pass that retains the node spans (never a second parse); the domain
/// `SourceMap` folds these into `SpanRole::Column` entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnSpan {
    /// The owning node id (a CTE alias or `TERMINAL_NODE_NAME`).
    pub node_id: String,
    /// The output column name (lowercased).
    pub column: String,
    /// The compiled byte/line/col span of the projection item — a sub-range
    /// of the owning `CteBody` entry's span.
    pub span: SourceSpan,
}

impl ColumnSpan {
    /// Canonical constructor.
    #[must_use]
    pub fn new(node_id: impl Into<String>, column: impl Into<String>, span: SourceSpan) -> Self {
        Self {
            node_id: node_id.into(),
            column: column.into(),
            span,
        }
    }
}

/// Directed acyclic graph of CTE nodes + edges produced by the CTE
/// engine (PR 7) and consumed by the renderer (PR 8b).
///
/// Edge endpoints are `usize` indices into [`Self::nodes`]; the
/// constructor takes ownership of both vectors so the indices remain
/// valid for the lifetime of the graph. The constructor does **not**
/// validate edge indices — the producer (PR 7) is responsible for
/// emitting only well-formed graphs; the renderer expects them to be.
///
/// `is_recursive` is `true` when the parsed query used `WITH RECURSIVE`.
/// v0.1 does not attempt to render recursive CTEs; the renderer should
/// surface a banner ("recursive CTE present; recursive arm omitted from
/// DAG") and render only the non-recursive portion. The CTE engine drops
/// self-referencing edges via the acyclicity invariant (`from < to`), so
/// the node/edge list is always DAG-safe.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CteGraph {
    #[serde(default)]
    nodes: Vec<CteNode>,
    #[serde(default)]
    edges: Vec<CteEdge>,
    /// `true` when the source query used `WITH RECURSIVE`.
    ///
    /// The renderer uses this to display a "recursive CTE present" banner.
    /// Always `false` for standard dbt-compiled models; surfaced by
    /// [`Self::new`] defaulting to `false` and [`Self::with_recursive`]
    /// setting it.
    #[serde(default)]
    is_recursive: bool,
    /// Per-LEFT-JOIN structural facts across every body in the query
    /// (cute-dbt#173). Engine-computed, consumed by the domain check
    /// detectors only — `#[serde(skip)]` keeps the embedded report
    /// payload byte-stable. Populated even when the query has no `WITH`
    /// clause (the graph then has zero nodes but still carries the
    /// terminal body's facts).
    #[serde(skip)]
    left_join_facts: Vec<LeftJoinFact>,
    /// Per-negated-subquery structural facts across every body in the
    /// query (cute-dbt#196) — the [`LeftJoinFact`] sibling family.
    /// Engine-computed, consumed by the domain check detectors only —
    /// `#[serde(skip)]` keeps the embedded report payload byte-stable.
    #[serde(skip)]
    subquery_facts: Vec<SubqueryFact>,
    /// Intra-model column-provenance edges (cute-dbt#447, CLL-2) — one
    /// per `(output column ← input column)` derivation across every CTE/
    /// terminal body, classified by [`ColumnEdgeKind`] + confidence.
    /// Engine-computed from the SAME single parse (the cute-dbt#40
    /// retain-don't-recompute pattern); the renderer projects them into
    /// the per-model `column_lineage.edges` section. Additive +
    /// `skip_serializing_if = Vec::is_empty` so models with no resolvable
    /// column edges stay byte-stable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    column_edges: Vec<ColumnEdge>,
    /// Per-output-column compiled spans (cute-dbt#447, CLL-2) — the domain
    /// facts the per-model `SourceMap` folds into `SpanRole::Column`
    /// entries (sub-ranges of the owning `CteBody` entry). Engine-computed
    /// in the same projection walk. Additive + `skip_serializing_if`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    column_spans: Vec<ColumnSpan>,
}

impl CteGraph {
    /// Canonical constructor — takes ownership of both vectors.
    ///
    /// `is_recursive` defaults to `false`. Use [`Self::with_recursive`] to
    /// flag a `WITH RECURSIVE` query.
    #[must_use]
    pub fn new(nodes: Vec<CteNode>, edges: Vec<CteEdge>) -> Self {
        Self {
            nodes,
            edges,
            is_recursive: false,
            left_join_facts: Vec::new(),
            subquery_facts: Vec::new(),
            column_edges: Vec::new(),
            column_spans: Vec::new(),
        }
    }

    /// Mark the graph as derived from a `WITH RECURSIVE` query.
    ///
    /// Returns `self` with `is_recursive` set to `true`. Called by the CTE
    /// engine when it detects `WITH RECURSIVE` in the parsed SQL.
    #[must_use]
    pub fn with_recursive(mut self) -> Self {
        self.is_recursive = true;
        self
    }

    /// Attach engine-computed per-LEFT-JOIN facts (cute-dbt#173).
    ///
    /// Returns `self` with `left_join_facts` set. Called by the CTE
    /// engine from the same parsed AST that feeds the cute-dbt#40 shape
    /// facts — never a second parse.
    #[must_use]
    pub fn with_left_join_facts(mut self, left_join_facts: Vec<LeftJoinFact>) -> Self {
        self.left_join_facts = left_join_facts;
        self
    }

    /// Per-LEFT-JOIN structural facts across every body in the query,
    /// in source order (cute-dbt#173). Empty for join-free queries and
    /// for graphs constructed without engine-computed facts.
    #[must_use]
    pub fn left_join_facts(&self) -> &[LeftJoinFact] {
        &self.left_join_facts
    }

    /// Attach engine-computed per-negated-subquery facts
    /// (cute-dbt#196).
    ///
    /// Returns `self` with `subquery_facts` set. Called by the CTE
    /// engine from the same parsed AST that feeds the cute-dbt#40 shape
    /// facts and the cute-dbt#173 LEFT JOIN facts — never a second
    /// parse.
    #[must_use]
    pub fn with_subquery_facts(mut self, subquery_facts: Vec<SubqueryFact>) -> Self {
        self.subquery_facts = subquery_facts;
        self
    }

    /// Per-negated-subquery structural facts across every body in the
    /// query, in source order (cute-dbt#196). Empty for subquery-free
    /// queries and for graphs constructed without engine-computed
    /// facts.
    #[must_use]
    pub fn subquery_facts(&self) -> &[SubqueryFact] {
        &self.subquery_facts
    }

    /// Attach engine-computed intra-model column-provenance edges
    /// (cute-dbt#447, CLL-2).
    ///
    /// Returns `self` with `column_edges` set. Called by the CTE engine
    /// from the same parsed AST that feeds the cute-dbt#40 shape facts —
    /// never a second parse.
    #[must_use]
    pub fn with_column_edges(mut self, column_edges: Vec<ColumnEdge>) -> Self {
        self.column_edges = column_edges;
        self
    }

    /// Intra-model column-provenance edges across every body in the query
    /// (cute-dbt#447). Empty for graphs constructed without engine-computed
    /// column lineage, and for models whose projections resolve to nothing
    /// statically recoverable.
    #[must_use]
    pub fn column_edges(&self) -> &[ColumnEdge] {
        &self.column_edges
    }

    /// Attach engine-computed per-output-column compiled spans
    /// (cute-dbt#447, CLL-2) — the domain facts the per-model `SourceMap`
    /// folds into `SpanRole::Column` entries.
    ///
    /// Returns `self` with `column_spans` set. Same single-parse pass as
    /// [`Self::with_column_edges`].
    #[must_use]
    pub fn with_column_spans(mut self, column_spans: Vec<ColumnSpan>) -> Self {
        self.column_spans = column_spans;
        self
    }

    /// Per-output-column compiled spans across every body in the query
    /// (cute-dbt#447). Each span is a sub-range of the owning `CteBody`
    /// node's span.
    #[must_use]
    pub fn column_spans(&self) -> &[ColumnSpan] {
        &self.column_spans
    }

    /// CTE nodes in declaration order.
    #[must_use]
    pub fn nodes(&self) -> &[CteNode] {
        &self.nodes
    }

    /// Directed edges between CTE nodes.
    #[must_use]
    pub fn edges(&self) -> &[CteEdge] {
        &self.edges
    }

    /// `true` when the source query used `WITH RECURSIVE`.
    ///
    /// The renderer should surface a banner when this is `true` and omit
    /// any self-referencing edges (which the engine already drops via the
    /// `from < to` acyclicity invariant).
    #[must_use]
    pub fn is_recursive(&self) -> bool {
        self.is_recursive
    }

    /// `true` when the graph carries no CTE nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Derive this model's CROSS-MODEL facts (cute-dbt#450, CLL-4) — its
    /// terminal OUTPUT columns and the bare leaf relations it reads — from the
    /// already-resolved intra-model column edges (never a second parse). The
    /// cross-model builder
    /// ([`ProjectColumnGraph::build`](crate::domain::column_lineage::ProjectColumnGraph::build))
    /// consumes these.
    ///
    /// `terminal_node_id` is the engine's terminal-select node name
    /// (`cte_engine::TERMINAL_NODE_NAME`) — passed in so the domain stays
    /// decoupled from the adapter constant.
    ///
    /// Output columns are the `to_col.column`s of every edge landing on the
    /// terminal node, in first-seen (edge) order, de-duplicated. The terminal
    /// projection is NON-ENUMERABLE (`output_columns = None`) when it carries
    /// an Opaque star — an edge onto the terminal whose `from_col.column` is
    /// `"*"` with `Opaque` confidence (a `SELECT *` over an unknown external
    /// relation the resolver could not expand). This is the honest
    /// project-wide output map: a model whose own output can't be enumerated
    /// can't have a downstream star expanded over it (no fabricated names).
    #[must_use]
    pub fn model_outputs(
        &self,
        terminal_node_id: &str,
    ) -> crate::domain::column_lineage::ModelOutputs {
        let output_columns = self.terminal_output_columns(terminal_node_id);
        let leaf_refs = self.model_leaf_refs(terminal_node_id);
        let leaf_reading_nodes = self.leaf_reading_nodes(terminal_node_id);
        let source_passthrough_columns = self.source_passthrough_terminal_columns(
            terminal_node_id,
            output_columns.as_deref().unwrap_or(&[]),
            &leaf_reading_nodes,
        );
        crate::domain::column_lineage::ModelOutputs::with_passthrough(
            output_columns,
            leaf_refs,
            source_passthrough_columns,
        )
    }

    /// The model's terminal OUTPUT columns (lowercased, first-seen order),
    /// or `None` when the terminal projection is non-enumerable — an Opaque
    /// `*` edge lands on the terminal, or the terminal produced no resolvable
    /// edge at all (we could not name a single output column).
    fn terminal_output_columns(&self, terminal_node_id: &str) -> Option<Vec<String>> {
        let is_terminal = |scope: &ColumnScope| matches!(scope, ColumnScope::Intra { node_id } if node_id == terminal_node_id);
        let opaque_terminal = self.column_edges.iter().any(|e| {
            is_terminal(&e.to_col.scope)
                && e.from_col.column == "*"
                && e.confidence == ColumnEdgeConfidence::Opaque
        });
        if opaque_terminal {
            return None;
        }
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut cols: Vec<String> = Vec::new();
        for edge in &self.column_edges {
            if is_terminal(&edge.to_col.scope) && seen.insert(edge.to_col.column.clone()) {
                cols.push(edge.to_col.column.clone());
            }
        }
        if cols.is_empty() { None } else { Some(cols) }
    }

    /// Every bare leaf the model reads across all bodies — the candidate
    /// `ref()`/`source()` boundaries. The cross-model builder constrains these
    /// to the model's REAL `depends_on` producers, so an intra-model CTE alias
    /// appearing here can never mis-stitch. A WITH-less model (no CTE nodes)
    /// recovers its leaf from the star edges' `from_col` scope node id.
    fn model_leaf_refs(&self, terminal_node_id: &str) -> Vec<String> {
        let mut leaf_seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut leaf_refs: Vec<String> = Vec::new();
        for node in &self.nodes {
            for leaf in node.body_leaf_table_refs() {
                if leaf_seen.insert(leaf.clone()) {
                    leaf_refs.push(leaf.clone());
                }
            }
        }
        for edge in &self.column_edges {
            if let ColumnScope::Intra { node_id } = &edge.from_col.scope
                && node_id != terminal_node_id
                && leaf_seen.insert(node_id.clone())
            {
                leaf_refs.push(node_id.clone());
            }
        }
        leaf_refs
    }

    /// The CTE/terminal node IDS whose body reads an EXTERNAL leaf relation (a
    /// `ref()`/`source()` boundary — NOT another CTE in this same model). A
    /// column reaching one of these flowed in from OUTSIDE the model.
    ///
    /// `body_leaf_table_refs` includes intra-CTE references (the import CTE
    /// `renamed` "reads" the CTE `source`), so we EXCLUDE refs that name a
    /// sibling CTE — only a truly external relation marks a leaf boundary.
    /// (The import CTE `source` reads `organizations`, an external relation →
    /// leaf-reading; `final` reads `renamed`, a sibling CTE → NOT leaf-reading,
    /// so a column computed in `final` like `_loaded_at` never falsely reaches
    /// a source.)
    fn leaf_reading_nodes(&self, terminal_node_id: &str) -> std::collections::BTreeSet<String> {
        let cte_names: std::collections::BTreeSet<String> = self
            .nodes
            .iter()
            .map(|n| n.name().to_ascii_lowercase())
            .filter(|n| n != terminal_node_id)
            .collect();
        let mut leaf_reading_nodes: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        for node in &self.nodes {
            let reads_external = node
                .body_leaf_table_refs()
                .iter()
                .any(|leaf| !cte_names.contains(leaf));
            if reads_external {
                leaf_reading_nodes.insert(node.name().to_ascii_lowercase());
            }
        }
        // WITH-less / star-only: the from-side leaf nodes of star edges are
        // leaf-reading boundaries when they name an external relation.
        for edge in &self.column_edges {
            if let ColumnScope::Intra { node_id } = &edge.from_col.scope
                && node_id != terminal_node_id
                && !cte_names.contains(node_id)
            {
                leaf_reading_nodes.insert(node_id.clone());
            }
        }
        leaf_reading_nodes
    }

    /// The terminal output columns whose intra provenance is a pure
    /// pass-through/rename chain to a leaf-reading boundary, MAPPED to the
    /// ORIGINAL source-side column name that name reaches at the boundary
    /// (cute-dbt#450, round-4 robust name-tracking).
    ///
    /// The map's KEY is the terminal output column; the VALUE is the column
    /// name on the leaf-reading boundary (the real source field). For a pure
    /// same-name pass-through they are equal (`order_id → order_id`); for a
    /// RENAME anywhere in the chain the value is the ORIGINAL upstream name
    /// (`order_amount → amount`, `qty → legacy_qty`). The cross-model source
    /// name-carry uses the VALUE as the source field, so a renamed staging
    /// column traces to the field that actually exists — never a fabricated
    /// `source.<renamed_name>` (the never-a-false-claim floor).
    ///
    /// `leaf_nodes` is the set of node ids that read a leaf relation (the
    /// `ref()`/`source()` boundary). A column whose chain dead-ends at a
    /// computed expression (a `Derived` edge, or a node with no resolvable
    /// inbound that is NOT a star-passthrough over a leaf) is EXCLUDED — never
    /// a fabricated source attribution. A column whose source name cannot be
    /// UNIQUELY resolved (a fork to two distinct source-side names) degrades to
    /// EXCLUDED rather than guess.
    fn source_passthrough_terminal_columns(
        &self,
        terminal_node_id: &str,
        output_columns: &[String],
        leaf_nodes: &std::collections::BTreeSet<String>,
    ) -> std::collections::BTreeMap<String, String> {
        let cte_names: std::collections::BTreeSet<String> = self
            .nodes
            .iter()
            .map(|n| n.name().to_ascii_lowercase())
            .collect();
        let mut memo: std::collections::HashMap<(String, String), Option<String>> =
            std::collections::HashMap::new();
        let mut out: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        for col in output_columns {
            if let Some(source_col) = self.resolve_leaf_column(
                terminal_node_id,
                col,
                leaf_nodes,
                &cte_names,
                &mut memo,
                0,
            ) {
                out.insert(col.clone(), source_col);
            }
        }
        out
    }

    /// Resolve the column `(node_id, column)` to the ORIGINAL source-side
    /// column name it reaches at a leaf-reading boundary, threading the name
    /// through every rename in the chain — or `None` when it does not reach a
    /// leaf cleanly (a computed/`Derived` dead-end, or a non-uniquely-resolvable
    /// fork). Memoized; depth-capped (guards a pathological / cyclic AST).
    ///
    /// The resolution carries the ORIGINAL name (cute-dbt#450 round-4): a
    /// `Renamed` edge (`legacy_qty AS qty`) is followed on its UPSTREAM column
    /// (`legacy_qty`), so the name reaching the boundary is the real source
    /// field — NEVER the renamed downstream name. The round-3 floor (no
    /// fabricated source field) is preserved AND the staging-rename headline
    /// trace-to-source is restored.
    ///
    /// Boundary cases:
    /// - `(leaf_node, column)` with an explicit inbound edge from an EXTERNAL
    ///   relation (`from` scope is not a sibling CTE): the edge's `from` column
    ///   IS the source field (`PassThrough` → same name; `Renamed` → original
    ///   name).
    /// - `(leaf_node, column)` with NO explicit inbound edge: the column flowed
    ///   in through a `select *` over the external leaf — it passes through
    ///   UNCHANGED, so the source field is `column` itself. (A column with a
    ///   `Derived` inbound is computed, not a star-passthrough → `None`.)
    /// - A `Derived` / `JoinKey` / Opaque `*→*` inbound anywhere → `None`.
    fn resolve_leaf_column(
        &self,
        node_id: &str,
        column: &str,
        leaf_nodes: &std::collections::BTreeSet<String>,
        cte_names: &std::collections::BTreeSet<String>,
        memo: &mut std::collections::HashMap<(String, String), Option<String>>,
        depth: u32,
    ) -> Option<String> {
        if depth > 256 {
            return None; // pathological chain — degrade (never a false claim).
        }
        let key = (node_id.to_owned(), column.to_owned());
        if let Some(hit) = memo.get(&key) {
            return hit.clone();
        }
        // Pre-seed `None` to break cycles (a column transitively feeding itself
        // is not a clean pass-through to a leaf).
        memo.insert(key.clone(), None);

        // Fold every inbound edge defining this column into a unique resolution
        // (or a degrade). The per-edge classification lives in
        // `inbound_source_name`; here we only accumulate uniqueness.
        let mut acc = LeafResolution::default();
        for edge in &self.column_edges {
            if !Self::edge_targets(edge, node_id, column) {
                continue;
            }
            acc.saw_edge = true;
            acc.absorb(self.inbound_source_name(edge, leaf_nodes, cte_names, memo, depth));
        }

        // The star-passthrough fallback (a column with NO inbound edge flowed
        // in through `select *` over the external leaf) fires ONLY when this
        // node ACTUALLY carries a star projection over an external leaf — a
        // recorded `*→*` edge landing on `(node_id, "*")` whose from-side is a
        // truly external relation. A leaf-reading node with NO star at all (its
        // projection is explicit) has a no-inbound-edge column because that
        // column is a LITERAL / computed expression (`42 AS magic`,
        // `current_timestamp AS t`) — which the engine emits NO edge for. Such
        // a column MUST degrade to `None`; it is not a source field. (#450
        // round-5: no `no-inbound-edge ⇒ assume pass-through` guess.)
        let star_over_external = self.has_external_star(node_id, cte_names);
        let result = acc.into_name(column, leaf_nodes.contains(node_id) && star_over_external);
        memo.insert(key, result.clone());
        result
    }

    /// `true` when `node_id` carries a star projection (`select *` / `q.*`)
    /// over an EXTERNAL leaf relation — a recorded `*→*` column edge whose
    /// target is `(node_id, "*")` and whose source node is NOT a sibling CTE
    /// (a genuine `ref()`/`source()` boundary). This is the ONLY justification
    /// for attributing a no-inbound-edge column to the source under its own
    /// name: it flowed through the star unchanged. A node whose star is over a
    /// KNOWN intra-model CTE instead expands to per-column edges (so its
    /// columns carry explicit inbound edges and never reach the fallback); a
    /// node with no star at all has no business attributing an edge-less
    /// (literal/computed) column to a source.
    fn has_external_star(
        &self,
        node_id: &str,
        cte_names: &std::collections::BTreeSet<String>,
    ) -> bool {
        self.column_edges.iter().any(|edge| {
            edge.from_col.column == "*"
                && Self::edge_targets(edge, node_id, "*")
                && matches!(
                    &edge.from_col.scope,
                    ColumnScope::Intra { node_id: from } if !cte_names.contains(from)
                )
        })
    }

    /// `true` when `edge` is an intra-scoped edge whose target is exactly
    /// `(node_id, column)` — the inbound edges defining this column.
    fn edge_targets(edge: &ColumnEdge, node_id: &str, column: &str) -> bool {
        matches!(
            &edge.to_col.scope,
            ColumnScope::Intra { node_id: to } if to == node_id
        ) && edge.to_col.column == column
    }

    /// Classify ONE inbound edge to the resolution name it contributes.
    /// `None` (degrade) for a non-intra source, a `Derived`/`JoinKey`/Opaque
    /// `*→*` edge (the column is computed/non-enumerable here), or an upstream
    /// recursion that itself degrades. `Some(name)` carries the ORIGINAL
    /// source-side column name: the upstream column for a sibling-CTE recursion
    /// (threading renames), or the external `from` column at the leaf boundary.
    fn inbound_source_name(
        &self,
        edge: &ColumnEdge,
        leaf_nodes: &std::collections::BTreeSet<String>,
        cte_names: &std::collections::BTreeSet<String>,
        memo: &mut std::collections::HashMap<(String, String), Option<String>>,
        depth: u32,
    ) -> Option<String> {
        let ColumnScope::Intra { node_id: from_node } = &edge.from_col.scope else {
            return None;
        };
        let clean = matches!(
            edge.kind,
            ColumnEdgeKind::PassThrough | ColumnEdgeKind::Renamed
        ) && edge.from_col.column != "*";
        if !clean {
            return None;
        }
        if cte_names.contains(from_node) {
            // Sibling CTE — recurse on the UPSTREAM name (rename follows its
            // original name).
            self.resolve_leaf_column(
                from_node,
                &edge.from_col.column,
                leaf_nodes,
                cte_names,
                memo,
                depth + 1,
            )
        } else {
            // External leaf boundary — the `from` column IS the source field.
            Some(edge.from_col.column.clone())
        }
    }
}

/// The accumulator for [`CteGraph::resolve_leaf_column`] — folds the per-edge
/// classifications into a UNIQUE source-side name (or a degrade). Keeps the
/// fold's branching out of the recursive function (crap4rs CC budget).
#[derive(Default)]
struct LeafResolution {
    /// The unique resolved source name so far (`None` until the first clean
    /// edge resolves).
    name: Option<String>,
    /// A clean edge defining this column was seen (so the "no explicit edge →
    /// star pass-through" fallback does NOT apply).
    saw_edge: bool,
    /// Two distinct source-side names, or a degrading/blocking edge — the
    /// resolution cannot be unique; degrade.
    blocked: bool,
}

impl LeafResolution {
    /// Absorb one edge's classification: `Some(name)` resolves (conflicting
    /// names block); `None` blocks (a computed/non-enumerable inbound).
    fn absorb(&mut self, candidate: Option<String>) {
        match candidate {
            Some(name) => match &self.name {
                Some(prev) if *prev != name => self.blocked = true,
                _ => self.name = Some(name),
            },
            None => self.blocked = true,
        }
    }

    /// Resolve the fold to a source-side column name, or `None` to degrade.
    /// `leaf_star_passthrough` is `true` ONLY when this node both reads an
    /// external leaf AND carries a genuine `*` projection over it (see
    /// [`CteGraph::has_external_star`]); in that case a column with NO clean
    /// inbound edge flowed in through the `select *` unchanged, so the source
    /// field is the column's own name. When the node has NO such star, an
    /// edge-less column is a LITERAL / computed expression (`42 AS magic`,
    /// `current_timestamp AS t`) — the engine emits no edge for it — and it
    /// MUST degrade to `None` (never a fabricated source field).
    fn into_name(self, column: &str, leaf_star_passthrough: bool) -> Option<String> {
        if self.blocked {
            None
        } else if let Some(name) = self.name {
            Some(name)
        } else if !self.saw_edge && leaf_star_passthrough {
            Some(column.to_owned())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_type_serde_roundtrip_all_variants() {
        for et in [
            EdgeType::From,
            EdgeType::Inner,
            EdgeType::Left,
            EdgeType::Right,
            EdgeType::Full,
            EdgeType::Cross,
            EdgeType::UnionAll,
            EdgeType::UnionDistinct,
        ] {
            let json = serde_json::to_string(&et).unwrap();
            let back: EdgeType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, et, "round-trip failed for {et:?}");
        }
    }

    #[test]
    fn edge_type_serializes_as_snake_case() {
        assert_eq!(serde_json::to_string(&EdgeType::From).unwrap(), "\"from\"");
        assert_eq!(
            serde_json::to_string(&EdgeType::Inner).unwrap(),
            "\"inner\""
        );
        assert_eq!(serde_json::to_string(&EdgeType::Left).unwrap(), "\"left\"");
        assert_eq!(
            serde_json::to_string(&EdgeType::Right).unwrap(),
            "\"right\""
        );
        assert_eq!(serde_json::to_string(&EdgeType::Full).unwrap(), "\"full\"");
        assert_eq!(
            serde_json::to_string(&EdgeType::Cross).unwrap(),
            "\"cross\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeType::UnionAll).unwrap(),
            "\"union_all\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeType::UnionDistinct).unwrap(),
            "\"union_distinct\""
        );
    }

    #[test]
    fn edge_type_is_copy_and_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(EdgeType::Inner);
        set.insert(EdgeType::Inner);
        set.insert(EdgeType::Left);
        set.insert(EdgeType::UnionAll);
        assert_eq!(set.len(), 3);
    }

    /// A `SourceSpan` over a single line, byte-offset endpoints — the
    /// retained-fact shape the CTE engine writes onto a `CteNode`.
    fn src_span(start_byte: u32, end_byte: u32) -> SourceSpan {
        SourceSpan {
            start: crate::domain::span::SourcePos {
                line: 3,
                col: 1,
                byte: start_byte,
            },
            end: crate::domain::span::SourcePos {
                line: 3,
                col: end_byte - start_byte + 1,
                byte: end_byte,
            },
        }
    }

    #[test]
    fn cte_node_constructor_and_getters() {
        let n = CteNode::new(
            "src_orders",
            Some(src_span(40, 64)),
            Some("select * from raw.orders".to_owned()),
            None,
        );
        assert_eq!(n.name(), "src_orders");
        assert_eq!(n.source_span(), Some(&src_span(40, 64)));
        assert_eq!(n.raw_sql(), Some("select * from raw.orders"));
        assert!(n.desc().is_none(), "v0.1 always emits desc: None");
        assert!(
            !n.is_simple_from_shape(),
            "default constructor classifies as Transform (the safer default)"
        );
        assert!(
            n.body_leaf_table_refs().is_empty(),
            "default constructor emits no AST-derived table refs"
        );
    }

    #[test]
    fn cte_node_with_shape_facts_attaches_engine_computed_data() {
        let n = CteNode::new("src_orders", None, None, None)
            .with_shape_facts(true, vec!["orders".to_owned()]);
        assert!(n.is_simple_from_shape());
        assert_eq!(n.body_leaf_table_refs(), &["orders".to_owned()]);
    }

    #[test]
    fn cte_node_tolerates_missing_optionals_on_wire() {
        // `{"name": "x"}` is the minimal wire form — every other field
        // is `#[serde(default)]` so older payloads deserialize cleanly
        // and the new shape-fact fields fall back to their safe defaults.
        let json = r#"{ "name": "x" }"#;
        let n: CteNode = serde_json::from_str(json).unwrap();
        assert_eq!(n.name(), "x");
        assert!(n.source_span().is_none());
        assert!(n.raw_sql().is_none());
        assert!(n.desc().is_none());
        assert!(!n.is_simple_from_shape());
        assert!(n.body_leaf_table_refs().is_empty());
    }

    #[test]
    fn cte_edge_constructor_and_getters() {
        let e = CteEdge::new(0, 1, EdgeType::Left);
        assert_eq!(e.from(), 0);
        assert_eq!(e.to(), 1);
        assert_eq!(e.edge_type(), EdgeType::Left);
    }

    #[test]
    fn cte_graph_default_is_empty() {
        let g = CteGraph::default();
        assert!(g.is_empty());
        assert!(g.nodes().is_empty());
        assert!(g.edges().is_empty());
    }

    #[test]
    fn cte_graph_new_holds_passed_nodes_and_edges() {
        let nodes = vec![
            CteNode::new("a", None, None, None),
            CteNode::new("b", None, None, None),
        ];
        let edges = vec![CteEdge::new(0, 1, EdgeType::Inner)];
        let g = CteGraph::new(nodes, edges);
        assert_eq!(g.nodes().len(), 2);
        assert_eq!(g.edges().len(), 1);
        assert!(!g.is_empty());
    }

    #[test]
    fn cte_graph_serde_roundtrip() {
        let g = CteGraph::new(
            vec![
                CteNode::new("a", None, None, None),
                CteNode::new("b", None, Some("select * from a".to_owned()), None),
            ],
            vec![
                CteEdge::new(0, 1, EdgeType::Inner),
                CteEdge::new(1, 0, EdgeType::Cross),
            ],
        );
        let back: CteGraph = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn cte_graph_new_defaults_is_recursive_to_false() {
        let g = CteGraph::new(vec![], vec![]);
        assert!(!g.is_recursive(), "new() sets is_recursive = false");
    }

    #[test]
    fn cte_graph_with_recursive_sets_flag() {
        let g = CteGraph::new(vec![], vec![]).with_recursive();
        assert!(
            g.is_recursive(),
            "with_recursive() sets is_recursive = true"
        );
    }

    #[test]
    fn cte_graph_is_recursive_survives_serde_roundtrip() {
        let g = CteGraph::new(vec![], vec![]).with_recursive();
        let json = serde_json::to_string(&g).unwrap();
        let back: CteGraph = serde_json::from_str(&json).unwrap();
        assert!(
            back.is_recursive(),
            "is_recursive round-trips through serde"
        );
    }

    #[test]
    fn left_join_fact_constructor_and_getters() {
        let fact = LeftJoinFact::new(
            "(final select)",
            "customers",
            vec![JoinKeyPair::new(
                Some("orders".to_owned()),
                "customer_id",
                "id",
            )],
            vec!["id".to_owned()],
            true,
            false,
        );
        assert_eq!(fact.consumer(), "(final select)");
        assert_eq!(fact.right_leaf(), "customers");
        assert_eq!(fact.equi_keys().len(), 1);
        assert_eq!(fact.equi_keys()[0].left_leaf(), Some("orders"));
        assert_eq!(fact.equi_keys()[0].left_column(), "customer_id");
        assert_eq!(fact.equi_keys()[0].right_column(), "id");
        assert_eq!(fact.where_is_null_columns(), &["id".to_owned()]);
        assert!(fact.projects_right_columns());
        assert!(!fact.select_is_distinct());
    }

    #[test]
    fn cte_graph_with_left_join_facts_attaches_engine_computed_data() {
        let fact = LeftJoinFact::new("final", "customers", Vec::new(), Vec::new(), false, true);
        let g = CteGraph::new(vec![], vec![]).with_left_join_facts(vec![fact.clone()]);
        assert_eq!(g.left_join_facts(), &[fact]);
        assert!(
            CteGraph::new(vec![], vec![]).left_join_facts().is_empty(),
            "constructor carries no left-join facts by default"
        );
    }

    #[test]
    fn left_join_facts_never_reach_the_wire() {
        // #[serde(skip)] — the facts are render-pass internal (domain
        // check detectors only); the embedded report payload must stay
        // byte-identical to the pre-#173 shape.
        let g = CteGraph::new(vec![], vec![]).with_left_join_facts(vec![LeftJoinFact::new(
            "final",
            "customers",
            Vec::new(),
            Vec::new(),
            true,
            false,
        )]);
        let json = serde_json::to_string(&g).unwrap();
        assert!(
            !json.contains("left_join_facts"),
            "left_join_facts must not serialize: {json}"
        );
        let back: CteGraph = serde_json::from_str(&json).unwrap();
        assert!(
            back.left_join_facts().is_empty(),
            "deserialization defaults to no facts"
        );
    }

    #[test]
    fn subquery_fact_constructor_and_getters() {
        let fact = SubqueryFact::new(
            SubqueryKind::NotExists,
            "(final select)",
            "stg_orders",
            vec![JoinKeyPair::new(
                Some("stg_customers".to_owned()),
                "customer_id",
                "customer_id",
            )],
        );
        assert_eq!(fact.kind(), SubqueryKind::NotExists);
        assert_eq!(fact.consumer(), "(final select)");
        assert_eq!(fact.inner_leaf(), "stg_orders");
        assert_eq!(fact.equi_keys().len(), 1);
        assert_eq!(fact.equi_keys()[0].left_leaf(), Some("stg_customers"));
        assert_eq!(fact.equi_keys()[0].left_column(), "customer_id");
        assert_eq!(fact.equi_keys()[0].right_column(), "customer_id");
    }

    #[test]
    fn cte_graph_with_subquery_facts_attaches_engine_computed_data() {
        let fact = SubqueryFact::new(SubqueryKind::NotIn, "final", "stg_refunds", Vec::new());
        let g = CteGraph::new(vec![], vec![]).with_subquery_facts(vec![fact.clone()]);
        assert_eq!(g.subquery_facts(), &[fact]);
        assert!(
            CteGraph::new(vec![], vec![]).subquery_facts().is_empty(),
            "constructor carries no subquery facts by default"
        );
    }

    #[test]
    fn subquery_facts_never_reach_the_wire() {
        // #[serde(skip)] — the facts are render-pass internal (domain
        // check detectors only); the embedded report payload must stay
        // byte-identical to the pre-#196 shape (the
        // left_join_facts_never_reach_the_wire twin).
        let g = CteGraph::new(vec![], vec![]).with_subquery_facts(vec![SubqueryFact::new(
            SubqueryKind::NotExists,
            "final",
            "stg_orders",
            Vec::new(),
        )]);
        let json = serde_json::to_string(&g).unwrap();
        assert!(
            !json.contains("subquery_facts"),
            "subquery_facts must not serialize: {json}"
        );
        let back: CteGraph = serde_json::from_str(&json).unwrap();
        assert!(
            back.subquery_facts().is_empty(),
            "deserialization defaults to no facts"
        );
    }

    #[test]
    fn cte_graph_is_recursive_defaults_to_false_on_old_wire() {
        // A serialized graph without an `is_recursive` field (old format)
        // must deserialize with is_recursive = false (the #[serde(default)]
        // path).
        let json = r#"{"nodes":[],"edges":[]}"#;
        let g: CteGraph = serde_json::from_str(json).unwrap();
        assert!(
            !g.is_recursive(),
            "missing is_recursive field defaults to false"
        );
    }

    // -----------------------------------------------------------------
    // CLL-2 (cute-dbt#447) — column-lineage PODs.
    // -----------------------------------------------------------------

    #[test]
    fn column_edge_serde_roundtrip_intra() {
        let edge = ColumnEdge::new(
            ColumnRef::intra("customers", "email"),
            ColumnRef::intra("(final select)", "contact_email"),
            ColumnEdgeKind::Renamed,
            ColumnEdgeConfidence::Resolved,
        );
        let json = serde_json::to_string(&edge).unwrap();
        let back: ColumnEdge = serde_json::from_str(&json).unwrap();
        assert_eq!(back, edge);
    }

    #[test]
    fn column_scope_serializes_as_tagged_snake_case() {
        let intra = ColumnScope::Intra {
            node_id: "stg".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&intra).unwrap(),
            r#"{"intra":{"node_id":"stg"}}"#
        );
        let cross = ColumnScope::Cross {
            model: NodeId::new("model.proj.stg_orders"),
        };
        assert_eq!(
            serde_json::to_string(&cross).unwrap(),
            r#"{"cross":{"model":"model.proj.stg_orders"}}"#
        );
    }

    #[test]
    fn column_edges_omitted_when_empty() {
        // Additive + skip_serializing_if = Vec::is_empty ⇒ a graph with no
        // column edges has no `column_edges` key (byte-stability for goldens).
        let g = CteGraph::new(Vec::new(), Vec::new());
        let json = serde_json::to_string(&g).unwrap();
        assert!(
            !json.contains("column_edges"),
            "empty column_edges must be omitted: {json}"
        );
        assert!(
            !json.contains("column_spans"),
            "empty column_spans must be omitted: {json}"
        );
    }

    #[test]
    fn graph_with_column_edges_and_spans_roundtrip() {
        let edges = vec![ColumnEdge::new(
            ColumnRef::intra("a", "x"),
            ColumnRef::intra("(final select)", "x"),
            ColumnEdgeKind::PassThrough,
            ColumnEdgeConfidence::Resolved,
        )];
        let spans = vec![ColumnSpan::new("a", "x", src_span(8, 9))];
        let g = CteGraph::new(Vec::new(), Vec::new())
            .with_column_edges(edges.clone())
            .with_column_spans(spans.clone());
        assert_eq!(g.column_edges(), edges.as_slice());
        assert_eq!(g.column_spans(), spans.as_slice());
        let json = serde_json::to_string(&g).unwrap();
        let back: CteGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(back.column_edges(), edges.as_slice());
        assert_eq!(back.column_spans(), spans.as_slice());
    }
}
