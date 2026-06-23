//! Cross-model column lineage — trace-to-source + downstream blast-radius
//! (cute-dbt#450, CLL-4, the v0.2 explorer headline).
//!
//! CLL-2/CLL-3 resolve column provenance **inside one model** (the
//! intra-model `CteGraph::column_edges`). An intra-model trace dead-ends at
//! the first `ref()` boundary — the staging value lives there. CLL-4 walks
//! PAST that boundary: it runs the intra resolver over EVERY model to build
//! the **project-wide output-column map** (cute-dbt's zero-compute
//! catalog-equivalent, derived from explicit SQL), then **stitches at
//! `ref()` boundaries** — an upstream model's terminal output column becomes
//! the source column for a `FROM M`/`ref` leaf in a downstream model. This
//! is where the founder headline (affordance C — trace-a-column-to-its-
//! SOURCE-field) lives, because a `source()` is by definition outside the
//! model.
//!
//! This module is PURE DOMAIN (std + serde only — the
//! `tests/domain_clean_arch.rs` gate). It consumes:
//!   - the [`RelationIndex`] — a normalized `(database, schema, identifier)`
//!     key → [`NodeId`] map built AT INGESTION (refinement 3, the hardest
//!     correctness seam's mitigation). It is NEVER a raw `relation_name`
//!     string-match: aliasing / `identifier` config / case-folding make a
//!     raw string-match a correctness hazard.
//!   - [`ModelLineage`] (`DagFacts.lineage`, S0) — the ONE full-manifest
//!     `depends_on` inversion. `backward` (consumer → producers) drives the
//!     trace-to-source walk; `forward` (producer → consumers, the
//!     `child_map`) drives the downstream blast-radius. This module NEVER
//!     re-inverts `depends_on` (the cute-dbt#443 single-inversion seam).
//!   - each model's intra-model facts — its terminal OUTPUT columns and the
//!     bare leaf relations its terminal projection reads (extracted by the
//!     adapter from the already-parsed `CteGraph`; never a second parse).
//!
//! **Never a false claim (the load-bearing honesty contract):** the
//! cross-model stitch attributes a leaf to an upstream `NodeId` ONLY when
//! that node is BOTH (a) an actual `depends_on` producer of the downstream
//! model AND (b) the UNIQUE producer whose normalized identifier matches the
//! leaf. A leaf that does not uniquely normalize-join against the model's
//! real producers degrades to [`StitchOutcome::Opaque`] — it is NEVER
//! attributed to the wrong upstream. A `SELECT *` over a known modeled
//! upstream resolves (the derived projection map IS the catalog-equivalent);
//! a `*` over an unknown external relation stays Opaque; chains of `*`
//! compound Opaque (honest thinning, no catalog).
//!
//! **Scope-as-parameter (architectural):** the project-wide graph is the
//! EXPLORER's compute envelope (epic #99, `FullProject` scope). The per-model
//! report path consumes ONLY the intra-model slice and MUST NOT build this
//! graph — see the explorer-arm caller in `adapters::explore`. This module
//! has no report-path caller by construction.

use crate::domain::lineage::ModelLineage;
use crate::domain::manifest::{Manifest, NodeId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// A normalized relation identity — the `(database, schema, identifier)`
/// 3-tuple, every part ASCII-lowercased (case-insensitive equality is the
/// engine's join-key contract — `dbt-ident::Ident` lowercases on `Hash` /
/// `eq_ignore_ascii_case` on `PartialEq`; fusion-research §4.4). Mirrors the
/// engine's `FullyQualifiedName { catalog, schema, table }` STRUCTURE
/// (fusion-research §4.5) — a 3-tuple of case-folded parts, NOT the
/// adapter-quoted `relation_name` string.
///
/// `database`/`schema` are `Option` because dbt may omit them (a
/// single-segment or two-segment `relation_name`); `identifier` is the one
/// required part. Two relations are the SAME relation iff all three parts
/// match case-insensitively.
///
/// Built AT INGESTION ([`RelationIndex::from_manifest`]) — the leaf→NodeId
/// bridge. The raw `relation_name` is parsed once into this normalized form;
/// downstream joins compare these tuples, never the raw string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NormalizedRelation {
    /// The resolved database/catalog, ASCII-lowercased. `None` when the
    /// `relation_name` carried fewer than three segments.
    pub database: Option<String>,
    /// The resolved schema, ASCII-lowercased. `None` when the
    /// `relation_name` carried fewer than two segments.
    pub schema: Option<String>,
    /// The table identifier (the leaf), ASCII-lowercased. Always present —
    /// the one required part of a relation identity.
    pub identifier: String,
}

impl NormalizedRelation {
    /// Construct from already-split parts, ASCII-lowercasing each.
    #[must_use]
    pub fn new(database: Option<&str>, schema: Option<&str>, identifier: impl AsRef<str>) -> Self {
        Self {
            database: database.map(str::to_ascii_lowercase),
            schema: schema.map(str::to_ascii_lowercase),
            identifier: identifier.as_ref().to_ascii_lowercase(),
        }
    }
}

