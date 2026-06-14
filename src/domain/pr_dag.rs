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
//! `nodes = modified ∪ connectors ∪ halo ∪ removed`, where
//!
//! - **modified** (M) is the caller-supplied [`ModelInScopeSet`] — the
//!   genuine PR-modified models (`changed_models` in the `PrDiff` arm, the
//!   `StateComparator` modified models in the baseline arm).
//! - **connectors** are the models on a directed lineage path *between two
//!   distinct* members of M — formally `(DESC(M) ∩ ANC(M)) \ M`, keeping
//!   only nodes reached forward from one seed *and* backward from a
//!   different seed. A node merely downstream of a single modified model
//!   is 1-hop context, **not** a connector.
//! - **halo** is the 1-hop context for a **disconnected** modified model
//!   (cute-dbt#428 — epic #427 slice A): a modified model with no directed
//!   lineage path to or from any *other* modified model is *disconnected*,
//!   and its immediate `depends_on` parents + direct children join the set as
//!   quiet, [`is_halo`](PrDagNode::is_halo)-flagged context — so an isolated
//!   change is shown with its neighbors rather than alone. A disconnected
//!   model with **no** neighbors still renders alone (no halo is possible). A
//!   modified model that IS connected to another (via connectors or a direct
//!   edge) gets **no** halo. A halo node that is itself a seed or a connector
//!   keeps that **stronger role** (the dedup contract); halo is a distinct
//!   role from connector — between-two vs context-for-one — kept separable
//!   for the render lane (#429) and the descriptor counts.
//! - **removed** are DELETED models (present in the baseline, absent from
//!   the current manifest) the caller derived as the baseline−current
//!   set-diff. They have no current `depends_on`, so they join the node
//!   set as DELETED ghosts but contribute no induced edges.
//!
//! ## Edges
//!
//! The induced subgraph over **seeds ∪ connectors ∪ removed**: every
//! model→model `depends_on` edge whose **both** endpoints are in that node
//! set. A **halo** node contributes edges **only to/from its anchor** (the
//! disconnected model it is a neighbor of) — never a generic induced edge
//! (e.g. a spurious halo↔halo link between two independent isolated anchors'
//! neighbors). The graph is acyclic (dbt lineage is a DAG) and
//! deterministically ordered ([`BTreeSet`]/sorted output), the
//! byte-identity-golden requirement the downstream render lane depends on.
//!
//! ## Per-node line counts (cute-dbt#403 — Slice B)
//!
//! Each [`PrDagNode`] carries unsigned [`lines_added`](PrDagNode::lines_added)
//! / [`lines_removed`](PrDagNode::lines_removed) counts. [`compute_pr_dag`]
//! (Slice A topology) emits `0/0`; the two arm-specific pure fns fill them:
//! [`pr_dag_lines_from_diff`] (pr-diff arm — the `+`/`-` hunk line counts for
//! the node's file) and [`pr_dag_lines_from_raw_code`] (baseline arm — the
//! `raw_code` old→new [`diff_lines`] counts). Slice C (#404) wires the
//! population into the run loop and renders the `±` chip; this slice is the
//! pure computation only. There is no I/O, no parser dependency, no `clap`,
//! no `askama` here: pure domain (`std` + `serde` derive), the same purity
//! contract the rest of `src/domain/` honors.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use crate::domain::manifest::{Manifest, NodeId};
use crate::domain::pr_diff::{DiffLineKind, NormalizedDiffIndex, diff_lines};
use crate::domain::state::ModelInScopeSet;

/// The default node-count cap above which the PR-scope mini-DAG collapses
/// to a one-line summary instead of rendering inline (cute-dbt#404).
///
/// The mini-DAG is an *overview*: a focused subgraph a reviewer reads at a
/// glance before drilling into the model selector. Past a few dozen nodes a
/// Mermaid `graph LR` stops being glanceable (it scrolls, overlaps, and
/// inflates the single-file report's `.rodata`), so a large PR collapses the
/// inline render to a "(N models in PR scope — too large to render inline)"
/// line, with the graph data still on the JSON payload for any downstream
/// consumer. `48` is an honest readability threshold (≈ a screenful of
/// `graph LR` lanes), not a hard correctness bound — a PR touching 48+ models
/// is better navigated through the selector than a wall-to-wall graph. The
/// cap counts NODES (modified ∪ connectors ∪ deleted), the same population
/// the inline render walks.
pub const DEFAULT_PR_DAG_NODE_CAP: usize = 48;

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
    /// `true` when this node is a *1-hop context halo* node — an immediate
    /// `depends_on` parent or direct child of a **disconnected** modified
    /// model (a modified model with no connector path to any *other* modified
    /// model), pulled in so an isolated change is not shown with zero
    /// neighbors (cute-dbt#428 — epic #427 slice A). Like a connector, a halo
    /// node is an unchanged carrier in the quiet/dimmed tier — but it is a
    /// **distinct role**: a connector lies *between two* modified models,
    /// whereas a halo node is context for a *single isolated* one, so the two
    /// stay separable for the render lane (#429) and the descriptor counts.
    ///
    /// Mutually exclusive with [`is_connector`](Self::is_connector) and with a
    /// genuine modified/new/deleted state: a node that is *also* a seed or a
    /// connector keeps that stronger role and is never flagged a halo (the
    /// dedup contract). `#[serde(skip_serializing_if)]` so a PR with no
    /// disconnected modified model emits **no** `is_halo` key — the goldens
    /// for the connected/empty cases stay byte-identical.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_halo: bool,
    /// Lines this node's declaring source *gained* in the PR (the diff's
    /// `+` count for the node's file). `0` for a connector / unchanged
    /// carrier, and for a node whose file is absent from the diff
    /// (cute-dbt#403 — Slice B). [`compute_pr_dag`] emits `0` here; the
    /// counts are filled by [`pr_dag_lines_from_diff`] (pr-diff arm) /
    /// [`pr_dag_lines_from_raw_code`] (baseline arm) — Slice C wires the
    /// population into the run loop and renders the `±` chip.
    #[serde(default)]
    pub lines_added: usize,
    /// Lines this node's declaring source *lost* in the PR (the diff's `-`
    /// count for the node's file). See [`Self::lines_added`] for the
    /// zero-default / connector / absent-file contract (cute-dbt#403).
    #[serde(default)]
    pub lines_removed: usize,
}

/// An unsigned per-node line-count delta — how many lines a node's
/// declaring source gained (`added`) and lost (`removed`) in the PR
/// (cute-dbt#403 — Slice B).
///
/// A render-payload-adjacent POD the two arm-specific counters
/// ([`pr_dag_lines_from_diff`] / [`pr_dag_lines_from_raw_code`]) return and
/// Slice C folds onto a [`PrDagNode`]'s
/// [`lines_added`](PrDagNode::lines_added) /
/// [`lines_removed`](PrDagNode::lines_removed). `Copy` (two `usize`s);
/// `std`-only (domain purity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineDelta {
    /// Lines gained (the `+` count). `0` for an unchanged carrier.
    pub added: usize,
    /// Lines lost (the `-` count). `0` for an unchanged carrier.
    pub removed: usize,
}

