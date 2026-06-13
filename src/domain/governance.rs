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

use std::collections::BTreeMap;

use serde::Serialize;

use crate::domain::manifest::{Manifest, Owner};
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
}

impl GovernanceFacts {
    /// `true` when the payload would render any DOM — the
    /// `has_governance` template flag. Slice 0: any group chip. Future
    /// slices OR their own surfaces in here.
    #[must_use]
    pub fn has_content(&self) -> bool {
        !self.group_chips.is_empty()
    }

    /// `true` when the payload carries nothing — the inverse of
    /// [`Self::has_content`]. The `skip_serializing_if` predicate on the
    /// `ReportPayload.governance` field: an empty payload serializes to
    /// ZERO bytes, keeping the non-experimental golden byte-identical.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.group_chips.is_empty()
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
    for model_id in models_in_scope.iter() {
        let Some(group_name) = manifest.node(model_id).and_then(|node| node.group()) else {
            continue;
        };
        by_group
            .entry(group_name)
            .or_insert_with(|| group_chip(manifest, group_name));
    }
    GovernanceFacts {
        group_chips: by_group.into_values().collect(),
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
    GroupChip {
        group: group_name.to_owned(),
        owner_name: owner.and_then(|o| o.name()).map(str::to_owned),
        owner_email: owner.and_then(first_email),
    }
}

/// The first declared email of an [`Owner`], owned. `None` for an owner
/// with no email (the post-normalized list is empty).
fn first_email(owner: &Owner) -> Option<String> {
    owner.email().first().map(String::to_owned)
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
}
