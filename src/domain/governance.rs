//! Governance facts (cute-dbt#260, Slice 0 — the walking skeleton).
//!
//! The render-lane payload for the PR-review governance surfaces. Every
//! surface in epic #260 is **render over already-parsed wire data**: the
//! group/owner/exposure/contract fields are all parsed wire→domain (the
//! [`crate::domain::manifest`] accessors) and surfaced here, behind the
//! [`Experiment::Governance`](crate::domain::Experiment::Governance) gate.
//!
//! **Gating contract (load-bearing, cute-dbt#291 precedent):**
//! [`GovernanceFacts`] is POD with a [`Default`] that is *empty*. The
//! cli gate (`execute_report`) passes the default when the experiment is
//! off, the renderer threads it as a payload field, and the template's
//! `{%- if has_governance %}` conditional emits **zero DOM** for an empty
//! payload — so the non-experimental (`experimental: ""`) goldens stay
//! byte-identical while gated surfaces land incrementally over many PRs.
//!
//! Slice 0 carries exactly one surface: the group/owner header chips
//! ([`GroupChip`]) for the in-scope models that declare a governance
//! group. The reverse-reachability exposure walk, the contract
//! classifier, the enforcement-reality annotation, and the lifecycle
//! chips arrive in Slices 1–5 as additive fields here — never a payload
//! rewrite.
//!
//! Domain purity (AGENTS.md): `std` + `serde` derive only. No I/O, no
//! clap, no askama. The cli layer reads the in-scope model set and the
//! manifest; this module computes the facts.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use crate::domain::manifest::{Exposure, Manifest, NodeId, Owner};
use crate::domain::state::ModelInScopeSet;

/// The governance render payload (cute-dbt#260) — a POD section on
/// [`ReportPayload`](crate::adapters::render::ReportPayload).
///
/// [`Default`] is **empty** (no chips): the off-gate value. An empty
/// payload renders zero DOM (the `{%- if has_governance %}` template
/// conditional), keeping the byte-identity golden gate intact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GovernanceFacts {
    /// One chip per distinct governance group the in-scope models
    /// declare, in deterministic (group-name) order. Empty when no
    /// in-scope model is grouped (or the experiment is off).
    pub group_chips: Vec<GroupChip>,
    /// One blast-radius statement per exposure an in-scope model feeds
    /// (cute-dbt#260 Slice 1), in deterministic (exposure-id) order.
    /// Empty when no in-scope model reaches an exposure (or the
    /// experiment is off).
    pub blast_radius: Vec<BlastRadius>,
}

impl GovernanceFacts {
    /// `true` when the payload would render any DOM — the
    /// `has_governance` template flag. Any group chip OR any blast-radius
    /// statement. Future slices OR their own surfaces in here.
    #[must_use]
    pub fn has_content(&self) -> bool {
        !self.group_chips.is_empty() || !self.blast_radius.is_empty()
    }

    /// `true` when the payload carries nothing — the inverse of
    /// [`Self::has_content`]. The `skip_serializing_if` predicate on the
    /// `ReportPayload.governance` field: an empty payload serializes to
    /// ZERO bytes, keeping the non-experimental golden byte-identical.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.has_content()
    }
}

/// A governance group + its owner, surfaced as a header chip
/// (cute-dbt#260). The dbt `group:` declaration is the only ownership
/// signal on a model node; the owner rides the top-level
/// [`Group`](crate::domain::manifest::Group), resolved via
/// [`Manifest::group_by_name`].
///
/// `owner_email` is post-normalized to the FIRST declared address (the
/// wire shape is `StringOrArrayOfStrings`; the chip names a single
/// routing address). `owner_name` / `owner_email` are independently
/// optional — fusion serializes an unset owner `name` as explicit
/// `null`, and a group may declare an owner with only one of the two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupChip {
    /// The governance group's name (the value a node's
    /// [`Node::group`](crate::domain::manifest::Node::group) carries).
    pub group: String,
    /// The owner's display name, when declared.
    pub owner_name: Option<String>,
    /// The owner's first declared email, when declared.
    pub owner_email: Option<String>,
}