/// Per-node line counts from a parsed PR diff (the **pr-diff arm**,
/// cute-dbt#403 — Slice B).
///
/// Sums the `+` and `-` line counts across every hunk touching the node's
/// declaring file, looked up in the [`NormalizedDiffIndex`] by
/// `original_file_path`. The definition is the **raw diff line count** — the
/// same `added_lines` / `removed_lines` bodies the inline diff view renders
/// from ([`crate::domain::pr_diff::Hunk`]), counted before the block-precise
/// narrowing or the whitespace-only collapse. This is the honest "lines the
/// PR touched in this file" the `±` chip reports, and it matches what a
/// reviewer sees in `git diff --stat` for that path.
///
/// Returns `0/0` (the [`LineDelta::default`]) when the node has no
/// `original_file_path` (a deleted ghost / synthetic node) **or** when its
/// file is absent from the diff (a connector / unchanged carrier, or a node
/// changed only via a sibling) — [`NormalizedDiffIndex::hunks_for`] returns
/// an empty slice in both cases, so the fold over zero hunks is `0/0` and
/// never panics. A deleted model carries no current `original_file_path`,
/// so its diff-arm count is `0/0` here; the baseline arm
/// ([`pr_dag_lines_from_raw_code`]) is where a deletion's removed-everything
/// count is surfaced.
#[must_use]
pub fn pr_dag_lines_from_diff(
    original_file_path: Option<&str>,
    index: &NormalizedDiffIndex,
) -> LineDelta {
    let Some(ofp) = original_file_path else {
        return LineDelta::default();
    };
    index
        .hunks_for(ofp)
        .iter()
        .fold(LineDelta::default(), |acc, hunk| LineDelta {
            added: acc.added + hunk.added_lines.len(),
            removed: acc.removed + hunk.removed_lines.len(),
        })
}

/// Per-node line counts from a model's `raw_code` old → new (the **baseline
/// arm**, cute-dbt#403 — Slice B).
///
/// Diffs the baseline `raw_code` (`old`) against the current `raw_code`
/// (`new`) with the domain's line differ ([`diff_lines`], an LCS line diff)
/// and counts the resulting [`DiffLineKind::Added`] / [`DiffLineKind::Removed`]
/// lines. The whitespace handling is deliberately *raw* — `diff_lines`
/// compares lines verbatim, so the count is "lines that differ", matching
/// the baseline-mode inline SQL diff the report renders.
///
/// The new-node / deleted-ghost ends are honest and documented:
///
/// - **new node** (`old == None` — absent from the baseline): every current
///   line is an addition. `0/0` only if the node also has no current
///   `raw_code`.
/// - **deleted ghost** (`new == None` — absent from the current manifest):
///   every baseline line is a removal — the "removed everything" count. A
///   deletion is a real, countable change, so this surfaces it (rather than
///   a silent `0/0`); the diff arm cannot see it (no current file), so the
///   baseline arm owns the deletion count.
/// - **both `None`** (no `raw_code` either side — a synthetic / non-SQL
///   node): `0/0`.
///
/// Empty (`Some("")`) and absent (`None`) `raw_code` are treated alike — an
/// empty body has zero lines, so a `Some("")` → `Some("x")` is one addition
/// and a `None` → `Some("x")` is one addition, identically. Pure (`std`-only
/// via [`diff_lines`]); never panics.
#[must_use]
pub fn pr_dag_lines_from_raw_code(old: Option<&str>, new: Option<&str>) -> LineDelta {
    let old_lines = split_raw_lines(old);
    let new_lines = split_raw_lines(new);
    diff_lines(&old_lines, &new_lines)
        .lines
        .iter()
        .fold(LineDelta::default(), |acc, line| match line.kind {
            DiffLineKind::Added => LineDelta {
                added: acc.added + 1,
                ..acc
            },
            DiffLineKind::Removed => LineDelta {
                removed: acc.removed + 1,
                ..acc
            },
            DiffLineKind::Context => acc,
        })
}

/// Split a `raw_code` body into the owned line vector [`diff_lines`]
/// consumes, treating absent / empty bodies as zero lines.
///
/// Strips a single trailing `\n` (git's line frame — the same
/// engine-divergence normalization
/// [`crate::domain::pr_diff::reconstruct_model_sql_diffs`] applies: dbt-core
/// strips the terminator, dbt-fusion retains it), so both engines yield the
/// same line count. An empty or `None` body is zero lines (not one phantom
/// `""`), so a new/deleted node's count is the true authored-line count.
fn split_raw_lines(raw: Option<&str>) -> Vec<String> {
    match raw {
        None | Some("") => Vec::new(),
        Some(body) => body
            .strip_suffix('\n')
            .unwrap_or(body)
            .split('\n')
            .map(str::to_owned)
            .collect(),
    }
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

    // The 1-hop context halo (cute-dbt#428): for every DISCONNECTED modified
    // model (no connector path to any OTHER modified model), its immediate
    // parents + direct children, minus any node that is itself a seed or a
    // connector (those keep their stronger role — the dedup contract). The
    // anchor↔halo edges are the ONLY edges a halo node induces.
    let halo = halo_for_disconnected(&adjacency, &seeds, &connectors);

    let nodes = assemble_nodes(manifest, &seeds, &connectors, &halo.nodes, new, removed);
    // The induced subgraph over the ORIGINAL node set (seeds ∪ connectors ∪
    // removed) is unchanged — byte-stable for a halo-free PR — and the halo's
    // anchor edges are unioned on. A halo node never contributes a generic
    // induced edge (e.g. a spurious halo↔halo link); it carries only the
    // explicit anchor edges, honoring "edges only to/from their anchor".
    let mut edges = induced_edges(&adjacency, &nodes, &halo.nodes);
    merge_halo_edges(&mut edges, &halo.edges);

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

/// The 1-hop context halo and its anchor edges (cute-dbt#428).
///
/// `nodes` are the halo node ids (immediate parents + direct children of the
/// disconnected modified models, minus seeds and connectors); `edges` are the
/// anchor↔halo `depends_on` edges (the ONLY edges a halo node induces).
struct Halo<'m> {
    nodes: BTreeSet<&'m NodeId>,
    edges: BTreeSet<(&'m NodeId, &'m NodeId)>,
}

