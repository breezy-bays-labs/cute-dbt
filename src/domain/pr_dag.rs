//! PR-scope lineage mini-DAG (cute-dbt#352 — Slice A, pure-domain core).
//!
//! Computes the *focused* cross-model lineage graph a PR review report
//! puts at the top: the models the PR modified (emphasized), plus the
//! **connectors** between them — the models lying on a directed lineage
//! path between two distinct modified models — so a reviewer grasps the
//! change's shape at a glance.
//!
//! This is a **DAG-only decoration**, not a change to scope selection.
//! [`crate::domain::scope`] owns the in-scope semantics (which unit tests
//! and model cards the report renders); this module owns only the graph
//! topology + per-node state that sits above them. The two never disagree
//! because this module *consumes* the already-computed modified set
//! ([`ModelInScopeSet`]) and the already-derived removed set (the
//! baseline−current set-diff) rather than recomputing either.
//!
//! ## Node set
//!
//! `nodes = modified ∪ connectors ∪ removed`, where
//!
//! - **modified** (M) is the caller-supplied [`ModelInScopeSet`] — the
//!   genuine PR-modified models (`changed_models` in the `PrDiff` arm, the
//!   `StateComparator` modified models in the baseline arm).
//! - **connectors** are the models on a directed lineage path *between two
//!   distinct* members of M — formally `(DESC(M) ∩ ANC(M)) \ M`, keeping
//!   only nodes reached forward from one seed *and* backward from a
//!   different seed. A node merely downstream of a single modified model
//!   is 1-hop context, **not** a connector. An isolated modified model
//!   (no lineage path to/from another modified model) contributes no
//!   connector and renders alone.
//! - **removed** are DELETED models (present in the baseline, absent from
//!   the current manifest) the caller derived as the baseline−current
//!   set-diff. They have no current `depends_on`, so they join the node
//!   set as DELETED ghosts but contribute no induced edges.
//!
//! ## Edges
//!
//! The induced subgraph: every model→model `depends_on` edge whose **both**
//! endpoints are in the node set. The graph is acyclic (dbt lineage is a
//! DAG) and deterministically ordered ([`BTreeSet`]/sorted output), the
//! byte-identity-golden requirement the downstream render lane depends on.
//!
//! ## What is deliberately out of this slice
//!
//! Per-node `lines ±` (added/removed counts) need the diff and are a
//! later render-side concern — Slice A is graph topology + node states
//! only. There is no I/O, no parser dependency, no `clap`, no `askama`
//! here: pure domain (`std` + `serde` derive), the same purity contract
//! the rest of `src/domain/` honors.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use crate::domain::manifest::{Manifest, NodeId};
use crate::domain::state::ModelInScopeSet;

/// The lifecycle state of a node in the PR-scope mini-DAG.
///
/// Reuses the scope taxonomy: a [`Self::Modified`] node changed in the PR
/// (and exists in the current manifest); a [`Self::New`] node is a
/// modified node the PR *added* (absent from the baseline); a
/// [`Self::Deleted`] node was removed by the PR (present in the baseline,
/// absent from the current manifest — the baseline−current set-diff).
/// Connector models are not flagged with a state of their own — they are
/// unchanged carriers and are distinguished by [`PrDagNode::is_connector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrDagState {
    /// A modified model the PR added — modified **and** absent from the
    /// baseline.
    New,
    /// A modified model that already existed in the baseline.
    Modified,
    /// A model the PR deleted — present in the baseline, absent from the
    /// current manifest.
    Deleted,
}

impl PrDagState {
    /// The serialized string form (`new` / `modified` / `deleted`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
        }
    }
}

/// One node of the PR-scope mini-DAG — a render-payload POD.
///
/// `Serialize`-only (additive render payload; no `Deserialize` round-trip
/// is needed). `id` is the full manifest node id; `name` is the bare
/// model name (the authored name, version-suffix stripped) the report's
/// model selector keys on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrDagNode {
    /// Full manifest node id (`model.<package>.<name>`).
    pub id: String,
    /// Bare authored model name (selector key — never the version-suffix
    /// leaf of a versioned id).
    pub name: String,
    /// Lifecycle state: new / modified / deleted.
    pub state: PrDagState,
    /// `true` when this node is a *connector* (a between-modified carrier),
    /// not itself a modified/new/deleted node. Mutually exclusive with a
    /// modified/new/deleted state: a connector is always [`PrDagState::Modified`]'s
    /// quiet counterpart — unchanged — and is emitted with
    /// `state = modified` only as a structural placeholder the render lane
    /// renders in the quiet tier when `is_connector` is set.
    pub is_connector: bool,
}

