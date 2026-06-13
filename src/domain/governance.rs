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

use crate::domain::checks::uniqueness_test_columns;
use crate::domain::grain::test_is_enabled;
use crate::domain::manifest::{
    ColumnFacts, Constraint, ConstraintKind, Exposure, Group, Manifest, Node, NodeId, Owner,
};
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
    /// One contract-classification drawer per in-scope model with a
    /// contract change (cute-dbt#260 Slice 2), in deterministic
    /// (model-name) order. Empty when no in-scope model's contract
    /// changed, in `--pr-diff` mode (no OLD manifest to compare), or when
    /// the experiment is off. Omitted from JSON when empty so a
    /// governance render that surfaces only Slice 0/1 facts (the
    /// committed `diff-showcase` golden — `--pr-diff`, so it never
    /// classifies) stays byte-identical.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contract_classes: Vec<ContractClass>,
    /// The dbt-native lifecycle chips (cute-dbt#260 Slice 4):
    /// public-surface-changed, ref-to-deprecated, version-bump-without-
    /// latest, group-owner-touch, access-violation. Deterministic order
    /// (built per in-scope model, model-id then kind). Omitted from JSON
    /// when empty so a governance render surfacing only earlier-slice
    /// facts stays byte-identical.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lifecycle_chips: Vec<GovChip>,
    /// Per in-scope model config-driven `meta` + `tags` (cute-dbt#260 /
    /// cute-dbt#348), model-name order. Empty when no in-scope model (or
    /// its columns) carries meta/tags, or the experiment is off. Omitted
    /// from JSON when empty so a meta-free governance render stays
    /// byte-identical.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub meta_tags: Vec<ModelMetaTags>,
}

impl GovernanceFacts {
    /// `true` when the payload would render any DOM — the
    /// `has_governance` template flag. Any group chip OR blast-radius
    /// statement OR contract-classification drawer OR lifecycle chip OR
    /// meta/tags block. Future slices OR their own surfaces in here.
    #[must_use]
    pub fn has_content(&self) -> bool {
        !self.group_chips.is_empty()
            || !self.blast_radius.is_empty()
            || !self.contract_classes.is_empty()
            || !self.meta_tags.is_empty()
            || !self.lifecycle_chips.is_empty()
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
/// [`Group`], resolved via [`Manifest::group_by_name`].
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

/// One model's contract-classification drawer (cute-dbt#260 Slice 2) —
/// the `safe`/`breaking` structural contract diff + the contract header
/// chip. Produced by [`gather_governance`] from
/// [`classify_contract`] when an in-scope model's contract changed and
/// the OLD model is available (`--baseline-manifest` mode).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContractClass {
    /// The bare model name this drawer belongs to.
    pub model: String,
    /// Overall verdict — `"safe"` (a non-breaking change) or
    /// `"breaking"`. Drives the chip + tag styling (the AA-contrast
    /// target).
    pub verdict: String,
    /// The contract header chip text
    /// (`Contract: enforced · v2 of 3 · access: public · group finance`).
    pub chip: String,
    /// One row per contracted column whose type changed.
    pub column_diffs: Vec<ContractColumnDiff>,
    /// Human-readable lines for the non-column reasons (columns-removed,
    /// constraint-removed, materialization-changed, enforcement). Empty
    /// for a column-type-only change. The template renders each verbatim.
    pub reasons: Vec<String>,
}

/// One column-level contract diff row (cute-dbt#260 Slice 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContractColumnDiff {
    /// The column name.
    pub name: String,
    /// The previous declared type (`"unknown"` when undeclared).
    pub old: String,
    /// The current declared type.
    pub new: String,
    /// Always `"breaking"` (a contracted type change is breaking) — the
    /// per-row `data-verdict` hook.
    pub verdict: String,
}

/// One dbt-native lifecycle chip (cute-dbt#260 Slice 4) — the
/// `Rust computes, the template renders` contract: the predicate fns
/// build the `kind` / `label` / optional `severity`, the template emits
/// `<span data-chip-kind=… data-chip-severity=…>{label}</span>`.
/// Intentionally unstyled beyond the shared chip/semantic classes —
/// Claude Design owns the visual pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GovChip {
    /// Stable kind discriminant — the `data-chip-kind` hook
    /// (`public-surface-changed` / `ref-to-deprecated` /
    /// `version-bump-without-latest` / `group-owner-touch` /
    /// `access-violation`).
    pub kind: String,
    /// The rendered chip text.
    pub label: String,
    /// Optional severity for the dual-state chips — the
    /// `data-chip-severity` hook that maps to the report's theme semantic
    /// tokens (`info` / `danger`), never a hardcoded color. `None` for the
    /// chips with no severity dimension.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
}

/// One in-scope model's config-driven `meta` + `tags` (cute-dbt#260 /
/// cute-dbt#348). Model-level `tags` become per-tag header chips;
/// model-level `meta` becomes one aggregate chip + tooltip; column-level
/// `meta`/`tags` ride the per-column rows in the drawer. Present only
/// when the model (or one of its columns) carries meta or tags, so the
/// payload stays byte-stable on meta-free models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelMetaTags {
    /// The bare model name (the drawer header + `data-model` hook).
    pub model: String,
    /// The model-level `config.tags`, deduplicated + sorted (dbt re-emits
    /// inherited tags, so the wire list carries duplicates). One chip per
    /// entry.
    pub tags: Vec<String>,
    /// The model-level `config.meta` as flattened `key: value` pairs,
    /// key-sorted — the aggregate-chip tooltip body. Each value is the
    /// compact-JSON rendering of the wire value (auto-escaped at render).
    pub meta: Vec<MetaPair>,
    /// Per-column `meta`/`tags` for the drawer — only columns carrying
    /// either appear, column-name-sorted.
    pub columns: Vec<ColumnMetaTags>,
}

/// One column's config-driven `meta` + `tags` (cute-dbt#348) — the
/// per-column drawer row. Present only for columns carrying either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColumnMetaTags {
    /// The column name.
    pub column: String,
    /// The column-level `tags`, deduplicated + sorted.
    pub tags: Vec<String>,
    /// The column-level `meta` as key-sorted `key: value` pairs.
    pub meta: Vec<MetaPair>,
}

/// One `meta` `key: value` entry (cute-dbt#348). The value is the
/// compact-JSON rendering of the (possibly nested) wire value, rendered
/// as auto-escaped text — never interpreted, never trusted as markup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetaPair {
    /// The meta key.
    pub key: String,
    /// The value, compact-JSON rendered (`"x"` / `42` / `["a","b"]` /
    /// `{"k":1}`).
    pub value: String,
}

/// A parsed `deprecation_date` (`%Y-%m-%d`) as a comparable civil date
/// (cute-dbt#260 Slice 4). `(year, month, day)` orders lexicographically
/// as a date, so the `scheduled` (future) vs `elapsed` (today-or-past)
/// split is a plain tuple comparison against the threaded generation
/// date — keeping the domain pure + deterministic (no `SystemTime` here;
/// the cli computes "today" and threads it in).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DepDate {
    /// Four-digit year.
    pub year: i32,
    /// Month, 1–12.
    pub month: u32,
    /// Day, 1–31.
    pub day: u32,
}

impl DepDate {
    /// Parse a `%Y-%m-%d` string (the dbt `deprecation_date` wire shape).
    /// `None` for any malformed value — the chip simply does not fire
    /// (never a panic, never a guess).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.split('-');
        let year: i32 = parts.next()?.parse().ok()?;
        let month: u32 = parts.next()?.parse().ok()?;
        let day: u32 = parts.next()?.parse().ok()?;
        if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        Some(Self { year, month, day })
    }
}

/// Gather the governance facts for the in-scope models (cute-dbt#260).
///
/// Pure: a single pass over `models_in_scope` collecting the group/owner
/// chips (Slice 0) + the reverse-reachability blast-radius statements
/// (Slice 1), plus — when `old_manifest` is `Some` (the
/// `--baseline-manifest` arm) — the per-model contract classifications
/// (Slice 2). `old_manifest` is `None` on the `--pr-diff` arm (no OLD
/// manifest to compare structurally) and whenever the caller has no
/// baseline, so contract classification is skipped and the payload stays
/// byte-identical.
///
/// The off-gate value is [`GovernanceFacts::default`] (empty); the cli
/// layer calls this only when
/// [`Experiment::Governance`](crate::domain::Experiment::Governance) is
/// enabled.
#[must_use]
pub fn gather_governance(
    manifest: &Manifest,
    models_in_scope: &ModelInScopeSet,
    old_manifest: Option<&Manifest>,
    today: Option<DepDate>,
) -> GovernanceFacts {
    let mut by_group: BTreeMap<&str, GroupChip> = BTreeMap::new();
    // Per-exposure tally of how many in-scope models feed it. Keyed by
    // exposure id (deterministic exposure-id order); the value carries
    // the exposure ref + the running count.
    let mut blast_by_exposure: BTreeMap<&NodeId, (&Exposure, usize)> = BTreeMap::new();
    // Contract classes keyed by bare model name (deterministic order).
    let mut contract_by_model: BTreeMap<&str, ContractClass> = BTreeMap::new();
    // Slice 4 lifecycle chips, accumulated per in-scope model (the
    // model_in_scope set iterates in node-id order, so the chip order is
    // deterministic; each model's chips append in fixed predicate order).
    let mut lifecycle_chips: Vec<GovChip> = Vec::new();
    // cute-dbt#348 — per-model config-driven meta/tags blocks.
    let mut meta_tags: Vec<ModelMetaTags> = Vec::new();
    // Precompute the two reverse maps ONCE (O(N + E)); the per-model BFS
    // below reads them via the helper instead of rebuilding them per
    // model (gemini on #336 — the loop was O(M × (N + E)); now it is
    // O(N + E + Σ reachable), linear).
    let consumers_of = reverse_node_adjacency(manifest);
    let exposure_sinks = exposure_sinks_by_producer(manifest);
    for model_id in models_in_scope.iter() {
        let Some(node) = manifest.node(model_id) else {
            continue;
        };
        if let Some(group_name) = node.group() {
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
        // Slice 2: classify against the OLD node (same id) when a baseline
        // is available (the helper handles the no-baseline / newly-added /
        // unchanged cases).
        if let Some(class) = classify_model_contract(old_manifest, model_id, node) {
            contract_by_model.insert(node.bare_name(), class);
        }
        // Slice 4: the dbt-native lifecycle chips for this model.
        lifecycle_chips.extend(lifecycle_chips_for(manifest, node, old_manifest, today));
        // cute-dbt#348: the config-driven meta/tags block for this model
        // (None when neither the model nor any column carries meta/tags).
        if let Some(block) = model_meta_tags(node) {
            meta_tags.push(block);
        }
    }
    GovernanceFacts {
        group_chips: by_group.into_values().collect(),
        blast_radius: blast_by_exposure
            .into_values()
            .map(|(exposure, count)| blast_radius(exposure, count))
            .collect(),
        contract_classes: contract_by_model.into_values().collect(),
        lifecycle_chips,
        meta_tags,
    }
}

/// The config-driven meta/tags block for one in-scope model
/// (cute-dbt#348), or `None` when neither the model nor any of its
/// columns carries `meta` or `tags`. Pure: reads `config.meta` /
/// `config.tags` (model-level, via the generic `NodeConfig` dict) and
/// [`ColumnFacts`] (column-level) — no new ingestion.
fn model_meta_tags(node: &Node) -> Option<ModelMetaTags> {
    let tags = config_tags(node);
    let meta = config_meta(node);
    let columns = column_meta_tags(node);
    if tags.is_empty() && meta.is_empty() && columns.is_empty() {
        return None;
    }
    Some(ModelMetaTags {
        model: node.bare_name().to_owned(),
        tags,
        meta,
        columns,
    })
}

/// Model-level `config.tags`, deduplicated + sorted (dbt re-emits
/// inherited tags, so the wire list carries duplicates). Non-string
/// entries are skipped (tags are categorical strings).
fn config_tags(node: &Node) -> Vec<String> {
    let Some(value) = node.config().config().get("tags") else {
        return Vec::new();
    };
    dedup_sorted_tags(value)
}

/// Deduplicate + sort the string entries of a JSON `tags` value (an
/// array, or a lone string). Empty for any other shape.
fn dedup_sorted_tags(value: &serde_json::Value) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    set.insert(s.to_owned());
                }
            }
        }
        serde_json::Value::String(s) => {
            set.insert(s.clone());
        }
        _ => {}
    }
    set.into_iter().collect()
}

/// Model-level `config.meta` as key-sorted [`MetaPair`]s. Empty when
/// absent or not an object.
fn config_meta(node: &Node) -> Vec<MetaPair> {
    node.config()
        .config()
        .get("meta")
        .map(meta_pairs)
        .unwrap_or_default()
}

