//! Scope-selection (cute-dbt#81 — Shape E Phase 1).
//!
//! Defines the in-scope set the run loop (PR-pdiff-1b) renders, from one
//! of two sources (resolved at CLI parse time):
//!
//! - [`ScopeInput::Baseline`] — the v0.1 `--baseline-manifest` path.
//!   Delegates to [`StateComparator::from_selectors`] — the always-on
//!   body checksum plus any opt-in `state:modified` sub-selectors
//!   (cute-dbt#160); with no sub-selectors the existing dbt
//!   `state:modified.body` semantics flow through unchanged.
//! - [`ScopeInput::PrDiff`] — the `--pr-diff` path (cute-dbt#85 renamed
//!   from `--scope-from-pr-diff` at cute-dbt#96). Carries a
//!   [`NormalizedDiffIndex`] built once from the parsed
//!   `git diff --unified=0`; the index is the single normalization
//!   authority that matches changed-file paths against
//!   [`crate::domain::manifest::Node::original_file_path`] and
//!   [`crate::domain::unit_test::UnitTest::original_file_path`].
//!
//! Two scope sources is a deliberate ADR-1 judgment call: free function
//! over trait until a third source arrives (a v0.2+ refactor moment).
//!
//! Path normalization lives in the [`crate::domain::path`] leaf and is
//! owned end-to-end by [`NormalizedDiffIndex`] (module DAG
//! `scope → pr_diff → path`) — `scope` no longer normalizes paths
//! directly, so the diff-side keyset and the declaring-side lookup
//! cannot diverge. Git-detected renames (cute-dbt#80) are handled inside
//! the index: both sides of every `rename from`/`rename to` pair join
//! the changed-file keyset, so a **pure** rename (which carries no
//! `+++` header and no hunks) still scopes the current node at its new
//! path — no scope-level code is rename-aware.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::domain::manifest::{Manifest, Node, NodeId};
use crate::domain::pr_diff::NormalizedDiffIndex;
use crate::domain::project_def::ConfigAttribution;
use crate::domain::state::{
    InScopeSet, ModelInScopeSet, ModifierKind, SeedInScopeSet, StateComparator,
    resolve_tested_model,
};

/// Source of the in-scope set: either a baseline manifest (dbt
/// `state:modified` semantics) or a parsed PR diff (CI/PR-review path).
#[derive(Debug, Clone)]
pub enum ScopeInput {
    /// Compare against a baseline manifest — v0.1 default, ADR-2 +
    /// ADR-3 semantics unchanged.
    Baseline {
        /// Already-parsed baseline manifest (Stage-1 pre-flight ran in
        /// the adapter). Boxed (`clippy::large_enum_variant`): a parsed
        /// `Manifest` is hundreds of bytes inline and grows with every
        /// ingestion wave (cute-dbt#256 added exposures/groups maps);
        /// the `PrDiff` arm is ~48 bytes — boxing keeps the enum small
        /// where it is moved through the run loop.
        manifest: Box<Manifest>,
        /// Opt-in `state:modified` sub-selector kinds composed alongside
        /// the always-on body checksum (cute-dbt#160 — the CLI
        /// `--modified-selectors` wiring). Empty is the body-only v0.1
        /// default, byte-identical to the pre-flag behavior.
        sub_selectors: Vec<ModifierKind>,
    },
    /// Scope to nodes whose `original_file_path` appears in the PR's
    /// parsed diff. CI/PR-review path — no baseline needed.
    PrDiff {
        /// The single normalization authority, built once from the
        /// parsed `git diff --unified=0` and the `--project-root` strip.
        /// Owns the changed-file keyset (diff-side, strip-applied) and
        /// the per-file hunks (consumed by cute-dbt#96's block-precise
        /// `changed` refinement and inline YAML diff). Typically the
        /// diff includes non-dbt files (README, workflow YAML,
        /// `dbt_project.yml`) which silently miss.
        index: NormalizedDiffIndex,
    },
}

/// The change-axes that fired for one in-scope model (cute-dbt#411) —
/// which of dbt's `state:modified` sub-selectors this PR touched, rolled
/// up to the model level for the Models lens.
///
/// Three axes in v0.1:
/// - **`body`** — the model's `.sql` (`original_file_path`) is in the diff.
/// - **`config`** — the model's `schema.yml` (its [`Node::patch_path`]) is
///   in the diff. **This is the load-bearing axis the bug fix adds**: today
///   `patch_path` is never a scope signal, so a config-only change vanishes.
///   Scoped strictly to the model's `schema.yml`; the `dbt_project.yml`
///   config-tree (cute-dbt#267) and `var` references (cute-dbt#268) keep
///   their own provenance chips and never set this bit.
/// - **`unit_test`** — the model hosts ≥1 in-scope unit test.
///
/// **Field-extensible by design (ADR-3/ADR-5 additive-never-rewrite).**
/// The future axes are dbt's remaining `state:modified` sub-selectors —
/// **contract**, **relation**, **macros**. Adding one is a new `bool`
/// field here plus a new detection arm in `select_in_scope_pr_diff`; no
/// comparator/scope/render rewrite. (A contract-only change lives in the
/// same `schema.yml` as `config`, so it is already caught by the `config`
/// axis today — the model is never dropped; labeling it distinctly is the
/// later additive block-level YAML attribution.) Render iterates the axes
/// generically, so a new field flows through without a render branch.
///
/// [`Node::patch_path`]: crate::domain::manifest::Node::patch_path
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChangeAxes {
    /// The model's `.sql` (`original_file_path`) is in the diff.
    pub body: bool,
    /// The model's `schema.yml` (`patch_path`) is in the diff.
    pub config: bool,
    /// The model hosts ≥1 in-scope unit test.
    pub unit_test: bool,
}

impl ChangeAxes {
    /// `true` when any axis fired (the "is this model attributed at all"
    /// guard). Under the lean-(b) encoding every `models_in_scope` member
    /// gets an `axes` entry, so a member can carry all-false (in scope only
    /// via a sibling in-scope test on the same model resolving it) — this
    /// guard distinguishes that case.
    #[must_use]
    pub fn any(&self) -> bool {
        self.body || self.config || self.unit_test
    }
}

/// The mutually-exclusive top-level lifecycle state of an in-scope model in
/// `--pr-diff` mode (cute-dbt#416) — the PR-scope state taxonomy the
/// 3-axis [`ChangeAxes`] attribution (cute-dbt#411) sits *underneath*.
///
/// Exactly one state per in-scope model:
/// - **`New`** — the PR adds the model's `.sql`/`.py` (its
///   `original_file_path` is in the diff's `added` keyset). Supersedes
///   `Modified` even when the added file also carries body hunks: a brand-new
///   file is NEW, not MODIFIED.
/// - **`Modified`** — an existing model (not in `added`) whose body/config/
///   unit-test axes fired. The default state and the cute-dbt#411 path.
/// - **`Removed`** — the PR deletes a model path that names no current node
///   (it's gone). REMOVED models are **node-less** (no current manifest
///   entry), so they are carried as a separate `removed_models` path list,
///   never an in-scope `Node` / a `model_states` entry.
///
/// `New`/`Modified` attach to a real in-scope model node (via
/// [`ScopeSelection::model_states`]); `Removed` has no node, so it never
/// appears in that map — its paths live in
/// [`ScopeSelection::removed_models`]. The variant exists on the enum for
/// vocabulary completeness (render reuses the same chip family) and so a
/// future baseline-arm parity slice can attribute a removed node a state.
///
/// The **baseline arm** produces no model states (the documented gap, the
/// `ChangeAxes` Option-A precedent): the always-on body checksum can't tell
/// add-vs-modify, and deletion ghosts there are the separate `pr_dag`
/// baseline−current set-diff. PR-diff is the primary CI/PR-review path, so
/// it owns the NEW/REMOVED signal here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelState {
    /// A model the PR **adds** (its source file is in the diff's `added`).
    New,
    /// An existing model the PR **modifies** (the cute-dbt#411 axes path).
    Modified,
    /// A model the PR **removes** — a node-less ghost (carried only as a
    /// path in [`ScopeSelection::removed_models`], never in
    /// [`ScopeSelection::model_states`]).
    Removed,
}

/// The resolved scope selection: the in-scope unit tests, the in-scope
/// models, and the **changed** (PR-updated) subset of the in-scope tests.
///
/// `changed` is the per-test "this PR updated this test" signal the report
/// foregrounds (cute-dbt#91). It is a strict subset of `in_scope`
/// (`changed ⊆ in_scope`) by construction in both arms:
///
/// - **`Baseline`** — `changed` is [`StateComparator::changed_unit_tests`]
///   (the precise `UnitTest` struct diff); a changed test is always in
///   scope via the `target_modified || test_changed` union. The changed
///   subset is modifier-independent: opt-in sub-selectors widen
///   `in_scope`, never `changed`.
/// - **`PrDiff`** — `changed` is the tests whose declaring YAML file
///   appears in the diff (file-granular here; cute-dbt#96 refines it to
///   block-precise as a post-scope run-loop narrowing). Collected in the
///   same traversal as `in_scope`, so the subset relation cannot drift.
///
/// Additive POD (ADR-5): the existing `InScopeSet` / `ModelInScopeSet`
/// types and their semantics are unchanged — this struct only *surfaces*
/// the label both arms already compute.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopeSelection {
    /// Unit-test ids the report renders (selection semantics unchanged).
    pub in_scope: InScopeSet,
    /// Model node ids the report renders (explorer-mode union, unchanged).
    pub models_in_scope: ModelInScopeSet,
    /// The subset of `in_scope` whose definition this diff updated — the
    /// report's "updated" tests (the rest are "context").
    pub changed: InScopeSet,
    /// Per-model change-axis attribution (cute-dbt#411) — which of
    /// `{body, config, unit_test}` fired for each in-scope model. Additive
    /// POD (ADR-5), the cute-dbt#91 `changed` precedent generalized from a
    /// set-membership bit to a map-to-record. `BTreeMap` for deterministic
    /// iteration (every other scope set here is `BTree`-backed for golden
    /// stability).
    ///
    /// **Invariant:** `axes.keys() ⊆ models_in_scope`. Under the lean-(b)
    /// encoding the `PrDiff` arm populates an entry for *every*
    /// `models_in_scope` member, so `axes.keys() == models_in_scope`
    /// exactly — a model in scope only via an in-scope test (no own body /
    /// config change) carries an all-false-but-`unit_test` (or all-false)
    /// `ChangeAxes` rather than being absent, killing the "absent means
    /// false" ambiguity for the render lookup.
    ///
    /// **Documented baseline gap (Option A, cute-dbt#411):** the
    /// `Baseline` arm produces an **empty** map. A config-only change is
    /// byte-invisible to the always-on SHA-256 `BodyChecksumModifier`
    /// (the body checksum is unchanged), so a baseline `config` axis could
    /// only ever come from a default-on `.configs` `StateModifier` — which
    /// is opt-in/off-by-default today. Baseline-side per-axis attribution
    /// awaits that future parity slice (an un-collapse of
    /// `StateComparator::modified_set`'s `.any()`); `state.rs` is
    /// deliberately untouched here. Pinned by `baseline_arm_produces_empty_axes`.
    pub axes: BTreeMap<NodeId, ChangeAxes>,
    /// Per-in-scope-model mutually-exclusive top-level state (cute-dbt#416)
    /// — [`ModelState::New`] or [`ModelState::Modified`] for each model node
    /// with a current manifest entry. Additive POD (ADR-5), the
    /// `axes` precedent (a parallel `BTreeMap<NodeId, _>`).
    ///
    /// **Invariant:** `model_states.keys() == axes.keys() == models_in_scope`
    /// on the `PrDiff` arm — every in-scope model node carries exactly one
    /// state. NEW supersedes MODIFIED (a model in `added` is `New` even when
    /// it also has body hunks). [`ModelState::Removed`] never appears here —
    /// removed models are node-less, carried in `removed_models`. The
    /// `Baseline` arm produces an **empty** map (the `axes` Option-A gap).
    pub model_states: BTreeMap<NodeId, ModelState>,
    /// The model **paths** the PR deletes (cute-dbt#416) — REMOVED models.
    /// Node-less by nature (no current manifest entry), so they cannot be
    /// `Node`s / `model_states` entries; they are diff `deleted` paths
    /// inferred to be model paths (`.sql`/`.py` under the dbt-default
    /// `models/` prefix) that resolve to no current node. Sorted for
    /// deterministic golden output. Empty on the `Baseline` arm (deletion
    /// ghosts there are the separate `pr_dag` baseline−current set-diff).
    pub removed_models: Vec<String>,
}

