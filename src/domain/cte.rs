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

/// 1-based `(line, column)` span anchor; future use by the renderer to
/// surface raw SQL spans in tooltips. Stored as a struct (not a tuple)
/// so additive fields stay mechanical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    line: u32,
    column: u32,
}

impl Span {
    /// Canonical constructor.
    #[must_use]
    pub fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }

    /// 1-based line number.
    #[must_use]
    pub fn line(&self) -> u32 {
        self.line
    }

    /// 1-based column number.
    #[must_use]
    pub fn column(&self) -> u32 {
        self.column
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CteNode {
    name: String,
    #[serde(default)]
    span: Option<Span>,
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
    /// `is_simple_from_shape` defaults to `false` and `body_leaf_table_refs`
    /// to empty. Use [`Self::with_shape_facts`] to attach engine-computed
    /// structural facts.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        span: Option<Span>,
        raw_sql: Option<String>,
        desc: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            span,
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

    /// Source-location anchor (for renderer tooltips); `None` in v0.1.
    #[must_use]
    pub fn span(&self) -> Option<&Span> {
        self.span.as_ref()
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

    #[test]
    fn span_constructor_and_getters() {
        let s = Span::new(7, 12);
        assert_eq!(s.line(), 7);
        assert_eq!(s.column(), 12);
    }

    #[test]
    fn cte_node_constructor_and_getters() {
        let n = CteNode::new(
            "src_orders",
            Some(Span::new(3, 1)),
            Some("select * from raw.orders".to_owned()),
            None,
        );
        assert_eq!(n.name(), "src_orders");
        assert_eq!(n.span(), Some(&Span::new(3, 1)));
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
        assert!(n.span().is_none());
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
}
