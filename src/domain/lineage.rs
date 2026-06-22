//! The full-manifest cross-model lineage — the ONE source of the
//! `depends_on` self-inversion (cute-dbt#443, the source-map-spine S0
//! prep slice).
//!
//! Pure POD + serde derive only (`tests/domain_clean_arch.rs` gate — no
//! parser, no I/O). Before this slice, three consumers each iterated
//! `manifest.nodes()` and inverted `depends_on` *independently* over
//! different node sets:
//!
//! - the explorer lineage (`adapters::explore::build_lineage`) — a
//!   forward-edge build over a typed node union;
//! - the PR mini-DAG (`domain::pr_dag::compute_pr_dag`) — a model→model
//!   adjacency;
//! - governance blast-radius (`domain::governance::reverse_node_adjacency`)
//!   — the reverse (producer→consumer) adjacency over ALL nodes.
//!
//! S0 hoists that inversion into a single full-manifest fact computed
//! ONCE ([`ModelLineage`]). Scope is applied at READ time
//! (scope-as-parameter, the ir-plan derive-forward rule) — never folded
//! back into the stored fact. The three consumers become FILTERS /
//! direction-reads over this one source and NEVER re-invert `depends_on`.
//!
//! The single inversion site is [`invert_depends_on`]: the ONE function
//! whose body reads `depends_on().nodes()` to build the
//! producer→consumer adjacency. [`ModelLineage::from_manifest`] owns the
//! spine fact (serde-round-trippable); the borrowed-view callers read
//! [`invert_depends_on`] directly. The falsifiable seam test
//! (`tests/lineage_seam.rs`) asserts no consumer re-inverts.

use crate::domain::manifest::{Manifest, NodeId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The ONE full-manifest cross-model lineage fact: every node's
/// `depends_on` self-inverted ONCE, in both directions. This is the
/// spine fact (`DagFacts.lineage` in the ir-plan home); the three
/// consumers read/filter/direction it instead of each re-inverting.
///
/// `forward` is the **inverted** adjacency (producer → the node ids that
/// consume it — the reverse of every `depends_on.nodes` edge); `backward`
/// is the forward `depends_on` adjacency, indexed (consumer → the node
/// ids it depends on). Both are derived from the SAME single pass over
/// the manifest (see [`invert_depends_on`]).
///
/// Owned + serde-round-trippable (the spine-fact commitment, ruling C6 —
/// `--context-out` round-trips). The values are node-id-ordered
/// ([`BTreeMap`]); each adjacency list preserves the manifest's
/// `depends_on` order (de-dup is the consumers' job, exactly as before
/// this hoist).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModelLineage {
    /// Producer id → the node ids that consume it (the INVERTED edges —
    /// the reverse of every `depends_on.nodes` edge). The governance
    /// blast-radius reverse read; the PR mini-DAG's `forward` view.
    forward: BTreeMap<NodeId, Vec<NodeId>>,
    /// Consumer id → the node ids it depends on (the forward
    /// `depends_on` adjacency, indexed). The PR mini-DAG's `backward`
    /// view.
    backward: BTreeMap<NodeId, Vec<NodeId>>,
}

impl ModelLineage {
    /// Build the ONE full-manifest lineage fact: invert every node's
    /// `depends_on` exactly once. This is the permanent domain home of
    /// the cross-model lineage; the three consumers read it (or its
    /// borrowed twin [`invert_depends_on`]) rather than re-inverting.
    #[must_use]
    pub fn from_manifest(manifest: &Manifest) -> Self {
        let mut forward: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
        let mut backward: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
        for (consumer_id, node) in manifest.nodes() {
            for producer_id in node.depends_on().nodes() {
                forward
                    .entry(producer_id.clone())
                    .or_default()
                    .push(consumer_id.clone());
                backward
                    .entry(consumer_id.clone())
                    .or_default()
                    .push(producer_id.clone());
            }
        }
        Self { forward, backward }
    }

    /// The inverted adjacency: producer id → the node ids that consume
    /// it. The governance blast-radius reverse read.
    #[must_use]
    pub fn forward(&self) -> &BTreeMap<NodeId, Vec<NodeId>> {
        &self.forward
    }

    /// The forward `depends_on` adjacency, indexed: consumer id → the
    /// node ids it depends on.
    #[must_use]
    pub fn backward(&self) -> &BTreeMap<NodeId, Vec<NodeId>> {
        &self.backward
    }
}

