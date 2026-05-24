//! State comparison — dbt `state:modified` diff-scoping (ADR-3).
//!
//! Pure computation over two already-parsed [`Manifest`]s: no I/O, no
//! adapter imports (`tests/domain_clean_arch.rs` greps this file for
//! `use crate::adapters` and fails the build on a hit).
//!
//! [`StateComparator`] holds a `Vec<Box<dyn StateModifier>>` and reports a
//! node modified when *any* registered modifier says so — mirroring dbt's
//! OR semantics across `state:modified` sub-selectors. v0.1 ships exactly
//! one modifier, [`BodyChecksumModifier`], which compares a node's
//! `checksum` between the current manifest and the `--baseline-manifest`.
//!
//! The module surfaces the items the run loop (PR 6) composes:
//!
//! - [`ModifiedSet`] — node ids reported `state:modified`.
//! - [`StateModifier`] + [`ModifierKind`] — the per-sub-selector strategy
//!   seam: object-safe, deliberately not `Send + Sync` (ADR-3; mirrors
//!   the scrap4rs port-conventions decision). Future `.configs` /
//!   `.relation` / `.macros` / `.contract` sub-selectors arrive as
//!   additive `impl StateModifier`s.
//! - [`BodyChecksumModifier`] — the only v0.1 modifier.
//! - [`StateComparator`] — registers modifiers; computes the modified set,
//!   the in-scope unit-test selection, and the in-scope model selection.
//! - [`InScopeSet`] — the unit-test ids the report renders.
//! - [`ModelInScopeSet`] — the model node ids the report renders
//!   (explorer-mode: every model targeted by an in-scope unit test plus
//!   every modified model with zero unit tests targeting it).
//! - [`resolve_target_model`] — maps a unit test's bare `model:` name to
//!   its full manifest node.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::domain::manifest::{Manifest, Node, NodeId};

/// The set of node ids reported as `state:modified` by the
/// [`StateComparator`]. Backed by a [`BTreeSet`] for deterministic
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

    /// Set-theoretic union of two modified sets.
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

/// The dbt `state:modified` sub-selector a [`StateModifier`] implements.
///
/// v0.1 defines only [`ModifierKind::Body`] — the body-checksum subset.
/// dbt's other sub-selectors (`.configs`, `.relation`, `.macros`,
/// `.contract`) become additional variants when their
/// `impl StateModifier`s land (ADR-3); the enum is `#[non_exhaustive]`
/// so that growth is additive for any external matcher.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierKind {
    /// `state:modified.body` — the model body checksum changed.
    Body,
}

/// A single dbt `state:modified` sub-selector.
///
/// Pure computation over two already-parsed domain [`Node`]s — a
/// *strategy*, not an I/O seam, so it lives in `domain` rather than
/// `ports` (ADR-1: ports are for I/O / polymorphic adapter seams).
/// Object-safe and deliberately **not** `Send + Sync`: v0.1 scoping is
/// single-threaded; thread bounds are added at a call site if parallelism
/// ever arrives.
pub trait StateModifier {
    /// Which `state:modified` sub-selector this modifier implements.
    #[must_use]
    fn kind(&self) -> ModifierKind;

    /// `true` when `current` differs from `baseline` under this
    /// modifier's sub-selector. `baseline` is `None` when the node is
    /// absent from the baseline manifest — a newly-added node, always
    /// modified.
    #[must_use]
    fn is_modified(&self, current: &Node, baseline: Option<&Node>) -> bool;
}

/// The only v0.1 [`StateModifier`]: a node is modified when its
/// `checksum` differs from the baseline (ADR-3, `state:modified.body`).
///
/// # v0.1 fidelity limit
///
/// Body-checksum scoping detects model **body** changes only. A pure
/// `.configs` / `.contract` / `.relation` / `.macros` change leaves the
/// body checksum identical, so it is **not** reported as modified. This
/// is a documented, named limit — not a defect; the missing sub-selectors
/// arrive as additive `impl StateModifier`s.
//
// tracked: breezy-bays-labs/cute-dbt#14 — v0.1 body-only scoping;
// .configs and .contract subselectors land with #15
#[derive(Debug, Clone, Copy)]
pub struct BodyChecksumModifier;

impl StateModifier for BodyChecksumModifier {
    fn kind(&self) -> ModifierKind {
        ModifierKind::Body
    }

    fn is_modified(&self, current: &Node, baseline: Option<&Node>) -> bool {
        match baseline {
            None => true,
            Some(baseline) => current.checksum() != baseline.checksum(),
        }
    }
}

