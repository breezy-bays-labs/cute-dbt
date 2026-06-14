//! The `cute-dbt explore` two-page renderer (cute-dbt#100, cute-dbt#101).
//!
//! Emits the full-manifest explorer into `--out-dir`:
//!
//! - **`dag.html`** — the **interactive** lineage DAG (cute-dbt#101):
//!   every `model` node plus — typed, since cute-dbt#253 — every
//!   snapshot / seed / source / exposure, edges from `depends_on.nodes`,
//!   rendered by the vendored Cytoscape UMD core + the cytoscape-dagre
//!   layout extension (left-to-right ranks) and driven by the
//!   first-party explore lineage engine
//!   (`templates/explore-lineage.js`). The server embeds the
//!   [`LineagePayload`] JSON carrier (nodes + **forward** dependency
//!   edges only — the client traverses both directions); the engine
//!   adds pan/zoom/drag, hand-rolled fuzzy search, click /
//!   search-select **highlight** (emphasize the node + its full
//!   transitive lineage, dim the complement) and the deliberate
//!   **Space** focus commit (center + write
//!   `document.body.dataset.selectedModel` — the only interaction that
//!   writes the external-drive signal). This replaced the V1 static
//!   Mermaid lineage (the epic #99 conscious throwaway).
//! - **`tests.html`** — the unit-test index: one section per model with
//!   its unit tests, plus the full engine-agnostic
//!   [`ReportPayload`] embedded
//!   as the `cute-dbt-data` JSON carrier (the same `build_payload`
//!   output the report renders — the verified reuse seam). The page
//!   carries **no** Mermaid and no `DataTables`; it is a server-rendered
//!   static page, so the headless liveness oracle for it is page-aware
//!   (DOM facts, never the report's Mermaid/DataTables probes).
//!
//! Fail-open contract: an uncompiled model (`compiled_code: null`)
//! renders as a **"not compiled"** node/badge on both pages — explore
//! never raises Stage-2 `NotCompiled`, and `PreflightError` keeps its
//! four variants.
//!
//! Both pages hold the zero-egress invariant independently: every asset
//! is embedded from [`asset_embed`](crate::adapters::asset_embed)
//! `.rodata` constants; the favicon is a `data:` URI; the only hrefs
//! are same-directory navigation anchors (`dag.html` ⇄ `tests.html`),
//! which load nothing until clicked and resolve over `file://`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::Path;

use askama::Template;
use serde::Serialize;

use crate::adapters::asset_embed::{
    APPEARANCE_JS, CYTOSCAPE_DAGRE_JS, CYTOSCAPE_JS, EXPLORE_CTE_JS, EXPLORE_LINEAGE_JS,
    EXPLORE_TESTS_JS, FAVICON_DATA_URI, SAKURA_CSS,
};
use crate::adapters::render::{DagPayload, ReportPayload};
use crate::domain::{
    GrainKind, MacroFocusSet, Manifest, ModelInScopeSet, Node, NodeId, SourceNode,
    model_grain_signals, resolve_tested_model,
};

/// The explorer's external-drive contract version (cute-dbt#105).
///
/// One readable string covering the page's whole host-facing surface:
///
/// - the two forward hooks (`window.focusModel(id)` /
///   `window.setView(kind)`),
/// - the dual-bound commit (the Space-only `data-selected-model` DOM
///   attribute AND the host-bridge `postMessage` commit event:
///   `{ type: "cute-dbt/commit", contractVersion, modelId, view,
///   paths }`),
/// - the [`NodePathsPayload`] shape carried per lineage node.
///
/// Server-rendered as the `data-cute-dbt-contract` attribute on
/// `dag.html`'s `<body>` (readable by attribute-only observers without
/// executing JS) and mirrored by the in-page `window.cuteDbtContract`
/// global, which reads the attribute back — the attribute is the single
/// source, so the two surfaces cannot drift. Bumps ONLY on a breaking
/// change to the named surface, governed by the release-discipline
/// CLI-surface `SemVer` policy (a bump is a v0.x minor / v1.0+ major
/// event) — no separate versioning system.
pub const EXPLORE_CONTRACT_VERSION: &str = "1";

/// Render-layer lineage node typing (cute-dbt#253) — the wire
/// vocabulary for dag.html's typed DAG nodes.
///
/// Mirrors the manifest's own partition @ dbt-fusion `9977b6cb…`:
/// `model` / `snapshot` / `seed` are `nodes`-map resource types (the
/// serde tag on fusion's `DbtNode` enum, `dbt-schemas`
/// `manifest/manifest.rs:52-64`); `source` entries live in the
/// top-level `sources` map (`ManifestSource`) and `exposure` entries in
/// the top-level `exposures` map (`ManifestExposure`). Serialized
/// `snake_case` into the [`LineageNodePayload`] — the exhaustive
/// [`Self::wire_key`] match is the compile-time half of the node-vocab
/// completeness guard (the `edge_type_wire_key` precedent); the
/// template-grep test below is the belt-and-braces half.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageNodeType {
    /// A `model` node — the only type the pre-#253 lineage rendered.
    Model,
    /// A `snapshot` node (`nodes` map) — mid-graph: it both depends on
    /// upstream nodes and feeds downstream models.
    Snapshot,
    /// A `seed` node (`nodes` map) — a root: seeds carry no
    /// `depends_on.nodes` (fusion serializes the key absent).
    Seed,
    /// A `sources`-map entry — a root: sources declare no dependencies.
    Source,
    /// An `exposures`-map entry — a sink: the lineage terminus
    /// (cute-dbt#253 folds exposures in alongside the AC types).
    Exposure,
}

impl LineageNodeType {
    /// Every variant — the iteration source for the completeness guard.
    pub const ALL: [Self; 5] = [
        Self::Model,
        Self::Snapshot,
        Self::Seed,
        Self::Source,
        Self::Exposure,
    ];

    /// Snake-case wire key — the exact serde string
    /// (`rename_all = "snake_case"`). Exhaustive: a new variant fails to
    /// compile here before it can ship untyped to the client engine.
    #[must_use]
    pub fn wire_key(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Snapshot => "snapshot",
            Self::Seed => "seed",
            Self::Source => "source",
            Self::Exposure => "exposure",
        }
    }
}

/// The focused-macro-DAG role of a lineage node (cute-dbt#345 Slice 3) —
/// the render-layer twin of the domain
/// [`MacroFocusSet`] partition.
///
/// Carried (serde-skip-gated) on [`LineageNodePayload::macro_role`] only
/// on the `macro.html` carrier: a focused payload stamps each node
/// `User` (a macro-calling model — emphasized on the page) or
/// `Downstream` (a node in the `ref()`-downstream closure of the callers
/// — dimmed-as-context). The full-manifest `dag.html` payload leaves it
/// `None`, so the existing golden carries no `macro_role` keys at all
/// (the `changed`/`is_false` serde-skip precedent — golden byte-stability,
/// R9). Serializes `"user"` / `"downstream"` for the engine's per-role
/// class assignment at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacroRole {
    /// A macro-calling root-project model
    /// ([`macro_blast_radius`](crate::domain::macro_blast_radius)) —
    /// emphasized.
    User,
    /// A node in the `ref()`-downstream closure of the callers, minus the
    /// callers themselves — dimmed-as-context.
    Downstream,
}

/// One node in the lineage graph (typed since cute-dbt#253).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageNode {
    /// Full manifest node id (`model.<package>.<name>`,
    /// `snapshot.<package>.<name>`, `seed.<package>.<name>`,
    /// `source.<package>.<source_name>.<name>`,
    /// `exposure.<package>.<name>`).
    pub id: String,
    /// Rendered label: the bare name (last dotted segment) for
    /// `nodes`-map types and exposures; `source_name.table` for sources
    /// (the two `source(...)` arguments — a bare table name could
    /// collide with a model name in search).
    pub name: String,
    /// The render-layer node type (cute-dbt#253).
    pub node_type: LineageNodeType,
    /// `true` when the manifest carries `compiled_code: null` for this
    /// model (`dbt parse`) — rendered as a "not compiled" node, never
    /// raised (the cute-dbt#100 fail-open contract).
    pub not_compiled: bool,
    /// YAML data-tests attached to this model (cute-dbt#103) — manifest
    /// `test` nodes whose `attached_node` is this model (fusion's
    /// `_lookup_attached_node` parity; see the private
    /// `data_test_counts` helper below).
    pub data_tests: usize,
    /// Unit tests targeting this model (cute-dbt#103) — manifest
    /// `unit_tests` entries whose target resolves here
    /// ([`resolve_tested_model`], the same bridge the report uses).
    pub unit_tests: usize,
}

/// Count the YAML data-tests per target model: manifest `test` nodes
/// keyed by their `attached_node` (cute-dbt#103).
///
/// `attached_node` is the authoritative data-test → target-model
/// linkage — fusion mirrors dbt-core's `_lookup_attached_node`
/// (`dbt-parser/src/resolve/resolve_tests/resolve_data_tests.rs`,
/// `9977b6cb…`): the attached node is the parent the test is declared
/// ON, independent of which YAML file declares it; a relationships
/// test's `to:` target rides `depends_on.nodes` but is **not** the
/// attached node, so attribution by `depends_on` would double-count.
/// Singular (SQL-file) tests carry `attached_node: null` on real fusion
/// manifests (the null-fill shape, verified on the committed playground
/// fixture) and deliberately count toward no model — the badge counts
/// **YAML** data-tests. Keys may name non-model parents (seeds,
/// snapshots); lineage nodes only ever look model ids up, so those
/// entries are inert.
fn data_test_counts(current: &Manifest) -> HashMap<&NodeId, usize> {
    let mut counts: HashMap<&NodeId, usize> = HashMap::new();
    for node in current.nodes().values() {
        if node.resource_type() != "test" {
            continue;
        }
        if let Some(target) = node.attached_node() {
            *counts.entry(target).or_insert(0) += 1;
        }
    }
    counts
}

/// Count the unit tests per target model (cute-dbt#103): each manifest
/// `unit_tests` entry is bridged to its node by [`resolve_tested_model`]
/// (the engine-resolved id when present, the bare `model:` name
/// otherwise — the report renderer's exact resolution, so the two
/// surfaces cannot disagree on a test's target). An unresolvable
/// `model:` reference contributes nothing (skipped, not failed — the
/// explore fail-open posture).
fn unit_test_counts(current: &Manifest) -> HashMap<NodeId, usize> {
    let mut counts: HashMap<NodeId, usize> = HashMap::new();
    for unit_test in current.unit_tests().values() {
        if let Some(model) = resolve_tested_model(current, unit_test) {
            *counts.entry(model.id().clone()).or_insert(0) += 1;
        }
    }
    counts
}

/// The full-manifest lineage: typed nodes in deterministic full-id
/// order, edges as `(from_index, to_index)` pairs pointing **upstream →
/// downstream** (a node depends on its `from`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lineage {
    /// Every lineage node (models + snapshots + seeds + sources +
    /// exposures since cute-dbt#253), ordered by full node id.
    pub nodes: Vec<LineageNode>,
    /// Dependency edges between entries of `nodes` (indices), ordered.
    pub edges: Vec<(usize, usize)>,
}

/// The rendered label for one typed lineage node (cute-dbt#253):
/// `source_name.table` for sources, the entry's own `name` for
/// exposures, the id's leaf segment otherwise.
fn lineage_node_name(current: &Manifest, id: &NodeId, node_type: LineageNodeType) -> String {
    match node_type {
        LineageNodeType::Source => current.sources().get(id).map_or_else(
            || leaf_segment(id.as_str()).to_owned(),
            |s| format!("{}.{}", s.source_name(), s.name()),
        ),
        LineageNodeType::Exposure => current.exposures().get(id).map_or_else(
            || leaf_segment(id.as_str()).to_owned(),
            |e| e.name().to_owned(),
        ),
        _ => leaf_segment(id.as_str()).to_owned(),
    }
}

/// Build the lineage graph for the explore scope.
///
/// Nodes are the `models` set (the
/// [`all_models`](crate::domain::all_models) seam) **unioned with**
/// (cute-dbt#253) every `snapshot`/`seed` node in the manifest's
/// `nodes` map, every `sources`-map entry and every `exposures`-map
/// entry — the typed-node fix for the severed `stg → snapshot →
/// downstream` chain (a filtered-out snapshot split the graph and faked
/// the downstream model as a root; absent seeds/sources faked further
/// roots). The union iterates in deterministic full-id order
/// (`BTreeMap`).
///
/// Edges come from each `nodes`-map entry's `depends_on.nodes` and each
/// exposure's `depends_on.nodes`, filtered to ids inside the union —
/// macros, cross-project refs and other resource types (`test`,
/// `operation`, `analysis`, `function` — the remaining fusion `DbtNode`
/// serde tags @ `9977b6cb…`) are silently skipped. Sources contribute
/// no outgoing dependencies (roots by construction). Self-edges are
/// skipped defensively (a manifest should never carry one).
#[must_use]
pub fn build_lineage(current: &Manifest, models: &ModelInScopeSet) -> Lineage {
    // The full-manifest typed union (models + every snapshot/seed/source/
    // exposure); the focused macro DAG restricts this via
    // [`focused_typed_node_map`]. Both feed the shared assembly core.
    let typed = build_typed_node_map(current, models);
    lineage_from_typed_map(current, &typed)
}

/// Build the typed node union for the lineage graph, ordered by full node
/// id (`BTreeMap`): the in-scope `models`, every `snapshot`/`seed` node,
/// every source, and every exposure (cute-dbt#253). `or_insert` keeps the
/// model type when an id appears in more than one set.
fn build_typed_node_map<'a>(
    current: &'a Manifest,
    models: &'a ModelInScopeSet,
) -> BTreeMap<&'a NodeId, LineageNodeType> {
    let mut typed: BTreeMap<&NodeId, LineageNodeType> = models
        .iter()
        .map(|id| (id, LineageNodeType::Model))
        .collect();
    for (id, node) in current.nodes() {
        let node_type = match node.resource_type() {
            "snapshot" => LineageNodeType::Snapshot,
            "seed" => LineageNodeType::Seed,
            _ => continue,
        };
        typed.entry(id).or_insert(node_type);
    }
    for id in current.sources().keys() {
        typed.entry(id).or_insert(LineageNodeType::Source);
    }
    for id in current.exposures().keys() {
        typed.entry(id).or_insert(LineageNodeType::Exposure);
    }
    typed
}

/// Build the deduplicated, sorted `(from_idx, to_idx)` edge list from each
/// node's `depends_on.nodes`, keeping only edges between union members and
/// dropping defensive self-edges. Sources contribute no outgoing deps.
fn lineage_edges(
    current: &Manifest,
    typed: &BTreeMap<&NodeId, LineageNodeType>,
    index_of: &HashMap<&NodeId, usize>,
) -> Vec<(usize, usize)> {
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (to_idx, (id, node_type)) in typed.iter().enumerate() {
        let deps: &[NodeId] = match node_type {
            LineageNodeType::Model | LineageNodeType::Snapshot | LineageNodeType::Seed => {
                current.node(id).map_or(&[], |n| n.depends_on().nodes())
            }
            LineageNodeType::Exposure => current
                .exposures()
                .get(*id)
                .map_or(&[], |e| e.depends_on().nodes()),
            LineageNodeType::Source => &[],
        };
        for dep in deps {
            if let Some(&from_idx) = index_of.get(dep)
                && from_idx != to_idx
            {
                edges.push((from_idx, to_idx));
            }
        }
    }
    edges.sort_unstable();
    edges.dedup();
    edges
}