/// One directed edge of the PR-scope mini-DAG (producer → consumer), both
/// endpoints in the node set. A render-payload POD (`Serialize`-only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrDagEdge {
    /// Full manifest node id of the upstream (depended-on) model.
    pub from: String,
    /// Full manifest node id of the downstream (depending) model.
    pub to: String,
}

/// The computed PR-scope mini-DAG — node set + induced edge set, both in
/// deterministic order. A `Serialize`-only render payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct PrDagGraph {
    /// The nodes: modified ∪ connectors ∪ deleted, node-id-ordered.
    pub nodes: Vec<PrDagNode>,
    /// The induced model→model edges among the node set, lexicographically
    /// ordered by `(from, to)`.
    pub edges: Vec<PrDagEdge>,
}

/// Compute the PR-scope mini-DAG for `manifest` given the modified set
/// `modified` (M), the optional `new` subset of M (the models the PR
/// *added* — absent from the baseline), and the `removed` (DELETED) node
/// ids (the baseline−current set-diff).
///
/// `modified` and `removed` are **consumed, not recomputed** — they are
/// the scope layer's own outputs ([`crate::domain::scope::changed_models`]
/// / [`crate::domain::state::StateComparator`] for M; the caller's
/// baseline−current diff for `removed`). `new` distinguishes
/// [`PrDagState::New`] from [`PrDagState::Modified`] within M; an empty
/// `new` collapses every modified node to [`PrDagState::Modified`] (the
/// safe default when the added/changed distinction is not surfaced).
///
/// Connector computation walks the **model→model** lineage graph only —
/// the `resource_type == "model"` projection of every `depends_on` edge —
/// mirroring the scope layer's model-only filter so a generic test / seed
/// / snapshot can never enter the mini-DAG.
#[must_use]
pub fn compute_pr_dag(
    manifest: &Manifest,
    modified: &ModelInScopeSet,
    new: &BTreeSet<NodeId>,
    removed: &[NodeId],
) -> PrDagGraph {
    // The seed set M, restricted to model nodes that exist in the current
    // manifest (a removed node is handled via `removed`, never M).
    let seeds: BTreeSet<&NodeId> = modified
        .iter()
        .filter(|id| is_current_model(manifest, id))
        .collect();

    // The model→model adjacency, both directions, built ONCE
    // (O(N + E)) — the governance precompute-once idiom.
    let adjacency = ModelAdjacency::build(manifest);

    let connectors = connectors_between(&adjacency, &seeds);

    let nodes = assemble_nodes(manifest, &seeds, &connectors, new, removed);
    let edges = induced_edges(&adjacency, &nodes);

    PrDagGraph { nodes, edges }
}

/// `true` when `id` is a `model` node present in the current manifest.
fn is_current_model(manifest: &Manifest, id: &NodeId) -> bool {
    manifest
        .node(id)
        .is_some_and(|node| node.resource_type() == "model")
}

/// Forward (producer → consumers) and backward (consumer → producers)
/// adjacency over the **model→model** subgraph. Built once per
/// `compute_pr_dag` call.
struct ModelAdjacency<'m> {
    /// producer id → model node ids that consume it.
    forward: BTreeMap<&'m NodeId, Vec<&'m NodeId>>,
    /// consumer id → model node ids it depends on.
    backward: BTreeMap<&'m NodeId, Vec<&'m NodeId>>,
}

impl<'m> ModelAdjacency<'m> {
    /// Build both adjacency maps from the manifest's `depends_on` edges,
    /// restricted to edges where **both** endpoints are `model` nodes.
    fn build(manifest: &'m Manifest) -> Self {
        // The model-id membership set, borrowed from the manifest's own
        // node map so every stored reference shares the `'m` lifetime.
        let models: BTreeSet<&'m NodeId> = manifest
            .nodes()
            .iter()
            .filter(|(_, node)| node.resource_type() == "model")
            .map(|(id, _)| id)
            .collect();
        let mut forward: BTreeMap<&NodeId, Vec<&NodeId>> = BTreeMap::new();
        let mut backward: BTreeMap<&NodeId, Vec<&NodeId>> = BTreeMap::new();
        for (consumer_id, node) in manifest.nodes() {
            if node.resource_type() != "model" {
                continue;
            }
            for producer_id in node.depends_on().nodes() {
                // Resolve the producer from the `models` set so the edge
                // is kept only when BOTH endpoints are model nodes and the
                // stored reference is `'m`-lived (the manifest's id, not
                // the consumer's local `depends_on` copy).
                let Some(&producer) = models.get(producer_id) else {
                    continue;
                };
                forward.entry(producer).or_default().push(consumer_id);
                backward.entry(consumer_id).or_default().push(producer);
            }
        }
        Self { forward, backward }
    }
}