/// Flatten a JSON `meta` value into key-sorted `key: value` pairs. A
/// non-object value yields no pairs (meta is a dict). Each value is the
/// compact-JSON rendering of the (possibly nested) wire value.
fn meta_pairs(value: &serde_json::Value) -> Vec<MetaPair> {
    let serde_json::Value::Object(map) = value else {
        return Vec::new();
    };
    // serde_json::Map preserves insertion order; sort by key for a
    // deterministic, golden-stable rendering.
    let mut pairs: Vec<MetaPair> = map
        .iter()
        .map(|(key, v)| MetaPair {
            key: key.clone(),
            value: meta_value_string(v),
        })
        .collect();
    pairs.sort_by(|a, b| a.key.cmp(&b.key));
    pairs
}

/// Render a meta value to a compact display string: a bare string stays
/// unquoted; everything else is compact JSON (`42` / `true` /
/// `["a","b"]` / `{"k":1}`). Auto-escaped as text at the render layer.
fn meta_value_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Per-column meta/tags for the drawer (cute-dbt#348) — only columns
/// carrying `meta` or `tags`, column-name-sorted (the `column_facts`
/// `BTreeMap` is already key-ordered).
fn column_meta_tags(node: &Node) -> Vec<ColumnMetaTags> {
    node.column_facts()
        .iter()
        .filter_map(|(column, facts)| {
            let tags = dedup_sorted_tag_slice(facts.tags());
            let meta = facts.meta().map(meta_pairs).unwrap_or_default();
            (!tags.is_empty() || !meta.is_empty()).then(|| ColumnMetaTags {
                column: column.clone(),
                tags,
                meta,
            })
        })
        .collect()
}

/// Deduplicate + sort an already-typed string tag slice (the column
/// `ColumnFacts::tags` shape).
fn dedup_sorted_tag_slice(tags: &[String]) -> Vec<String> {
    let set: BTreeSet<String> = tags.iter().cloned().collect();
    set.into_iter().collect()
}

/// The Slice-4 lifecycle chips for one in-scope `node`, in fixed
/// predicate order: public-surface-changed, version-bump-without-latest,
/// group-owner-touch (this-model chips), then the ref-driven chips
/// (ref-to-deprecated, access-violation) over the node's `depends_on`
/// targets. Each predicate is pure and returns `Option`/`Vec`; the
/// assembler just collects.
fn lifecycle_chips_for(
    manifest: &Manifest,
    node: &Node,
    old_manifest: Option<&Manifest>,
    today: Option<DepDate>,
) -> Vec<GovChip> {
    let mut chips = Vec::new();
    chips.extend(public_surface_changed_chip(node, old_manifest));
    chips.extend(version_bump_without_latest_chip(node));
    chips.extend(group_owner_touch_chip(manifest, node));
    chips.extend(ref_chips(manifest, node, today));
    chips
}

/// `public-surface-changed` (cute-dbt#260 Slice 4) — fires when a
/// `public` model's contract change is `Breaking` (reuses Slice 5's
/// [`classify_contract`]). `None` when the model is not public, there is
/// no baseline (no old node to classify), or the change is not breaking.
/// A missing `access` defaults to `protected` (Intel C — missing ≠
/// public), so an unset model never trips this.
fn public_surface_changed_chip(node: &Node, old_manifest: Option<&Manifest>) -> Option<GovChip> {
    if effective_access(node) != "public" {
        return None;
    }
    let old = old_manifest?.node(node.id())?;
    if !matches!(classify_contract(old, node), ContractChange::Breaking(_)) {
        return None;
    }
    Some(GovChip {
        kind: "public-surface-changed".to_owned(),
        label: "Public contract — breaking change".to_owned(),
        severity: None,
    })
}

/// `version-bump-without-latest` (cute-dbt#260 Slice 4, ADVISORY) — the
/// model declares a `version` that differs from its `latest_version`
/// (`latest_version` lag during migration is legitimate, so this is a
/// cue, never a defect). `None` when either is unset or they match.
/// `version`/`latest_version` are post-normalized strings, so `2` and
/// `"2"` compare equal (Intel C).
fn version_bump_without_latest_chip(node: &Node) -> Option<GovChip> {
    let version = node.version()?;
    let latest = node.latest_version()?;
    if version == latest {
        return None;
    }
    Some(GovChip {
        kind: "version-bump-without-latest".to_owned(),
        label: format!(
            "{} v{version} — latest_version still v{latest}",
            node.bare_name()
        ),
        severity: None,
    })
}

/// `group-owner-touch` (cute-dbt#260 Slice 4) — the model belongs to a
/// governance group whose owner declares an email. `None` for ungrouped
/// models, unresolvable groups, or owners with no email.
fn group_owner_touch_chip(manifest: &Manifest, node: &Node) -> Option<GovChip> {
    let group = node.group()?;
    let owner = manifest.group_by_name(group).and_then(Group::owner)?;
    let email = owner.email().first()?;
    Some(GovChip {
        kind: "group-owner-touch".to_owned(),
        label: format!("Touches group {group} (owner: {email})"),
        severity: None,
    })
}

/// The ref-driven chips for `node`: for each model it `ref()`s
/// (`depends_on.nodes`), a `ref-to-deprecated` chip (the target carries a
/// `deprecation_date`) and/or an `access-violation` chip (the target is
/// `private` and in a different group). Deterministic: walks
/// `depends_on.nodes` in wire order.
fn ref_chips(manifest: &Manifest, node: &Node, today: Option<DepDate>) -> Vec<GovChip> {
    let mut chips = Vec::new();
    for target_id in node.depends_on().nodes() {
        let Some(target) = manifest.node(target_id) else {
            continue;
        };
        if target.resource_type() != "model" {
            continue;
        }
        chips.extend(ref_to_deprecated_chip(target, today));
        chips.extend(access_violation_chip(node, target));
    }
    chips
}

/// `ref-to-deprecated` (cute-dbt#260 Slice 4, DUAL-STATE) — the PR refs a
/// model carrying a `deprecation_date`. Future date ⇒ `scheduled` /
/// `info`; today-or-past ⇒ `elapsed` / `danger`. A malformed date ⇒ no
/// chip (no panic, no guess — Intel C). When `today` is unknown the chip
/// still fires with no severity (date set is the signal; the
/// scheduled/elapsed split is the refinement).
fn ref_to_deprecated_chip(target: &Node, today: Option<DepDate>) -> Option<GovChip> {
    let raw = target.deprecation_date()?;
    let date = DepDate::parse(raw)?;
    let severity = today.map(|now| {
        if date > now {
            "info".to_owned() // scheduled
        } else {
            "danger".to_owned() // elapsed (today-or-past)
        }
    });
    Some(GovChip {
        kind: "ref-to-deprecated".to_owned(),
        label: format!("Refs deprecated {} (deprecated {raw})", target.bare_name()),
        severity,
    })
}

/// `access-violation` (cute-dbt#260 Slice 4) — a `private` model ref'd
/// across a group boundary. Fires when the ref TARGET is `private` and
/// the referer's group differs from the target's group (a private model
/// is only legitimately referenced within its own group). A target with
/// the same group (or both ungrouped, matching `None`) is in-bounds.
fn access_violation_chip(referer: &Node, target: &Node) -> Option<GovChip> {
    if effective_access(target) != "private" {
        return None;
    }
    if referer.group() == target.group() {
        return None;
    }
    Some(GovChip {
        kind: "access-violation".to_owned(),
        label: format!(
            "Private {} referenced across group boundary",
            target.bare_name()
        ),
        severity: None,
    })
}

/// The model's effective access level (cute-dbt#260 Slice 4) — the
/// declared `access`, defaulting to `"protected"` when unset (Intel C:
/// missing access ≠ public).
fn effective_access(node: &Node) -> &str {
    node.access().unwrap_or("protected")
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

// ===== cute-dbt#260 Slice 5: structural contract breaking-change =====
//
// The shared primitive for surface 2 (the classified contract-diff
// drawer). Mirrors fusion's `DbtModel::same_contract`
// (`dbt-schemas/src/schemas/nodes.rs:4911` @ dbt-labs/dbt-core main) —
// the authoritative engine source for what a contract breaking change
// IS. Verified against that source 2026-06-13.
//
// Load-bearing rules from the engine:
// - Do NOT key on `contract.checksum` alone — it is
//   `skip_serializing_if = Option::is_none`, frequently null on fusion,
//   and has a known upstream bug (dbt-core#8030). The engine uses it
//   ONLY as a fast-path equality short-circuit
//   (`enforced && checksum == checksum ⇒ unchanged`); the verdict is
//   otherwise structural (column sets + types + constraints).
// - A column ADD is never breaking (engine: "present in self.columns …
//   not a breaking change").
// - The enforced-constraint-removed + materialization-changed categories
//   are evaluated ONLY when the OLD materialization enforces constraints
//   (`Table | Incremental`) — views never enforce, so removing a
//   constraint on a view is not breaking.
// - `ConstraintType` has six kinds incl. `Custom`; column-level drops
//   `Custom` (dbt convention — custom column constraints are free-form).

/// The structural verdict of comparing a model's contract across a change
/// (cute-dbt#260 Slice 5). Mirrors fusion's `same_contract` outcome
/// space: identical / changed-but-safe / breaking (with the engine's six
/// reason categories).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractChange {
    /// No contract-relevant change: neither side enforces, or the
    /// enforced checksums are identical, or the structural diff is empty.
    Unchanged,
    /// A change that is NOT breaking — the only such transition the
    /// engine recognizes: an unenforced→enforced contract (newly
    /// contracted, nothing downstream relied on the contract before).
    ChangedNotBreaking,
    /// A breaking change, carrying every reason that fired (engine OR
    /// semantics — any one reason makes it breaking).
    Breaking(Vec<BreakingReason>),
}

/// One category of contract breaking change (cute-dbt#260 Slice 5) — the
/// engine's six verbatim categories (`same_contract_both_present`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakingReason {
    /// The contract was enforced and no longer is (engine:
    /// `contract_enforced_disabled`).
    ContractEnforcedDisabled,
    /// Contracted columns were removed (engine: `columns_removed`).
    ColumnsRemoved(Vec<String>),
    /// A contracted column's `data_type` changed (engine:
    /// `column_type_changes`). Alias-equal types (`string`↔`varchar`
    /// under `alias_types`) are NOT reported.
    ColumnTypeChanged {
        /// The column whose type changed.
        col: String,
        /// The previous data type (`"unknown"` when undeclared, the
        /// engine's fallback).
        prev: String,
        /// The current data type.
        current: String,
    },
    /// An enforced COLUMN-level constraint was removed (engine:
    /// `enforced_column_constraint_removed`) — only on a `Table` /
    /// `Incremental` old materialization.
    EnforcedColumnConstraintRemoved,
    /// An enforced MODEL-level constraint was removed (engine:
    /// `enforced_model_constraint_removed`) — only on a `Table` /
    /// `Incremental` old materialization.
    EnforcedModelConstraintRemoved,
    /// The materialization moved off a constraint-enforcing strategy
    /// while constraints existed (engine: `materialization_changed`).
    MaterializationChanged,
}

/// Classify one in-scope model's contract against the OLD manifest
/// (cute-dbt#260 Slice 2). `None` when there is no baseline
/// (`old_manifest == None`, the `--pr-diff` arm), the model is newly
/// added (absent from the old manifest), or the contract is unchanged.
fn classify_model_contract(
    old_manifest: Option<&Manifest>,
    model_id: &NodeId,
    current: &Node,
) -> Option<ContractClass> {
    let old_node = old_manifest?.node(model_id)?;
    contract_class(old_node, current)
}

/// Build a [`ContractClass`] drawer for an in-scope model (cute-dbt#260
/// Slice 2). `None` when the classification is
/// [`ContractChange::Unchanged`] (no drawer). The chip + reason lines
/// reflect the CURRENT model's contract metadata.
fn contract_class(old: &Node, current: &Node) -> Option<ContractClass> {
    match classify_contract(old, current) {
        ContractChange::Unchanged => None,
        ContractChange::ChangedNotBreaking => Some(ContractClass {
            model: current.bare_name().to_owned(),
            verdict: "safe".to_owned(),
            chip: contract_chip(current),
            column_diffs: Vec::new(),
            reasons: vec!["Contract is now enforced (newly contracted).".to_owned()],
        }),
        ContractChange::Breaking(breaking) => {
            let (column_diffs, reasons) = breaking_to_rows(&breaking);
            Some(ContractClass {
                model: current.bare_name().to_owned(),
                verdict: "breaking".to_owned(),
                chip: contract_chip(current),
                column_diffs,
                reasons,
            })
        }
    }
}

/// Split the breaking reasons into column-level diff rows + non-column
/// reason lines.
fn breaking_to_rows(breaking: &[BreakingReason]) -> (Vec<ContractColumnDiff>, Vec<String>) {
    let mut column_diffs = Vec::new();
    let mut reasons = Vec::new();
    for reason in breaking {
        match reason {
            BreakingReason::ColumnTypeChanged { col, prev, current } => {
                column_diffs.push(ContractColumnDiff {
                    name: col.clone(),
                    old: prev.clone(),
                    new: current.clone(),
                    verdict: "breaking".to_owned(),
                });
            }
            other => reasons.push(breaking_reason_line(other)),
        }
    }
    (column_diffs, reasons)
}