/// Parse dbt's fully-qualified `relation_name`
/// (`"database"."schema"."identifier"`) into a [`NormalizedRelation`].
///
/// dbt emits the relation as dot-joined, optionally-quoted segments. We
/// split on `.` at the top level, strip surrounding double-quotes from each
/// segment, and ASCII-lowercase. A trailing `identifier` is required; a
/// 1-/2-/3-segment name fills `identifier`, then `schema`, then `database`
/// from the right. More than three segments → the LAST three are taken
/// (database.schema.identifier), the rest dropped (defensive — dbt never
/// emits >3).
///
/// Returns `None` for an empty / unparseable name — that node simply does
/// not enter the [`RelationIndex`] (it can never be a join target, so the
/// stitch degrades Opaque rather than mis-attributing).
///
/// NOTE on quoting: a quoted identifier is technically case-sensitive in
/// some warehouses (the `ResolvedQuoting` exception, fusion-research §3.1).
/// cute-dbt does not carry `quoting` on the wire, so we lowercase
/// uniformly — the conservative direction: an over-folded key can only ever
/// FAIL to join (degrade Opaque), it can never mis-join two genuinely
/// distinct case-sensitive relations into a false attribution, because the
/// join is ALSO gated on the authoritative `depends_on` edge set. The
/// case-fold is a candidate filter, not the trust boundary.
#[must_use]
pub fn parse_relation_name(relation_name: &str) -> Option<NormalizedRelation> {
    let segments: Vec<String> = split_relation_segments(relation_name);
    let mut rev = segments.iter().rev();
    let identifier = rev.next()?.clone();
    if identifier.is_empty() {
        return None;
    }
    let schema = rev.next().cloned();
    let database = rev.next().cloned();
    Some(NormalizedRelation::new(
        database.as_deref(),
        schema.as_deref(),
        identifier,
    ))
}

/// Split a `relation_name` into its dot-separated segments, stripping
/// surrounding double-quotes and ASCII-lowercasing each. A dot INSIDE a
/// quoted segment is preserved (dbt rarely emits one, but a quoted
/// identifier may legally contain a dot).
fn split_relation_segments(relation_name: &str) -> Vec<String> {
    let mut segments: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in relation_name.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '.' if !in_quotes => {
                segments.push(std::mem::take(&mut current).to_ascii_lowercase());
            }
            _ => current.push(ch),
        }
    }
    segments.push(current.to_ascii_lowercase());
    segments
}

/// The normalized relation → `NodeId` index, built ONCE at ingestion — the
/// leaf→NodeId bridge for the cross-model stitch (refinement 3).
///
/// Keyed by [`NormalizedRelation`]. A key that more than one node normalizes
/// to is AMBIGUOUS — it is recorded in `ambiguous` and removed from
/// `by_relation`, so a leaf that hits it can never be attributed to either
/// node (degrade Opaque, never mis-join). Sources and seeds enter the index
/// too (a `ref()`/`source()` leaf may resolve to a seed or source).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationIndex {
    /// Unique normalized relation → its owning node id. A relation that
    /// exactly ONE node claims.
    by_relation: BTreeMap<NormalizedRelation, NodeId>,
    /// Normalized relations claimed by more than one node — recorded so a
    /// leaf hitting one degrades Opaque (never a coin-flip attribution).
    ambiguous: BTreeSet<NormalizedRelation>,
    /// Bare identifier (the leaf, lowercased) → the set of full normalized
    /// relations carrying that identifier. Lets a bare leaf ref (the engine
    /// only retains the last segment, `cte_engine.rs:994`) recover its full
    /// relation WHEN the identifier is globally unique.
    by_identifier: BTreeMap<String, BTreeSet<NormalizedRelation>>,
}

impl RelationIndex {
    /// Build the index from a manifest — every relational node (`relation_name`
    /// present) plus every source. AT INGESTION; the single normalization
    /// site.
    #[must_use]
    pub fn from_manifest(manifest: &Manifest) -> Self {
        let mut by_relation: BTreeMap<NormalizedRelation, NodeId> = BTreeMap::new();
        let mut ambiguous: BTreeSet<NormalizedRelation> = BTreeSet::new();
        let mut by_identifier: BTreeMap<String, BTreeSet<NormalizedRelation>> = BTreeMap::new();
        // Collect (relation_name, node_id) over nodes + sources, in id order
        // so the "first claimant" is deterministic before ambiguity removal.
        let mut claims: BTreeMap<NormalizedRelation, BTreeSet<NodeId>> = BTreeMap::new();
        for (id, node) in node_id_ordered(manifest.nodes()) {
            if let Some(rn) = node.relation_name()
                && let Some(rel) = parse_relation_name(rn)
            {
                claims.entry(rel).or_default().insert(id.clone());
            }
        }
        for (id, source) in node_id_ordered(manifest.sources()) {
            if let Some(rn) = source.relation_name()
                && let Some(rel) = parse_relation_name(rn)
            {
                claims.entry(rel).or_default().insert(id.clone());
            }
        }
        for (rel, owners) in claims {
            by_identifier
                .entry(rel.identifier.clone())
                .or_default()
                .insert(rel.clone());
            let mut owners_iter = owners.into_iter();
            match (owners_iter.next(), owners_iter.next()) {
                // Exactly one owner — a unique relation identity.
                (Some(owner), None) => {
                    by_relation.insert(rel, owner);
                }
                // Two or more owners — ambiguous; a leaf hitting it degrades
                // Opaque (never a coin-flip attribution).
                _ => {
                    ambiguous.insert(rel);
                }
            }
        }
        Self {
            by_relation,
            ambiguous,
            by_identifier,
        }
    }

    /// Resolve a FULLY-qualified normalized relation to its owning node id.
    /// `None` for an unknown or ambiguous relation.
    #[must_use]
    pub fn node_for(&self, rel: &NormalizedRelation) -> Option<&NodeId> {
        self.by_relation.get(rel)
    }

    /// Resolve a BARE leaf identifier (the engine's retained last segment) to
    /// its owning node id — ONLY when exactly one relation in the project
    /// carries that identifier (globally unique). A leaf carried by two
    /// relations (e.g. the same table name in two schemas) is ambiguous →
    /// `None` (degrade Opaque). This is the honest answer when the engine
    /// dropped the schema/database qualifier.
    #[must_use]
    pub fn node_for_bare_leaf(&self, identifier: &str) -> Option<&NodeId> {
        let leaf = identifier.to_ascii_lowercase();
        let relations = self.by_identifier.get(&leaf)?;
        let mut iter = relations.iter();
        match (iter.next(), iter.next()) {
            // Exactly one relation carries this leaf — globally unique.
            (Some(rel), None) => self.by_relation.get(rel),
            // Zero or two-plus — not uniquely resolvable (degrade Opaque).
            _ => None,
        }
    }