/// The connectors: model node ids on a directed lineage path between two
/// *distinct* members of `seeds` — `(DESC(M) ∩ ANC(M)) \ M`, keeping only
/// nodes reached forward from one seed and backward from a different seed.
///
/// Multi-source: a single shared queue + visited set over all of M in each
/// direction collapses the naive `O(|M| × (N + E))` to `O(N + E)`. Each
/// reached non-seed node is tagged with the seed(s) that reached it; the
/// differing-seed predicate (forward-seed ≠ backward-seed for some pair)
/// is what enforces "between" rather than "merely downstream of one".
fn connectors_between<'m>(
    adjacency: &ModelAdjacency<'m>,
    seeds: &BTreeSet<&'m NodeId>,
) -> BTreeSet<&'m NodeId> {
    // Fewer than two seeds ⇒ no "between" is possible.
    if seeds.len() < 2 {
        return BTreeSet::new();
    }
    let desc = reach(seeds, &adjacency.forward);
    let anc = reach(seeds, &adjacency.backward);

    let mut connectors = BTreeSet::new();
    for (&node, forward_seeds) in &desc {
        if seeds.contains(node) {
            continue; // a seed is never its own connector
        }
        let Some(backward_seeds) = anc.get(node) else {
            continue; // not on any upstream path to a seed
        };
        // "Between two DISTINCT modified models": some seed reaches `node`
        // forward and a DIFFERENT seed reaches it backward.
        if has_distinct_pair(forward_seeds, backward_seeds) {
            connectors.insert(node);
        }
    }
    connectors
}

/// Multi-source BFS over `adjacency` from every seed, returning each
/// reached node mapped to the set of seeds that reached it (seeds reach
/// themselves). Cycle-guarded via the visited tagging; deterministic
/// ([`BTreeMap`]/[`BTreeSet`]).
fn reach<'m>(
    seeds: &BTreeSet<&'m NodeId>,
    adjacency: &BTreeMap<&'m NodeId, Vec<&'m NodeId>>,
) -> BTreeMap<&'m NodeId, BTreeSet<&'m NodeId>> {
    let mut reached: BTreeMap<&NodeId, BTreeSet<&NodeId>> = BTreeMap::new();
    for &seed in seeds {
        // One single-source BFS per seed, accumulating the seed tag on
        // every node it reaches. The per-node `reached` tag set is the
        // visited guard: re-tagging a node already carrying this seed is
        // the cycle/duplicate stop.
        let mut queue: VecDeque<&NodeId> = VecDeque::new();
        queue.push_back(seed);
        while let Some(current) = queue.pop_front() {
            if !reached.entry(current).or_default().insert(seed) {
                continue; // already reached from this seed — stop
            }
            for &next in adjacency.get(current).into_iter().flatten() {
                queue.push_back(next);
            }
        }
    }
    reached
}

/// `true` when `forward_seeds` and `backward_seeds` contain two *distinct*
/// elements (so the node lies between two different modified models). A
/// node reached forward and backward from only the *same* single seed is
/// not a connector (it would require a lineage cycle through that seed,
/// which a DAG forbids, but the predicate is explicit for safety).
fn has_distinct_pair(
    forward_seeds: &BTreeSet<&NodeId>,
    backward_seeds: &BTreeSet<&NodeId>,
) -> bool {
    // O(1) instead of the O(|forward| × |backward|) nested scan (gemini on
    // #357), provably equivalent:
    // - either side empty ⇒ no pair exists;
    // - both singletons ⇒ a distinct pair iff the lone elements differ
    //   (for singletons, set inequality ⟺ element inequality);
    // - otherwise one side has ≥2 elements, so a distinct pair is
    //   guaranteed (the other side has ≥1 element, which cannot equal
    //   both of two distinct elements).
    if forward_seeds.is_empty() || backward_seeds.is_empty() {
        return false;
    }
    if forward_seeds.len() == 1 && backward_seeds.len() == 1 {
        return forward_seeds != backward_seeds;
    }
    true
}

