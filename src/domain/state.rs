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
use crate::domain::unit_test::UnitTest;

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
/// v0.1 shipped only [`ModifierKind::Body`] — the body-checksum subset.
/// The v0.2 sub-selectors (`.configs`, `.relation`, `.macros`,
/// `.contract`) land here as additional variants alongside their
/// `impl StateModifier`s (ADR-3, cute-dbt#17); the enum is
/// `#[non_exhaustive]` so that growth is additive for any external
/// matcher.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierKind {
    /// `state:modified.body` — the model body checksum changed.
    Body,
    /// `state:modified.configs` — the resolved config block changed.
    Configs,
    /// `state:modified.relation` — the fully-qualified relation name
    /// (database / schema / alias / identifier) changed.
    Relation,
    /// `state:modified.macros` — the set of upstream macros the node
    /// depends on changed.
    Macros,
    /// `state:modified.contract` — the data contract changed
    /// (`config.contract.enforced` or the column set).
    Contract,
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

/// The v0.1 [`StateModifier`]: a node is modified when its `checksum`
/// differs from the baseline (ADR-3, `state:modified.body`).
///
/// # Sub-selector companions (cute-dbt#17)
///
/// Body-checksum scoping detects model **body** changes only — a pure
/// `.configs` / `.contract` / `.relation` / `.macros` change leaves the
/// body checksum identical, so [`BodyChecksumModifier`] alone does not
/// report it. The v0.1 fidelity limit that tracked (cute-dbt#14, now
/// resolved) is lifted by the four additive companion modifiers below —
/// [`ConfigsModifier`], [`RelationModifier`], [`MacrosModifier`],
/// [`ContractModifier`] — registered via
/// [`StateComparator::with_sub_selectors`]. The default
/// [`StateComparator::body_only`] comparator is unchanged: callers opt in
/// to the wider fidelity explicitly.
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

/// `state:modified.configs` — a node is modified when its resolved
/// `config` block differs from the baseline (cute-dbt#17).
///
/// The comparison is over the whole config dict: the **key set and value
/// set** of [`NodeConfig::config`](crate::domain::manifest::NodeConfig::config),
/// stored as a `BTreeMap` so a reordering of keys between two manifests
/// is *not* a change. A new node (absent from the baseline) is modified.
///
/// dbt's own `.configs` selector diffs the *unrendered* config; this
/// modifier diffs the **resolved** `config` dict the manifest carries.
/// The resolved dict is broader, so this can over-report relative to dbt
/// (e.g. an environment-driven config value that resolved differently
/// flags as a change where dbt's unrendered diff would not). That is an
/// accepted trade for the opt-in wider scope — it never *misses* a config
/// change, and it catches the pure config-only changes
/// [`BodyChecksumModifier`] cannot see.
#[derive(Debug, Clone, Copy)]
pub struct ConfigsModifier;

impl StateModifier for ConfigsModifier {
    fn kind(&self) -> ModifierKind {
        ModifierKind::Configs
    }

    fn is_modified(&self, current: &Node, baseline: Option<&Node>) -> bool {
        match baseline {
            None => true,
            Some(baseline) => current.config().config() != baseline.config().config(),
        }
    }
}

/// `state:modified.relation` — a node is modified when its
/// fully-qualified relation name changed (cute-dbt#17).
///
/// dbt records the relation as a single
/// `"database"."schema"."identifier"` string
/// ([`Node::relation_name`](crate::domain::manifest::Node::relation_name))
/// that encodes all four of database / schema / alias / identifier
/// together — so comparing the one field detects a change in *any* of
/// them, matching dbt's own relation diff. A new node (absent from the
/// baseline) is modified.
#[derive(Debug, Clone, Copy)]
pub struct RelationModifier;

impl StateModifier for RelationModifier {
    fn kind(&self) -> ModifierKind {
        ModifierKind::Relation
    }