/// Resolve the [`ScopeSelection`] for the current manifest and the given
/// [`ScopeInput`].
///
/// - [`ScopeInput::Baseline`] delegates to
///   [`StateComparator::from_selectors`] (the body checksum plus any
///   opt-in `sub_selectors` — empty is the body-only default) for the
///   in-scope/model sets and to
///   [`StateComparator::changed_unit_tests`] for the changed subset.
/// - [`ScopeInput::PrDiff`] matches changed-file paths against
///   `original_file_path` via the [`NormalizedDiffIndex`], collecting the
///   in-scope and changed sets in one pass. The `PrDiff` arm never
///   constructs a [`StateComparator`], so sub-selectors are structurally
///   meaningless here — the CLI rejects `--modified-selectors` with
///   `--pr-diff` at parse time (cute-dbt#160).
#[must_use]
pub fn select_in_scope(current: &Manifest, input: &ScopeInput) -> ScopeSelection {
    match input {
        ScopeInput::Baseline {
            manifest: baseline,
            sub_selectors,
        } => {
            let cmp = StateComparator::from_selectors(sub_selectors);
            ScopeSelection {
                in_scope: cmp.in_scope_unit_tests(current, baseline),
                models_in_scope: cmp.models_in_scope(current, baseline),
                changed: StateComparator::changed_unit_tests(current, baseline),
                // Option A (cute-dbt#411): the baseline arm produces no
                // per-axis attribution — `state.rs` is untouched and a
                // config-only change is invisible to the body checksum.
                // See the `ScopeSelection::axes` documented-gap note.
                axes: BTreeMap::new(),
                // cute-dbt#416: the baseline arm produces no NEW/REMOVED
                // model states either — the same documented gap. PR-diff
                // owns the add/remove signal; the baseline-arm deletion
                // ghosts are the separate pr_dag set-diff.
                model_states: BTreeMap::new(),
                removed_models: Vec::new(),
            }
        }
        ScopeInput::PrDiff { index } => select_in_scope_pr_diff(current, index),
    }
}

/// The full-manifest model scope (cute-dbt#100 — the `explore` verb's
/// `all_models` seam): every `model` node in the manifest, no baseline,
/// no diff.
///
/// Non-model resource types (`test`, `seed`, `snapshot`, …) are
/// excluded — the same `resource_type == "model"` filter both
/// diff-scoping arms apply (cute-dbt#167), so a generic test node can
/// never surface as a model card on the explore pages either.
/// Compiled-ness is deliberately **not** consulted here: explore is
/// fail-open on uncompiled models (they render as "not compiled"), so
/// the seam returns them like any other model. The returned
/// [`ModelInScopeSet`] iterates in deterministic node-id order
/// (`BTreeSet`).
#[must_use]
pub fn all_models(current: &Manifest) -> ModelInScopeSet {
    current
        .nodes()
        .iter()
        .filter(|(_, node)| node.resource_type() == "model")
        .map(|(id, _)| id.clone())
        .collect()
}

/// Every `seed` node in the manifest — the `explore` verb's full-manifest
/// seed seam (cute-dbt#398), the seed twin of [`all_models`].
///
/// The explorer is full-manifest with no scope source, so its seed-node
/// detail cards must show data for **every** seed, not just the modified
/// subset [`select_seeds_in_scope`] returns for the report's diff-scoped
/// "Data tables" section. This projection feeds
/// [`build_seed_cards`](crate::domain::build_seed_cards) → the CLI gather
/// stage → the explorer's `seed_tables` side-map. Non-`seed` resource types
/// are excluded (the seed mirror of the cute-dbt#167 model-only filter). The
/// returned [`SeedInScopeSet`] iterates in deterministic node-id order
/// (`BTreeSet`).
#[must_use]
pub fn all_seeds(current: &Manifest) -> SeedInScopeSet {
    current
        .nodes()
        .iter()
        .filter(|(_, node)| node.resource_type() == "seed")
        .map(|(id, _)| id.clone())
        .collect()
}