/// Strategy holder for `state:modified` scoping (ADR-3).
///
/// Registers a `Vec<Box<dyn StateModifier>>` and reports a node modified
/// when *any* modifier matches (dbt's OR semantics across sub-selectors).
/// The `Box<dyn StateModifier>` field is itself the object-safety pin: a
/// future generic trait method stops this struct from compiling.
pub struct StateComparator {
    modifiers: Vec<Box<dyn StateModifier>>,
}

impl StateComparator {
    /// The v0.1 comparator — a single [`BodyChecksumModifier`].
    #[must_use]
    pub fn body_only() -> Self {
        Self {
            modifiers: vec![Box::new(BodyChecksumModifier)],
        }
    }

    /// Node ids reported `state:modified` — every node in `current` that
    /// any registered modifier flags against `baseline`. Nodes deleted
    /// since the baseline are absent: a deleted node has no `current`
    /// entry and cannot host an in-scope unit test.
    #[must_use]
    pub fn modified_set(&self, current: &Manifest, baseline: &Manifest) -> ModifiedSet {
        let mut modified = ModifiedSet::new();
        for (id, node) in current.nodes() {
            let baseline_node = baseline.node(id);
            if self
                .modifiers
                .iter()
                .any(|modifier| modifier.is_modified(node, baseline_node))
            {
                modified.insert(id.clone());
            }
        }
        modified
    }

    /// Unit-test ids in scope for this diff.
    ///
    /// A unit test is in scope when **either** its target model is in the
    /// modified set (resolved from the bare `model:` name via
    /// [`resolve_target_model`]) **or** the unit test itself was added or
    /// changed relative to the baseline. The second arm is ADR-3's "a
    /// changed test on an unchanged model is in scope": because dbt unit
    /// tests are a top-level manifest map — not checksum-bearing `nodes` —
    /// "the test itself changed" is detected by direct `UnitTest`
    /// inequality between the two manifests, not via the node-keyed
    /// modified set.
    #[must_use]
    pub fn in_scope_unit_tests(&self, current: &Manifest, baseline: &Manifest) -> InScopeSet {
        let modified = self.modified_set(current, baseline);
        Self::in_scope_unit_tests_with_modified(current, baseline, &modified)
    }

    /// Inner implementation — computes in-scope unit tests given a
    /// pre-computed `modified` set. Shared by `in_scope_unit_tests` and
    /// `models_in_scope` so the `modified_set` computation is not
    /// duplicated when both outputs are needed.
    fn in_scope_unit_tests_with_modified(
        current: &Manifest,
        baseline: &Manifest,
        modified: &ModifiedSet,
    ) -> InScopeSet {
        let mut in_scope = InScopeSet::new();
        for (id, unit_test) in current.unit_tests() {
            let target_modified = resolve_target_model(current, unit_test.model())
                .is_some_and(|node| modified.contains(node.id()));
            let test_changed = baseline.unit_test(id) != Some(unit_test);
            if target_modified || test_changed {
                in_scope.ids.insert(id.clone());
            }
        }
        in_scope
    }

    /// Model node ids in scope for this diff (explorer mode, PR C / #30).
    ///
    /// The **union** of two sources, deduplicated and in deterministic
    /// [`BTreeSet`] order:
    ///
    /// 1. Every model that is the resolved target of an in-scope unit test
    ///    (the same models the existing `in_scope_unit_tests` would surface
    ///    via `resolve_target_model`).
    /// 2. Every modified model that has **zero** unit tests targeting it in
    ///    the current manifest — the "no tests wired" signal.
    ///
    /// Together these give the render layer a complete per-model view:
    /// models with tests in scope appear with their tests; modified models
    /// with no tests appear with an explicit empty-test signal.
    #[must_use]
    pub fn models_in_scope(&self, current: &Manifest, baseline: &Manifest) -> ModelInScopeSet {
        // Compute modified_set once and reuse it for both in_scope_unit_tests
        // and the arm-2 no-test check — avoids the redundant second traversal.
        let modified = self.modified_set(current, baseline);
        let in_scope_tests = Self::in_scope_unit_tests_with_modified(current, baseline, &modified);

        // Build a map: resolved model node id → list of unit-test ids that
        // target it in the current manifest.
        let test_targets = unit_test_targets(current);

        let mut ids = BTreeSet::new();

        // Arm 1: every model resolved from an in-scope unit test.
        for test_id in in_scope_tests.iter() {
            let Some(unit_test) = current.unit_test(test_id) else {
                continue;
            };
            if let Some(model) = resolve_target_model(current, unit_test.model()) {
                ids.insert(model.id().clone());
            }
        }

        // Arm 2: every modified model that has zero unit tests targeting it.
        for modified_id in modified.iter() {
            let has_tests = test_targets.get(modified_id).is_some_and(|v| !v.is_empty());
            if !has_tests {
                ids.insert(modified_id.clone());
            }
        }

        ModelInScopeSet { ids }
    }
}