/// Compute the 1-hop context halo: for every DISCONNECTED modified model (a
/// seed with no directed lineage path to or from any *other* seed), its
/// immediate `depends_on` parents + direct children become halo context
/// nodes, and the producer→consumer edge between the seed and each neighbor
/// becomes an anchor edge.
///
/// Dedup (the issue's "keeps its stronger role" clause): a neighbor that is
/// itself a seed or a connector is **excluded** from the halo node set — it
/// already renders in its own (stronger) tier. The anchor *edge* to such a
/// neighbor is likewise dropped: were the neighbor a seed, the two seeds would
/// not be disconnected (they share a direct edge), so this only ever drops a
/// would-be edge to an already-present node, never a real lineage edge the
/// generic [`induced_edges`] pass already emits.
///
/// "Disconnected" is decided over the model→model lineage: a seed is
/// CONNECTED when some *other* seed is reachable from it forward (a
/// descendant) or backward (an ancestor) — exactly the condition under which
/// a connecting path (and thus a connector, for paths of length ≥ 2) exists.
/// A seed with no other seed reachable either way is disconnected and earns a
/// halo. A seed with no neighbors at all yields an empty halo (it still
/// renders alone — no halo is possible).
fn halo_for_disconnected<'m>(
    adjacency: &ModelAdjacency<'m>,
    seeds: &BTreeSet<&'m NodeId>,
    connectors: &BTreeSet<&'m NodeId>,
) -> Halo<'m> {
    let mut halo = Halo {
        nodes: BTreeSet::new(),
        edges: BTreeSet::new(),
    };
    for &seed in seeds {
        if is_connected_to_another_seed(adjacency, seed, seeds) {
            continue; // linked to another modified model ⇒ no halo
        }
        // The 1-hop ring: direct children (the anchor edge is seed → child)
        // and immediate parents (parent → seed). `Direction` carries which
        // adjacency map to read and how to orient the anchor edge; the
        // dedup (a neighbor that is itself a seed or connector keeps its
        // stronger role) lives in the shared collector.
        collect_halo_neighbors(
            &mut halo,
            adjacency,
            seed,
            seeds,
            connectors,
            Direction::Child,
        );
        collect_halo_neighbors(
            &mut halo,
            adjacency,
            seed,
            seeds,
            connectors,
            Direction::Parent,
        );
    }
    halo
}

/// Which 1-hop direction [`collect_halo_neighbors`] walks from an anchor seed.
#[derive(Clone, Copy)]
enum Direction {
    /// Direct children (read `forward`; the anchor edge is `seed → neighbor`).
    Child,
    /// Immediate parents (read `backward`; the anchor edge is `neighbor → seed`).
    Parent,
}

/// Collect the 1-hop neighbors of `seed` in one direction into `halo`, adding
/// each as a halo node + its anchor edge, and skipping any neighbor that is
/// itself a seed or a connector (the dedup: a stronger role wins).
fn collect_halo_neighbors<'m>(
    halo: &mut Halo<'m>,
    adjacency: &ModelAdjacency<'m>,
    seed: &'m NodeId,
    seeds: &BTreeSet<&'m NodeId>,
    connectors: &BTreeSet<&'m NodeId>,
    direction: Direction,
) {
    let map = match direction {
        Direction::Child => &adjacency.forward,
        Direction::Parent => &adjacency.backward,
    };
    for &neighbor in map.get(seed).into_iter().flatten() {
        if seeds.contains(neighbor) || connectors.contains(neighbor) {
            continue; // a neighbor with a stronger role keeps it
        }
        halo.nodes.insert(neighbor);
        let edge = match direction {
            Direction::Child => (seed, neighbor),
            Direction::Parent => (neighbor, seed),
        };
        halo.edges.insert(edge);
    }
}

/// `true` when some seed OTHER than `seed` is reachable from `seed` over the
/// model→model lineage in either direction (a descendant via `forward` or an
/// ancestor via `backward`) — i.e. `seed` lies on a directed lineage path
/// to/from another modified model and is therefore NOT disconnected.
///
/// A bounded BFS in each direction; the first foreign seed reached short-
/// circuits. Cycle-guarded by the `visited` set (a malformed cyclic manifest
/// cannot loop). The `seed` itself is never counted (it is the start, not a
/// "different" modified model).
fn is_connected_to_another_seed<'m>(
    adjacency: &ModelAdjacency<'m>,
    seed: &'m NodeId,
    seeds: &BTreeSet<&'m NodeId>,
) -> bool {
    [&adjacency.forward, &adjacency.backward]
        .into_iter()
        .any(|dir| reaches_a_foreign_seed(dir, seed, seeds))
}

/// Single-direction BFS from `seed` over `dir`, returning `true` as soon as a
/// seed other than `seed` is reached. Visited-guarded (deterministic, cycle-
/// safe); the start node is enqueued but never itself counts as a foreign
/// seed.
fn reaches_a_foreign_seed<'m>(
    dir: &BTreeMap<&'m NodeId, Vec<&'m NodeId>>,
    seed: &'m NodeId,
    seeds: &BTreeSet<&'m NodeId>,
) -> bool {
    let mut visited: BTreeSet<&NodeId> = BTreeSet::new();
    let mut queue: VecDeque<&NodeId> = VecDeque::new();
    queue.push_back(seed);
    visited.insert(seed);
    while let Some(current) = queue.pop_front() {
        if current != seed && seeds.contains(current) {
            return true; // a different modified model is reachable
        }
        for &next in dir.get(current).into_iter().flatten() {
            if visited.insert(next) {
                queue.push_back(next);
            }
        }
    }
    false
}

/// Union the halo's anchor edges onto the already-induced edge set, holding
/// the `(from, to)` lexicographic order (cute-dbt#428).
///
/// Building a fresh [`BTreeSet`] keyed by the owned `(from, to)` strings keeps
/// the merge deterministic and idempotent — a halo edge that an induced edge
/// already covers (it cannot, by the disconnected invariant, but the set
/// dedups regardless) collapses to one.
fn merge_halo_edges(edges: &mut Vec<PrDagEdge>, halo_edges: &BTreeSet<(&NodeId, &NodeId)>) {
    if halo_edges.is_empty() {
        return; // byte-stable for a halo-free PR (no re-sort, no realloc)
    }
    let mut all: BTreeSet<(&str, &str)> = edges
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect();
    for (from, to) in halo_edges {
        all.insert((from.as_str(), to.as_str()));
    }
    *edges = all
        .into_iter()
        .map(|(from, to)| PrDagEdge {
            from: from.to_owned(),
            to: to.to_owned(),
        })
        .collect();
}