/// One human-readable line per non-column breaking reason.
fn breaking_reason_line(reason: &BreakingReason) -> String {
    match reason {
        BreakingReason::ContractEnforcedDisabled => "Contract enforcement was removed.".to_owned(),
        BreakingReason::ColumnsRemoved(cols) => format!("Columns removed: {}.", cols.join(", ")),
        BreakingReason::EnforcedColumnConstraintRemoved => {
            "An enforced column-level constraint was removed.".to_owned()
        }
        BreakingReason::EnforcedModelConstraintRemoved => {
            "An enforced model-level constraint was removed.".to_owned()
        }
        BreakingReason::MaterializationChanged => {
            "Materialization moved off a constraint-enforcing strategy.".to_owned()
        }
        // ColumnTypeChanged rides a column-diff row, never a line.
        BreakingReason::ColumnTypeChanged { col, prev, current } => {
            format!("Column {col} type changed ({prev} → {current}).")
        }
    }
}

/// The contract header chip text (cute-dbt#260 Slice 2) — assembled from
/// the CURRENT model's contract metadata, segment by segment so each
/// optional field stays a small testable formatter:
/// `Contract: enforced · v2 of 3 · access: public · group finance`.
fn contract_chip(current: &Node) -> String {
    let mut segments = vec![format!(
        "Contract: {}",
        if current.config().contract_enforced() {
            "enforced"
        } else {
            "unenforced"
        }
    )];
    if let Some(version) = contract_version_segment(current) {
        segments.push(version);
    }
    if let Some(access) = current.access() {
        segments.push(format!("access: {access}"));
    }
    if let Some(group) = current.group() {
        segments.push(format!("group {group}"));
    }
    segments.join(" · ")
}

/// The `v{version} of {latest}` chip segment (cute-dbt#260 Slice 2).
/// `version` / `latest_version` are post-normalized strings (the wire
/// `StringOrInteger` — `2` and `"2"` arrive identically). `None` for an
/// unversioned model; the `of {latest}` half is dropped without a latest.
fn contract_version_segment(current: &Node) -> Option<String> {
    let version = current.version()?;
    Some(match current.latest_version() {
        Some(latest) => format!("v{version} of {latest}"),
        None => format!("v{version}"),
    })
}

/// Classify the contract change between an `old` and `current` model node
/// (cute-dbt#260 Slice 5) — the structural mirror of fusion's
/// `same_contract`.
///
/// Composition root: the top-level transition gate
/// (`classify_top_level_transition`) handles the enforced-flag +
/// checksum-fast-path cases; when neither short-circuits, the structural
/// diff (`diff_columns` + `diff_constraints`) builds the breaking
/// reason set. An empty reason set is [`ContractChange::Unchanged`]
/// (the founder-taste call: NO "changed-investigate" bucket — surface
/// nothing when the structure is identical, the plan's §5 risk-#5).
#[must_use]
pub fn classify_contract(old: &Node, current: &Node) -> ContractChange {
    if let Some(decided) = classify_top_level_transition(old, current) {
        return decided;
    }
    // Both enforce + checksums differ (or a checksum is absent): run the
    // structural diff. Constraint-level + materialization reasons are
    // gated on the OLD materialization enforcing constraints.
    let old_enforces_constraints = materialization_enforces(old.config().materialized());
    let mut reasons = diff_columns(old, current, old_enforces_constraints);
    reasons.extend(diff_constraints(old, current, old_enforces_constraints));
    if reasons.is_empty() {
        ContractChange::Unchanged
    } else {
        ContractChange::Breaking(reasons)
    }
}

/// The enforced-flag + checksum top-level gate (engine:
/// `same_contract` + the head of `same_contract_both_present`). Returns
/// `Some(verdict)` when the transition decides the outcome without a
/// structural diff; `None` when both sides enforce and the structure must
/// be compared.
fn classify_top_level_transition(old: &Node, current: &Node) -> Option<ContractChange> {
    let old_enforced = old.config().contract_enforced();
    let current_enforced = current.config().contract_enforced();
    match (old_enforced, current_enforced) {
        // Neither enforces ⇒ no contract change.
        (false, false) => Some(ContractChange::Unchanged),
        // Newly enforced ⇒ a change, but NOT breaking (engine).
        (false, true) => Some(ContractChange::ChangedNotBreaking),
        // Enforcement dropped ⇒ breaking.
        (true, false) => Some(ContractChange::Breaking(vec![
            BreakingReason::ContractEnforcedDisabled,
        ])),
        // Both enforce: fast-path on identical, non-null checksums
        // (engine's happy path), else fall through to the structural
        // diff. A null checksum (fusion's frequent omission, dbt-core#8030)
        // never short-circuits — the structure decides.
        (true, true) => match (old.contract_checksum(), current.contract_checksum()) {
            (Some(o), Some(c)) if o == c => Some(ContractChange::Unchanged),
            _ => None,
        },
    }
}

/// Whether a materialization enforces constraints (engine:
/// `materialization_enforces_constraints` ⇒ `Table | Incremental`). A
/// view never enforces, so a constraint removed on a view is not
/// breaking. `None` (unset materialization) is treated as non-enforcing.
fn materialization_enforces(materialized: Option<&str>) -> bool {
    matches!(materialized, Some("table" | "incremental"))
}

/// The per-column half of the structural diff (engine: the
/// `for old_value in old.columns` loop). Reports removed columns,
/// alias-aware type changes, and — only when `old_enforces_constraints` —
/// removed enforced column-level constraints.
fn diff_columns(old: &Node, current: &Node, old_enforces_constraints: bool) -> Vec<BreakingReason> {
    let mut reasons = Vec::new();
    let mut removed = Vec::new();
    let current_columns = current.columns();
    for (name, old_type) in old.columns() {
        let Some(current_type) = current_columns.get(name) else {
            // Column removed (a column ADD is the inverse — never breaking).
            removed.push(name.clone());
            continue;
        };
        if !data_types_equal(old_type.as_deref(), current_type.as_deref()) {
            // Borrow + convert only on the breaking path (no Option<String>
            // clone on the happy path; the engine's "unknown" fallback).
            reasons.push(BreakingReason::ColumnTypeChanged {
                col: name.clone(),
                prev: old_type.as_deref().unwrap_or("unknown").to_owned(),
                current: current_type.as_deref().unwrap_or("unknown").to_owned(),
            });
        }
    }
    if !removed.is_empty() {
        reasons.push(BreakingReason::ColumnsRemoved(removed));
    }
    if old_enforces_constraints && column_constraint_removed(old, current) {
        reasons.push(BreakingReason::EnforcedColumnConstraintRemoved);
    }
    reasons
}

/// The model-level constraint + materialization half of the structural
/// diff (engine: the model-constraint loop + the materialization-changed
/// check). Both are gated on the OLD materialization enforcing
/// constraints.
fn diff_constraints(
    old: &Node,
    current: &Node,
    old_enforces_constraints: bool,
) -> Vec<BreakingReason> {
    let mut reasons = Vec::new();
    if !old_enforces_constraints {
        return reasons;
    }
    if constraints_removed(old.constraints(), current.constraints()) {
        reasons.push(BreakingReason::EnforcedModelConstraintRemoved);
    }
    // Materialization moved OFF a constraint-enforcing strategy while
    // constraints existed (engine: `materialization_changed`).
    let current_enforces = materialization_enforces(current.config().materialized());
    let had_constraints = !old.constraints().is_empty() || any_column_constraint(old);
    if !current_enforces && had_constraints {
        reasons.push(BreakingReason::MaterializationChanged);
    }
    reasons
}

/// Whether any old column-level constraint was removed in `current`
/// (engine: the inner `old_value.constraints != current_column.constraints`
/// loop). Custom column-level constraints are dropped (dbt convention).
/// Iterates references — no temp `Vec` allocation.
fn column_constraint_removed(old: &Node, current: &Node) -> bool {
    let current_facts = current.column_facts();
    old.column_facts().iter().any(|(name, facts)| {
        let current_constraints = current_facts.get(name).map(ColumnFacts::constraints);
        facts
            .constraints()
            .iter()
            .filter(|c| is_structural_constraint(c))
            .any(|old_c| !current_has(current_constraints, old_c))
    })
}

/// Whether `current_constraints` (the slice, or `None` when the column is
/// gone) contains `old_c` — the constraint-removal predicate over a
/// borrowed slice (no temp `Vec`).
fn current_has(current_constraints: Option<&[Constraint]>, old_c: &Constraint) -> bool {
    current_constraints.is_some_and(|cs| cs.contains(old_c))
}

/// Whether a column-level constraint is contract-structural — i.e. not
/// `Custom` (dbt convention: custom column constraints are free-form).
fn is_structural_constraint(constraint: &Constraint) -> bool {
    constraint.kind() != ConstraintKind::Custom
}

/// Whether `old` has any column-level constraint at all (the
/// `column_constraints_exist` flag in the engine's materialization gate).
/// Iterates references — no temp `Vec`.
fn any_column_constraint(old: &Node) -> bool {
    old.column_facts()
        .values()
        .any(|facts| facts.constraints().iter().any(is_structural_constraint))
}

/// Whether any constraint present in `old` is absent from `current`
/// (engine: `!current.contains(old_constraint)`). A constraint ADD is not
/// a removal.
fn constraints_removed(old: &[Constraint], current: &[Constraint]) -> bool {
    old.iter().any(|c| !current.contains(c))
}

/// Data-type equality with `alias_types` honored (default-on dbt
/// behavior): two declared types are equal when they are byte-equal OR
/// canonicalize to the same alias family (`string`/`varchar`/`text`,
/// `int`/`integer`, `bool`/`boolean`). Two undeclared types (`None`) are
/// equal; a declared-vs-undeclared pair is not.
fn data_types_equal(old: Option<&str>, current: Option<&str>) -> bool {
    match (old, current) {
        (None, None) => true,
        (Some(o), Some(c)) => o.eq_ignore_ascii_case(c) || canonical_type(o) == canonical_type(c),
        _ => false,
    }
}

/// Canonicalize a declared SQL type to its `alias_types` family, so
/// `string`↔`varchar`↔`text` (etc.) compare equal. An unknown type
/// canonicalizes to its lowercased self (so two distinct unknowns stay
/// distinct).
fn canonical_type(declared: &str) -> String {
    // Strip a length/precision suffix: `varchar(255)` → `varchar`.
    let base = declared
        .split('(')
        .next()
        .unwrap_or(declared)
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "string" | "varchar" | "text" | "char" | "character varying" => "string".to_owned(),
        "int" | "integer" | "int4" => "integer".to_owned(),
        "bigint" | "int8" => "bigint".to_owned(),
        "bool" | "boolean" => "boolean".to_owned(),
        "float" | "float8" | "double" | "double precision" => "double".to_owned(),
        other => other.to_owned(),
    }
}

// ===== cute-dbt#260 Slice 3: enforcement reality + backing-test join ===
//
// The enforcement-reality surface answers "this PK/unique is DECLARED —
// is it actually enforced by the warehouse, and does a data test back
// it?". Two pure pieces:
//
// 1. `constraint_support(adapter, kind)` — the hardcoded
//    adapter × constraint matrix, a verbatim mirror of fusion's
//    `get_constraint_support` (`dbt-adapter/src/adapter/adapter_impl.rs:2328`
//    @ dbt-labs/dbt-core main, verified 2026-06-13). NOTE the source
//    corrects two common assumptions: Postgres does NOT enforce
//    PK/Unique (only NotNull + FK); DuckDB (the committed fixtures'
//    adapter) "follows Postgres".
// 2. `backing_test_for(manifest, model, column, kind)` — the INFERRED
//    constraint→test edge. The manifest never links a constraint to its
//    backing test; cute-dbt infers it: a `test.*` node `attached_node`'d
//    to the model whose `test_metadata.name ∈ {unique, not_null}` and
//    whose column matches. The inference can MISS renamed/custom tests —
//    the copy must say "backing test" (authoring discipline), never
//    imply the warehouse lacks the index.

/// Whether a constraint kind is enforced by a given warehouse adapter
/// (cute-dbt#260 Slice 3) — mirrors fusion's `ConstraintSupport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ConstraintSupport {
    /// The warehouse enforces this constraint at write time.
    Enforced,
    /// The warehouse accepts the declaration but does NOT enforce it
    /// (metadata-only) — the common PK/unique case on analytics
    /// warehouses.
    NotEnforced,
    /// The warehouse does not support this constraint kind at all.
    NotSupported,
}