    /// `true` when the normalized relation is claimed by more than one node.
    #[must_use]
    pub fn is_ambiguous(&self, rel: &NormalizedRelation) -> bool {
        self.ambiguous.contains(rel)
    }
}

/// One model's terminal OUTPUT columns + the bare leaf relations its terminal
/// body reads — the intra-model facts the adapter extracts from the
/// already-parsed `CteGraph` (never a second parse) and hands to the
/// cross-model builder.
///
/// `output_columns` are the model's externally-visible columns (the columns a
/// DOWNSTREAM `select * from {{ ref(this) }}` would receive). `None` ⇒ the
/// terminal projection was non-enumerable (an Opaque `*` over an unknown
/// external, or an anonymous expression) — a downstream star over THIS model
/// then stays Opaque (the honest chain).
///
/// `leaf_refs` are the bare lowercased leaf identifiers the terminal body
/// reads directly (from `body_leaf_table_refs`) — the candidate `ref()`
/// boundaries to stitch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOutputs {
    /// The model's terminal output columns (lowercased, projection order),
    /// or `None` when the terminal projection is non-enumerable.
    pub output_columns: Option<Vec<String>>,
    /// The bare leaf identifiers the terminal body reads (lowercased).
    pub leaf_refs: Vec<String>,
}

impl ModelOutputs {
    /// Canonical constructor.
    #[must_use]
    pub fn new(output_columns: Option<Vec<String>>, leaf_refs: Vec<String>) -> Self {
        Self {
            output_columns,
            leaf_refs,
        }
    }
}

/// How a downstream `ref()` leaf stitched to an upstream node — the honest
/// 3-state outcome (never-a-false-claim).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StitchOutcome {
    /// The leaf uniquely normalize-joined to a real `depends_on` producer.
    Resolved {
        /// The upstream node the leaf resolves to.
        upstream: NodeId,
    },
    /// The leaf did not uniquely join to a single real producer (an unknown
    /// external relation, an ambiguous identifier, or no matching producer) —
    /// degraded honestly, NEVER attributed to a wrong upstream.
    Opaque,
}

/// One cross-model column edge: an upstream model's output column flows into
/// a downstream model (the `Cross`-scoped refinement of the model→model DAG
/// edge). The bidirectional edge set serves trace-to-source (forward walk)
/// and blast-radius (reverse index) from one structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossModelEdge {
    /// The upstream (producer) model.
    pub upstream: NodeId,
    /// The column on the upstream model.
    pub upstream_column: String,
    /// The downstream (consumer) model.
    pub downstream: NodeId,
    /// The column on the downstream model (after the upstream column flows
    /// in — for a `select *` stitch the name is carried through).
    pub downstream_column: String,
    /// `true` when this edge came through a `SELECT *` over the upstream
    /// (expanded from the upstream's derived projection map) rather than an
    /// explicit column reference.
    pub via_star: bool,
}

/// The project-wide cross-model column graph — the explorer's `FullProject`
/// compute envelope. Built ONCE over every model; the report path never
/// constructs it (scope-as-parameter).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectColumnGraph {
    /// Every stitched cross-model column edge, deterministically ordered.
    edges: Vec<CrossModelEdge>,
    /// Per-model terminal output columns (the project-wide output map) — the
    /// catalog-equivalent. `None` ⇒ non-enumerable model.
    outputs: BTreeMap<NodeId, Option<Vec<String>>>,
}

/// A column reached by the trace-to-source walk, with how the trace
/// terminated (the honesty surface).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceHop {
    /// The node the column lives on.
    pub node: NodeId,
    /// The column name on that node.
    pub column: String,
}

/// Why a trace-to-source walk stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceTermination {
    /// Reached a `source()` or `seed` leaf — the founder headline's happy
    /// path (the column's ultimate origin).
    Source,
    /// The trace thinned to an Opaque hop (a `*` over an unknown external, or
    /// a leaf that could not uniquely join) — honest dead-end, never a
    /// fabricated source.
    Opaque,
    /// The column reached a model with no further upstream column edge (a
    /// root model whose column derives in-SQL, not from a ref) — terminated
    /// at the model boundary.
    Root,
}

/// The result of tracing a column back toward its source field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceToSource {
    /// The hop chain from the queried column toward its origin, in walk
    /// order (the queried column first).
    pub hops: Vec<TraceHop>,
    /// How the walk terminated.
    pub termination: TraceTermination,
}

impl ProjectColumnGraph {
    /// Build the project-wide cross-model column graph — the EXPLORER arm.
    ///
    /// `model_outputs` is each model's intra-model facts (terminal output
    /// columns + bare leaf refs), extracted by the adapter from the
    /// already-parsed `CteGraph`. `lineage` is `DagFacts.lineage`
    /// ([`ModelLineage`], S0) — the ONE `depends_on` inversion; we read its
    /// `backward` (consumer → producers) to constrain every stitch to a REAL
    /// producer. `index` is the ingestion-built [`RelationIndex`].
    ///
    /// The stitch is the honesty seam: for each downstream model, each bare
    /// leaf its terminal body reads is matched against the model's ACTUAL
    /// `depends_on` producers (from `lineage.backward`), and resolved ONLY
    /// when the leaf's normalized identifier uniquely picks one producer.
    /// Non-unique / no-match → Opaque (no edge — never a wrong attribution).
    #[must_use]
    pub fn build(
        manifest: &Manifest,
        lineage: &ModelLineage,
        index: &RelationIndex,
        model_outputs: &BTreeMap<NodeId, ModelOutputs>,
    ) -> Self {
        let outputs: BTreeMap<NodeId, Option<Vec<String>>> = model_outputs
            .iter()
            .map(|(id, mo)| (id.clone(), mo.output_columns.clone()))
            .collect();

        let mut edges: Vec<CrossModelEdge> = Vec::new();
        for (downstream, mo) in model_outputs {
            // The downstream model's REAL producers (the authoritative edge
            // set) — a stitch may ONLY ever attribute to one of these.
            let producers: &[NodeId] = lineage
                .backward()
                .get(downstream)
                .map_or(&[], Vec::as_slice);
            if producers.is_empty() {
                continue;
            }
            // PHASE 1 — enumerable MODEL upstreams flow their output columns
            // into the downstream; non-enumerable source/seed producers are
            // collected for phase 2's name-carry.
            let (covered, source_upstreams) = stitch_enumerable(
                manifest, downstream, mo, producers, index, &outputs, &mut edges,
            );
            // PHASE 2 — source/seed name-carry for the uncovered residue.
            source_name_carry(downstream, mo, &covered, source_upstreams, &mut edges);
        }
        // Determinism: the build order is model-id-ordered (model_outputs is a
        // BTreeMap) then leaf order then column order — already stable. Sort +
        // dedup defensively so duplicate ref()s (a model joining the same
        // upstream twice) collapse.
        edges.sort_by(cross_edge_sort_key);
        edges.dedup();
        Self { edges, outputs }
    }