/// A blast-radius statement (cute-dbt#260 Slice 1) — "this PR touches N
/// in-scope models feeding `<exposure>`, owner …". An **aggregate panel
/// callout**, not a per-model verdict: an exposure has no per-model DAG
/// home (the report DAG is per-model CTE-level), so it rides
/// [`GovernanceFacts`] as a panel statement rather than a
/// [`Finding`](crate::domain::checks::Finding) or a DAG node
/// (founder-taste placement default, 2026-06-13 — re-placeable by Design
/// later).
///
/// Produced by [`gather_governance`] when an in-scope model reaches an
/// exposure via the reverse-reachability walk
/// ([`exposures_reachable_from`]). `owner_email` is post-normalized to
/// the FIRST declared address (the wire `owner.email` is
/// `StringOrArrayOfStrings`); `owner_name`/`owner_email` are
/// independently optional (fusion tolerates an exposure owner with
/// content-free fields → `None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlastRadius {
    /// The exposure's bare name (the YAML `- name:`).
    pub exposure_label: String,
    /// The exposure kind — `"dashboard"` / `"notebook"` / `"analysis"` /
    /// `"ml"` / `"application"`, when declared.
    pub exposure_type: Option<String>,
    /// The exposure owner's display name, when declared.
    pub owner_name: Option<String>,
    /// The exposure owner's first declared email, when declared.
    pub owner_email: Option<String>,
    /// How many **in-scope** models feed this exposure (reach it via the
    /// reverse-reachability walk). Always `>= 1` for a statement to
    /// exist.
    pub in_scope_model_count: usize,
}

/// Gather the Slice-0 governance facts for the in-scope models.
///
/// Pure: a single pass over `models_in_scope`, collecting one
/// [`GroupChip`] per distinct group an in-scope model declares
/// (deduplicated, group-name-ordered via the intermediate [`BTreeMap`]).
/// Ungrouped models, and groups with no resolvable
/// [`Group`](crate::domain::manifest::Group), contribute nothing.
///
/// The off-gate value is [`GovernanceFacts::default`] (empty); the cli
/// layer calls this only when
/// [`Experiment::Governance`](crate::domain::Experiment::Governance) is
/// enabled.
#[must_use]
pub fn gather_governance(
    manifest: &Manifest,
    models_in_scope: &ModelInScopeSet,
) -> GovernanceFacts {
    let mut by_group: BTreeMap<&str, GroupChip> = BTreeMap::new();
    // Per-exposure tally of how many in-scope models feed it. Keyed by
    // exposure id (deterministic exposure-id order); the value carries
    // the exposure ref + the running count.
    let mut blast_by_exposure: BTreeMap<&NodeId, (&Exposure, usize)> = BTreeMap::new();
    // Precompute the two reverse maps ONCE (O(N + E)); the per-model BFS
    // below reads them via the helper instead of rebuilding them per
    // model (gemini on #336 — the loop was O(M × (N + E)); now it is
    // O(N + E + Σ reachable), linear).
    let consumers_of = reverse_node_adjacency(manifest);
    let exposure_sinks = exposure_sinks_by_producer(manifest);
    for model_id in models_in_scope.iter() {
        if let Some(group_name) = manifest.node(model_id).and_then(|node| node.group()) {
            by_group
                .entry(group_name)
                .or_insert_with(|| group_chip(manifest, group_name));
        }
        for exposure in exposures_reachable_from_helper(model_id, &consumers_of, &exposure_sinks) {
            blast_by_exposure
                .entry(exposure.id())
                .and_modify(|(_, count)| *count += 1)
                .or_insert((exposure, 1));
        }
    }
    GovernanceFacts {
        group_chips: by_group.into_values().collect(),
        blast_radius: blast_by_exposure
            .into_values()
            .map(|(exposure, count)| blast_radius(exposure, count))
            .collect(),
    }
}

/// Build a [`BlastRadius`] for `exposure` fed by `in_scope_model_count`
/// in-scope models. The exposure's own owner (name + first email)
/// becomes the routing copy.
fn blast_radius(exposure: &Exposure, in_scope_model_count: usize) -> BlastRadius {
    let (owner_name, owner_email) = owner_copy(exposure.owner());
    BlastRadius {
        exposure_label: exposure.name().to_owned(),
        exposure_type: exposure.exposure_type().map(str::to_owned),
        owner_name,
        owner_email,
        in_scope_model_count,
    }
}