/// The adapter × constraint-kind enforcement matrix (cute-dbt#260
/// Slice 3) — a verbatim mirror of fusion's `get_constraint_support`.
/// A flat lookup table (low CRAP). `adapter` is matched
/// case-insensitively; an unknown adapter (or one fusion leaves
/// `unimplemented!`, e.g. Athena/Spark/Trino) returns `NotEnforced` —
/// the honest "declared, enforcement unknown" default, never a false
/// `Enforced` claim. `Other`/unknown constraint kinds also degrade to
/// `NotEnforced`.
// `match_same_arms`: the per-adapter rows are a deliberate VERBATIM mirror
// of fusion's `get_constraint_support` table — one row per adapter, in the
// engine's order. Merging arms with identical bodies (e.g. every
// `Unique | PrimaryKey => NotEnforced`) across adapters would collapse the
// audit trail that makes this a checkable mirror of the source. Keep the
// table shape; the lint's dedup goal is not the goal here.
#[allow(clippy::match_same_arms)]
#[must_use]
pub fn constraint_support(adapter: &str, kind: ConstraintKind) -> ConstraintSupport {
    use ConstraintKind::{Check, Custom, ForeignKey, NotNull, PrimaryKey, Unique};
    use ConstraintSupport::{Enforced, NotEnforced, NotSupported};
    match (adapter.to_ascii_lowercase().as_str(), kind) {
        // Postgres + DuckDB ("follows Postgres" — the fixtures' adapter).
        ("postgres" | "duckdb", NotNull | ForeignKey) => Enforced,
        ("postgres" | "duckdb", Unique | PrimaryKey) => NotEnforced,
        ("postgres" | "duckdb", Check | Custom) => NotSupported,
        // Snowflake.
        ("snowflake", NotNull | ForeignKey) => Enforced,
        ("snowflake", Unique | PrimaryKey) => NotEnforced,
        ("snowflake", Check | Custom) => NotSupported,
        // BigQuery.
        ("bigquery", NotNull) => Enforced,
        ("bigquery", PrimaryKey | ForeignKey) => NotEnforced,
        ("bigquery", Unique | Check | Custom) => NotSupported,
        // Databricks (+ Spark shares the metadata adapter; Check is
        // enforced there).
        ("databricks", NotNull | Check) => Enforced,
        ("databricks", PrimaryKey | ForeignKey) => NotEnforced,
        ("databricks", Unique | Custom) => NotSupported,
        // Redshift.
        ("redshift", NotNull) => Enforced,
        ("redshift", Unique | PrimaryKey | ForeignKey) => NotEnforced,
        ("redshift", Check | Custom) => NotSupported,
        // Unknown / fusion-unimplemented adapters: honest "declared,
        // enforcement unknown" — never a false Enforced, never invents
        // NotSupported.
        _ => NotEnforced,
    }
}

/// The id of the first enabled data test that backs a `kind` constraint
/// on `column` of `model` (cute-dbt#260 Slice 3), or `None` when the
/// constraint has no inferable backing — the INFERRED constraint→test
/// edge.
///
/// The manifest never links a constraint to its backing test, so cute-dbt
/// joins: a `test.*` node `attached_node`'d to `model`, enabled, whose
/// `test_metadata.name` is the generic test that backs `kind`
/// (`unique` ⇒ `"unique"`, `not_null` ⇒ `"not_null"`), and whose target
/// column (the `column_name` kwarg, falling back to the node-level
/// `column_name`) equals `column` (case-insensitive). Returns the real
/// test node id so a Covered finding can attribute it (the
/// `Verdict::Covered.by` contract — never a synthetic label). The
/// inference can MISS a renamed test or a custom/singular test that
/// asserts the same thing — the surface copy admits this (authoring
/// discipline, never a warehouse-truth claim).
///
/// Iteration order is the `HashMap`'s, so the chosen id among multiple
/// backing tests is unspecified — but the finding's presence (Covered vs
/// Uncovered) is order-independent, which is the load-bearing property.
#[must_use]
pub fn backing_test_for<'m>(
    manifest: &'m Manifest,
    model: &NodeId,
    column: &str,
    kind: ConstraintKind,
) -> Option<&'m NodeId> {
    match kind {
        // PK / Unique on a single column: a uniqueness test whose column
        // SET equals `{column}` (a `unique` on it, OR a
        // `unique_combination_of_columns` over exactly that one column).
        ConstraintKind::PrimaryKey | ConstraintKind::Unique => {
            backing_test_for_columns(manifest, model, &[column.to_owned()])
        }
        // NotNull: a `not_null` test on the column (not a uniqueness test).
        ConstraintKind::NotNull => manifest
            .nodes()
            .iter()
            .find(|(_, node)| not_null_test_backs(node, model, column))
            .map(|(id, _)| id),
        _ => None,
    }
}

/// The id of the first enabled `unique` / `unique_combination_of_columns`
/// data test attached to `model` whose asserted columns are a SUBSET of
/// the declared `columns` (cute-dbt#341) — the composite-aware uniqueness
/// backing join.
///
/// SUBSET (case-insensitive), mirroring the #259 grain
/// [`covers_key`](crate::domain::checks) direction exactly: a uniqueness
/// test on a proper subset of a composite key is a STRONGER guarantee
/// that backs it (`{a}` unique ⟹ `(a, b)` unique). An exact match is the
/// subset boundary case, so it backs too. A test over a SUPERSET, or one
/// only partially overlapping, is NOT a subset and does not back the key
/// (superset-uniqueness does not imply subset-uniqueness). Reuses the
/// #259 grain recognizer `uniqueness_test_columns` (the crate-private
/// `crate::domain::checks` extractor) for "which columns does this test
/// assert unique?" — one authority across the grain and enforcement
/// surfaces. Returns the REAL test node id so a Covered finding
/// attributes it.
#[must_use]
pub fn backing_test_for_columns<'m>(
    manifest: &'m Manifest,
    model: &NodeId,
    columns: &[String],
) -> Option<&'m NodeId> {
    let key = column_set(columns);
    if key.is_empty() {
        return None;
    }
    manifest
        .nodes()
        .iter()
        .find(|(_, node)| uniqueness_test_covers_key(node, model, &key))
        .map(|(id, _)| id)
}

/// The lowercased column set of `columns` (the case-insensitive
/// match key).
fn column_set(columns: &[String]) -> BTreeSet<String> {
    columns.iter().map(|c| c.to_ascii_lowercase()).collect()
}

/// Whether `node` is an enabled uniqueness test attached to `model` whose
/// asserted columns are a non-empty SUBSET of `key` (already lowercased)
/// — the grain `covers_key` direction (cute-dbt#169 / #341).
fn uniqueness_test_covers_key(node: &Node, model: &NodeId, key: &BTreeSet<String>) -> bool {
    if node.resource_type() != "test" || node.attached_node() != Some(model) {
        return false;
    }
    if !test_is_enabled(node) {
        return false;
    }
    let Some(columns) = uniqueness_test_columns(node) else {
        return false;
    };
    let test_cols = column_set(&columns);
    !test_cols.is_empty() && test_cols.is_subset(key)
}