/// The models whose source files a PR diff changed — the `explore`
/// verb's **change-context** seam (cute-dbt#106).
///
/// Matches each `model` node's `original_file_path` against the
/// [`NormalizedDiffIndex`] changed-file keyset — the exact cute-dbt#81
/// matching the report's `PrDiff` arm applies (the private
/// `select_in_scope_pr_diff` consumes this same function, so the two
/// verbs cannot disagree about which models a diff touched). Git renames
/// (cute-dbt#80) are handled inside the index: both sides of every
/// rename pair join the keyset, so a **pure** rename still marks the
/// current node at its new path. Non-`model` resource types never mark
/// (the cute-dbt#167 filter).
///
/// Change context **never narrows scope**: explore renders the full
/// manifest regardless; this set only decorates the changed nodes with
/// the "changed" context treatment.
#[must_use]
pub fn changed_models(current: &Manifest, index: &NormalizedDiffIndex) -> ModelInScopeSet {
    current
        .nodes()
        .iter()
        .filter(|(_, node)| node.resource_type() == "model")
        .filter(|(_, node)| {
            node.original_file_path()
                .is_some_and(|ofp| index.contains_changed(ofp))
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// Resolve the in-scope **seed** set for the current manifest and the
/// given [`ScopeInput`] (cute-dbt#350) — the seed sibling of
/// [`select_in_scope`].
///
/// - [`ScopeInput::Baseline`] delegates to
///   [`StateComparator::seeds_in_scope`] (the always-on body checksum plus
///   any opt-in `state:modified` sub-selectors, exactly as the model arm
///   composes its comparator) — a seed is in scope when its `checksum`
///   changed against the baseline.
/// - [`ScopeInput::PrDiff`] delegates to the private `changed_seeds` — a
///   seed is in scope when its `original_file_path` (the `seeds/<name>.csv`
///   the diff
///   edited) appears in the [`NormalizedDiffIndex`] changed-file keyset,
///   matched through the index's single normalization authority (the same
///   path-matching both [`changed_models`] and the report's `PrDiff` arm
///   apply).
///
/// Additive sibling of [`select_in_scope`]: the model selection and the
/// `resource_type == "model"` filter (cute-dbt#167) are untouched — this is
/// a parallel projection, never a rewrite. The output feeds
/// [`build_seed_cards`](crate::domain::build_seed_cards) → the CLI gather
/// stage → the render payload.
#[must_use]
pub fn select_seeds_in_scope(current: &Manifest, input: &ScopeInput) -> SeedInScopeSet {
    match input {
        ScopeInput::Baseline {
            manifest: baseline,
            sub_selectors,
        } => StateComparator::from_selectors(sub_selectors).seeds_in_scope(current, baseline),
        ScopeInput::PrDiff { index } => changed_seeds(current, index),
    }
}

/// The seeds whose CSV file a PR diff changed — the `PrDiff` arm of
/// [`select_seeds_in_scope`] (cute-dbt#350).
///
/// The seed dual of [`changed_models`]: match each `seed` node's
/// `original_file_path` against the [`NormalizedDiffIndex`] changed-file
/// keyset. Seeds are graph **roots** (no targeting unit tests, no upstream
/// `depends_on`), so a seed is in scope precisely when its own source file
/// was edited — there is no second arm to union. The index owns the
/// changed-file keyset (including both sides of every git-rename pair), so
/// this consults it rather than normalizing paths here (the single
/// normalization authority [`changed_models`] also respects). Non-`seed`
/// resource types never match (the `resource_type == "seed"` filter, the
/// seed mirror of the cute-dbt#167 model-only filter).
#[must_use]
fn changed_seeds(current: &Manifest, index: &NormalizedDiffIndex) -> SeedInScopeSet {
    current
        .nodes()
        .iter()
        .filter(|(_, node)| node.resource_type() == "seed")
        .filter(|(_, node)| {
            node.original_file_path()
                .is_some_and(|ofp| index.contains_changed(ofp))
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// Config-tree scope widening (cute-dbt#267 — the one widening category
/// of epic #262, by-DEFINITION change): union the models a
/// `dbt_project.yml` config-tree edit selected
/// ([`crate::domain::project_def::attribute_config_tree_changes`] — fqn
/// prefix descent, fusion's own resolution) into the selection.
///
/// Union semantics, never replacement:
///
/// - every attributed model joins `models_in_scope` (it renders a model
///   card with its provenance chip);
/// - every unit test whose resolved target model was widened joins
///   `in_scope` as **context** — the same `target_modified ⇒ test
///   in-scope` OR-arm both existing scope sources apply;
/// - `changed` is untouched (a config-tree edit updates no test
///   definition), and since the function only ever ADDS to `in_scope`,
///   `changed ⊆ in_scope` is preserved by construction.
///
/// The attribution map is keyed by model node id
/// ([`attribute_config_tree_changes`] already filtered to `model` nodes
/// of the same manifest); an id that no longer resolves is skipped
/// (belt-and-braces). An empty map returns the selection unchanged —
/// baseline mode and panel-degrade arms widen nothing.
///
/// **Does NOT populate `selection.axes` (cute-dbt#411, locked Q4):** the
/// `config` change-axis is the model's `schema.yml` (`patch_path`) ONLY.
/// The `dbt_project.yml` config-tree this function widens on is a distinct
/// 4th provenance with its own `config_attribution` chip row — folding it
/// into the `config` axis would make that chip ambiguous across
/// schema.yml-config vs dbt_project.yml-config-tree vs vars (cute-dbt#268).
/// The widened models join `models_in_scope`; their `axes` entries (if
/// they have one from the `PrDiff` arm) are left exactly as the `PrDiff`
/// arm set them, and a config-tree-only model gains no `axes` entry here.
///
/// [`attribute_config_tree_changes`]: crate::domain::project_def::attribute_config_tree_changes
#[must_use]
pub fn widen_with_config_attributions(
    mut selection: ScopeSelection,
    current: &Manifest,
    attributions: &BTreeMap<String, Vec<ConfigAttribution>>,
) -> ScopeSelection {
    if attributions.is_empty() {
        return selection;
    }
    // One NodeId allocation per attributed key, reused for both the
    // manifest existence probe and the membership set below. A fully
    // borrowed `&str` keyset would not save it: `Manifest::node` keys on
    // `&NodeId` (no `Borrow<str>` bridge on the node map), so the probe
    // needs the owned id either way.
    let widened: BTreeSet<NodeId> = attributions
        .keys()
        .map(|id| NodeId::new(id.clone()))
        .filter(|id| current.node(id).is_some())
        .collect();
    // In-place union over the by-value selection — pre-existing members
    // are never re-cloned; the widened tests stream straight into the
    // set. `changed` is untouched.
    selection.in_scope.extend(
        current
            .unit_tests()
            .iter()
            .filter(|(_, ut)| {
                resolve_tested_model(current, ut).is_some_and(|model| widened.contains(model.id()))
            })
            .map(|(id, _)| id.clone()),
    );
    // cute-dbt#416: a config-tree-widened model is an existing node edited
    // via `dbt_project.yml` — a MODIFIED model. Give each one a state entry
    // (unless the `PrDiff` arm already attributed it NEW/MODIFIED) so the
    // `model_states.keys() == models_in_scope` invariant survives the
    // widening. A config-tree edit can never make a model NEW (the model
    // node already exists in the current manifest), so `Modified` is the
    // correct default. The `Baseline` arm passes empty `model_states`, so
    // this is inert there (`config_attributions` is project-state-gated and
    // never set on the baseline path).
    for id in &widened {
        selection
            .model_states
            .entry(id.clone())
            .or_insert(ModelState::Modified);
    }
    selection.models_in_scope.extend(widened);
    selection
}

// ---------------------------------------------------------------------
// PrDiff arm
// ---------------------------------------------------------------------

fn select_in_scope_pr_diff(current: &Manifest, index: &NormalizedDiffIndex) -> ScopeSelection {
    // Identify path-modified models — the PrDiff analog of the baseline
    // `modified_set`. Only `model` nodes participate (other resource
    // types do not host unit tests in v0.1). The index owns the
    // changed-file keyset, so this consults it rather than normalizing
    // paths here (single normalization authority). Shared with the
    // explore verb's change context (cute-dbt#106) via [`changed_models`]
    // — one matching authority for both verbs.
    let path_modified_models = changed_models(current, index);

    // Identify config-modified models (cute-dbt#411 — THE BUG FIX): every
    // `model` node whose `schema.yml` (its `patch_path`) is in the diff
    // keyset. Before this, `patch_path` was never a scope signal (0 refs
    // in scope.rs/state.rs), so a model whose ONLY change is its
    // `schema.yml` `config:` block vanished from the report. Matched
    // through the SAME `index.contains_changed` authority as the body/test
    // arms, so the config-side and diff-side keysets cannot diverge
    // (the `--project-root` strip is applied uniformly diff-side at index
    // construction; the manifest-side `patch_path` is already
    // package-relative scheme-stripped at ingestion — `manifest.rs`). One
    // `schema.yml` fans out to EVERY model it patches (file-granular).
    // Maps id → patch_path string (the value is the grouping key the
    // render layer reads off `Node::patch_path()` directly — kept here
    // only for the `config` axis membership probe).
    let config_modified_models: BTreeMap<NodeId, String> = current
        .nodes()
        .iter()
        .filter(|(_, node)| node.resource_type() == "model")
        .filter_map(|(id, node)| {
            node.patch_path()
                .filter(|p| index.contains_changed(p))
                .map(|p| (id.clone(), p.to_owned()))
        })
        .collect();

    // In-scope unit tests + the changed subset, in ONE traversal so
    // `changed ⊆ in_scope` holds by construction (cute-dbt#91). A test is
    // in scope when its target model is path-modified OR config-modified
    // (a config-only `schema.yml` edit pulls the model's tests in as
    // context — the dbt OR-semantics, the cute-dbt#267 shape) OR its own
    // `original_file_path` (the declaring YAML file) is in the change set.
    // It is *changed* only when that declaring YAML appears in the diff —
    // file-granular here (cute-dbt#96 narrows it to block-precise in a
    // post-scope run-loop step, leaving `changed ⊆ in_scope` intact). A
    // config-only model edit updates no test definition, so the new
    // `target_config_modified` disjunct never touches `changed` —
    // `changed ⊆ in_scope` is preserved (the cute-dbt#267 invariant).
    let mut in_scope_ids: Vec<String> = Vec::new();
    let mut changed_ids: Vec<String> = Vec::new();
    for (test_id, ut) in current.unit_tests() {
        let target_model = resolve_tested_model(current, ut);
        let target_path_modified =
            target_model.is_some_and(|model| path_modified_models.contains(model.id()));
        let target_config_modified =
            target_model.is_some_and(|model| config_modified_models.contains_key(model.id()));
        let test_yaml_changed = ut
            .original_file_path()
            .is_some_and(|p| index.contains_changed(p));
        if test_yaml_changed {
            changed_ids.push(test_id.clone());
        }
        if target_path_modified || target_config_modified || test_yaml_changed {
            in_scope_ids.push(test_id.clone());
        }
    }
    let in_scope: InScopeSet = in_scope_ids.into_iter().collect();
    let changed: InScopeSet = changed_ids.into_iter().collect();

    // Models in scope — explorer-mode union:
    //   Arm 1: every model resolved from an in-scope unit test (so the
    //          renderer has the model context for the test).
    //   Arm 2: every path-modified model with zero unit tests targeting
    //          it (the "no tests wired" explorer signal).
    //   Arm 3: every config-modified model with zero unit tests targeting
    //          it (cute-dbt#411 — the silent-drop fix for the no-tests
    //          path; the with-tests case enters via Arm 1 once its tests
    //          join `in_scope` through the `target_config_modified`
    //          disjunct above). Mirrors Arm 2 exactly.
    let tests_per_model: HashMap<NodeId, usize> = current
        .unit_tests()
        .values()
        .filter_map(|ut| resolve_tested_model(current, ut).map(|m| m.id().clone()))
        .fold(HashMap::new(), |mut acc, id| {
            *acc.entry(id).or_insert(0) += 1;
            acc
        });
    let has_tests = |model_id: &NodeId| tests_per_model.get(model_id).copied().unwrap_or(0) > 0;

    // Arm 1 is also the `unit_test`-axis reverse rollup (model → "hosts ≥1
    // in-scope test"): one pass over `in_scope` seeds both `model_ids` and
    // `models_with_in_scope_test` (compute-once — the cute-dbt#167/#485
    // discipline).
    let mut models_with_in_scope_test: BTreeSet<NodeId> = BTreeSet::new();
    for test_id in in_scope.iter() {
        if let Some(ut) = current.unit_test(test_id)
            && let Some(model) = resolve_tested_model(current, ut)
        {
            models_with_in_scope_test.insert(model.id().clone());
        }
    }
    let mut model_ids: BTreeSet<NodeId> = models_with_in_scope_test.clone();
    for model_id in path_modified_models.iter() {
        if !has_tests(model_id) {
            model_ids.insert(model_id.clone());
        }
    }
    for model_id in config_modified_models.keys() {
        if !has_tests(model_id) {
            model_ids.insert(model_id.clone());
        }
    }

    // Per-model axis assembly (cute-dbt#411). Under the lean-(b) encoding
    // EVERY `models_in_scope` member gets an `axes` entry
    // (`axes.keys() == models_in_scope`), so a model in scope only via a
    // test carries `unit_test: true` with body/config false rather than
    // being absent.
    let mut axes: BTreeMap<NodeId, ChangeAxes> = BTreeMap::new();
    // Per-model top-level state (cute-dbt#416), in lockstep with `axes`
    // (`model_states.keys() == axes.keys() == models_in_scope`). A model
    // whose `original_file_path` is in the diff's `added` keyset is NEW
    // (supersedes MODIFIED even with body hunks); every other in-scope
    // model node is MODIFIED. REMOVED is node-less and handled separately
    // below — it never enters this map.
    let mut model_states: BTreeMap<NodeId, ModelState> = BTreeMap::new();
    for model_id in &model_ids {
        axes.insert(
            model_id.clone(),
            ChangeAxes {
                body: path_modified_models.contains(model_id),
                config: config_modified_models.contains_key(model_id),
                unit_test: models_with_in_scope_test.contains(model_id),
            },
        );
        let is_new = current
            .node(model_id)
            .and_then(Node::original_file_path)
            .is_some_and(|ofp| index.contains_added(ofp));
        model_states.insert(
            model_id.clone(),
            if is_new {
                ModelState::New
            } else {
                ModelState::Modified
            },
        );
    }

    let removed_models = removed_model_paths(current, index);

    let models_in_scope: ModelInScopeSet = model_ids.into_iter().collect();
    ScopeSelection {
        in_scope,
        models_in_scope,
        changed,
        axes,
        model_states,
        removed_models,
    }
}

/// The dbt-default model directory prefix (cute-dbt#416). Used by the
/// REMOVED model-path heuristic when no ingested `model-paths` config is
/// threadable into the scope path. dbt's own default is `model-paths:
/// ["models"]` (dbt-core `dbt_project.yml` default), so a deleted
/// `.sql`/`.py` under `models/` is overwhelmingly a model deletion.
const DEFAULT_MODEL_PATH_PREFIX: &str = "models/";

/// The model file extensions cute-dbt treats as a model definition
/// (cute-dbt#416): SQL models and Python models. A deleted file with any
/// other extension (a `schema.yml`, a `.csv` seed, a `.md` doc) is never a
/// REMOVED **model**.
const MODEL_FILE_EXTENSIONS: [&str; 2] = [".sql", ".py"];

/// Infer the **REMOVED** model paths from a diff's `deleted` keyset
/// (cute-dbt#416).
///
/// A REMOVED model is node-less — the PR deleted it, so it has no current
/// manifest node to anchor an `original_file_path` or a `resource_type`
/// check. With **no baseline manifest** in `--pr-diff` mode, "is this a
/// model?" can only be inferred from the deleted path itself. The
/// heuristic, documented and deliberately conservative:
///
/// 1. the path ends in `.sql` or `.py` (a model definition extension), and
/// 2. the path is under the dbt-default `models/` prefix (the
///    [`DEFAULT_MODEL_PATH_PREFIX`]), and
/// 3. the path names **no current node** (it really is gone — a path still
///    present as a node is a modify/rename, not a removal).
///
/// The `models/`-prefix fallback is used because the ingested
/// `model-paths` config (cute-dbt#262/#270 `ProjectDefinition`) is gated
/// behind the project-state experiment and is **not** threaded into the
/// core scope path; the dbt default covers the overwhelming majority of
/// projects. A custom-`model-paths` project under-reports REMOVED models
/// (conservative: a false negative drops a chip, never a false claim) —
/// threading the config here is a clean additive follow-up.
///
/// The deleted paths are already strip-normalized (the index applied the
/// `--project-root` strip at construction), so the `models/` prefix test
/// is project-relative, matching the manifest's `original_file_path`
/// scheme. The result is sorted for deterministic golden output.
fn removed_model_paths(current: &Manifest, index: &NormalizedDiffIndex) -> Vec<String> {
    let mut removed: Vec<String> = index
        .deleted_paths()
        .filter(|path| is_model_path(path))
        .filter(|path| !path_names_a_current_node(current, path))
        .map(str::to_owned)
        .collect();
    removed.sort_unstable();
    removed.dedup();
    removed
}

/// Whether `path` looks like a dbt model definition file (cute-dbt#416) —
/// a `.sql`/`.py` under the dbt-default `models/` prefix. See
/// [`removed_model_paths`] for why this heuristic (no baseline manifest in
/// pr-diff mode).
fn is_model_path(path: &str) -> bool {
    path.starts_with(DEFAULT_MODEL_PATH_PREFIX)
        && MODEL_FILE_EXTENSIONS.iter().any(|ext| path.ends_with(ext))
}

/// Whether `path` is the `original_file_path` of some current model node
/// (cute-dbt#416). A deleted path that still resolves to a node is a
/// modify/rename, NOT a removal — only genuinely node-less deletions are
/// REMOVED models.
fn path_names_a_current_node(current: &Manifest, path: &str) -> bool {
    current
        .nodes()
        .iter()
        .filter(|(_, node)| node.resource_type() == "model")
        .any(|(_, node)| node.original_file_path() == Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{Checksum, DependsOn, ManifestMetadata, Node, NodeConfig};
    use crate::domain::pr_diff::{FileHunks, Hunk, PrDiff};
    use crate::domain::unit_test::{UnitTest, UnitTestExpect};
    use std::collections::{BTreeMap, HashMap};
    use std::path::Path;

    // ----- PrDiff test builders -----

    /// Build a file-granular [`PrDiff`] from changed-file paths (one
    /// minimal hunk each — block precision is exercised separately by the
    /// `pr_diff` overlap tests; here only the changed-file keyset matters).
    fn prdiff_from_paths(paths: &[&str]) -> PrDiff {
        PrDiff {
            renames: Vec::new(),
            deleted: Vec::new(),
            added: Vec::new(),
            files: paths
                .iter()
                .map(|p| FileHunks {
                    path: (*p).to_owned(),
                    hunks: vec![Hunk {
                        new_start: 1,
                        new_len: 1,
                        removed_lines: Vec::new(),
                        added_lines: Vec::new(),
                    }],
                })
                .collect(),
        }
    }

    /// A [`ScopeInput::PrDiff`] wrapping the index built from `paths` and
    /// an optional project-root strip.
    fn pr_diff_input(paths: &[&str], strip: Option<&Path>) -> ScopeInput {
        ScopeInput::PrDiff {
            index: NormalizedDiffIndex::new(&prdiff_from_paths(paths), strip),
        }
    }

    // ----- select_in_scope: Baseline arm -----

    #[test]
    fn baseline_arm_matches_state_comparator_body_only() {
        // Two-model manifest: one modified (checksum diff), one unchanged.
        let modified_id = NodeId::new("model.shop.dim_payers");
        let unchanged_id = NodeId::new("model.shop.stg_customers");
        let mut current_nodes = HashMap::new();
        current_nodes.insert(
            modified_id.clone(),
            model_node(&modified_id, "ck-current", None),
        );
        current_nodes.insert(
            unchanged_id.clone(),
            model_node(&unchanged_id, "ck-same", None),
        );

        let mut baseline_nodes = HashMap::new();
        baseline_nodes.insert(
            modified_id.clone(),
            model_node(&modified_id, "ck-baseline", None),
        );
        baseline_nodes.insert(
            unchanged_id.clone(),
            model_node(&unchanged_id, "ck-same", None),
        );

        let test_id = "unit_test.shop.dim_payers.injects_unknown";
        let mut current_tests = HashMap::new();
        current_tests.insert(
            test_id.to_owned(),
            test_for("injects_unknown", "dim_payers"),
        );

        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            current_nodes,
            current_tests,
            HashMap::new(),
        );
        let baseline = Manifest::new(
            ManifestMetadata::new("v12"),
            baseline_nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = ScopeInput::Baseline {
            manifest: Box::new(baseline),
            sub_selectors: Vec::new(),
        };
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert!(in_scope.contains(test_id));
        assert!(models.contains(&modified_id));
        assert!(!models.contains(&unchanged_id));
    }

    #[test]
    fn baseline_arm_without_sub_selectors_keeps_a_config_only_change_out_of_scope() {
        // The byte-identical default (cute-dbt#160): no sub-selectors
        // opted in ⇒ a config-only change (identical body checksum) stays
        // out of scope, exactly as before the flag existed.
        let (current, baseline) = config_only_pair();
        let input = ScopeInput::Baseline {
            manifest: Box::new(baseline),
            sub_selectors: Vec::new(),
        };
        let selection = select_in_scope(&current, &input);
        assert!(selection.in_scope.is_empty());
        assert!(selection.models_in_scope.is_empty());
    }

    #[test]
    fn baseline_arm_with_configs_sub_selector_scopes_a_config_only_change() {
        // The opt-in widening (cute-dbt#160): the SAME config-only change
        // is in scope once `.configs` is composed into the comparator.
        let (current, baseline) = config_only_pair();
        let input = ScopeInput::Baseline {
            manifest: Box::new(baseline),
            sub_selectors: vec![ModifierKind::Configs],
        };
        let selection = select_in_scope(&current, &input);
        assert!(
            selection
                .in_scope
                .contains("unit_test.shop.dim_payers.injects_unknown"),
        );
        assert!(
            selection
                .models_in_scope
                .contains(&NodeId::new("model.shop.dim_payers")),
        );
        // The test definition itself is unchanged — sub-selectors widen
        // `in_scope`, never the `changed` subset (it stays a precise
        // UnitTest struct diff).
        assert!(selection.changed.is_empty());
    }

    /// A current/baseline pair where `dim_payers` differs ONLY in its
    /// resolved config (`materialized: table` vs `view`) — identical
    /// body checksum — and carries one unit test (identical in both).
    fn config_only_pair() -> (Manifest, Manifest) {
        let id = NodeId::new("model.shop.dim_payers");
        let test_id = "unit_test.shop.dim_payers.injects_unknown";

        let node_with = |materialized: &str| {
            let config: BTreeMap<String, serde_json::Value> = [(
                "materialized".to_owned(),
                serde_json::Value::from(materialized),
            )]
            .into_iter()
            .collect();
            Node::new(
                id.clone(),
                "model",
                checksum("ck-same"),
                Some("select 1".to_owned()),
                None,
                DependsOn::default(),
                None,
                NodeConfig::new(config, false),
                None,
                BTreeMap::new(),
            )
        };

        let manifest_with = |materialized: &str| {
            let mut nodes = HashMap::new();
            nodes.insert(id.clone(), node_with(materialized));
            let mut tests = HashMap::new();
            tests.insert(
                test_id.to_owned(),
                test_for("injects_unknown", "dim_payers"),
            );
            Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new())
        };

        (manifest_with("table"), manifest_with("view"))
    }

    #[test]
    fn baseline_arm_excludes_a_new_non_model_node_from_model_scope() {
        // Regression (cute-dbt#167, observed live on PR #166): a newly
        // added generic test node is `state:modified` (absent from the
        // baseline) and has zero unit tests targeting it, but it must NOT
        // surface as a model card in baseline mode.
        let stg_orders = NodeId::new("model.shop.stg_orders");
        let generic_test = NodeId::new("test.shop.not_null_stg_orders_id");

        let mut current_nodes = HashMap::new();
        current_nodes.insert(stg_orders.clone(), model_node(&stg_orders, "ck-same", None));
        current_nodes.insert(
            generic_test.clone(),
            typed_node(&generic_test, "test", "ck-new"),
        );

        let mut baseline_nodes = HashMap::new();
        baseline_nodes.insert(stg_orders.clone(), model_node(&stg_orders, "ck-same", None));

        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            current_nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let baseline = Manifest::new(
            ManifestMetadata::new("v12"),
            baseline_nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = ScopeInput::Baseline {
            manifest: Box::new(baseline),
            sub_selectors: Vec::new(),
        };
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert_eq!(in_scope.len(), 0);
        assert!(
            !models.contains(&generic_test),
            "a modified non-model node must not render as a model card",
        );
        assert_eq!(models.len(), 0);
    }

    // ----- select_in_scope: PrDiff arm -----

    #[test]
    fn pr_diff_arm_puts_modified_model_and_its_test_in_scope() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let stg_customers = NodeId::new("model.shop.stg_customers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        nodes.insert(
            stg_customers.clone(),
            model_node_with_path(&stg_customers, "ck2", "models/staging/stg_customers.sql"),
        );

        let test_id = "unit_test.shop.dim_payers.injects_unknown";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_for("injects_unknown", "dim_payers"),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input(&["models/marts/dim_payers.sql"], None);
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert!(in_scope.contains(test_id));
        assert!(models.contains(&dim_payers));
        assert!(!models.contains(&stg_customers));
    }

    #[test]
    fn pr_diff_arm_silently_skips_extraneous_paths() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(
            &[
                "README.md",
                ".github/workflows/ci.yml",
                "packages.yml",
                "dbt_project.yml",
                "models/deleted_model.sql",
            ],
            None,
        );
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert_eq!(in_scope.len(), 0);
        assert_eq!(models.len(), 0);
    }

    #[test]
    fn pr_diff_arm_picks_up_changed_unit_test_yaml() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );

        let test_id = "unit_test.shop.dim_payers.injects_unknown";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_with_path(
                "injects_unknown",
                "dim_payers",
                Some("models/marts/_core__models.yml"),
            ),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        // Only the YAML file changed — model SQL untouched.
        let input = pr_diff_input(&["models/marts/_core__models.yml"], None);
        let ScopeSelection { in_scope, .. } = select_in_scope(&current, &input);

        assert!(in_scope.contains(test_id));
    }

    #[test]
    fn pr_diff_arm_explorer_mode_for_modified_model_with_no_tests() {
        let stg_payments = NodeId::new("model.shop.stg_payments");
        let mut nodes = HashMap::new();
        nodes.insert(
            stg_payments.clone(),
            model_node_with_path(&stg_payments, "ck1", "models/staging/stg_payments.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(&["models/staging/stg_payments.sql"], None);
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert_eq!(in_scope.len(), 0);
        assert!(models.contains(&stg_payments));
    }

    #[test]
    fn pr_diff_arm_excludes_a_changed_non_model_node_from_model_scope() {
        // The PrDiff analog of the cute-dbt#167 baseline gap: a generic
        // test node whose declaring SQL file is in the diff must not
        // surface as a model card. Pins the existing `resource_type ==
        // "model"` filter in `select_in_scope_pr_diff` so the two arms
        // cannot drift apart.
        let generic_test = NodeId::new("test.shop.assert_positive_total");
        let mut nodes = HashMap::new();
        nodes.insert(
            generic_test.clone(),
            typed_node_with_path(
                &generic_test,
                "test",
                "ck1",
                "tests/assert_positive_total.sql",
            ),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(&["tests/assert_positive_total.sql"], None);
        let ScopeSelection {
            in_scope,
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert_eq!(in_scope.len(), 0);
        assert!(
            !models.contains(&generic_test),
            "a path-changed non-model node must not render as a model card",
        );
        assert_eq!(models.len(), 0);
    }

    // ----- select_in_scope: git renames (cute-dbt#80) -----

    /// A [`ScopeInput::PrDiff`] for a diff carrying rename pairs (and
    /// optionally plain changed files).
    fn pr_diff_input_with_renames(
        paths: &[&str],
        renames: &[(&str, &str)],
        strip: Option<&Path>,
    ) -> ScopeInput {
        let mut diff = prdiff_from_paths(paths);
        diff.renames = renames
            .iter()
            .map(|(f, t)| crate::domain::pr_diff::RenamePair {
                from: (*f).to_owned(),
                to: (*t).to_owned(),
            })
            .collect();
        ScopeInput::PrDiff {
            index: NormalizedDiffIndex::new(&diff, strip),
        }
    }

    /// A `PrDiff` scope input carrying `files` (the changed-file keyset),
    /// `added`, and `deleted` paths — the NEW/REMOVED state-attribution
    /// fixture builder (cute-dbt#416). `added` paths ALSO seed a `files`
    /// entry (a real addition is a changed file with `+`-only hunks), so a
    /// model in `added` is also path-modified, mirroring real git output.
    fn pr_diff_input_with_added_deleted(
        added: &[&str],
        deleted: &[&str],
        strip: Option<&Path>,
    ) -> ScopeInput {
        // An addition is also a changed file (its `+++ b/<path>` opens a
        // `files` entry); seed both so the body axis can fire for a NEW
        // model exactly as the parser would produce.
        let mut diff = prdiff_from_paths(added);
        diff.added = added.iter().map(|p| (*p).to_owned()).collect();
        diff.deleted = deleted.iter().map(|p| (*p).to_owned()).collect();
        ScopeInput::PrDiff {
            index: NormalizedDiffIndex::new(&diff, strip),
        }
    }

    #[test]
    fn pr_diff_arm_pure_rename_scopes_the_renamed_model_at_its_new_path() {
        // models/marts/dim_a.sql → models/marts/dim_b.sql, 100% similar:
        // the diff carries ONLY the rename pair (no file entry). The
        // current manifest (compiled at head) has the node at the NEW
        // path; it must scope, and its unit test is in scope as context
        // (the test's declaring YAML is untouched → not `changed`).
        let dim_b = NodeId::new("model.shop.dim_b");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_b.clone(),
            model_node_with_path(&dim_b, "ck1", "models/marts/dim_b.sql"),
        );

        let test_id = "unit_test.shop.dim_b.checks_rows";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_with_path("checks_rows", "dim_b", Some("models/marts/_models.yml")),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input_with_renames(
            &[],
            &[("models/marts/dim_a.sql", "models/marts/dim_b.sql")],
            None,
        );
        let selection = select_in_scope(&current, &input);

        assert!(
            selection.models_in_scope.contains(&dim_b),
            "the renamed model scopes under its NEW path",
        );
        assert!(selection.in_scope.contains(test_id));
        assert!(
            !selection.changed.contains(test_id),
            "a pure model rename does not mark the test's YAML changed",
        );
    }

    #[test]
    fn pr_diff_arm_rename_with_edit_scopes_the_model_once_not_twice() {
        // Rename + edit: the new path appears BOTH as a file entry (with
        // hunks) and as the rename `to`. The model must scope exactly
        // once, and nothing extra may enter the scope sets.
        let dim_b = NodeId::new("model.shop.dim_b");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_b.clone(),
            model_node_with_path(&dim_b, "ck1", "models/marts/dim_b.sql"),
        );

        let test_id = "unit_test.shop.dim_b.checks_rows";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_with_path("checks_rows", "dim_b", Some("models/marts/_models.yml")),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input_with_renames(
            &["models/marts/dim_b.sql"],
            &[("models/marts/dim_a.sql", "models/marts/dim_b.sql")],
            None,
        );
        let selection = select_in_scope(&current, &input);

        assert_eq!(
            selection.models_in_scope.len(),
            1,
            "the renamed-and-edited model scopes once, not twice",
        );
        assert!(selection.models_in_scope.contains(&dim_b));
        assert_eq!(selection.in_scope.len(), 1);
        assert!(selection.in_scope.contains(test_id));
    }

    #[test]
    fn pr_diff_arm_rename_old_path_matching_no_current_node_is_inert() {
        // The rename's old path maps to no current-manifest node (the
        // node moved). It must scope nothing — no phantom models, no
        // phantom tests.
        let unrelated = NodeId::new("model.shop.stg_x");
        let mut nodes = HashMap::new();
        nodes.insert(
            unrelated.clone(),
            model_node_with_path(&unrelated, "ck1", "models/staging/stg_x.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        // Neither rename side exists in the manifest (e.g. a non-dbt file
        // was renamed, or the manifest predates the new path).
        let input =
            pr_diff_input_with_renames(&[], &[("docs/old_readme.md", "docs/new_readme.md")], None);
        let selection = select_in_scope(&current, &input);

        assert_eq!(selection.in_scope.len(), 0);
        assert_eq!(selection.models_in_scope.len(), 0);
    }

    #[test]
    fn pr_diff_arm_pure_rename_of_declaring_yaml_marks_its_tests_in_scope() {
        // A purely renamed unit-test YAML: the test's current
        // original_file_path is the NEW path, which is in the rename
        // keyset → in scope AND file-granular `changed` (the post-scope
        // block-precise refinement then narrows it to context, since a
        // pure rename carries zero hunks — existing #96 machinery).
        let dim = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim.clone(),
            model_node_with_path(&dim, "ck1", "models/marts/dim_payers.sql"),
        );

        let test_id = "unit_test.shop.dim_payers.checks_rows";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_with_path(
                "checks_rows",
                "dim_payers",
                Some("models/marts/_renamed__models.yml"),
            ),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input_with_renames(
            &[],
            &[(
                "models/marts/_old__models.yml",
                "models/marts/_renamed__models.yml",
            )],
            None,
        );
        let selection = select_in_scope(&current, &input);

        assert!(selection.in_scope.contains(test_id));
        assert!(
            selection.changed.contains(test_id),
            "file-granular changed at scope level (refinement narrows later)",
        );
    }

    #[test]
    fn pr_diff_arm_rename_honors_project_root_strip_on_both_sides() {
        let dim_b = NodeId::new("model.shop.dim_b");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_b.clone(),
            model_node_with_path(&dim_b, "ck1", "models/marts/dim_b.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input_with_renames(
            &[],
            &[(
                "dbt_project/models/marts/dim_a.sql",
                "dbt_project/models/marts/dim_b.sql",
            )],
            Some(Path::new("dbt_project")),
        );
        let selection = select_in_scope(&current, &input);

        assert!(selection.models_in_scope.contains(&dim_b));
    }

    #[test]
    fn pr_diff_arm_honors_project_root_strip() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(
            &["dbt_project/models/marts/dim_payers.sql"],
            Some(Path::new("dbt_project")),
        );
        let ScopeSelection {
            models_in_scope: models,
            ..
        } = select_in_scope(&current, &input);

        assert!(models.contains(&dim_payers));
    }

    // ----- select_in_scope: changed subset (cute-dbt#91) -----

    #[test]
    fn pr_diff_arm_changed_is_subset_and_distinguishes_updated_from_context() {
        // The load-bearing invariant for the PrDiff arm: `changed` is a
        // strict subset of `in_scope`, and it distinguishes updated tests
        // from context tests.
        //   - dim_payers.sql changed → its test (declaring YAML untouched)
        //     is in scope via target_path_modified, but NOT changed →
        //     context.
        //   - _changed.yml changed → stg_x's test is in scope AND changed
        //     (its declaring YAML is in the diff) even though stg_x.sql is
        //     untouched → updated.
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let stg_x = NodeId::new("model.shop.stg_x");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        nodes.insert(
            stg_x.clone(),
            model_node_with_path(&stg_x, "ck2", "models/staging/stg_x.sql"),
        );

        let ctx_id = "unit_test.shop.test_ctx";
        let upd_id = "unit_test.shop.test_upd";
        let mut tests = HashMap::new();
        tests.insert(
            ctx_id.to_owned(),
            test_with_path(
                "test_ctx",
                "dim_payers",
                Some("models/marts/_unchanged.yml"),
            ),
        );
        tests.insert(
            upd_id.to_owned(),
            test_with_path("test_upd", "stg_x", Some("models/marts/_changed.yml")),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input(
            &["models/marts/dim_payers.sql", "models/marts/_changed.yml"],
            None,
        );
        let selection = select_in_scope(&current, &input);

        // changed ⊆ in_scope — by construction (single traversal).
        for id in selection.changed.iter() {
            assert!(
                selection.in_scope.contains(id),
                "changed id {id:?} must be in scope (changed ⊆ in_scope)",
            );
        }
        assert!(selection.in_scope.contains(ctx_id));
        assert!(selection.in_scope.contains(upd_id));
        assert!(
            selection.changed.contains(upd_id),
            "test_upd is updated (its declaring YAML is in the diff)",
        );
        assert!(
            !selection.changed.contains(ctx_id),
            "test_ctx is context (in scope via its model's SQL, YAML unchanged)",
        );
    }

    // ----- select_in_scope: 3-axis change attribution (cute-dbt#411) -----

    /// A model node with both an `original_file_path` (the `.sql`) and a
    /// `patch_path` (the `schema.yml` patching it) — the config-axis test
    /// input. Builds on `model_node_with_path` + the `with_patch_path`
    /// builder (cute-dbt#105).
    fn model_node_with_patch_path(id: &NodeId, ck: &str, ofp: &str, patch_path: &str) -> Node {
        model_node_with_path(id, ck, ofp).with_patch_path(Some(patch_path.to_owned()))
    }

    #[test]
    fn pr_diff_arm_scopes_a_schema_yml_config_only_change() {
        // THE RED-FIRST TEST (cute-dbt#411): a model whose ONLY diff hit
        // is its `schema.yml` (`patch_path`) — `.sql` untouched — must
        // enter scope, drag its tests in as context, and carry
        // `config: true`. Before the fix `patch_path` was never a scope
        // signal, so this model + its tests vanished from the report.
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_patch_path(
                &dim_payers,
                "ck1",
                "models/marts/dim_payers.sql",
                "models/marts/_core__models.yml",
            ),
        );

        let test_id = "unit_test.shop.dim_payers.injects_unknown";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_with_path(
                "injects_unknown",
                "dim_payers",
                Some("models/marts/_unit_tests.yml"),
            ),
        );

        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        // ONLY the schema.yml is in the diff — the model's .sql and the
        // test's own declaring YAML are both untouched.
        let input = pr_diff_input(&["models/marts/_core__models.yml"], None);
        let selection = select_in_scope(&current, &input);

        assert!(
            selection.models_in_scope.contains(&dim_payers),
            "a config-only model must be in scope (was silently dropped)",
        );
        assert!(
            selection.in_scope.contains(test_id),
            "the config-only model's tests join in_scope as context",
        );
        assert!(
            !selection.changed.contains(test_id),
            "a config edit updates no test definition — context, never changed",
        );
        let axes = selection.axes.get(&dim_payers).copied().unwrap_or_default();
        assert!(axes.config, "the config axis fired");
        assert!(!axes.body, "the body axis did not fire (sql untouched)");
        assert!(axes.unit_test, "the model has an in-scope test");
    }

    #[test]
    fn pr_diff_arm_shared_schema_yml_fans_out_to_all_models() {
        // One `schema.yml` patches three models; the diff touches ONLY
        // that file → all three are in scope with `config: true`; a fourth
        // model under a DIFFERENT schema.yml stays out.
        let shared_yml = "models/marts/_analytics__models.yml";
        let other_yml = "models/staging/_staging__models.yml";
        let m1 = NodeId::new("model.shop.fct_a");
        let m2 = NodeId::new("model.shop.fct_b");
        let m3 = NodeId::new("model.shop.fct_c");
        let m4 = NodeId::new("model.shop.stg_other");
        let mut nodes = HashMap::new();
        nodes.insert(
            m1.clone(),
            model_node_with_patch_path(&m1, "ck1", "models/marts/fct_a.sql", shared_yml),
        );
        nodes.insert(
            m2.clone(),
            model_node_with_patch_path(&m2, "ck2", "models/marts/fct_b.sql", shared_yml),
        );
        nodes.insert(
            m3.clone(),
            model_node_with_patch_path(&m3, "ck3", "models/marts/fct_c.sql", shared_yml),
        );
        nodes.insert(
            m4.clone(),
            model_node_with_patch_path(&m4, "ck4", "models/staging/stg_other.sql", other_yml),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(&[shared_yml], None);
        let selection = select_in_scope(&current, &input);

        for m in [&m1, &m2, &m3] {
            assert!(selection.models_in_scope.contains(m), "{m:?} fans out");
            assert!(
                selection.axes.get(m).is_some_and(|a| a.config),
                "{m:?} carries config:true",
            );
        }
        assert!(
            !selection.models_in_scope.contains(&m4),
            "a model under a different schema.yml stays out",
        );
    }

    #[test]
    fn pr_diff_arm_axes_body_only_for_a_sql_only_change() {
        // A `.sql`-only change (schema.yml untouched) → body:true,
        // config:false. Pins body↔config independence.
        let dim = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim.clone(),
            model_node_with_patch_path(
                &dim,
                "ck1",
                "models/marts/dim_payers.sql",
                "models/marts/_models.yml",
            ),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(&["models/marts/dim_payers.sql"], None);
        let selection = select_in_scope(&current, &input);

        let axes = selection.axes.get(&dim).copied().unwrap_or_default();
        assert!(axes.body, "the body axis fired");
        assert!(
            !axes.config,
            "the config axis did not fire (yaml untouched)"
        );
        assert!(!axes.unit_test, "no tests target this model");
    }

    #[test]
    fn pr_diff_arm_axes_all_three_fire_independently() {
        // The 3-axis cube corner: a model whose `.sql`, whose `schema.yml`,
        // AND whose test's own YAML are all in the diff → all three axes.
        let dim = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim.clone(),
            model_node_with_patch_path(
                &dim,
                "ck1",
                "models/marts/dim_payers.sql",
                "models/marts/_core__models.yml",
            ),
        );
        let test_id = "unit_test.shop.dim_payers.injects_unknown";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_with_path(
                "injects_unknown",
                "dim_payers",
                Some("models/marts/_unit_tests.yml"),
            ),
        );
        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input(
            &[
                "models/marts/dim_payers.sql",
                "models/marts/_core__models.yml",
                "models/marts/_unit_tests.yml",
            ],
            None,
        );
        let selection = select_in_scope(&current, &input);

        let axes = selection.axes.get(&dim).copied().unwrap_or_default();
        assert!(axes.body, "body fired");
        assert!(axes.config, "config fired");
        assert!(axes.unit_test, "unit_test fired");
        assert!(axes.any());
        // The all-three corner also marks the test changed (its own YAML
        // is in the diff).
        assert!(selection.changed.contains(test_id));
    }

    #[test]
    fn pr_diff_arm_test_only_axis_for_a_changed_test_yaml() {
        // A test-only change (the test's declaring YAML is in the diff,
        // the model's .sql and schema.yml untouched) → unit_test:true,
        // body:false, config:false. The model is in scope ONLY because it
        // hosts the in-scope test → lean-(b) gives it an all-but-unit_test
        // axes entry rather than dropping it.
        let dim = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim.clone(),
            model_node_with_patch_path(
                &dim,
                "ck1",
                "models/marts/dim_payers.sql",
                "models/marts/_core__models.yml",
            ),
        );
        let test_id = "unit_test.shop.dim_payers.injects_unknown";
        let mut tests = HashMap::new();
        tests.insert(
            test_id.to_owned(),
            test_with_path(
                "injects_unknown",
                "dim_payers",
                Some("models/marts/_unit_tests.yml"),
            ),
        );
        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input(&["models/marts/_unit_tests.yml"], None);
        let selection = select_in_scope(&current, &input);

        assert!(selection.models_in_scope.contains(&dim));
        let axes = selection.axes.get(&dim).copied().unwrap_or_default();
        assert!(axes.unit_test, "the unit_test axis fired");
        assert!(!axes.body, "the body axis did not fire");
        assert!(!axes.config, "the config axis did not fire");
    }

    #[test]
    fn pr_diff_arm_model_with_no_patch_path_never_gets_config_axis() {
        // A model with `patch_path == None` (in-SQL `config()` block, no
        // schema.yml entry). Even when the diff touches its `.sql`, the
        // config axis must NOT fire — an in-SQL config() change is a BODY
        // change (it lives in the .sql, changes the checksum). No false
        // config-positive.
        let dim = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        // model_node_with_path leaves patch_path = None.
        nodes.insert(
            dim.clone(),
            model_node_with_path(&dim, "ck1", "models/marts/dim_payers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(&["models/marts/dim_payers.sql"], None);
        let selection = select_in_scope(&current, &input);

        let axes = selection.axes.get(&dim).copied().unwrap_or_default();
        assert!(axes.body, "an in-SQL config() change is a body change");
        assert!(
            !axes.config,
            "no patch_path ⇒ the config axis can never fire (no false positive)",
        );
    }

    #[test]
    fn pr_diff_arm_config_modified_model_with_zero_tests_is_in_scope() {
        // The exact silent-drop the bug opens with, on the no-tests path:
        // a config-only change on a model with ZERO unit tests still puts
        // the model in `models_in_scope` (via the new Arm 3), with
        // config:true and unit_test:false.
        let stg = NodeId::new("model.shop.stg_payments");
        let mut nodes = HashMap::new();
        nodes.insert(
            stg.clone(),
            model_node_with_patch_path(
                &stg,
                "ck1",
                "models/staging/stg_payments.sql",
                "models/staging/_staging__models.yml",
            ),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input(&["models/staging/_staging__models.yml"], None);
        let selection = select_in_scope(&current, &input);

        assert!(
            selection.models_in_scope.contains(&stg),
            "a config-only no-tests model must be in scope (Arm 3)",
        );
        assert_eq!(selection.in_scope.len(), 0, "no tests to scope");
        let axes = selection.axes.get(&stg).copied().unwrap_or_default();
        assert!(axes.config);
        assert!(!axes.unit_test);
        assert!(!axes.body);
    }

    #[test]
    fn pr_diff_arm_axes_keys_equal_models_in_scope_over_a_kind_cube() {
        // The structural invariant `axes.keys() ⊆ models_in_scope`, and
        // under lean-(b) exactly `axes.keys() == models_in_scope`.
        // Exhaustively enumerated (house style — no proptest) over a small
        // cube of model kinds: body-only, config-only, test-only,
        // all-three, and an out-of-scope model. The selection's `axes` key
        // set must equal its model-scope set for EVERY constructed shape.
        let id = |kind: &str| NodeId::new(format!("model.shop.{kind}"));
        let (body_only, config_only, test_only, all_three, untouched) = (
            id("body_only"),
            id("config_only"),
            id("test_only"),
            id("all_three"),
            id("untouched"),
        );

        let mut nodes = HashMap::new();
        // patch_path = models/_<kind>.yml; sql = models/<kind>.sql.
        for (n, (m, ck)) in [
            (&body_only, ("b", "ck1")),
            (&config_only, ("c", "ck2")),
            (&test_only, ("t", "ck3")),
            (&all_three, ("a", "ck4")),
            (&untouched, ("u", "ck5")),
        ] {
            let stem = n.as_str().rsplit('.').next().unwrap();
            nodes.insert(
                n.clone(),
                model_node_with_patch_path(
                    n,
                    ck,
                    &format!("models/{stem}.sql"),
                    &format!("models/_{m}.yml"),
                ),
            );
        }

        let mut tests = HashMap::new();
        for kind in ["test_only", "all_three"] {
            tests.insert(
                format!("unit_test.shop.{kind}.checks"),
                test_with_path("checks", kind, Some(&format!("models/_{kind}_tests.yml"))),
            );
        }
        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());

        let input = pr_diff_input(
            &[
                "models/body_only.sql",        // body_only: body
                "models/_c.yml",               // config_only: config
                "models/_test_only_tests.yml", // test_only: unit_test
                "models/all_three.sql",        // all_three: body
                "models/_a.yml",               // all_three: config
                "models/_all_three_tests.yml", // all_three: unit_test
            ],
            None,
        );
        let selection = select_in_scope(&current, &input);

        // ⊆ in both directions ⇒ equality (lean-(b)).
        let key_set: BTreeSet<NodeId> = selection.axes.keys().cloned().collect();
        let model_set: BTreeSet<NodeId> = selection.models_in_scope.iter().cloned().collect();
        assert_eq!(
            key_set, model_set,
            "axes.keys() == models_in_scope under lean-(b)",
        );
        for id in selection.axes.keys() {
            assert!(
                selection.models_in_scope.contains(id),
                "axes key {id:?} ⊆ models_in_scope",
            );
        }
        assert!(
            !selection.models_in_scope.contains(&untouched),
            "the untouched model stays out of scope",
        );

        // Per-kind axis truth (the cube corners) as (body, config, unit_test).
        let ax = |n: &NodeId| {
            let a = selection.axes.get(n).copied().unwrap_or_default();
            (a.body, a.config, a.unit_test)
        };
        assert_eq!(ax(&body_only), (true, false, false));
        assert_eq!(ax(&config_only), (false, true, false));
        assert_eq!(ax(&test_only), (false, false, true));
        assert_eq!(ax(&all_three), (true, true, true));
    }

    #[test]
    fn pr_diff_arm_config_axis_honors_project_root_strip() {
        // U2 strip-uniformity: `patch_path` reaches `index.contains_changed`
        // exactly as `original_file_path` does. The diff-side keyset is
        // built with a `--project-root` strip; the manifest-side
        // `patch_path` is already project-relative (scheme-stripped at
        // ingestion). The config axis must fire just as the body axis does
        // under the same strip (mirror of `pr_diff_arm_honors_project_root_strip`).
        let dim = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim.clone(),
            model_node_with_patch_path(
                &dim,
                "ck1",
                "models/marts/dim_payers.sql",
                "models/marts/_core__models.yml",
            ),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        // The diff carries the project-root-prefixed schema.yml path ONLY;
        // strip removes the `dbt_project/` prefix so it matches the
        // model's project-relative `patch_path`.
        let input = pr_diff_input(
            &["dbt_project/models/marts/_core__models.yml"],
            Some(Path::new("dbt_project")),
        );
        let selection = select_in_scope(&current, &input);

        assert!(
            selection.models_in_scope.contains(&dim),
            "the config-modified model is in scope under the project-root strip",
        );
        assert!(
            selection.axes.get(&dim).is_some_and(|a| a.config),
            "the config axis fires identically to body under the strip",
        );
    }

    #[test]
    fn pr_diff_arm_rename_of_schema_yml_still_fires_config_axis() {
        // A pure rename of a model's schema.yml: both sides join the index
        // keyset (cute-dbt#80), so the model's current `patch_path` (the
        // NEW path) matches → config axis fires.
        let dim = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim.clone(),
            model_node_with_patch_path(
                &dim,
                "ck1",
                "models/marts/dim_payers.sql",
                "models/marts/_renamed__models.yml",
            ),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input_with_renames(
            &[],
            &[(
                "models/marts/_old__models.yml",
                "models/marts/_renamed__models.yml",
            )],
            None,
        );
        let selection = select_in_scope(&current, &input);

        assert!(selection.models_in_scope.contains(&dim));
        assert!(
            selection.axes.get(&dim).is_some_and(|a| a.config),
            "a renamed schema.yml fires the config axis at the new path",
        );
    }

    #[test]
    fn baseline_arm_produces_empty_axes() {
        // Option A pin (cute-dbt#411): the baseline arm carries NO per-axis
        // attribution. A future un-collapse of `modified_set` must change
        // this test deliberately, never silently. Uses the config_only_pair
        // WITH the `.configs` sub-selector so the model IS in scope — yet
        // `axes` stays empty (the gap is the attribution, not the scoping).
        let (current, baseline) = config_only_pair();
        let input = ScopeInput::Baseline {
            manifest: Box::new(baseline),
            sub_selectors: vec![ModifierKind::Configs],
        };
        let selection = select_in_scope(&current, &input);
        assert!(
            !selection.models_in_scope.is_empty(),
            "precondition: the .configs selector put the model in scope",
        );
        assert!(
            selection.axes.is_empty(),
            "Option A: the baseline arm produces empty axes (documented gap)",
        );
        assert!(
            selection.model_states.is_empty(),
            "cute-dbt#416: the baseline arm produces no NEW/MODIFIED states",
        );
        assert!(
            selection.removed_models.is_empty(),
            "cute-dbt#416: the baseline arm surfaces no REMOVED model paths",
        );
    }

    // ----- select_in_scope: NEW/MODIFIED/REMOVED states (cute-dbt#416) -----

    #[test]
    fn pr_diff_arm_added_model_is_new_not_modified() {
        // A model whose `.sql` is in the diff's `added` keyset is NEW —
        // even though its added file also carries body hunks (so the body
        // axis fires). NEW supersedes MODIFIED.
        let added = NodeId::new("model.shop.fct_new");
        let mut nodes = HashMap::new();
        nodes.insert(
            added.clone(),
            model_node_with_path(&added, "ck1", "models/marts/fct_new.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input_with_added_deleted(&["models/marts/fct_new.sql"], &[], None);
        let selection = select_in_scope(&current, &input);

        assert!(
            selection.models_in_scope.contains(&added),
            "a NEW model is in scope (full current node + detail)",
        );
        assert_eq!(
            selection.model_states.get(&added),
            Some(&ModelState::New),
            "an added-file model is NEW, not MODIFIED",
        );
        assert!(
            selection.axes.get(&added).is_some_and(|a| a.body),
            "the NEW model still carries its body axis (the added file has hunks)",
        );
        assert!(
            selection.removed_models.is_empty(),
            "no deletions → no REMOVED models",
        );
    }

    #[test]
    fn pr_diff_arm_modified_model_is_modified_not_new() {
        // An existing model (not in `added`) whose `.sql` is in the diff is
        // MODIFIED — the cute-dbt#411 path.
        let dim = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim.clone(),
            model_node_with_path(&dim, "ck1", "models/marts/dim_payers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        // The body file is changed but NOT in `added` — a modify.
        let input = pr_diff_input(&["models/marts/dim_payers.sql"], None);
        let selection = select_in_scope(&current, &input);

        assert_eq!(
            selection.model_states.get(&dim),
            Some(&ModelState::Modified),
            "a changed-but-not-added model is MODIFIED",
        );
    }

    #[test]
    fn pr_diff_arm_deleted_model_path_is_removed_node_less() {
        // A deleted `.sql` under `models/` that names no current node is a
        // REMOVED model — carried as a path, never a node / a model_states
        // entry.
        let kept = NodeId::new("model.shop.dim_kept");
        let mut nodes = HashMap::new();
        nodes.insert(
            kept.clone(),
            model_node_with_path(&kept, "ck1", "models/marts/dim_kept.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input_with_added_deleted(&[], &["models/marts/dim_gone.sql"], None);
        let selection = select_in_scope(&current, &input);

        assert_eq!(
            selection.removed_models,
            vec!["models/marts/dim_gone.sql".to_owned()],
            "the deleted model path is a REMOVED model",
        );
        assert!(
            !selection
                .model_states
                .values()
                .any(|s| *s == ModelState::Removed),
            "REMOVED is node-less — never a model_states entry",
        );
        assert!(
            selection.models_in_scope.is_empty(),
            "a REMOVED model contributes no in-scope NODE (it's gone)",
        );
    }

    #[test]
    fn pr_diff_arm_deleted_non_model_path_is_not_removed() {
        // A deleted file that is NOT a model path (a `schema.yml`, a doc, a
        // file outside `models/`) is never a REMOVED model.
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let input = pr_diff_input_with_added_deleted(
            &[],
            &[
                "models/marts/_schema.yml", // a schema.yml, not a .sql/.py
                "macros/util.sql",          // a .sql but outside models/
                "README.md",                // a doc
                "seeds/raw.csv",            // a seed
            ],
            None,
        );
        let selection = select_in_scope(&current, &input);
        assert!(
            selection.removed_models.is_empty(),
            "non-model deleted paths are not REMOVED models",
        );
    }

    #[test]
    fn pr_diff_arm_deleted_path_still_a_node_is_not_removed() {
        // A deleted path that STILL resolves to a current node (a modify or
        // rename mis-classified, or a re-added file) is not a removal —
        // only genuinely node-less deletions are REMOVED.
        let dim = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim.clone(),
            model_node_with_path(&dim, "ck1", "models/marts/dim_payers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let input = pr_diff_input_with_added_deleted(
            &[],
            &["models/marts/dim_payers.sql"], // deleted, but still a node
            None,
        );
        let selection = select_in_scope(&current, &input);
        assert!(
            selection.removed_models.is_empty(),
            "a deleted path that still names a current node is not REMOVED",
        );
    }

    #[test]
    fn pr_diff_arm_removed_model_paths_honor_project_root_strip() {
        // The REMOVED heuristic runs on strip-normalized deleted paths, so a
        // project-root-prefixed deleted path resolves to the project-relative
        // `models/` prefix (mirror of `pr_diff_arm_config_axis_honors_project_root_strip`).
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let input = pr_diff_input_with_added_deleted(
            &[],
            &["dbt_project/models/marts/dim_gone.sql"],
            Some(Path::new("dbt_project")),
        );
        let selection = select_in_scope(&current, &input);
        assert_eq!(
            selection.removed_models,
            vec!["models/marts/dim_gone.sql".to_owned()],
            "the strip rebases the deleted path under the `models/` prefix",
        );
    }

    #[test]
    fn pr_diff_arm_model_states_keys_equal_models_in_scope() {
        // The structural invariant `model_states.keys() == models_in_scope`
        // on the PrDiff arm — every in-scope model NODE carries exactly one
        // state. Exhaustively enumerated over NEW + MODIFIED + REMOVED with a
        // deletion that does NOT add a node-state key.
        let new_id = NodeId::new("model.shop.fct_new");
        let mod_id = NodeId::new("model.shop.dim_mod");
        let mut nodes = HashMap::new();
        nodes.insert(
            new_id.clone(),
            model_node_with_path(&new_id, "ck1", "models/marts/fct_new.sql"),
        );
        nodes.insert(
            mod_id.clone(),
            model_node_with_path(&mod_id, "ck2", "models/marts/dim_mod.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        // fct_new added; dim_mod modified (changed but not added); a third
        // path deleted (REMOVED, node-less).
        let mut diff = prdiff_from_paths(&["models/marts/fct_new.sql", "models/marts/dim_mod.sql"]);
        diff.added = vec!["models/marts/fct_new.sql".to_owned()];
        diff.deleted = vec!["models/marts/dim_gone.sql".to_owned()];
        let input = ScopeInput::PrDiff {
            index: NormalizedDiffIndex::new(&diff, None),
        };
        let selection = select_in_scope(&current, &input);

        let state_keys: BTreeSet<NodeId> = selection.model_states.keys().cloned().collect();
        let model_set: BTreeSet<NodeId> = selection.models_in_scope.iter().cloned().collect();
        assert_eq!(
            state_keys, model_set,
            "model_states.keys() == models_in_scope (every node has one state)",
        );
        assert_eq!(selection.model_states.get(&new_id), Some(&ModelState::New));
        assert_eq!(
            selection.model_states.get(&mod_id),
            Some(&ModelState::Modified)
        );
        assert_eq!(
            selection.removed_models,
            vec!["models/marts/dim_gone.sql".to_owned()],
            "the node-less deletion is a REMOVED path, not a state key",
        );
    }

    #[test]
    fn change_axes_any_is_true_iff_some_axis_fired() {
        // Exhaustive enumeration of the 2^3 ChangeAxes cube (house style):
        // `any()` is the OR of the three bools.
        for body in [false, true] {
            for config in [false, true] {
                for unit_test in [false, true] {
                    let axes = ChangeAxes {
                        body,
                        config,
                        unit_test,
                    };
                    assert_eq!(axes.any(), body || config || unit_test);
                }
            }
        }
        assert!(!ChangeAxes::default().any(), "the default is all-false");
    }

    // ----- widen_with_config_attributions (cute-dbt#267) -----

    use crate::domain::project_def::ConfigAttribution;
    use std::collections::BTreeMap as StdBTreeMap;

    /// An attribution map selecting the given model ids (one
    /// `+materialized` chip each — the chip content is irrelevant to
    /// widening membership).
    fn attributions_for(ids: &[&str]) -> StdBTreeMap<String, Vec<ConfigAttribution>> {
        ids.iter()
            .map(|id| {
                (
                    (*id).to_owned(),
                    vec![ConfigAttribution {
                        key: "materialized".to_owned(),
                        path: "models.shop.marts".to_owned(),
                    }],
                )
            })
            .collect()
    }

    /// A two-model manifest (`dim_payers` with one unit test, `stg_x`
    /// with none) and a `PrDiff` selection scoped to NOTHING (the diff
    /// touches only extraneous paths).
    fn widening_fixture() -> (Manifest, ScopeSelection) {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let stg_x = NodeId::new("model.shop.stg_x");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        nodes.insert(
            stg_x.clone(),
            model_node_with_path(&stg_x, "ck2", "models/staging/stg_x.sql"),
        );
        let mut tests = HashMap::new();
        tests.insert(
            "unit_test.shop.dim_payers.injects_unknown".to_owned(),
            test_with_path(
                "injects_unknown",
                "dim_payers",
                Some("models/marts/_models.yml"),
            ),
        );
        let current = Manifest::new(ManifestMetadata::new("v12"), nodes, tests, HashMap::new());
        let selection = select_in_scope(&current, &pr_diff_input(&["README.md"], None));
        assert!(selection.models_in_scope.is_empty(), "fixture precondition");
        (current, selection)
    }

    #[test]
    fn widen_adds_attributed_models_and_their_tests_as_context() {
        let (current, selection) = widening_fixture();
        let widened = widen_with_config_attributions(
            selection,
            &current,
            &attributions_for(&["model.shop.dim_payers"]),
        );
        assert!(
            widened
                .models_in_scope
                .contains(&NodeId::new("model.shop.dim_payers")),
            "the attributed model joins the model scope",
        );
        assert!(
            widened
                .in_scope
                .contains("unit_test.shop.dim_payers.injects_unknown"),
            "the widened model's unit test joins in_scope (target-modified OR-arm)",
        );
        assert!(
            widened.changed.is_empty(),
            "a config-tree edit updates no test definition — context, never changed",
        );
        assert!(
            !widened
                .models_in_scope
                .contains(&NodeId::new("model.shop.stg_x")),
            "unattributed models stay out",
        );
    }

    #[test]
    fn widen_with_empty_attributions_is_identity() {
        let (current, selection) = widening_fixture();
        let widened =
            widen_with_config_attributions(selection.clone(), &current, &StdBTreeMap::new());
        assert_eq!(widened, selection, "an empty map widens nothing");
    }

    #[test]
    fn widen_unions_with_an_existing_selection_never_replaces() {
        // dim_payers is already in scope via its changed SQL; widening
        // stg_x must ADD it while every pre-existing member (including
        // the changed subset) survives untouched.
        let (current, _) = widening_fixture();
        let selection = select_in_scope(
            &current,
            &pr_diff_input(
                &["models/marts/dim_payers.sql", "models/marts/_models.yml"],
                None,
            ),
        );
        let before_in_scope: Vec<String> = selection.in_scope.iter().map(str::to_owned).collect();
        let before_changed = selection.changed.clone();
        let widened = widen_with_config_attributions(
            selection,
            &current,
            &attributions_for(&["model.shop.stg_x"]),
        );
        assert!(
            widened
                .models_in_scope
                .contains(&NodeId::new("model.shop.dim_payers")),
            "pre-existing members survive",
        );
        assert!(
            widened
                .models_in_scope
                .contains(&NodeId::new("model.shop.stg_x")),
            "the widened model joins",
        );
        for id in &before_in_scope {
            assert!(widened.in_scope.contains(id), "{id} must survive the union");
        }
        assert_eq!(
            widened.changed, before_changed,
            "the changed subset is never touched by widening",
        );
        for id in widened.changed.iter() {
            assert!(
                widened.in_scope.contains(id),
                "changed ⊆ in_scope holds after widening",
            );
        }
    }

    #[test]
    fn widen_skips_an_attribution_for_a_node_absent_from_the_manifest() {
        let (current, selection) = widening_fixture();
        let widened = widen_with_config_attributions(
            selection,
            &current,
            &attributions_for(&["model.shop.gone"]),
        );
        assert!(
            widened.models_in_scope.is_empty(),
            "an unresolvable id widens nothing (belt-and-braces)",
        );
    }

    #[test]
    fn widen_does_not_populate_axes_for_a_config_tree_model() {
        // A7 / locked Q4 (cute-dbt#411): the dbt_project.yml config-tree
        // widening adds the model to `models_in_scope` but must NOT set its
        // `config` axis — the `config` axis is the model's schema.yml
        // (`patch_path`) ONLY. The config-tree keeps its own provenance
        // chip row. So a model in scope SOLELY via the config-tree
        // attribution has no `axes` entry from this function.
        let (current, selection) = widening_fixture();
        assert!(selection.axes.is_empty(), "fixture precondition: no axes");
        let widened = widen_with_config_attributions(
            selection,
            &current,
            &attributions_for(&["model.shop.dim_payers"]),
        );
        assert!(
            widened
                .models_in_scope
                .contains(&NodeId::new("model.shop.dim_payers")),
            "the config-tree model joins model scope",
        );
        assert!(
            widened.axes.is_empty(),
            "config-tree widening never populates the config axis (locked Q4)",
        );
    }

    // ----- changed_models (cute-dbt#106 — explore change context) -----

    #[test]
    fn changed_models_marks_exactly_the_path_modified_models() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let stg_customers = NodeId::new("model.shop.stg_customers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        nodes.insert(
            stg_customers.clone(),
            model_node_with_path(&stg_customers, "ck2", "models/staging/stg_customers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let index = NormalizedDiffIndex::new(
            &prdiff_from_paths(&["models/marts/dim_payers.sql", "README.md"]),
            None,
        );
        let changed = changed_models(&current, &index);

        assert!(changed.contains(&dim_payers));
        assert!(!changed.contains(&stg_customers));
        assert_eq!(changed.len(), 1, "extraneous diff paths mark nothing");
    }

    #[test]
    fn changed_models_excludes_non_model_nodes() {
        // A generic test node whose declaring SQL is in the diff must not
        // surface as a changed MODEL (the cute-dbt#167 filter, shared with
        // select_in_scope_pr_diff so the two consumers cannot drift).
        let generic_test = NodeId::new("test.shop.assert_positive_total");
        let mut nodes = HashMap::new();
        nodes.insert(
            generic_test.clone(),
            typed_node_with_path(
                &generic_test,
                "test",
                "ck1",
                "tests/assert_positive_total.sql",
            ),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let index = NormalizedDiffIndex::new(
            &prdiff_from_paths(&["tests/assert_positive_total.sql"]),
            None,
        );
        assert!(changed_models(&current, &index).is_empty());
    }

    #[test]
    fn changed_models_marks_a_purely_renamed_model_at_its_new_path() {
        // The cute-dbt#80 rename lineage: a pure rename carries no `+++`
        // header and no hunks — only the rename pair — and the current
        // manifest holds the node at the NEW path. The index keyset
        // carries both sides, so the model still marks as changed.
        let dim_b = NodeId::new("model.shop.dim_b");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_b.clone(),
            model_node_with_path(&dim_b, "ck1", "models/marts/dim_b.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let mut diff = prdiff_from_paths(&[]);
        diff.renames = vec![crate::domain::pr_diff::RenamePair {
            from: "models/marts/dim_a.sql".to_owned(),
            to: "models/marts/dim_b.sql".to_owned(),
        }];
        let index = NormalizedDiffIndex::new(&diff, None);
        let changed = changed_models(&current, &index);
        assert!(
            changed.contains(&dim_b),
            "the renamed model marks changed under its NEW path",
        );
    }

    #[test]
    fn changed_models_honors_the_project_root_strip() {
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let index = NormalizedDiffIndex::new(
            &prdiff_from_paths(&["dbt_project/models/marts/dim_payers.sql"]),
            Some(Path::new("dbt_project")),
        );
        assert!(changed_models(&current, &index).contains(&dim_payers));
    }

    #[test]
    fn changed_models_matches_the_pr_diff_arm_model_marking() {
        // The reuse property: explore's changed set is EXACTLY the PrDiff
        // arm's path-modified model marking (one matching authority — the
        // two verbs cannot disagree about which models a diff touched).
        let dim_payers = NodeId::new("model.shop.dim_payers");
        let mut nodes = HashMap::new();
        nodes.insert(
            dim_payers.clone(),
            model_node_with_path(&dim_payers, "ck1", "models/marts/dim_payers.sql"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let index =
            NormalizedDiffIndex::new(&prdiff_from_paths(&["models/marts/dim_payers.sql"]), None);
        let changed = changed_models(&current, &index);
        let selection = select_in_scope(
            &current,
            &ScopeInput::PrDiff {
                index: index.clone(),
            },
        );
        // dim_payers has zero unit tests, so the report arm surfaces it
        // via arm 2 — the same membership changed_models reports.
        assert_eq!(changed, selection.models_in_scope);
    }

    // ----- all_models (cute-dbt#100 — the explore verb's seam) -----

    #[test]
    fn all_models_returns_every_model_node_and_nothing_else() {
        // Three models (one of them UNCOMPILED — explore is fail-open, so
        // compiled-ness must not filter) plus a generic test node and a
        // seed node that must both stay out.
        let m1 = NodeId::new("model.shop.dim_payers");
        let m2 = NodeId::new("model.shop.stg_customers");
        let m3 = NodeId::new("model.shop.stg_uncompiled");
        let t1 = NodeId::new("test.shop.not_null_dim_payers_id");
        let s1 = NodeId::new("seed.shop.raw_payers");

        let mut nodes = HashMap::new();
        nodes.insert(m1.clone(), model_node(&m1, "ck1", None));
        nodes.insert(m2.clone(), model_node(&m2, "ck2", None));
        nodes.insert(m3.clone(), uncompiled_model_node(&m3, "ck3"));
        nodes.insert(t1.clone(), typed_node(&t1, "test", "ck4"));
        nodes.insert(s1.clone(), typed_node(&s1, "seed", "ck5"));
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );

        let models = all_models(&current);

        assert_eq!(models.len(), 3, "exactly the three model nodes");
        assert!(models.contains(&m1));
        assert!(models.contains(&m2));
        assert!(
            models.contains(&m3),
            "an uncompiled model is still in the full-manifest scope (fail-open)",
        );
        assert!(!models.contains(&t1), "a generic test node is not a model");
        assert!(!models.contains(&s1), "a seed node is not a model");
    }

    #[test]
    fn all_models_of_an_empty_manifest_is_empty() {
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        assert!(all_models(&current).is_empty());
    }

    #[test]
    fn all_models_iterates_in_deterministic_node_id_order() {
        // BTreeSet-backed: insertion order (HashMap) must not leak into
        // iteration order — the rendered explore pages depend on it.
        let ids = ["model.shop.zeta", "model.shop.alpha", "model.shop.mid"];
        let mut nodes = HashMap::new();
        for id in ids {
            let node_id = NodeId::new(id);
            nodes.insert(node_id.clone(), model_node(&node_id, "ck", None));
        }
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let models = all_models(&current);
        let ordered: Vec<&str> = models.iter().map(NodeId::as_str).collect();
        assert_eq!(
            ordered,
            vec!["model.shop.alpha", "model.shop.mid", "model.shop.zeta"],
        );
    }

    /// A model node with `compiled_code: None` — the `dbt parse` shape
    /// explore renders fail-open as "not compiled".
    fn uncompiled_model_node(id: &NodeId, ck: &str) -> Node {
        Node::new(
            id.clone(),
            "model",
            checksum(ck),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    // ----- helpers -----

    fn checksum(value: &str) -> Checksum {
        Checksum::new("sha256", value)
    }

    fn model_node(id: &NodeId, ck: &str, ofp: Option<&str>) -> Node {
        Node::new(
            id.clone(),
            "model",
            checksum(ck),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            ofp.map(str::to_owned),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    fn model_node_with_path(id: &NodeId, ck: &str, ofp: &str) -> Node {
        model_node(id, ck, Some(ofp))
    }

    /// A node of an arbitrary `resource_type` (cute-dbt#167 — the arm-2
    /// resource-type filter regression tests).
    fn typed_node(id: &NodeId, resource_type: &str, ck: &str) -> Node {
        Node::new(
            id.clone(),
            resource_type,
            checksum(ck),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    /// A non-model node with an `original_file_path` (the `PrDiff` arm's
    /// path-matching input).
    fn typed_node_with_path(id: &NodeId, resource_type: &str, ck: &str, ofp: &str) -> Node {
        Node::new(
            id.clone(),
            resource_type,
            checksum(ck),
            None,
            None,
            DependsOn::default(),
            Some(ofp.to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    fn test_for(name: &str, model_bare: &str) -> UnitTest {
        test_with_path(name, model_bare, None)
    }

    // ----- select_seeds_in_scope (cute-dbt#350) -----

    #[test]
    fn select_seeds_baseline_arm_scopes_a_modified_seed() {
        // A seed whose checksum changed against the baseline is in scope;
        // an unchanged seed is not.
        let modified = NodeId::new("seed.shop.raw_customers");
        let unchanged = NodeId::new("seed.shop.raw_orders");
        let mut current_nodes = HashMap::new();
        current_nodes.insert(modified.clone(), typed_node(&modified, "seed", "ck-new"));
        current_nodes.insert(unchanged.clone(), typed_node(&unchanged, "seed", "ck-same"));
        let mut baseline_nodes = HashMap::new();
        baseline_nodes.insert(modified.clone(), typed_node(&modified, "seed", "ck-old"));
        baseline_nodes.insert(unchanged.clone(), typed_node(&unchanged, "seed", "ck-same"));

        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            current_nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let baseline = Manifest::new(
            ManifestMetadata::new("v12"),
            baseline_nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let input = ScopeInput::Baseline {
            manifest: Box::new(baseline),
            sub_selectors: Vec::new(),
        };

        let seeds = select_seeds_in_scope(&current, &input);
        assert!(seeds.contains(&modified));
        assert!(!seeds.contains(&unchanged));
        assert_eq!(seeds.len(), 1);
    }

    #[test]
    fn select_seeds_pr_diff_arm_scopes_a_seed_whose_csv_the_diff_changed() {
        // The seed's `original_file_path` is in the diff ⇒ in scope; a seed
        // whose CSV the diff did not touch is not.
        let touched = NodeId::new("seed.shop.raw_customers");
        let untouched = NodeId::new("seed.shop.raw_orders");
        let mut nodes = HashMap::new();
        nodes.insert(
            touched.clone(),
            typed_node_with_path(&touched, "seed", "ck", "seeds/raw_customers.csv"),
        );
        nodes.insert(
            untouched.clone(),
            typed_node_with_path(&untouched, "seed", "ck", "seeds/raw_orders.csv"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let input = pr_diff_input(&["seeds/raw_customers.csv"], None);

        let seeds = select_seeds_in_scope(&current, &input);
        assert!(seeds.contains(&touched));
        assert!(!seeds.contains(&untouched));
    }

    #[test]
    fn select_seeds_pr_diff_arm_filters_non_seed_resource_types() {
        // A model whose `.sql` the diff changed never enters the SEED set —
        // the seed mirror of the cute-dbt#167 resource-type filter.
        let model = NodeId::new("model.shop.stg_customers");
        let seed = NodeId::new("seed.shop.raw_customers");
        let mut nodes = HashMap::new();
        nodes.insert(
            model.clone(),
            model_node_with_path(&model, "ck", "models/stg_customers.sql"),
        );
        nodes.insert(
            seed.clone(),
            typed_node_with_path(&seed, "seed", "ck", "seeds/raw_customers.csv"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );
        // The diff touches BOTH files; only the seed should surface.
        let input = pr_diff_input(
            &["models/stg_customers.sql", "seeds/raw_customers.csv"],
            None,
        );

        let seeds = select_seeds_in_scope(&current, &input);
        assert!(seeds.contains(&seed));
        assert!(!seeds.contains(&model));
        assert_eq!(seeds.len(), 1);
    }

    #[test]
    fn select_seeds_pr_diff_arm_is_empty_when_no_seed_csv_changed() {
        // A diff that touches only a model file scopes zero seeds.
        let seed = NodeId::new("seed.shop.raw_customers");
        let mut nodes = HashMap::new();
        nodes.insert(
            seed.clone(),
            typed_node_with_path(&seed, "seed", "ck", "seeds/raw_customers.csv"),
        );
        let current = Manifest::new(
            ManifestMetadata::new("v12"),
            nodes,
            HashMap::new(),
            HashMap::new(),
        );
        let input = pr_diff_input(&["models/stg_customers.sql"], None);

        assert!(select_seeds_in_scope(&current, &input).is_empty());
    }

    fn test_with_path(name: &str, model_bare: &str, ofp: Option<&str>) -> UnitTest {
        UnitTest::new(
            name.to_owned(),
            NodeId::new(model_bare),
            Vec::new(),
            UnitTestExpect::new(serde_json::Value::Null, None, None),
            None,
            DependsOn::default(),
            None,
            None,
            ofp.map(str::to_owned),
        )
    }
}