/// THE single `depends_on` inversion site (cute-dbt#443). Returns the
/// reversed node-dependency adjacency — producer id → the node ids that
/// consume it (the reverse of every `depends_on.nodes` edge) — as a
/// borrowed view tied to `manifest`, the zero-alloc shape the
/// precompute-once governance loop and the PR-DAG model view both need.
///
/// This is the ONLY function in the crate whose body reads
/// `depends_on().nodes()` to build a producer→consumer adjacency. Every
/// consumer that needs the inversion calls this (or holds a
/// [`ModelLineage`] built from it) — none re-implements the loop. That
/// is the falsifiable seam (`tests/lineage_seam.rs`): the inversion
/// happens exactly once.
///
/// Determinism: keyed by a [`BTreeMap`] (producer-id order); each
/// adjacency list preserves the manifest's `depends_on` iteration order
/// — identical to the pre-hoist `reverse_node_adjacency`, so every
/// downstream walk (BFS visited order, de-dup) is byte-stable.
#[must_use]
pub fn invert_depends_on(manifest: &Manifest) -> BTreeMap<&NodeId, Vec<&NodeId>> {
    let mut consumers_of: BTreeMap<&NodeId, Vec<&NodeId>> = BTreeMap::new();
    for (consumer_id, node) in manifest.nodes() {
        for producer_id in node.depends_on().nodes() {
            consumers_of
                .entry(producer_id)
                .or_default()
                .push(consumer_id);
        }
    }
    consumers_of
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{Checksum, DependsOn, ManifestMetadata, Node, NodeConfig};
    use std::collections::{BTreeSet, HashMap};

    /// A node of an arbitrary resource type with `depends_on` producers.
    fn node(id: &str, resource_type: &str, producers: &[&str]) -> Node {
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
            nodes.insert(NodeId::new(*id), node(id, rt, producers));
        }
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        )
    }

    /// The pre-hoist `reverse_node_adjacency` body, INLINED here as the
    /// behaviour-preservation oracle (cute-dbt#443): the new single
    /// inversion site must produce byte-identical output to the loop the
    /// three consumers each ran before the hoist.
    fn legacy_reverse(manifest: &Manifest) -> BTreeMap<&NodeId, Vec<&NodeId>> {
        let mut consumers_of: BTreeMap<&NodeId, Vec<&NodeId>> = BTreeMap::new();
        for (consumer_id, node) in manifest.nodes() {
            for producer_id in node.depends_on().nodes() {
                consumers_of
                    .entry(producer_id)
                    .or_default()
                    .push(consumer_id);
            }
        }
        consumers_of
    }

    fn nid(s: &str) -> NodeId {
        NodeId::new(s)
    }

    // ---- the inversion is correct + behaviour-preserving ----------------

    /// `invert_depends_on` byte-equals the pre-hoist `reverse_node_adjacency`
    /// loop over a mixed-type manifest (TDD test 4 — the governance reverse
    /// read equals pre-refactor).
    #[test]
    fn invert_equals_legacy_reverse_over_mixed_manifest() {
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b", "model.s.a"]),
            ("snapshot.s.snap", "snapshot", &["model.s.b"]),
            ("seed.s.raw", "seed", &[]),
            ("test.s.t", "test", &["model.s.c"]),
        ]);
        assert_eq!(invert_depends_on(&manifest), legacy_reverse(&manifest));
    }

    /// `forward` is the producer→consumer inversion of `depends_on`; it is
    /// byte-equal to the borrowed `invert_depends_on` view (the one source).
    #[test]
    fn from_manifest_forward_equals_invert_view() {
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b", "model.s.a"]),
        ]);
        let lineage = ModelLineage::from_manifest(&manifest);
        // Materialize the borrowed view into the owned shape for comparison.
        let view: BTreeMap<NodeId, Vec<NodeId>> = invert_depends_on(&manifest)
            .into_iter()
            .map(|(k, v)| (k.clone(), v.into_iter().cloned().collect()))
            .collect();
        assert_eq!(*lineage.forward(), view);
    }

    /// `backward` is the forward `depends_on` adjacency, indexed: each
    /// consumer maps to its producers in `depends_on` order.
    #[test]
    fn from_manifest_backward_indexes_depends_on() {
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b", "model.s.a"]),
        ]);
        let lineage = ModelLineage::from_manifest(&manifest);
        assert_eq!(
            lineage.backward().get(&nid("model.s.c")),
            Some(&vec![nid("model.s.b"), nid("model.s.a")]),
        );
        assert_eq!(
            lineage.backward().get(&nid("model.s.b")),
            Some(&vec![nid("model.s.a")]),
        );
        // A root (no producers) has no backward entry.
        assert_eq!(lineage.backward().get(&nid("model.s.a")), None);
    }

    /// The inversion: `model.s.a` is consumed by `b` and `c`.
    #[test]
    fn forward_lists_consumers() {
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b", "model.s.a"]),
        ]);
        let lineage = ModelLineage::from_manifest(&manifest);
        let consumers: BTreeSet<&str> = lineage
            .forward()
            .get(&nid("model.s.a"))
            .into_iter()
            .flatten()
            .map(NodeId::as_str)
            .collect();
        assert_eq!(consumers, BTreeSet::from(["model.s.b", "model.s.c"]),);
    }

    /// An empty manifest yields empty maps (no panic, default).
    #[test]
    fn empty_manifest_is_empty_lineage() {
        let manifest = manifest_of(&[]);
        let lineage = ModelLineage::from_manifest(&manifest);
        assert!(lineage.forward().is_empty());
        assert!(lineage.backward().is_empty());
        assert_eq!(lineage, ModelLineage::default());
    }

    // ---- the spine-fact serde round-trip (TDD test 5) -------------------

    /// `ModelLineage` is a spine fact: it serde-round-trips exactly
    /// (cute-dbt#443 — `--context-out` round-trips, ruling C6).
    #[test]
    fn serde_round_trips() {
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b", "model.s.a"]),
            ("snapshot.s.snap", "snapshot", &["model.s.b"]),
        ]);
        let lineage = ModelLineage::from_manifest(&manifest);
        let json = serde_json::to_string(&lineage).expect("serialize");
        let back: ModelLineage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(lineage, back);
    }

    /// Empty `ModelLineage` round-trips (default is serde-stable).
    #[test]
    fn empty_serde_round_trips() {
        let lineage = ModelLineage::default();
        let json = serde_json::to_string(&lineage).expect("serialize");
        let back: ModelLineage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(lineage, back);
    }
}