/// Whether `node` is an enabled `not_null` data test attached to `model`
/// targeting `column` (case-insensitive).
fn not_null_test_backs(node: &Node, model: &NodeId, column: &str) -> bool {
    if node.resource_type() != "test" || node.attached_node() != Some(model) {
        return false;
    }
    if !test_is_enabled(node) {
        return false;
    }
    let Some(metadata) = node.test_metadata() else {
        return false;
    };
    if metadata.name() != "not_null" {
        return false;
    }
    let target = metadata
        .kwargs()
        .get("column_name")
        .and_then(serde_json::Value::as_str)
        .or_else(|| node.column_name());
    target.is_some_and(|t| t.eq_ignore_ascii_case(column))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::*;
    use crate::domain::manifest::{
        Checksum, ColumnFacts, DependsOn, Group, Manifest, ManifestMetadata, Node, NodeConfig,
        NodeId, Owner, TestMetadata,
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
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]), None, None);
        assert!(facts.has_content());
        assert!(!facts.is_empty());
    }

    #[test]
    fn ungrouped_in_scope_models_yield_no_chips() {
        let manifest = manifest_with(
            vec![model("model.pkg.a", None), model("model.pkg.b", None)],
            vec![],
        );
        let facts = gather_governance(
            &manifest,
            &in_scope(&["model.pkg.a", "model.pkg.b"]),
            None,
            None,
        );
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
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]), None, None);
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
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]), None, None);
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
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]), None, None);
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
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]), None, None);
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
            None,
            None,
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
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]), None, None);
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
        let facts = gather_governance(&manifest, &ModelInScopeSet::new(), None, None);
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
        let facts = gather_governance(manifest, &in_scope(&[model_id]), None, None);
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
        let facts = gather_governance(
            &manifest,
            &in_scope(&["model.pkg.a", "model.pkg.b"]),
            None,
            None,
        );
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
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.fct"]), None, None);
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
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.a"]), None, None);
        assert!(facts.blast_radius.is_empty());
        assert!(!facts.has_content(), "no group, no exposure ⇒ no DOM");
    }

    #[test]
    fn blast_radius_alone_makes_the_payload_non_empty() {
        // Ungrouped model reaching an exposure ⇒ has_content via the
        // blast-radius arm (the group_chips arm is empty).
        let manifest = manifest_one_exposure(None);
        let facts = gather_governance(&manifest, &in_scope(&["model.pkg.fct"]), None, None);
        assert!(facts.group_chips.is_empty());
        assert!(!facts.blast_radius.is_empty());
        assert!(facts.has_content());
        assert!(!facts.is_empty());
    }

    // ===== Slice 5: structural contract breaking-change classifier =====

    /// A contracted model-node builder. `cols` is `(name, type)` pairs;
    /// `mat` the materialization; `enforced` the `contract.enforced`
    /// flag; `checksum` the (optional) `contract.checksum`; `model_cons`
    /// the model-level constraints; `col_cons` the per-column constraints.
    struct ContractNode {
        cols: Vec<(&'static str, Option<&'static str>)>,
        mat: &'static str,
        enforced: bool,
        checksum: Option<&'static str>,
        model_cons: Vec<Constraint>,
        col_cons: Vec<(&'static str, Vec<Constraint>)>,
    }

    impl ContractNode {
        fn base(mat: &'static str, enforced: bool) -> Self {
            Self {
                cols: Vec::new(),
                mat,
                enforced,
                checksum: None,
                model_cons: Vec::new(),
                col_cons: Vec::new(),
            }
        }

        fn cols(mut self, cols: &[(&'static str, Option<&'static str>)]) -> Self {
            self.cols = cols.to_vec();
            self
        }

        fn checksum(mut self, checksum: &'static str) -> Self {
            self.checksum = Some(checksum);
            self
        }

        fn model_cons(mut self, cons: Vec<Constraint>) -> Self {
            self.model_cons = cons;
            self
        }

        fn col_cons(mut self, col: &'static str, cons: Vec<Constraint>) -> Self {
            self.col_cons.push((col, cons));
            self
        }

        fn build(self) -> Node {
            let mut config = BTreeMap::new();
            config.insert("materialized".to_owned(), serde_json::json!(self.mat));
            let columns: BTreeMap<String, Option<String>> = self
                .cols
                .iter()
                .map(|(n, t)| ((*n).to_owned(), t.map(str::to_owned)))
                .collect();
            let column_facts: BTreeMap<String, ColumnFacts> = self
                .col_cons
                .iter()
                .map(|(col, cons)| {
                    (
                        (*col).to_owned(),
                        ColumnFacts::new(None, Vec::new(), Vec::new(), cons.clone()),
                    )
                })
                .collect();
            Node::new(
                NodeId::new("model.pkg.m"),
                "model",
                sample_checksum(),
                Some("select 1".to_owned()),
                None,
                DependsOn::default(),
                None,
                NodeConfig::new(config, self.enforced),
                None,
                columns,
            )
            .with_contract_facts(
                self.model_cons.clone(),
                Vec::new(),
                self.checksum.map(str::to_owned),
            )
            .with_column_facts(column_facts)
        }
    }

    fn pk(col: &str) -> Constraint {
        Constraint::new(
            "primary_key",
            vec![col.to_owned()],
            None,
            None,
            None,
            Vec::new(),
        )
    }

    fn not_null() -> Constraint {
        Constraint::new("not_null", Vec::new(), None, None, None, Vec::new())
    }

    fn custom() -> Constraint {
        Constraint::new("custom", Vec::new(), None, None, None, Vec::new())
    }

    // ---- table-driven transitions (one per Intel-C category) ----

    #[test]
    fn unenforced_both_sides_is_unchanged() {
        let old = ContractNode::base("table", false).build();
        let current = ContractNode::base("table", false).build();
        assert_eq!(classify_contract(&old, &current), ContractChange::Unchanged);
    }

    #[test]
    fn newly_enforced_is_changed_not_breaking() {
        let old = ContractNode::base("table", false).build();
        let current = ContractNode::base("table", true).build();
        assert_eq!(
            classify_contract(&old, &current),
            ContractChange::ChangedNotBreaking,
        );
    }

    #[test]
    fn enforcement_dropped_is_breaking() {
        let old = ContractNode::base("table", true).build();
        let current = ContractNode::base("table", false).build();
        assert_eq!(
            classify_contract(&old, &current),
            ContractChange::Breaking(vec![BreakingReason::ContractEnforcedDisabled]),
        );
    }

    #[test]
    fn identical_enforced_checksums_short_circuit_unchanged() {
        // The fast path: both enforce + identical non-null checksums ⇒
        // unchanged WITHOUT a structural diff (even if columns differ —
        // proving the short-circuit fires).
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .checksum("abc")
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("string"))]) // would be a type change…
            .checksum("abc") // …but the checksum short-circuits
            .build();
        assert_eq!(classify_contract(&old, &current), ContractChange::Unchanged);
    }

    #[test]
    fn column_added_is_not_breaking() {
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("int")), ("b", Some("string"))])
            .build();
        assert_eq!(classify_contract(&old, &current), ContractChange::Unchanged);
    }

    #[test]
    fn column_removed_is_breaking() {
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int")), ("b", Some("string"))])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        assert_eq!(
            classify_contract(&old, &current),
            ContractChange::Breaking(vec![BreakingReason::ColumnsRemoved(vec!["b".to_owned()])]),
        );
    }

    #[test]
    fn column_type_changed_is_breaking() {
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("string"))])
            .build();
        assert_eq!(
            classify_contract(&old, &current),
            ContractChange::Breaking(vec![BreakingReason::ColumnTypeChanged {
                col: "a".to_owned(),
                prev: "int".to_owned(),
                current: "string".to_owned(),
            }]),
        );
    }

    #[test]
    fn alias_type_change_is_not_breaking() {
        // string ↔ varchar under alias_types ⇒ unchanged.
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("string"))])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("varchar(255)"))])
            .build();
        assert_eq!(classify_contract(&old, &current), ContractChange::Unchanged);
    }

    #[test]
    fn both_undeclared_column_types_are_equal() {
        // (None, None) ⇒ equal: a column untyped on both sides is no
        // change.
        let old = ContractNode::base("table", true)
            .cols(&[("a", None)])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", None)])
            .build();
        assert_eq!(classify_contract(&old, &current), ContractChange::Unchanged);
    }

    #[test]
    fn declared_to_undeclared_type_is_breaking() {
        // (Some, None) ⇒ not equal: dropping a declared type is a type
        // change (engine fallback labels the current side "unknown").
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", None)])
            .build();
        assert_eq!(
            classify_contract(&old, &current),
            ContractChange::Breaking(vec![BreakingReason::ColumnTypeChanged {
                col: "a".to_owned(),
                prev: "int".to_owned(),
                current: "unknown".to_owned(),
            }]),
        );
    }

    #[test]
    fn model_constraint_removed_is_breaking_on_table() {
        let old = ContractNode::base("table", true)
            .model_cons(vec![pk("id")])
            .build();
        let current = ContractNode::base("table", true).build();
        assert_eq!(
            classify_contract(&old, &current),
            ContractChange::Breaking(vec![BreakingReason::EnforcedModelConstraintRemoved]),
        );
    }

    #[test]
    fn model_constraint_removed_on_view_is_not_breaking() {
        // A view never enforces constraints — removing one is not
        // breaking (the materialization gate).
        let old = ContractNode::base("view", true)
            .model_cons(vec![pk("id")])
            .build();
        let current = ContractNode::base("view", true).build();
        assert_eq!(classify_contract(&old, &current), ContractChange::Unchanged);
    }

    #[test]
    fn column_constraint_removed_is_breaking_on_incremental() {
        let old = ContractNode::base("incremental", true)
            .cols(&[("id", Some("int"))])
            .col_cons("id", vec![not_null()])
            .build();
        let current = ContractNode::base("incremental", true)
            .cols(&[("id", Some("int"))])
            .build();
        assert_eq!(
            classify_contract(&old, &current),
            ContractChange::Breaking(vec![BreakingReason::EnforcedColumnConstraintRemoved]),
        );
    }

    #[test]
    fn custom_column_constraint_removed_is_not_breaking() {
        // Custom column-level constraints are dropped (dbt convention).
        let old = ContractNode::base("table", true)
            .cols(&[("id", Some("int"))])
            .col_cons("id", vec![custom()])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("id", Some("int"))])
            .build();
        assert_eq!(classify_contract(&old, &current), ContractChange::Unchanged);
    }

    #[test]
    fn materialization_off_table_with_constraints_is_breaking() {
        // table → view while a model constraint existed ⇒ both the
        // constraint-removed-style reason AND MaterializationChanged.
        let old = ContractNode::base("table", true)
            .model_cons(vec![pk("id")])
            .build();
        let current = ContractNode::base("view", true)
            .model_cons(vec![pk("id")])
            .build();
        let ContractChange::Breaking(reasons) = classify_contract(&old, &current) else {
            panic!("expected breaking");
        };
        assert!(reasons.contains(&BreakingReason::MaterializationChanged));
    }

    #[test]
    fn materialization_change_with_no_constraints_is_not_breaking() {
        // table → view but no constraints existed ⇒ not breaking (the
        // had_constraints guard).
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let current = ContractNode::base("view", true)
            .cols(&[("a", Some("int"))])
            .build();
        assert_eq!(classify_contract(&old, &current), ContractChange::Unchanged);
    }

    // ---- property-shaped tests ----

    #[test]
    fn classify_is_reflexive_unchanged() {
        // classify(n, n) == Unchanged for a rich contracted node.
        let node = ContractNode::base("table", true)
            .cols(&[("id", Some("int")), ("name", Some("varchar"))])
            .model_cons(vec![pk("id")])
            .col_cons("id", vec![not_null()])
            .checksum("xyz")
            .build();
        assert_eq!(classify_contract(&node, &node), ContractChange::Unchanged);
    }

    #[test]
    fn alias_symmetry_holds_for_known_families() {
        // string ↔ varchar ↔ text (and int ↔ integer, bool ↔ boolean)
        // canonicalize equal under alias_types in both directions.
        for (a, b) in [
            ("string", "varchar"),
            ("varchar", "text"),
            ("int", "integer"),
            ("bool", "boolean"),
            ("float", "double precision"),
        ] {
            let old = ContractNode::base("table", true)
                .cols(&[("c", Some(a))])
                .build();
            let current = ContractNode::base("table", true)
                .cols(&[("c", Some(b))])
                .build();
            assert_eq!(
                classify_contract(&old, &current),
                ContractChange::Unchanged,
                "{a} ↔ {b} should be alias-equal",
            );
            // Symmetric the other direction.
            assert_eq!(
                classify_contract(&current, &old),
                ContractChange::Unchanged,
                "{b} ↔ {a} (reverse) should be alias-equal",
            );
        }
    }

    #[test]
    fn null_checksum_does_not_short_circuit_falls_through_to_structure() {
        // fusion frequently null-fills contract.checksum (dbt-core#8030);
        // a None checksum must NOT short-circuit — the structure decides.
        // Here the structure is identical ⇒ Unchanged via the diff, not
        // the checksum fast path.
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        assert_eq!(classify_contract(&old, &current), ContractChange::Unchanged);
        // …and a real type change with null checksums is still caught.
        let changed = ContractNode::base("table", true)
            .cols(&[("a", Some("date"))])
            .build();
        assert_eq!(
            classify_contract(&old, &changed),
            ContractChange::Breaking(vec![BreakingReason::ColumnTypeChanged {
                col: "a".to_owned(),
                prev: "int".to_owned(),
                current: "date".to_owned(),
            }]),
        );
    }

    #[test]
    fn multiple_breaking_reasons_accumulate() {
        // A column removed AND a type changed AND a model constraint
        // removed (on table) ⇒ all three reasons fire.
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int")), ("b", Some("string"))])
            .model_cons(vec![pk("a")])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("date"))]) // b removed, a retyped
            .build();
        let ContractChange::Breaking(reasons) = classify_contract(&old, &current) else {
            panic!("expected breaking");
        };
        assert!(reasons.contains(&BreakingReason::ColumnsRemoved(vec!["b".to_owned()])));
        assert!(reasons.iter().any(|r| matches!(
            r,
            BreakingReason::ColumnTypeChanged { col, .. } if col == "a"
        )));
        assert!(reasons.contains(&BreakingReason::EnforcedModelConstraintRemoved));
    }

    // ===== Slice 2: contract classes through gather_governance =====

    /// Wrap a single contract node (`model.pkg.m`) into a manifest.
    fn manifest_one_model(node: Node) -> Manifest {
        let mut nodes = HashMap::new();
        nodes.insert(node.id().clone(), node);
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn classes_for(old: Node, current: Node) -> Vec<ContractClass> {
        let old_m = manifest_one_model(old);
        let current_m = manifest_one_model(current);
        let in_scope = in_scope(&["model.pkg.m"]);
        gather_governance(&current_m, &in_scope, Some(&old_m), None).contract_classes
    }

    #[test]
    fn no_baseline_yields_no_contract_classes() {
        // The --pr-diff arm (old_manifest = None): no classification.
        let current_m = manifest_one_model(
            ContractNode::base("table", true)
                .cols(&[("a", Some("int"))])
                .build(),
        );
        let facts = gather_governance(&current_m, &in_scope(&["model.pkg.m"]), None, None);
        assert!(facts.contract_classes.is_empty());
    }

    #[test]
    fn unchanged_contract_yields_no_class() {
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        assert!(classes_for(old, current).is_empty());
    }

    #[test]
    fn breaking_type_change_yields_a_breaking_class_with_column_diff() {
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("date"))])
            .build();
        let classes = classes_for(old, current);
        assert_eq!(classes.len(), 1);
        let c = &classes[0];
        assert_eq!(c.model, "m");
        assert_eq!(c.verdict, "breaking");
        assert_eq!(
            c.column_diffs,
            vec![ContractColumnDiff {
                name: "a".to_owned(),
                old: "int".to_owned(),
                new: "date".to_owned(),
                verdict: "breaking".to_owned(),
            }],
        );
        assert!(
            c.reasons.is_empty(),
            "a pure type change has no extra lines"
        );
        assert!(c.chip.starts_with("Contract: enforced"));
    }

    #[test]
    fn newly_enforced_yields_a_safe_class() {
        let old = ContractNode::base("table", false).build();
        let current = ContractNode::base("table", true).build();
        let classes = classes_for(old, current);
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].verdict, "safe");
        assert!(classes[0].column_diffs.is_empty());
        assert_eq!(
            classes[0].reasons,
            vec!["Contract is now enforced (newly contracted).".to_owned()],
        );
    }

    #[test]
    fn columns_removed_rides_a_reason_line_not_a_column_diff() {
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int")), ("b", Some("string"))])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let classes = classes_for(old, current);
        assert_eq!(classes[0].verdict, "breaking");
        assert!(classes[0].column_diffs.is_empty());
        assert_eq!(classes[0].reasons, vec!["Columns removed: b.".to_owned()]);
    }

    #[test]
    fn contract_class_makes_payload_non_empty() {
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("date"))])
            .build();
        let old_m = manifest_one_model(old);
        let current_m = manifest_one_model(current);
        let facts = gather_governance(&current_m, &in_scope(&["model.pkg.m"]), Some(&old_m), None);
        assert!(!facts.contract_classes.is_empty());
        assert!(facts.has_content());
        assert!(!facts.is_empty());
    }

    // ---- contract chip formatting ----

    fn chip_of(build: impl FnOnce(Node) -> Node) -> String {
        let base = ContractNode::base("table", true).build();
        contract_chip(&build(base))
    }

    #[test]
    fn chip_names_enforcement() {
        assert!(chip_of(|n| n).starts_with("Contract: enforced"));
    }

    #[test]
    fn chip_includes_version_access_and_group() {
        let node = ContractNode::base("table", true)
            .build()
            .with_versions(Some("2".to_owned()), Some("3".to_owned()), None)
            .with_governance(Some("finance".to_owned()), Some("public".to_owned()));
        let chip = contract_chip(&node);
        assert_eq!(
            chip,
            "Contract: enforced · v2 of 3 · access: public · group finance",
        );
    }

    #[test]
    fn chip_version_normalizes_string_or_integer_identically() {
        // `version` arrives post-normalized to a string (the wire
        // StringOrInteger — `2` and `"2"` are identical by the time the
        // node carries it). The chip reads that normalized string.
        let v_int_like = ContractNode::base("table", true).build().with_versions(
            Some("2".to_owned()),
            None,
            None,
        );
        assert_eq!(contract_version_segment(&v_int_like), Some("v2".to_owned()));
        // No latest_version ⇒ the `of {latest}` half is dropped.
        let unversioned = ContractNode::base("table", true).build();
        assert_eq!(contract_version_segment(&unversioned), None);
    }

    #[test]
    fn chip_names_unenforced_when_the_flag_is_off() {
        let node = ContractNode::base("table", false).build();
        assert!(contract_chip(&node).starts_with("Contract: unenforced"));
    }

    #[test]
    fn every_breaking_reason_line_renders() {
        // Direct coverage of each non-column reason's copy.
        assert_eq!(
            breaking_reason_line(&BreakingReason::ContractEnforcedDisabled),
            "Contract enforcement was removed.",
        );
        assert_eq!(
            breaking_reason_line(&BreakingReason::ColumnsRemoved(vec![
                "a".to_owned(),
                "b".to_owned(),
            ])),
            "Columns removed: a, b.",
        );
        assert_eq!(
            breaking_reason_line(&BreakingReason::EnforcedColumnConstraintRemoved),
            "An enforced column-level constraint was removed.",
        );
        assert_eq!(
            breaking_reason_line(&BreakingReason::EnforcedModelConstraintRemoved),
            "An enforced model-level constraint was removed.",
        );
        assert_eq!(
            breaking_reason_line(&BreakingReason::MaterializationChanged),
            "Materialization moved off a constraint-enforcing strategy.",
        );
        // The ColumnTypeChanged arm is unreachable via breaking_to_rows
        // (it rides a column-diff row); the line form is exercised here as
        // a defensive fallback.
        assert_eq!(
            breaking_reason_line(&BreakingReason::ColumnTypeChanged {
                col: "a".to_owned(),
                prev: "int".to_owned(),
                current: "date".to_owned(),
            }),
            "Column a type changed (int → date).",
        );
    }

    #[test]
    fn enforcement_dropped_yields_a_breaking_class_with_a_reason_line() {
        // Drives the ContractEnforcedDisabled reason through the full
        // gather path (old enforced, current not).
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let current = ContractNode::base("table", false)
            .cols(&[("a", Some("int"))])
            .build();
        let classes = classes_for(old, current);
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].verdict, "breaking");
        assert_eq!(
            classes[0].reasons,
            vec!["Contract enforcement was removed.".to_owned()],
        );
    }

    #[test]
    fn newly_added_model_has_no_old_node_so_no_class() {
        // The current model exists but the OLD manifest omits it ⇒
        // classify_model_contract returns None (no prior contract).
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let current_m = manifest_one_model(current);
        let old_m = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let facts = gather_governance(&current_m, &in_scope(&["model.pkg.m"]), Some(&old_m), None);
        assert!(facts.contract_classes.is_empty());
    }

    // ===== Slice 3: enforcement matrix + backing-test join =====

    use crate::domain::manifest::ConstraintKind;

    #[test]
    fn enforcement_matrix_matches_the_fusion_source() {
        use ConstraintKind::{Check, Custom, ForeignKey, NotNull, PrimaryKey, Unique};
        use ConstraintSupport::{Enforced, NotEnforced, NotSupported};
        // One assertion per (adapter, kind) cell, verbatim from fusion's
        // get_constraint_support (dbt-adapter adapter_impl.rs:2328).
        let cases = [
            // Postgres (and DuckDB "follows Postgres").
            ("postgres", NotNull, Enforced),
            ("postgres", ForeignKey, Enforced),
            ("postgres", Unique, NotEnforced),
            ("postgres", PrimaryKey, NotEnforced),
            ("postgres", Check, NotSupported),
            ("duckdb", PrimaryKey, NotEnforced),
            ("duckdb", NotNull, Enforced),
            // Snowflake.
            ("snowflake", NotNull, Enforced),
            ("snowflake", PrimaryKey, NotEnforced),
            ("snowflake", Unique, NotEnforced),
            ("snowflake", Check, NotSupported),
            // BigQuery — unique is NOT supported; PK not enforced.
            ("bigquery", NotNull, Enforced),
            ("bigquery", PrimaryKey, NotEnforced),
            ("bigquery", Unique, NotSupported),
            // Databricks — unique not supported; check IS enforced.
            ("databricks", PrimaryKey, NotEnforced),
            ("databricks", Unique, NotSupported),
            ("databricks", Check, Enforced),
            ("databricks", NotNull, Enforced),
            // Redshift.
            ("redshift", NotNull, Enforced),
            ("redshift", PrimaryKey, NotEnforced),
            ("redshift", Unique, NotEnforced),
            // Custom is never enforced anywhere we model.
            ("postgres", Custom, NotSupported),
        ];
        for (adapter, kind, expected) in cases {
            assert_eq!(
                constraint_support(adapter, kind),
                expected,
                "{adapter} × {kind:?}",
            );
        }
    }

    #[test]
    fn enforcement_matrix_is_case_insensitive_on_adapter() {
        assert_eq!(
            constraint_support("Postgres", ConstraintKind::PrimaryKey),
            ConstraintSupport::NotEnforced,
        );
        assert_eq!(
            constraint_support("DUCKDB", ConstraintKind::NotNull),
            ConstraintSupport::Enforced,
        );
    }

    #[test]
    fn unknown_adapter_degrades_to_not_enforced_never_a_false_claim() {
        // An adapter outside the matrix (or fusion-unimplemented like
        // athena/spark) is the honest "declared, enforcement unknown" —
        // never a false Enforced, never invents NotSupported.
        assert_eq!(
            constraint_support("athena", ConstraintKind::PrimaryKey),
            ConstraintSupport::NotEnforced,
        );
        assert_eq!(
            constraint_support("some-future-warehouse", ConstraintKind::NotNull),
            ConstraintSupport::NotEnforced,
        );
        // An Other/unknown constraint kind also degrades.
        assert_eq!(
            constraint_support("postgres", ConstraintKind::Other),
            ConstraintSupport::NotEnforced,
        );
    }

    // ---- backing_test_for: the inferred constraint→test edge ----

    /// A generic data-test node attached to `model`, asserting `test_name`
    /// on `column`.
    fn test_node(id: &str, model: &str, test_name: &str, column: &str) -> Node {
        Node::new(
            NodeId::new(id),
            "test",
            sample_checksum(),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            Some(column.to_owned()),
            Some(NodeId::new(model)),
            Some(TestMetadata::new(test_name, None, serde_json::json!({}))),
        )
    }

    fn manifest_with_nodes(nodes: Vec<Node>) -> Manifest {
        let node_map: HashMap<NodeId, Node> =
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect();
        Manifest::new(
            ManifestMetadata::new("v12"),
            node_map,
            HashMap::new(),
            HashMap::new(),
        )
    }

    #[test]
    fn backing_test_present_returns_the_real_test_node_id() {
        // The id is the REAL test node — so a Covered finding attributes
        // the actual test, never a synthetic label (the Verdict::Covered.by
        // contract; CodeRabbit on #339).
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            test_node("test.pkg.u", "model.pkg.m", "unique", "id"),
        ]);
        assert_eq!(
            backing_test_for(&manifest, &model_id, "id", ConstraintKind::PrimaryKey),
            Some(&NodeId::new("test.pkg.u")),
        );
        assert_eq!(
            backing_test_for(&manifest, &model_id, "id", ConstraintKind::Unique),
            Some(&NodeId::new("test.pkg.u")),
        );
    }

    #[test]
    fn backing_test_missing_when_no_test_targets_the_column() {
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            test_node("test.pkg.u", "model.pkg.m", "unique", "other_col"),
        ]);
        assert!(backing_test_for(&manifest, &model_id, "id", ConstraintKind::PrimaryKey).is_none());
    }

    #[test]
    fn backing_test_inference_misses_a_renamed_or_singular_test() {
        // The HEDGE case: a test that asserts the same uniqueness but
        // under a different test_metadata.name (e.g. a custom/renamed
        // generic test) is NOT matched — the surface copy admits this.
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            test_node("test.pkg.x", "model.pkg.m", "my_custom_unique", "id"),
        ]);
        assert!(
            backing_test_for(&manifest, &model_id, "id", ConstraintKind::PrimaryKey).is_none(),
            "the inferred edge can miss a renamed test (the hedge)",
        );
    }

    #[test]
    fn backing_test_reads_the_column_name_kwarg_over_the_node_field() {
        // The test's column comes from the column_name kwarg when present.
        let model_id = NodeId::new("model.pkg.m");
        let mut t = test_node("test.pkg.u", "model.pkg.m", "unique", "wrong");
        t = t.with_test_attachment(
            Some("wrong".to_owned()),
            Some(NodeId::new("model.pkg.m")),
            Some(TestMetadata::new(
                "unique",
                None,
                serde_json::json!({"column_name": "id"}),
            )),
        );
        let manifest = manifest_with_nodes(vec![model("model.pkg.m", None), t]);
        assert!(backing_test_for(&manifest, &model_id, "id", ConstraintKind::Unique).is_some());
    }

    #[test]
    fn backing_test_ignores_a_test_attached_to_a_different_model() {
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            test_node("test.pkg.u", "model.pkg.other", "unique", "id"),
        ]);
        assert!(backing_test_for(&manifest, &model_id, "id", ConstraintKind::Unique).is_none());
    }

    #[test]
    fn backing_test_for_non_unique_kinds_is_always_none() {
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![model("model.pkg.m", None)]);
        for kind in [
            ConstraintKind::ForeignKey,
            ConstraintKind::Check,
            ConstraintKind::Custom,
        ] {
            assert!(backing_test_for(&manifest, &model_id, "id", kind).is_none());
        }
    }

    #[test]
    fn not_null_constraint_matches_a_not_null_test() {
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            test_node("test.pkg.nn", "model.pkg.m", "not_null", "id"),
        ]);
        assert!(backing_test_for(&manifest, &model_id, "id", ConstraintKind::NotNull).is_some());
    }

    #[test]
    fn not_null_backing_ignores_wrong_column_model_and_disabled() {
        let model_id = NodeId::new("model.pkg.m");
        // Wrong column.
        let m1 = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            test_node("test.pkg.nn", "model.pkg.m", "not_null", "other"),
        ]);
        assert!(backing_test_for(&m1, &model_id, "id", ConstraintKind::NotNull).is_none());
        // Attached to a different model.
        let m2 = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            test_node("test.pkg.nn", "model.pkg.other", "not_null", "id"),
        ]);
        assert!(backing_test_for(&m2, &model_id, "id", ConstraintKind::NotNull).is_none());
        // A unique test does NOT back a NotNull constraint (wrong name).
        let m3 = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            test_node("test.pkg.u", "model.pkg.m", "unique", "id"),
        ]);
        assert!(backing_test_for(&m3, &model_id, "id", ConstraintKind::NotNull).is_none());
        // No test node at all.
        let m4 = manifest_with_nodes(vec![model("model.pkg.m", None)]);
        assert!(backing_test_for(&m4, &model_id, "id", ConstraintKind::NotNull).is_none());
    }

    #[test]
    fn an_other_constraint_kind_has_no_backing() {
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![model("model.pkg.m", None)]);
        assert!(backing_test_for(&manifest, &model_id, "id", ConstraintKind::ForeignKey).is_none());
    }

    #[test]
    fn a_disabled_backing_test_does_not_count() {
        // config.enabled: false ⇒ the test asserts nothing ⇒ not backing.
        let model_id = NodeId::new("model.pkg.m");
        let mut config = BTreeMap::new();
        config.insert("enabled".to_owned(), serde_json::json!(false));
        let disabled = Node::new(
            NodeId::new("test.pkg.u"),
            "test",
            sample_checksum(),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            Some("id".to_owned()),
            Some(NodeId::new("model.pkg.m")),
            Some(TestMetadata::new("unique", None, serde_json::json!({}))),
        );
        let manifest = manifest_with_nodes(vec![model("model.pkg.m", None), disabled]);
        assert!(backing_test_for(&manifest, &model_id, "id", ConstraintKind::Unique).is_none());
    }

    #[test]
    fn a_non_test_node_is_never_a_backing_test() {
        // No test node at all ⇒ no backing.
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![model("model.pkg.m", None)]);
        assert!(backing_test_for(&manifest, &model_id, "id", ConstraintKind::Unique).is_none());
    }

    // ---- backing_test_for_columns: composite-key join (cute-dbt#341) ----

    /// A `unique_combination_of_columns` test node over `cols`, attached
    /// to `model`.
    fn combo_node(id: &str, model: &str, cols: &[&str]) -> Node {
        Node::new(
            NodeId::new(id),
            "test",
            sample_checksum(),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            None,
            Some(NodeId::new(model)),
            Some(TestMetadata::new(
                "unique_combination_of_columns",
                Some("dbt_utils".to_owned()),
                serde_json::json!({ "combination_of_columns": cols }),
            )),
        )
    }

    fn cols(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn an_exact_match_combination_test_backs_and_returns_the_id() {
        // Exact match is the subset boundary case — it backs.
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            combo_node("test.pkg.combo", "model.pkg.m", &["a", "b"]),
        ]);
        assert_eq!(
            backing_test_for_columns(&manifest, &model_id, &cols(&["a", "b"])),
            Some(&NodeId::new("test.pkg.combo")),
        );
    }

    #[test]
    fn columns_match_is_order_and_case_independent() {
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            combo_node("test.pkg.combo", "model.pkg.m", &["B", "a"]),
        ]);
        assert!(
            backing_test_for_columns(&manifest, &model_id, &cols(&["a", "b"])).is_some(),
            "order + case don't matter — it's a set comparison",
        );
    }

    #[test]
    fn a_subset_combination_backs_the_tuple() {
        // `{a}` unique ⟹ `(a, b)` unique — a uniqueness test on a proper
        // SUBSET of the key is a stronger guarantee that backs it (the
        // #259 grain `covers_key` direction; cute-dbt#341 corrected).
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            combo_node("test.pkg.combo", "model.pkg.m", &["a"]),
        ]);
        assert_eq!(
            backing_test_for_columns(&manifest, &model_id, &cols(&["a", "b"])),
            Some(&NodeId::new("test.pkg.combo")),
            "a subset uniqueness test backs the wider key",
        );
    }

    #[test]
    fn a_single_column_unique_backs_a_composite_key() {
        // A plain `unique` on one tuple column is a 1-element subset of
        // the key — it backs the composite key.
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            test_node("test.pkg.u", "model.pkg.m", "unique", "a"),
        ]);
        assert_eq!(
            backing_test_for_columns(&manifest, &model_id, &cols(&["a", "b"])),
            Some(&NodeId::new("test.pkg.u")),
        );
    }

    #[test]
    fn a_superset_combination_does_not_back_the_tuple() {
        // A superset is NOT a subset — uniqueness of `(a, b, c)` does not
        // imply uniqueness of `(a, b)`, so it does not back the key.
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            combo_node("test.pkg.combo", "model.pkg.m", &["a", "b", "c"]),
        ]);
        assert!(
            backing_test_for_columns(&manifest, &model_id, &cols(&["a", "b"])).is_none(),
            "a wider combination is not a subset of the key",
        );
    }

    #[test]
    fn a_partial_overlap_combination_does_not_back_the_tuple() {
        // `{a, c}` is neither a subset nor a superset of `{a, b}` — it
        // overlaps but is not contained, so it does not back the key.
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            combo_node("test.pkg.combo", "model.pkg.m", &["a", "c"]),
        ]);
        assert!(
            backing_test_for_columns(&manifest, &model_id, &cols(&["a", "b"])).is_none(),
            "partial overlap (not contained) is not coverage",
        );
    }

    #[test]
    fn a_single_column_unique_backs_a_single_column_key_via_columns() {
        // The single-column key path: a plain `unique` is set-equal to a
        // 1-column tuple.
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![
            model("model.pkg.m", None),
            test_node("test.pkg.u", "model.pkg.m", "unique", "id"),
        ]);
        assert_eq!(
            backing_test_for_columns(&manifest, &model_id, &cols(&["id"])),
            Some(&NodeId::new("test.pkg.u")),
        );
    }

    #[test]
    fn empty_columns_never_back() {
        let model_id = NodeId::new("model.pkg.m");
        let manifest = manifest_with_nodes(vec![model("model.pkg.m", None)]);
        assert!(backing_test_for_columns(&manifest, &model_id, &[]).is_none());
    }

    #[test]
    fn a_disabled_combination_test_does_not_back() {
        let model_id = NodeId::new("model.pkg.m");
        let mut config = BTreeMap::new();
        config.insert("enabled".to_owned(), serde_json::json!(false));
        let disabled = Node::new(
            NodeId::new("test.pkg.combo"),
            "test",
            sample_checksum(),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            None,
            Some(NodeId::new("model.pkg.m")),
            Some(TestMetadata::new(
                "unique_combination_of_columns",
                Some("dbt_utils".to_owned()),
                serde_json::json!({ "combination_of_columns": ["a", "b"] }),
            )),
        );
        let manifest = manifest_with_nodes(vec![model("model.pkg.m", None), disabled]);
        assert!(backing_test_for_columns(&manifest, &model_id, &cols(&["a", "b"])).is_none());
    }

    // ===== Slice 4: dbt-native lifecycle chips =====

    /// A model node with the full Slice-4 governance field set + refs.
    #[allow(clippy::too_many_arguments)]
    fn gov_model(
        full_id: &str,
        access: Option<&str>,
        group: Option<&str>,
        version: Option<&str>,
        latest: Option<&str>,
        deprecation: Option<&str>,
        refs: &[&str],
    ) -> Node {
        Node::new(
            NodeId::new(full_id),
            "model",
            sample_checksum(),
            Some("select 1".to_owned()),
            None,
            DependsOn::new(Vec::new(), refs.iter().map(|r| NodeId::new(*r)).collect()),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_governance(group.map(str::to_owned), access.map(str::to_owned))
        .with_versions(
            version.map(str::to_owned),
            latest.map(str::to_owned),
            deprecation.map(str::to_owned),
        )
    }

    /// The lifecycle chips for one in-scope model, gathered through the
    /// public entry (so the assembler order is exercised end to end).
    fn chips_for(manifest: &Manifest, model_id: &str, today: Option<DepDate>) -> Vec<GovChip> {
        gather_governance(manifest, &in_scope(&[model_id]), None, today).lifecycle_chips
    }

    fn far_past() -> DepDate {
        DepDate {
            year: 2020,
            month: 1,
            day: 1,
        }
    }

    fn far_future() -> DepDate {
        DepDate {
            year: 2099,
            month: 1,
            day: 1,
        }
    }

    fn today_2026() -> DepDate {
        DepDate {
            year: 2026,
            month: 6,
            day: 13,
        }
    }

    // ---- DepDate::parse ----

    #[test]
    fn dep_date_parses_a_well_formed_date() {
        assert_eq!(
            DepDate::parse("2024-12-31"),
            Some(DepDate {
                year: 2024,
                month: 12,
                day: 31
            }),
        );
    }

    #[test]
    fn dep_date_rejects_malformed_dates_without_panic() {
        for bad in [
            "",
            "2024",
            "2024-12",
            "2024-13-01", // month out of range
            "2024-00-01", // month 0
            "2024-12-32", // day out of range
            "2024-12-00", // day 0
            "not-a-date",
            "2024-12-31-extra", // too many parts
            "2024/12/31",       // wrong separator
        ] {
            assert_eq!(DepDate::parse(bad), None, "{bad:?} must not parse");
        }
    }

    // ---- public-surface-changed ----

    /// Manifest with an old + current node pair for the contract classify
    /// path, plus an optional group for owner chips.
    fn manifest_pair(old: Node, current: Node) -> (Manifest, Manifest) {
        let mut old_nodes = HashMap::new();
        old_nodes.insert(old.id().clone(), old);
        let mut cur_nodes = HashMap::new();
        cur_nodes.insert(current.id().clone(), current);
        let mk = |nodes| {
            Manifest::new(
                ManifestMetadata::new("v12"),
                nodes,
                HashMap::new(),
                HashMap::new(),
            )
        };
        (mk(old_nodes), mk(cur_nodes))
    }

    #[test]
    fn public_breaking_contract_fires_the_chip() {
        // public access + a breaking contract change (a removed column).
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int")), ("b", Some("string"))])
            .build()
            .with_governance(None, Some("public".to_owned()));
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))]) // b removed ⇒ breaking
            .build()
            .with_governance(None, Some("public".to_owned()));
        let (old_m, cur_m) = manifest_pair(old, current);
        let facts = gather_governance(&cur_m, &in_scope(&["model.pkg.m"]), Some(&old_m), None);
        assert!(
            facts
                .lifecycle_chips
                .iter()
                .any(|c| c.kind == "public-surface-changed"
                    && c.label == "Public contract — breaking change"),
        );
    }

    #[test]
    fn protected_breaking_contract_does_not_fire_the_public_chip() {
        // The default access (protected — missing ≠ public): no public chip
        // even on a breaking change.
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int")), ("b", Some("string"))])
            .build();
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build();
        let (old_m, cur_m) = manifest_pair(old, current);
        let facts = gather_governance(&cur_m, &in_scope(&["model.pkg.m"]), Some(&old_m), None);
        assert!(
            !facts
                .lifecycle_chips
                .iter()
                .any(|c| c.kind == "public-surface-changed"),
            "missing access defaults to protected — never public",
        );
    }

    #[test]
    fn public_non_breaking_change_does_not_fire_the_chip() {
        // public + a column ADD (safe, not breaking) ⇒ no chip.
        let old = ContractNode::base("table", true)
            .cols(&[("a", Some("int"))])
            .build()
            .with_governance(None, Some("public".to_owned()));
        let current = ContractNode::base("table", true)
            .cols(&[("a", Some("int")), ("b", Some("string"))]) // add ⇒ safe
            .build()
            .with_governance(None, Some("public".to_owned()));
        let (old_m, cur_m) = manifest_pair(old, current);
        let facts = gather_governance(&cur_m, &in_scope(&["model.pkg.m"]), Some(&old_m), None);
        assert!(
            !facts
                .lifecycle_chips
                .iter()
                .any(|c| c.kind == "public-surface-changed"),
        );
    }

    // ---- version-bump-without-latest ----

    #[test]
    fn version_ahead_of_latest_fires_an_advisory_chip() {
        let m = manifest_with_nodes(vec![gov_model(
            "model.pkg.m",
            None,
            None,
            Some("3"),
            Some("2"),
            None,
            &[],
        )]);
        let chips = chips_for(&m, "model.pkg.m", None);
        let chip = chips
            .iter()
            .find(|c| c.kind == "version-bump-without-latest")
            .expect("version chip fires");
        assert_eq!(chip.label, "m v3 — latest_version still v2");
        assert!(chip.severity.is_none(), "advisory — no severity");
    }

    #[test]
    fn version_equal_to_latest_does_not_fire() {
        // Normalized strings — `2` == `"2"` at the node layer; equal ⇒ no
        // chip.
        let m = manifest_with_nodes(vec![gov_model(
            "model.pkg.m",
            None,
            None,
            Some("2"),
            Some("2"),
            None,
            &[],
        )]);
        assert!(
            !chips_for(&m, "model.pkg.m", None)
                .iter()
                .any(|c| c.kind == "version-bump-without-latest"),
        );
    }

    #[test]
    fn unversioned_model_fires_no_version_chip() {
        let m = manifest_with_nodes(vec![gov_model(
            "model.pkg.m",
            None,
            None,
            None,
            None,
            None,
            &[],
        )]);
        assert!(
            !chips_for(&m, "model.pkg.m", None)
                .iter()
                .any(|c| c.kind == "version-bump-without-latest"),
        );
    }

    // ---- group-owner-touch ----

    #[test]
    fn grouped_model_with_owner_email_fires_the_touch_chip() {
        let m = manifest_with(
            vec![gov_model(
                "model.pkg.m",
                None,
                Some("finance"),
                None,
                None,
                None,
                &[],
            )],
            vec![Group::new(
                "finance",
                Some(Owner::new(None, vec!["fin@corp.example".to_owned()])),
            )],
        );
        let chip = chips_for(&m, "model.pkg.m", None)
            .into_iter()
            .find(|c| c.kind == "group-owner-touch")
            .expect("group-owner chip fires");
        assert_eq!(
            chip.label,
            "Touches group finance (owner: fin@corp.example)"
        );
    }

    #[test]
    fn grouped_model_without_owner_email_fires_no_touch_chip() {
        let m = manifest_with(
            vec![gov_model(
                "model.pkg.m",
                None,
                Some("finance"),
                None,
                None,
                None,
                &[],
            )],
            vec![Group::new("finance", None)],
        );
        assert!(
            !chips_for(&m, "model.pkg.m", None)
                .iter()
                .any(|c| c.kind == "group-owner-touch"),
        );
    }

    // ---- ref-to-deprecated (dual state) ----

    #[test]
    fn ref_to_elapsed_deprecation_is_danger() {
        // Far-past date ⇒ always elapsed ⇒ danger (stable golden).
        let m = manifest_with_nodes(vec![
            gov_model(
                "model.pkg.a",
                None,
                None,
                None,
                None,
                None,
                &["model.pkg.dep"],
            ),
            gov_model(
                "model.pkg.dep",
                None,
                None,
                None,
                None,
                Some("2020-01-01"),
                &[],
            ),
        ]);
        let chip = chips_for(&m, "model.pkg.a", Some(today_2026()))
            .into_iter()
            .find(|c| c.kind == "ref-to-deprecated")
            .expect("ref-to-deprecated chip fires");
        assert_eq!(chip.label, "Refs deprecated dep (deprecated 2020-01-01)");
        assert_eq!(chip.severity, Some("danger".to_owned()));
    }

    #[test]
    fn ref_to_scheduled_deprecation_is_info() {
        // Far-future date ⇒ always scheduled ⇒ info (stable golden).
        let m = manifest_with_nodes(vec![
            gov_model(
                "model.pkg.a",
                None,
                None,
                None,
                None,
                None,
                &["model.pkg.dep"],
            ),
            gov_model(
                "model.pkg.dep",
                None,
                None,
                None,
                None,
                Some("2099-01-01"),
                &[],
            ),
        ]);
        let chip = chips_for(&m, "model.pkg.a", Some(far_past()))
            .into_iter()
            .find(|c| c.kind == "ref-to-deprecated")
            .expect("chip fires");
        assert_eq!(chip.severity, Some("info".to_owned()));
    }

    #[test]
    fn ref_to_deprecation_on_today_is_danger() {
        // today-or-past ⇒ elapsed/danger (the boundary is inclusive of
        // today).
        let m = manifest_with_nodes(vec![
            gov_model(
                "model.pkg.a",
                None,
                None,
                None,
                None,
                None,
                &["model.pkg.dep"],
            ),
            gov_model(
                "model.pkg.dep",
                None,
                None,
                None,
                None,
                Some("2026-06-13"),
                &[],
            ),
        ]);
        let chip = chips_for(&m, "model.pkg.a", Some(today_2026()))
            .into_iter()
            .find(|c| c.kind == "ref-to-deprecated")
            .expect("chip fires");
        assert_eq!(chip.severity, Some("danger".to_owned()));
    }

    #[test]
    fn ref_to_deprecation_with_unknown_today_fires_without_severity() {
        let m = manifest_with_nodes(vec![
            gov_model(
                "model.pkg.a",
                None,
                None,
                None,
                None,
                None,
                &["model.pkg.dep"],
            ),
            gov_model(
                "model.pkg.dep",
                None,
                None,
                None,
                None,
                Some("2020-01-01"),
                &[],
            ),
        ]);
        let chip = chips_for(&m, "model.pkg.a", None)
            .into_iter()
            .find(|c| c.kind == "ref-to-deprecated")
            .expect("chip fires even with unknown today");
        assert_eq!(chip.severity, None);
    }

    #[test]
    fn ref_to_malformed_deprecation_date_fires_no_chip() {
        let m = manifest_with_nodes(vec![
            gov_model(
                "model.pkg.a",
                None,
                None,
                None,
                None,
                None,
                &["model.pkg.dep"],
            ),
            gov_model(
                "model.pkg.dep",
                None,
                None,
                None,
                None,
                Some("not-a-date"),
                &[],
            ),
        ]);
        assert!(
            !chips_for(&m, "model.pkg.a", Some(far_future()))
                .iter()
                .any(|c| c.kind == "ref-to-deprecated"),
            "a malformed date is no panic + no chip",
        );
    }

    #[test]
    fn ref_to_non_deprecated_model_fires_no_chip() {
        let m = manifest_with_nodes(vec![
            gov_model(
                "model.pkg.a",
                None,
                None,
                None,
                None,
                None,
                &["model.pkg.b"],
            ),
            gov_model("model.pkg.b", None, None, None, None, None, &[]),
        ]);
        assert!(
            !chips_for(&m, "model.pkg.a", Some(today_2026()))
                .iter()
                .any(|c| c.kind == "ref-to-deprecated"),
        );
    }

    #[test]
    fn refs_to_missing_or_non_model_targets_are_skipped() {
        // A ref to a node absent from the manifest, and a ref to a test
        // node (non-model), both skip cleanly — no chip, no panic.
        let test_node = Node::new(
            NodeId::new("test.pkg.t"),
            "test",
            sample_checksum(),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            None,
            Some(NodeId::new("model.pkg.a")),
            Some(TestMetadata::new("not_null", None, serde_json::json!({}))),
        );
        let m = manifest_with_nodes(vec![
            gov_model(
                "model.pkg.a",
                None,
                None,
                None,
                None,
                None,
                &["model.pkg.absent", "test.pkg.t"],
            ),
            test_node,
        ]);
        assert!(
            chips_for(&m, "model.pkg.a", Some(today_2026())).is_empty(),
            "missing + non-model refs contribute no chips",
        );
    }

    // ---- access-violation ----

    #[test]
    fn private_ref_across_group_boundary_fires() {
        // a (group finance) refs b (private, group marketing) ⇒ violation.
        let m = manifest_with_nodes(vec![
            gov_model(
                "model.pkg.a",
                None,
                Some("finance"),
                None,
                None,
                None,
                &["model.pkg.b"],
            ),
            gov_model(
                "model.pkg.b",
                Some("private"),
                Some("marketing"),
                None,
                None,
                None,
                &[],
            ),
        ]);
        let chip = chips_for(&m, "model.pkg.a", None)
            .into_iter()
            .find(|c| c.kind == "access-violation")
            .expect("access-violation chip fires");
        assert_eq!(chip.label, "Private b referenced across group boundary");
    }

    #[test]
    fn private_ref_within_same_group_does_not_fire() {
        // Both in `finance` — a private model ref'd within its own group is
        // in-bounds.
        let m = manifest_with_nodes(vec![
            gov_model(
                "model.pkg.a",
                None,
                Some("finance"),
                None,
                None,
                None,
                &["model.pkg.b"],
            ),
            gov_model(
                "model.pkg.b",
                Some("private"),
                Some("finance"),
                None,
                None,
                None,
                &[],
            ),
        ]);
        assert!(
            !chips_for(&m, "model.pkg.a", None)
                .iter()
                .any(|c| c.kind == "access-violation"),
        );
    }

    #[test]
    fn public_ref_never_fires_access_violation() {
        let m = manifest_with_nodes(vec![
            gov_model(
                "model.pkg.a",
                None,
                Some("finance"),
                None,
                None,
                None,
                &["model.pkg.b"],
            ),
            gov_model(
                "model.pkg.b",
                Some("public"),
                Some("marketing"),
                None,
                None,
                None,
                &[],
            ),
        ]);
        assert!(
            !chips_for(&m, "model.pkg.a", None)
                .iter()
                .any(|c| c.kind == "access-violation"),
        );
    }

    #[test]
    fn protected_target_never_fires_access_violation() {
        // The default (protected) target across a group boundary is NOT a
        // violation — only `private` is.
        let m = manifest_with_nodes(vec![
            gov_model(
                "model.pkg.a",
                None,
                Some("finance"),
                None,
                None,
                None,
                &["model.pkg.b"],
            ),
            gov_model(
                "model.pkg.b",
                None,
                Some("marketing"),
                None,
                None,
                None,
                &[],
            ),
        ]);
        assert!(
            !chips_for(&m, "model.pkg.a", None)
                .iter()
                .any(|c| c.kind == "access-violation"),
        );
    }

    #[test]
    fn lifecycle_chips_make_the_payload_non_empty() {
        let m = manifest_with(
            vec![gov_model(
                "model.pkg.m",
                None,
                Some("finance"),
                None,
                None,
                None,
                &[],
            )],
            vec![Group::new(
                "finance",
                Some(Owner::new(None, vec!["fin@corp.example".to_owned()])),
            )],
        );
        let facts = gather_governance(&m, &in_scope(&["model.pkg.m"]), None, None);
        assert!(!facts.lifecycle_chips.is_empty());
        assert!(facts.has_content());
        assert!(!facts.is_empty());
    }

    // ===== cute-dbt#348: config-driven meta + tags =====

    /// A model node carrying a `config` dict (`meta`/`tags` ride here) and
    /// optional per-column meta/tags. `col_meta_tags` is
    /// `(column, tags, meta_json)` triples — `meta_json` is `None` for no
    /// column meta.
    fn meta_model(
        full_id: &str,
        config: serde_json::Value,
        col_meta_tags: &[(&str, &[&str], Option<serde_json::Value>)],
    ) -> Node {
        let config_map: BTreeMap<String, serde_json::Value> = match config {
            serde_json::Value::Object(map) => map.into_iter().collect(),
            _ => BTreeMap::new(),
        };
        let columns: BTreeMap<String, Option<String>> = col_meta_tags
            .iter()
            .map(|(name, _, _)| ((*name).to_owned(), None))
            .collect();
        let column_facts: BTreeMap<String, ColumnFacts> = col_meta_tags
            .iter()
            .map(|(name, tags, meta)| {
                (
                    (*name).to_owned(),
                    ColumnFacts::new(
                        meta.clone(),
                        tags.iter().map(|t| (*t).to_owned()).collect(),
                        Vec::new(),
                        Vec::new(),
                    ),
                )
            })
            .collect();
        Node::new(
            NodeId::new(full_id),
            "model",
            sample_checksum(),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config_map, false),
            None,
            columns,
        )
        .with_column_facts(column_facts)
    }

    fn meta_pair(key: &str, value: &str) -> MetaPair {
        MetaPair {
            key: key.to_owned(),
            value: value.to_owned(),
        }
    }

    #[test]
    fn model_without_meta_or_tags_yields_no_block() {
        let m = manifest_with(
            vec![meta_model("model.pkg.m", serde_json::json!({}), &[])],
            vec![],
        );
        let facts = gather_governance(&m, &in_scope(&["model.pkg.m"]), None, None);
        assert!(facts.meta_tags.is_empty(), "no meta/tags ⇒ no block");
        assert!(!facts.has_content());
    }

    #[test]
    fn model_tags_become_dedup_sorted_chips() {
        // dbt re-emits inherited tags, so the wire list carries duplicates
        // (and unsorted) — the block dedups + sorts.
        let m = manifest_with(
            vec![meta_model(
                "model.pkg.m",
                serde_json::json!({
                    "tags": ["marts", "analytics", "dimension", "marts", "analytics"]
                }),
                &[],
            )],
            vec![],
        );
        let facts = gather_governance(&m, &in_scope(&["model.pkg.m"]), None, None);
        assert_eq!(facts.meta_tags.len(), 1);
        assert_eq!(
            facts.meta_tags[0].tags,
            vec![
                "analytics".to_owned(),
                "dimension".to_owned(),
                "marts".to_owned()
            ],
        );
        assert!(facts.meta_tags[0].meta.is_empty());
        assert!(facts.has_content());
    }

    #[test]
    fn a_lone_string_tag_is_accepted() {
        // dbt also allows `tags: a_single_tag` (scalar, not array).
        let m = manifest_with(
            vec![meta_model(
                "model.pkg.m",
                serde_json::json!({ "tags": "hourly" }),
                &[],
            )],
            vec![],
        );
        let facts = gather_governance(&m, &in_scope(&["model.pkg.m"]), None, None);
        assert_eq!(facts.meta_tags[0].tags, vec!["hourly".to_owned()]);
    }

    #[test]
    fn model_meta_flattens_to_key_sorted_pairs() {
        let m = manifest_with(
            vec![meta_model(
                "model.pkg.m",
                serde_json::json!({
                    "meta": {
                        "owner": "clinical-quality",
                        "contains_pii": false,
                        "row_count": 42
                    }
                }),
                &[],
            )],
            vec![],
        );
        let facts = gather_governance(&m, &in_scope(&["model.pkg.m"]), None, None);
        assert_eq!(
            facts.meta_tags[0].meta,
            vec![
                meta_pair("contains_pii", "false"),
                meta_pair("owner", "clinical-quality"),
                meta_pair("row_count", "42"),
            ],
        );
        assert!(facts.meta_tags[0].tags.is_empty());
    }

    #[test]
    fn nested_meta_value_renders_as_compact_json() {
        // A nested dict/array value is compact-JSON rendered; a bare string
        // stays unquoted. Both are auto-escaped as text at render.
        let m = manifest_with(
            vec![meta_model(
                "model.pkg.m",
                serde_json::json!({
                    "meta": {
                        "labels": ["a", "b"],
                        "sla": { "freshness_hours": 6 },
                        "note": "ships nightly"
                    }
                }),
                &[],
            )],
            vec![],
        );
        let facts = gather_governance(&m, &in_scope(&["model.pkg.m"]), None, None);
        assert_eq!(
            facts.meta_tags[0].meta,
            vec![
                meta_pair("labels", r#"["a","b"]"#),
                meta_pair("note", "ships nightly"),
                meta_pair("sla", r#"{"freshness_hours":6}"#),
            ],
        );
    }

    #[test]
    fn column_meta_and_tags_ride_the_columns_list() {
        let m = manifest_with(
            vec![meta_model(
                "model.pkg.m",
                serde_json::json!({}),
                &[
                    (
                        "payer_key",
                        &["dimension_key", "dimension_key"][..],
                        Some(serde_json::json!({ "pii": false, "owner": "cq" })),
                    ),
                    ("plain_col", &[][..], None),
                ],
            )],
            vec![],
        );
        let facts = gather_governance(&m, &in_scope(&["model.pkg.m"]), None, None);
        // Block exists purely on the strength of column-level meta/tags.
        assert_eq!(facts.meta_tags.len(), 1);
        assert!(facts.meta_tags[0].tags.is_empty());
        assert!(facts.meta_tags[0].meta.is_empty());
        // Only the column carrying meta/tags appears; `plain_col` is filtered.
        assert_eq!(facts.meta_tags[0].columns.len(), 1);
        let col = &facts.meta_tags[0].columns[0];
        assert_eq!(col.column, "payer_key");
        assert_eq!(col.tags, vec!["dimension_key".to_owned()]);
        assert_eq!(
            col.meta,
            vec![meta_pair("owner", "cq"), meta_pair("pii", "false")],
        );
        assert!(facts.has_content());
    }

    #[test]
    fn out_of_scope_model_contributes_no_block() {
        let m = manifest_with(
            vec![meta_model(
                "model.pkg.m",
                serde_json::json!({ "tags": ["analytics"] }),
                &[],
            )],
            vec![],
        );
        // Empty in-scope set ⇒ no block, no content.
        let facts = gather_governance(&m, &in_scope(&[]), None, None);
        assert!(facts.meta_tags.is_empty());
        assert!(!facts.has_content());
    }

    #[test]
    fn meta_tags_block_makes_the_payload_non_empty() {
        let m = manifest_with(
            vec![meta_model(
                "model.pkg.m",
                serde_json::json!({ "tags": ["analytics"] }),
                &[],
            )],
            vec![],
        );
        let facts = gather_governance(&m, &in_scope(&["model.pkg.m"]), None, None);
        assert!(facts.group_chips.is_empty());
        assert!(facts.lifecycle_chips.is_empty());
        assert!(!facts.meta_tags.is_empty());
        assert!(facts.has_content());
        assert!(!facts.is_empty());
    }
}