/// Assemble the node list: modified seeds (New / Modified) ∪ connectors
/// (quiet, `is_connector`) ∪ removed (Deleted). Node-id-ordered via the
/// [`BTreeMap`] keying, so the output is deterministic.
fn assemble_nodes(
    manifest: &Manifest,
    seeds: &BTreeSet<&NodeId>,
    connectors: &BTreeSet<&NodeId>,
    new: &BTreeSet<NodeId>,
    removed: &[NodeId],
) -> Vec<PrDagNode> {
    // Keyed by node id for deterministic order + idempotent membership
    // (a node is at most one of seed / connector / removed by
    // construction, but the map collapses any accidental overlap to a
    // single deterministic node).
    let mut by_id: BTreeMap<&NodeId, PrDagNode> = BTreeMap::new();

    for &id in seeds {
        let state = if new.contains(id) {
            PrDagState::New
        } else {
            PrDagState::Modified
        };
        by_id.insert(id, pr_dag_node(manifest, id, state, false));
    }
    for &id in connectors {
        // Connectors are unchanged carriers; render-quiet. State is the
        // structural placeholder `Modified` flagged `is_connector`.
        by_id
            .entry(id)
            .or_insert_with(|| pr_dag_node(manifest, id, PrDagState::Modified, true));
    }
    for id in removed {
        // A removed node is absent from the current manifest, so its bare
        // name falls back to the id's leaf-free form (see `bare_name_for`).
        by_id
            .entry(id)
            .or_insert_with(|| pr_dag_node(manifest, id, PrDagState::Deleted, false));
    }

    by_id.into_values().collect()
}

/// Build a single [`PrDagNode`] for `id` with the given state/connector
/// flag, resolving the bare name from the current manifest when present.
fn pr_dag_node(
    manifest: &Manifest,
    id: &NodeId,
    state: PrDagState,
    is_connector: bool,
) -> PrDagNode {
    PrDagNode {
        id: id.as_str().to_owned(),
        name: bare_name_for(manifest, id),
        state,
        is_connector,
    }
}

/// The bare model name for `id`: the manifest node's authored
/// [`crate::domain::manifest::Node::bare_name`] when the node is present
/// (the version-suffix-safe name the selector keys on), else a best-effort
/// fallback derived from the id for a removed node not in the current
/// manifest.
fn bare_name_for(manifest: &Manifest, id: &NodeId) -> String {
    if let Some(node) = manifest.node(id) {
        return node.bare_name().to_owned();
    }
    fallback_bare_name(id.as_str())
}

/// Best-effort bare name for a node id absent from the current manifest (a
/// removed model). dbt model ids are `model.<package>.<name>` or, for a
/// versioned model, `model.<package>.<name>.v<N>`; the authored name is
/// the segment after the package, **not** the trailing version. This
/// recovers `<name>` from the id without a manifest node to consult.
fn fallback_bare_name(id: &str) -> String {
    let segments: Vec<&str> = id.split('.').collect();
    // `model . package . name [ . vN ]` — the authored name is index 2
    // when present; a trailing `vN` is stripped.
    match segments.as_slice() {
        [_kind, _package, name, version] if is_version_suffix(version) => (*name).to_owned(),
        [_kind, _package, name, ..] => (*name).to_owned(),
        // Unrecognized shape: fall back to the whole id (never panics).
        _ => id.to_owned(),
    }
}