/// Resolve a unit test's `model:` reference to its manifest node.
///
/// dbt records `unit_tests.<id>.model` as the **bare** model name (e.g.
/// `stg_customers`), not the fully-qualified `model.<package>.<name>`
/// node id the `nodes` map is keyed by. This function bridges the gap:
/// it returns the `model` node whose id leaf segment matches `target`.
///
/// v0.1 assumes model names are unique within a single-package manifest
/// (the N=1 use case). Should two packages each define a model with the
/// same leaf name, resolution is still deterministic — the
/// lexicographically smallest node id wins — so the result never depends
/// on `HashMap` iteration order.
#[must_use]
pub fn resolve_target_model<'m>(manifest: &'m Manifest, target: &NodeId) -> Option<&'m Node> {
    let wanted = leaf_segment(target.as_str());
    manifest
        .nodes()
        .values()
        .filter(|node| {
            node.resource_type() == "model" && leaf_segment(node.id().as_str()) == wanted
        })
        .min_by(|a, b| a.id().cmp(b.id()))
}

/// The final `.`-delimited segment of a node id (`model.shop.x` -> `x`).
/// A bare name (no `.`) is returned unchanged.
fn leaf_segment(id: &str) -> &str {
    id.rsplit('.').next().unwrap_or(id)
}

/// Build a map from resolved model node id to the unit-test ids in
/// `manifest` that target it.
///
/// Used by [`StateComparator::models_in_scope`] to determine which
/// modified models have zero unit tests targeting them. Resolution is
/// via [`resolve_target_model`]; unresolvable `model:` references
/// contribute nothing to the map (they are skipped, not failed).
fn unit_test_targets(manifest: &Manifest) -> HashMap<NodeId, Vec<String>> {
    let mut map: HashMap<NodeId, Vec<String>> = HashMap::new();
    for (test_id, unit_test) in manifest.unit_tests() {
        if let Some(model) = resolve_target_model(manifest, unit_test.model()) {
            map.entry(model.id().clone())
                .or_default()
                .push(test_id.clone());
        }
    }
    map
}

/// The diff-scope banner text shown when no unit test is in scope.
///
/// A single shared constant so the CLI banner emitter (PR 6) and the
/// report template (PR 8b) cannot drift apart — the empty-scope contract
/// in `report_generation.feature` is asserted against this exact string.
pub const BANNER_EMPTY_SCOPE: &str = "0 unit tests in scope";

/// The set of unit-test ids in scope for the current diff — the run loop
/// renders exactly these (PR 6 / PR 8b).
///
/// Keyed by the manifest unit-test id (e.g.
/// `unit_test.jaffle_shop.stg_customers.test_…`). Backed by a
/// [`BTreeSet`] for deterministic iteration: the renderer and golden
/// snapshots depend on a stable order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InScopeSet {
    ids: BTreeSet<String>,
}

impl InScopeSet {
    /// Empty set (equivalent to `Default::default`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when no unit test is in scope — the run loop then renders
    /// the empty-but-valid "0 unit tests in scope" report (the
    /// baseline-required policy reserves fail-closed for *unusable*
    /// input, never *empty* scope).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Number of in-scope unit tests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Membership test by unit-test id.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Deterministic iteration over the in-scope unit-test ids
    /// ([`BTreeSet`] ordering).
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.ids.iter().map(String::as_str)
    }
}

impl FromIterator<String> for InScopeSet {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self {
            ids: iter.into_iter().collect(),
        }
    }
}

/// The set of model node ids in scope for the current diff.
///
/// Explorer mode (#30): every model targeted by an in-scope unit test
/// **plus** every modified model that has zero unit tests targeting it in
/// the current manifest. Backed by a [`BTreeSet`] for deterministic
/// iteration: the preflight pass and the renderer depend on a stable order.
///
/// Produced by [`StateComparator::models_in_scope`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelInScopeSet {
    ids: BTreeSet<NodeId>,
}

impl ModelInScopeSet {
    /// Empty set (equivalent to `Default::default`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when no model is in scope.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Number of models in scope.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Membership test by full model node id.
    #[must_use]
    pub fn contains(&self, id: &NodeId) -> bool {
        self.ids.contains(id)
    }

    /// Deterministic iteration over the in-scope model node ids
    /// ([`BTreeSet`] ordering).
    pub fn iter(&self) -> impl Iterator<Item = &NodeId> {
        self.ids.iter()
    }
}

