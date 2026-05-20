//! `ModifiedSet` — the output of the `StateComparator` (PR 5).
//!
//! This module **only** introduces the data shape; the `StateModifier`
//! trait, `BodyChecksumModifier` impl, `StateComparator::body_only`
//! constructor, and `modified_set` union live in PR 5 per the plan.
//!
//! The set has set semantics over [`NodeId`]: callers add ids,
//! check membership, iterate, and union two sets (PR 5 builds a
//! `ModifiedSet` per registered `StateModifier` and unions the results
//! to mirror dbt's OR semantics across sub-selectors per ADR-3).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::domain::manifest::NodeId;

/// The set of node ids reported as `state:modified` by the
/// `StateComparator`. Backed by a [`BTreeSet`] for deterministic
/// iteration order (renderer + golden snapshots in PR 8b / PR 10 depend
/// on it).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModifiedSet {
    ids: BTreeSet<NodeId>,
}

impl ModifiedSet {
    /// Empty set (equivalent to `Default::default`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a node id. Returns `true` if the id was not already
    /// present (mirrors `BTreeSet::insert`).
    pub fn insert(&mut self, id: NodeId) -> bool {
        self.ids.insert(id)
    }

    /// `true` when this set carries no node ids.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Number of node ids in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Membership test.
    #[must_use]
    pub fn contains(&self, id: &NodeId) -> bool {
        self.ids.contains(id)
    }

    /// Deterministic iteration order ([`BTreeSet`] ordering).
    pub fn iter(&self) -> impl Iterator<Item = &NodeId> {
        self.ids.iter()
    }

    /// Set-theoretic union — used by the `StateComparator` (PR 5) to OR
    /// together the per-modifier modified sets per ADR-3.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        Self {
            ids: self.ids.union(&other.ids).cloned().collect(),
        }
    }
}

impl FromIterator<NodeId> for ModifiedSet {
    fn from_iter<I: IntoIterator<Item = NodeId>>(iter: I) -> Self {
        Self {
            ids: iter.into_iter().collect(),
        }
    }
}

impl<'a> IntoIterator for &'a ModifiedSet {
    type Item = &'a NodeId;
    type IntoIter = std::collections::btree_set::Iter<'a, NodeId>;

    fn into_iter(self) -> Self::IntoIter {
        self.ids.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> NodeId {
        NodeId::new(name)
    }

    #[test]
    fn new_and_default_are_empty() {
        assert!(ModifiedSet::new().is_empty());
        assert!(ModifiedSet::default().is_empty());
        assert_eq!(ModifiedSet::new().len(), 0);
    }

    #[test]
    fn insert_reports_freshness() {
        let mut s = ModifiedSet::new();
        assert!(s.insert(id("model.shop.a")));
        assert!(!s.insert(id("model.shop.a")));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn contains_reflects_membership() {
        let s = ModifiedSet::from_iter([id("a"), id("b")]);
        assert!(s.contains(&id("a")));
        assert!(s.contains(&id("b")));
        assert!(!s.contains(&id("c")));
    }

    #[test]
    fn iter_is_deterministic_btreeset_order() {
        let s = ModifiedSet::from_iter([id("c"), id("a"), id("b")]);
        let collected: Vec<&NodeId> = s.iter().collect();
        assert_eq!(collected, vec![&id("a"), &id("b"), &id("c")]);
    }

    #[test]
    fn union_merges_two_sets_without_duplicates() {
        let a = ModifiedSet::from_iter([id("a"), id("b")]);
        let b = ModifiedSet::from_iter([id("b"), id("c")]);
        let u = a.union(&b);
        assert_eq!(u.len(), 3);
        assert!(u.contains(&id("a")));
        assert!(u.contains(&id("b")));
        assert!(u.contains(&id("c")));
    }

    #[test]
    fn union_is_commutative_on_membership() {
        let a = ModifiedSet::from_iter([id("a")]);
        let b = ModifiedSet::from_iter([id("b")]);
        assert_eq!(a.union(&b), b.union(&a));
    }

    #[test]
    fn serde_roundtrip_is_transparent_array() {
        let s = ModifiedSet::from_iter([id("a"), id("b")]);
        let json = serde_json::to_string(&s).unwrap();
        // Transparent over BTreeSet -> JSON array of NodeId strings.
        assert_eq!(json, "[\"a\",\"b\"]");
        let back: ModifiedSet = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