/// Assemble the node list: modified seeds (New / Modified) ∪ connectors
/// (quiet, `is_connector`) ∪ halo (quiet, `is_halo`) ∪ removed (Deleted).
/// Node-id-ordered via the [`BTreeMap`] keying, so the output is
/// deterministic.
///
/// The insertion order encodes the dedup precedence (`insert` overwrites,
/// `or_insert_with` defers): seeds win over everything, connectors over halo,
/// halo over a removed ghost. By construction `halo` already excludes seeds
/// and connectors ([`halo_for_disconnected`]), so the `or_insert_with` guard
/// is the belt-and-suspenders that pins the contract.
fn assemble_nodes(
    manifest: &Manifest,
    seeds: &BTreeSet<&NodeId>,
    connectors: &BTreeSet<&NodeId>,
    halo: &BTreeSet<&NodeId>,
    new: &BTreeSet<NodeId>,
    removed: &[NodeId],
) -> Vec<PrDagNode> {
    // Keyed by node id for deterministic order + idempotent membership
    // (a node is at most one of seed / connector / halo / removed by
    // construction, but the map collapses any accidental overlap to a
    // single deterministic node).
    let mut by_id: BTreeMap<&NodeId, PrDagNode> = BTreeMap::new();

    for &id in seeds {
        let state = if new.contains(id) {
            PrDagState::New
        } else {
            PrDagState::Modified
        };
        by_id.insert(id, pr_dag_node(manifest, id, state, false, false));
    }
    for &id in connectors {
        // Connectors are unchanged carriers; render-quiet. State is the
        // structural placeholder `Modified` flagged `is_connector`.
        by_id
            .entry(id)
            .or_insert_with(|| pr_dag_node(manifest, id, PrDagState::Modified, true, false));
    }
    for &id in halo {
        // Halo nodes are unchanged context for a disconnected modified model;
        // render-quiet. State is the structural placeholder `Modified` flagged
        // `is_halo` (never `is_connector` — a distinct role).
        by_id
            .entry(id)
            .or_insert_with(|| pr_dag_node(manifest, id, PrDagState::Modified, false, true));
    }
    for id in removed {
        // A removed node is absent from the current manifest, so its bare
        // name falls back to the id's leaf-free form (see `bare_name_for`).
        by_id
            .entry(id)
            .or_insert_with(|| pr_dag_node(manifest, id, PrDagState::Deleted, false, false));
    }

    by_id.into_values().collect()
}