/// One node in the serialized lineage payload (the `explore-dag-data`
/// JSON carrier consumed by `templates/explore-lineage.js`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineageNodePayload {
    /// Full manifest node id (`model.<package>.<name>`) — the Cytoscape
    /// element id and the value the Space focus commit writes to
    /// `document.body.dataset.selectedModel`.
    pub id: String,
    /// Rendered label — the canvas-text label and the fuzzy-search
    /// candidate (`source_name.table` for sources, the bare name
    /// otherwise).
    pub name: String,
    /// The render-layer node type (cute-dbt#253) — the engine's
    /// `node[type = "…"]` style/shape hook and the legend vocabulary.
    /// Always serialized (the explicit posture; `"model"` included).
    pub node_type: LineageNodeType,
    /// The fail-open "not compiled" flag (cute-dbt#100) — rendered as a
    /// dashed node, never raised. Honest per type (cute-dbt#253):
    /// consulted only for SQL-bearing types (`model`, `snapshot` — both
    /// engines backfill snapshot `compiled_code` on compile; fusion
    /// null-fills it at parse, `dbt-tasks-sa/src/utils.rs:151-172` vs
    /// `dbt-schemas/src/schemas/manifest/manifest_nodes.rs:616-617` @
    /// `9977b6cb…`). Seeds are NEVER flagged: fusion null-fills seed
    /// `compiled_code` unconditionally (`manifest_nodes.rs:232-233`) —
    /// a seed has no SQL to compile, so the flag would be noise, not
    /// honesty. Sources/exposures have no code at all.
    pub not_compiled: bool,
    /// YAML data-tests attached to this model (cute-dbt#103). Always
    /// serialized — the 0/0 badge is explicit, never an omitted key.
    pub data_tests: usize,
    /// Unit tests targeting this model (cute-dbt#103). Always
    /// serialized, same contract as `data_tests`.
    pub unit_tests: usize,
    /// The pre-formatted badge line (`"2 data-tests · 1 unit-test"`,
    /// including the explicit `"0 data-tests · 0 unit-tests"`) —
    /// composed in Rust so the lineage engine stays a pure renderer
    /// (the cute-dbt#138 posture). These are TEST-COUNT facts straight
    /// off the manifest — never check-engine (coverage-intelligence)
    /// output, so no display toggle gates them.
    pub badge: String,
    /// The model-detail facts (cute-dbt#104) — the highlight card's and
    /// the hover tooltip's data, all manifest-derived and pre-rendered
    /// in Rust (the engine stays a pure renderer).
    pub detail: ModelDetailPayload,
    /// Per-node file paths (cute-dbt#105) — the external-drive
    /// contract's "open the file" surface. Always serialized (absence
    /// is `null`/`[]`, never an omitted key — the explicit-0/0
    /// posture).
    pub paths: NodePathsPayload,
    /// PR-diff **change context** (cute-dbt#106): `true` when the
    /// explore run carried `--pr-diff` and this model's source file
    /// appears in the diff (the cute-dbt#81 `original_file_path`
    /// matching via [`crate::domain::changed_models`], renames included
    /// per cute-dbt#80). Serialized **only when `true`** — a deliberate
    /// exception to the explicit-0/0 posture so the no-context payload
    /// stays byte-identical to the pre-#106 shape (the committed
    /// explore goldens render without `--pr-diff`). Context never
    /// narrows scope: every model is still a node; this flag only
    /// decorates.
    #[serde(skip_serializing_if = "is_false")]
    pub changed: bool,
    /// Focused-macro-DAG role (cute-dbt#345 Slice 3): `Some(User)` for a
    /// macro-calling model and `Some(Downstream)` for a node in their
    /// `ref()`-downstream closure — set ONLY on the `macro.html` focused
    /// carrier. `None` on the full-manifest `dag.html` payload, and
    /// serialized **only when `Some`** (the `changed` serde-skip
    /// precedent) so the committed `dag.html` golden carries no
    /// `macro_role` keys and stays byte-identical (R9).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macro_role: Option<MacroRole>,
}

/// `serde(skip_serializing_if)` predicate for
/// [`LineageNodePayload::changed`]: omit the key when `false`, so a
/// no-context lineage payload carries no `changed` keys at all
/// (cute-dbt#106).
#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes &bool
fn is_false(b: &bool) -> bool {
    !*b
}

/// Per-node file paths (cute-dbt#105) — everything a host needs to open
/// this model's files, straight off the manifest. All paths are
/// **project-relative** as dbt emits them (`original_file_path` /
/// `patch_path` are relative by design — never an absolute path, the
/// `root_path` leak class).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NodePathsPayload {
    /// The model's SQL source (`nodes.<id>.original_file_path`, e.g.
    /// `models/marts/core/dim_payers.sql`) — the cute-dbt#189
    /// precedent. `null` when the manifest omits it (synthetic /
    /// pre-1.8 inputs).
    pub sql: Option<String>,
    /// The schema-properties YAML that patches this model
    /// (`nodes.<id>.patch_path`, ingested with its `<package>://` URI
    /// scheme stripped). `null` for an unpatched model.
    pub schema_yaml: Option<String>,
    /// One entry per unit test targeting this model, name-ordered
    /// (deterministic render). Empty for an untested model.
    pub unit_tests: Vec<UnitTestPathsPayload>,
}

/// One unit test's file paths (cute-dbt#105).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnitTestPathsPayload {
    /// User-facing unit-test name.
    pub name: String,
    /// The declaring `.yml` file (`unit_tests.<id>.original_file_path`
    /// — the unit-test node's OWN top-level path field, the cute-dbt#69
    /// plumbing; fusion serializes no `patch_path` on unit-test entries,
    /// verified on the committed playground fixture). `null` when the
    /// manifest omits it.
    pub yaml: Option<String>,
    /// External fixture file references (`given[i].fixture` in given
    /// order, then `expect.fixture` — the cute-dbt#126 plumbing),
    /// carried **verbatim** as the manifest emits them: fusion resolves
    /// them to project-relative paths (`tests/fixtures/<name>.csv`,
    /// verified on the committed playground fixture); dbt-core MAY emit
    /// a bare fixture name, which hosts resolve via the documented
    /// `tests/fixtures/<name>.csv` convention (the same fallback the
    /// report's external-fixture reader applies). Empty for
    /// inline-rows-only tests.
    pub fixtures: Vec<String>,
}

/// Per-model detail facts for the highlight card + hover tooltip
/// (cute-dbt#104). Every field is manifest-derived; every key is always
/// serialized (the explicit-0/0 posture — absence is `null`/`[]`, never
/// an omitted key).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ModelDetailPayload {
    /// Authored model description (cute-dbt#200 ingestion) — `null` for
    /// an undescribed model.
    pub description: Option<String>,
    /// `config.materialized` — `null` when the manifest omits it.
    pub materialized: Option<String>,
    /// Resolved model tags (the authoritative deduplicated top-level
    /// wire list — the cute-dbt#200 decision; `config.tags` carries
    /// merge duplicates on real dbt-core manifests and is not read).
    pub tags: Vec<String>,
    /// `config.meta` entries, key-ordered, values pre-rendered (strings
    /// verbatim, everything else compact JSON — the private
    /// `render_meta_value` helper).
    pub meta: Vec<MetaEntryPayload>,
    /// Declared columns (name-ordered): declared `data_type` and
    /// authored description, each `null` when absent.
    pub columns: Vec<ColumnDetailPayload>,
    /// The resolved grain + every detected signal (cute-dbt#104).
    pub grain: GrainPayload,
}

/// One `config.meta` entry — the value pre-rendered to a display string
/// in Rust (strings verbatim; everything else compact JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetaEntryPayload {
    /// Meta key.
    pub key: String,
    /// Pre-rendered value.
    pub value: String,
}

/// One declared column on the detail card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColumnDetailPayload {
    /// Column name.
    pub name: String,
    /// Declared `data_type`, when present.
    pub data_type: Option<String>,
    /// Authored description, when present (the cute-dbt#165
    /// non-empty-only ingestion).
    pub description: Option<String>,
}

/// The model's grain, resolved by the cute-dbt#104 precedence ladder
/// (explicit `config.meta.grain` → primary-key-class test →
/// compound-unique test → single `unique` test → explicit "unknown").
/// `detected` carries EVERY detected signal in precedence order — all
/// surfaced, never silently dropped; an unresolved grain is the
/// explicit `"unknown"`/`known: false` shape, never a guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrainPayload {
    /// The resolved grain value, or the literal `"unknown"`.
    pub value: String,
    /// Where the winning signal came from: `"config.meta.grain"`,
    /// `"primary-key test"`, `"compound-unique test"`, `"unique test"`,
    /// or the literal `"unknown"`.
    pub source: String,
    /// `false` ⇔ the explicit-unknown rung.
    pub known: bool,
    /// Every detected signal, precedence-ordered (the winner first).
    pub detected: Vec<GrainDetectedPayload>,
}

impl Default for GrainPayload {
    fn default() -> Self {
        Self {
            value: "unknown".to_owned(),
            source: "unknown".to_owned(),
            known: false,
            detected: Vec::new(),
        }
    }
}

/// One detected grain signal on the card's "all detected" surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrainDetectedPayload {
    /// The signal's precedence class, in the [`GrainPayload::source`]
    /// vocabulary.
    pub kind: String,
    /// Rendered grain value.
    pub value: String,
    /// `"config.meta.grain"` or the detecting test's node id.
    pub origin: String,
}

/// The display label for one grain precedence class (the carrier's
/// `source`/`kind` vocabulary).
fn grain_kind_label(kind: GrainKind) -> &'static str {
    match kind {
        GrainKind::Meta => "config.meta.grain",
        GrainKind::PrimaryKey => "primary-key test",
        GrainKind::CompoundUnique => "compound-unique test",
        GrainKind::Unique => "unique test",
    }
}

/// Resolve the grain carrier for one model via the domain ladder
/// ([`model_grain_signals`]). No signals ⇒ the explicit-unknown shape.
fn grain_payload(current: &Manifest, node: &Node) -> GrainPayload {
    let signals = model_grain_signals(current, node);
    let Some(winner) = signals.first() else {
        return GrainPayload::default();
    };
    GrainPayload {
        value: winner.value.clone(),
        source: grain_kind_label(winner.kind).to_owned(),
        known: true,
        detected: signals
            .iter()
            .map(|s| GrainDetectedPayload {
                kind: grain_kind_label(s.kind).to_owned(),
                value: s.value.clone(),
                origin: s.origin.clone(),
            })
            .collect(),
    }
}

/// Pre-render one `config.meta` value: strings verbatim, everything
/// else (numbers, bools, arrays, objects, null) as compact JSON — the
/// card surfaces the authored value, never interprets it.
fn render_meta_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Assemble the cute-dbt#104 detail facts for one model node. A missing
/// node (defensive — explore ids come from the manifest) yields the
/// empty detail with the explicit-unknown grain.
fn model_detail(current: &Manifest, node: Option<&Node>) -> ModelDetailPayload {
    let Some(node) = node else {
        return ModelDetailPayload::default();
    };
    let meta = node
        .config()
        .config()
        .get("meta")
        .and_then(serde_json::Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .map(|(key, value)| MetaEntryPayload {
                    key: key.clone(),
                    value: render_meta_value(value),
                })
                .collect()
        })
        .unwrap_or_default();
    let columns = node
        .columns()
        .iter()
        .map(|(name, data_type)| ColumnDetailPayload {
            name: name.clone(),
            data_type: data_type.clone(),
            description: node.column_descriptions().get(name).cloned(),
        })
        .collect();
    ModelDetailPayload {
        description: node.description().map(str::to_owned),
        materialized: node.config().materialized().map(str::to_owned),
        tags: node.tags().to_vec(),
        meta,
        columns,
        grain: grain_payload(current, node),
    }
}

/// Collect each model's unit-test file paths (cute-dbt#105), keyed by
/// resolved target model: each `unit_tests` entry is bridged by
/// [`resolve_tested_model`] (the [`unit_test_counts`]
/// twin — the badge count and the paths list cannot disagree on a
/// test's target). Entries are name-ordered per model (the manifest
/// `unit_tests` map iterates non-deterministically). An unresolvable
/// `model:` reference contributes nothing (the explore fail-open
/// posture).
fn unit_test_paths_by_model(current: &Manifest) -> HashMap<NodeId, Vec<UnitTestPathsPayload>> {
    let mut by_model: HashMap<NodeId, Vec<UnitTestPathsPayload>> = HashMap::new();
    for unit_test in current.unit_tests().values() {
        let Some(model) = resolve_tested_model(current, unit_test) else {
            continue;
        };
        // given-order fixture refs, then the expect's — verbatim off
        // the manifest (see [`UnitTestPathsPayload::fixtures`]).
        let mut fixtures: Vec<String> = unit_test
            .given()
            .iter()
            .filter_map(|g| g.fixture().map(str::to_owned))
            .collect();
        if let Some(f) = unit_test.expect().fixture() {
            fixtures.push(f.to_owned());
        }
        by_model
            .entry(model.id().clone())
            .or_default()
            .push(UnitTestPathsPayload {
                name: unit_test.name().to_owned(),
                yaml: unit_test.original_file_path().map(str::to_owned),
                fixtures,
            });
    }
    for tests in by_model.values_mut() {
        tests.sort_by(|a, b| a.name.cmp(&b.name));
    }
    by_model
}

/// Assemble the cute-dbt#105 per-node file paths for one model node.
fn node_paths(node: Option<&Node>, unit_tests: Vec<UnitTestPathsPayload>) -> NodePathsPayload {
    NodePathsPayload {
        sql: node.and_then(|n| n.original_file_path().map(str::to_owned)),
        schema_yaml: node.and_then(|n| n.patch_path().map(str::to_owned)),
        unit_tests,
    }
}

/// Assemble the detail facts for a `sources`-map node (cute-dbt#253):
/// the authored column descriptions are the only detail the ingested
/// [`SourceNode`] carries (cute-dbt#235); every other fact stays the
/// explicit default (the [`ModelDetailPayload`] absent shapes — `null`
/// / `[]` / the explicit-unknown grain).
fn source_detail(source: Option<&SourceNode>) -> ModelDetailPayload {
    let Some(source) = source else {
        return ModelDetailPayload::default();
    };
    ModelDetailPayload {
        columns: source
            .column_descriptions()
            .iter()
            .map(|(name, description)| ColumnDetailPayload {
                name: name.clone(),
                data_type: None,
                description: Some(description.clone()),
            })
            .collect(),
        ..Default::default()
    }
}

/// One dependency edge in the serialized lineage payload, by node id.
///
/// Edges are **forward only** — upstream (`from`) → downstream (`to`),
/// straight off `depends_on.nodes`. The client engine traverses both
/// directions (`predecessors()` / `successors()`) for the highlight, so
/// the payload never carries a reverse edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineageEdgePayload {
    /// The upstream model's node id (the dependency).
    pub from: String,
    /// The downstream model's node id (the dependent).
    pub to: String,
}