    /// Every cross-model column edge, in deterministic order.
    #[must_use]
    pub fn edges(&self) -> &[CrossModelEdge] {
        &self.edges
    }

    /// The project-wide output-column map — node id → terminal output columns
    /// (`None` = non-enumerable). The catalog-equivalent.
    #[must_use]
    pub fn outputs(&self) -> &BTreeMap<NodeId, Option<Vec<String>>> {
        &self.outputs
    }

    /// Affordance C — trace a `(model, column)` toward its source field.
    ///
    /// Forward recursion across `ref()` boundaries: from the queried column,
    /// follow the cross-model edge whose `downstream_column` matches back to
    /// the `(upstream, upstream_column)`, and recurse. Terminates at a
    /// `source()`/`seed` leaf ([`TraceTermination::Source`]), a model with no
    /// further upstream edge ([`TraceTermination::Root`]), or an Opaque thin
    /// ([`TraceTermination::Opaque`]). A visited-set guards against cycles
    /// (a manifest should be acyclic, but never spin).
    #[must_use]
    pub fn trace_to_source(
        &self,
        manifest: &Manifest,
        model: &NodeId,
        column: &str,
    ) -> TraceToSource {
        let column = column.to_ascii_lowercase();
        let mut hops: Vec<TraceHop> = vec![TraceHop {
            node: model.clone(),
            column: column.clone(),
        }];
        let mut visited: BTreeSet<(NodeId, String)> = BTreeSet::new();
        visited.insert((model.clone(), column.clone()));
        let mut current = (model.clone(), column);
        loop {
            // A source or seed node is a terminal leaf — the column's origin.
            if is_source_or_seed(manifest, &current.0) {
                return TraceToSource {
                    hops,
                    termination: TraceTermination::Source,
                };
            }
            // Find the single upstream edge feeding this column.
            let next = self
                .edges
                .iter()
                .find(|e| e.downstream == current.0 && e.downstream_column == current.1);
            let Some(edge) = next else {
                // No further cross-model edge. Distinguish "this column maps
                // to a real producer but the producer is non-enumerable
                // (Opaque)" from "this column genuinely originates here
                // (Root)". If the current node has a producer whose stitch is
                // Opaque AND no resolved edge fed this column, the trace
                // honestly thinned.
                let termination = if self.column_thins_opaque(manifest, &current.0) {
                    TraceTermination::Opaque
                } else {
                    TraceTermination::Root
                };
                return TraceToSource { hops, termination };
            };
            let upstream = (edge.upstream.clone(), edge.upstream_column.clone());
            if !visited.insert(upstream.clone()) {
                // Cycle guard — stop, honest dead-end.
                return TraceToSource {
                    hops,
                    termination: TraceTermination::Root,
                };
            }
            hops.push(TraceHop {
                node: upstream.0.clone(),
                column: upstream.1.clone(),
            });
            current = upstream;
        }
    }

    /// `true` when `node`'s lineage thins to Opaque — it has at least one
    /// producer but no enumerable output edge could be formed (a `*` over a
    /// non-enumerable / unknown upstream). Used only to label a trace
    /// dead-end honestly (Opaque vs Root).
    fn column_thins_opaque(&self, manifest: &Manifest, node: &NodeId) -> bool {
        // The node consumes something (has an inbound model edge in the DAG)
        // but no resolved cross-model column edge lands on it ⇒ the boundary
        // was Opaque. A genuine root (no producers) is Root, not Opaque.
        let has_producer = manifest
            .node(node)
            .is_some_and(|n| !n.depends_on().nodes().is_empty());
        let has_inbound_col_edge = self.edges.iter().any(|e| &e.downstream == node);
        has_producer && !has_inbound_col_edge
    }

    /// Affordance B — column-grain downstream blast-radius.
    ///
    /// Reverse index over the cross-model edge set (the `child_map`
    /// direction): every `(downstream, downstream_column)` reachable from the
    /// queried `(model, column)` by following edges FORWARD (upstream →
    /// downstream). BFS with a visited-set — the fusion-research §6 contract.
    /// Reads only the cross-model edges built from `DagFacts.lineage`; never
    /// re-inverts `depends_on`.
    #[must_use]
    pub fn blast_radius(&self, model: &NodeId, column: &str) -> Vec<TraceHop> {
        let column = column.to_ascii_lowercase();
        let mut visited: BTreeSet<(NodeId, String)> = BTreeSet::new();
        let start = (model.clone(), column);
        visited.insert(start.clone());
        let mut queue: VecDeque<(NodeId, String)> = VecDeque::new();
        queue.push_back(start);
        let mut reached: Vec<TraceHop> = Vec::new();
        while let Some((node, col)) = queue.pop_front() {
            for edge in &self.edges {
                if edge.upstream == node && edge.upstream_column == col {
                    let next = (edge.downstream.clone(), edge.downstream_column.clone());
                    if visited.insert(next.clone()) {
                        reached.push(TraceHop {
                            node: next.0.clone(),
                            column: next.1.clone(),
                        });
                        queue.push_back(next);
                    }
                }
            }
        }
        reached.sort_by(|a, b| (a.node.as_str(), &a.column).cmp(&(b.node.as_str(), &b.column)));
        reached
    }
}