/// `true` when `segment` is a dbt version suffix (`v` followed by digits,
/// e.g. `v2`).
fn is_version_suffix(segment: &str) -> bool {
    segment
        .strip_prefix('v')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// The induced edge set: every model→model `depends_on` edge whose both
/// endpoints are in the node set. Sourced from the precomputed forward
/// adjacency, lexicographically ordered by `(from, to)`, deduplicated.
fn induced_edges(adjacency: &ModelAdjacency<'_>, nodes: &[PrDagNode]) -> Vec<PrDagEdge> {
    let in_set: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    // BTreeSet over (from, to) string pairs ⇒ sorted + deduplicated.
    let mut edges: BTreeSet<(&str, &str)> = BTreeSet::new();
    for (&producer, consumers) in &adjacency.forward {
        let producer = producer.as_str();
        if !in_set.contains(producer) {
            continue;
        }
        for &consumer in consumers {
            let consumer = consumer.as_str();
            // Drop a degenerate self-edge (dbt lineage is acyclic, but a
            // malformed manifest must not yield a from==to edge).
            if consumer != producer && in_set.contains(consumer) {
                edges.insert((producer, consumer));
            }
        }
    }
    edges
        .into_iter()
        .map(|(from, to)| PrDagEdge {
            from: from.to_owned(),
            to: to.to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{
        Checksum, DependsOn, ManifestMetadata, Node, NodeConfig, NodeId,
    };
    use std::collections::HashMap;

    // ---- builders -------------------------------------------------------

    /// A `model` node with the given full id and `depends_on` producers
    /// (full node ids of the upstream models it consumes).
    fn model_node(id: &str, producers: &[&str]) -> Node {
        let depends_on = DependsOn::new(
            Vec::new(),
            producers.iter().map(|p| NodeId::new(*p)).collect(),
        );
        Node::new(
            NodeId::new(id),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            None,
            depends_on,
            None,
            NodeConfig::default(),
            None,
            std::collections::BTreeMap::new(),
        )
    }

    /// A node of an arbitrary resource type (for the model-only filter
    /// tests) with `depends_on` producers.
    fn typed_node(id: &str, resource_type: &str, producers: &[&str]) -> Node {
        let depends_on = DependsOn::new(
            Vec::new(),
            producers.iter().map(|p| NodeId::new(*p)).collect(),
        );
        Node::new(
            NodeId::new(id),
            resource_type,
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            None,
            depends_on,
            None,
            NodeConfig::default(),
            None,
            std::collections::BTreeMap::new(),
        )
    }

    /// Build a manifest from `(id, resource_type, producers)` triples.
    fn manifest_of(specs: &[(&str, &str, &[&str])]) -> Manifest {
        let mut nodes = HashMap::new();
        for (id, rt, producers) in specs {
            let node = if *rt == "model" {
                model_node(id, producers)
            } else {
                typed_node(id, rt, producers)
            };
            nodes.insert(NodeId::new(*id), node);
        }
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        )
    }

    /// A modified set (M) from full model ids.
    fn modified_of(ids: &[&str]) -> ModelInScopeSet {
        ids.iter().map(|id| NodeId::new(*id)).collect()
    }

    /// A `new` (added-models) set from full model ids.
    fn new_of(ids: &[&str]) -> BTreeSet<NodeId> {
        ids.iter().map(|id| NodeId::new(*id)).collect()
    }

    /// The node-id set of a computed graph (for membership assertions).
    fn ids_of(graph: &PrDagGraph) -> BTreeSet<&str> {
        graph.nodes.iter().map(|n| n.id.as_str()).collect()
    }

    /// The `(from, to)` edge set of a computed graph.
    fn edges_of(graph: &PrDagGraph) -> BTreeSet<(&str, &str)> {
        graph
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect()
    }

    /// Look up a node by full id in a computed graph.
    fn node_of<'g>(graph: &'g PrDagGraph, id: &str) -> &'g PrDagNode {
        graph
            .nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("node {id} absent from graph"))
    }

    const NO_REMOVED: &[NodeId] = &[];

    // A linear chain  A -> B -> C -> D  (each consumes the previous).
    fn chain_abcd() -> Manifest {
        manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b"]),
            ("model.s.d", "model", &["model.s.c"]),
        ])
    }

    // ---- property: connector ON a path between two modified models ------

    #[test]
    fn node_on_a_path_between_two_modified_models_is_a_connector() {
        // A -> B -> C, with A and C modified. B lies between them ⇒ B is a
        // connector.
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.c"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        let ids = ids_of(&graph);
        assert!(ids.contains("model.s.b"), "B is between A and C");
        let b = node_of(&graph, "model.s.b");
        assert!(b.is_connector, "B is a connector, not a modified node");
        assert_eq!(b.state, PrDagState::Modified, "connector quiet-tier state");

        // The modified seeds are present and NOT connectors.
        for seed in ["model.s.a", "model.s.c"] {
            assert!(ids.contains(seed));
            assert!(!node_of(&graph, seed).is_connector);
        }
    }

    #[test]
    fn a_long_connector_spine_between_two_modified_models_is_fully_included() {
        // A -> B -> C -> D, A and D modified. B and C are both connectors.
        let manifest = chain_abcd();
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.d"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        let ids = ids_of(&graph);
        assert!(ids.contains("model.s.b") && ids.contains("model.s.c"));
        assert!(node_of(&graph, "model.s.b").is_connector);
        assert!(node_of(&graph, "model.s.c").is_connector);
    }

    // ---- property: a node OFF all modified-paths is excluded ------------

    #[test]
    fn a_node_downstream_of_only_one_modified_model_is_not_a_connector() {
        // A (modified) -> B (unmodified leaf). B is 1-hop context, NOT a
        // connector (there is no SECOND modified model on the other side).
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        let ids = ids_of(&graph);
        assert!(ids.contains("model.s.a"));
        assert!(
            !ids.contains("model.s.b"),
            "downstream-of-one is not between"
        );
    }

    #[test]
    fn a_convergence_sink_below_two_modified_models_is_not_a_connector() {
        // A -> C, B -> C, with A and B modified. C is a COMMON DESCENDANT
        // (a sink) of both — it is downstream of two modified models but
        // BETWEEN neither: C ∈ DESC(M) but C ∉ ANC(M). Under the strict
        // (DESC ∩ ANC) \ M definition, a convergence sink is NOT a
        // connector. (A node downstream of modified models is 1-hop
        // context, surfaced later, not a between-connector.)
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &[]),
            ("model.s.c", "model", &["model.s.a", "model.s.b"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.b"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        let ids = ids_of(&graph);
        assert!(
            !ids.contains("model.s.c"),
            "a convergence sink is downstream-of-both, between-neither",
        );
        assert_eq!(ids, BTreeSet::from(["model.s.a", "model.s.b"]));
    }

    #[test]
    fn a_node_off_every_modified_path_is_excluded() {
        // A -> B -> C, A and C modified ⇒ B is the connector. X is an
        // unrelated island that touches no modified-path: it must never
        // appear, and B (genuinely between A and C) must.
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b"]),
            ("model.s.x", "model", &[]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.c"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        let ids = ids_of(&graph);
        assert!(ids.contains("model.s.b"), "B is between A and C");
        assert!(node_of(&graph, "model.s.b").is_connector);
        assert!(!ids.contains("model.s.x"), "unrelated island excluded");
    }

    // ---- property: isolated modified model is still in the node set -----

    #[test]
    fn an_isolated_modified_model_is_shown_alone() {
        // Two modified models with NO lineage path between them: both show,
        // no connectors.
        let manifest = manifest_of(&[("model.s.a", "model", &[]), ("model.s.b", "model", &[])]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.b"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        let ids = ids_of(&graph);
        assert_eq!(ids, BTreeSet::from(["model.s.a", "model.s.b"]));
        assert!(graph.edges.is_empty(), "no path ⇒ no induced edges");
        assert!(graph.nodes.iter().all(|n| !n.is_connector));
    }

    #[test]
    fn a_single_modified_model_is_shown_alone_with_no_connectors() {
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.b"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        let ids = ids_of(&graph);
        assert_eq!(ids, BTreeSet::from(["model.s.b"]));
        assert!(graph.edges.is_empty());
    }

    // ---- property: induced edge set is EXACTLY the model edges in-set ---

    #[test]
    fn induced_edges_are_exactly_the_model_edges_among_the_node_set() {
        // A -> B -> C -> D, A and D modified ⇒ node set {A,B,C,D}; every
        // chain edge is induced.
        let manifest = chain_abcd();
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.d"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert_eq!(
            edges_of(&graph),
            BTreeSet::from([
                ("model.s.a", "model.s.b"),
                ("model.s.b", "model.s.c"),
                ("model.s.c", "model.s.d"),
            ]),
        );
    }

    #[test]
    fn an_edge_to_a_node_outside_the_set_is_not_induced() {
        // A (modified) -> B (unmodified, excluded). The A->B edge must NOT
        // appear (B is not in the node set).
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert!(graph.edges.is_empty(), "B excluded ⇒ A->B not induced");
    }

    #[test]
    fn edges_are_deterministically_ordered() {
        // Diamond: A -> B, A -> C, B -> D, C -> D; A and D modified ⇒ all of
        // B and C are connectors, all four edges induced, sorted by (from,to).
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.a"]),
            ("model.s.d", "model", &["model.s.b", "model.s.c"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.d"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        let ordered: Vec<(&str, &str)> = graph
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect();
        let mut sorted = ordered.clone();
        sorted.sort_unstable();
        assert_eq!(ordered, sorted, "edges emitted in (from,to) order");
        assert_eq!(
            edges_of(&graph),
            BTreeSet::from([
                ("model.s.a", "model.s.b"),
                ("model.s.a", "model.s.c"),
                ("model.s.b", "model.s.d"),
                ("model.s.c", "model.s.d"),
            ]),
        );
    }

    // ---- property: DELETED (removed) nodes appear with state=deleted ----

    #[test]
    fn removed_nodes_appear_with_deleted_state_and_no_edges() {
        // A modified, plus a removed model (absent from current manifest).
        let manifest = manifest_of(&[("model.s.a", "model", &[])]);
        let removed = vec![NodeId::new("model.s.gone")];
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a"]),
            &new_of(&[]),
            &removed,
        );
        let ids = ids_of(&graph);
        assert!(ids.contains("model.s.gone"));
        let gone = node_of(&graph, "model.s.gone");
        assert_eq!(gone.state, PrDagState::Deleted);
        assert!(!gone.is_connector);
        // A removed node has no current depends_on ⇒ contributes no edges.
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn a_removed_versioned_model_recovers_its_authored_name() {
        // model.shop.dim_customers.v2 — bare name is dim_customers, NOT v2.
        let manifest = manifest_of(&[("model.s.a", "model", &[])]);
        let removed = vec![NodeId::new("model.shop.dim_customers.v2")];
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a"]),
            &new_of(&[]),
            &removed,
        );
        let gone = node_of(&graph, "model.shop.dim_customers.v2");
        assert_eq!(gone.name, "dim_customers", "version suffix stripped");
    }

    // ---- property: NEW vs MODIFIED taxonomy -----------------------------

    #[test]
    fn an_added_modified_model_is_new_the_rest_are_modified() {
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.b"]),
            &new_of(&["model.s.b"]),
            NO_REMOVED,
        );
        assert_eq!(node_of(&graph, "model.s.b").state, PrDagState::New);
        assert_eq!(node_of(&graph, "model.s.a").state, PrDagState::Modified);
    }

    #[test]
    fn empty_new_set_collapses_every_modified_node_to_modified() {
        let manifest = manifest_of(&[("model.s.a", "model", &[])]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert_eq!(node_of(&graph, "model.s.a").state, PrDagState::Modified);
    }

    // ---- property: model-only projection --------------------------------

    #[test]
    fn non_model_nodes_never_enter_the_graph_or_its_edges() {
        // A (model, modified) -> t (test) -> C (model, modified). The test
        // node t breaks the MODEL lineage path, so A and C are isolated:
        // no connector through a non-model node, and the test never appears.
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("test.s.t", "test", &["model.s.a"]),
            ("model.s.c", "model", &["test.s.t"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.c"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        let ids = ids_of(&graph);
        assert!(!ids.contains("test.s.t"), "a non-model node never appears");
        // No MODEL path A->C (the only link runs through a test) ⇒ no
        // connector, no induced edges.
        assert!(graph.edges.is_empty());
        assert_eq!(ids, BTreeSet::from(["model.s.a", "model.s.c"]));
    }

    #[test]
    fn a_modified_id_that_is_not_a_current_model_is_dropped_from_seeds() {
        // A modified id pointing at a non-model node (or an absent node) is
        // not a current model ⇒ it is not a seed and contributes nothing
        // (removed-node handling is the `removed` channel, not M).
        let manifest = manifest_of(&[("model.s.a", "model", &[]), ("seed.s.raw", "seed", &[])]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "seed.s.raw", "model.s.absent"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        let ids = ids_of(&graph);
        assert_eq!(ids, BTreeSet::from(["model.s.a"]));
    }

    // ---- property: acyclicity / reflexivity sanity ----------------------

    #[test]
    fn no_self_edge_is_emitted() {
        // A self-referential depends_on (degenerate) must not yield a
        // from==to edge.
        let manifest = manifest_of(&[("model.s.a", "model", &["model.s.a"])]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert!(
            graph.edges.iter().all(|e| e.from != e.to),
            "no self-edge in the induced graph",
        );
    }

    #[test]
    fn the_emitted_graph_is_acyclic() {
        // Every edge of the computed mini-DAG is part of a DAG: a
        // depth-first cycle check over the induced edges finds no back edge.
        let manifest = chain_abcd();
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.d"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert!(is_acyclic(&graph), "induced mini-DAG is acyclic");
    }

    /// A simple Kahn's-algorithm acyclicity check over a computed graph.
    fn is_acyclic(graph: &PrDagGraph) -> bool {
        let mut indegree: BTreeMap<&str, usize> = graph
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), 0usize))
            .collect();
        let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for edge in &graph.edges {
            adj.entry(edge.from.as_str())
                .or_default()
                .push(edge.to.as_str());
            *indegree.entry(edge.to.as_str()).or_default() += 1;
        }
        let mut queue: VecDeque<&str> = indegree
            .iter()
            .filter(|&(_, &d)| d == 0)
            .map(|(&id, _)| id)
            .collect();
        let mut visited = 0usize;
        while let Some(id) = queue.pop_front() {
            visited += 1;
            for &next in adj.get(id).into_iter().flatten() {
                let d = indegree.get_mut(next).expect("edge endpoint is a node");
                *d -= 1;
                if *d == 0 {
                    queue.push_back(next);
                }
            }
        }
        visited == graph.nodes.len()
    }

    // ---- unit: has_distinct_pair (the O(1) connector predicate) ---------

    #[test]
    fn has_distinct_pair_covers_empty_singleton_and_multi_cases() {
        let a = NodeId::new("model.s.a");
        let b = NodeId::new("model.s.b");
        let empty: BTreeSet<&NodeId> = BTreeSet::new();
        let only_a: BTreeSet<&NodeId> = BTreeSet::from([&a]);
        let only_b: BTreeSet<&NodeId> = BTreeSet::from([&b]);
        let a_and_b: BTreeSet<&NodeId> = BTreeSet::from([&a, &b]);

        // Either side empty ⇒ no pair.
        assert!(!has_distinct_pair(&empty, &only_a));
        assert!(!has_distinct_pair(&only_a, &empty));
        assert!(!has_distinct_pair(&empty, &empty));

        // Both singletons, SAME element ⇒ no distinct pair ({a} vs {a}).
        assert!(!has_distinct_pair(&only_a, &only_a));
        // Both singletons, DIFFERENT element ⇒ distinct pair ({a} vs {b}).
        assert!(has_distinct_pair(&only_a, &only_b));

        // One side ≥2 elements ⇒ a distinct pair is guaranteed, regardless
        // of which lone element the other side holds ({a,b} vs {a}).
        assert!(has_distinct_pair(&a_and_b, &only_a));
        assert!(has_distinct_pair(&only_a, &a_and_b));
        assert!(has_distinct_pair(&a_and_b, &a_and_b));
    }

    // ---- property: multi-source BFS == per-seed single-source union -----

    #[test]
    fn connectors_equal_the_oracle_pairwise_intersection() {
        // Cross-validate `connectors_between` against a brute-force oracle:
        // a node is a connector iff for SOME ordered pair (m1, m2) of
        // distinct seeds, it is forward-reachable from m1 and backward-
        // reachable from m2.
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b"]),
            ("model.s.d", "model", &["model.s.c"]),
            ("model.s.e", "model", &["model.s.a"]), // off-spine branch
        ]);
        let seeds_ids = ["model.s.a", "model.s.d"];
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&seeds_ids),
            &new_of(&[]),
            NO_REMOVED,
        );
        let connectors: BTreeSet<&str> = graph
            .nodes
            .iter()
            .filter(|n| n.is_connector)
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(
            connectors,
            BTreeSet::from(["model.s.b", "model.s.c"]),
            "E is off the A->D spine ⇒ not a connector",
        );
    }

    // ---- determinism: output stable across manifest insertion order ----

    #[test]
    fn output_is_independent_of_manifest_insertion_order() {
        let forward = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b"]),
        ]);
        let reversed = manifest_of(&[
            ("model.s.c", "model", &["model.s.b"]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.a", "model", &[]),
        ]);
        let m = modified_of(&["model.s.a", "model.s.c"]);
        let g1 = compute_pr_dag(&forward, &m, &new_of(&[]), NO_REMOVED);
        let g2 = compute_pr_dag(&reversed, &m, &new_of(&[]), NO_REMOVED);
        assert_eq!(g1, g2, "the graph is a pure function of the facts");
    }

    #[test]
    fn an_empty_modified_set_yields_an_empty_graph() {
        let manifest = chain_abcd();
        let graph = compute_pr_dag(&manifest, &modified_of(&[]), &new_of(&[]), NO_REMOVED);
        assert!(graph.nodes.is_empty() && graph.edges.is_empty());
    }
}