/// The `explore-dag-data` JSON carrier embedded in `dag.html` —
/// nodes = the typed union (models + snapshots + seeds + sources +
/// exposures, cute-dbt#253), edges = forward dependency edges. An empty
/// `nodes` array selects the page's empty-state message instead of a
/// Cytoscape render.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LineagePayload {
    /// Every typed lineage node, in deterministic full-id order.
    pub nodes: Vec<LineageNodePayload>,
    /// Forward dependency edges between entries of `nodes`, ordered.
    pub edges: Vec<LineageEdgePayload>,
    /// Per-model CTE DAGs (cute-dbt#102) — the CTE ⇄ model view
    /// toggle's data, keyed by full model node id. Each entry is the
    /// SAME [`DagPayload`] the report renders for that model (the
    /// `build_payload` reuse seam): engine-extracted CTE nodes with
    /// render-classified roles plus join-typed edges. Models with an
    /// empty graph carry **no** entry — the client renders the
    /// "no CTE DAG" sparse state for a compiled CTE-less model and the
    /// labeled fail-open degraded view for a `not_compiled` one.
    pub cte_dags: BTreeMap<String, DagPayload>,
}

/// Build the serializable lineage payload for `dag.html` (cute-dbt#101).
///
/// Composes [`build_lineage`] (nodes = the typed union of the model set
/// with the manifest's snapshots / seeds / sources / exposures since
/// cute-dbt#253; edges = `depends_on.nodes` filtered to the union,
/// **forward only**) into the id-keyed POD the Cytoscape engine
/// consumes. Pure assembly over owned manifest data — no I/O.
///
/// `changed` is the optional PR-diff **change context** (cute-dbt#106):
/// `Some(set)` marks each member node `changed: true` (the set comes
/// from [`crate::domain::changed_models`]); `None` — the no-`--pr-diff`
/// path — marks nothing, and the serialized payload carries no
/// `changed` keys at all (byte-identical to the pre-#106 shape).
/// Context never narrows scope: the node set is `models` either way.
#[must_use]
pub fn build_lineage_payload(
    current: &Manifest,
    models: &ModelInScopeSet,
    changed: Option<&ModelInScopeSet>,
) -> LineagePayload {
    let lineage = build_lineage(current, models);
    // Full-manifest lineage carries no macro role — every node maps to
    // `None`, so the serde-skip gate keeps the dag.html golden byte-stable.
    lineage_payload_from(
        current,
        lineage,
        |id| changed.is_some_and(|set| set.contains(id)),
        |_| None,
    )
}

/// Assemble a [`LineagePayload`] from a built [`Lineage`], resolving each
/// node's `changed` flag and `macro_role` via the supplied closures.
///
/// The shared core of [`build_lineage_payload`] (full manifest, no role)
/// and [`build_macro_lineage_payload`] (focused subgraph, role-stamped) —
/// the per-type detail/paths/badge assembly lives here once. Pure
/// assembly over owned manifest data; no I/O.
fn lineage_payload_from(
    current: &Manifest,
    lineage: Lineage,
    is_changed: impl Fn(&NodeId) -> bool,
    role_of: impl Fn(&NodeId) -> Option<MacroRole>,
) -> LineagePayload {
    let edges = lineage
        .edges
        .iter()
        .map(|&(from, to)| LineageEdgePayload {
            from: lineage.nodes[from].id.clone(),
            to: lineage.nodes[to].id.clone(),
        })
        .collect();
    let mut test_paths = unit_test_paths_by_model(current);
    let nodes = lineage
        .nodes
        .into_iter()
        .map(|n| {
            // Each lineage node carries its full manifest id — rebind it
            // for the per-type detail/paths assembly (the pre-#253 zip
            // against `models` no longer holds over the typed union).
            let id = NodeId::new(n.id.as_str());
            let (detail, paths) = match n.node_type {
                // `nodes`-map types share the model detail assembly —
                // snapshots/seeds carry the same authored description /
                // config / columns surfaces (fusion's shared
                // `ManifestNodeBaseAttributes` @ `9977b6cb…`).
                LineageNodeType::Model | LineageNodeType::Snapshot | LineageNodeType::Seed => (
                    model_detail(current, current.node(&id)),
                    node_paths(
                        current.node(&id),
                        test_paths.remove(&id).unwrap_or_default(),
                    ),
                ),
                LineageNodeType::Source => (
                    source_detail(current.sources().get(&id)),
                    NodePathsPayload::default(),
                ),
                LineageNodeType::Exposure => {
                    (ModelDetailPayload::default(), NodePathsPayload::default())
                }
            };
            LineageNodePayload {
                badge: typed_badge(n.node_type, n.data_tests, n.unit_tests),
                changed: is_changed(&id),
                macro_role: role_of(&id),
                id: n.id,
                name: n.name,
                node_type: n.node_type,
                not_compiled: n.not_compiled,
                data_tests: n.data_tests,
                unit_tests: n.unit_tests,
                detail,
                paths,
            }
        })
        .collect();
    LineagePayload {
        nodes,
        edges,
        cte_dags: BTreeMap::new(),
    }
}

/// Build the focused-macro lineage payload for `macro.html` (cute-dbt#345
/// Slice 3) — the [`MacroFocusSet`]
/// restricted to its own `users ∪ downstream` vertex set, with each node
/// stamped [`MacroRole`].
///
/// Restricts the typed-node union to the focus ids
/// (`focused_typed_node_map`) instead of the whole manifest — without
/// the restriction, the shared `build_typed_node_map` unconditionally
/// folds in EVERY snapshot/seed/source/exposure, flooding the focused
/// DAG. The `depends_on.nodes` edges then fall out via the existing
/// induced-subgraph filter (`lineage_edges` keeps only edges between
/// union members), so a focused payload is the same forward-edge POD over
/// a smaller vertex set. Roles come from `MacroFocusSet` membership:
/// `users` ⇒ [`MacroRole::User`], everything else (the closure) ⇒
/// [`MacroRole::Downstream`]. The two sets are disjoint by construction,
/// so a node is `User` iff it is in `focus.users`.
///
/// Pure assembly — no domain walk here; the focus set is computed upstream
/// (`cli::execute_explore`), keeping this layer a pure renderer.
#[must_use]
pub fn build_macro_lineage_payload(current: &Manifest, focus: &MacroFocusSet) -> LineagePayload {
    let typed = focused_typed_node_map(current, focus);
    let lineage = lineage_from_typed_map(current, &typed);
    // `changed` is unused on the macro page (no `--pr-diff` underlay on
    // the focused carrier); every node maps to `false`.
    lineage_payload_from(
        current,
        lineage,
        |_| false,
        |id| {
            Some(if focus.users.contains(id) {
                MacroRole::User
            } else {
                MacroRole::Downstream
            })
        },
    )
}

/// The typed node union RESTRICTED to a focus id set (cute-dbt#345 Slice
/// 3) — the focused-DAG counterpart of [`build_typed_node_map`].
///
/// Unlike the full builder, this one never folds in the whole manifest:
/// every node id is typed by consulting the manifest, but ONLY if it is
/// in `focus.users ∪ focus.downstream`. A focus id absent from the
/// manifest's `nodes`/`sources`/`exposures` maps is skipped (a defensive
/// dangling id never becomes an untyped vertex). [`BTreeMap`] ordering
/// keeps the payload deterministic (golden stability).
fn focused_typed_node_map<'a>(
    current: &'a Manifest,
    focus: &MacroFocusSet,
) -> BTreeMap<&'a NodeId, LineageNodeType> {
    let mut typed: BTreeMap<&NodeId, LineageNodeType> = BTreeMap::new();
    let in_focus = |id: &NodeId| focus.users.contains(id) || focus.downstream.contains(id);
    for (id, node) in current.nodes() {
        if let Some(node_type) = in_focus(id)
            .then(|| lineage_vertex_type(node.resource_type()))
            .flatten()
        {
            typed.insert(id, node_type);
        }
    }
    for id in current.sources().keys() {
        if in_focus(id) {
            typed.insert(id, LineageNodeType::Source);
        }
    }
    for id in current.exposures().keys() {
        if in_focus(id) {
            typed.insert(id, LineageNodeType::Exposure);
        }
    }
    typed
}

/// Classify a manifest node's `resource_type` into its lineage-vertex
/// vocabulary member, or `None` when the type is not a vertex (a `test` /
/// `operation` / any other consumer the closure reached — silently skipped,
/// the same posture the edge builder applies to non-vertex deps).
///
/// Only the `nodes()`-borne resource types are classified here; `source` and
/// `exposure` are keyed off their own manifest maps by the callers (they are
/// not `nodes()` entries), so they are deliberately absent from this match.
/// Factored out of [`focused_typed_node_map`] so the focus loop stays a thin
/// driver and the classification is unit-testable in isolation.
fn lineage_vertex_type(resource_type: &str) -> Option<LineageNodeType> {
    match resource_type {
        "model" => Some(LineageNodeType::Model),
        "snapshot" => Some(LineageNodeType::Snapshot),
        "seed" => Some(LineageNodeType::Seed),
        _ => None,
    }
}

/// Assemble a [`Lineage`] (typed nodes + forward edges) from a prebuilt
/// typed-node map — the shared core of [`build_lineage`] (full manifest)
/// and [`build_macro_lineage_payload`] (focused subgraph). Both compute
/// `not_compiled` per type and the deduplicated forward edge list the
/// same way; only the node-set source differs.
fn lineage_from_typed_map(
    current: &Manifest,
    typed: &BTreeMap<&NodeId, LineageNodeType>,
) -> Lineage {
    let index_of: HashMap<&NodeId, usize> =
        typed.keys().enumerate().map(|(i, id)| (*id, i)).collect();
    let data_tests = data_test_counts(current);
    let unit_tests = unit_test_counts(current);
    let nodes: Vec<LineageNode> = typed
        .iter()
        .map(|(id, &node_type)| {
            let node = current.node(id);
            LineageNode {
                id: id.as_str().to_owned(),
                name: lineage_node_name(current, id, node_type),
                node_type,
                not_compiled: matches!(
                    node_type,
                    LineageNodeType::Model | LineageNodeType::Snapshot
                ) && node.is_none_or(|n| n.compiled_code().is_none()),
                data_tests: data_tests.get(*id).copied().unwrap_or(0),
                unit_tests: unit_tests.get(*id).copied().unwrap_or(0),
            }
        })
        .collect();
    let edges = lineage_edges(current, typed, &index_of);
    Lineage { nodes, edges }
}

/// Build the per-model CTE-DAG map for the dag.html carrier
/// (cute-dbt#102) — the CTE ⇄ model view toggle's data.
///
/// Pure zip over the documented one-to-one contract between `models`
/// and `payload.models` (see the private `explore_models` assembler
/// below, which documents the zip's soundness): each model id keys
/// its payload entry's [`DagPayload`] — the same engine-extracted,
/// role-classified graph the report renders, parsed exactly once during
/// `build_payload`. Models whose graph is empty (an uncompiled
/// `dbt parse` node, or compiled SQL with no `WITH` clause) contribute
/// **no** entry, keeping the carrier lean; the client distinguishes the
/// two off the lineage node's `not_compiled` flag (fail-open: the
/// degraded view is labeled, never an error).
#[must_use]
pub fn cte_dags_by_model(
    models: &ModelInScopeSet,
    payload: &ReportPayload,
) -> BTreeMap<String, DagPayload> {
    models
        .iter()
        .zip(payload.models.iter())
        .filter(|(_, model_payload)| !model_payload.dag.nodes.is_empty())
        .map(|(id, model_payload)| (id.as_str().to_owned(), model_payload.dag.clone()))
        .collect()
}

/// One model section on `tests.html` (server-rendered).
struct ExploreModel {
    /// Full manifest node id — the `data-model-id` DOM handle.
    id: String,
    /// Bare model name.
    name: String,
    /// Project-relative source path, when the manifest carries one.
    path: Option<String>,
    /// The fail-open "not compiled" badge (cute-dbt#100).
    not_compiled: bool,
    /// The model's unit tests (every test targeting it — full manifest,
    /// so there is no in-scope/changed distinction here).
    tests: Vec<ExploreTest>,
}

/// One unit-test row on `tests.html`.
struct ExploreTest {
    /// Manifest unit-test id — the `data-test-id` handle the index row
    /// carries so the viewer (cute-dbt#102) can select it in place.
    id: String,
    /// User-facing test name.
    name: String,
    /// Optional `description:` from the manifest.
    description: Option<String>,
    /// Pre-formatted fixture-shape summary (given count + expect row
    /// count) — built in Rust so the template stays a pure renderer.
    shape: String,
}

/// The one-line fixture-shape summary for a test row
/// (`"2 givens, expects 1 row"`).
fn test_shape(given_count: usize, expect_rows: Option<usize>) -> String {
    let givens = plural(given_count, "given");
    match expect_rows {
        Some(n) => format!("{givens}, expects {}", plural(n, "row")),
        None => givens,
    }
}

/// `1 given` / `2 givens` — simple `+s` pluralization.
fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The per-node test-count badge line (cute-dbt#103):
/// `"N data-tests · M unit-tests"`, pluralized via [`plural`] and
/// explicit at 0/0 (`"0 data-tests · 0 unit-tests"`).
fn test_badge(data_tests: usize, unit_tests: usize) -> String {
    format!(
        "{} \u{b7} {}",
        plural(data_tests, "data-test"),
        plural(unit_tests, "unit-test")
    )
}

/// The type-aware badge line (cute-dbt#253). Models keep the explicit
/// 0/0 [`test_badge`] (the cute-dbt#103 posture). Snapshots / seeds /
/// sources badge their data-test count only when non-zero — dbt unit
/// tests cannot target them, so an explicit `"0 unit-tests"` would be
/// structural noise, not honesty, and an all-zero line carries no fact.
/// Exposures are untestable and never badge (single-line label).
fn typed_badge(node_type: LineageNodeType, data_tests: usize, unit_tests: usize) -> String {
    match node_type {
        LineageNodeType::Model => test_badge(data_tests, unit_tests),
        LineageNodeType::Snapshot | LineageNodeType::Seed | LineageNodeType::Source => {
            if data_tests > 0 {
                plural(data_tests, "data-test")
            } else {
                String::new()
            }
        }
        LineageNodeType::Exposure => String::new(),
    }
}

/// askama binding for `templates/explore-dag.html`.
#[derive(Template)]
#[template(path = "explore-dag.html", escape = "html")]
struct ExploreDagTemplate<'a> {
    sakura_css: &'a str,
    /// SHARED appearance engine (cute-dbt#242) — the page honors the
    /// saved `cute-dbt.appearance.v1` appearance (read-only; the
    /// explore-side settings affordance is cute-dbt#219).
    appearance_js: &'a str,
    cytoscape_js: &'a str,
    cytoscape_dagre_js: &'a str,
    explore_lineage_js: &'a str,
    /// First-party CTE-view engine (cute-dbt#102) — the CTE ⇄ model
    /// view toggle and the per-model Cytoscape CTE DAG.
    explore_cte_js: &'a str,
    favicon_data_uri: &'a str,
    /// Pre-escaped JSON for the `explore-dag-data` carrier (the
    /// [`LineagePayload`]).
    dag_json: &'a str,
    /// `model`-typed lineage nodes only (cute-dbt#253 — the header's
    /// typed counts; pre-#253 this was every node).
    model_count: usize,
    /// Typed-node counts (cute-dbt#253) — each gates its header segment
    /// and its legend chip (rendered only when present).
    snapshot_count: usize,
    seed_count: usize,
    source_count: usize,
    exposure_count: usize,
    edge_count: usize,
    not_compiled_count: usize,
    /// The external-drive contract version (cute-dbt#105) — rendered as
    /// the `data-cute-dbt-contract` attribute on `<body>`, the single
    /// source the in-page `window.cuteDbtContract` global reads back.
    contract_version: &'a str,
    /// `true` iff the run carried `--pr-diff` change context
    /// (cute-dbt#106) — gates the header's "changed in this diff"
    /// count and the legend's "changed" chip. When `false` (the
    /// no-context path) the template emits NOTHING extra, keeping the
    /// no-flag render shape unchanged.
    has_change_context: bool,
    /// Number of lineage nodes the change context marked (0 is honest:
    /// a diff touching no model files still renders the count).
    changed_count: usize,
    /// `true` iff the run emitted `macro.html` (cute-dbt#345 — a
    /// `--pr-diff` that changed a root-project macro). Gates the third
    /// nav anchor so the no-macro goldens stay byte-identical (the
    /// conditional-anchor contract, mirrored on every explore page).
    has_macro_focus: bool,
}