    fn is_modified(&self, current: &Node, baseline: Option<&Node>) -> bool {
        match baseline {
            None => true,
            Some(baseline) => current.relation_name() != baseline.relation_name(),
        }
    }
}

/// `state:modified.macros` — a node is modified when the set of upstream
/// macros it depends on diverges from the baseline (cute-dbt#17).
///
/// The comparison is over
/// [`DependsOn::macros`](crate::domain::manifest::DependsOn::macros) — the
/// macro ids the node references — compared as a **set** (order- and
/// duplicate-independent). A new node (absent from the baseline) is
/// modified.
///
/// # v0.2 fidelity limit
///
/// A [`StateModifier`] sees two [`Node`]s, never the two manifests, so it
/// cannot compare macro **bodies** (which live in `Manifest.macros`). dbt
/// proper re-flags a node when a depended-on macro's *body* changes even
/// if the dependency set is identical; cute-dbt v0.2 detects only a
/// change in the depended-on macro *set*. This is a documented, named
/// limit — not a defect — and lifting it would require widening the
/// trait signature, the comparator/scoping rewrite ADR-3 forbids.
#[derive(Debug, Clone, Copy)]
pub struct MacrosModifier;

impl StateModifier for MacrosModifier {
    fn kind(&self) -> ModifierKind {
        ModifierKind::Macros
    }

    fn is_modified(&self, current: &Node, baseline: Option<&Node>) -> bool {
        match baseline {
            None => true,
            Some(baseline) => {
                let current_macros: BTreeSet<&str> = current
                    .depends_on()
                    .macros()
                    .iter()
                    .map(String::as_str)
                    .collect();
                let baseline_macros: BTreeSet<&str> = baseline
                    .depends_on()
                    .macros()
                    .iter()
                    .map(String::as_str)
                    .collect();
                current_macros != baseline_macros
            }
        }
    }
}

/// `state:modified.contract` — a node is modified when its data contract
/// changed (cute-dbt#17).
///
/// "The contract changed" is two diffs, OR'd:
///
/// 1. `config.contract.enforced`
///    ([`NodeConfig::contract_enforced`](crate::domain::manifest::NodeConfig::contract_enforced))
///    flipped — the model started or stopped enforcing a contract.
/// 2. The **column set** ([`Node::columns`](crate::domain::manifest::Node::columns),
///    name → declared `data_type`) changed — a column added, removed,
///    renamed, or re-typed.
///
/// A new node (absent from the baseline) is modified. The column set is a
/// `BTreeMap`, so the comparison is order-independent.
#[derive(Debug, Clone, Copy)]
pub struct ContractModifier;

impl StateModifier for ContractModifier {
    fn kind(&self) -> ModifierKind {
        ModifierKind::Contract
    }