/// Build one [`GroupChip`] for `group_name`, resolving the owner via the
/// top-level group lookup. The name/email come from the
/// [`Owner`] when the group declares one; a group with no owner (or an
/// unresolvable group) still chips (name + email `None`) so a grouped
/// model is never silently invisible.
fn group_chip(manifest: &Manifest, group_name: &str) -> GroupChip {
    let owner = manifest
        .group_by_name(group_name)
        .and_then(|group| group.owner());
    let (owner_name, owner_email) = owner_copy(owner);
    GroupChip {
        group: group_name.to_owned(),
        owner_name,
        owner_email,
    }
}

/// Normalize an optional [`Owner`] to the `(name, first email)` copy the
/// group chip and blast-radius statement both render.
///
/// The wire `owner.email` is `StringOrArrayOfStrings` (post-normalized to
/// a list by the adapter, a lone string becoming a one-element list); the
/// copy names a single routing address, so it takes the FIRST. `name` and
/// `email` are independently optional (fusion serializes an unset owner
/// `name` as explicit `null`, and an owner may declare only one of the
/// two), so an absent owner — or one with content-free fields — yields
/// `(None, None)`.
fn owner_copy(owner: Option<&Owner>) -> (Option<String>, Option<String>) {
    (
        owner.and_then(|o| o.name()).map(str::to_owned),
        owner.and_then(|o| o.email().first().cloned()),
    )
}

/// The exposures reachable **downstream** from `model_id` (cute-dbt#260
/// Slice 1) — the exposures this model feeds.
///
/// dbt's `depends_on.nodes` is a CONSUMER→PRODUCER edge (a node lists the
/// nodes it reads). cute-dbt walks it REVERSED (producer→consumer) to
/// answer "what does a change to this model reach?" — the manifest's
/// own child/parent maps are single-hop, so the transitive closure is
/// cute-dbt's job. An exposure is a downstream SINK: it depends on the
/// models it reads, so a model is reached-from when it is in the
/// transitive upstream closure of the exposure's dependencies.
///
/// BFS over the reversed node edges from `model_id`, collecting every
/// exposure that directly depends on a visited node. A [`BTreeSet`]
/// visited set makes the walk acyclic-safe (a cyclic manifest cannot
/// loop forever) and the result deterministic (exposure-id order).
/// Returns each reachable exposure once.
///
/// Standalone convenience: builds the two reverse maps then delegates to
/// the private `exposures_reachable_from_helper`. The map construction is
/// O(N + E) (a full-manifest scan), so a per-model loop must NOT call
/// this — it precomputes the maps once and calls the helper directly
/// ([`gather_governance`] does exactly that). This wrapper exists for the
/// single-model callers + the property tests.
#[must_use]
pub fn exposures_reachable_from<'m>(
    manifest: &'m Manifest,
    model_id: &NodeId,
) -> Vec<&'m Exposure> {
    let consumers_of = reverse_node_adjacency(manifest);
    let exposure_sinks = exposure_sinks_by_producer(manifest);
    exposures_reachable_from_helper(model_id, &consumers_of, &exposure_sinks)
}

/// The reverse-reachability BFS core (cute-dbt#260 Slice 1) — reads the
/// PRECOMPUTED reverse maps instead of rebuilding them, so a per-model
/// caller pays the O(N + E) map construction ONCE, not per model. The
/// walk itself is O(reachable from `model_id`); the [`BTreeSet`] visited
/// set keeps it acyclic-safe and the [`BTreeMap`] `hit` keeps the result
/// deduplicated + exposure-id-ordered.
#[must_use]
fn exposures_reachable_from_helper<'m>(
    model_id: &NodeId,
    consumers_of: &BTreeMap<&NodeId, Vec<&NodeId>>,
    exposure_sinks: &BTreeMap<&NodeId, Vec<&'m Exposure>>,
) -> Vec<&'m Exposure> {
    let mut reached: BTreeSet<&NodeId> = BTreeSet::new();
    let mut queue: VecDeque<&NodeId> = VecDeque::new();
    queue.push_back(model_id);
    // `hit` keyed by exposure id so each exposure is returned once,
    // exposure-id-ordered (BTreeMap).
    let mut hit: BTreeMap<&NodeId, &Exposure> = BTreeMap::new();
    while let Some(current) = queue.pop_front() {
        if !reached.insert(current) {
            continue;
        }
        for exposure in exposure_sinks.get(current).into_iter().flatten() {
            hit.insert(exposure.id(), exposure);
        }
        for consumer in consumers_of.get(current).into_iter().flatten() {
            queue.push_back(consumer);
        }
    }
    hit.into_values().collect()
}