/// askama binding for `templates/explore-tests.html`.
#[derive(Template)]
#[template(path = "explore-tests.html", escape = "html")]
struct ExploreTestsTemplate<'a> {
    sakura_css: &'a str,
    /// SHARED appearance engine (cute-dbt#242) — the page honors the
    /// saved `cute-dbt.appearance.v1` appearance (read-only; the
    /// explore-side settings affordance is cute-dbt#219).
    appearance_js: &'a str,
    favicon_data_uri: &'a str,
    models: &'a [ExploreModel],
    model_count: usize,
    test_count: usize,
    /// First-party unit-test viewer engine (cute-dbt#102) — renders the
    /// selected test's fixtures into the shared test-card partial from
    /// the embedded payload.
    explore_tests_js: &'a str,
    /// Pre-escaped JSON for the `cute-dbt-data` carrier (the full
    /// [`ReportPayload`] — the `build_payload` reuse seam).
    payload_json: &'a str,
    /// `true` iff the run emitted `macro.html` (cute-dbt#345). Gates the
    /// third nav anchor — see [`ExploreDagTemplate::has_macro_focus`].
    has_macro_focus: bool,
}

/// askama binding for `templates/explore-macro.html`.
///
/// The third explore sub-page (cute-dbt#345, epic cute-dbt#99). Emitted
/// only when a `--pr-diff` changed a root-project macro. Slice 3 renders
/// the focused macro DAG: the [`build_macro_lineage_payload`] carrier (the
/// `users ∪ downstream` subgraph, role-stamped) driven by the SAME
/// vendored Cytoscape + cytoscape-dagre core and `explore-lineage.js`
/// engine as `dag.html`, with the macro-callers emphasized and the
/// downstream closure dimmed-as-context. The filtered model+test
/// directory (Slice 4) and the legibility banner (Slice 5) land in this
/// shell later.
#[derive(Template)]
#[template(path = "explore-macro.html", escape = "html")]
struct ExploreMacroTemplate<'a> {
    sakura_css: &'a str,
    /// SHARED appearance engine (cute-dbt#242) — the page honors the
    /// saved `cute-dbt.appearance.v1` appearance (read-only).
    appearance_js: &'a str,
    /// The vendored Cytoscape UMD core + the cytoscape-dagre layout
    /// extension + the first-party lineage engine — the SAME pinned
    /// assets `dag.html` embeds (cute-dbt#101; `assets/MANIFEST.toml`
    /// untouched, R6). The macro page reuses the lineage engine verbatim
    /// over the focused carrier.
    cytoscape_js: &'a str,
    cytoscape_dagre_js: &'a str,
    explore_lineage_js: &'a str,
    favicon_data_uri: &'a str,
    /// Pre-escaped JSON for the `explore-dag-data` carrier — the focused
    /// [`LineagePayload`] from [`build_macro_lineage_payload`]. Each node
    /// carries its `macro_role` (`"user"`/`"downstream"`), the per-role
    /// boot-class hook in `explore-lineage.js`.
    dag_json: &'a str,
    /// The macro-caller (`users`) count — the emphasized models.
    user_count: usize,
    /// The downstream-closure count — the dimmed-as-context nodes.
    downstream_count: usize,
    /// Forward dependency edges in the focused subgraph.
    edge_count: usize,
}