/// PHASE 1 of the per-model stitch — flow each ENUMERABLE model upstream's
/// output columns into the downstream over its `ref()` boundary, and collect
/// the non-enumerable `source()`/`seed` producers for phase 2's name-carry.
///
/// Returns `(covered, source_upstreams)`: the downstream columns already
/// explained by an enumerable model edge (so phase 2 only fills the residue),
/// and the de-duplicated source/seed producers the model reads.
#[allow(clippy::too_many_arguments)]
fn stitch_enumerable(
    manifest: &Manifest,
    downstream: &NodeId,
    mo: &ModelOutputs,
    producers: &[NodeId],
    index: &RelationIndex,
    outputs: &BTreeMap<NodeId, Option<Vec<String>>>,
    edges: &mut Vec<CrossModelEdge>,
) -> (BTreeSet<String>, BTreeSet<NodeId>) {
    let mut covered: BTreeSet<String> = BTreeSet::new();
    let mut source_upstreams: BTreeSet<NodeId> = BTreeSet::new();
    for leaf in &mo.leaf_refs {
        let StitchOutcome::Resolved { upstream } = stitch_leaf(leaf, producers, index) else {
            continue; // Opaque — no edge, honest gap.
        };
        match outputs.get(&upstream).cloned().flatten() {
            // The upstream's output columns ARE the columns flowing into the
            // downstream over this `ref()` boundary (the catalog-equivalent).
            Some(upstream_cols) => {
                for column in upstream_cols {
                    covered.insert(column.clone());
                    edges.push(CrossModelEdge {
                        upstream: upstream.clone(),
                        upstream_column: column.clone(),
                        downstream: downstream.clone(),
                        downstream_column: column,
                        via_star: true,
                    });
                }
            }
            // A non-enumerable `source()`/`seed` is the column's ORIGIN (the
            // founder-headline terminus) — defer to phase 2. A non-enumerable
            // MODEL (a `*` over an unknown external) stays Opaque (no edge).
            None if is_source_or_seed(manifest, &upstream) => {
                source_upstreams.insert(upstream);
            }
            None => {}
        }
    }
    (covered, source_upstreams)
}

/// PHASE 2 of the per-model stitch — the source/seed name-carry for the
/// UNCOVERED residue. We cannot enumerate a source's columns (no SQL, no
/// catalog), but a downstream output column NOT explained by any enumerable
/// model producer, when the model reads a single source/seed leaf, honestly
/// originates at that source under the SAME name (a star / direct ref carries
/// the name through — never a fabricated DIFFERENT name). With TWO source
/// producers we cannot tell which owns the column, so we attribute to NONE
/// (degrade — never a coin-flip).
fn source_name_carry(
    downstream: &NodeId,
    mo: &ModelOutputs,
    covered: &BTreeSet<String>,
    source_upstreams: BTreeSet<NodeId>,
    edges: &mut Vec<CrossModelEdge>,
) {
    let mut source_iter = source_upstreams.into_iter();
    let (Some(down_cols), (Some(upstream), None)) =
        (&mo.output_columns, (source_iter.next(), source_iter.next()))
    else {
        return;
    };
    for column in down_cols {
        if covered.contains(column) {
            continue;
        }
        edges.push(CrossModelEdge {
            upstream: upstream.clone(),
            upstream_column: column.clone(),
            downstream: downstream.clone(),
            downstream_column: column.clone(),
            via_star: true,
        });
    }
}

/// Stitch a single bare leaf identifier to a real `depends_on` producer.
///
/// THE never-mis-join seam: the leaf is resolved ONLY when (a) the
/// [`RelationIndex`] maps its identifier to a node AND (b) that node is among
/// the downstream model's ACTUAL producers. If the index returns an ambiguous
/// / unknown identifier, OR the resolved node is not a real producer, the
/// stitch is [`StitchOutcome::Opaque`] — never attributed to a wrong
/// upstream. As a belt-and-suspenders fallback, when the index can't resolve
/// the bare leaf but EXACTLY ONE producer's normalized identifier matches the
/// leaf, that producer is taken (the engine dropped the qualifier but the
/// producer set disambiguates).
fn stitch_leaf(leaf: &str, producers: &[NodeId], index: &RelationIndex) -> StitchOutcome {
    let leaf_lc = leaf.to_ascii_lowercase();
    // Primary: the index uniquely resolves the bare leaf, AND it is a real
    // producer.
    if let Some(node) = index.node_for_bare_leaf(&leaf_lc)
        && producers.contains(node)
    {
        return StitchOutcome::Resolved {
            upstream: node.clone(),
        };
    }
    // Fallback: among the REAL producers, exactly one carries this leaf as the
    // identifier of its normalized relation. (The producer set is the
    // authoritative edge — this can never reach outside it.)
    let mut matches = producers.iter().filter(|p| {
        index
            .by_relation
            .iter()
            .any(|(rel, owner)| owner == *p && rel.identifier == leaf_lc)
    });
    match (matches.next(), matches.next()) {
        (Some(only), None) => StitchOutcome::Resolved {
            upstream: only.clone(),
        },
        _ => StitchOutcome::Opaque,
    }
}

/// `true` when the node is a `source` or `seed` — a trace-to-source terminal
/// leaf. Sources live in the sources map; seeds are nodes with
/// `resource_type == "seed"`.
fn is_source_or_seed(manifest: &Manifest, id: &NodeId) -> bool {
    if manifest.sources().contains_key(id) {
        return true;
    }
    manifest
        .node(id)
        .is_some_and(|n| n.resource_type() == "seed")
}

