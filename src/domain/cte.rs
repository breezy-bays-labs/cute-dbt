//! `CteGraph` + `CteNode` + `CteEdge` + `JoinType` — the AST output the
//! sqlparser CTE engine (PR 7) produces and the renderer (PR 8b)
//! consumes.
//!
//! Edges store endpoints as `usize` indices into the `nodes` vector —
//! the renderer needs a stable iteration order and Mermaid `graph LR`
//! syntax is index-friendly. The constructor takes ownership of the
//! `nodes` vector exactly once so indices remain valid for the lifetime
//! of the `CteGraph`.
//!
//! `JoinType` is `#[non_exhaustive]` per the
//! [enums-yes-structs-no rule](https://github.com/cmbays/.claude/blob/main/rules/non-exhaustive.md):
//! consumers pattern-match this and new SQL dialect joins (e.g.
//! `LATERAL`) are additive.

use serde::{Deserialize, Serialize};

/// SQL join kind classified by the CTE engine.
///
/// `#[non_exhaustive]` — adding a dialect-specific variant is a v0.x
/// additive change that consumers must opt into via `_` arms.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JoinType {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CteNode {
    name: String,
    #[serde(default)]
    span: Option<Span>,
    #[serde(default)]
    raw_sql: Option<String>,
    #[serde(default)]
    desc: Option<String>,
}

impl CteNode {
    /// Canonical constructor.
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
        }
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
}

/// A directed edge between two CTE nodes in [`CteGraph`].
///
/// `from` and `to` are indices into the parent `CteGraph::nodes` vector;
/// `join_type` classifies the SQL relationship the edge represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CteEdge {
    from: usize,
    to: usize,
    join_type: JoinType,
}

impl CteEdge {
    /// Canonical constructor.
    #[must_use]
    pub fn new(from: usize, to: usize, join_type: JoinType) -> Self {
        Self {
            from,
            to,
            join_type,
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

    /// SQL join kind classified by the CTE engine.
    #[must_use]
    pub fn join_type(&self) -> JoinType {
        self.join_type
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CteGraph {
    #[serde(default)]
    nodes: Vec<CteNode>,
    #[serde(default)]
    edges: Vec<CteEdge>,
}

impl CteGraph {
    /// Canonical constructor — takes ownership of both vectors.
    #[must_use]
    pub fn new(nodes: Vec<CteNode>, edges: Vec<CteEdge>) -> Self {
        Self { nodes, edges }
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
    fn join_type_serde_roundtrip_lowercase_variants() {
        for jt in [
            JoinType::Inner,
            JoinType::Left,
            JoinType::Right,
            JoinType::Full,
            JoinType::Cross,
        ] {
            let json = serde_json::to_string(&jt).unwrap();
            let back: JoinType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, jt, "round-trip failed for {jt:?}");
        }
    }

    #[test]
    fn join_type_serializes_as_lowercase() {
        assert_eq!(
            serde_json::to_string(&JoinType::Inner).unwrap(),
            "\"inner\""
        );
        assert_eq!(serde_json::to_string(&JoinType::Left).unwrap(), "\"left\"");
        assert_eq!(
            serde_json::to_string(&JoinType::Right).unwrap(),
            "\"right\""
        );
        assert_eq!(serde_json::to_string(&JoinType::Full).unwrap(), "\"full\"");
        assert_eq!(
            serde_json::to_string(&JoinType::Cross).unwrap(),
            "\"cross\""
        );
    }

    #[test]
    fn join_type_is_copy_and_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(JoinType::Inner);
        set.insert(JoinType::Inner);
        set.insert(JoinType::Left);
        assert_eq!(set.len(), 2);
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
    }

    #[test]
    fn cte_node_tolerates_missing_optionals_on_wire() {
        let json = r#"{ "name": "x" }"#;
        let n: CteNode = serde_json::from_str(json).unwrap();
        assert_eq!(n.name(), "x");
        assert!(n.span().is_none());
        assert!(n.raw_sql().is_none());
        assert!(n.desc().is_none());
    }

    #[test]
    fn cte_edge_constructor_and_getters() {
        let e = CteEdge::new(0, 1, JoinType::Left);
        assert_eq!(e.from(), 0);
        assert_eq!(e.to(), 1);
        assert_eq!(e.join_type(), JoinType::Left);
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
        let edges = vec![CteEdge::new(0, 1, JoinType::Inner)];
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
                CteEdge::new(0, 1, JoinType::Inner),
                CteEdge::new(1, 0, JoinType::Cross),
            ],
        );
        let back: CteGraph = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
    }
}