/// Serialize `value` for safe embedding inside an HTML
/// `<script type="application/json">` block.
///
/// The generic twin of the report renderer's
/// `payload_json_for_html_script` (`src/adapters/render.rs`) — kept
/// local so the explore lane never touches the render-lane file; the
/// escape contract is identical: every `<` followed by `/`, `!`, `?`,
/// or an ASCII letter becomes `<` (the tag-opening shapes under
/// HTML5's script-data state machine), which is a documented JSON
/// escape, so `JSON.parse` round-trips the original characters.
fn json_for_html_script<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string(value)?;
    let mut out = String::with_capacity(json.len() + 16);
    let mut chars = json.chars().peekable();
    while let Some(c) = chars.next() {
        let tag_opener = matches!(chars.peek(), Some('/' | '!' | '?' | 'a'..='z' | 'A'..='Z'));
        if c == '<' && tag_opener {
            out.push_str("\\u003c");
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

/// The last dotted segment of a manifest node id — the bare model name.
/// (Local twin of the render module's private `leaf_segment`.)
fn leaf_segment(id: &str) -> &str {
    id.rsplit('.').next().unwrap_or(id)
}

/// Assemble the server-rendered model sections for `tests.html`.
///
/// `payload.models` mirrors `models` one-to-one and in the same order:
/// `build_payload` iterates the same `ModelInScopeSet`, and under the
/// explore composition every id in the set came **from** the manifest
/// (`all_models`), so its skip-missing-node branch never fires. The zip
/// therefore pairs each node id with its payload entry; the
/// `not_compiled` flag is read from the manifest node (fail-open).
fn explore_models(
    current: &Manifest,
    models: &ModelInScopeSet,
    payload: &ReportPayload,
) -> Vec<ExploreModel> {
    models
        .iter()
        .zip(payload.models.iter())
        .map(|(id, model_payload)| {
            let node = current.node(id);
            ExploreModel {
                id: id.as_str().to_owned(),
                name: model_payload.name.clone(),
                path: model_payload.path.clone(),
                not_compiled: node.is_none_or(|n| n.compiled_code().is_none()),
                tests: model_payload
                    .tests
                    .iter()
                    .map(|t| ExploreTest {
                        id: t.id.clone(),
                        name: t.name.clone(),
                        description: t.description.clone(),
                        shape: test_shape(
                            t.given.len(),
                            t.expected.table.as_ref().map(|table| table.rows.len()),
                        ),
                    })
                    .collect(),
            }
        })
        .collect()
}

/// Render the explore pages into `out_dir` (created if absent).
///
/// Writes `dag.html` then `tests.html`, and — when `macro_focus` is
/// `Some` — the third sub-page `macro.html` (cute-dbt#345). A failure on
/// any write (or on directory creation) surfaces the underlying
/// [`io::Error`] — the cli layer names `--out-dir` in the operator
/// message. Template rendering itself is compile-time-checked askama
/// (the same infallible-at-runtime posture as the report renderer).
///
/// `changed` is the optional `--pr-diff` change context (cute-dbt#106):
/// `Some(set)` marks the member lineage nodes and renders the header
/// count + legend chip; `None` renders exactly the pre-#106 no-context
/// page. Either way the full `models` set renders — context never
/// narrows scope.
///
/// `macro_focus` (cute-dbt#345) is the resolved
/// [`MacroFocusSet`] for the macro a
/// `--pr-diff` changed: `Some(focus)` renders the focused macro DAG into
/// `macro.html` (and the third nav anchor on every page); `None` keeps
/// the two-page output (and its byte-identical goldens) unchanged. The
/// renderer takes the pre-resolved focus set, not the raw changed-macro
/// ids — scope resolution (and the domain walk) stays in `cli/mod.rs`, so
/// this layer is a pure renderer (the explore-lane posture).
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when `out_dir` cannot be
/// created or any page cannot be written.
pub fn render_explore(
    out_dir: &Path,
    current: &Manifest,
    models: &ModelInScopeSet,
    changed: Option<&ModelInScopeSet>,
    payload: &ReportPayload,
    macro_focus: Option<&MacroFocusSet>,
) -> io::Result<()> {
    fs::create_dir_all(out_dir)?;

    // The presence of a focus set gates the macro page + the conditional
    // third nav anchor on every page (the byte-identity-golden contract:
    // no anchor, no macro.html when `None`).
    let has_macro_focus = macro_focus.is_some();

    let mut lineage = build_lineage_payload(current, models, changed);
    // cute-dbt#102 — the CTE ⇄ model toggle's per-model CTE DAGs ride
    // the same carrier (the payload's graphs, parsed once upstream).
    lineage.cte_dags = cte_dags_by_model(models, payload);
    let not_compiled_count = lineage.nodes.iter().filter(|n| n.not_compiled).count();
    // The marked-node count (what actually renders), not `changed.len()`
    // — a defensive id outside the model set must not inflate the banner.
    let changed_count = lineage.nodes.iter().filter(|n| n.changed).count();
    // cute-dbt#253 — typed counts for the header + the legend gating.
    let count_of = |node_type: LineageNodeType| {
        lineage
            .nodes
            .iter()
            .filter(|n| n.node_type == node_type)
            .count()
    };
    let dag_json = json_for_html_script(&lineage)
        .map_err(|err| io::Error::other(format!("dag payload serialization: {err}")))?;
    let dag_html = ExploreDagTemplate {
        sakura_css: SAKURA_CSS,
        appearance_js: APPEARANCE_JS,
        cytoscape_js: CYTOSCAPE_JS,
        cytoscape_dagre_js: CYTOSCAPE_DAGRE_JS,
        explore_lineage_js: EXPLORE_LINEAGE_JS,
        explore_cte_js: EXPLORE_CTE_JS,
        favicon_data_uri: FAVICON_DATA_URI,
        dag_json: &dag_json,
        model_count: count_of(LineageNodeType::Model),
        snapshot_count: count_of(LineageNodeType::Snapshot),
        seed_count: count_of(LineageNodeType::Seed),
        source_count: count_of(LineageNodeType::Source),
        exposure_count: count_of(LineageNodeType::Exposure),
        edge_count: lineage.edges.len(),
        not_compiled_count,
        contract_version: EXPLORE_CONTRACT_VERSION,
        has_change_context: changed.is_some(),
        changed_count,
        has_macro_focus,
    }
    .render()
    .map_err(|err| io::Error::other(format!("render dag.html: {err}")))?;
    fs::write(out_dir.join("dag.html"), dag_html)?;

    let models_pod = explore_models(current, models, payload);
    let test_count = models_pod.iter().map(|m| m.tests.len()).sum();
    let payload_json = json_for_html_script(payload)
        .map_err(|err| io::Error::other(format!("payload serialization: {err}")))?;
    let tests_html = ExploreTestsTemplate {
        sakura_css: SAKURA_CSS,
        appearance_js: APPEARANCE_JS,
        favicon_data_uri: FAVICON_DATA_URI,
        models: &models_pod,
        model_count: models_pod.len(),
        test_count,
        explore_tests_js: EXPLORE_TESTS_JS,
        payload_json: &payload_json,
        has_macro_focus,
    }
    .render()
    .map_err(|err| io::Error::other(format!("render tests.html: {err}")))?;
    fs::write(out_dir.join("tests.html"), tests_html)?;

    // cute-dbt#345 — the third explore sub-page. Emitted only when the
    // run carried a `--pr-diff` changing a root-project macro; the
    // no-macro path writes just dag.html/tests.html (the goldens' shape).
    // Slice 3 renders the focused macro DAG: the `users ∪ downstream`
    // subgraph (role-stamped) through the same vendored Cytoscape +
    // cytoscape-dagre engine `dag.html` uses.
    if let Some(focus) = macro_focus {
        let macro_lineage = build_macro_lineage_payload(current, focus);
        // The displayed counts must reflect what RENDERS, not the domain
        // closure: `focus.downstream` crosses every consumer node type
        // (incl. `test` nodes), but `focused_typed_node_map` keeps only
        // lineage-vertex types (model/snapshot/seed/source/exposure). A
        // closure with N test nodes would inflate `focus.downstream.len()`
        // far above the rendered vertex count — an over-claim (the
        // never-a-false-claim invariant). So count the roles that actually
        // materialized in the focused payload (qodo #4, cute-dbt#345).
        let user_count = macro_lineage
            .nodes
            .iter()
            .filter(|n| n.macro_role == Some(MacroRole::User))
            .count();
        let downstream_count = macro_lineage
            .nodes
            .iter()
            .filter(|n| n.macro_role == Some(MacroRole::Downstream))
            .count();
        let macro_dag_json = json_for_html_script(&macro_lineage)
            .map_err(|err| io::Error::other(format!("macro dag payload serialization: {err}")))?;
        let macro_html = ExploreMacroTemplate {
            sakura_css: SAKURA_CSS,
            appearance_js: APPEARANCE_JS,
            cytoscape_js: CYTOSCAPE_JS,
            cytoscape_dagre_js: CYTOSCAPE_DAGRE_JS,
            explore_lineage_js: EXPLORE_LINEAGE_JS,
            favicon_data_uri: FAVICON_DATA_URI,
            dag_json: &macro_dag_json,
            user_count,
            downstream_count,
            edge_count: macro_lineage.edges.len(),
        }
        .render()
        .map_err(|err| io::Error::other(format!("render macro.html: {err}")))?;
        fs::write(out_dir.join("macro.html"), macro_html)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap as StdHashMap};

    use crate::adapters::render::build_payload;
    use crate::domain::{
        Checksum, DependsOn, InScopeSet, ManifestMetadata, Node, NodeConfig, all_models,
    };

    fn model(id: &str, compiled: Option<&str>, deps: &[&str]) -> Node {
        Node::new(
            NodeId::new(id),
            "model",
            Checksum::new("sha256", "ck"),
            compiled.map(str::to_owned),
            None,
            DependsOn::new(Vec::new(), deps.iter().map(|d| NodeId::new(*d)).collect()),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    fn manifest_of(nodes: Vec<Node>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            StdHashMap::new(),
            StdHashMap::new(),
        )
    }

    fn three_model_manifest() -> Manifest {
        manifest_of(vec![
            model("model.shop.stg_orders", Some("select 1"), &[]),
            model(
                "model.shop.dim_orders",
                Some("select 1"),
                &["model.shop.stg_orders"],
            ),
            // Uncompiled — the fail-open node.
            model(
                "model.shop.mart_orders",
                None,
                &["model.shop.dim_orders", "source.shop.raw.orders"],
            ),
        ])
    }

    // ----- unit_test_counts (cute-dbt#254) ---------------------------

    #[test]
    fn unit_test_counts_bind_a_versioned_model_via_engine_resolved_id() {
        // A versioned model's leaf segment is its version suffix
        // (`…dim_customers.v2` → `"v2"`), so bare-name resolution can
        // never bind it — the engine-resolved `tested_node_unique_id`
        // must carry the badge attribution (cute-dbt#254).
        use crate::domain::{UnitTest, UnitTestExpect};
        let versioned = model("model.shop.dim_customers.v2", Some("select 1"), &[]);
        let ut = UnitTest::new(
            "t1",
            NodeId::new("dim_customers"),
            Vec::new(),
            UnitTestExpect::new(serde_json::Value::Null, None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
        .with_tested_node_unique_id(Some(NodeId::new("model.shop.dim_customers.v2")));
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            [(versioned.id().clone(), versioned)].into_iter().collect(),
            [("unit_test.shop.t1.v2".to_owned(), ut)]
                .into_iter()
                .collect(),
            StdHashMap::new(),
        );
        let counts = unit_test_counts(&current);
        assert_eq!(
            counts
                .get(&NodeId::new("model.shop.dim_customers.v2"))
                .copied(),
            Some(1),
            "the versioned model's badge must count its unit test",
        );
    }

    // ----- build_lineage --------------------------------------------

    #[test]
    fn lineage_has_one_node_per_model_in_id_order() {
        let current = three_model_manifest();
        let lineage = build_lineage(&current, &all_models(&current));
        let ids: Vec<&str> = lineage.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "model.shop.dim_orders",
                "model.shop.mart_orders",
                "model.shop.stg_orders",
            ],
        );
        assert_eq!(lineage.nodes[0].name, "dim_orders");
    }

    #[test]
    fn lineage_edges_connect_models_and_skip_non_model_deps() {
        let current = three_model_manifest();
        let lineage = build_lineage(&current, &all_models(&current));
        // stg_orders(2) -> dim_orders(0); dim_orders(0) -> mart_orders(1).
        // The source.shop.raw.orders dependency is NOT a lineage edge.
        assert_eq!(lineage.edges, vec![(0, 1), (2, 0)]);
    }

    // ----- typed lineage nodes (cute-dbt#253) -------------------------

    /// A `nodes`-map entry of an arbitrary resource type with explicit
    /// dependency edges — the snapshot/seed builder.
    fn typed_node(id: &str, resource_type: &str, compiled: Option<&str>, deps: &[&str]) -> Node {
        Node::new(
            NodeId::new(id),
            resource_type,
            Checksum::new("sha256", "ck"),
            compiled.map(str::to_owned),
            None,
            DependsOn::new(Vec::new(), deps.iter().map(|d| NodeId::new(*d)).collect()),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    /// A `sources`-map entry.
    fn source_entry(id: &str, source_name: &str, table: &str) -> crate::domain::SourceNode {
        crate::domain::SourceNode::new(
            NodeId::new(id),
            source_name,
            table,
            None,
            "main",
            None,
            None,
        )
    }

    /// An `exposures`-map entry depending on `deps`.
    fn exposure_entry(id: &str, name: &str, deps: &[&str]) -> crate::domain::Exposure {
        crate::domain::Exposure::new(
            NodeId::new(id),
            name,
            Some("dashboard".to_owned()),
            None,
            None,
            DependsOn::new(Vec::new(), deps.iter().map(|d| NodeId::new(*d)).collect()),
        )
    }

    /// source → stg → snapshot → dim → exposure, plus seed → stg — every
    /// cute-dbt#253 node type in one connected manifest.
    fn typed_manifest() -> Manifest {
        manifest_of(vec![
            model(
                "model.shop.stg_patients",
                Some("select 1"),
                &["source.shop.raw.patients", "seed.shop.raw_codes"],
            ),
            typed_node(
                "snapshot.shop.snp_patients",
                "snapshot",
                Some("select 1"),
                &["model.shop.stg_patients"],
            ),
            typed_node("seed.shop.raw_codes", "seed", None, &[]),
            model(
                "model.shop.dim_patients",
                Some("select 1"),
                &["snapshot.shop.snp_patients"],
            ),
        ])
        .with_sources(
            [(
                NodeId::new("source.shop.raw.patients"),
                source_entry("source.shop.raw.patients", "raw", "patients"),
            )]
            .into_iter()
            .collect(),
        )
        .with_exposures(
            [(
                NodeId::new("exposure.shop.patient_dashboard"),
                exposure_entry(
                    "exposure.shop.patient_dashboard",
                    "patient_dashboard",
                    &["model.shop.dim_patients"],
                ),
            )]
            .into_iter()
            .collect(),
        )
    }

    /// Edge lookup by node name — index-based assertions are brittle
    /// across the typed union's id ordering.
    fn edge_names(lineage: &Lineage) -> Vec<(String, String)> {
        lineage
            .edges
            .iter()
            .map(|&(from, to)| {
                (
                    lineage.nodes[from].name.clone(),
                    lineage.nodes[to].name.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn lineage_renders_a_snapshot_as_a_typed_mid_chain_node() {
        // The cute-dbt#253 defect: stg → snapshot → dim must stay ONE
        // connected chain — the snapshot is a typed node, never filtered
        // (which severed the graph and faked dim as a root).
        let current = typed_manifest();
        let lineage = build_lineage(&current, &all_models(&current));
        let snp = lineage
            .nodes
            .iter()
            .find(|n| n.id == "snapshot.shop.snp_patients")
            .expect("the snapshot is a lineage node");
        assert_eq!(snp.node_type, LineageNodeType::Snapshot);
        let edges = edge_names(&lineage);
        assert!(
            edges.contains(&("stg_patients".to_owned(), "snp_patients".to_owned())),
            "upstream chain into the snapshot survives: {edges:?}",
        );
        assert!(
            edges.contains(&("snp_patients".to_owned(), "dim_patients".to_owned())),
            "downstream chain out of the snapshot survives (dim is NOT a false root): {edges:?}",
        );
    }

    #[test]
    fn lineage_renders_seeds_and_sources_as_typed_roots() {
        let current = typed_manifest();
        let lineage = build_lineage(&current, &all_models(&current));
        let by_id: StdHashMap<&str, &LineageNode> =
            lineage.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        assert_eq!(
            by_id["seed.shop.raw_codes"].node_type,
            LineageNodeType::Seed
        );
        assert_eq!(
            by_id["source.shop.raw.patients"].node_type,
            LineageNodeType::Source
        );
        let edges = edge_names(&lineage);
        assert!(
            edges.contains(&("raw_codes".to_owned(), "stg_patients".to_owned())),
            "seed feeds the staging model: {edges:?}",
        );
        assert!(
            edges.contains(&("raw.patients".to_owned(), "stg_patients".to_owned())),
            "source feeds the staging model (stg is NOT a false root): {edges:?}",
        );
        // Roots: nothing flows INTO a seed or a source.
        for root in ["raw_codes", "raw.patients"] {
            assert!(
                !edges.iter().any(|(_, to)| to == root),
                "{root} must have no incoming edge: {edges:?}",
            );
        }
    }

    #[test]
    fn lineage_renders_exposures_as_typed_sinks() {
        let current = typed_manifest();
        let lineage = build_lineage(&current, &all_models(&current));
        let exposure = lineage
            .nodes
            .iter()
            .find(|n| n.id == "exposure.shop.patient_dashboard")
            .expect("the exposure is a lineage node");
        assert_eq!(exposure.node_type, LineageNodeType::Exposure);
        let edges = edge_names(&lineage);
        assert!(
            edges.contains(&("dim_patients".to_owned(), "patient_dashboard".to_owned())),
            "the exposure terminates the lineage: {edges:?}",
        );
        assert!(
            !edges.iter().any(|(from, _)| from == "patient_dashboard"),
            "an exposure is a sink — no outgoing edge: {edges:?}",
        );
    }

    #[test]
    fn lineage_source_labels_carry_the_source_name_prefix() {
        let current = typed_manifest();
        let lineage = build_lineage(&current, &all_models(&current));
        let source = lineage
            .nodes
            .iter()
            .find(|n| n.node_type == LineageNodeType::Source)
            .expect("source node present");
        assert_eq!(
            source.name, "raw.patients",
            "source labels are source_name.table — the two source(...) arguments",
        );
    }

    #[test]
    fn seeds_are_never_flagged_not_compiled() {
        // fusion null-fills seed compiled_code UNCONDITIONALLY
        // (manifest_nodes.rs:232-233 @ 9977b6cb…) — a seed has no SQL,
        // so the dbt-parse "not compiled" treatment would be dishonest.
        let current = typed_manifest();
        let lineage = build_lineage(&current, &all_models(&current));
        let seed = lineage
            .nodes
            .iter()
            .find(|n| n.node_type == LineageNodeType::Seed)
            .expect("seed node present");
        assert!(!seed.not_compiled, "seeds never render dbt-parse-dashed");
        let source = lineage
            .nodes
            .iter()
            .find(|n| n.node_type == LineageNodeType::Source)
            .expect("source node present");
        assert!(!source.not_compiled, "sources carry no code at all");
    }

    #[test]
    fn snapshot_not_compiled_mirrors_compiled_code_presence() {
        // fusion null-fills snapshot compiled_code at parse
        // (manifest_nodes.rs:616-617 @ 9977b6cb…) and backfills it on
        // compile (dbt-tasks-sa/src/utils.rs:151-172) — the flag is the
        // honest dbt-parse signal for snapshots, exactly like models.
        let current = manifest_of(vec![
            typed_node("snapshot.shop.compiled", "snapshot", Some("select 1"), &[]),
            typed_node("snapshot.shop.parse_only", "snapshot", None, &[]),
        ]);
        let lineage = build_lineage(&current, &all_models(&current));
        let by_id: StdHashMap<&str, bool> = lineage
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.not_compiled))
            .collect();
        assert!(!by_id["snapshot.shop.compiled"]);
        assert!(by_id["snapshot.shop.parse_only"]);
    }

    #[test]
    fn typed_nodes_order_deterministically_by_full_id() {
        let current = typed_manifest();
        let lineage = build_lineage(&current, &all_models(&current));
        let ids: Vec<&str> = lineage.nodes.iter().map(|n| n.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "typed union iterates in full-id order");
        assert_eq!(lineage.nodes.len(), 6, "all five types render");
    }

    #[test]
    fn non_model_badges_show_data_tests_only_and_only_when_present() {
        // Models keep the explicit 0/0 badge (the cute-dbt#103 posture).
        // The new types show their data-test count only when non-zero
        // (unit tests cannot target them — an explicit "0 unit-tests"
        // there would be structural noise, not honesty); exposures are
        // untestable and never badge.
        let mut current = typed_manifest();
        // Attach one data test to the snapshot and one to the source
        // (the `attached_node` linkage names non-model parents here —
        // pre-#253 those entries were inert; now they badge).
        let t1 = data_test(
            "test.shop.snp_check",
            Some("snapshot.shop.snp_patients"),
            &[],
            None,
        );
        let t2 = data_test(
            "test.shop.src_check",
            Some("source.shop.raw.patients"),
            &[],
            None,
        );
        let mut nodes = current.nodes().clone();
        nodes.insert(t1.id().clone(), t1);
        nodes.insert(t2.id().clone(), t2);
        current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            StdHashMap::new(),
            StdHashMap::new(),
        )
        .with_sources(current.sources().clone())
        .with_exposures(current.exposures().clone());
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        let badge_of = |id: &str| -> &str {
            payload
                .nodes
                .iter()
                .find(|n| n.id == id)
                .map_or_else(|| panic!("{id} missing from payload"), |n| n.badge.as_str())
        };
        assert_eq!(
            badge_of("model.shop.dim_patients"),
            "0 data-tests \u{b7} 0 unit-tests",
            "models keep the explicit 0/0 badge",
        );
        assert_eq!(badge_of("snapshot.shop.snp_patients"), "1 data-test");
        assert_eq!(badge_of("source.shop.raw.patients"), "1 data-test");
        assert_eq!(
            badge_of("seed.shop.raw_codes"),
            "",
            "untested seed: no badge line"
        );
        assert_eq!(badge_of("exposure.shop.patient_dashboard"), "");
    }

    #[test]
    fn payload_serializes_node_type_wire_keys() {
        let current = typed_manifest();
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        let json: serde_json::Value = serde_json::to_value(&payload).expect("payload serializes");
        let type_of = |id: &str| -> String {
            json["nodes"]
                .as_array()
                .expect("nodes array")
                .iter()
                .find(|n| n["id"] == id)
                .unwrap_or_else(|| panic!("{id} missing"))["node_type"]
                .as_str()
                .expect("node_type is a string")
                .to_owned()
        };
        assert_eq!(type_of("model.shop.stg_patients"), "model");
        assert_eq!(type_of("snapshot.shop.snp_patients"), "snapshot");
        assert_eq!(type_of("seed.shop.raw_codes"), "seed");
        assert_eq!(type_of("source.shop.raw.patients"), "source");
        assert_eq!(type_of("exposure.shop.patient_dashboard"), "exposure");
    }

    #[test]
    fn source_payload_detail_carries_column_descriptions_and_empty_paths() {
        let mut sources = StdHashMap::new();
        sources.insert(
            NodeId::new("source.shop.raw.patients"),
            source_entry("source.shop.raw.patients", "raw", "patients").with_column_descriptions(
                [("patient_id".to_owned(), "natural key".to_owned())]
                    .into_iter()
                    .collect(),
            ),
        );
        let current = manifest_of(vec![model(
            "model.shop.stg_patients",
            Some("select 1"),
            &["source.shop.raw.patients"],
        )])
        .with_sources(sources);
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        let source = payload
            .nodes
            .iter()
            .find(|n| n.id == "source.shop.raw.patients")
            .expect("source node present");
        assert_eq!(source.detail.columns.len(), 1);
        assert_eq!(source.detail.columns[0].name, "patient_id");
        assert_eq!(
            source.detail.columns[0].description.as_deref(),
            Some("natural key"),
        );
        assert_eq!(source.paths, NodePathsPayload::default());
    }

    #[test]
    fn every_lineage_node_type_is_styled_and_legended() {
        // The node-vocab completeness guard (cute-dbt#253) — the
        // edge-vocab-completeness twin, test-level: every wire key must
        // have a Cytoscape style selector in the lineage engine AND a
        // legend chip in the dag template. The exhaustive
        // `wire_key` match is the compile-time half.
        let engine = include_str!("../../templates/explore-lineage.js");
        let template = include_str!("../../templates/explore-dag.html");
        for node_type in LineageNodeType::ALL {
            let key = node_type.wire_key();
            assert!(
                engine.contains(&format!("node[type = \"{key}\"]")),
                "templates/explore-lineage.js must style node[type = \"{key}\"]",
            );
            assert!(
                template.contains(&format!("legend-chip type-{key}")),
                "templates/explore-dag.html must legend the {key} chip",
            );
        }
    }

    #[test]
    fn lineage_marks_uncompiled_models_not_compiled() {
        let current = three_model_manifest();
        let lineage = build_lineage(&current, &all_models(&current));
        let by_name: StdHashMap<&str, bool> = lineage
            .nodes
            .iter()
            .map(|n| (n.name.as_str(), n.not_compiled))
            .collect();
        assert!(by_name["mart_orders"], "dbt-parse model is flagged");
        assert!(!by_name["stg_orders"]);
    }

    #[test]
    fn lineage_of_an_empty_manifest_is_empty() {
        let current = manifest_of(Vec::new());
        let lineage = build_lineage(&current, &all_models(&current));
        assert!(lineage.nodes.is_empty());
        assert!(lineage.edges.is_empty());
    }

    // ----- build_lineage_payload ------------------------------------

    #[test]
    fn payload_carries_id_keyed_nodes_in_id_order() {
        let current = three_model_manifest();
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        let ids: Vec<&str> = payload.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "model.shop.dim_orders",
                "model.shop.mart_orders",
                "model.shop.stg_orders",
            ],
        );
        assert_eq!(payload.nodes[0].name, "dim_orders");
        assert!(
            payload.nodes[1].not_compiled,
            "the dbt-parse model carries the fail-open flag",
        );
    }

    #[test]
    fn payload_edges_are_forward_only_and_id_keyed() {
        let current = three_model_manifest();
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        // stg_orders -> dim_orders; dim_orders -> mart_orders. The
        // source.shop.raw.orders dependency is NOT a lineage edge, and
        // no reverse edge is ever emitted (the client traverses both
        // directions itself).
        assert_eq!(
            payload.edges,
            vec![
                LineageEdgePayload {
                    from: "model.shop.dim_orders".to_owned(),
                    to: "model.shop.mart_orders".to_owned(),
                },
                LineageEdgePayload {
                    from: "model.shop.stg_orders".to_owned(),
                    to: "model.shop.dim_orders".to_owned(),
                },
            ],
        );
        for edge in &payload.edges {
            assert!(
                !payload
                    .edges
                    .iter()
                    .any(|e| e.from == edge.to && e.to == edge.from),
                "no reverse twin for {edge:?}",
            );
        }
    }

    #[test]
    fn payload_of_an_empty_manifest_is_empty() {
        let current = manifest_of(Vec::new());
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        assert!(payload.nodes.is_empty());
        assert!(payload.edges.is_empty());
    }

    #[test]
    fn payload_serializes_hostile_names_as_json_data_never_markup() {
        let current = manifest_of(vec![model(
            "model.shop.evil</script><img src=x>",
            Some("select 1"),
            &[],
        )]);
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        let json = json_for_html_script(&payload).expect("serializes");
        assert!(
            !json.contains("</script>") && !json.contains("<img"),
            "tag-opening shapes must be escaped in the carrier: {json}",
        );
        let round: serde_json::Value = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(
            round["nodes"][0]["name"].as_str(),
            Some("evil</script><img src=x>"),
            "the hostile name survives as DATA",
        );
    }

    // ----- json_for_html_script --------------------------------------

    #[test]
    fn json_escapes_tag_opening_lt() {
        #[derive(Serialize)]
        struct Doc {
            s: String,
        }
        let out = json_for_html_script(&Doc {
            s: "</script><!-- <b> but 1 < 2 stays".to_owned(),
        })
        .expect("serializes");
        assert!(!out.contains("</script>"), "{out}");
        assert!(out.contains("\\u003c/script>"), "{out}");
        assert!(out.contains("\\u003c!--"), "{out}");
        assert!(out.contains("\\u003cb>"), "{out}");
        assert!(
            out.contains("1 < 2"),
            "bare < before space stays raw: {out}"
        );
    }

    // ----- render_explore (filesystem integration) -------------------

    fn tmp_dir(stem: &str) -> std::path::PathBuf {
        let p =
            std::env::temp_dir().join(format!("cute-dbt-explore-{}-{stem}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    fn payload_for(current: &Manifest, models: &ModelInScopeSet) -> ReportPayload {
        build_payload(
            current,
            &InScopeSet::new(),
            models,
            &StdHashMap::new(),
            &StdHashMap::new(),
            &StdHashMap::new(),
            &HashMap::new(),
            &StdHashMap::new(),
            "",
        )
    }

    #[test]
    fn render_explore_writes_both_pages_under_out_dir() {
        let current = three_model_manifest();
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dir = tmp_dir("both-pages");

        render_explore(&dir, &current, &models, None, &payload, None).expect("explore renders");

        let dag = fs::read_to_string(dir.join("dag.html")).expect("dag.html written");
        let tests = fs::read_to_string(dir.join("tests.html")).expect("tests.html written");
        // The model-count oracle: the rendered model set equals the
        // manifest's model count on BOTH pages.
        assert_eq!(
            dag.matches("\"not_compiled\":").count(),
            3,
            "the dag carrier embeds one payload node per manifest model",
        );
        // The interactive engine ships: the Cytoscape core, the dagre
        // layout extension, and the first-party lineage engine.
        assert!(
            dag.contains("cytoscapeDagre"),
            "dag.html embeds the cytoscape-dagre UMD extension",
        );
        assert!(
            dag.contains("cute-dbt explore lineage engine v1"),
            "dag.html embeds the first-party lineage engine",
        );
        assert_eq!(
            tests.matches("class=\"explore-model\"").count(),
            3,
            "tests.html renders one section per manifest model",
        );
        // The fail-open badge surfaces on the tests page too.
        assert!(tests.contains("not compiled"), "{tests}");
        // tests.html embeds the build_payload reuse seam.
        assert!(tests.contains("id=\"cute-dbt-data\""), "payload embedded");
        // tests.html is the page-aware static page: no Mermaid BUNDLE.
        // (The bare substring "mermaid" legitimately appears since
        // cute-dbt#242 — the shared appearance engine's DAG-engine pref
        // defaults to the string "mermaid"; the page-weight contract is
        // about the vendored bundle's bytes, so pin exactly those.)
        assert!(
            !tests.contains(crate::adapters::asset_embed::MERMAID_JS),
            "tests.html carries no Mermaid bundle",
        );
        assert!(
            !tests.contains("mermaid.initialize"),
            "tests.html runs no Mermaid init",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_explore_is_deterministic() {
        let current = three_model_manifest();
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dir_a = tmp_dir("det-a");
        let dir_b = tmp_dir("det-b");
        render_explore(&dir_a, &current, &models, None, &payload, None).expect("first render");
        render_explore(&dir_b, &current, &models, None, &payload, None).expect("second render");
        for page in ["dag.html", "tests.html"] {
            let a = fs::read(dir_a.join(page)).expect("page a");
            let b = fs::read(dir_b.join(page)).expect("page b");
            assert_eq!(a, b, "{page} renders byte-identically");
        }
        let _ = fs::remove_dir_all(&dir_a);
        let _ = fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn render_explore_creates_the_out_dir() {
        let current = three_model_manifest();
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dir = tmp_dir("nested").join("deeper");
        render_explore(&dir, &current, &models, None, &payload, None).expect("creates out-dir");
        assert!(dir.join("dag.html").exists());
        assert!(dir.join("tests.html").exists());
        let _ = fs::remove_dir_all(dir.parent().expect("parent"));
    }

    #[test]
    fn render_explore_empty_manifest_renders_the_empty_state() {
        let current = manifest_of(Vec::new());
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dir = tmp_dir("empty");
        render_explore(&dir, &current, &models, None, &payload, None)
            .expect("empty manifest renders");
        let dag = fs::read_to_string(dir.join("dag.html")).expect("dag.html");
        assert!(
            dag.contains("\"nodes\":[]"),
            "the empty manifest embeds an empty payload (the JS empty-state trigger)",
        );
        let tests = fs::read_to_string(dir.join("tests.html")).expect("tests.html");
        assert!(
            tests.contains("No models in this manifest"),
            "the empty state is explicit, not a blank page: {tests}",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // ----- cte_dags_by_model (cute-dbt#102) ---------------------------

    /// A compiled model whose SQL carries one CTE — the smallest shape
    /// that yields a non-empty [`DagPayload`].
    fn cte_model(id: &str) -> Node {
        model(
            id,
            Some("with src_orders as (select * from db.sch.orders) select * from src_orders"),
            &[],
        )
    }

    #[test]
    fn cte_dags_map_carries_one_entry_per_cte_bearing_model() {
        let current = manifest_of(vec![
            cte_model("model.shop.dim_orders"),
            cte_model("model.shop.stg_orders"),
        ]);
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dags = cte_dags_by_model(&models, &payload);
        assert_eq!(dags.len(), 2, "one CTE DAG per CTE-bearing model");
        let dag = dags
            .get("model.shop.dim_orders")
            .expect("keyed by full model node id");
        let names: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(
            names.contains(&"src_orders"),
            "the CTE node rides the map entry: {names:?}",
        );
        assert!(
            dag.nodes.len() >= 2,
            "the terminal node rides alongside the CTE: {names:?}",
        );
    }

    #[test]
    fn cte_dags_map_skips_uncompiled_and_cteless_models() {
        let current = manifest_of(vec![
            // No WITH clause -> empty graph -> no entry.
            model("model.shop.flat", Some("select 1"), &[]),
            // Uncompiled (dbt parse) -> fail-open, no entry (the JS
            // renders the labeled degraded view off `not_compiled`).
            model("model.shop.parsed_only", None, &[]),
            cte_model("model.shop.dim_orders"),
        ]);
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dags = cte_dags_by_model(&models, &payload);
        assert_eq!(
            dags.keys().collect::<Vec<_>>(),
            vec!["model.shop.dim_orders"],
            "only the CTE-bearing compiled model gets a map entry",
        );
    }

    #[test]
    fn render_explore_dag_embeds_the_cte_carrier_and_view_toggle() {
        let current = manifest_of(vec![cte_model("model.shop.dim_orders")]);
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dir = tmp_dir("cte-toggle");
        render_explore(&dir, &current, &models, None, &payload, None).expect("explore renders");
        let dag = fs::read_to_string(dir.join("dag.html")).expect("dag.html");
        // The carrier embeds the per-model CTE DAG map.
        assert!(dag.contains("\"cte_dags\":{"), "cte_dags carrier present");
        assert!(
            dag.contains("src_orders"),
            "the CTE node id rides the dag.html carrier",
        );
        // The view toggle: lineage arm active, CTE arm gated on a
        // highlight at render time (selection is a runtime act).
        assert!(
            dag.contains("data-view=\"lineage\""),
            "lineage toggle arm present",
        );
        assert!(dag.contains("data-view=\"cte\""), "CTE toggle arm present");
        // The CTE view host renders hidden — lineage is the boot view.
        assert!(
            dag.contains("class=\"cte-view\" hidden"),
            "the CTE view host starts hidden: {dag}",
        );
        // The first-party CTE engine ships on the page.
        assert!(
            dag.contains("cute-dbt explore CTE engine v1"),
            "dag.html embeds the explore CTE engine",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // ----- tests.html shared unit-test card (cute-dbt#102) ------------

    #[test]
    fn render_explore_tests_embeds_the_shared_test_card_and_viewer() {
        let mut current = three_model_manifest();
        let ut = crate::domain::UnitTest::new(
            "test_dim_orders",
            NodeId::new("dim_orders"),
            Vec::new(),
            crate::domain::UnitTestExpect::new(serde_json::Value::Null, None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let mut unit_tests = StdHashMap::new();
        unit_tests.insert("unit_test.shop.dim_orders.test_dim_orders".to_owned(), ut);
        current = Manifest::new(
            ManifestMetadata::new("v12"),
            current.nodes().clone(),
            unit_tests,
            StdHashMap::new(),
        );
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dir = tmp_dir("tests-viewer");
        render_explore(&dir, &current, &models, None, &payload, None).expect("explore renders");
        let tests = fs::read_to_string(dir.join("tests.html")).expect("tests.html");
        // The shared askama partial (report.html's test card) renders.
        assert!(
            tests.contains("class=\"test-section\""),
            "the shared test-card partial renders on tests.html",
        );
        assert!(tests.contains("id=\"test-select\""), "test selector");
        assert!(
            tests.contains("class=\"panel-row\""),
            "the Given/Expected panel pair renders",
        );
        // Each listed test wires its id for the viewer.
        assert!(
            tests.contains("data-test-id=\"unit_test.shop.dim_orders.test_dim_orders\""),
            "the index rows carry data-test-id handles",
        );
        // The first-party viewer engine ships; no graph engine does.
        assert!(
            tests.contains("cute-dbt explore tests viewer v1"),
            "tests.html embeds the explore tests viewer",
        );
        assert!(
            !tests.contains("The Cytoscape Consortium") && !tests.contains("cytoscapeDagre"),
            "tests.html embeds NO Cytoscape assets",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // ----- per-node test-count badges (cute-dbt#103) -------------------

    /// A generic/singular data-test node: `attached` is the fusion
    /// `attached_node` linkage (`None` = the singular-test wire shape —
    /// fusion null-fills it, verified on the real playground fixture),
    /// `deps` the `depends_on.nodes` edges and `path` the declaring
    /// YAML file.
    fn data_test(id: &str, attached: Option<&str>, deps: &[&str], path: Option<&str>) -> Node {
        Node::new(
            NodeId::new(id),
            "test",
            Checksum::new("none", ""),
            None,
            None,
            DependsOn::new(Vec::new(), deps.iter().map(|d| NodeId::new(*d)).collect()),
            path.map(str::to_owned),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(None, attached.map(NodeId::new), None)
    }

    /// A minimal unit test targeting `target_bare` (the manifest stores
    /// the BARE model name — resolution is `resolve_tested_model`).
    fn unit_test_on(target_bare: &str) -> crate::domain::UnitTest {
        crate::domain::UnitTest::new(
            format!("test_{target_bare}"),
            NodeId::new(target_bare),
            Vec::new(),
            crate::domain::UnitTestExpect::new(serde_json::Value::Null, None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
    }

    fn manifest_with_unit_tests(
        nodes: Vec<Node>,
        unit_tests: Vec<(&str, crate::domain::UnitTest)>,
    ) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            unit_tests
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            StdHashMap::new(),
        )
    }

    /// Look one lineage node up by bare name.
    fn lineage_node<'l>(lineage: &'l Lineage, name: &str) -> &'l LineageNode {
        lineage
            .nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("no lineage node named {name:?}"))
    }

    #[test]
    fn lineage_counts_data_tests_by_attached_target_not_declaring_file() {
        // One test ATTACHED to dim_orders but DECLARED in stg_orders'
        // YAML file, plus a relationships-style test attached to
        // dim_orders whose depends_on also reaches stg_orders.
        // Attribution follows `attached_node` (fusion's
        // `_lookup_attached_node` parity) — never the declaring file
        // and never the depends_on complement.
        let current = manifest_of(vec![
            model("model.shop.dim_orders", Some("select 1"), &[]),
            model("model.shop.stg_orders", Some("select 1"), &[]),
            data_test(
                "test.shop.not_null_dim_orders_id",
                Some("model.shop.dim_orders"),
                &["model.shop.dim_orders"],
                Some("models/staging/stg_orders.yml"),
            ),
            data_test(
                "test.shop.relationships_dim_orders_stg",
                Some("model.shop.dim_orders"),
                &["model.shop.stg_orders", "model.shop.dim_orders"],
                None,
            ),
        ]);
        let lineage = build_lineage(&current, &all_models(&current));
        assert_eq!(lineage.nodes.len(), 2, "test nodes are never lineage nodes");
        assert_eq!(lineage_node(&lineage, "dim_orders").data_tests, 2);
        assert_eq!(
            lineage_node(&lineage, "stg_orders").data_tests,
            0,
            "neither the declaring file nor a depends_on reach attributes \
             a data test — only attached_node does",
        );
    }

    #[test]
    fn lineage_counts_unit_tests_by_resolved_bare_target() {
        let current = manifest_with_unit_tests(
            vec![
                model("model.shop.dim_orders", Some("select 1"), &[]),
                model("model.shop.stg_orders", Some("select 1"), &[]),
            ],
            vec![
                ("unit_test.shop.dim_orders.a", unit_test_on("dim_orders")),
                ("unit_test.shop.dim_orders.b", unit_test_on("dim_orders")),
                // Unresolvable bare target — contributes nothing.
                ("unit_test.shop.ghost.c", unit_test_on("ghost_model")),
            ],
        );
        let lineage = build_lineage(&current, &all_models(&current));
        assert_eq!(lineage_node(&lineage, "dim_orders").unit_tests, 2);
        assert_eq!(lineage_node(&lineage, "stg_orders").unit_tests, 0);
    }

    #[test]
    fn lineage_singular_test_without_attached_node_counts_for_no_model() {
        // The real fusion wire shape for singular (SQL-file) tests:
        // `attached_node: null` even though depends_on names the model
        // (20 such nodes on the committed playground fixture).
        let current = manifest_of(vec![
            model("model.shop.dim_orders", Some("select 1"), &[]),
            data_test(
                "test.shop.assert_dim_orders_valid",
                None,
                &["model.shop.dim_orders"],
                Some("tests/assert_dim_orders_valid.sql"),
            ),
        ]);
        let lineage = build_lineage(&current, &all_models(&current));
        assert_eq!(
            lineage_node(&lineage, "dim_orders").data_tests,
            0,
            "a singular test (attached_node: null) is not a YAML data-test",
        );
    }

    #[test]
    fn lineage_zero_test_model_counts_zero_zero() {
        let current = three_model_manifest();
        let lineage = build_lineage(&current, &all_models(&current));
        for node in &lineage.nodes {
            assert_eq!((node.data_tests, node.unit_tests), (0, 0), "{}", node.name);
        }
    }

    #[test]
    fn payload_carries_counts_and_the_preformatted_badge() {
        let current = manifest_with_unit_tests(
            vec![
                model("model.shop.dim_orders", Some("select 1"), &[]),
                model("model.shop.stg_orders", Some("select 1"), &[]),
                data_test(
                    "test.shop.not_null_dim_orders_id",
                    Some("model.shop.dim_orders"),
                    &["model.shop.dim_orders"],
                    None,
                ),
                data_test(
                    "test.shop.unique_dim_orders_id",
                    Some("model.shop.dim_orders"),
                    &["model.shop.dim_orders"],
                    None,
                ),
            ],
            vec![("unit_test.shop.dim_orders.a", unit_test_on("dim_orders"))],
        );
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        let by_name: StdHashMap<&str, &LineageNodePayload> =
            payload.nodes.iter().map(|n| (n.name.as_str(), n)).collect();
        let dim = by_name["dim_orders"];
        assert_eq!((dim.data_tests, dim.unit_tests), (2, 1));
        assert_eq!(
            dim.badge, "2 data-tests · 1 unit-test",
            "the badge is Rust-composed (pluralized) — the JS engine \
             stays a pure renderer",
        );
        let stg = by_name["stg_orders"];
        assert_eq!((stg.data_tests, stg.unit_tests), (0, 0));
        assert_eq!(stg.badge, "0 data-tests · 0 unit-tests");
        // The 0/0 badge is EXPLICIT in the carrier — the counts are
        // never skip-serialized away.
        let json = json_for_html_script(&payload).expect("serializes");
        assert!(json.contains("\"data_tests\":0"), "{json}");
        assert!(json.contains("\"unit_tests\":0"), "{json}");
        assert!(json.contains("0 data-tests · 0 unit-tests"), "{json}");
    }

    #[test]
    fn explore_models_zip_carries_tests_and_badges() {
        let mut current = three_model_manifest();
        // Attach one unit test to dim_orders.
        let ut = crate::domain::UnitTest::new(
            "test_dim_orders",
            NodeId::new("dim_orders"),
            Vec::new(),
            crate::domain::UnitTestExpect::new(serde_json::Value::Null, None, None),
            Some("checks the dim".to_owned()),
            DependsOn::default(),
            None,
            None,
            None,
        );
        let mut unit_tests = StdHashMap::new();
        unit_tests.insert("unit_test.shop.dim_orders.test_dim_orders".to_owned(), ut);
        current = Manifest::new(
            ManifestMetadata::new("v12"),
            current.nodes().clone(),
            unit_tests,
            StdHashMap::new(),
        );
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let pods = explore_models(&current, &models, &payload);
        assert_eq!(pods.len(), 3);
        let dim = pods
            .iter()
            .find(|m| m.name == "dim_orders")
            .expect("dim_orders present");
        assert_eq!(dim.tests.len(), 1);
        assert_eq!(dim.tests[0].name, "test_dim_orders");
        assert_eq!(dim.tests[0].description.as_deref(), Some("checks the dim"));
        assert_eq!(
            dim.tests[0].shape, "0 givens, expects 0 rows",
            "a Null expect tabulates to an empty grid (0 rows)",
        );
        let mart = pods
            .iter()
            .find(|m| m.name == "mart_orders")
            .expect("mart_orders present");
        assert!(mart.not_compiled, "fail-open badge data");
        assert!(mart.tests.is_empty(), "zero-test model still renders");
    }

    // ----- model-detail card payload (cute-dbt#104) ---------------------

    /// A described / tagged / configured model with declared columns —
    /// the full detail-card input shape.
    fn detailed_model(id: &str) -> Node {
        let mut config = BTreeMap::new();
        config.insert("materialized".to_owned(), serde_json::json!("table"));
        config.insert(
            "meta".to_owned(),
            serde_json::json!({
                "grain": "order_id + order_date",
                "owner": "analytics",
                "uses": ["reporting", "billing"],
            }),
        );
        let mut columns: BTreeMap<String, Option<String>> = BTreeMap::new();
        columns.insert("order_id".to_owned(), Some("varchar".to_owned()));
        columns.insert("order_date".to_owned(), None);
        let mut descriptions = BTreeMap::new();
        descriptions.insert("order_id".to_owned(), "Primary key.".to_owned());
        Node::new(
            NodeId::new(id),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            crate::domain::NodeConfig::new(config, false),
            None,
            columns,
        )
        .with_column_descriptions(descriptions)
        .with_model_metadata(
            Some("One row per order.".to_owned()),
            vec!["marts".to_owned(), "core".to_owned()],
        )
    }

    /// A `unique`-class data test attached to `target` (the cute-dbt#104
    /// grain-inference input shape).
    fn unique_test(id: &str, target: &str, column: &str) -> Node {
        data_test(id, Some(target), &[target], None).with_test_attachment(
            Some(column.to_owned()),
            Some(NodeId::new(target)),
            Some(crate::domain::TestMetadata::new(
                "unique",
                None,
                serde_json::json!({ "column_name": column }),
            )),
        )
    }

    #[test]
    fn payload_carries_the_model_detail_facts() {
        let current = manifest_of(vec![detailed_model("model.shop.dim_orders")]);
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        let detail = &payload.nodes[0].detail;
        assert_eq!(detail.description.as_deref(), Some("One row per order."));
        assert_eq!(detail.materialized.as_deref(), Some("table"));
        assert_eq!(detail.tags, vec!["marts", "core"]);
        assert_eq!(
            detail
                .meta
                .iter()
                .map(|m| (m.key.as_str(), m.value.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("grain", "order_id + order_date"),
                ("owner", "analytics"),
                ("uses", "[\"reporting\",\"billing\"]"),
            ],
            "meta entries are key-ordered; strings verbatim, the rest compact JSON",
        );
        assert_eq!(
            detail.columns,
            vec![
                ColumnDetailPayload {
                    name: "order_date".to_owned(),
                    data_type: None,
                    description: None,
                },
                ColumnDetailPayload {
                    name: "order_id".to_owned(),
                    data_type: Some("varchar".to_owned()),
                    description: Some("Primary key.".to_owned()),
                },
            ],
        );
    }

    #[test]
    fn payload_grain_resolves_meta_over_inferred_and_surfaces_all_detected() {
        let current = manifest_of(vec![
            detailed_model("model.shop.dim_orders"),
            unique_test(
                "test.shop.unique_dim_orders",
                "model.shop.dim_orders",
                "order_id",
            ),
        ]);
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        let grain = &payload.nodes[0].detail.grain;
        assert_eq!(grain.value, "order_id + order_date");
        assert_eq!(grain.source, "config.meta.grain");
        assert!(grain.known);
        // ALL detected grains surfaced — the inferred signal rides along.
        assert_eq!(grain.detected.len(), 2, "{grain:?}");
        assert_eq!(grain.detected[0].kind, "config.meta.grain");
        assert_eq!(grain.detected[1].kind, "unique test");
        assert_eq!(grain.detected[1].value, "order_id");
        assert_eq!(grain.detected[1].origin, "test.shop.unique_dim_orders");
    }

    #[test]
    fn payload_grain_infers_from_a_unique_test_without_meta() {
        let current = manifest_of(vec![
            model("model.shop.stg_orders", Some("select 1"), &[]),
            unique_test(
                "test.shop.unique_stg_orders",
                "model.shop.stg_orders",
                "order_id",
            ),
        ]);
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        let grain = &payload.nodes[0].detail.grain;
        assert_eq!(grain.value, "order_id");
        assert_eq!(grain.source, "unique test");
        assert!(grain.known);
    }

    #[test]
    fn payload_grain_is_explicitly_unknown_when_nothing_is_detected() {
        let current = three_model_manifest();
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        for node in &payload.nodes {
            assert_eq!(node.detail.grain.value, "unknown", "{}", node.name);
            assert_eq!(node.detail.grain.source, "unknown");
            assert!(!node.detail.grain.known, "never silently guessed");
            assert!(node.detail.grain.detected.is_empty());
        }
        // The unknown rung is EXPLICIT in the carrier — never an omitted
        // key (the 0/0-badge posture).
        let json = json_for_html_script(&payload).expect("serializes");
        assert!(json.contains("\"value\":\"unknown\""), "{json}");
        assert!(json.contains("\"known\":false"), "{json}");
    }

    // ----- per-node file paths + contract version (cute-dbt#105) --------

    /// A model with the full path complement: SQL source + schema YAML.
    fn pathed_model(id: &str) -> Node {
        Node::new(
            NodeId::new(id),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            Some("models/marts/dim_orders.sql".to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_patch_path(Some("models/marts/_core__models.yml".to_owned()))
    }

    /// A unit test on `target_bare` with a declaring YAML path and
    /// external fixture refs on its givens/expect.
    fn pathed_unit_test(
        name: &str,
        target_bare: &str,
        yaml: Option<&str>,
        given_fixtures: &[&str],
        expect_fixture: Option<&str>,
    ) -> crate::domain::UnitTest {
        let given = given_fixtures
            .iter()
            .map(|f| {
                crate::domain::UnitTestGiven::new(
                    "ref('stg_orders')",
                    serde_json::Value::Null,
                    Some("csv".to_owned()),
                    Some((*f).to_owned()),
                )
            })
            .collect();
        crate::domain::UnitTest::new(
            name,
            NodeId::new(target_bare),
            given,
            crate::domain::UnitTestExpect::new(
                serde_json::Value::Null,
                None,
                expect_fixture.map(str::to_owned),
            ),
            None,
            DependsOn::default(),
            None,
            None,
            yaml.map(str::to_owned),
        )
    }

    #[test]
    fn payload_carries_the_per_node_file_paths() {
        let current = manifest_with_unit_tests(
            vec![pathed_model("model.shop.dim_orders")],
            vec![
                (
                    "unit_test.shop.dim_orders.b_second",
                    pathed_unit_test(
                        "b_second",
                        "dim_orders",
                        Some("models/marts/_core__models.yml"),
                        &[],
                        None,
                    ),
                ),
                (
                    "unit_test.shop.dim_orders.a_first",
                    pathed_unit_test(
                        "a_first",
                        "dim_orders",
                        Some("models/marts/_core__models.yml"),
                        &["tests/fixtures/orders_given.csv", "bare_name_fixture"],
                        Some("tests/fixtures/orders_expected.csv"),
                    ),
                ),
            ],
        );
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        let paths = &payload.nodes[0].paths;
        assert_eq!(paths.sql.as_deref(), Some("models/marts/dim_orders.sql"));
        assert_eq!(
            paths.schema_yaml.as_deref(),
            Some("models/marts/_core__models.yml"),
        );
        // Name-ordered (the manifest map iterates non-deterministically).
        assert_eq!(
            paths
                .unit_tests
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a_first", "b_second"],
        );
        let first = &paths.unit_tests[0];
        assert_eq!(
            first.yaml.as_deref(),
            Some("models/marts/_core__models.yml")
        );
        // given-order, then expect; VERBATIM off the manifest (a bare
        // dbt-core fixture name is carried as-is — hosts apply the
        // documented tests/fixtures/<name>.csv convention).
        assert_eq!(
            first.fixtures,
            vec![
                "tests/fixtures/orders_given.csv",
                "bare_name_fixture",
                "tests/fixtures/orders_expected.csv",
            ],
        );
        assert!(paths.unit_tests[1].fixtures.is_empty());
    }

    #[test]
    fn pathless_model_paths_are_explicit_nulls_never_omitted_keys() {
        let current = three_model_manifest();
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        for node in &payload.nodes {
            assert!(node.paths.sql.is_none(), "{}", node.name);
            assert!(node.paths.schema_yaml.is_none(), "{}", node.name);
            assert!(node.paths.unit_tests.is_empty(), "{}", node.name);
        }
        // The explicit-absence posture in the carrier: null/[] keys,
        // never omitted.
        let json = json_for_html_script(&payload).expect("serializes");
        assert!(json.contains("\"paths\":{"), "{json}");
        assert!(json.contains("\"sql\":null"), "{json}");
        assert!(json.contains("\"schema_yaml\":null"), "{json}");
        assert!(json.contains("\"unit_tests\":[]"), "{json}");
    }

    #[test]
    fn unresolvable_unit_test_target_contributes_no_paths_entry() {
        let current = manifest_with_unit_tests(
            vec![pathed_model("model.shop.dim_orders")],
            vec![(
                "unit_test.shop.ghost.g",
                pathed_unit_test("g", "ghost_model", Some("models/g.yml"), &[], None),
            )],
        );
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        assert!(
            payload.nodes[0].paths.unit_tests.is_empty(),
            "an unresolvable model: reference is skipped, not failed (fail-open)",
        );
    }

    #[test]
    fn render_explore_dag_carries_the_contract_version_attribute() {
        let current = three_model_manifest();
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dir = tmp_dir("contract-version");
        render_explore(&dir, &current, &models, None, &payload, None).expect("explore renders");
        let dag = fs::read_to_string(dir.join("dag.html")).expect("dag.html");
        assert!(
            dag.contains(&format!(
                "<body data-cute-dbt-contract=\"{EXPLORE_CONTRACT_VERSION}\">"
            )),
            "the contract version is server-rendered on <body> \
             (attribute-only observers read it without executing JS)",
        );
        // tests.html is NOT a contract surface — the attribute is
        // dag.html's (the drivable page).
        let tests = fs::read_to_string(dir.join("tests.html")).expect("tests.html");
        assert!(
            !tests.contains("data-cute-dbt-contract"),
            "tests.html carries no contract attribute",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detail_of_an_undetailed_model_is_empty_but_explicit() {
        let current = three_model_manifest();
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        let detail = &payload.nodes[0].detail;
        assert_eq!(detail.description, None);
        assert_eq!(detail.materialized, None);
        assert!(detail.tags.is_empty());
        assert!(detail.meta.is_empty());
        assert!(detail.columns.is_empty());
        // Every detail key serializes — absence is null/[], never an
        // omitted key.
        let json = json_for_html_script(&payload).expect("serializes");
        assert!(json.contains("\"description\":null"), "{json}");
        assert!(json.contains("\"materialized\":null"), "{json}");
    }

    // ----- PR-diff change context (cute-dbt#106) ------------------------

    /// A compiled model with an `original_file_path` (the change-context
    /// matching key).
    fn model_at(id: &str, ofp: &str) -> Node {
        Node::new(
            NodeId::new(id),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            Some(ofp.to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    fn changed_set(ids: &[&str]) -> ModelInScopeSet {
        ids.iter().map(|s| NodeId::new(*s)).collect()
    }

    #[test]
    fn payload_marks_changed_nodes_and_never_narrows_the_node_set() {
        let current = manifest_of(vec![
            model_at("model.shop.dim_orders", "models/marts/dim_orders.sql"),
            model_at("model.shop.stg_orders", "models/staging/stg_orders.sql"),
        ]);
        let changed = changed_set(&["model.shop.dim_orders"]);
        let payload = build_lineage_payload(&current, &all_models(&current), Some(&changed));
        // The FULL graph renders — context never narrows scope.
        assert_eq!(payload.nodes.len(), 2);
        let by_name: StdHashMap<&str, &LineageNodePayload> =
            payload.nodes.iter().map(|n| (n.name.as_str(), n)).collect();
        assert!(by_name["dim_orders"].changed);
        assert!(!by_name["stg_orders"].changed);
        // Carrier shape: the marked node serializes `"changed":true`;
        // the unmarked node serializes NO `changed` key (never false).
        let json = json_for_html_script(&payload).expect("serializes");
        assert_eq!(json.matches("\"changed\":true").count(), 1, "{json}");
        assert!(!json.contains("\"changed\":false"), "{json}");
    }

    #[test]
    fn no_context_payload_carries_no_changed_keys_at_all() {
        // The byte-stability guarantee for the committed explore goldens
        // (rendered WITHOUT --pr-diff): a no-context payload is
        // byte-identical to the pre-#106 shape — zero `changed` keys.
        let current = three_model_manifest();
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        assert!(payload.nodes.iter().all(|n| !n.changed));
        let json = json_for_html_script(&payload).expect("serializes");
        assert!(
            !json.contains("\"changed\""),
            "the no-context carrier must omit the key entirely: {json}",
        );
    }

    #[test]
    fn render_explore_with_context_renders_the_count_and_legend_chip() {
        let current = manifest_of(vec![
            model_at("model.shop.dim_orders", "models/marts/dim_orders.sql"),
            model_at("model.shop.stg_orders", "models/staging/stg_orders.sql"),
        ]);
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let changed = changed_set(&["model.shop.dim_orders"]);
        let dir = tmp_dir("change-context");
        render_explore(&dir, &current, &models, Some(&changed), &payload, None)
            .expect("explore renders");
        let dag = fs::read_to_string(dir.join("dag.html")).expect("dag.html");
        assert!(
            dag.contains("1 changed in this diff"),
            "the header counts the changed models",
        );
        assert!(
            dag.contains("<span class=\"legend-chip changed\">changed</span>"),
            "the legend explains the changed treatment",
        );
        assert!(
            dag.contains("the full graph always renders every model"),
            "the legend states the never-narrows contract",
        );
        assert!(
            dag.contains("\"changed\":true"),
            "the carrier marks the changed node",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_explore_with_context_but_zero_changed_is_honest() {
        // --pr-diff given, but the diff touched no model files: context
        // exists (Some(empty)), so the banner renders the honest 0.
        let current = manifest_of(vec![model_at(
            "model.shop.dim_orders",
            "models/marts/dim_orders.sql",
        )]);
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let empty = ModelInScopeSet::new();
        let dir = tmp_dir("change-context-zero");
        render_explore(&dir, &current, &models, Some(&empty), &payload, None)
            .expect("explore renders");
        let dag = fs::read_to_string(dir.join("dag.html")).expect("dag.html");
        assert!(dag.contains("0 changed in this diff"), "honest zero");
        assert!(
            !dag.contains("\"changed\":true"),
            "no node marks changed in the carrier",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // ----- build_macro_lineage_payload (cute-dbt#345 Slice 3) --------
    //
    // The focused macro DAG restricts the lineage to `users ∪ downstream`
    // and stamps each node's `macro_role`. `build_macro_lineage_payload`
    // takes the already-computed `MacroFocusSet` (the domain seam) — it
    // never calls a domain walker, so these adapter tests construct the
    // focus set by hand and assert the focused subgraph + roles.

    /// A small lineage: stg -> dim -> mart, plus an unrelated `other`
    /// node that must NOT appear in a focus restricted to stg/dim/mart.
    fn focus_chain_manifest() -> Manifest {
        manifest_of(vec![
            model("model.shop.stg_orders", Some("select 1"), &[]),
            model(
                "model.shop.dim_orders",
                Some("select 1"),
                &["model.shop.stg_orders"],
            ),
            model(
                "model.shop.mart_orders",
                Some("select 1"),
                &["model.shop.dim_orders"],
            ),
            // Outside the focus set — proves the restriction excludes it.
            model("model.shop.other", Some("select 1"), &[]),
        ])
    }

    fn focus_of(users: &[&str], downstream: &[&str]) -> MacroFocusSet {
        MacroFocusSet {
            users: users.iter().map(|s| NodeId::new(*s)).collect(),
            downstream: downstream.iter().map(|s| NodeId::new(*s)).collect(),
        }
    }

    #[test]
    fn macro_payload_restricts_nodes_to_the_focus_set() {
        let current = focus_chain_manifest();
        // stg is the macro caller; dim + mart are its downstream.
        let focus = focus_of(
            &["model.shop.stg_orders"],
            &["model.shop.dim_orders", "model.shop.mart_orders"],
        );
        let payload = build_macro_lineage_payload(&current, &focus);
        let ids: Vec<&str> = payload.nodes.iter().map(|n| n.id.as_str()).collect();
        // Exactly the focus union, id-ordered — `other` is excluded even
        // though `build_typed_node_map` would otherwise pull the whole
        // manifest's non-model nodes in.
        assert_eq!(
            ids,
            vec![
                "model.shop.dim_orders",
                "model.shop.mart_orders",
                "model.shop.stg_orders",
            ],
        );
        assert!(
            !ids.contains(&"model.shop.other"),
            "a node outside the focus set must not render: {ids:?}",
        );
    }

    #[test]
    fn macro_payload_stamps_user_and_downstream_roles() {
        let current = focus_chain_manifest();
        let focus = focus_of(
            &["model.shop.stg_orders"],
            &["model.shop.dim_orders", "model.shop.mart_orders"],
        );
        let payload = build_macro_lineage_payload(&current, &focus);
        let role_of = |id: &str| {
            payload
                .nodes
                .iter()
                .find(|n| n.id == id)
                .and_then(|n| n.macro_role)
        };
        assert_eq!(role_of("model.shop.stg_orders"), Some(MacroRole::User));
        assert_eq!(
            role_of("model.shop.dim_orders"),
            Some(MacroRole::Downstream),
        );
        assert_eq!(
            role_of("model.shop.mart_orders"),
            Some(MacroRole::Downstream),
        );
    }

    #[test]
    fn macro_payload_edges_are_the_induced_subgraph() {
        let current = focus_chain_manifest();
        let focus = focus_of(
            &["model.shop.stg_orders"],
            &["model.shop.dim_orders", "model.shop.mart_orders"],
        );
        let payload = build_macro_lineage_payload(&current, &focus);
        // stg -> dim -> mart, all inside the focus; the `other` node's
        // (absent) edges never appear.
        assert_eq!(
            payload.edges,
            vec![
                LineageEdgePayload {
                    from: "model.shop.dim_orders".to_owned(),
                    to: "model.shop.mart_orders".to_owned(),
                },
                LineageEdgePayload {
                    from: "model.shop.stg_orders".to_owned(),
                    to: "model.shop.dim_orders".to_owned(),
                },
            ],
        );
    }

    #[test]
    fn macro_role_serializes_to_user_and_downstream_strings() {
        let current = focus_chain_manifest();
        let focus = focus_of(&["model.shop.stg_orders"], &["model.shop.dim_orders"]);
        let payload = build_macro_lineage_payload(&current, &focus);
        let json = json_for_html_script(&payload).expect("serializes");
        let round: serde_json::Value = serde_json::from_str(&json).expect("round-trips");
        let role = |id: &str| {
            round["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["id"] == id)
                .and_then(|n| n["macro_role"].as_str())
                .map(str::to_owned)
        };
        assert_eq!(role("model.shop.stg_orders").as_deref(), Some("user"));
        assert_eq!(role("model.shop.dim_orders").as_deref(), Some("downstream"));
    }

    #[test]
    fn non_macro_payload_carries_no_macro_role_keys() {
        // The byte-stability guard for the committed dag.html golden: the
        // full-manifest lineage payload (no focus) serializes ZERO
        // `macro_role` keys — the serde-skip gate keeps the pre-#345 shape.
        let current = three_model_manifest();
        let payload = build_lineage_payload(&current, &all_models(&current), None);
        assert!(payload.nodes.iter().all(|n| n.macro_role.is_none()));
        let json = json_for_html_script(&payload).expect("serializes");
        assert!(
            !json.contains("macro_role"),
            "the non-macro carrier must omit the key entirely: {json}",
        );
    }

    #[test]
    fn render_explore_with_macro_focus_writes_a_non_empty_macro_page() {
        let current = focus_chain_manifest();
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let focus = focus_of(
            &["model.shop.stg_orders"],
            &["model.shop.dim_orders", "model.shop.mart_orders"],
        );
        let dir = tmp_dir("macro-focus");
        render_explore(&dir, &current, &models, None, &payload, Some(&focus))
            .expect("explore renders");
        let macro_html = fs::read_to_string(dir.join("macro.html")).expect("macro.html written");
        // The focused carrier rides macro.html and carries the role keys.
        assert!(
            macro_html.contains("explore-dag-data"),
            "macro.html embeds the focused lineage carrier",
        );
        assert!(
            macro_html.contains("\"macro_role\":\"user\""),
            "the carrier marks the macro caller: {macro_html}",
        );
        assert!(
            macro_html.contains("\"macro_role\":\"downstream\""),
            "the carrier marks the downstream nodes",
        );
        // The dag.html carrier stays role-free (the serde-skip gate) —
        // the literal `n.macro_role` appears in the embedded engine SOURCE,
        // so the guard targets the SERIALIZED KEY (`"macro_role":`), which
        // only a focused carrier emits.
        let dag = fs::read_to_string(dir.join("dag.html")).expect("dag.html");
        assert!(
            !dag.contains("\"macro_role\":"),
            "the full-manifest dag.html carrier carries no macro_role keys",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_explore_without_macro_focus_writes_no_macro_page() {
        let current = three_model_manifest();
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dir = tmp_dir("no-macro-focus");
        render_explore(&dir, &current, &models, None, &payload, None).expect("explore renders");
        assert!(
            !dir.join("macro.html").exists(),
            "the no-focus path must not emit macro.html",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn macro_page_downstream_count_excludes_non_vertex_closure_nodes() {
        // qodo #4 — the displayed downstream count must reflect what
        // RENDERS, not the domain closure. The `MacroFocusSet` closure
        // (correctly) crosses `test` nodes, but `focused_typed_node_map`
        // keeps only lineage vertices — so a `focus.downstream.len()`
        // count would over-claim. The page must count the rendered
        // `Downstream`-role nodes.
        //
        // stg (user, model) -> dim (downstream, model) -> a `test` node
        // consuming dim. The test is in the closure (so `focus.downstream`
        // = {dim, test}, len 2) but is NOT a lineage vertex — only `dim`
        // renders, so the page must say "1 downstream node", not 2.
        let current = manifest_of(vec![
            model("model.shop.stg", Some("select 1"), &[]),
            model("model.shop.dim", Some("select 1"), &["model.shop.stg"]),
            data_test(
                "test.shop.assert_dim",
                Some("model.shop.dim"),
                &["model.shop.dim"],
                Some("tests/assert_dim.sql"),
            ),
        ]);
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        // The focus a real run would produce: users = {stg}, downstream =
        // the full closure minus users = {dim, test.shop.assert_dim}.
        let focus = MacroFocusSet {
            users: [NodeId::new("model.shop.stg")].into_iter().collect(),
            downstream: [
                NodeId::new("model.shop.dim"),
                NodeId::new("test.shop.assert_dim"),
            ]
            .into_iter()
            .collect(),
        };
        // Precondition: the domain closure includes the non-vertex test —
        // the over-claim source if counted directly.
        assert_eq!(focus.downstream.len(), 2, "closure includes the test node");

        let dir = tmp_dir("macro-count-vertex");
        render_explore(&dir, &current, &models, None, &payload, Some(&focus))
            .expect("explore renders");
        let macro_html = fs::read_to_string(dir.join("macro.html")).expect("macro.html");

        // The rendered focused DAG carries exactly 2 vertices (stg + dim);
        // the test node is not a vertex.
        let dom = tl::parse(&macro_html, tl::ParserOptions::default()).expect("parse");
        let parser = dom.parser();
        let carrier = dom
            .get_element_by_id("explore-dag-data")
            .expect("carrier present")
            .get(parser)
            .expect("resolves")
            .inner_text(parser);
        let payload_json: serde_json::Value = serde_json::from_str(&carrier).expect("carrier JSON");
        assert_eq!(
            payload_json["nodes"].as_array().map_or(0, Vec::len),
            2,
            "only the two model vertices render — the test is not a vertex",
        );

        // The displayed count reflects the RENDERED downstream vertices
        // (1: dim), never the domain closure size (2). The over-claim
        // would read "2 downstream nodes".
        assert!(
            macro_html.contains("1 calling model, 1 downstream node, 1 edge"),
            "the header must count rendered vertices, not the closure \
             (which includes the non-rendering test node): {}",
            macro_html
                .lines()
                .find(|l| l.contains("explore-counts"))
                .unwrap_or("<no counts line>"),
        );
        assert!(
            !macro_html.contains("2 downstream node"),
            "the count must NOT over-claim the test node as a downstream vertex",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_explore_without_context_renders_no_change_chrome() {
        // The no-flag page: no legend chip, no header clause — the
        // committed explore goldens keep rendering this shape. (The
        // literal "changed in this diff" still appears INSIDE the
        // embedded engine source — the oracles below target the
        // server-rendered chrome, not the JS string literals.)
        let current = three_model_manifest();
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dir = tmp_dir("no-change-context");
        render_explore(&dir, &current, &models, None, &payload, None).expect("explore renders");
        let dag = fs::read_to_string(dir.join("dag.html")).expect("dag.html");
        assert!(
            !dag.contains("<span class=\"legend-chip changed\">"),
            "no change-context legend chip without --pr-diff",
        );
        assert!(
            !dag.contains("changed in this diff</p>"),
            "no header clause without --pr-diff",
        );
        assert!(
            !dag.contains("\"changed\":"),
            "no changed keys in the no-context carrier",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // ===== lineage_vertex_type (cute-dbt#404 extraction) =====

    #[test]
    fn lineage_vertex_type_maps_each_vertex_resource_type() {
        assert_eq!(lineage_vertex_type("model"), Some(LineageNodeType::Model));
        assert_eq!(
            lineage_vertex_type("snapshot"),
            Some(LineageNodeType::Snapshot)
        );
        assert_eq!(lineage_vertex_type("seed"), Some(LineageNodeType::Seed));
    }

    #[test]
    fn lineage_vertex_type_skips_non_vertex_resource_types() {
        // `test`/`operation`/anything else the focus closure reached is not a
        // lineage vertex — the silent-skip arm. `source`/`exposure` are keyed
        // off their own manifest maps, not `nodes()`, so they are `None` here.
        assert_eq!(lineage_vertex_type("test"), None);
        assert_eq!(lineage_vertex_type("unit_test"), None);
        assert_eq!(lineage_vertex_type("operation"), None);
        assert_eq!(lineage_vertex_type("source"), None);
        assert_eq!(lineage_vertex_type("exposure"), None);
        assert_eq!(lineage_vertex_type(""), None);
    }
}