/// Deterministic sort key for a cross-model edge.
fn cross_edge_sort_key(a: &CrossModelEdge, b: &CrossModelEdge) -> std::cmp::Ordering {
    (
        a.upstream.as_str(),
        a.upstream_column.as_str(),
        a.downstream.as_str(),
        a.downstream_column.as_str(),
        a.via_star,
    )
        .cmp(&(
            b.upstream.as_str(),
            b.upstream_column.as_str(),
            b.downstream.as_str(),
            b.downstream_column.as_str(),
            b.via_star,
        ))
}

/// Iterate a node-id-keyed `HashMap` in deterministic full-id order.
fn node_id_ordered<V>(map: &std::collections::HashMap<NodeId, V>) -> Vec<(&NodeId, &V)> {
    let mut entries: Vec<(&NodeId, &V)> = map.iter().collect();
    entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{
        Checksum, DependsOn, ManifestMetadata, Node, NodeConfig, SourceNode,
    };
    use std::collections::HashMap;

    fn nid(s: &str) -> NodeId {
        NodeId::new(s)
    }

    /// A model node with a `relation_name` + `depends_on` producers.
    fn model(id: &str, relation_name: Option<&str>, producers: &[&str]) -> Node {
        Node::new(
            nid(id),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            None,
            DependsOn::new(Vec::new(), producers.iter().map(|p| nid(p)).collect()),
            None,
            NodeConfig::default(),
            relation_name.map(str::to_owned),
            std::collections::BTreeMap::new(),
        )
    }

    fn seed(id: &str, relation_name: Option<&str>) -> Node {
        Node::new(
            nid(id),
            "seed",
            Checksum::new("sha256", "ck"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            relation_name.map(str::to_owned),
            std::collections::BTreeMap::new(),
        )
    }

    fn source(id: &str, schema: &str, database: Option<&str>, relation_name: &str) -> SourceNode {
        SourceNode::new(
            nid(id),
            "raw",
            id.rsplit('.').next().unwrap_or(id),
            None,
            schema,
            database.map(str::to_owned),
            Some(relation_name.to_owned()),
        )
    }

    fn manifest_of(nodes: Vec<Node>, sources: Vec<SourceNode>) -> Manifest {
        let mut node_map: HashMap<NodeId, Node> = HashMap::new();
        for n in nodes {
            node_map.insert(n.id().clone(), n);
        }
        let mut source_map: HashMap<NodeId, SourceNode> = HashMap::new();
        for s in sources {
            source_map.insert(s.id().clone(), s);
        }
        Manifest::new(
            ManifestMetadata::new("v12"),
            node_map,
            HashMap::new(),
            HashMap::new(),
        )
        .with_sources(source_map)
    }

    // ---- TDD 1: normalized join key -----------------------------------

    #[test]
    fn parses_three_segment_quoted_relation_name() {
        let rel = parse_relation_name("\"memory\".\"main_staging\".\"stg_orgs\"").expect("parses");
        assert_eq!(
            rel,
            NormalizedRelation::new(Some("memory"), Some("main_staging"), "stg_orgs")
        );
    }

    #[test]
    fn relation_name_parse_is_case_folded() {
        let a = parse_relation_name("\"DB\".\"Schema\".\"Tbl\"").expect("a");
        let b = parse_relation_name("\"db\".\"schema\".\"tbl\"").expect("b");
        assert_eq!(a, b, "case-insensitive equality is the join-key contract");
    }

    #[test]
    fn two_and_one_segment_relation_names() {
        assert_eq!(
            parse_relation_name("schema.tbl").expect("two"),
            NormalizedRelation::new(None, Some("schema"), "tbl")
        );
        assert_eq!(
            parse_relation_name("tbl").expect("one"),
            NormalizedRelation::new(None, None, "tbl")
        );
    }

    #[test]
    fn join_key_built_at_ingestion_resolves_correct_upstream() {
        // Two models with the SAME leaf identifier in different schemas — a
        // raw relation_name string-match would be a hazard; the normalized
        // tuple + producer-set gating attributes correctly.
        let manifest = manifest_of(
            vec![
                model(
                    "model.p.stg_orders",
                    Some("\"db\".\"staging\".\"orders\""),
                    &["seed.p.raw_orders"],
                ),
                seed("seed.p.raw_orders", Some("\"db\".\"raw\".\"orders\"")),
            ],
            vec![],
        );
        let index = RelationIndex::from_manifest(&manifest);
        // The bare leaf "orders" is carried by TWO relations (staging.orders +
        // raw.orders) → NOT globally unique → bare-leaf resolve is None.
        assert_eq!(index.node_for_bare_leaf("orders"), None);
        // But the FULLY-qualified normalized tuples resolve distinctly.
        assert_eq!(
            index.node_for(&NormalizedRelation::new(Some("db"), Some("raw"), "orders")),
            Some(&nid("seed.p.raw_orders"))
        );
        assert_eq!(
            index.node_for(&NormalizedRelation::new(
                Some("db"),
                Some("staging"),
                "orders"
            )),
            Some(&nid("model.p.stg_orders"))
        );
    }

    #[test]
    fn never_mis_joins_across_aliasing_case_fold_identifier_config() {
        // The downstream reads bare leaf "orders". The index has two "orders"
        // (ambiguous bare). But the downstream's REAL producer set is exactly
        // {seed.p.raw_orders}, so the producer-gated fallback resolves to it —
        // NEVER the other "orders".
        let manifest = manifest_of(
            vec![
                model(
                    "model.p.other_orders",
                    Some("\"db\".\"staging\".\"orders\""),
                    &[],
                ),
                seed("seed.p.raw_orders", Some("\"DB\".\"RAW\".\"Orders\"")),
                model(
                    "model.p.stg_orders",
                    Some("\"db\".\"staging\".\"stg_orders\""),
                    &["seed.p.raw_orders"],
                ),
            ],
            vec![],
        );
        let index = RelationIndex::from_manifest(&manifest);
        let producers = [nid("seed.p.raw_orders")];
        let outcome = stitch_leaf("orders", &producers, &index);
        assert_eq!(
            outcome,
            StitchOutcome::Resolved {
                upstream: nid("seed.p.raw_orders")
            },
            "a bare leaf resolves ONLY among the real producers, case-folded"
        );
        // And it never reaches the non-producer model with the same leaf.
        let outcome_no_producer = stitch_leaf("orders", &[], &index);
        assert_eq!(
            outcome_no_producer,
            StitchOutcome::Opaque,
            "no producers ⇒ Opaque, never a wrong attribution"
        );
    }

    // ---- TDD 2: cross-model stitch ------------------------------------

    fn three_model_chain() -> (
        Manifest,
        ModelLineage,
        RelationIndex,
        BTreeMap<NodeId, ModelOutputs>,
    ) {
        // seed.raw_orders → stg_orders → dim_orders, each select *.
        let manifest = manifest_of(
            vec![
                seed("seed.p.raw_orders", Some("\"db\".\"raw\".\"raw_orders\"")),
                model(
                    "model.p.stg_orders",
                    Some("\"db\".\"staging\".\"stg_orders\""),
                    &["seed.p.raw_orders"],
                ),
                model(
                    "model.p.dim_orders",
                    Some("\"db\".\"marts\".\"dim_orders\""),
                    &["model.p.stg_orders"],
                ),
            ],
            vec![],
        );
        let lineage = ModelLineage::from_manifest(&manifest);
        let index = RelationIndex::from_manifest(&manifest);
        let mut outputs: BTreeMap<NodeId, ModelOutputs> = BTreeMap::new();
        outputs.insert(
            nid("seed.p.raw_orders"),
            ModelOutputs::new(Some(vec!["order_id".into(), "amount".into()]), vec![]),
        );
        outputs.insert(
            nid("model.p.stg_orders"),
            ModelOutputs::new(
                Some(vec!["order_id".into(), "amount".into()]),
                vec!["raw_orders".into()],
            ),
        );
        outputs.insert(
            nid("model.p.dim_orders"),
            ModelOutputs::new(
                Some(vec!["order_id".into(), "amount".into()]),
                vec!["stg_orders".into()],
            ),
        );
        (manifest, lineage, index, outputs)
    }

    #[test]
    fn cross_model_stitch_flows_upstream_output_to_downstream_leaf() {
        let (manifest, lineage, index, outputs) = three_model_chain();
        let graph = ProjectColumnGraph::build(&manifest, &lineage, &index, &outputs);
        // dim_orders ← stg_orders for both columns.
        let dim_edges: Vec<_> = graph
            .edges()
            .iter()
            .filter(|e| e.downstream == nid("model.p.dim_orders"))
            .collect();
        assert_eq!(dim_edges.len(), 2, "two columns flow stg→dim");
        assert!(
            dim_edges
                .iter()
                .all(|e| e.upstream == nid("model.p.stg_orders"))
        );
        assert!(
            dim_edges
                .iter()
                .any(|e| e.upstream_column == "order_id" && e.downstream_column == "order_id")
        );
    }

    // ---- TDD 3: B blast-radius ----------------------------------------

    #[test]
    fn blast_radius_reaches_all_downstream_columns() {
        let (manifest, lineage, index, outputs) = three_model_chain();
        let graph = ProjectColumnGraph::build(&manifest, &lineage, &index, &outputs);
        // order_id on the seed flows to stg AND dim.
        let reached = graph.blast_radius(&nid("seed.p.raw_orders"), "order_id");
        let nodes: BTreeSet<&str> = reached.iter().map(|h| h.node.as_str()).collect();
        assert!(nodes.contains("model.p.stg_orders"));
        assert!(nodes.contains("model.p.dim_orders"));
        assert!(reached.iter().all(|h| h.column == "order_id"));
    }

    // ---- TDD 4: C trace-to-source -------------------------------------

    #[test]
    fn trace_to_source_walks_to_the_seed_leaf() {
        let (manifest, lineage, index, outputs) = three_model_chain();
        let graph = ProjectColumnGraph::build(&manifest, &lineage, &index, &outputs);
        let trace = graph.trace_to_source(&manifest, &nid("model.p.dim_orders"), "order_id");
        assert_eq!(trace.termination, TraceTermination::Source);
        let nodes: Vec<&str> = trace.hops.iter().map(|h| h.node.as_str()).collect();
        assert_eq!(
            nodes,
            vec![
                "model.p.dim_orders",
                "model.p.stg_orders",
                "seed.p.raw_orders"
            ],
            "the trace walks dim → stg → the seed source leaf"
        );
    }

    // ---- TDD 5: star discipline ---------------------------------------

    #[test]
    fn star_over_known_upstream_resolves() {
        // dim_orders does `select *` over stg_orders (known, enumerable) →
        // every stg column flows in (via_star), Resolved.
        let (manifest, lineage, index, outputs) = three_model_chain();
        let graph = ProjectColumnGraph::build(&manifest, &lineage, &index, &outputs);
        let dim_edges: Vec<_> = graph
            .edges()
            .iter()
            .filter(|e| e.downstream == nid("model.p.dim_orders"))
            .collect();
        assert!(!dim_edges.is_empty(), "star over known upstream resolves");
        assert!(dim_edges.iter().all(|e| e.via_star));
    }

    #[test]
    fn star_over_unknown_external_stays_opaque() {
        // stg_orders' upstream "raw_orders" is non-enumerable (None outputs)
        // → no cross-model edges land on stg for those columns.
        let manifest = manifest_of(
            vec![
                seed("seed.p.raw_orders", Some("\"db\".\"raw\".\"raw_orders\"")),
                model(
                    "model.p.stg_orders",
                    Some("\"db\".\"staging\".\"stg_orders\""),
                    &["seed.p.raw_orders"],
                ),
            ],
            vec![],
        );
        let lineage = ModelLineage::from_manifest(&manifest);
        let index = RelationIndex::from_manifest(&manifest);
        let mut outputs: BTreeMap<NodeId, ModelOutputs> = BTreeMap::new();
        // The seed is NON-ENUMERABLE (a `*` over an unknown external upstream).
        outputs.insert(nid("seed.p.raw_orders"), ModelOutputs::new(None, vec![]));
        outputs.insert(
            nid("model.p.stg_orders"),
            ModelOutputs::new(None, vec!["raw_orders".into()]),
        );
        let graph = ProjectColumnGraph::build(&manifest, &lineage, &index, &outputs);
        assert!(
            graph
                .edges()
                .iter()
                .all(|e| e.downstream != nid("model.p.stg_orders")),
            "a star over a non-enumerable upstream yields NO fabricated edges"
        );
        // The trace honestly thins to Opaque, never a fabricated source.
        let trace = graph.trace_to_source(&manifest, &nid("model.p.stg_orders"), "anything");
        assert_eq!(trace.termination, TraceTermination::Opaque);
    }

    #[test]
    fn star_chain_compounds_opaque() {
        // raw(None) → stg(None, `*` over raw) → dim(`*` over stg): the Opaque
        // compounds — NO enumerable cross-model edges anywhere downstream.
        let manifest = manifest_of(
            vec![
                seed("seed.p.raw", Some("\"db\".\"raw\".\"raw\"")),
                model("model.p.stg", Some("\"db\".\"s\".\"stg\""), &["seed.p.raw"]),
                model(
                    "model.p.dim",
                    Some("\"db\".\"m\".\"dim\""),
                    &["model.p.stg"],
                ),
            ],
            vec![],
        );
        let lineage = ModelLineage::from_manifest(&manifest);
        let index = RelationIndex::from_manifest(&manifest);
        let mut outputs: BTreeMap<NodeId, ModelOutputs> = BTreeMap::new();
        outputs.insert(nid("seed.p.raw"), ModelOutputs::new(None, vec![]));
        outputs.insert(
            nid("model.p.stg"),
            ModelOutputs::new(None, vec!["raw".into()]),
        );
        outputs.insert(
            nid("model.p.dim"),
            ModelOutputs::new(None, vec!["stg".into()]),
        );
        let graph = ProjectColumnGraph::build(&manifest, &lineage, &index, &outputs);
        assert!(
            graph.edges().is_empty(),
            "a chain of `*` over non-enumerable upstreams fabricates nothing"
        );
    }

    // ---- TDD 7: uses DagFacts.lineage, not a fresh inversion ----------

    #[test]
    fn stitch_is_constrained_to_real_depends_on_producers() {
        // dim_orders declares NO dependency on the model whose leaf it reads —
        // the stitch must NOT cross to a non-producer even if the identifier
        // matches. (This is the lineage-as-authority guard.)
        let manifest = manifest_of(
            vec![
                model(
                    "model.p.stg_orders",
                    Some("\"db\".\"staging\".\"stg_orders\""),
                    &[],
                ),
                // dim reads "stg_orders" by leaf but declares NO producer.
                model("model.p.dim_orders", Some("\"db\".\"m\".\"dim\""), &[]),
            ],
            vec![],
        );
        let lineage = ModelLineage::from_manifest(&manifest);
        let index = RelationIndex::from_manifest(&manifest);
        let mut outputs: BTreeMap<NodeId, ModelOutputs> = BTreeMap::new();
        outputs.insert(
            nid("model.p.stg_orders"),
            ModelOutputs::new(Some(vec!["order_id".into()]), vec![]),
        );
        outputs.insert(
            nid("model.p.dim_orders"),
            ModelOutputs::new(Some(vec!["order_id".into()]), vec!["stg_orders".into()]),
        );
        let graph = ProjectColumnGraph::build(&manifest, &lineage, &index, &outputs);
        assert!(
            graph.edges().is_empty(),
            "no depends_on edge ⇒ no cross-model attribution (lineage is the authority)"
        );
    }

    #[test]
    fn graph_serde_round_trips() {
        let (manifest, lineage, index, outputs) = three_model_chain();
        let graph = ProjectColumnGraph::build(&manifest, &lineage, &index, &outputs);
        let json = serde_json::to_string(&graph).expect("serialize");
        let back: ProjectColumnGraph = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(graph, back);
    }

    #[test]
    fn build_is_deterministic() {
        let (manifest, lineage, index, outputs) = three_model_chain();
        let a = ProjectColumnGraph::build(&manifest, &lineage, &index, &outputs);
        let b = ProjectColumnGraph::build(&manifest, &lineage, &index, &outputs);
        assert_eq!(a, b);
    }

    /// A real `source()` node (in the sources map, not a seed) terminates the
    /// trace-to-source as `Source` via the name-carry edge — the founder
    /// headline against a genuine source leaf.
    #[test]
    fn trace_terminates_at_a_real_source_node() {
        let manifest = manifest_of(
            vec![model(
                "model.p.stg",
                Some("\"db\".\"staging\".\"stg\""),
                &["source.p.raw.orders"],
            )],
            vec![source(
                "source.p.raw.orders",
                "raw",
                Some("db"),
                "\"db\".\"raw\".\"orders\"",
            )],
        );
        let lineage = ModelLineage::from_manifest(&manifest);
        let index = RelationIndex::from_manifest(&manifest);
        let mut outputs: BTreeMap<NodeId, ModelOutputs> = BTreeMap::new();
        // stg reads the source by its leaf `orders` and exposes order_id.
        outputs.insert(
            nid("model.p.stg"),
            ModelOutputs::new(Some(vec!["order_id".into()]), vec!["orders".into()]),
        );
        let graph = ProjectColumnGraph::build(&manifest, &lineage, &index, &outputs);
        let trace = graph.trace_to_source(&manifest, &nid("model.p.stg"), "order_id");
        assert_eq!(
            trace.termination,
            TraceTermination::Source,
            "the trace reaches the real source() leaf"
        );
        let last = trace.hops.last().expect("at least one hop");
        assert_eq!(last.node, nid("source.p.raw.orders"));
    }
}