    fn is_modified(&self, current: &Node, baseline: Option<&Node>) -> bool {
        match baseline {
            None => true,
            Some(baseline) => {
                current.config().contract_enforced() != baseline.config().contract_enforced()
                    || current.columns() != baseline.columns()
            }
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
    ///
    /// This is the default scoping fidelity and the one the run loop
    /// wires; the wider [`Self::with_sub_selectors`] comparator is opt-in.
    #[must_use]
    pub fn body_only() -> Self {
        Self {
            modifiers: vec![Box::new(BodyChecksumModifier)],
        }
    }

    /// The full v0.1 + v0.2 comparator (cute-dbt#17) — registers
    /// [`BodyChecksumModifier`] plus the four v0.2 sub-selectors
    /// ([`ConfigsModifier`], [`RelationModifier`], [`MacrosModifier`],
    /// [`ContractModifier`]). Union semantics are preserved: a node is
    /// modified when **any** registered modifier flags it (dbt's OR
    /// across `state:modified` sub-selectors).
    ///
    /// This is the additive extension ADR-3's revisit condition locks: a
    /// new sub-selector is a new `impl StateModifier` registered here,
    /// never a comparator / domain / scoping rewrite. It does **not**
    /// replace [`Self::body_only`] — callers opt in to the wider fidelity
    /// explicitly; the default run-loop scoping is unchanged.
    #[must_use]
    pub fn with_sub_selectors() -> Self {
        Self {
            modifiers: vec![
                Box::new(BodyChecksumModifier),
                Box::new(ConfigsModifier),
                Box::new(RelationModifier),
                Box::new(MacrosModifier),
                Box::new(ContractModifier),
            ],
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
            let test_changed = unit_test_is_changed(baseline, id, unit_test);
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

    /// Unit-test ids whose **definition changed** relative to the baseline
    /// — the precise "this PR updated this test" signal (cute-dbt#91).
    ///
    /// A test is *changed* when its `UnitTest` differs from the baseline's
    /// entry (added, or edited in place) — the `unit_test_is_changed`
    /// predicate.
    /// This is a strict subset of [`Self::in_scope_unit_tests`]: a changed
    /// test is always in scope (the `target_modified || test_changed`
    /// union), so `changed ⊆ in_scope` holds by construction. Modifier-
    /// independent — a changed test is in scope regardless of which
    /// `state:modified` sub-selectors are registered — hence an associated
    /// function, not a `&self` method.
    #[must_use]
    pub fn changed_unit_tests(current: &Manifest, baseline: &Manifest) -> InScopeSet {
        let mut ids: Vec<String> = Vec::new();
        for (id, unit_test) in current.unit_tests() {
            if unit_test_is_changed(baseline, id, unit_test) {
                ids.push(id.clone());
            }
        }
        ids.into_iter().collect()
    }
}

/// `true` when `current`'s `unit_test` differs from the baseline's entry
/// for `id` — the single definition of "this unit test changed".
///
/// A test absent from the baseline (`None`) is changed (newly added); a
/// test present but not byte-equal is changed (edited). Shared by
/// [`StateComparator::in_scope_unit_tests`] (its branch-B "a changed test
/// on an unchanged model is in scope") and
/// [`StateComparator::changed_unit_tests`] so the two predicates cannot
/// drift apart (cute-dbt#91).
#[must_use]
fn unit_test_is_changed(baseline: &Manifest, id: &str, unit_test: &UnitTest) -> bool {
    baseline.unit_test(id) != Some(unit_test)
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
    use crate::domain::manifest::{Checksum, DependsOn, ManifestMetadata, NodeConfig};
    use crate::domain::unit_test::{UnitTest, UnitTestExpect};
    use serde_json::Value;
    use std::collections::{BTreeMap, HashMap};

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
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
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
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    /// A unit test targeting `model_bare`, carrying `description` so two
    /// otherwise-identical tests can be made to differ.
    fn unit_test_for(model_bare: &str, description: Option<&str>) -> UnitTest {
        UnitTest::new(
            "t",
            NodeId::new(model_bare),
            Vec::new(),
            UnitTestExpect::new(Value::Null, None, None),
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

    // ===== v0.2 sub-selector modifiers (cute-dbt#17) =====
    //
    // Each modifier mirrors the BodyChecksumModifier example-test set:
    //   1. None baseline ⇒ modified (a new node is always modified).
    //   2. Reflexive — is_modified(n, Some(n)) == false.
    //   3. Symmetric in equality — two distinct-but-equal nodes agree in
    //      both directions, and a differing field flags modified.

    /// A `model` node carrying an explicit [`NodeConfig`], relation name,
    /// and column set — the sub-selector inputs.
    fn rich_model(
        full_id: &str,
        checksum: &str,
        config: NodeConfig,
        relation_name: Option<&str>,
        columns: BTreeMap<String, Option<String>>,
    ) -> Node {
        Node::new(
            NodeId::new(full_id),
            "model",
            Checksum::new("sha256", checksum),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            config,
            relation_name.map(str::to_owned),
            columns,
        )
    }

    /// A [`NodeConfig`] from `(key, json-value)` pairs, contract not
    /// enforced.
    fn config_of(pairs: &[(&str, Value)]) -> NodeConfig {
        let map: BTreeMap<String, Value> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        NodeConfig::new(map, false)
    }

    /// A column set from `(name, data_type)` pairs.
    fn columns_of(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(n, t)| ((*n).to_owned(), t.map(str::to_owned)))
            .collect()
    }

    /// A `model` node depending on the given macro ids.
    fn macro_model(full_id: &str, macros: &[&str]) -> Node {
        Node::new(
            NodeId::new(full_id),
            "model",
            Checksum::new("sha256", "same"),
            Some("select 1".to_owned()),
            None,
            DependsOn::new(macros.iter().map(|m| (*m).to_owned()).collect(), Vec::new()),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    // ----- ConfigsModifier -----

    #[test]
    fn configs_modifier_kind_is_configs() {
        assert_eq!(ConfigsModifier.kind(), ModifierKind::Configs);
    }

    #[test]
    fn configs_modifier_treats_a_node_absent_from_baseline_as_modified() {
        let current = rich_model(
            "model.shop.new",
            "same",
            config_of(&[("materialized", Value::from("table"))]),
            None,
            BTreeMap::new(),
        );
        assert!(ConfigsModifier.is_modified(&current, None));
    }

    #[test]
    fn configs_modifier_is_reflexive() {
        let node = rich_model(
            "model.shop.x",
            "same",
            config_of(&[("materialized", Value::from("view"))]),
            None,
            BTreeMap::new(),
        );
        assert!(!ConfigsModifier.is_modified(&node, Some(&node)));
    }

    #[test]
    fn configs_modifier_agrees_symmetrically_on_equal_configs() {
        // Two distinct nodes whose config dicts are equal (even with keys
        // inserted in a different order) compare unmodified in both
        // directions. The body checksum DIFFERS to prove ConfigsModifier
        // reads config, not checksum.
        let a = rich_model(
            "model.shop.x",
            "aaa",
            config_of(&[
                ("materialized", Value::from("table")),
                ("enabled", Value::from(true)),
            ]),
            None,
            BTreeMap::new(),
        );
        let b = rich_model(
            "model.shop.x",
            "bbb",
            config_of(&[
                ("enabled", Value::from(true)),
                ("materialized", Value::from("table")),
            ]),
            None,
            BTreeMap::new(),
        );
        assert!(!ConfigsModifier.is_modified(&a, Some(&b)));
        assert!(!ConfigsModifier.is_modified(&b, Some(&a)));
    }

    #[test]
    fn configs_modifier_detects_a_value_set_change() {
        let current = rich_model(
            "model.shop.x",
            "same",
            config_of(&[("materialized", Value::from("table"))]),
            None,
            BTreeMap::new(),
        );
        let baseline = rich_model(
            "model.shop.x",
            "same",
            config_of(&[("materialized", Value::from("view"))]),
            None,
            BTreeMap::new(),
        );
        assert!(ConfigsModifier.is_modified(&current, Some(&baseline)));
        assert!(ConfigsModifier.is_modified(&baseline, Some(&current)));
    }

    #[test]
    fn configs_modifier_detects_a_key_set_change() {
        let current = rich_model(
            "model.shop.x",
            "same",
            config_of(&[
                ("materialized", Value::from("table")),
                ("tags", Value::from("nightly")),
            ]),
            None,
            BTreeMap::new(),
        );
        let baseline = rich_model(
            "model.shop.x",
            "same",
            config_of(&[("materialized", Value::from("table"))]),
            None,
            BTreeMap::new(),
        );
        assert!(ConfigsModifier.is_modified(&current, Some(&baseline)));
    }

    #[test]
    fn configs_modifier_ignores_a_pure_body_change() {
        // A body-only change (same config) is NOT a config change.
        let current = rich_model(
            "model.shop.x",
            "new_body",
            config_of(&[("materialized", Value::from("table"))]),
            None,
            BTreeMap::new(),
        );
        let baseline = rich_model(
            "model.shop.x",
            "old_body",
            config_of(&[("materialized", Value::from("table"))]),
            None,
            BTreeMap::new(),
        );
        assert!(!ConfigsModifier.is_modified(&current, Some(&baseline)));
    }

    // ----- RelationModifier -----

    #[test]
    fn relation_modifier_kind_is_relation() {
        assert_eq!(RelationModifier.kind(), ModifierKind::Relation);
    }

    #[test]
    fn relation_modifier_treats_a_node_absent_from_baseline_as_modified() {
        let current = rich_model(
            "model.shop.new",
            "same",
            NodeConfig::default(),
            Some("\"db\".\"main\".\"new\""),
            BTreeMap::new(),
        );
        assert!(RelationModifier.is_modified(&current, None));
    }

    #[test]
    fn relation_modifier_is_reflexive() {
        let node = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::default(),
            Some("\"db\".\"main\".\"x\""),
            BTreeMap::new(),
        );
        assert!(!RelationModifier.is_modified(&node, Some(&node)));
    }

    #[test]
    fn relation_modifier_agrees_symmetrically_on_equal_relations() {
        let a = rich_model(
            "model.shop.x",
            "aaa",
            NodeConfig::default(),
            Some("\"db\".\"main\".\"x\""),
            BTreeMap::new(),
        );
        let b = rich_model(
            "model.shop.x",
            "bbb",
            NodeConfig::default(),
            Some("\"db\".\"main\".\"x\""),
            BTreeMap::new(),
        );
        assert!(!RelationModifier.is_modified(&a, Some(&b)));
        assert!(!RelationModifier.is_modified(&b, Some(&a)));
    }

    #[test]
    fn relation_modifier_detects_a_schema_change() {
        // Database / schema / alias / identifier all live in relation_name;
        // a schema rename flips the fully-qualified string.
        let current = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::default(),
            Some("\"db\".\"analytics\".\"x\""),
            BTreeMap::new(),
        );
        let baseline = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::default(),
            Some("\"db\".\"main\".\"x\""),
            BTreeMap::new(),
        );
        assert!(RelationModifier.is_modified(&current, Some(&baseline)));
        assert!(RelationModifier.is_modified(&baseline, Some(&current)));
    }

    #[test]
    fn relation_modifier_detects_an_alias_change() {
        let current = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::default(),
            Some("\"db\".\"main\".\"renamed\""),
            BTreeMap::new(),
        );
        let baseline = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::default(),
            Some("\"db\".\"main\".\"x\""),
            BTreeMap::new(),
        );
        assert!(RelationModifier.is_modified(&current, Some(&baseline)));
    }

    // ----- MacrosModifier -----

    #[test]
    fn macros_modifier_kind_is_macros() {
        assert_eq!(MacrosModifier.kind(), ModifierKind::Macros);
    }

    #[test]
    fn macros_modifier_treats_a_node_absent_from_baseline_as_modified() {
        let current = macro_model("model.shop.new", &["macro.shop.helper"]);
        assert!(MacrosModifier.is_modified(&current, None));
    }

    #[test]
    fn macros_modifier_is_reflexive() {
        let node = macro_model("model.shop.x", &["macro.shop.a", "macro.shop.b"]);
        assert!(!MacrosModifier.is_modified(&node, Some(&node)));
    }

    #[test]
    fn macros_modifier_agrees_symmetrically_on_equal_macro_sets() {
        // Same set, different order ⇒ NOT modified (set comparison).
        let a = macro_model("model.shop.x", &["macro.shop.a", "macro.shop.b"]);
        let b = macro_model("model.shop.x", &["macro.shop.b", "macro.shop.a"]);
        assert!(!MacrosModifier.is_modified(&a, Some(&b)));
        assert!(!MacrosModifier.is_modified(&b, Some(&a)));
    }

    #[test]
    fn macros_modifier_detects_an_added_macro_dependency() {
        let current = macro_model("model.shop.x", &["macro.shop.a", "macro.shop.b"]);
        let baseline = macro_model("model.shop.x", &["macro.shop.a"]);
        assert!(MacrosModifier.is_modified(&current, Some(&baseline)));
        assert!(MacrosModifier.is_modified(&baseline, Some(&current)));
    }

    #[test]
    fn macros_modifier_detects_a_removed_macro_dependency() {
        let current = macro_model("model.shop.x", &[]);
        let baseline = macro_model("model.shop.x", &["macro.shop.a"]);
        assert!(MacrosModifier.is_modified(&current, Some(&baseline)));
    }

    // ----- ContractModifier -----

    #[test]
    fn contract_modifier_kind_is_contract() {
        assert_eq!(ContractModifier.kind(), ModifierKind::Contract);
    }

    #[test]
    fn contract_modifier_treats_a_node_absent_from_baseline_as_modified() {
        let current = rich_model(
            "model.shop.new",
            "same",
            NodeConfig::new(BTreeMap::new(), true),
            None,
            columns_of(&[("id", Some("integer"))]),
        );
        assert!(ContractModifier.is_modified(&current, None));
    }

    #[test]
    fn contract_modifier_is_reflexive() {
        let node = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::new(BTreeMap::new(), true),
            None,
            columns_of(&[("id", Some("integer")), ("name", Some("varchar"))]),
        );
        assert!(!ContractModifier.is_modified(&node, Some(&node)));
    }

    #[test]
    fn contract_modifier_agrees_symmetrically_on_equal_contracts() {
        let a = rich_model(
            "model.shop.x",
            "aaa",
            NodeConfig::new(BTreeMap::new(), true),
            None,
            columns_of(&[("id", Some("integer"))]),
        );
        let b = rich_model(
            "model.shop.x",
            "bbb",
            NodeConfig::new(BTreeMap::new(), true),
            None,
            columns_of(&[("id", Some("integer"))]),
        );
        assert!(!ContractModifier.is_modified(&a, Some(&b)));
        assert!(!ContractModifier.is_modified(&b, Some(&a)));
    }

    #[test]
    fn contract_modifier_detects_an_enforcement_flip() {
        let current = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::new(BTreeMap::new(), true),
            None,
            columns_of(&[("id", Some("integer"))]),
        );
        let baseline = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::new(BTreeMap::new(), false),
            None,
            columns_of(&[("id", Some("integer"))]),
        );
        assert!(ContractModifier.is_modified(&current, Some(&baseline)));
        assert!(ContractModifier.is_modified(&baseline, Some(&current)));
    }

    #[test]
    fn contract_modifier_detects_a_column_type_change() {
        let current = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::new(BTreeMap::new(), true),
            None,
            columns_of(&[("id", Some("bigint"))]),
        );
        let baseline = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::new(BTreeMap::new(), true),
            None,
            columns_of(&[("id", Some("integer"))]),
        );
        assert!(ContractModifier.is_modified(&current, Some(&baseline)));
    }

    #[test]
    fn contract_modifier_detects_an_added_column() {
        let current = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::new(BTreeMap::new(), true),
            None,
            columns_of(&[("id", Some("integer")), ("name", Some("varchar"))]),
        );
        let baseline = rich_model(
            "model.shop.x",
            "same",
            NodeConfig::new(BTreeMap::new(), true),
            None,
            columns_of(&[("id", Some("integer"))]),
        );
        assert!(ContractModifier.is_modified(&current, Some(&baseline)));
    }

    // ----- StateComparator::with_sub_selectors (union semantics) -----

    #[test]
    fn with_sub_selectors_detects_a_pure_config_change_body_only_misses() {
        // The headline win: a config-only change (identical body checksum)
        // is invisible to body_only() but caught by with_sub_selectors().
        let current = manifest(
            vec![rich_model(
                "model.shop.stg_orders",
                "identical",
                config_of(&[("materialized", Value::from("table"))]),
                Some("\"db\".\"main\".\"stg_orders\""),
                BTreeMap::new(),
            )],
            vec![],
        );
        let baseline = manifest(
            vec![rich_model(
                "model.shop.stg_orders",
                "identical",
                config_of(&[("materialized", Value::from("view"))]),
                Some("\"db\".\"main\".\"stg_orders\""),
                BTreeMap::new(),
            )],
            vec![],
        );
        let id = id("model.shop.stg_orders");
        assert!(
            !StateComparator::body_only()
                .modified_set(&current, &baseline)
                .contains(&id),
            "body_only misses a config-only change (the v0.1 limit)",
        );
        assert!(
            StateComparator::with_sub_selectors()
                .modified_set(&current, &baseline)
                .contains(&id),
            "with_sub_selectors catches the config-only change",
        );
    }

    #[test]
    fn with_sub_selectors_still_detects_a_body_change() {
        // Union semantics: the body modifier is still registered, so a
        // body-only change (same config/relation/columns) is still caught.
        let current = manifest(vec![model("model.shop.stg_orders", "new")], vec![]);
        let baseline = manifest(vec![model("model.shop.stg_orders", "old")], vec![]);
        assert!(
            StateComparator::with_sub_selectors()
                .modified_set(&current, &baseline)
                .contains(&id("model.shop.stg_orders")),
        );
    }

    #[test]
    fn with_sub_selectors_is_empty_when_nothing_changed() {
        // No modifier fires when every sub-selector input is identical.
        let node = rich_model(
            "model.shop.stg_orders",
            "same",
            config_of(&[("materialized", Value::from("table"))]),
            Some("\"db\".\"main\".\"stg_orders\""),
            columns_of(&[("id", Some("integer"))]),
        );
        let current = manifest(vec![node.clone()], vec![]);
        let baseline = manifest(vec![node], vec![]);
        assert!(
            StateComparator::with_sub_selectors()
                .modified_set(&current, &baseline)
                .is_empty(),
        );
    }

    #[test]
    fn with_sub_selectors_detects_a_relation_only_change() {
        let current = manifest(
            vec![rich_model(
                "model.shop.x",
                "same",
                NodeConfig::default(),
                Some("\"db\".\"prod\".\"x\""),
                BTreeMap::new(),
            )],
            vec![],
        );
        let baseline = manifest(
            vec![rich_model(
                "model.shop.x",
                "same",
                NodeConfig::default(),
                Some("\"db\".\"dev\".\"x\""),
                BTreeMap::new(),
            )],
            vec![],
        );
        assert!(
            StateComparator::with_sub_selectors()
                .modified_set(&current, &baseline)
                .contains(&id("model.shop.x")),
        );
    }

    #[test]
    fn with_sub_selectors_detects_a_macros_only_change() {
        let current = manifest(
            vec![macro_model(
                "model.shop.x",
                &["macro.shop.a", "macro.shop.b"],
            )],
            vec![],
        );
        let baseline = manifest(vec![macro_model("model.shop.x", &["macro.shop.a"])], vec![]);
        assert!(
            StateComparator::with_sub_selectors()
                .modified_set(&current, &baseline)
                .contains(&id("model.shop.x")),
        );
    }

    #[test]
    fn with_sub_selectors_detects_a_contract_only_change() {
        let current = manifest(
            vec![rich_model(
                "model.shop.x",
                "same",
                NodeConfig::new(BTreeMap::new(), true),
                None,
                columns_of(&[("id", Some("integer"))]),
            )],
            vec![],
        );
        let baseline = manifest(
            vec![rich_model(
                "model.shop.x",
                "same",
                NodeConfig::new(BTreeMap::new(), false),
                None,
                columns_of(&[("id", Some("integer"))]),
            )],
            vec![],
        );
        assert!(
            StateComparator::with_sub_selectors()
                .modified_set(&current, &baseline)
                .contains(&id("model.shop.x")),
        );
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

    // ===== StateComparator::changed_unit_tests (cute-dbt#91) =====

    #[test]
    fn changed_unit_tests_includes_an_edited_test() {
        // A test whose definition changed (description differs) is in the
        // changed set even though its model body is identical.
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
        let changed = StateComparator::changed_unit_tests(&current, &baseline);
        assert!(changed.contains("unit_test.shop.stg_customers.t"));
    }

    #[test]
    fn changed_unit_tests_includes_a_newly_added_test() {
        // A test absent from the baseline is changed (added by this diff).
        let current = manifest(
            vec![model("model.shop.stg_customers", "same")],
            vec![(
                "unit_test.shop.stg_customers.fresh",
                unit_test_for("stg_customers", None),
            )],
        );
        let baseline = manifest(vec![model("model.shop.stg_customers", "same")], vec![]);
        let changed = StateComparator::changed_unit_tests(&current, &baseline);
        assert!(changed.contains("unit_test.shop.stg_customers.fresh"));
    }

    #[test]
    fn changed_unit_tests_excludes_an_identical_test_on_a_modified_model() {
        // The context case: the model body changed (so the test is IN
        // SCOPE via target_modified) but the test definition is byte-equal
        // in both manifests — it is NOT changed. This is the distinction
        // the report's updated-vs-context classification rides on.
        let test = unit_test_for("stg_orders", None);
        let current = manifest(
            vec![model("model.shop.stg_orders", "new")],
            vec![("unit_test.shop.stg_orders.t", test.clone())],
        );
        let baseline = manifest(
            vec![model("model.shop.stg_orders", "old")],
            vec![("unit_test.shop.stg_orders.t", test)],
        );
        let cmp = StateComparator::body_only();
        let in_scope = cmp.in_scope_unit_tests(&current, &baseline);
        let changed = StateComparator::changed_unit_tests(&current, &baseline);
        assert!(
            in_scope.contains("unit_test.shop.stg_orders.t"),
            "an identical test on a modified model is in scope (target_modified)",
        );
        assert!(
            !changed.contains("unit_test.shop.stg_orders.t"),
            "but it is NOT changed — it is context, not updated",
        );
    }

    #[test]
    fn changed_unit_tests_is_a_subset_of_in_scope() {
        // The load-bearing invariant `changed ⊆ in_scope` for the baseline
        // arm: every changed id must also be in scope. Mixed manifest —
        // one edited test on an unchanged model, one identical test on a
        // modified model, one untouched test on an untouched model.
        let current = manifest(
            vec![
                model("model.shop.edited_test_model", "same"),
                model("model.shop.changed_body_model", "new"),
                model("model.shop.untouched", "same"),
            ],
            vec![
                (
                    "unit_test.shop.edited",
                    unit_test_for("edited_test_model", Some("after")),
                ),
                (
                    "unit_test.shop.context",
                    unit_test_for("changed_body_model", None),
                ),
                ("unit_test.shop.untouched", unit_test_for("untouched", None)),
            ],
        );
        let baseline = manifest(
            vec![
                model("model.shop.edited_test_model", "same"),
                model("model.shop.changed_body_model", "old"),
                model("model.shop.untouched", "same"),
            ],
            vec![
                (
                    "unit_test.shop.edited",
                    unit_test_for("edited_test_model", Some("before")),
                ),
                (
                    "unit_test.shop.context",
                    unit_test_for("changed_body_model", None),
                ),
                ("unit_test.shop.untouched", unit_test_for("untouched", None)),
            ],
        );
        let cmp = StateComparator::body_only();
        let in_scope = cmp.in_scope_unit_tests(&current, &baseline);
        let changed = StateComparator::changed_unit_tests(&current, &baseline);
        for id in changed.iter() {
            assert!(
                in_scope.contains(id),
                "changed id {id:?} must be in scope (changed ⊆ in_scope)",
            );
        }
        assert!(changed.contains("unit_test.shop.edited"));
        assert!(!changed.contains("unit_test.shop.context"));
        assert!(!changed.contains("unit_test.shop.untouched"));
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