/// Build a single [`PrDagNode`] for `id` with the given state / connector /
/// halo role, resolving the bare name from the current manifest when present.
fn pr_dag_node(
    manifest: &Manifest,
    id: &NodeId,
    state: PrDagState,
    is_connector: bool,
    is_halo: bool,
) -> PrDagNode {
    PrDagNode {
        id: id.as_str().to_owned(),
        name: bare_name_for(manifest, id),
        state,
        is_connector,
        is_halo,
        // Slice A topology emits 0/0; the counts are computed by the
        // arm-specific fns (`pr_dag_lines_from_diff` /
        // `pr_dag_lines_from_raw_code`) and folded on in Slice C (#403/#404).
        lines_added: 0,
        lines_removed: 0,
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
/// endpoints are in the node set AND **neither endpoint is a halo node**.
/// Sourced from the precomputed forward adjacency, lexicographically ordered
/// by `(from, to)`, deduplicated.
///
/// Halo nodes are excluded from this generic pass because a halo node induces
/// edges **only to/from its anchor** (cute-dbt#428) — those anchor edges are
/// supplied explicitly by [`merge_halo_edges`], so letting the generic pass
/// also emit (e.g.) a spurious halo↔halo link between two independent isolated
/// anchors' neighbors would over-draw the context. For a halo-free PR the
/// `halo` set is empty and this is exactly the original induced subgraph
/// (byte-stable).
fn induced_edges(
    adjacency: &ModelAdjacency<'_>,
    nodes: &[PrDagNode],
    halo: &BTreeSet<&NodeId>,
) -> Vec<PrDagEdge> {
    let halo_ids: BTreeSet<&str> = halo.iter().map(|id| id.as_str()).collect();
    let in_set: BTreeSet<&str> = nodes
        .iter()
        .map(|n| n.id.as_str())
        .filter(|id| !halo_ids.contains(id))
        .collect();
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

/// Fold a per-node [`LineDelta`] onto every node of `graph` in place (the
/// Slice C population step, cute-dbt#404).
///
/// `delta_for` is the caller's arm-specific counter — the run loop binds it
/// to [`pr_dag_lines_from_diff`] (pr-diff arm) or
/// [`pr_dag_lines_from_raw_code`] (baseline arm), each keyed by the node's
/// id. Keeping the fold here (rather than inline in the cli) keeps the
/// population a pure, unit-testable domain step over the already-computed
/// topology: [`compute_pr_dag`] emits `0/0` (byte-neutral to goldens), and
/// this is the single place that overwrites them. A node whose
/// `delta_for` returns `0/0` (a connector, or a node whose file is absent
/// from the diff) keeps its zero counts — the documented unchanged-carrier
/// contract.
pub fn populate_line_counts(graph: &mut PrDagGraph, delta_for: impl Fn(&PrDagNode) -> LineDelta) {
    for node in &mut graph.nodes {
        let delta = delta_for(node);
        node.lines_added = delta.added;
        node.lines_removed = delta.removed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{
        Checksum, DependsOn, ManifestMetadata, Node, NodeConfig, NodeId,
    };
    use crate::domain::pr_diff::{FileHunks, Hunk, PrDiff};
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
    fn a_node_downstream_of_only_one_modified_model_is_a_halo_not_a_connector() {
        // A (modified) -> B (unmodified leaf), A the only modified model. B is
        // 1-hop context, NOT a connector (there is no SECOND modified model on
        // the other side). Since cute-dbt#428, A is disconnected (no other
        // modified model), so B joins as its HALO node — present, but flagged
        // `is_halo`, never `is_connector`.
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
        let b = node_of(&graph, "model.s.b");
        assert!(b.is_halo, "downstream-of-one is now a halo node");
        assert!(
            !b.is_connector,
            "downstream-of-one is not a between-connector"
        );
    }

    #[test]
    fn a_convergence_sink_below_two_modified_models_is_a_halo_not_a_connector() {
        // A -> C, B -> C, with A and B modified. C is a COMMON DESCENDANT
        // (a sink) of both — it is downstream of two modified models but
        // BETWEEN neither: C ∈ DESC(M) but C ∉ ANC(M). Under the strict
        // (DESC ∩ ANC) \ M definition, a convergence sink is NOT a connector.
        // Since cute-dbt#428: A and B are roots with NO directed path between
        // them ⇒ both are disconnected, so C is their SHARED 1-hop halo child
        // (present once, flagged `is_halo`, never `is_connector`).
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
        let c = node_of(&graph, "model.s.c");
        assert!(c.is_halo, "the convergence sink is a shared halo child");
        assert!(
            !c.is_connector,
            "a sink is downstream-of-both, between-neither"
        );
        assert_eq!(
            ids_of(&graph),
            BTreeSet::from(["model.s.a", "model.s.b", "model.s.c"]),
        );
        // C is the shared halo of two disconnected anchors ⇒ an anchor edge to
        // EACH.
        assert_eq!(
            edges_of(&graph),
            BTreeSet::from([("model.s.a", "model.s.c"), ("model.s.b", "model.s.c"),]),
        );
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
    fn a_single_modified_model_has_no_connectors_and_halos_its_one_parent() {
        // A -> B, only B modified. B has no second modified model anywhere ⇒
        // disconnected ⇒ no connector, but its one parent A becomes a halo
        // node (cute-dbt#428). The anchor edge A->B is induced.
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
        assert_eq!(ids_of(&graph), BTreeSet::from(["model.s.a", "model.s.b"]));
        assert!(graph.nodes.iter().all(|n| !n.is_connector), "no connectors");
        assert!(
            node_of(&graph, "model.s.a").is_halo,
            "the one parent is halo"
        );
        assert_eq!(
            edges_of(&graph),
            BTreeSet::from([("model.s.a", "model.s.b")])
        );
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
        // A (modified) -> B (halo child) -> C (2-hop, EXCLUDED). A is the only
        // modified model ⇒ disconnected ⇒ B is its halo child (so A->B IS
        // induced as an anchor edge), but C is 2 hops away — outside the node
        // set — so the B->C edge must NOT appear.
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert!(
            !ids_of(&graph).contains("model.s.c"),
            "a 2-hop node is outside the halo node set",
        );
        assert_eq!(
            edges_of(&graph),
            BTreeSet::from([("model.s.a", "model.s.b")]),
            "only the anchor edge A->B; the B->C edge to an excluded node is not induced",
        );
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

    // ---- per-node line counts (cute-dbt#403 — Slice B) ------------------

    /// A `--unified=0`-shaped hunk over the given `+`/`-` bodies (the
    /// `new_start` / `new_len` are nominal — `pr_dag_lines_from_diff` reads
    /// only the body lengths, so the footprint is unconstrained here).
    fn hunk(removed: &[&str], added: &[&str]) -> Hunk {
        Hunk {
            new_start: 1,
            new_len: added.len(),
            removed_lines: removed.iter().map(|s| (*s).to_owned()).collect(),
            added_lines: added.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// A [`NormalizedDiffIndex`] mapping each `(path, hunks)` (no project-root
    /// strip, so manifest `original_file_path` keys resolve identically).
    fn index_of(files: Vec<(&str, Vec<Hunk>)>) -> NormalizedDiffIndex {
        let diff = PrDiff {
            renames: Vec::new(),
            deleted: Vec::new(),
            files: files
                .into_iter()
                .map(|(path, hunks)| FileHunks {
                    path: path.to_owned(),
                    hunks,
                })
                .collect(),
        };
        NormalizedDiffIndex::new(&diff, None)
    }

    // -- pr-diff arm ------------------------------------------------------

    #[test]
    fn pr_diff_arm_counts_the_added_and_removed_hunk_lines_for_the_file() {
        // A modified model: 3 added, 2 removed across two hunks on its file.
        let index = index_of(vec![(
            "models/stg_orders.sql",
            vec![
                hunk(&["old a", "old b"], &["new a", "new b", "new c"]),
                Hunk {
                    new_start: 20,
                    new_len: 0,
                    removed_lines: Vec::new(),
                    added_lines: Vec::new(),
                },
            ],
        )]);
        let delta = pr_dag_lines_from_diff(Some("models/stg_orders.sql"), &index);
        assert_eq!(
            delta,
            LineDelta {
                added: 3,
                removed: 2
            },
            "added/removed summed across every hunk on the file",
        );
    }

    #[test]
    fn pr_diff_arm_sums_added_and_removed_across_multiple_files_only_for_the_node() {
        // The index holds two files; the node resolves to exactly one — the
        // OTHER file's counts must NOT leak into this node's delta.
        let index = index_of(vec![
            ("models/a.sql", vec![hunk(&[], &["+1", "+2"])]),
            ("models/b.sql", vec![hunk(&["-1", "-2", "-3"], &[])]),
        ]);
        assert_eq!(
            pr_dag_lines_from_diff(Some("models/a.sql"), &index),
            LineDelta {
                added: 2,
                removed: 0
            },
        );
        assert_eq!(
            pr_dag_lines_from_diff(Some("models/b.sql"), &index),
            LineDelta {
                added: 0,
                removed: 3
            },
        );
    }

    #[test]
    fn pr_diff_arm_is_zero_for_a_connector_whose_file_is_absent_from_the_diff() {
        // A connector / unchanged carrier: the diff touches another file, so
        // this node's file is absent ⇒ 0/0 (never a panic).
        let index = index_of(vec![("models/other.sql", vec![hunk(&[], &["x"])])]);
        let delta = pr_dag_lines_from_diff(Some("models/connector.sql"), &index);
        assert_eq!(delta, LineDelta::default(), "absent file ⇒ 0/0");
    }

    #[test]
    fn pr_diff_arm_is_zero_when_the_node_has_no_original_file_path() {
        // A deleted ghost / synthetic node has no current `original_file_path`
        // ⇒ the diff arm cannot key it ⇒ 0/0 (the baseline arm owns deletions).
        let index = index_of(vec![("models/a.sql", vec![hunk(&["x"], &["y"])])]);
        assert_eq!(
            pr_dag_lines_from_diff(None, &index),
            LineDelta::default(),
            "no original_file_path ⇒ 0/0 on the diff arm",
        );
    }

    #[test]
    fn pr_diff_arm_over_an_empty_index_is_zero() {
        let index = index_of(vec![]);
        assert_eq!(
            pr_dag_lines_from_diff(Some("models/a.sql"), &index),
            LineDelta::default(),
        );
    }

    // -- baseline arm -----------------------------------------------------

    #[test]
    fn baseline_arm_counts_added_and_removed_from_raw_code_old_to_new() {
        // old: a, b, c   new: a, B2, c, d  ⇒ b→B2 is one remove + one add,
        // d is one add ⇒ added=2, removed=1.
        let old = "a\nb\nc";
        let new = "a\nB2\nc\nd";
        let delta = pr_dag_lines_from_raw_code(Some(old), Some(new));
        assert_eq!(
            delta,
            LineDelta {
                added: 2,
                removed: 1
            },
            "raw_code old→new line diff counts",
        );
    }

    #[test]
    fn baseline_arm_unchanged_raw_code_is_zero() {
        // A connector / unchanged carrier in baseline mode: identical
        // raw_code ⇒ all-Context ⇒ 0/0.
        let same = "select 1\nfrom t\nwhere x";
        assert_eq!(
            pr_dag_lines_from_raw_code(Some(same), Some(same)),
            LineDelta::default(),
            "identical raw_code ⇒ 0/0",
        );
    }

    #[test]
    fn baseline_arm_new_node_counts_every_current_line_as_added() {
        // A new node: absent from the baseline (old = None) ⇒ every current
        // line is an addition, nothing removed.
        let new = "select 1\nfrom t\nwhere x";
        assert_eq!(
            pr_dag_lines_from_raw_code(None, Some(new)),
            LineDelta {
                added: 3,
                removed: 0
            },
            "new node ⇒ all current lines added",
        );
    }

    #[test]
    fn baseline_arm_deleted_ghost_counts_every_baseline_line_as_removed() {
        // A deleted ghost: absent from the current manifest (new = None) ⇒
        // every baseline line is a removal — the removed-everything count.
        let old = "select 1\nfrom t\nwhere x\ngroup by 1";
        assert_eq!(
            pr_dag_lines_from_raw_code(Some(old), None),
            LineDelta {
                added: 0,
                removed: 4
            },
            "deleted ghost ⇒ all baseline lines removed",
        );
    }

    #[test]
    fn baseline_arm_both_absent_is_zero() {
        // A synthetic / non-SQL node with no raw_code either side ⇒ 0/0.
        assert_eq!(pr_dag_lines_from_raw_code(None, None), LineDelta::default(),);
    }

    #[test]
    fn baseline_arm_empty_and_absent_raw_code_are_equivalent() {
        // `Some("")` (an empty body — some node types ship `raw_code: ""`)
        // is zero lines, identical to `None`: a one-line addition either way.
        let new = "select 1";
        assert_eq!(
            pr_dag_lines_from_raw_code(Some(""), Some(new)),
            pr_dag_lines_from_raw_code(None, Some(new)),
            "Some(\"\") and None are both zero-line bodies",
        );
        assert_eq!(
            pr_dag_lines_from_raw_code(Some(""), Some(new)),
            LineDelta {
                added: 1,
                removed: 0
            },
        );
    }

    #[test]
    fn baseline_arm_trailing_newline_does_not_inflate_the_count() {
        // dbt-fusion retains the file's trailing `\n` on raw_code; dbt-core
        // strips it. A single trailing terminator must NOT register as an
        // extra phantom line on either side — fusion-vs-core parity.
        let core = "select 1\nfrom t"; // dbt-core: no trailing newline
        let fusion = "select 1\nfrom t\n"; // dbt-fusion: trailing newline
        // Same body, different terminator framing ⇒ no change counted.
        assert_eq!(
            pr_dag_lines_from_raw_code(Some(core), Some(fusion)),
            LineDelta::default(),
            "a lone trailing-newline difference is not a line change",
        );
        // And a real EOF blank line survives (only ONE terminator stripped):
        // "a\n\n" → lines [a, ""] vs "a" → [a] ⇒ one added blank line.
        assert_eq!(
            pr_dag_lines_from_raw_code(Some("a"), Some("a\n\n")),
            LineDelta {
                added: 1,
                removed: 0
            },
            "a genuine EOF blank line is a real added line",
        );
    }

    // -- field plumbing ---------------------------------------------------

    #[test]
    fn compute_pr_dag_emits_zero_line_counts_on_every_node() {
        // Slice A topology is unchanged: every node carries 0/0 counts until
        // Slice C folds the arm-specific deltas on. (Byte-neutral to goldens.)
        let manifest = chain_abcd();
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.d"]),
            &new_of(&["model.s.a"]),
            &[NodeId::new("model.s.gone")],
        );
        assert!(
            graph
                .nodes
                .iter()
                .all(|n| n.lines_added == 0 && n.lines_removed == 0),
            "compute_pr_dag emits 0/0 line counts (Slice C populates them)",
        );
    }

    #[test]
    fn populate_line_counts_overwrites_every_node_from_the_closure() {
        // Slice C population: the topology emits 0/0, then `populate_line_counts`
        // folds the arm-specific delta onto each node. The closure keys on the
        // node id; an unkeyed node keeps 0/0 (the unchanged-carrier contract).
        let manifest = chain_abcd();
        let mut graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.d"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        populate_line_counts(&mut graph, |node| match node.id.as_str() {
            "model.s.a" => LineDelta {
                added: 5,
                removed: 2,
            },
            "model.s.d" => LineDelta {
                added: 0,
                removed: 9,
            },
            // b, c are connectors — the closure returns 0/0 for them.
            _ => LineDelta::default(),
        });
        let a = node_of(&graph, "model.s.a");
        assert_eq!((a.lines_added, a.lines_removed), (5, 2));
        let d = node_of(&graph, "model.s.d");
        assert_eq!((d.lines_added, d.lines_removed), (0, 9));
        // The connectors keep the zero default.
        let b = node_of(&graph, "model.s.b");
        assert_eq!((b.lines_added, b.lines_removed), (0, 0));
    }

    // ---- 1-hop context halo for disconnected modified models (#428) -----

    /// The `is_halo`-flagged node ids of a computed graph.
    fn halo_ids_of(graph: &PrDagGraph) -> BTreeSet<&str> {
        graph
            .nodes
            .iter()
            .filter(|n| n.is_halo)
            .map(|n| n.id.as_str())
            .collect()
    }

    /// The `is_connector`-flagged node ids of a computed graph.
    fn connector_ids_of(graph: &PrDagGraph) -> BTreeSet<&str> {
        graph
            .nodes
            .iter()
            .filter(|n| n.is_connector)
            .map(|n| n.id.as_str())
            .collect()
    }

    // -- case: a CONNECTED cluster gets NO halo ---------------------------

    #[test]
    fn a_connected_cluster_of_modified_models_gets_no_halo() {
        // A -> B -> C, A and C modified, B the connector. There IS a path
        // between two modified models ⇒ neither A nor C is disconnected ⇒ no
        // halo. (The unchanged D below A and the unchanged E above C must NOT
        // be pulled in — they would only appear were A/C disconnected.)
        let manifest = manifest_of(&[
            ("model.s.up", "model", &[]),
            ("model.s.a", "model", &["model.s.up"]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b"]),
            ("model.s.down", "model", &["model.s.c"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.c"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert!(
            halo_ids_of(&graph).is_empty(),
            "a connected cluster pulls in no halo context",
        );
        // The node set is exactly the connected cluster (seeds + connector).
        assert_eq!(
            ids_of(&graph),
            BTreeSet::from(["model.s.a", "model.s.b", "model.s.c"]),
        );
    }

    #[test]
    fn two_directly_adjacent_modified_models_get_no_halo() {
        // A -> B, both modified (a connecting EDGE, zero connectors). They are
        // linked directly ⇒ connected ⇒ no halo, even though there is no
        // intermediate connector node.
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.leaf", "model", &["model.s.b"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.b"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert!(
            halo_ids_of(&graph).is_empty(),
            "directly-adjacent modified models are connected ⇒ no halo",
        );
        assert_eq!(ids_of(&graph), BTreeSet::from(["model.s.a", "model.s.b"]));
    }

    // -- case: a SINGLE isolated model WITH neighbors gains a halo ---------

    #[test]
    fn a_single_isolated_modified_model_with_neighbors_gains_its_one_hop_halo() {
        // up -> M -> down, only M modified. M has no other modified model on
        // any path ⇒ disconnected ⇒ its immediate parent `up` and direct child
        // `down` become halo context nodes.
        let manifest = manifest_of(&[
            ("model.s.up", "model", &[]),
            ("model.s.m", "model", &["model.s.up"]),
            ("model.s.down", "model", &["model.s.m"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.m"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert_eq!(
            halo_ids_of(&graph),
            BTreeSet::from(["model.s.up", "model.s.down"]),
            "the disconnected model's 1-hop parents + children are the halo",
        );
        // The anchor M is a genuine modified node, never flagged halo/connector.
        let m = node_of(&graph, "model.s.m");
        assert_eq!(m.state, PrDagState::Modified);
        assert!(!m.is_halo && !m.is_connector);
        // Halo nodes are quiet placeholders: state=Modified, is_halo, never a
        // connector.
        for hid in ["model.s.up", "model.s.down"] {
            let h = node_of(&graph, hid);
            assert!(h.is_halo, "{hid} flagged halo");
            assert!(!h.is_connector, "{hid} is NOT a connector (distinct role)");
            assert_eq!(h.state, PrDagState::Modified, "halo quiet-tier state");
        }
        // Edges run ONLY between the anchor and its halo neighbors.
        assert_eq!(
            edges_of(&graph),
            BTreeSet::from([("model.s.up", "model.s.m"), ("model.s.m", "model.s.down"),]),
            "halo induces edges only to/from the anchor",
        );
    }

    #[test]
    fn a_halo_includes_all_parents_and_all_children_of_the_isolated_anchor() {
        // p1, p2 -> M -> c1, c2, only M modified. ALL four direct neighbors
        // join the halo (the immediate parents + the direct children, the full
        // 1-hop ring), and nothing further (gp -> p1, the grandparent, stays
        // out — 2 hops).
        let manifest = manifest_of(&[
            ("model.s.gp", "model", &[]),
            ("model.s.p1", "model", &["model.s.gp"]),
            ("model.s.p2", "model", &[]),
            ("model.s.m", "model", &["model.s.p1", "model.s.p2"]),
            ("model.s.c1", "model", &["model.s.m"]),
            ("model.s.c2", "model", &["model.s.m"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.m"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert_eq!(
            halo_ids_of(&graph),
            BTreeSet::from(["model.s.p1", "model.s.p2", "model.s.c1", "model.s.c2",]),
            "the full 1-hop ring (all parents + all children), grandparent excluded",
        );
        assert!(
            !ids_of(&graph).contains("model.s.gp"),
            "a 2-hop grandparent is not halo context",
        );
    }

    // -- case: an isolated model with NO neighbors renders ALONE ----------

    #[test]
    fn a_single_isolated_modified_model_with_no_neighbors_renders_alone() {
        // M modified, no depends_on and nothing consumes it ⇒ disconnected
        // AND zero neighbors ⇒ no halo is possible; it still renders alone.
        let manifest = manifest_of(&[
            ("model.s.m", "model", &[]),
            ("model.s.unrelated", "model", &[]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.m"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert_eq!(ids_of(&graph), BTreeSet::from(["model.s.m"]));
        assert!(halo_ids_of(&graph).is_empty(), "no neighbors ⇒ no halo");
        assert!(graph.edges.is_empty());
    }

    // -- case: TWO independent isolated models ⇒ TWO independent halos ----

    #[test]
    fn two_disconnected_modified_models_get_two_independent_halos() {
        // Cluster 1:  p1 -> M1 -> c1.   Cluster 2:  p2 -> M2 -> c2.
        // M1 and M2 are in separate components (no path between them) ⇒ both
        // disconnected ⇒ each gets its own 1-hop halo, independent of the
        // other.
        let manifest = manifest_of(&[
            ("model.s.p1", "model", &[]),
            ("model.s.m1", "model", &["model.s.p1"]),
            ("model.s.c1", "model", &["model.s.m1"]),
            ("model.s.p2", "model", &[]),
            ("model.s.m2", "model", &["model.s.p2"]),
            ("model.s.c2", "model", &["model.s.m2"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.m1", "model.s.m2"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert_eq!(
            halo_ids_of(&graph),
            BTreeSet::from(["model.s.p1", "model.s.c1", "model.s.p2", "model.s.c2",]),
            "each isolated anchor contributes its own 1-hop halo",
        );
        // Edges: each anchor wired only to its own halo (no cross-cluster edge).
        assert_eq!(
            edges_of(&graph),
            BTreeSet::from([
                ("model.s.m1", "model.s.c1"),
                ("model.s.m2", "model.s.c2"),
                ("model.s.p1", "model.s.m1"),
                ("model.s.p2", "model.s.m2"),
            ]),
        );
    }

    // -- case: a halo node that is itself modified DEDUPS to modified -----

    #[test]
    fn a_halo_node_that_is_also_modified_keeps_its_modified_role() {
        // up -> M -> N, with BOTH M and N modified. M and N are directly
        // adjacent (connected) ⇒ NO halo for either; even though `up` is M's
        // parent, M is connected to N, so M earns no halo and `up` stays out.
        // This pins the strongest dedup: a connected pair never spawns a halo,
        // so a neighbor can never be wrongly flagged.
        let manifest = manifest_of(&[
            ("model.s.up", "model", &[]),
            ("model.s.m", "model", &["model.s.up"]),
            ("model.s.n", "model", &["model.s.m"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.m", "model.s.n"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert!(halo_ids_of(&graph).is_empty());
        assert_eq!(ids_of(&graph), BTreeSet::from(["model.s.m", "model.s.n"]));
    }

    #[test]
    fn a_neighbor_that_is_itself_a_modified_seed_keeps_modified_not_halo() {
        // Two DISCONNECTED-from-each-other-looking anchors that actually share
        // a neighbor edge: M1 (isolated from M2 via no directed path) has child
        // X; M2 has child X too. X is a plain unmodified neighbor. Construct so
        // X is ALSO modified to exercise the dedup: M1 -> X, with X modified.
        // Then M1 and X are directly adjacent (connected) — so M1 is NOT
        // disconnected and spawns no halo; X, modified, is its own seed.
        let manifest = manifest_of(&[
            ("model.s.m1", "model", &[]),
            ("model.s.x", "model", &["model.s.m1"]),
            ("model.s.solo", "model", &[]),
            ("model.s.solo_child", "model", &["model.s.solo"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.m1", "model.s.x", "model.s.solo"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        // X is a seed — never a halo node.
        assert!(!node_of(&graph, "model.s.x").is_halo);
        assert_eq!(node_of(&graph, "model.s.x").state, PrDagState::Modified);
        // M1—X are connected (direct edge) ⇒ no halo for that cluster.
        // `solo` is disconnected ⇒ its child `solo_child` is a halo node.
        assert_eq!(
            halo_ids_of(&graph),
            BTreeSet::from(["model.s.solo_child"]),
            "only the disconnected `solo` anchor's child is halo; X stays a seed",
        );
    }

    #[test]
    fn a_halo_node_shared_between_two_isolated_anchors_appears_once() {
        // p1 -> shared <- via: M1 -> shared, M2 -> shared, with M1 and M2 each
        // isolated (no directed path between them). `shared` is a direct child
        // of BOTH ⇒ it is a halo node for both anchors but appears EXACTLY once
        // (BTreeSet dedup), carrying edges to BOTH anchors.
        let manifest = manifest_of(&[
            ("model.s.m1", "model", &[]),
            ("model.s.m2", "model", &[]),
            ("model.s.shared", "model", &["model.s.m1", "model.s.m2"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.m1", "model.s.m2"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        // `shared` is a common DESCENDANT of two modified models (a convergence
        // sink) — NOT a connector under (DESC ∩ ANC)\M; here it is the shared
        // 1-hop halo of two disconnected anchors.
        assert!(
            connector_ids_of(&graph).is_empty(),
            "a sink is not a connector"
        );
        assert_eq!(
            halo_ids_of(&graph),
            BTreeSet::from(["model.s.shared"]),
            "the shared neighbor appears exactly once",
        );
        // Exactly one `shared` node in the list (dedup, not duplicated).
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|n| n.id == "model.s.shared")
                .count(),
            1,
        );
        // It carries an anchor edge to EACH isolated anchor.
        assert_eq!(
            edges_of(&graph),
            BTreeSet::from([
                ("model.s.m1", "model.s.shared"),
                ("model.s.m2", "model.s.shared"),
            ]),
        );
    }

    // -- case: a halo node that is a CONNECTOR keeps the connector role ---

    #[test]
    fn a_neighbor_that_is_a_connector_for_another_pair_keeps_connector_role() {
        // A -> K -> C with A,C modified ⇒ K is a connector. Separately, an
        // isolated modified model M with K as a direct child: K must keep its
        // CONNECTOR role (stronger), never be re-flagged halo.
        //   A -> K -> C   (A,C modified ⇒ K connector)
        //   M -> K        (M isolated, K is M's child)
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.k", "model", &["model.s.a", "model.s.m"]),
            ("model.s.c", "model", &["model.s.k"]),
            ("model.s.m", "model", &[]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.c", "model.s.m"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        // K is the A->C connector; M is connected to A/C THROUGH K? No — M's
        // only link is M->K->C, a directed path M ... C, so M IS connected to C.
        // Thus M is NOT disconnected and spawns no halo. K stays a connector.
        let k = node_of(&graph, "model.s.k");
        assert!(k.is_connector, "K keeps its connector role");
        assert!(!k.is_halo, "K is never re-flagged as halo");
        assert!(
            halo_ids_of(&graph).is_empty(),
            "M reaches C through K ⇒ M is connected ⇒ no halo",
        );
    }

    // -- determinism + acyclicity carry over to the halo node set ---------

    #[test]
    fn the_halo_graph_is_acyclic_and_order_independent() {
        let forward = manifest_of(&[
            ("model.s.up", "model", &[]),
            ("model.s.m", "model", &["model.s.up"]),
            ("model.s.down", "model", &["model.s.m"]),
        ]);
        let reversed = manifest_of(&[
            ("model.s.down", "model", &["model.s.m"]),
            ("model.s.m", "model", &["model.s.up"]),
            ("model.s.up", "model", &[]),
        ]);
        let m = modified_of(&["model.s.m"]);
        let g1 = compute_pr_dag(&forward, &m, &new_of(&[]), NO_REMOVED);
        let g2 = compute_pr_dag(&reversed, &m, &new_of(&[]), NO_REMOVED);
        assert_eq!(g1, g2, "the halo graph is a pure function of the facts");
        assert!(is_acyclic(&g1), "the halo-augmented mini-DAG stays acyclic");
    }

    #[test]
    fn a_disconnected_seed_alongside_a_connected_cluster_only_halos_the_isolated_one() {
        // Cluster: A -> B -> C (A,C modified, B connector). Isolated: M with a
        // child leaf. The connected cluster gets NO halo; only M's leaf does.
        let manifest = manifest_of(&[
            ("model.s.a", "model", &[]),
            ("model.s.b", "model", &["model.s.a"]),
            ("model.s.c", "model", &["model.s.b"]),
            ("model.s.m", "model", &[]),
            ("model.s.leaf", "model", &["model.s.m"]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.a", "model.s.c", "model.s.m"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        assert_eq!(connector_ids_of(&graph), BTreeSet::from(["model.s.b"]));
        assert_eq!(
            halo_ids_of(&graph),
            BTreeSet::from(["model.s.leaf"]),
            "only the isolated M's neighbor is halo; the A-C cluster has none",
        );
    }

    // -- a halo node never bleeds non-anchor induced edges ----------------

    #[test]
    fn halo_nodes_of_independent_anchors_do_not_link_to_each_other() {
        // M1 -> X, M2 -> X is the shared-sink case; here force a DIFFERENT
        // shape where M1's child could spuriously edge M2's child: arrange
        // M1 -> h1 -> h2 <- M2 with M1, M2 isolated. h1 is M1's child, h2 is
        // M2's child; h1 -> h2 is a real depends_on edge. Because h1, h2 are
        // halo nodes, that h1->h2 edge must NOT be induced — only the anchor
        // edges M1->h1 and M2->h2 appear.
        let manifest = manifest_of(&[
            ("model.s.m1", "model", &[]),
            ("model.s.h1", "model", &["model.s.m1"]),
            ("model.s.h2", "model", &["model.s.h1", "model.s.m2"]),
            ("model.s.m2", "model", &[]),
        ]);
        let graph = compute_pr_dag(
            &manifest,
            &modified_of(&["model.s.m1", "model.s.m2"]),
            &new_of(&[]),
            NO_REMOVED,
        );
        // M1 reaches M2? M1 -> h1 -> h2 <- M2: there is NO directed path from
        // M1 to M2 (h2 <- M2 is M2 -> h2, the wrong direction), nor M2 to M1.
        // So both are disconnected. h1 ∈ halo(M1), h2 ∈ halo(M2).
        assert_eq!(
            halo_ids_of(&graph),
            BTreeSet::from(["model.s.h1", "model.s.h2"]),
        );
        // The h1->h2 edge is BETWEEN two halo nodes ⇒ excluded; only the two
        // anchor edges survive.
        assert_eq!(
            edges_of(&graph),
            BTreeSet::from([("model.s.m1", "model.s.h1"), ("model.s.m2", "model.s.h2"),]),
            "halo↔halo edges are not induced; only anchor edges appear",
        );
    }
}