impl FromIterator<NodeId> for ModelInScopeSet {
    fn from_iter<I: IntoIterator<Item = NodeId>>(iter: I) -> Self {
        Self {
            ids: iter.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{Checksum, DependsOn, ManifestMetadata};
    use crate::domain::unit_test::{UnitTest, UnitTestExpect};
    use serde_json::Value;
    use std::collections::HashMap;

    // Object-safety pin. `StateComparator`'s `Vec<Box<dyn StateModifier>>`
    // field already requires this; the const states the intent explicitly
    // so a future generic trait method fails with a clear signal here too
    // (`dyn StateModifier` cannot name a non-object-safe trait).
    const _: fn(&dyn StateModifier) = |_| {};

    fn id(name: &str) -> NodeId {
        NodeId::new(name)
    }

    /// A `model` node with the given full id and body checksum.
    fn model(full_id: &str, checksum: &str) -> Node {
        Node::new(
            NodeId::new(full_id),
            "model",
            Checksum::new("sha256", checksum),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
        )
    }

    /// A node of an arbitrary `resource_type` (for resolution tests).
    fn typed_node(full_id: &str, resource_type: &str) -> Node {
        Node::new(
            NodeId::new(full_id),
            resource_type,
            Checksum::new("sha256", "x"),
            None,
            None,
            DependsOn::default(),
        )
    }

    /// A unit test targeting `model_bare`, carrying `description` so two
    /// otherwise-identical tests can be made to differ.
    fn unit_test_for(model_bare: &str, description: Option<&str>) -> UnitTest {
        UnitTest::new(
            "t",
            NodeId::new(model_bare),
            Vec::new(),
            UnitTestExpect::new(Value::Null, None),
            description.map(str::to_owned),
            DependsOn::default(),
            None,
            None,
            None,
        )
    }

    fn manifest(nodes: Vec<Node>, unit_tests: Vec<(&str, UnitTest)>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            unit_tests
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            HashMap::new(),
        )
    }

    // ===== ModifiedSet =====

    #[test]
    fn new_and_default_are_empty() {
        assert!(ModifiedSet::new().is_empty());
        assert!(ModifiedSet::default().is_empty());
        assert_eq!(ModifiedSet::new().len(), 0);
    }

    #[test]
    fn is_empty_is_false_on_a_non_empty_set() {
        // Kills the `is_empty -> true` mutant: every other ModifiedSet
        // test asserts the `true` direction only.
        let populated = ModifiedSet::from_iter([id("model.shop.a")]);
        assert!(!populated.is_empty());
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
    fn ref_into_iter_yields_every_id() {
        // Kills the `IntoIterator::into_iter -> Default::default()`
        // mutant: `.iter()` exercises the inherent method, not the
        // `IntoIterator for &ModifiedSet` impl.
        let s = ModifiedSet::from_iter([id("a"), id("b")]);
        let collected: Vec<&NodeId> = (&s).into_iter().collect();
        assert_eq!(collected, vec![&id("a"), &id("b")]);
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

    // ===== StateModifier / BodyChecksumModifier =====

    #[test]
    fn body_checksum_modifier_kind_is_body() {
        assert_eq!(BodyChecksumModifier.kind(), ModifierKind::Body);
    }

    #[test]
    fn body_checksum_treats_a_node_absent_from_baseline_as_modified() {
        let current = model("model.shop.new", "aaa");
        assert!(BodyChecksumModifier.is_modified(&current, None));
    }

    #[test]
    fn body_checksum_treats_an_identical_checksum_as_unmodified() {
        let current = model("model.shop.x", "same");
        let baseline = model("model.shop.x", "same");
        assert!(!BodyChecksumModifier.is_modified(&current, Some(&baseline)));
    }

    #[test]
    fn body_checksum_treats_a_differing_checksum_as_modified() {
        let current = model("model.shop.x", "new");
        let baseline = model("model.shop.x", "old");
        assert!(BodyChecksumModifier.is_modified(&current, Some(&baseline)));
    }

    // ===== StateComparator::modified_set =====

    #[test]
    fn modified_set_includes_a_body_changed_model() {
        let current = manifest(vec![model("model.shop.stg_orders", "new")], vec![]);
        let baseline = manifest(vec![model("model.shop.stg_orders", "old")], vec![]);
        let modified = StateComparator::body_only().modified_set(&current, &baseline);
        assert!(modified.contains(&id("model.shop.stg_orders")));
    }

    #[test]
    fn modified_set_excludes_an_unchanged_model() {
        let current = manifest(vec![model("model.shop.stg_customers", "same")], vec![]);
        let baseline = manifest(vec![model("model.shop.stg_customers", "same")], vec![]);
        let modified = StateComparator::body_only().modified_set(&current, &baseline);
        assert!(!modified.contains(&id("model.shop.stg_customers")));
        assert!(modified.is_empty());
    }

    #[test]
    fn modified_set_includes_a_model_absent_from_baseline() {
        let current = manifest(vec![model("model.shop.stg_returns", "x")], vec![]);
        let baseline = manifest(vec![], vec![]);
        let modified = StateComparator::body_only().modified_set(&current, &baseline);
        assert!(modified.contains(&id("model.shop.stg_returns")));
    }

    #[test]
    fn modified_set_is_empty_when_nothing_changed() {
        let current = manifest(
            vec![model("model.shop.a", "a1"), model("model.shop.b", "b1")],
            vec![],
        );
        let modified = StateComparator::body_only().modified_set(&current, &current);
        assert!(modified.is_empty());
    }

    // ===== resolve_target_model =====

    #[test]
    fn resolve_target_model_finds_a_model_by_its_bare_name() {
        let m = manifest(vec![model("model.jaffle_shop.stg_customers", "c")], vec![]);
        let resolved = resolve_target_model(&m, &id("stg_customers"));
        assert_eq!(
            resolved.map(|n| n.id().as_str()),
            Some("model.jaffle_shop.stg_customers"),
        );
    }

    #[test]
    fn resolve_target_model_returns_none_for_an_unknown_name() {
        let m = manifest(vec![model("model.jaffle_shop.stg_customers", "c")], vec![]);
        assert!(resolve_target_model(&m, &id("does_not_exist")).is_none());
    }

    #[test]
    fn resolve_target_model_skips_non_model_nodes_sharing_the_leaf_name() {
        // A seed and a model both end in `.orders`; resolution must pick
        // the model, never the seed.
        let m = manifest(
            vec![
                typed_node("seed.jaffle_shop.orders", "seed"),
                model("model.jaffle_shop.orders", "o"),
            ],
            vec![],
        );
        let resolved = resolve_target_model(&m, &id("orders"));
        assert_eq!(
            resolved.map(|n| n.id().as_str()),
            Some("model.jaffle_shop.orders"),
        );
    }

    #[test]
    fn resolve_target_model_is_deterministic_under_a_name_collision() {
        // Two packages each define `dup`; resolution is deterministic —
        // the lexicographically smallest node id wins regardless of
        // HashMap order.
        let m = manifest(
            vec![model("model.pkg_b.dup", "b"), model("model.pkg_a.dup", "a")],
            vec![],
        );
        let resolved = resolve_target_model(&m, &id("dup"));
        assert_eq!(resolved.map(|n| n.id().as_str()), Some("model.pkg_a.dup"));
    }

    // ===== InScopeSet =====

    #[test]
    fn in_scope_set_new_and_default_are_empty() {
        assert!(InScopeSet::new().is_empty());
        assert!(InScopeSet::default().is_empty());
        assert_eq!(InScopeSet::new().len(), 0);
    }

    #[test]
    fn in_scope_set_reports_membership_and_length() {
        let s = InScopeSet::from_iter(["unit_test.shop.a".to_owned()]);
        assert!(!s.is_empty());
        assert_eq!(s.len(), 1);
        assert!(s.contains("unit_test.shop.a"));
        assert!(!s.contains("unit_test.shop.b"));
    }

    #[test]
    fn in_scope_set_iterates_in_deterministic_order() {
        let s = InScopeSet::from_iter([
            "unit_test.shop.c".to_owned(),
            "unit_test.shop.a".to_owned(),
            "unit_test.shop.b".to_owned(),
        ]);
        let collected: Vec<&str> = s.iter().collect();
        assert_eq!(
            collected,
            vec!["unit_test.shop.a", "unit_test.shop.b", "unit_test.shop.c"],
        );
    }

    // ===== BANNER_EMPTY_SCOPE =====

    #[test]
    fn empty_scope_banner_is_the_locked_contract_string() {
        // report_generation.feature asserts the diff-scope banner
        // "states '0 unit tests in scope'" — pin the exact wording so
        // the CLI emitter and the PR 8b template cannot drift from it.
        assert_eq!(BANNER_EMPTY_SCOPE, "0 unit tests in scope");
    }

    // ===== StateComparator::in_scope_unit_tests — the diff_scoping.feature scenarios =====

    #[test]
    fn a_model_with_a_changed_body_puts_its_unit_test_in_scope() {
        // diff_scoping.feature: "A model whose body changed is in scope".
        // Branch A only — the unit test itself is identical in both.
        let test = unit_test_for("stg_orders", None);
        let current = manifest(
            vec![model("model.shop.stg_orders", "new")],
            vec![("unit_test.shop.stg_orders.t", test.clone())],
        );
        let baseline = manifest(
            vec![model("model.shop.stg_orders", "old")],
            vec![("unit_test.shop.stg_orders.t", test)],
        );
        let in_scope = StateComparator::body_only().in_scope_unit_tests(&current, &baseline);
        assert!(in_scope.contains("unit_test.shop.stg_orders.t"));
    }

    #[test]
    fn an_unchanged_model_keeps_its_unit_test_out_of_scope() {
        // diff_scoping.feature: "A model unchanged in body is out of scope".
        let test = unit_test_for("stg_customers", None);
        let current = manifest(
            vec![model("model.shop.stg_customers", "same")],
            vec![("unit_test.shop.stg_customers.t", test.clone())],
        );
        let baseline = manifest(
            vec![model("model.shop.stg_customers", "same")],
            vec![("unit_test.shop.stg_customers.t", test)],
        );
        let in_scope = StateComparator::body_only().in_scope_unit_tests(&current, &baseline);
        assert!(!in_scope.contains("unit_test.shop.stg_customers.t"));
        assert!(in_scope.is_empty());
    }

    #[test]
    fn a_newly_added_model_puts_its_unit_test_in_scope() {
        // diff_scoping.feature: "A newly added model is in scope".
        // Branch A via the new-model path; the unit test is identical in
        // both manifests so branch B stays silent.
        let test = unit_test_for("stg_returns", None);
        let current = manifest(
            vec![model("model.shop.stg_returns", "x")],
            vec![("unit_test.shop.stg_returns.t", test.clone())],
        );
        let baseline = manifest(vec![], vec![("unit_test.shop.stg_returns.t", test)]);
        let in_scope = StateComparator::body_only().in_scope_unit_tests(&current, &baseline);
        assert!(in_scope.contains("unit_test.shop.stg_returns.t"));
    }

    #[test]
    fn a_changed_unit_test_on_an_unchanged_model_is_in_scope() {
        // diff_scoping.feature: "A changed unit test on an unchanged
        // model is in scope". Branch B only — the model body is identical.
        let current = manifest(
            vec![model("model.shop.stg_customers", "same")],
            vec![(
                "unit_test.shop.stg_customers.t",
                unit_test_for("stg_customers", Some("revised assertion")),
            )],
        );
        let baseline = manifest(
            vec![model("model.shop.stg_customers", "same")],
            vec![(
                "unit_test.shop.stg_customers.t",
                unit_test_for("stg_customers", Some("original assertion")),
            )],
        );
        let in_scope = StateComparator::body_only().in_scope_unit_tests(&current, &baseline);
        assert!(in_scope.contains("unit_test.shop.stg_customers.t"));
    }

    #[test]
    fn a_newly_added_unit_test_is_in_scope() {
        // Branch B via the new-test path: a unit test absent from the
        // baseline is a change this diff introduced. The model body is
        // unchanged, isolating branch B.
        let current = manifest(
            vec![model("model.shop.stg_customers", "same")],
            vec![(
                "unit_test.shop.stg_customers.fresh",
                unit_test_for("stg_customers", None),
            )],
        );
        let baseline = manifest(vec![model("model.shop.stg_customers", "same")], vec![]);
        let in_scope = StateComparator::body_only().in_scope_unit_tests(&current, &baseline);
        assert!(in_scope.contains("unit_test.shop.stg_customers.fresh"));
    }

    #[test]
    fn a_config_only_change_is_not_detected_in_v0_1() {
        // diff_scoping.feature: "A config-only change is NOT detected in
        // v0.1 (documented limit)". A `.configs`-only change leaves the
        // body checksum identical, so body-checksum scoping cannot see
        // it — the named v0.1 fidelity limit (cute-dbt#14).
        let test = unit_test_for("stg_orders", None);
        let current = manifest(
            vec![model("model.shop.stg_orders", "identical")],
            vec![("unit_test.shop.stg_orders.t", test.clone())],
        );
        let baseline = manifest(
            vec![model("model.shop.stg_orders", "identical")],
            vec![("unit_test.shop.stg_orders.t", test)],
        );
        let in_scope = StateComparator::body_only().in_scope_unit_tests(&current, &baseline);
        assert!(!in_scope.contains("unit_test.shop.stg_orders.t"));
    }

    #[test]
    fn in_scope_selection_picks_exactly_the_affected_tests() {
        // Two models — `a` changed, `b` unchanged — each with one unit
        // test identical in both manifests. Only `a`'s test is in scope.
        let test_a = unit_test_for("a", None);
        let test_b = unit_test_for("b", None);
        let current = manifest(
            vec![model("model.shop.a", "a2"), model("model.shop.b", "b1")],
            vec![
                ("unit_test.shop.a.t", test_a.clone()),
                ("unit_test.shop.b.t", test_b.clone()),
            ],
        );
        let baseline = manifest(
            vec![model("model.shop.a", "a1"), model("model.shop.b", "b1")],
            vec![
                ("unit_test.shop.a.t", test_a),
                ("unit_test.shop.b.t", test_b),
            ],
        );
        let in_scope = StateComparator::body_only().in_scope_unit_tests(&current, &baseline);
        assert_eq!(in_scope.len(), 1);
        assert!(in_scope.contains("unit_test.shop.a.t"));
        assert!(!in_scope.contains("unit_test.shop.b.t"));
    }

    // ===== ModelInScopeSet =====

    #[test]
    fn model_in_scope_set_new_and_default_are_empty() {
        assert!(ModelInScopeSet::new().is_empty());
        assert!(ModelInScopeSet::default().is_empty());
        assert_eq!(ModelInScopeSet::new().len(), 0);
    }

    #[test]
    fn model_in_scope_set_is_empty_is_false_on_a_non_empty_set() {
        let s = ModelInScopeSet::from_iter([id("model.shop.a")]);
        assert!(!s.is_empty());
    }

    #[test]
    fn model_in_scope_set_reports_membership_and_length() {
        let s = ModelInScopeSet::from_iter([id("model.shop.a")]);
        assert!(!s.is_empty());
        assert_eq!(s.len(), 1);
        assert!(s.contains(&id("model.shop.a")));
        assert!(!s.contains(&id("model.shop.b")));
    }

    #[test]
    fn model_in_scope_set_iterates_in_deterministic_order() {
        let s = ModelInScopeSet::from_iter([
            id("model.shop.c"),
            id("model.shop.a"),
            id("model.shop.b"),
        ]);
        let collected: Vec<&NodeId> = s.iter().collect();
        assert_eq!(
            collected,
            vec![
                &id("model.shop.a"),
                &id("model.shop.b"),
                &id("model.shop.c")
            ],
        );
    }

    // ===== StateComparator::models_in_scope =====

    #[test]
    fn models_in_scope_includes_target_of_an_in_scope_unit_test() {
        // Arm 1: a model targeted by an in-scope unit test appears in
        // models_in_scope (even though the model itself has tests).
        let test = unit_test_for("stg_orders", None);
        let current = manifest(
            vec![model("model.shop.stg_orders", "new")],
            vec![("unit_test.shop.stg_orders.t", test.clone())],
        );
        let baseline = manifest(
            vec![model("model.shop.stg_orders", "old")],
            vec![("unit_test.shop.stg_orders.t", test)],
        );
        let models = StateComparator::body_only().models_in_scope(&current, &baseline);
        assert!(models.contains(&id("model.shop.stg_orders")));
        assert_eq!(models.len(), 1);
    }

    #[test]
    fn models_in_scope_includes_a_modified_model_with_zero_unit_tests() {
        // Arm 2: a modified model with no unit tests targeting it in the
        // current manifest is included in models_in_scope.
        let current = manifest(
            vec![model("model.shop.stg_orders", "new")],
            vec![], // zero unit tests
        );
        let baseline = manifest(vec![model("model.shop.stg_orders", "old")], vec![]);
        let models = StateComparator::body_only().models_in_scope(&current, &baseline);
        assert!(models.contains(&id("model.shop.stg_orders")));
        assert_eq!(models.len(), 1);
    }

    #[test]
    fn models_in_scope_deduplicates_when_model_has_in_scope_test_and_is_modified() {
        // A model that is BOTH the target of an in-scope unit test AND
        // is modified with tests present appears exactly once in models_in_scope.
        // (This exercises the dedup between arm 1 and arm 2 is a non-issue
        // when the model has tests — arm 2 would be suppressed.)
        let test = unit_test_for("stg_orders", None);
        let current = manifest(
            vec![model("model.shop.stg_orders", "new")],
            vec![("unit_test.shop.stg_orders.t", test.clone())],
        );
        let baseline = manifest(
            vec![model("model.shop.stg_orders", "old")],
            vec![("unit_test.shop.stg_orders.t", test)],
        );
        let models = StateComparator::body_only().models_in_scope(&current, &baseline);
        assert_eq!(models.len(), 1, "deduplication: model counted once");
        assert!(models.contains(&id("model.shop.stg_orders")));
    }

    #[test]
    fn models_in_scope_is_empty_when_nothing_changed() {
        // An unchanged model with a unit test that itself is also unchanged
        // produces an empty models_in_scope.
        let test = unit_test_for("stg_customers", None);
        let current = manifest(
            vec![model("model.shop.stg_customers", "same")],
            vec![("unit_test.shop.stg_customers.t", test.clone())],
        );
        let baseline = manifest(
            vec![model("model.shop.stg_customers", "same")],
            vec![("unit_test.shop.stg_customers.t", test)],
        );
        let models = StateComparator::body_only().models_in_scope(&current, &baseline);
        assert!(models.is_empty());
    }

    #[test]
    fn models_in_scope_excludes_an_unchanged_model_with_zero_unit_tests() {
        // Arm 2 is gated on the model being modified. An unchanged model
        // with zero tests does NOT appear in models_in_scope.
        let current = manifest(vec![model("model.shop.stg_customers", "same")], vec![]);
        let baseline = manifest(vec![model("model.shop.stg_customers", "same")], vec![]);
        let models = StateComparator::body_only().models_in_scope(&current, &baseline);
        assert!(models.is_empty());
    }

    #[test]
    fn models_in_scope_union_covers_both_arms_simultaneously() {
        // Two models:
        // - `has_test` is modified and has a unit test in scope (arm 1).
        // - `no_test` is modified and has zero unit tests (arm 2).
        // Both must appear in models_in_scope; total = 2.
        let test = unit_test_for("has_test", None);
        let current = manifest(
            vec![
                model("model.shop.has_test", "new"),
                model("model.shop.no_test", "new"),
            ],
            vec![("unit_test.shop.has_test.t", test.clone())],
        );
        let baseline = manifest(
            vec![
                model("model.shop.has_test", "old"),
                model("model.shop.no_test", "old"),
            ],
            vec![("unit_test.shop.has_test.t", test)],
        );
        let models = StateComparator::body_only().models_in_scope(&current, &baseline);
        assert_eq!(models.len(), 2);
        assert!(models.contains(&id("model.shop.has_test")));
        assert!(models.contains(&id("model.shop.no_test")));
    }

    #[test]
    fn models_in_scope_iterates_in_deterministic_model_id_order() {
        // Two no-test modified models; iteration order must be BTreeSet
        // (lexicographic NodeId) order.
        let current = manifest(
            vec![
                model("model.shop.zzz", "new"),
                model("model.shop.aaa", "new"),
            ],
            vec![],
        );
        let baseline = manifest(
            vec![
                model("model.shop.zzz", "old"),
                model("model.shop.aaa", "old"),
            ],
            vec![],
        );
        let models = StateComparator::body_only().models_in_scope(&current, &baseline);
        let collected: Vec<&NodeId> = models.iter().collect();
        assert_eq!(
            collected,
            vec![&id("model.shop.aaa"), &id("model.shop.zzz")],
        );
    }

    #[test]
    fn unit_test_targets_maps_model_id_to_test_ids() {
        // Direct test of `unit_test_targets`: ensures the function is not
        // replaced by `HashMap::new()` (which would produce an empty map,
        // letting arm 2 spuriously insert every modified model even those
        // with tests).
        let test = unit_test_for("stg_orders", None);
        let m = manifest(
            vec![model("model.shop.stg_orders", "x")],
            vec![
                ("unit_test.shop.stg_orders.t1", test.clone()),
                ("unit_test.shop.stg_orders.t2", test),
            ],
        );
        let targets = unit_test_targets(&m);
        let entry = targets
            .get(&id("model.shop.stg_orders"))
            .expect("model.shop.stg_orders is in the targets map");
        assert_eq!(entry.len(), 2, "two tests registered for the model");
        // A model with zero tests is absent from the map.
        assert!(
            !targets.contains_key(&id("model.shop.other")),
            "model with no tests has no entry",
        );
    }

    #[test]
    fn unit_test_targets_returns_empty_for_manifest_with_no_unit_tests() {
        // Explicit empty-map case: no unit tests → empty targets.
        // Kills the `unit_test_targets -> HashMap::new()` mutant when
        // combined with the non-empty case above — a manifest with tests
        // must produce a non-empty map.
        let m = manifest(vec![model("model.shop.stg_orders", "x")], vec![]);
        assert!(unit_test_targets(&m).is_empty());
    }

    #[test]
    fn models_in_scope_does_not_include_an_unresolvable_unit_test_target() {
        // A unit test whose model: reference cannot be resolved (no
        // matching model node) contributes nothing to models_in_scope.
        let current = manifest(
            vec![],
            vec![("unit_test.shop.ghost", unit_test_for("missing_model", None))],
        );
        let baseline = manifest(
            vec![],
            vec![], // ghost test is new → in_scope, but no resolvable target
        );
        let models = StateComparator::body_only().models_in_scope(&current, &baseline);
        assert!(models.is_empty());
    }
}