/// The reversed node-dependency adjacency: producer id → the node ids
/// that consume it (the reverse of every `depends_on.nodes` edge).
fn reverse_node_adjacency(manifest: &Manifest) -> BTreeMap<&NodeId, Vec<&NodeId>> {
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

/// Exposure sinks keyed by producer: producer id → the exposures that
/// directly depend on it (an exposure is a downstream lineage terminus).
fn exposure_sinks_by_producer(manifest: &Manifest) -> BTreeMap<&NodeId, Vec<&Exposure>> {
    let mut exposure_sinks: BTreeMap<&NodeId, Vec<&Exposure>> = BTreeMap::new();
    for exposure in manifest.exposures().values() {
        for producer_id in exposure.depends_on().nodes() {
            exposure_sinks
                .entry(producer_id)
                .or_default()
                .push(exposure);
        }
    }
    exposure_sinks
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::*;
    use crate::domain::manifest::{
        Checksum, DependsOn, Group, Manifest, ManifestMetadata, Node, NodeConfig, NodeId, Owner,
    };

    fn sample_checksum() -> Checksum {
        Checksum::new("sha256", "0".repeat(64))
    }

    /// A bare model node carrying an optional group declaration.
    fn model(full_id: &str, group: Option<&str>) -> Node {
        Node::new(
            NodeId::new(full_id),
            "model",
            sample_checksum(),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_governance(group.map(str::to_owned), None)
    }

    fn manifest_with(nodes: Vec<Node>, groups: Vec<Group>) -> Manifest {
        let node_map: HashMap<NodeId, Node> = nodes
            .into_iter()
            .map(|node| (node.id().clone(), node))
            .collect();
        let group_map: HashMap<String, Group> = groups
            .into_iter()
            .map(|group| (format!("group.healthcare.{}", group.name()), group))
            .collect();
        Manifest::new(
            ManifestMetadata::new("v12"),
            node_map,
            HashMap::new(),
            HashMap::new(),
        )
        .with_groups(group_map)
    }

    fn in_scope(ids: &[&str]) -> ModelInScopeSet {
        ids.iter().map(|id| NodeId::new(*id)).collect()
    }

    #[test]
    fn default_governance_facts_are_empty() {
        let facts = GovernanceFacts::default();
        assert!(facts.group_chips.is_empty());
        assert!(!facts.has_content());
        assert!(facts.is_empty());
    }

    #[test]
    fn populated_facts_report_content_and_not_empty() {
        let manifest = manifest_with(
            vec![model("model.pkg.a", Some("finance"))],
            vec![Group::new("finance", None)],
        );
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]));
        assert!(facts.has_content());
        assert!(!facts.is_empty());
    }

    #[test]
    fn ungrouped_in_scope_models_yield_no_chips() {
        let manifest = manifest_with(
            vec![model("model.pkg.a", None), model("model.pkg.b", None)],
            vec![],
        );
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a", "model.pkg.b"]));
        assert!(facts.group_chips.is_empty());
        assert!(!facts.has_content());
    }

    #[test]
    fn grouped_in_scope_model_yields_a_chip_with_owner() {
        let manifest = manifest_with(
            vec![model("model.pkg.a", Some("finance"))],
            vec![Group::new(
                "finance",
                Some(Owner::new(
                    Some("Finance Team".to_owned()),
                    vec!["finance@corp.example".to_owned()],
                )),
            )],
        );
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]));
        assert_eq!(
            facts.group_chips,
            vec![GroupChip {
                group: "finance".to_owned(),
                owner_name: Some("Finance Team".to_owned()),
                owner_email: Some("finance@corp.example".to_owned()),
            }],
        );
        assert!(facts.has_content());
    }

    #[test]
    fn only_first_owner_email_rides_the_chip() {
        // The wire shape is StringOrArrayOfStrings; the chip names one
        // routing address.
        let manifest = manifest_with(
            vec![model("model.pkg.a", Some("data"))],
            vec![Group::new(
                "data",
                Some(Owner::new(
                    None,
                    vec![
                        "first@corp.example".to_owned(),
                        "second@corp.example".to_owned(),
                    ],
                )),
            )],
        );
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]));
        assert_eq!(facts.group_chips[0].owner_name, None);
        assert_eq!(
            facts.group_chips[0].owner_email,
            Some("first@corp.example".to_owned()),
        );
    }

    #[test]
    fn group_with_no_owner_still_chips() {
        // A grouped model is never silently invisible: the chip renders
        // with name + email None.
        let manifest = manifest_with(
            vec![model("model.pkg.a", Some("orphan"))],
            vec![Group::new("orphan", None)],
        );
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]));
        assert_eq!(
            facts.group_chips,
            vec![GroupChip {
                group: "orphan".to_owned(),
                owner_name: None,
                owner_email: None,
            }],
        );
    }

    #[test]
    fn grouped_model_with_unresolvable_group_still_chips_ownerless() {
        // The node declares a group the manifest's top-level map omits —
        // the chip names the group, owner None.
        let manifest = manifest_with(vec![model("model.pkg.a", Some("ghost"))], vec![]);
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]));
        assert_eq!(
            facts.group_chips,
            vec![GroupChip {
                group: "ghost".to_owned(),
                owner_name: None,
                owner_email: None,
            }],
        );
    }

    #[test]
    fn distinct_groups_dedup_and_sort_by_name() {
        let manifest = manifest_with(
            vec![
                model("model.pkg.a", Some("zeta")),
                model("model.pkg.b", Some("alpha")),
                model("model.pkg.c", Some("alpha")), // same group as b
                model("model.pkg.d", None),
            ],
            vec![Group::new("zeta", None), Group::new("alpha", None)],
        );
        let facts = gather_governance(
            &manifest,
            &in_scope(&["model.pkg.a", "model.pkg.b", "model.pkg.c", "model.pkg.d"]),
        );
        let names: Vec<&str> = facts
            .group_chips
            .iter()
            .map(|chip| chip.group.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "zeta"], "deduped + name-sorted");
    }

    #[test]
    fn out_of_scope_grouped_models_contribute_nothing() {
        let manifest = manifest_with(
            vec![
                model("model.pkg.a", Some("finance")),
                model("model.pkg.b", Some("marketing")),
            ],
            vec![Group::new("finance", None), Group::new("marketing", None)],
        );
        // Only `a` is in scope.
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]));
        let names: Vec<&str> = facts
            .group_chips
            .iter()
            .map(|chip| chip.group.as_str())
            .collect();
        assert_eq!(names, vec!["finance"]);
    }

    #[test]
    fn empty_in_scope_set_yields_empty_facts() {
        let manifest = manifest_with(
            vec![model("model.pkg.a", Some("finance"))],
            vec![Group::new("finance", None)],
        );
        let facts = gather_governance(&manifest, &ModelInScopeSet::new());
        assert!(facts.group_chips.is_empty());
    }

    // ===== Slice 1: exposure reverse-reachability + blast radius =====

    use crate::domain::manifest::Exposure;

    /// A model node depending on `upstream` (the `depends_on.nodes`,
    /// consumer→producer edges).
    fn model_with_deps(full_id: &str, upstream: &[&str]) -> Node {
        Node::new(
            NodeId::new(full_id),
            "model",
            sample_checksum(),
            Some("select 1".to_owned()),
            None,
            DependsOn::new(
                Vec::new(),
                upstream.iter().map(|u| NodeId::new(*u)).collect(),
            ),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    /// An exposure reading `producers` (its `depends_on.nodes`).
    fn exposure(
        full_id: &str,
        name: &str,
        kind: Option<&str>,
        owner: Option<Owner>,
        producers: &[&str],
    ) -> Exposure {
        Exposure::new(
            NodeId::new(full_id),
            name,
            kind.map(str::to_owned),
            None,
            owner,
            DependsOn::new(
                Vec::new(),
                producers.iter().map(|p| NodeId::new(*p)).collect(),
            ),
        )
    }

    fn manifest_lineage(nodes: Vec<Node>, exposures: Vec<Exposure>) -> Manifest {
        let node_map: HashMap<NodeId, Node> = nodes
            .into_iter()
            .map(|node| (node.id().clone(), node))
            .collect();
        let exposure_map: HashMap<NodeId, Exposure> =
            exposures.into_iter().map(|e| (e.id().clone(), e)).collect();
        Manifest::new(
            ManifestMetadata::new("v12"),
            node_map,
            HashMap::new(),
            HashMap::new(),
        )
        .with_exposures(exposure_map)
    }

    fn reachable_names(manifest: &Manifest, model_id: &str) -> Vec<String> {
        exposures_reachable_from(manifest, &NodeId::new(model_id))
            .into_iter()
            .map(|e| e.name().to_owned())
            .collect()
    }

    // ---- property-shaped tests for the reverse-reachability closure ----

    #[test]
    fn reachability_reflexive_sink_membership() {
        // (a) A model the exposure DIRECTLY depends on is reachable from
        // itself (the BFS visits the start node and collects its sinks).
        let manifest = manifest_lineage(
            vec![model_with_deps("model.pkg.fct", &[])],
            vec![exposure(
                "exposure.pkg.dash",
                "dash",
                Some("dashboard"),
                None,
                &["model.pkg.fct"],
            )],
        );
        assert_eq!(reachable_names(&manifest, "model.pkg.fct"), vec!["dash"]);
    }

    #[test]
    fn reachability_is_transitive() {
        // (b) A → B → exposure: the exposure reads B, B reads A, so the
        // exposure is reachable from A (the closure, not just one hop).
        let manifest = manifest_lineage(
            vec![
                model_with_deps("model.pkg.a", &[]),
                model_with_deps("model.pkg.b", &["model.pkg.a"]),
            ],
            vec![exposure(
                "exposure.pkg.dash",
                "dash",
                Some("dashboard"),
                None,
                &["model.pkg.b"],
            )],
        );
        assert_eq!(reachable_names(&manifest, "model.pkg.a"), vec!["dash"]);
        assert_eq!(reachable_names(&manifest, "model.pkg.b"), vec!["dash"]);
    }

    #[test]
    fn reachability_has_no_false_positives() {
        // (c) An exposure NOT reachable from a model is never returned: a
        // sibling branch (c→exposure) is unreachable from an unrelated
        // model (a).
        let manifest = manifest_lineage(
            vec![
                model_with_deps("model.pkg.a", &[]),
                model_with_deps("model.pkg.c", &[]),
            ],
            vec![exposure(
                "exposure.pkg.dash",
                "dash",
                Some("dashboard"),
                None,
                &["model.pkg.c"],
            )],
        );
        assert!(
            reachable_names(&manifest, "model.pkg.a").is_empty(),
            "an unreachable exposure is never returned",
        );
        assert_eq!(reachable_names(&manifest, "model.pkg.c"), vec!["dash"]);
    }

    #[test]
    fn reachability_is_acyclic_safe() {
        // (d) A cyclic manifest (a↔b) must not loop forever — the visited
        // set terminates the BFS and the exposure still resolves.
        let manifest = manifest_lineage(
            vec![
                model_with_deps("model.pkg.a", &["model.pkg.b"]),
                model_with_deps("model.pkg.b", &["model.pkg.a"]),
            ],
            vec![exposure(
                "exposure.pkg.dash",
                "dash",
                Some("dashboard"),
                None,
                &["model.pkg.b"],
            )],
        );
        // Terminates (no infinite loop) and both nodes reach the exposure.
        assert_eq!(reachable_names(&manifest, "model.pkg.a"), vec!["dash"]);
        assert_eq!(reachable_names(&manifest, "model.pkg.b"), vec!["dash"]);
    }

    #[test]
    fn reachability_returns_each_exposure_once_in_id_order() {
        // Two exposures feeding off one shared model, plus the diamond
        // join — each returned once, exposure-id-ordered.
        let manifest = manifest_lineage(
            vec![model_with_deps("model.pkg.fct", &[])],
            vec![
                exposure(
                    "exposure.pkg.zeta",
                    "zeta",
                    Some("ml"),
                    None,
                    &["model.pkg.fct"],
                ),
                exposure(
                    "exposure.pkg.alpha",
                    "alpha",
                    Some("dashboard"),
                    None,
                    &["model.pkg.fct"],
                ),
            ],
        );
        // BTreeMap over exposure id → alpha (exposure.pkg.alpha) before
        // zeta (exposure.pkg.zeta).
        assert_eq!(
            reachable_names(&manifest, "model.pkg.fct"),
            vec!["alpha", "zeta"],
        );
    }

    #[test]
    fn reachability_model_feeding_no_exposure_is_empty() {
        let manifest = manifest_lineage(vec![model_with_deps("model.pkg.lonely", &[])], vec![]);
        assert!(reachable_names(&manifest, "model.pkg.lonely").is_empty());
    }

    #[test]
    fn wrapper_and_precomputed_helper_agree() {
        // The wrapper (builds the maps then delegates) and the helper
        // (reads precomputed maps — the gather_governance path) must
        // return identical exposures (gemini on #336: the O(M×(N+E)) →
        // O(N+E) refactor is behavior-preserving).
        let manifest = manifest_lineage(
            vec![
                model_with_deps("model.pkg.a", &[]),
                model_with_deps("model.pkg.b", &["model.pkg.a"]),
                model_with_deps("model.pkg.fct", &["model.pkg.b"]),
            ],
            vec![exposure(
                "exposure.pkg.dash",
                "dash",
                Some("dashboard"),
                None,
                &["model.pkg.fct"],
            )],
        );
        let consumers_of = reverse_node_adjacency(&manifest);
        let exposure_sinks = exposure_sinks_by_producer(&manifest);
        for model in ["model.pkg.a", "model.pkg.b", "model.pkg.fct"] {
            let id = NodeId::new(model);
            let via_wrapper: Vec<&NodeId> = exposures_reachable_from(&manifest, &id)
                .into_iter()
                .map(Exposure::id)
                .collect();
            let via_helper: Vec<&NodeId> =
                exposures_reachable_from_helper(&id, &consumers_of, &exposure_sinks)
                    .into_iter()
                    .map(Exposure::id)
                    .collect();
            assert_eq!(via_wrapper, via_helper, "wrapper == helper for {model}");
        }
    }

    // ---- owner-shape unit tests (StringOrArrayOfStrings + None) ----

    fn blast_for(manifest: &Manifest, model_id: &str) -> BlastRadius {
        let facts = gather_governance(manifest, &in_scope(&[model_id]));
        assert_eq!(facts.blast_radius.len(), 1, "exactly one blast statement");
        facts.blast_radius.into_iter().next().unwrap()
    }

    fn manifest_one_exposure(owner: Option<Owner>) -> Manifest {
        manifest_lineage(
            vec![model_with_deps("model.pkg.fct", &[])],
            vec![exposure(
                "exposure.pkg.dash",
                "dash",
                Some("dashboard"),
                owner,
                &["model.pkg.fct"],
            )],
        )
    }

    #[test]
    fn blast_owner_email_as_string_takes_the_lone_address() {
        // Wire `email` as a single string normalizes to a one-element
        // list; the statement names it.
        let manifest = manifest_one_exposure(Some(Owner::new(
            Some("Data".to_owned()),
            vec!["data@corp.example".to_owned()],
        )));
        let b = blast_for(&manifest, "model.pkg.fct");
        assert_eq!(b.owner_name, Some("Data".to_owned()));
        assert_eq!(b.owner_email, Some("data@corp.example".to_owned()));
    }

    #[test]
    fn blast_owner_email_as_array_takes_the_first() {
        let manifest = manifest_one_exposure(Some(Owner::new(
            Some("Data".to_owned()),
            vec![
                "first@corp.example".to_owned(),
                "second@corp.example".to_owned(),
            ],
        )));
        let b = blast_for(&manifest, "model.pkg.fct");
        assert_eq!(b.owner_email, Some("first@corp.example".to_owned()));
    }

    #[test]
    fn blast_owner_email_absent_is_none() {
        let manifest = manifest_one_exposure(Some(Owner::new(Some("Data".to_owned()), Vec::new())));
        let b = blast_for(&manifest, "model.pkg.fct");
        assert_eq!(b.owner_name, Some("Data".to_owned()));
        assert_eq!(b.owner_email, None);
    }

    #[test]
    fn blast_owner_name_none_with_email_present() {
        let manifest =
            manifest_one_exposure(Some(Owner::new(None, vec!["data@corp.example".to_owned()])));
        let b = blast_for(&manifest, "model.pkg.fct");
        assert_eq!(b.owner_name, None);
        assert_eq!(b.owner_email, Some("data@corp.example".to_owned()));
    }

    #[test]
    fn blast_no_owner_at_all_is_none_none() {
        let manifest = manifest_one_exposure(None);
        let b = blast_for(&manifest, "model.pkg.fct");
        assert_eq!(b.owner_name, None);
        assert_eq!(b.owner_email, None);
    }

    // ---- blast-radius aggregation through gather_governance ----

    #[test]
    fn blast_radius_carries_label_type_and_count() {
        let b = blast_for(
            &manifest_one_exposure(Some(Owner::new(
                Some("Team".to_owned()),
                vec!["team@corp.example".to_owned()],
            ))),
            "model.pkg.fct",
        );
        assert_eq!(b.exposure_label, "dash");
        assert_eq!(b.exposure_type, Some("dashboard".to_owned()));
        assert_eq!(b.in_scope_model_count, 1);
    }

    #[test]
    fn blast_radius_counts_distinct_in_scope_models_feeding_one_exposure() {
        // Two in-scope models (a, b) both feed the same exposure through
        // the diamond fct; the count is 2 (a and b reach it), and the
        // statement appears once.
        let manifest = manifest_lineage(
            vec![
                model_with_deps("model.pkg.a", &[]),
                model_with_deps("model.pkg.b", &[]),
                model_with_deps("model.pkg.fct", &["model.pkg.a", "model.pkg.b"]),
            ],
            vec![exposure(
                "exposure.pkg.dash",
                "dash",
                Some("dashboard"),
                None,
                &["model.pkg.fct"],
            )],
        );
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a", "model.pkg.b"]));
        assert_eq!(facts.blast_radius.len(), 1);
        assert_eq!(facts.blast_radius[0].exposure_label, "dash");
        assert_eq!(facts.blast_radius[0].in_scope_model_count, 2);
    }

    #[test]
    fn blast_radius_statements_are_exposure_id_ordered() {
        let manifest = manifest_lineage(
            vec![model_with_deps("model.pkg.fct", &[])],
            vec![
                exposure(
                    "exposure.pkg.zeta",
                    "zeta",
                    Some("ml"),
                    None,
                    &["model.pkg.fct"],
                ),
                exposure(
                    "exposure.pkg.alpha",
                    "alpha",
                    Some("dashboard"),
                    None,
                    &["model.pkg.fct"],
                ),
            ],
        );
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.fct"]));
        let labels: Vec<&str> = facts
            .blast_radius
            .iter()
            .map(|b| b.exposure_label.as_str())
            .collect();
        assert_eq!(labels, vec!["alpha", "zeta"]);
    }

    #[test]
    fn no_exposure_in_scope_yields_no_blast_and_no_content() {
        let manifest = manifest_lineage(vec![model_with_deps("model.pkg.a", &[])], vec![]);
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]));
        assert!(facts.blast_radius.is_empty());
        assert!(!facts.has_content(), "no group, no exposure ⇒ no DOM");
    }

    #[test]
    fn blast_radius_alone_makes_the_payload_non_empty() {
        // Ungrouped model reaching an exposure ⇒ has_content via the
        // blast-radius arm (the group_chips arm is empty).
        let manifest = manifest_one_exposure(None);
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.fct"]));
        assert!(facts.group_chips.is_empty());
        assert!(!facts.blast_radius.is_empty());
        assert!(facts.has_content());
        assert!(!facts.is_empty());
    }
}
