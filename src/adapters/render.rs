//! askama 0.16 renderer that produces the v0.1 `report.html` from the
//! cute-dbt domain.
//!
//! Composition layer between [`Manifest`] / [`InScopeSet`] /
//! [`ModelInScopeSet`] / [`CteGraph`] and the askama template at
//! `templates/report.html`. The template owns DOM + class + JS structure;
//! this module owns:
//!
//! - **Per-model payload assembly.** Walks `models_in_scope`, resolves
//!   each model node, builds a [`ModelPayload`] carrying its CTE graph,
//!   per-CTE compiled SQL, and the list of in-scope unit tests targeting
//!   it. Models with zero in-scope unit tests render the "0 unit tests
//!   wired" empty state.
//! - **CTE graph parsing.** Invokes [`parse_cte_graph`] once per in-scope
//!   model. The Stage-2 preflight already proves `compiled_code` is
//!   `Some` for every in-scope model; a parse failure here is treated as
//!   an empty graph (the renderer surfaces "no DAG available" — the
//!   report stays valid, the model card is just sparse).
//! - **Node-role classification.** Walks the [`CteGraph`]; the terminal
//!   node (named [`TERMINAL_NODE_NAME`]) is `final`; a CTE whose body is
//!   a plain `SELECT … FROM <single relation>` with zero incoming edges
//!   is `import`; everything else is `transform`.
//! - **Import-CTE binding.** Parses `ref('NAME')` out of each unit
//!   test's `given[].input`, then locates the matching leaf-CTE node
//!   in two passes (case-insensitive). Pass-1: name match — an
//!   import-CTE whose own name equals `NAME` (the unwrapper
//!   convention; strict role gate so a transform CTE cannot
//!   spuriously bind). Pass-2: body match — any non-terminal leaf
//!   CTE (zero incoming edges) whose engine-extracted
//!   `body_leaf_table_refs` contain `NAME`. Pass-2 catches both the
//!   dbt-idiomatic `with source as (select * from
//!   "db"."schema"."MODEL")` shape and the messy multi-ref case
//!   (cute-dbt#34) where one CTE body references multiple `ref()`
//!   targets via `UNION ALL`, `JOIN`, or derived subqueries —
//!   classified `Transform` in the DAG but still a valid binding
//!   surface for every leaf ref. The template stacks every matching
//!   given vertically; unmatched givens against an import-CTE
//!   surface "no fixture provided — dbt treats unspecified inputs
//!   as empty". `source()` references are NOT yet bound — tracked
//!   as cute-dbt#57 for v0.2.
//!
//! ## Security
//!
//! The JSON payload is emitted into a `<script type="application/json">`
//! block via askama's `| json | safe` pipeline. askama's `json` filter
//! escapes `<`, `>`, `&`, and `'` (per the askama 0.16 book), so a
//! manifest-derived `</script>` substring cannot break out of the JSON
//! block. The Mermaid runtime renders DAGs from this same payload under
//! `securityLevel: 'strict'`; click-to-expand is wired by external jQuery
//! handlers binding to the rendered SVG `<g>` elements — never Mermaid's
//! `click` directive (which `'strict'` disables). See
//! [`ARCHITECTURE.md` §5](../../../ARCHITECTURE.md) for the zero-egress
//! gate this preserves.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::Path;

use askama::Template;
use serde::Serialize;
use serde_json::Value;

use crate::adapters::asset_embed::{
    APPEARANCE_JS, CYTO_DAG_JS, CYTOSCAPE_JS, DATATABLES_CSS, DATATABLES_JS, FAVICON_DATA_URI,
    INTERACTION_JS, JQUERY_JS, MERMAID_JS, REPORT_CSS, SAKURA_CSS, THEME_JS,
};
use crate::adapters::cte_engine::{TERMINAL_NODE_NAME, parse_cte_graph};
use crate::domain::{
    BANNER_EMPTY_SCOPE, BlockDiff, ChangeAxes, CheckId, CheckPolicy, ColumnContext, ColumnEdge,
    CommentsView, ConfigAttribution, CteGraph, DEFAULT_SEED_ROW_CAP, DiffLine, DiffLineKind,
    EdgeType, Finding, FixtureTable, FixtureTableDiff, GovernanceFacts, HeuristicId,
    HookChangeFacts, HookManifestPresence, InScopeSet, Instrument, MacroIdentity, Manifest,
    ModelInScopeSet, ModelState, ModelYamlOutcome, Node, NodeId, NormalizedDiffIndex, PrDagGraph,
    PrRef, ProjectChange, ProjectChangeCategory, ProjectChangePanel, ProjectDefinition,
    ProjectFacts, ProjectFallbackReason, SeedCard, SourceMap, SourceMapEntry, SourceNode,
    SourcePos, SourceSpan, SpanRole, TestMetadata, Tier, UnitTest, UnitTestDataDiff, UnitTestGiven,
    UnitTestOverrides, UnitTestYamlBlock, VarAttribution, VarChangeFacts, VarReference,
    VarScanFootprint, ZoneKind, apply_check_policy, column_contexts, macro_blast_radius,
    model_findings, reconstruct_macro_sql_diff, resolve_target_model, resolve_tested_model,
    table_from_manifest_rows,
};

/// Snake-case wire key for an [`EdgeType`] — the exact JSON-serde string
/// the JS `JOIN_COLORS` map keys are matched against.
///
/// Exhaustive match — adding a new [`EdgeType`] variant fails to compile
/// here, which keeps the render-side palette in lockstep with the
/// classifier vocabulary. The `edge-vocab-completeness` CI guard greps
/// this match (and the JS palette in `templates/report.html`) so the
/// invariant is structurally enforced both at compile time and at CI
/// time.
#[must_use]
pub fn edge_type_wire_key(edge_type: EdgeType) -> &'static str {
    match edge_type {
        EdgeType::From => "from",
        EdgeType::Inner => "inner",
        EdgeType::Left => "left",
        EdgeType::Right => "right",
        EdgeType::Full => "full",
        EdgeType::Cross => "cross",
        EdgeType::UnionAll => "union_all",
        EdgeType::UnionDistinct => "union_distinct",
    }
}

/// Role of a node in the rendered CTE DAG.
///
/// Classification happens at the render layer (not in `domain`) because
/// it depends on graph topology (incoming edges) and on the terminal
/// node name, both of which are properties of the parsed graph rather
/// than the dbt manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    /// The terminal `SELECT` — keyed by [`TERMINAL_NODE_NAME`] in the
    /// graph, rendered as the model's bare name in the payload.
    Final,
    /// A leaf CTE whose body is a plain `SELECT … FROM <single relation>`
    /// with no incoming edges — the moral equivalent of a dbt source.
    Import,
    /// Anything else — joins, transformations, intermediate CTEs.
    Transform,
}

/// Parse the bare name out of a unit-test `given[].input` string of the
/// form `ref('NAME')` / `ref("NAME")` (case-insensitive `ref`; either
/// quote style — dbt accepts both in authored YAML and both engines
/// ship the authored string **verbatim** on the manifest wire,
/// cute-dbt#245).
///
/// The keyword check is case-insensitive across any byte casing
/// (`ref` / `REF` / `Ref` / `rEf` / …) and tolerates whitespace between
/// the keyword and the opening parenthesis (`ref ('x')`, `REF\t('y')`,
/// etc. — Jinja's `{{ ref(...) }}` macro accepts this).
///
/// Returns `None` when the input does not match the `ref('…')` shape,
/// when the inner name is empty, or when the parentheses / quotes are
/// unbalanced or mismatched (open/close must be the same character —
/// see `strip_matching_quotes`). The caller (`bind_import_to_given`)
/// treats `None` as "no import-CTE match" and surfaces the design's
/// empty-state copy.
#[must_use]
pub fn parse_ref_name(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    let prefix = trimmed.get(..3)?;
    if !prefix.eq_ignore_ascii_case("ref") {
        return None;
    }
    let after_ref = trimmed[3..].trim_start();
    let inside = after_ref.strip_prefix('(')?.strip_suffix(')')?;
    let name = strip_matching_quotes(inside.trim())?;
    if name.is_empty() { None } else { Some(name) }
}

/// Strip one pair of **matching** string-literal quotes (`'…'` or
/// `"…"`) from the ends of `s`.
///
/// dbt given inputs are authored as Python/Jinja string literals, which
/// accept either quote character but require the open and close to be
/// the same one. A mixed pair (`"x'`), an unbalanced quote (`'x`), or a
/// bare token returns `None` — the fail-open posture both callers
/// ([`parse_ref_name`] / [`parse_source_ref`]) rely on (cute-dbt#245).
fn strip_matching_quotes(s: &str) -> Option<&str> {
    s.strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .or_else(|| s.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')))
}

/// Parse the `(source_name, table_name)` pair out of a unit-test
/// `given[].input` string of the form `source('a', 'b')` (cute-dbt#57)
/// — the exact sibling of [`parse_ref_name`].
///
/// Both engines serialize the given input **verbatim** from the authored
/// YAML (fusion clones the string into the manifest node; core
/// round-trips it byte-for-byte), and dbt's `source()` Jinja function
/// takes exactly two arguments, so the manifest string is the literal
/// authored call. The same tolerances as [`parse_ref_name`] apply:
/// case-insensitive keyword (`source` / `SOURCE` / …), whitespace
/// between the keyword and the parenthesis and around the arguments,
/// and either quote style with the matching-quote rule applied **per
/// argument** (each arg is its own string literal, so
/// `source("a", 'b')` is engine-valid; cute-dbt#245 — the
/// `name=`/`table_name=` kwargs variants stay engine-valid-but-rare and
/// deliberately deferred).
///
/// Returns `None` when the input does not match the `source('…','…')`
/// shape — including a call with more than two top-level arguments —
/// or when either name is empty. The caller treats `None` as "no
/// import-CTE match" and surfaces the design's empty-state copy
/// (fail-open, same as an unresolvable `ref`).
#[must_use]
pub fn parse_source_ref(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim();
    let prefix = trimmed.get(..6)?;
    if !prefix.eq_ignore_ascii_case("source") {
        return None;
    }
    let after_keyword = trimmed[6..].trim_start();
    let inside = after_keyword.strip_prefix('(')?.strip_suffix(')')?;
    // Top-level comma split into AT MOST two parts: dbt's source()
    // takes exactly two arguments, so a third comma-separated part —
    // a malformed 3-arg call, or a comma inside a quoted name pushing
    // the split past two fragments — rejects the call outright rather
    // than stripping the tail to a garbage pair (CodeRabbit PR #248).
    // A comma inside a quoted name that still yields exactly two parts
    // leaves a fragment with an unbalanced or mismatched quote pair,
    // which the matching-quote strip below rejects. Both paths fall
    // through to `None` — fail-open by construction.
    let mut parts = inside.splitn(3, ',');
    let first = parts.next()?;
    let second = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let source_name = strip_matching_quotes(first.trim())?;
    let table_name = strip_matching_quotes(second.trim())?;
    if source_name.is_empty() || table_name.is_empty() {
        None
    } else {
        Some((source_name, table_name))
    }
}

/// Bind a unit test's `given[]` entries to a named import-CTE.
///
/// Returns the first `given` whose `input` parses to `ref('NAME')` with
/// `NAME` matching `cte_name` (case-insensitive). dbt's manifest uses
/// case-insensitive identifiers for `ref(...)`; the case-sensitive
/// `CteNode::name()` value is matched against the case-folded ref name.
#[must_use]
pub fn bind_import_to_given<'t>(
    unit_test: &'t UnitTest,
    cte_name: &str,
) -> Option<&'t UnitTestGiven> {
    let target = cte_name.to_ascii_lowercase();
    unit_test.given().iter().find(|given| {
        parse_ref_name(given.input()).is_some_and(|n| n.eq_ignore_ascii_case(&target))
    })
}

/// Classify a graph node into a [`NodeRole`].
///
/// The terminal node (whose name equals [`TERMINAL_NODE_NAME`]) is
/// `Final`. A CTE with zero incoming edges whose body the engine
/// classified as `is_simple_from_shape` is `Import`. Everything else
/// is `Transform`. The renderer reads the engine-computed POD fact
/// directly; it never re-parses the CTE body slice (cute-dbt#40).
#[must_use]
pub fn classify_node_role(graph: &CteGraph, node_index: usize) -> NodeRole {
    let Some(node) = graph.nodes().get(node_index) else {
        return NodeRole::Transform;
    };
    if node.name() == TERMINAL_NODE_NAME {
        return NodeRole::Final;
    }
    let has_incoming = graph.edges().iter().any(|edge| edge.to() == node_index);
    if has_incoming {
        return NodeRole::Transform;
    }
    if node.is_simple_from_shape() {
        NodeRole::Import
    } else {
        NodeRole::Transform
    }
}

/// The per-model change-axis attribution as the JSON payload carries it
/// (cute-dbt#413, Slice B of the 3-axis Models lens). A render-owned
/// mirror of the domain [`ChangeAxes`] POD — render owns the wire
/// vocabulary, the domain stays free of the serde shape (the
/// `ModelYamlPayload` / `FindingPayload` precedent). The Models lens reads
/// these three bits to render the per-model axis chips (Body / Config /
/// Unit test), each reflecting exactly which of dbt's `state:modified`
/// sub-selectors this PR touched for the model.
///
/// Present (serialized) only on the `--pr-diff` arm, where the domain
/// populates `ScopeSelection.axes` for every in-scope model; the baseline
/// arm produces an empty map (the documented Option-A gap in
/// `scope.rs`), so its `ModelPayload::axes` is `None` and every baseline
/// golden stays byte-identical.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct AxesPayload {
    /// The model's `.sql` (`original_file_path`) is in the diff.
    pub body: bool,
    /// The model's `schema.yml` (`patch_path`) is in the diff.
    pub config: bool,
    /// The model hosts ≥1 in-scope unit test.
    pub unit_test: bool,
}

impl From<ChangeAxes> for AxesPayload {
    fn from(axes: ChangeAxes) -> Self {
        Self {
            body: axes.body,
            config: axes.config,
            unit_test: axes.unit_test,
        }
    }
}

/// One raw-Jinja zone projection (cute-dbt#448, Z2 — CORE). Projected once from
/// a `SpanRole::Zone` [`SourceMapEntry`] by `gather_raw_zones`: the zone's raw
/// `start`/`end` source positions, the 3-state `presence` verdict DERIVED ONCE
/// in Rust and emitted as a string the prototype renders verbatim (L6), and the
/// owning `node_id` back-ref DERIVED via `contains_range` — `null` (omitted)
/// when the zone compiled OUT (type-incapable of an edge, never-a-false-claim,
/// honesty principle 1). In a zone-free model `CodeMapPayload.raw_zones` is
/// empty and never serializes (`skip_serializing_if`), so pre-#448 goldens stay
/// byte-stable (L7).
#[derive(Debug, Clone, Serialize)]
pub struct RawZonePayload {
    /// Which control-flow construct this zone is (`incremental_guard` /
    /// `for_loop` on the wire).
    pub kind: ZoneKind,
    /// The zone's raw-source start position (`raw.start` — a zone always has a
    /// raw span).
    pub start: SourcePos,
    /// The zone's raw-source end position (`raw.end`).
    pub end: SourcePos,
    /// The 3-state presence verdict, DERIVED ONCE in Rust via
    /// [`SourceMapEntry::presence`] at this projection boundary and serialized
    /// as the `snake_case` string the prototype renders verbatim (`compiled_in` /
    /// `compiled_out` / `structural`). NEVER recomputed in JS (L6).
    pub presence: &'static str,
    /// The owning node, DERIVED via `contains_range`; `null` (omitted) when the
    /// zone compiled OUT — type-incapable of an edge (honesty principle 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

/// The per-model source-map render projection (cute-dbt#445) — Serialize-only,
/// PROJECTS the domain [`SourceMap`] spine fact. The faithful full `compiled`
/// text plus the derived `CteBody` node-span table; `raw_zones` is the deferred
/// `Zone` projection (empty in core S2).
#[derive(Debug, Clone, Serialize)]
pub struct CodeMapPayload {
    /// `SourceMap.compiled` — the one faithful text every span indexes into.
    pub compiled: String,
    /// Derived: the `CteBody` entries' compiled spans, keyed by node id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub node_spans: BTreeMap<String, SourceSpan>,
    /// Derived: the `Zone` entries (empty in core S2; the S4/S5 raw-zone path
    /// fills it).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub raw_zones: Vec<RawZonePayload>,
    /// Derived: the `SpanRole::Column` entries (cute-dbt#447, CLL-2) — each
    /// output column's compiled span, keyed `"node_id\u{1f}column"` (unit
    /// separator), a sub-range of the owning node span. The JS finds a column
    /// anchor as a `partition_point` over these nested under the `CteBody`
    /// entry. Empty (omitted) until CLL-2 resolves a column edge, so older
    /// goldens stay byte-stable.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub column_spans: BTreeMap<String, SourceSpan>,
    /// Derived: the RAW-coordinate `CteBody` node-span table (cute-dbt#469, S1)
    /// — the raw twin of [`Self::node_spans`], filled by the `raw_scan` adapter
    /// on a UNIQUE lexical match in the Jinja-masked raw text. A node whose raw
    /// origin is not uniquely anchored is OMITTED (never a picked offset). Empty
    /// (the whole key omitted) when no CTE resolves a raw span, so models
    /// without verbatim CTEs stay byte-stable on the wire.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub raw_node_spans: BTreeMap<String, SourceSpan>,
    /// Derived: the RAW-coordinate `Column` span table (cute-dbt#469, S1) — the
    /// raw twin of [`Self::column_spans`], keyed `"node_id\u{1f}column"`. A
    /// templated / macro-expanded / ambiguous column is OMITTED (no sound raw
    /// region). Empty (omitted) until a column resolves a unique raw anchor.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub raw_column_spans: BTreeMap<String, SourceSpan>,
}

impl CodeMapPayload {
    /// Project a domain [`SourceMap`] into the render payload — the faithful
    /// text, the derived `CteBody` node-span table, and the (deferred) raw
    /// zones. Pure projection: every field is read off the spine fact, never
    /// recomputed.
    fn from_source_map(sm: &SourceMap) -> Self {
        let node_spans = sm.node_spans();
        let raw_zones = gather_raw_zones(sm, &node_spans);
        // cute-dbt#447 (CLL-2) — flatten the `SpanRole::Column` entries into a
        // string-keyed map for the JS column-anchor lookup. The unit-separator
        // join keeps the (node_id, column) pair addressable in JS without a
        // nested object, and BTreeMap ordering keeps the wire deterministic.
        let column_spans = sm
            .column_spans()
            .into_iter()
            .map(|((node_id, column), span)| (format!("{node_id}\u{1f}{column}"), span))
            .collect();
        // cute-dbt#469 (S1) — the raw-coordinate twins, read off the `e.raw`
        // slot the `raw_scan` adapter filled (unique-match-only). Same shapes /
        // same unit-separator key as their compiled twins, so the JS sync layer
        // indexes them identically; omit-when-empty keeps pre-#469 goldens
        // byte-stable.
        let raw_node_spans = sm.raw_node_spans();
        let raw_column_spans = sm
            .raw_column_spans()
            .into_iter()
            .map(|((node_id, column), span)| (format!("{node_id}\u{1f}{column}"), span))
            .collect();
        Self {
            compiled: sm.compiled.clone(),
            node_spans,
            raw_zones,
            column_spans,
            raw_node_spans,
            raw_column_spans,
        }
    }
}

/// Project the `SpanRole::Zone` entries of a [`SourceMap`] into
/// [`RawZonePayload`]s (cute-dbt#448, Z2) — the single localized `gather_<feat>`
/// fn. For each zone entry: read `kind` + the raw `start`/`end`; DERIVE the
/// 3-state `presence` verdict ONCE via [`SourceMapEntry::presence`] over the
/// model's `CteBody` compiled spans, and serialize it as a string (L6); DERIVE
/// the owning `node_id` back-ref via `contains_range` against those same spans.
///
/// Never-a-false-claim (honesty principle 1): a `compiled: None` zone is
/// `compiled_out` with `node_id: None` — type-incapable of an edge. A zone with
/// no raw span is skipped (a zone ALWAYS has a raw origin; a raw-less Zone entry
/// is not a thing the scanner produces, but the projection degrades to absence
/// rather than fabricating a `(0,0)` span).
fn gather_raw_zones(
    sm: &SourceMap,
    node_spans: &BTreeMap<String, SourceSpan>,
) -> Vec<RawZonePayload> {
    // The CteBody compiled spans of THIS model — the presence/back-ref basis.
    let cte_spans: Vec<SourceSpan> = node_spans.values().copied().collect();
    sm.entries
        .iter()
        .filter_map(|e| {
            let SpanRole::Zone { kind } = &e.role else {
                return None;
            };
            let raw = e.raw?;
            let presence = e.presence(&cte_spans);
            // The owning node, DERIVED via contains_range — only when the zone
            // compiled IN. `compiled_out` ⇒ no edge (None), by construction.
            let node_id = e.compiled.and_then(|c| owning_node_id(node_spans, &c));
            Some(RawZonePayload {
                kind: *kind,
                start: raw.start,
                end: raw.end,
                presence: presence.to_wire(),
                node_id,
            })
        })
        .collect()
}

/// The smallest `CteBody` node whose compiled span CONTAINS `compiled` — the
/// zone's owning node back-ref (cute-dbt#448). "Smallest" so a Structural zone
/// nested inside several enclosing bodies binds to the TIGHTEST one (the
/// containing CTE, not the whole terminal select). Returns `None` if no node
/// span contains the zone's compiled span (an honest no-edge, never fabricated).
fn owning_node_id(
    node_spans: &BTreeMap<String, SourceSpan>,
    compiled: &SourceSpan,
) -> Option<String> {
    node_spans
        .iter()
        .filter(|(_, span)| span.contains_range(compiled))
        .min_by_key(|(_, span)| span.end.byte.saturating_sub(span.start.byte))
        .map(|(node_id, _)| node_id.clone())
}

/// Scan `raw` for control zones ([`locate_raw_zones`]) and append one
/// `SpanRole::Zone` [`SourceMapEntry`] per located zone to `sm.entries`
/// (cute-dbt#448, Z2). Each entry's `compiled` span is resolved by HONEST
/// token-location in `compiled`: the zone's longest literal SQL fragment, found
/// in the compiled text ⇒ `Some(span)`; absent (e.g. an `is_incremental()`
/// guard pruned this build) ⇒ `None` — the honest "pruned this build" verdict,
/// NEVER fabricated. The cute-dbt#40 adapter-parses-domain-holds pattern.
fn append_zone_entries(sm: &mut SourceMap, raw: &str, compiled: &str) {
    for zone in locate_raw_zones(raw) {
        let compiled_span = resolve_zone_compiled(raw, &zone.raw_span, compiled);
        sm.entries.push(SourceMapEntry {
            role: SpanRole::Zone { kind: zone.kind },
            raw: Some(zone.raw_span),
            compiled: compiled_span,
        });
    }
}

/// Resolve a zone's compiled span by honest token-location (cute-dbt#448, §6).
/// Extract the zone body's literal SQL fragments (Jinja `{%…%}`/`{{…}}`/`{#…#}`
/// stripped, longest first), then locate the BEST UNAMBIGUOUS fragment in
/// `compiled`. A fragment is usable only when it occurs EXACTLY ONCE in
/// `compiled`: a fragment that appears MULTIPLY (e.g. a pruned guard whose
/// literal also lives outside the zone region) could bind the FIRST occurrence
/// — a region OUTSIDE the actual zone — and fabricate a false
/// `Some`/`CompiledIn`. The honest verdict there is absence: try the next-best
/// fragment, and if NONE is unambiguous, return `None` (`CompiledOut`). Degrade,
/// never guess (never-a-false-claim). Raw-text matching is the LAST-RESORT
/// mechanism the shaping permits; this fix makes it ambiguity-safe so it cannot
/// lie about presence.
fn resolve_zone_compiled(raw: &str, raw_span: &SourceSpan, compiled: &str) -> Option<SourceSpan> {
    let body = raw.get(raw_span.byte_range())?;
    // Candidates longest-first; bind the first that resolves UNAMBIGUOUSLY.
    for anchor in literal_fragments(body) {
        // The anchor must be a meaningful token run, not a stray symbol — a
        // single char/whitespace match would over-bind. Require ≥3
        // non-whitespace bytes.
        if anchor.chars().filter(|c| !c.is_whitespace()).count() < 3 {
            continue;
        }
        // UNAMBIGUOUS ONLY: bind iff the anchor occurs exactly once. A multiply
        // occurring anchor cannot tell the zone's region from a coincidental
        // twin elsewhere in `compiled`, so binding it would risk a false claim.
        let mut occurrences = compiled.match_indices(anchor);
        let first = occurrences.next();
        let unique = occurrences.next().is_none();
        if let Some((at, _)) = first.filter(|_| unique) {
            // Exactly one occurrence — an honest, unambiguous bind.
            return byte_span(compiled, at, at + anchor.len());
        }
        // 0 occurrences (absent) or ≥2 (ambiguous): try the next-best candidate.
    }
    // No unambiguous literal anchor present ⇒ honest absence (CompiledOut).
    None
}

/// Every contiguous literal (non-Jinja) text fragment in a zone body, ORDERED
/// LONGEST-FIRST (cute-dbt#448) — the token-location anchor candidates. Strips
/// `{%…%}` / `{{…}}` / `{#…#}` constructs (each is a boundary) and trims each
/// remaining run. Empty when the body is all-Jinja / whitespace. The
/// ambiguity-safe [`resolve_zone_compiled`] walks these in order, binding the
/// first that occurs EXACTLY ONCE in the compiled text.
fn literal_fragments(body: &str) -> Vec<&str> {
    let bytes = body.as_bytes();
    let n = bytes.len();
    let mut frags: Vec<(usize, usize)> = Vec::new();
    let mut seg_start = 0usize;
    let mut i = 0usize;
    let push_seg = |start: usize, end: usize, frags: &mut Vec<(usize, usize)>| {
        let frag = body[start..end].trim();
        if !frag.is_empty() {
            // Map the trimmed fragment back to byte offsets within `body`.
            let lead = body[start..end].len() - body[start..end].trim_start().len();
            let fs = start + lead;
            let fe = fs + frag.len();
            frags.push((fs, fe));
        }
    };
    while i < n {
        if bytes[i] == b'{' && i + 1 < n && matches!(bytes[i + 1], b'%' | b'{' | b'#') {
            // Close the current literal segment, then skip the Jinja construct.
            push_seg(seg_start, i, &mut frags);
            let close_delim = match bytes[i + 1] {
                b'%' => b'%',
                b'{' => b'}',
                _ => b'#',
            };
            // Find the construct's closer (literal scan — anchors don't need
            // string-literal fidelity, only a boundary; on no closer, stop).
            let mut j = i + 2;
            let mut closed = None;
            while j + 1 < n {
                if bytes[j] == close_delim && bytes[j + 1] == b'}' {
                    closed = Some(j + 2);
                    break;
                }
                j += 1;
            }
            let Some(after) = closed else {
                // Unterminated construct: nothing literal beyond here.
                seg_start = n;
                break;
            };
            i = after;
            seg_start = after;
        } else {
            i += 1;
        }
    }
    push_seg(seg_start, n, &mut frags);
    // Longest-first: the most specific anchor is tried before shorter ones.
    frags.sort_by_key(|(s, e)| std::cmp::Reverse(e - s));
    frags.into_iter().map(|(s, e)| &body[s..e]).collect()
}

/// The longest contiguous literal (non-Jinja) text fragment in a zone body —
/// the single best token-location anchor (cute-dbt#448). Thin wrapper over the
/// longest-first [`literal_fragments`]. `None` when the body is all-Jinja /
/// whitespace. Test-only since the ambiguity-safe [`resolve_zone_compiled`]
/// walks the full [`literal_fragments`] candidate list, not just the head.
#[cfg(test)]
fn longest_literal_fragment(body: &str) -> Option<&str> {
    literal_fragments(body).into_iter().next()
}

/// The per-model column-lineage render projection (cute-dbt#446, CLL-1) —
/// Serialize-only. In CLL-1 it carries ONLY the Tier-2 `context` half (the
/// pure manifest fold: per-column definition + tested-by); the Tier-1 `edges`
/// array is filled by CLL-2 (`SpanRole::Column` provenance) and is omitted
/// from the wire entirely until then. The `context`-only shape lets the
/// column-selection UX ship before any edge math, and keeps every pre-#446
/// golden byte-stable (the whole key is omitted when a model has no documented
/// or tested columns).
#[derive(Debug, Clone, Serialize)]
pub struct ColumnLineagePayload {
    /// Per-column context keyed by column name — definition (`data_type` +
    /// `description`) + tested-by (`TestFact`s) + the `documented` honesty
    /// flag. Omitted when empty (an undocumented, untested model) so older
    /// goldens stay byte-identical.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, ColumnContext>,
    /// Tier-1 intra-model column-provenance edges (cute-dbt#447, CLL-2) —
    /// pass-through / rename derivations, each with an honest `confidence`.
    /// Omitted when empty (a model with no statically-resolvable column
    /// edges) so pre-#447 goldens stay byte-stable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<ColumnEdge>,
}

/// Per-model entry in the JSON payload — mirrors the design's
/// `window.CUTE_DBT_SAMPLE.models[i]` shape so the inlined interaction
/// script consumes it without remapping.
#[derive(Debug, Clone, Serialize)]
pub struct ModelPayload {
    /// Bare model name (e.g. `customer_rollup`) — the model selector
    /// label and the terminal-node id in the DAG.
    pub name: String,
    /// Full project-relative path of the model's source file
    /// (`Node::original_file_path`, e.g. `models/staging/stg_orders.sql`)
    /// — the Model-SQL code-card file-path header (cute-dbt#179; founder
    /// call: the full `models/…/x.sql`, not the bare filename). `None`
    /// (key omitted, older fixtures stay byte-stable) when the manifest
    /// carries no `original_file_path`; the JS then falls back to the
    /// synthesized `<name>.sql` (the cute-dbt#155 terminal label).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Authored model description (cute-dbt#200) — the model node's
    /// top-level `description`, surfaced as the in-card model context
    /// (handoff README §2.5). `None` (key omitted — pre-#200 payloads
    /// and undescribed models stay byte-stable) when the manifest
    /// carries no prose (the adapter drops dbt-core's empty-string
    /// unset shape).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// DAG nodes + edges, keyed for the design's JS.
    pub dag: DagPayload,
    /// Per-node compiled SQL, keyed by node id (CTE name or the stable
    /// terminal id for the final select). Empty when the CTE engine could
    /// not parse (the model card still renders the metadata + tests + an
    /// empty DAG). A DERIVED PROJECTION of [`Self::code_map`]'s source map
    /// (cute-dbt#445) — `compiled_sql[id]` byte-equals
    /// `code_map.compiled[node_spans[id].byte_range()]` by construction.
    pub compiled_sql: BTreeMap<String, String>,
    /// The per-model source map projection (cute-dbt#445) — the faithful
    /// full compiled text plus the `CteBody` node-span table. The single
    /// source of truth `compiled_sql` derives from; the JS DAG↔code sync
    /// (cute-dbt#446) indexes the compiled `<pre>` through `node_spans`.
    /// `None` only when the model has no compiled code (a seed/source); the
    /// key is omitted then so older fixtures stay byte-stable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_map: Option<CodeMapPayload>,
    /// Column-lineage context (cute-dbt#446, CLL-1) — the Tier-2 pure
    /// manifest fold: per-column definition (`data_type` + `description`) +
    /// tested-by + the `documented` honesty flag. `None` (key omitted) when
    /// the model has no documented or tested columns, so every pre-#446
    /// golden stays byte-stable. The Tier-1 column edges (#447, CLL-2) land
    /// as an additive `edges` array inside this section later.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_lineage: Option<ColumnLineagePayload>,
    /// Raw Jinja source of the model file (`models/**/*.sql`).
    /// Surfaced verbatim in the per-model "Model SQL" expandable
    /// section (cute-dbt#47). `None` only when the manifest lacks
    /// `raw_code` (defensive — dbt 1.8+ populates this on every node).
    /// Skipped from the JSON payload when `None` so older fixtures
    /// stay stable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_sql: Option<String>,
    /// Inline diff of this model's RAW SQL (`raw_code`) when the PR diff
    /// changed the model's `.sql` (cute-dbt#111) — present only in PR-diff
    /// mode, block aligned + touched + a substantive (non-whitespace)
    /// change. `None` (key omitted) for baseline mode, models in scope
    /// only via a changed test, stale diffs, and whitespace-only edits, so
    /// the template's Model SQL section falls back to the plain raw view.
    /// Mirrors [`TestPayload::yaml_diff`]'s threading exactly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_diff: Option<BlockDiff>,
    /// The model's authored schema-file `models:` entry (cute-dbt#247) —
    /// the Model-YAML section, peer of Model SQL. Built from the cli's
    /// `gather_model_yaml` outcome (`Node.patch_path` read through the
    /// `ProjectFileReader`): either the sliced block (`raw`, plus a
    /// `diff` in PR-diff mode when the diff edited it) or a truthful
    /// `missing` placeholder naming what is absent. `None` (key omitted
    /// — pre-#247 payloads and render paths that skip the gather stay
    /// byte-stable) only when no gather outcome exists for the model;
    /// the template then hides the section entirely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_yaml: Option<ModelYamlPayload>,
    /// Unit tests targeting this model that are in scope. Empty
    /// `[]` triggers the "0 unit tests wired" empty state.
    pub tests: Vec<TestPayload>,
    /// `true` when the model's compiled SQL was a `WITH RECURSIVE`
    /// query — the template surfaces a banner; the recursive arm has
    /// already been omitted from the graph by the CTE engine.
    pub is_recursive: bool,
    /// `true` when `config.materialized == "incremental"` (cute-dbt#145).
    /// Drives the model-header incremental badge and gates the per-test
    /// mode badge / expect-semantics tooltip (the template reads this
    /// enclosing-model flag — the `is_recursive` precedent — rather than
    /// denormalizing it onto each test). Serialized only on incremental
    /// models (`skip_serializing_if = std::ops::Not::not`) so the example
    /// diff stays localized to incremental models; the template's
    /// `!(m && m.is_incremental)` read is undefined-safe when omitted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_incremental: bool,
    /// Coverage-intelligence findings for this model (cute-dbt#169) —
    /// the per-(construct, check) verdicts the check engine computed
    /// during payload assembly ([`model_findings`]: evaluate ALL
    /// registered checks → resolve supersedes; display filtering is a
    /// separate downstream concern). Since cute-dbt#170 each entry is a
    /// [`FindingPayload`] — the domain [`Finding`] flattened verbatim
    /// plus the render-resolved `pin_node` / `sketches` fields the
    /// findings surface consumes. Omitted from JSON when empty so every
    /// pre-#169 payload (and the committed goldens whose models trip no
    /// check) stays byte-stable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<FindingPayload>,
    /// Config-tree provenance chips (cute-dbt#267): the `dbt_project.yml`
    /// subtree edits whose fqn-resolved value changed for this model —
    /// the reason a config-widened model is in the report. The JS
    /// renders one chip per entry
    /// (`+materialized via dbt_project.yml · models.shop.marts`).
    /// Omitted from JSON when empty (baseline mode, unwidened models,
    /// every pre-#267 payload stays byte-stable).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub config_attributions: Vec<ConfigAttribution>,
    /// Var-reference chips (cute-dbt#268): the edited `dbt_project.yml`
    /// vars this model references, tiered (DIRECT / CONFIG / MACRO).
    /// Context only — a var edit never widens scope, so chips appear
    /// exactly on models that are in scope for some OTHER reason. The
    /// JS renders one chip per entry
    /// (`reads var 'dq_threshold' · direct`). Omitted from JSON when
    /// empty (baseline mode + every pre-#268 payload stays byte-stable).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub var_references: Vec<VarReference>,
    /// Per-model change-axis attribution (cute-dbt#413) — which of
    /// `{body, config, unit_test}` fired for this in-scope model. Drives
    /// the Models-lens axis chips. `None` (key omitted) outside the
    /// `--pr-diff` arm: the domain `ScopeSelection.axes` map is empty in
    /// baseline mode (the documented Option-A gap), so every baseline
    /// golden stays byte-identical. Attached by
    /// `build_payload_with_externals` from the threaded `axes` map (this
    /// builder never sees the scope selection's per-axis record).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub axes: Option<AxesPayload>,
    /// The model's `schema.yml` (its scheme-stripped `patch_path`) — the
    /// non-interactive config-file chip in the Models lens AND the shared
    /// grouping key the model `<select>` uses to build its `<optgroup>`s
    /// (every model patched by the same schema file groups together).
    /// `None` (key omitted) outside the `--pr-diff` arm (gated to the same
    /// `axes`-present models) and for any model with no `patch_path`, so
    /// baseline goldens stay byte-identical. The grouping is presentation
    /// only — it never re-scopes (the filter toggle is the separate
    /// cute-dbt#414 Slice C).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_file: Option<String>,
    /// The model's mutually-exclusive top-level state (cute-dbt#416) — the
    /// wire form of the domain [`ModelState`]: `"new"` or `"modified"`. The
    /// Models lens renders a NEW state chip (alongside the 3-axis MODIFIED
    /// chips) when this is `"new"`; `"modified"` renders no extra state chip
    /// (the axis chips already say MODIFIED). `None` (key omitted) outside
    /// the `--pr-diff` arm — the baseline arm produces an empty
    /// `ScopeSelection.model_states` map (the `axes` Option-A gap), so every
    /// baseline golden stays byte-identical. Attached by
    /// `build_payload_with_externals` from the threaded `model_states` map.
    /// [`ModelState::Removed`] never appears here — removed models are
    /// node-less and surfaced via [`ReportPayload::removed_models`].
    ///
    /// [`ModelState`]: crate::domain::ModelState
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'static str>,
}

/// The Model-YAML section's render shape (cute-dbt#247): the model's
/// authored schema-file `models:` entry, or a truthful degrade.
///
/// Exactly one of [`raw`](Self::raw) / [`missing`](Self::missing) is
/// set. `Rust computes, JS only renders`: the degrade copy is composed
/// here (the private `model_yaml_payload` mapping) from the domain
/// [`ModelYamlOutcome`], so the wording lives in one testable place and
/// the template never invents text.
#[derive(Debug, Clone, Serialize)]
pub struct ModelYamlPayload {
    /// Project-relative schema file path (the scheme-stripped manifest
    /// `patch_path`) — the code-card header label. `None` (key omitted)
    /// only on the no-`patch_path` degrade (there is no file to name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The sliced authored `models:` entry, verbatim (the
    /// [`crate::domain::extract_model_block`] slice — leading/trailing
    /// comments included). `None` on every degrade arm.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// Inline diff of the block (PR-diff mode, block aligned + touched +
    /// substantive — `attach_model_yaml_diffs`). `None` in baseline mode
    /// and whenever the diff did not edit this block, so the section
    /// shows the plain File view. Same Diff/File semantics as
    /// [`ModelPayload::sql_diff`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<BlockDiff>,
    /// Truthful placeholder copy when the authored block could not be
    /// surfaced — names exactly what is missing (no `patch_path`, no
    /// `--project-root`, file missing/unreadable, entry not found). The
    /// template renders this text verbatim; it never shows an empty or
    /// misleading section.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<String>,
}

/// Map a model's gather outcome onto the Model-YAML render shape
/// (cute-dbt#247). `None` in → `None` out (no gather ran for this model
/// — direct render paths and the explore arm — so the template hides
/// the section). `model_name` is the bare model name, used by the
/// entry-not-found copy.
fn model_yaml_payload(
    outcome: Option<&ModelYamlOutcome>,
    model_name: &str,
) -> Option<ModelYamlPayload> {
    let payload = match outcome? {
        ModelYamlOutcome::Found { path, block, diff } => ModelYamlPayload {
            path: Some(path.clone()),
            raw: Some(block.raw.clone()),
            diff: diff.clone(),
            missing: None,
        },
        ModelYamlOutcome::NoPatchPath => ModelYamlPayload {
            path: None,
            raw: None,
            diff: None,
            missing: Some(
                "No schema file declares this model in the manifest — there is no authored \
                 models: entry to show."
                    .to_owned(),
            ),
        },
        ModelYamlOutcome::NoProjectRoot { path } => ModelYamlPayload {
            path: Some(path.clone()),
            raw: None,
            diff: None,
            missing: Some(format!(
                "Schema file {path} was not read — re-run with --project-root to surface the \
                 authored model YAML."
            )),
        },
        ModelYamlOutcome::FileMissing { path } => ModelYamlPayload {
            path: Some(path.clone()),
            raw: None,
            diff: None,
            missing: Some(format!(
                "Schema file {path} was not found under the project root."
            )),
        },
        ModelYamlOutcome::Unreadable { path } => ModelYamlPayload {
            path: Some(path.clone()),
            raw: None,
            diff: None,
            missing: Some(format!("Schema file {path} could not be read.")),
        },
        ModelYamlOutcome::EntryNotFound { path } => ModelYamlPayload {
            path: Some(path.clone()),
            raw: None,
            diff: None,
            missing: Some(format!(
                "No models: entry named \"{model_name}\" was found in {path}."
            )),
        },
    };
    Some(payload)
}

/// One coverage finding in render shape (cute-dbt#170): the domain
/// [`Finding`] — serialized FLAT, so every cute-dbt#169 wire key
/// (`check` / `tier` / `instrument` / `model_id` / `construct` /
/// `verdict` / `evidence` / `recommendation` / `suppressed`) is
/// unchanged — plus the two render-resolved fields the findings panel
/// consumes. Rust computes, JS only renders.
#[derive(Debug, Clone, Serialize)]
pub struct FindingPayload {
    /// The policy-applied domain finding, flattened onto this object.
    #[serde(flatten)]
    pub finding: Finding<HeuristicId>,
    /// The DAG node id this finding's evidence pins to: a
    /// `group[<node>]` construct (e.g. `union[combined_metrics]`) pins
    /// the named CTE node; a model-level construct (e.g.
    /// `config.unique_key`) pins the terminal node. `None` (key
    /// omitted) when the model's graph is empty or the named node is
    /// not in the graph — the template then renders no pin affordance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_node: Option<String>,
    /// Copy-pasteable fixture sketches LIFTED out of the evidence list
    /// (the `SUGGESTED_GIVEN_LABEL` entries the union check emits,
    /// cute-dbt#172) — the template renders each as a copyable code
    /// block instead of a plain evidence row. Omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sketches: Vec<String>,
}

/// The evidence label the union check stamps on its copy-pasteable
/// given-row sketches (`suggested_given_sketch` in
/// `src/domain/checks.rs`). [`finding_payload`] lifts these entries into
/// [`FindingPayload::sketches`]; everything else stays plain evidence.
const SUGGESTED_GIVEN_LABEL: &str = "suggested given";

/// Resolve the DAG node a finding pins to (see
/// [`FindingPayload::pin_node`]). Bracketed constructs name an engine
/// node verbatim (`union[<consumer>]`) or qualify it with a sub-construct
/// segment (`left_join[<consumer>:<right>]`, cute-dbt#173) — the consumer
/// node is the pin either way; the match is case-insensitive because SQL
/// identifiers fold.
fn resolve_pin_node(graph: &CteGraph, construct: &str) -> Option<String> {
    let node_named = |name: &str| {
        graph
            .nodes()
            .iter()
            .find(|node| node.name().eq_ignore_ascii_case(name))
            .map(|node| node.name().to_owned())
    };
    let named = construct
        .find('[')
        .and_then(|open| construct[open + 1..].strip_suffix(']'))
        .and_then(|name| {
            node_named(name).or_else(|| {
                name.split(':')
                    .next()
                    .filter(|c| !c.is_empty())
                    .and_then(node_named)
            })
        });
    named.or_else(|| node_named(TERMINAL_NODE_NAME))
}

/// Wrap one policy-applied domain [`Finding`] into its render shape:
/// resolve the pin target and lift the `suggested given` evidence
/// entries into [`FindingPayload::sketches`].
fn finding_payload(graph: &CteGraph, mut finding: Finding<HeuristicId>) -> FindingPayload {
    let pin_node = resolve_pin_node(graph, &finding.construct);
    let (sketches, evidence): (Vec<_>, Vec<_>) = std::mem::take(&mut finding.evidence)
        .into_iter()
        .partition(|entry| entry.label == SUGGESTED_GIVEN_LABEL);
    finding.evidence = evidence;
    FindingPayload {
        finding,
        pin_node,
        sketches: sketches.into_iter().map(|entry| entry.value).collect(),
    }
}

/// Base URL of the published book's GENERATED check pages
/// (`book/src/checks/<id>.md` → `<base><id>.html`; mdBook `site-url`
/// `/cute-dbt/`). Rides the payload as [`CheckSpecPayload::book_href`]
/// and renders as a plain click-only `<a>` — never fetched at load, so
/// the zero-egress gate holds (the report makes zero requests until the
/// user deliberately leaves it).
const BOOK_CHECKS_BASE: &str = "https://breezy-bays-labs.github.io/cute-dbt/checks/";

/// The spec catalog entry for one registered check (cute-dbt#170) —
/// everything the inline rationale drawer ("what is this check?")
/// renders OFFLINE, denormalized from the [`HeuristicId`] spec statics
/// so the JS never reaches back into Rust. Carried once per check id in
/// [`ReportPayload::check_specs`], not per finding.
#[derive(Debug, Clone, Serialize)]
pub struct CheckSpecPayload {
    /// Human-facing display name (e.g. `Unexercised UNION arm`).
    pub name: &'static str,
    /// Check group (the dotted id's prefix).
    pub group: &'static str,
    /// Accuracy tier — labeled in the UI, never blended.
    pub tier: Tier,
    /// Recommended testing instrument.
    pub instrument: Instrument,
    /// Prose mirror of the trigger + satisfaction predicate.
    pub conditions: &'static [&'static str],
    /// Shapes the check deliberately stays silent (or `UNKNOWN`) on.
    pub exclusions: &'static [&'static str],
    /// Why the gap matters — embedded inline (zero-egress).
    pub rationale: &'static str,
    /// Outbound link to the generated book check page — click-only.
    pub book_href: String,
}

/// Build the [`CheckSpecPayload`] for one registered check.
fn check_spec_payload(id: HeuristicId) -> CheckSpecPayload {
    let spec = id.spec();
    CheckSpecPayload {
        name: spec.name,
        group: spec.group,
        tier: spec.tier,
        instrument: spec.instrument,
        conditions: spec.conditions,
        exclusions: spec.exclusions,
        rationale: spec.rationale,
        book_href: format!("{BOOK_CHECKS_BASE}{}.html", spec.id_str),
    }
}

/// One DAG node — stable id, display label, and role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodePayload {
    /// Stable node id — the key for the DAG (Mermaid node), the
    /// `compiled_sql` map, edge endpoints, and given→node binding. Always
    /// the engine's node name: a CTE alias for CTE nodes, or the
    /// collision-proof [`TERMINAL_NODE_NAME`] for the terminal. The model's
    /// bare name is NEVER the id (it rides in [`Self::label`]) — keeping it
    /// out of the id is what stops a self-named import CTE (`with orders as
    /// (...)` on the `orders` model) from collapsing into the terminal node
    /// (cute-dbt#155).
    pub id: String,
    /// Human-facing label for the DAG node + the node-detail title.
    /// `Some(model_name)` for the terminal (so it reads as the model, not
    /// the literal `(final select)`); `None` for CTE nodes, where the
    /// template falls back to [`Self::id`]. Omitted from JSON when `None`
    /// so CTE node payloads — and the byte-gated examples — stay minimal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Render-layer classification (see [`NodeRole`]).
    pub role: NodeRole,
}

/// One DAG edge — `from` and `to` are the [`NodePayload::id`] strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EdgePayload {
    /// Source node id (the [`NodePayload::id`] the edge starts from).
    pub from: String,
    /// Destination node id (the [`NodePayload::id`] the edge ends at).
    pub to: String,
    /// Edge classification driving the Mermaid edge color + legend
    /// entry (`from` / `inner` / `left` / `right` / `full` / `cross` /
    /// `union_all` / `union_distinct`).
    pub edge_type: EdgeType,
}

/// The full DAG carried in a [`ModelPayload`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DagPayload {
    /// CTE nodes plus the terminal model node, in stable rendering
    /// order.
    pub nodes: Vec<NodePayload>,
    /// Directed edges between [`Self::nodes`] entries, classified by
    /// [`EdgePayload::edge_type`].
    pub edges: Vec<EdgePayload>,
}

/// Column-header metadata for one fixture-table column (cute-dbt#165):
/// the model's authored column description plus the summarized
/// column-level data tests. Rendered by the template as the th tooltip
/// bubble — the payload is the complete renderable POD (Rust computes,
/// JS only renders).
///
/// Emitted only for columns that actually appear in the carrying
/// given/expect [`FixtureTable`] AND have at least a description or one
/// test (no empty bubbles); a column with neither simply has no map
/// entry, which the template reads as "no affordance".
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ColumnMetaPayload {
    /// Authored column description from the owning model node's
    /// `columns` map. Omitted from JSON when the column has tests but no
    /// description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Structured column-level data tests (built by
    /// [`column_test_payload`], the handoff README §2.2 display-string
    /// mapping): each entry carries the display `name` plus — distinctly —
    /// the `accepted_values` args (rendered as pills) or the
    /// relationships/range `detail` (muted mono). Deterministically
    /// ordered (name, values, detail, then test node id). Omitted from
    /// JSON when the column is described but untested.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<ColumnTestPayload>,
}

/// One column-scoped data test in display shape (cute-dbt#178, the
/// handoff README §2.2 contract). The JS renders `name` in the accent
/// color, each `values` entry as a chip, and `detail` as muted mono —
/// so the three are carried distinctly, never pre-joined.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ColumnTestPayload {
    /// Display name: `unique` / `not null` / `accepted values` /
    /// `relationships` / `accepted range` for the known built-ins (the
    /// §2.2 prose forms), else the package-qualified raw test name
    /// (identifiers are never prose-mangled).
    pub name: String,
    /// `accepted_values` args, one chip per authored value. Empty (and
    /// omitted from JSON) for every other test.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// Muted mono detail: `relationships` → `"model.field"`;
    /// `accepted_range` → `"0–100"` / `"≥ 0"` / `"≤ 1"`. `None` (and
    /// omitted) when the test carries no interpretable detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One entry of the report-level `manifest_nodes` lookup (cute-dbt#200,
/// handoff README §2.5–2.6): the model context the node-detail shelf
/// (cute-dbt#201) and the model-ref / expected-model hover cards
/// (cute-dbt#202) render — keyed by BARE model name in
/// [`ReportPayload::manifest_nodes`]. A model absent from the lookup is
/// the graceful no-hover (the JS contract); a present entry always has
/// at least one non-empty field (`build_manifest_nodes` skips
/// all-empty entries so bare synthetic fixtures stay byte-stable).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ManifestNodePayload {
    /// Authored model description ([`Node::description`] — the adapter
    /// already dropped dbt-core's empty-string unset shape).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `config.materialized` (`"view"` / `"table"` / `"incremental"` /
    /// …) — the already-ingested cute-dbt#145 accessor, re-plumbed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materialized: Option<String>,
    /// Resolved model tags ([`Node::tags`] — the deduplicated top-level
    /// wire list).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The model's declared columns with their per-column context, in
    /// deterministic name order. Empty (key omitted) for models without
    /// a columns block.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ManifestColumnPayload>,
    /// MODEL-LEVEL data tests (cute-dbt#200): ingested test nodes with
    /// `attached_node` = this model and `column_name` = `None`, mapped
    /// through the same §2.2 display vocabulary as column tests. A
    /// SECOND, model-scoped projection — the per-table
    /// `given/expected.column_meta` th-tooltips (#165/#166) are
    /// untouched.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub model_tests: Vec<ModelTestPayload>,
}

/// One declared column in a [`ManifestNodePayload`] — name, authored
/// description, declared `data_type`, and the column-scoped data tests
/// in the SHIPPED [`ColumnTestPayload`] display shape (cute-dbt#166/#189
/// — reused, never a parallel type).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ManifestColumnPayload {
    /// Column name as declared in the model's `columns` map.
    pub name: String,
    /// Authored column description (the #165 ingestion; empty-string
    /// unset shapes already dropped).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Declared `data_type` from the already-ingested [`Node::columns`]
    /// map (`None` — key omitted — for untyped columns).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub column_type: Option<String>,
    /// Column-scoped data tests, the §2.2 display mapping (same entries
    /// the per-table `column_meta` carries for this column).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<ColumnTestPayload>,
}

/// One MODEL-LEVEL data test in display shape (cute-dbt#200): the
/// [`column_test_payload`] §2.2 mapping reduced to `name` + `detail`
/// (known built-ins keep their prose names + detail; unknown tests carry
/// the package-qualified raw name with no detail — their open-ended arg
/// vocabularies stay uninterpreted, the v1 stance).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ModelTestPayload {
    /// Display name (the §2.2 vocabulary or the package-qualified raw
    /// test name).
    pub name: String,
    /// Muted mono detail when the test carries an interpretable one
    /// (`relationships` target / range bound). `None` — key omitted —
    /// otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Structured display mapping of a column-scoped generic test
/// (cute-dbt#165 → restructured for cute-dbt#178 per the handoff README
/// §2.2 table):
///
/// | dbt test | `name` | `values` / `detail` |
/// |---|---|---|
/// | `unique` | `unique` | — |
/// | `not_null` | `not null` | — |
/// | `accepted_values` | `accepted values` | `values` = the list (pills) |
/// | `relationships (to: ref('m'), field: f)` | `relationships` | `detail = "m.f"` |
/// | `accepted_range (min/max)` | `accepted range` | `detail = "0–100"` / `"≥ 0"` / `"≤ 1"` |
///
/// Any other test (incl. package tests like `dbt_expectations.*`)
/// carries its package-qualified raw name with no values/detail — their
/// arg vocabularies are open-ended and v1 does not interpret them.
/// `accepted_range` maps by bare name regardless of namespace (it
/// usually ships as `dbt_utils.accepted_range`).
#[must_use]
pub fn column_test_payload(tm: &TestMetadata) -> ColumnTestPayload {
    let qualified = match tm.namespace() {
        Some(ns) => format!("{ns}.{}", tm.name()),
        None => tm.name().to_owned(),
    };
    match tm.name() {
        "unique" => ColumnTestPayload {
            name: "unique".to_owned(),
            ..ColumnTestPayload::default()
        },
        "not_null" => ColumnTestPayload {
            name: "not null".to_owned(),
            ..ColumnTestPayload::default()
        },
        "accepted_values" => {
            let values = tm
                .kwargs()
                .get("values")
                .and_then(Value::as_array)
                .map(|values| values.iter().map(scalar_token).collect())
                .unwrap_or_default();
            ColumnTestPayload {
                name: "accepted values".to_owned(),
                values,
                detail: None,
            }
        }
        "relationships" => ColumnTestPayload {
            name: "relationships".to_owned(),
            values: Vec::new(),
            detail: relationships_detail(tm),
        },
        "accepted_range" => ColumnTestPayload {
            name: "accepted range".to_owned(),
            values: Vec::new(),
            detail: accepted_range_detail(tm),
        },
        _ => ColumnTestPayload {
            name: qualified,
            ..ColumnTestPayload::default()
        },
    }
}

/// `relationships` detail per README §2.2: `to: ref('m'), field: f` →
/// `"m.f"`. The `to` target unwraps a `ref('…')` / `source('…','…')`
/// jinja call to its last quoted name; a non-call target renders
/// verbatim. Field-less relationships show just the target.
fn relationships_detail(tm: &TestMetadata) -> Option<String> {
    let to = tm.kwargs().get("to").and_then(Value::as_str)?;
    let target = unquote_last_jinja_arg(to);
    match tm.kwargs().get("field").and_then(Value::as_str) {
        Some(field) => Some(format!("{target}.{field}")),
        None => Some(target),
    }
}

/// The last single-quoted argument of a jinja-ish call (`ref('m')` →
/// `m`; `source('raw', 'orders')` → `orders`), or the input verbatim
/// when no quoted argument is present.
fn unquote_last_jinja_arg(value: &str) -> String {
    let mut last: Option<&str> = None;
    let mut rest = value;
    while let Some(open) = rest.find('\'') {
        let tail = &rest[open + 1..];
        let Some(close) = tail.find('\'') else { break };
        last = Some(&tail[..close]);
        rest = &tail[close + 1..];
    }
    last.unwrap_or(value).to_owned()
}

/// `accepted_range` detail per README §2.2: both bounds → `"min–max"`
/// (en dash); min only → `"≥ min"`; max only → `"≤ max"`; neither →
/// `None`. Bounds render via [`scalar_token`] (authored JSON scalars).
fn accepted_range_detail(tm: &TestMetadata) -> Option<String> {
    let min = tm.kwargs().get("min_value").filter(|v| !v.is_null());
    let max = tm.kwargs().get("max_value").filter(|v| !v.is_null());
    match (min, max) {
        (Some(min), Some(max)) => Some(format!(
            "{}\u{2013}{}",
            scalar_token(min),
            scalar_token(max)
        )),
        (Some(min), None) => Some(format!("\u{2265} {}", scalar_token(min))),
        (None, Some(max)) => Some(format!("\u{2264} {}", scalar_token(max))),
        (None, None) => None,
    }
}

/// Display token for one `accepted_values` entry: a JSON string renders
/// bare (no quotes — the authored value, matching how dbt docs show the
/// list), any other scalar via its JSON rendering.
fn scalar_token(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Build the full column-metadata map for `model` (cute-dbt#165):
/// authored descriptions from the model node's `columns` map, plus the
/// **column-scoped** generic tests attached to it (manifest `test` nodes
/// with `attached_node == model` AND `column_name` set — dbt's
/// column-`tests:` shape). Model-level tests that merely take a column
/// argument carry `column_name: null` and are deliberately out of v1
/// scope. Only columns with a description and/or ≥1 test appear.
///
/// Deterministic: descriptions iterate a `BTreeMap`; tests are sorted by
/// (column, name, values, detail, test node id) before insertion —
/// `Manifest::nodes` is a `HashMap` with no inherent order.
#[must_use]
pub fn column_meta_for_model(
    current: &Manifest,
    model: &Node,
) -> BTreeMap<String, ColumnMetaPayload> {
    let mut meta: BTreeMap<String, ColumnMetaPayload> = BTreeMap::new();
    for (column, description) in model.column_descriptions() {
        meta.entry(column.clone()).or_default().description = Some(description.clone());
    }
    for (column, test) in attached_column_tests(current, model.id()) {
        meta.entry(column.to_owned()).or_default().tests.push(test);
    }
    meta
}

/// The column-scoped data tests attached to the node `attached` —
/// shared by the model/seed arm ([`column_meta_for_model`]) and the
/// source arm ([`column_meta_for_source`]) of the cute-dbt#165/#235
/// column-header tooltips. Deterministic: sorted by (column, name,
/// values, detail, test node id) — `Manifest::nodes` is a `HashMap`
/// with no inherent order.
fn attached_column_tests<'m>(
    current: &'m Manifest,
    attached: &NodeId,
) -> Vec<(&'m str, ColumnTestPayload)> {
    let mut tests: Vec<(&str, ColumnTestPayload, &str)> = current
        .nodes()
        .iter()
        .filter_map(|(id, node)| {
            if node.resource_type() != "test" || node.attached_node() != Some(attached) {
                return None;
            }
            let column = node.column_name()?;
            let tm = node.test_metadata()?;
            Some((column, column_test_payload(tm), id.as_str()))
        })
        .collect();
    tests.sort_by(|a, b| {
        (a.0, &a.1.name, &a.1.values, &a.1.detail, a.2).cmp(&(
            b.0,
            &b.1.name,
            &b.1.values,
            &b.1.detail,
            b.2,
        ))
    });
    tests
        .into_iter()
        .map(|(column, test, _)| (column, test))
        .collect()
}

/// A SOURCE's column-metadata map (cute-dbt#235) — the
/// [`column_meta_for_model`] twin for `source('a','b')` given inputs:
/// authored column descriptions from the ingested source `columns`
/// block, merged with column-scoped data tests whose `attached_node` is
/// the source. Only columns with a description and/or ≥1 test appear,
/// so a metadata-less source contributes nothing (honest degrade — the
/// JS renders no trigger, never an empty bubble).
fn column_meta_for_source(
    current: &Manifest,
    source: &SourceNode,
) -> BTreeMap<String, ColumnMetaPayload> {
    let mut meta: BTreeMap<String, ColumnMetaPayload> = BTreeMap::new();
    for (column, description) in source.column_descriptions() {
        meta.entry(column.clone()).or_default().description = Some(description.clone());
    }
    for (column, test) in attached_column_tests(current, source.id()) {
        meta.entry(column.to_owned()).or_default().tests.push(test);
    }
    meta
}

/// Resolve a unit-test GIVEN's `ref(...)` to its manifest node
/// (cute-dbt#235). Unlike [`resolve_target_model`] (a unit test's
/// `model:` target is always a model), a given's ref resolves over
/// dbt's full refable set — models, seeds, and snapshots (the committed
/// jaffle-shop fixture's `ref('raw_customers')` seed given is the real
/// wire shape; fusion validates given inputs as any ref/source/this,
/// `dbt-parser` `resolve_unit_tests.rs` @ `9977b6cb…`).
///
/// Matching is by [`Node::bare_name`] (cute-dbt#256, the #254 handoff
/// root fix): the ingested wire `name` when present — the only handle
/// that binds a VERSIONED model, whose id leaf segment is the `.vN`
/// suffix — with the pre-#256 leaf-segment fallback for synthetic
/// fixtures. Among matches, the node whose `version` equals
/// `latest_version` wins (dbt's unpinned-ref resolution rule for
/// versioned families); otherwise the lexicographically smallest node
/// id wins (`model.*` sorts before `seed.*`/`snapshot.*`) — the
/// unchanged determinism contract.
fn resolve_given_ref_node<'m>(current: &'m Manifest, ref_name: &str) -> Option<&'m Node> {
    const REFABLE: [&str; 3] = ["model", "seed", "snapshot"];
    let matches = current
        .nodes()
        .values()
        .filter(|node| REFABLE.contains(&node.resource_type()) && node.bare_name() == ref_name);
    matches.min_by(|a, b| {
        let a_latest = a.version().is_some() && a.version() == a.latest_version();
        let b_latest = b.version().is_some() && b.version() == b.latest_version();
        // A latest-version node sorts first; ties fall back to the
        // smallest-id rule.
        b_latest.cmp(&a_latest).then_with(|| a.id().cmp(b.id()))
    })
}

/// Filter a model's full column-metadata map down to the columns that
/// actually appear in one rendered fixture table. `None` table (sql /
/// opaque / unloaded-external fixture — no grid, no headers) ⇒ empty map
/// (the `skip_serializing_if` then omits the key entirely).
fn column_meta_for_table(
    meta: &BTreeMap<String, ColumnMetaPayload>,
    table: Option<&FixtureTable>,
) -> BTreeMap<String, ColumnMetaPayload> {
    let Some(table) = table else {
        return BTreeMap::new();
    };
    table
        .columns
        .iter()
        .filter_map(|column| meta.get(column).map(|m| (column.clone(), m.clone())))
        .collect()
}

/// Build one model's [`ManifestNodePayload`] (cute-dbt#200): the
/// authored description + tags (#200 ingestion), the #145 `materialized`
/// accessor, every DECLARED column (the [`Node::columns`] map — name
/// order, deterministic) decorated with its #165 description and
/// column-scoped tests, and the model-level test grouping. Mostly
/// re-plumbing of already-ingested data — the genuinely new computation
/// is the model-level grouping in [`model_tests_for_model`].
fn manifest_node_payload(current: &Manifest, model: &Node) -> ManifestNodePayload {
    let meta = column_meta_for_model(current, model);
    // Declared columns drive the list; a meta-only key (a column-scoped
    // test whose column the model does not declare — possible only on
    // hand-built manifests) is appended via the BTreeMap union so no
    // ingested test silently disappears.
    let mut columns: BTreeMap<&String, ManifestColumnPayload> = model
        .columns()
        .iter()
        .map(|(name, data_type)| {
            (
                name,
                ManifestColumnPayload {
                    name: name.clone(),
                    description: None,
                    column_type: data_type.clone(),
                    tests: Vec::new(),
                },
            )
        })
        .collect();
    for (name, m) in &meta {
        let entry = columns
            .entry(name)
            .or_insert_with(|| ManifestColumnPayload {
                name: name.clone(),
                ..ManifestColumnPayload::default()
            });
        entry.description.clone_from(&m.description);
        entry.tests.clone_from(&m.tests);
    }
    ManifestNodePayload {
        description: model.description().map(str::to_owned),
        materialized: model.config().materialized().map(str::to_owned),
        tags: model.tags().to_vec(),
        columns: columns.into_values().collect(),
        model_tests: model_tests_for_model(current, model),
    }
}

/// The MODEL-LEVEL data tests attached to `model` (cute-dbt#200):
/// ingested generic-test nodes with `attached_node == model` AND
/// `column_name == None` (dbt's model-`data_tests:` shape — a
/// column-scoped test carries `column_name` and belongs to the per-column
/// projection instead). Singular (SQL-file) tests carry no
/// `test_metadata` — and on real manifests no `attached_node` either —
/// so they are out of v1 scope, exactly like the #165 column path.
/// Mapped through [`column_test_payload`] and reduced to `name` +
/// `detail` ([`ModelTestPayload`]); sorted by (name, detail, test node
/// id) — `Manifest::nodes` is a `HashMap` with no inherent order.
fn model_tests_for_model(current: &Manifest, model: &Node) -> Vec<ModelTestPayload> {
    let mut tests: Vec<(ModelTestPayload, &str)> = current
        .nodes()
        .iter()
        .filter_map(|(id, node)| {
            if node.resource_type() != "test"
                || node.attached_node() != Some(model.id())
                || node.column_name().is_some()
            {
                return None;
            }
            let tm = node.test_metadata()?;
            let mapped = column_test_payload(tm);
            Some((
                ModelTestPayload {
                    name: mapped.name,
                    detail: mapped.detail,
                },
                id.as_str(),
            ))
        })
        .collect();
    tests.sort_by(|a, b| (&a.0.name, &a.0.detail, a.1).cmp(&(&b.0.name, &b.0.detail, b.1)));
    tests.into_iter().map(|(t, _)| t).collect()
}

/// Build the report-level `manifest_nodes` lookup (cute-dbt#200), keyed
/// by BARE model name. Scope is deliberately narrow: the in-scope models
/// plus every model referenced by a rendered test's `given.input`
/// `ref()` — never the whole project graph. A `this` given resolves to
/// the in-scope target model (already present); `source(...)` inputs and
/// unresolvable refs contribute nothing (manifest `sources` are not
/// model nodes — the pill renders without a hover card, the graceful JS
/// contract). All-empty entries are skipped so bare synthetic manifests
/// keep the `manifest_nodes` key off the wire entirely.
fn build_manifest_nodes(
    current: &Manifest,
    models_in_scope: &ModelInScopeSet,
    model_tests: &HashMap<NodeId, Vec<(&str, &UnitTest)>>,
) -> BTreeMap<String, ManifestNodePayload> {
    let mut referenced: BTreeMap<String, &Node> = BTreeMap::new();
    for model_id in models_in_scope.iter() {
        let Some(model) = current.node(model_id) else {
            continue;
        };
        referenced.insert(leaf_segment(model.id().as_str()).to_owned(), model);
        for (_, unit_test) in model_tests.get(model_id).into_iter().flatten() {
            for given in unit_test.given() {
                let Some(upstream) = parse_ref_name(given.input())
                    .and_then(|ref_name| resolve_target_model(current, &NodeId::new(ref_name)))
                else {
                    continue;
                };
                referenced.insert(leaf_segment(upstream.id().as_str()).to_owned(), upstream);
            }
        }
    }
    referenced
        .into_iter()
        .filter_map(|(name, node)| {
            let payload = manifest_node_payload(current, node);
            (payload != ManifestNodePayload::default()).then_some((name, payload))
        })
        .collect()
}

/// One `given[i]` entry from a unit test, lifted into payload shape.
///
/// `bound_to_node` ties the given to an import-CTE node id when a match
/// was found (the node-detail panel's "given · ref('…')" table). When
/// `None`, the design surfaces the "no fixture provided" empty-state
/// copy on the bound node.
#[derive(Debug, Clone, Serialize)]
pub struct GivenPayload {
    /// Verbatim `input:` value from the unit-test `given[i]` entry
    /// (typically `ref('…')` or `source('…','…')`).
    pub input: String,
    /// Import-CTE node id this given binds to, when the engine
    /// successfully matched [`Self::input`] to a node in the model's
    /// CTE graph. `None` triggers the "no fixture provided" empty-state
    /// copy on the bound node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_to_node: Option<String>,
    /// The Rust-computed Current-view [`FixtureTable`] — the authoritative
    /// tabulated cells (authored `display` + canonical `key` per cell),
    /// computed in the domain via `table_from_manifest_rows` (cute-dbt#138).
    /// `None` for a non-tabulatable fixture (sql/opaque, or external-`fixture:`
    /// rows not in the manifest); the JS then renders the sql code block or the
    /// external-fixture affordance from [`rows`](Self::rows) /
    /// [`format`](Self::format) / [`fixture`](Self::fixture). When `Some`, the
    /// template is a PURE renderer of this POD — it no longer parses
    /// csv/dict in JS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<FixtureTable>,
    /// Tabular fixture rows lifted verbatim from the unit-test
    /// `given[i].rows` field (kept as serde `Value`). Retained for the
    /// non-tabulatable fallback only: the sql code block, and external-fixture
    /// detection (`rows == null`). When [`table`](Self::table) is `Some` the
    /// renderer ignores this field (cute-dbt#138). `Value::Null` when this
    /// given's data lives in an external [`fixture`](Self::fixture) file.
    pub rows: Value,
    /// Fixture format hint (`csv`, `yaml`, `dict`, etc.) when the
    /// manifest specified one; absent for inline-rows fixtures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Name of the external fixture file this given's rows live in (dbt's
    /// `fixture:` key), when set. `None` (key omitted) for inline-rows
    /// givens. When present **with `rows == Value::Null`**, the data is not
    /// in the manifest at all: the JS surfaces a "data in external fixture
    /// file: `<name>`" affordance and falls back to the cute-dbt#96 YAML
    /// text view instead of rendering a silently-empty grid (cute-dbt#126).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixture: Option<String>,
    /// `true` when this given's `input` is the literal `this` — dbt's
    /// convention for mocking the model's own prior state on an incremental
    /// model (cute-dbt#145; fusion's own `input.as_str().eq("this")`
    /// discriminator). The template marks the given "prior model state".
    /// Serialized only when `true` (`skip_serializing_if =
    /// std::ops::Not::not`); a normal `ref(...)` / `source(...)` given omits
    /// the key.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_this: bool,
    /// Column-header metadata for this given's table (cute-dbt#165),
    /// keyed by column name — resolved against the model that OWNS the
    /// given's columns: the `ref(...)` input model (`this` resolves to
    /// the target model itself; `source(...)` inputs resolve to nothing —
    /// manifest `sources` are not ingested in v0.x). Only columns present
    /// in [`table`](Self::table) with a description and/or ≥1 column test
    /// appear; empty ⇒ the key is omitted and the template renders no
    /// affordance.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub column_meta: BTreeMap<String, ColumnMetaPayload>,
}

/// One unit test in the per-model payload.
#[derive(Debug, Clone, Serialize)]
pub struct TestPayload {
    /// Manifest unit-test id (`unit_test.<package>.<model>.<name>`) — the
    /// test selector's stable handle.
    pub id: String,
    /// User-facing test name (`UnitTest::name()`).
    pub name: String,
    /// `model:` reference verbatim from the manifest.
    pub target_model: String,
    /// `true` when this PR/diff **updated** this test (added or edited its
    /// definition); `false` when it is *context* — rendered only because
    /// its target model is in scope (cute-dbt#91). Always serialized (the
    /// report's JS foregrounds updated tests and toggles context on). The
    /// classifier rides on the existing in-scope selection — selection is
    /// unchanged; this is an additive label (ADR-3 / ADR-5).
    pub changed: bool,
    /// Optional human-readable test description from the manifest's
    /// `description:` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional `config.tags` list from the manifest (e.g. `["smoke",
    /// "nightly"]`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Optional `config.meta` blob from the manifest (kept as serde
    /// `Value` to accept any JSON shape teams put under `meta`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    /// Optional `original_file_path` from the manifest — the on-disk
    /// location the test was defined in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defined_in: Option<String>,
    /// Raw YAML slice for this `unit_test` as authored — populated by
    /// the `gather_authoring_yaml` run-loop step (cute-dbt#69) when
    /// the project root is resolvable and the source file is readable.
    /// `None` when no project root is configured, the source file is
    /// missing, or the test entry cannot be located inside the file
    /// (defensive: a manifest can carry a `name` the source no longer
    /// contains).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoring_yaml: Option<String>,
    /// Inline diff of this test's authored YAML block (cute-dbt#96
    /// concern 2) — present only when the diff edited this test's own
    /// block (PR-diff mode, block present + aligned + touched). `None`
    /// (key omitted) for context tests, baseline mode, and edits that
    /// fall outside the block, so the drawer falls back to the plain
    /// authored-YAML view. Mirrors `authoring_yaml`'s threading exactly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaml_diff: Option<BlockDiff>,
    /// Cell-level data-table diff of this test's `given`/`expect` fixture
    /// rows (cute-dbt#98) — present only when the PR diff edited this
    /// test's own YAML block AND at least one fixture table carried a real
    /// cell change (PR-diff mode, block present + aligned + touched,
    /// non-opaque rows, `has_real_change()`). `None` (key omitted) for
    /// context tests, baseline mode, sql/opaque fixtures, and
    /// format-only / pure-reorder edits, so the given/expect grids default
    /// to the plain "Current" data view. Mirrors `yaml_diff`'s threading
    /// exactly; the JS defaults the per-table Current↔Diff toggle to Diff
    /// when this is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_diff: Option<UnitTestDataDiff>,
    /// dbt incremental-mode flag, lifted from
    /// `overrides.macros.is_incremental` (cute-dbt#145). `Some(true)` ⇒ the
    /// test exercises the incremental branch — the template shows a
    /// "incremental branch" mode badge AND the expect-semantics tooltip
    /// (`expect` is the rows merged/inserted, not the final table);
    /// `Some(false)` ⇒ explicit full-refresh branch (mode badge, NO
    /// tooltip — there `expect` IS the final table); `None` (key omitted)
    /// ⇒ no override. The mode badge renders only when the enclosing model
    /// is incremental ([`ModelPayload::is_incremental`]); the tooltip rides
    /// the authoritative bool (`=== true`), never the `this`-given proxy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_incremental_mode: Option<bool>,
    /// The FULL dbt `overrides` blob (cute-dbt#200): group (`"macros"` /
    /// `"vars"` / `"env_vars"`) → name → **native** value (serde `Value`
    /// passthrough — `true` / `7` / `0.05` stay bool/number on the wire,
    /// the cute-dbt#197 founder decision; never stringified). Drives the
    /// `overrides · N` badge + hover tooltip (cute-dbt#202; handoff
    /// README §2.6). `None` (key omitted — pre-#200 payloads stay
    /// byte-stable) when the test declares no effective override; the
    /// adapter already dropped null/empty groups. ADDITIVE context next
    /// to the lifted [`Self::is_incremental_mode`] flag (#145) and the
    /// #125 YAML-slice text diffs — both stay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<UnitTestOverrides>,
    /// Ordered list of fixture inputs for the test (`given[…]`).
    pub given: Vec<GivenPayload>,
    /// Expected result block (`expect`).
    pub expected: ExpectedPayload,
}

/// `expect` block lifted into payload shape.
#[derive(Debug, Clone, Serialize)]
pub struct ExpectedPayload {
    /// The Rust-computed Current-view [`FixtureTable`] for the `expect`
    /// fixture (authored `display` + canonical `key` per cell, cute-dbt#138).
    /// `None` for a non-tabulatable fixture (sql/opaque or external);
    /// otherwise the template renders this POD directly. Same contract as
    /// [`GivenPayload::table`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<FixtureTable>,
    /// Expected tabular rows lifted verbatim from the unit-test
    /// `expect.rows` field (serde `Value`). Retained for the non-tabulatable
    /// fallback only (sql code block; external-fixture detection). When
    /// [`table`](Self::table) is `Some` the renderer ignores this field
    /// (cute-dbt#138). `Value::Null` when the data lives in an external
    /// [`fixture`](Self::fixture) file.
    pub rows: Value,
    /// Expected-block format hint (`csv`, `yaml`, `dict`, etc.) when
    /// the manifest specified one; absent for inline-rows fixtures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Name of the external fixture file the `expect` rows live in (dbt's
    /// `fixture:` key), when set. `None` (key omitted) for inline-rows
    /// expects. Same external-fixture affordance contract as
    /// [`GivenPayload::fixture`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixture: Option<String>,
    /// Column-header metadata for the expect table (cute-dbt#165), keyed
    /// by column name — resolved against the TARGET model (the expect
    /// table's columns are the model's output columns). Same emission
    /// contract as [`GivenPayload::column_meta`].
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub column_meta: BTreeMap<String, ColumnMetaPayload>,
}

/// The full JSON blob the askama template emits into the
/// `<script type="application/json" id="cute-dbt-data">` element.
#[derive(Debug, Clone, Serialize)]
pub struct ReportPayload {
    /// Human-readable baseline reference (filename or supplied label).
    pub baseline: String,
    /// One entry per model in `models_in_scope` (deterministic
    /// `BTreeSet` ordering inherited from the comparator).
    pub models: Vec<ModelPayload>,
    /// Report-level model-context lookup (cute-dbt#200), keyed by BARE
    /// model name: the in-scope models plus every model referenced by a
    /// rendered test's `given.input` `ref()` — NEVER the whole project
    /// graph. `source(...)` inputs and unresolvable refs contribute
    /// nothing; an absent entry is the graceful no-hover (the JS
    /// contract). Omitted from JSON when empty (all-empty entries are
    /// skipped too) so bare synthetic payloads stay byte-stable.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub manifest_nodes: BTreeMap<String, ManifestNodePayload>,
    /// Spec catalog for every check that appears in any model's
    /// `findings` (cute-dbt#170), keyed by dotted check id — the
    /// rationale drawer, tier vocabulary, and book link render from
    /// this. Omitted from JSON when no finding fired anywhere, so
    /// findings-free payloads (and the jaffle-shop golden) stay
    /// byte-stable.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub check_specs: BTreeMap<String, CheckSpecPayload>,
    /// The parsed working-tree `dbt_project.yml` (cute-dbt#266) —
    /// **standing metadata**, present on both scope arms whenever the
    /// file is readable + parseable under the resolved project root.
    /// Future consumers (explorer panes, provenance chips) read it from
    /// here; nothing in the current report chrome renders it directly.
    /// Omitted when absent so pre-#266 payloads stay byte-stable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_definition: Option<ProjectDefinition>,
    /// The "Project definition changed" panel content (cute-dbt#266) —
    /// present exactly when `dbt_project.yml` is in the PR diff. The
    /// panel itself is server-rendered (the template's `project_panel`
    /// view); the payload carries the structured facts for downstream
    /// consumers + the BDD payload assertions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_change_panel: Option<ProjectChangePanel>,
    /// The PR-review governance facts (cute-dbt#260) — group/owner chips
    /// (Slice 0), with the exposure / contract / enforcement / lifecycle
    /// surfaces added additively in later slices. Gated behind
    /// [`Experiment::Governance`](crate::domain::Experiment::Governance):
    /// the cli layer passes the empty default when the experiment is off.
    /// Omitted from JSON when empty so the non-experimental
    /// (`experimental: ""`) goldens stay byte-identical.
    #[serde(skip_serializing_if = "GovernanceFacts::is_empty")]
    pub governance: GovernanceFacts,
    /// The macro perspective lens facts (cute-dbt#265, Slice B) — the
    /// changed root-project macros, each with its body diff, its
    /// blast-radius impacted-model directory tree, and the count. Gated
    /// behind [`Experiment::MacroLens`](crate::domain::Experiment::MacroLens):
    /// the cli layer passes `None` when the experiment is off, so the key
    /// is omitted from the JSON and the `{%- if macro_lens %}` template
    /// section emits zero bytes — keeping the non-macro goldens
    /// byte-identical. The same struct rides the JSON payload (downstream
    /// consumers + headless assertions) and is server-rendered into the
    /// section.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macro_lens: Option<MacroLensPayload>,
    /// The source-PR reference (cute-dbt#346) — the PR number, title, and
    /// URL the `--pr-diff` change-context banner links to. `Some` only on
    /// the PR-diff arm when a usable PR context was supplied at generation
    /// time (`[pr]` config / `--pr-*` flags / `review`-derived); `None` ⇒
    /// the key is omitted from JSON and the banner renders link-free,
    /// keeping the no-PR-context goldens byte-identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_ref: Option<PrRefPayload>,
    /// The in-scope seed cards' RENDER VIEW (cute-dbt#350) — one
    /// [`SeedSectionCard`] per seed the diff modified, built from the raw
    /// [`SeedCard`]s the CLI gathered (`build_seed_section`) plus the
    /// resolved row cap. Each carries the seed's identity, project-relative
    /// path, direct downstream-model lineage, config-display strings, the
    /// CAPPED current table (with its true pre-cap row total for the honest
    /// "showing N of M rows" label), and — on the pr-diff arm — the FULL
    /// (never capped) old→new cell-diff. A seed whose data could not be read
    /// carries `table: None` (the labeled "data unavailable" state, the
    /// cute-dbt#126 lesson — never a silent blank grid). Gated behind the
    /// `seeds` experiment: the cli passes an EMPTY raw vec when the
    /// experiment is off, so this view is empty ⇒ omitted from JSON
    /// (`#[serde(skip_serializing_if)]`) ⇒ the "Data tables" section renders
    /// zero DOM (`DATA.seed_cards` absent) and every seed-free golden stays
    /// byte-identical (the `macro_lens` / governance precedent).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub seed_cards: Vec<SeedSectionCard>,
    /// The PR-scope lineage mini-DAG render view (cute-dbt#404, epic #352) —
    /// the focused cross-model subgraph (modified ∪ connectors ∪ deleted) the
    /// report puts at the top. Gated behind
    /// [`Experiment::PrScopeMiniDag`](crate::domain::Experiment::PrScopeMiniDag):
    /// the cli passes `None` when the experiment is off, so the key is omitted
    /// from JSON and the `{% match pr_dag %}` section emits zero bytes —
    /// keeping every default golden byte-identical (the `macro_lens` /
    /// governance / seeds precedent). When `Some`, the same payload rides the
    /// JSON (the JS `renderPrDag` reads `DATA.pr_dag`) AND drives the
    /// server-rendered descriptor line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_dag: Option<PrDagPayload>,
    /// The REMOVED model paths (cute-dbt#416) — the PR-deleted model files
    /// that name no current node (node-less, so they cannot be
    /// [`ModelPayload`]s / model-dropdown entries). The Models lens renders
    /// these as a summary chip/count + a short path list, NOT as dropdown
    /// options. Sorted (the domain `ScopeSelection::removed_models` is
    /// pre-sorted). Empty ⇒ omitted from JSON (the baseline arm + every
    /// addition-free PR), keeping non-removal goldens byte-identical (the
    /// `seed_cards` precedent).
    ///
    /// **Design note (cute-dbt#360-revisitable):** chip-only placement is a
    /// sensible default — a removed model has no current detail to show. The
    /// final REMOVED presentation (and whether it ever enters the dropdown)
    /// is a #360 design decision.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed_models: Vec<String>,
    /// The PR review-comments render view (cute-dbt#419–#422, epic #353) —
    /// the ingested GitHub review threads, anchored onto the rendered diff
    /// (the shipped [`anchor_comment_thread`](crate::domain::anchor_comment_thread),
    /// never re-anchored) and grouped per model. Gated behind
    /// [`Experiment::PrComments`](crate::domain::Experiment::PrComments): the
    /// cli passes `None` when the experiment is off, when there is no PR
    /// context, or when no comments were ingested — so the key is omitted
    /// from JSON, the static count container stays empty (the JS never fills
    /// it), and every default golden stays byte-identical (the `pr_dag` /
    /// `seed_cards` precedent). When `Some`, the JS (`renderPrComments`)
    /// reads `DATA.pr_comments` and (1) injects each thread inline at its
    /// anchored Model-SQL diff line, (2) fills the top-of-report total-count
    /// navigation button, and (3) sets each model's per-model count tooltip.
    /// Inlined at gen-time, view-time zero-egress (any navigate is in-page JS).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_comments: Option<CommentsView>,
}

/// The PR-scope lineage mini-DAG render view (cute-dbt#404) — the domain
/// [`PrDagGraph`] plus the render-time facts the top-of-report section needs:
/// the per-state counts for the one-line descriptor and the size-bound
/// collapse decision.
///
/// The graph itself ([`PrDagGraph`]) is the pure-domain topology + per-node
/// line counts; this render POD wraps it with the descriptor counts (so the
/// server-rendered "N modified · M connectors · K deleted" line is composed
/// in Rust, the house rule) and the `collapsed` flag (the size-bound: when the
/// node count exceeds the cap the inline Mermaid render is replaced by a
/// summary line, but the full graph still rides the JSON for any downstream
/// consumer). `Serialize`-only — an additive render payload, never round-tripped.
#[derive(Debug, Clone, Serialize)]
pub struct PrDagPayload {
    /// The computed mini-DAG: nodes (modified ∪ connectors ∪ deleted, with
    /// per-node line counts) + induced model→model edges, both deterministic.
    pub graph: PrDagGraph,
    /// Count of genuinely modified models (new + modified, not connectors,
    /// not halo, not deleted) — the emphasized tier in the descriptor.
    pub modified_count: usize,
    /// Count of connector models (the quiet between-modified carriers).
    pub connector_count: usize,
    /// Count of 1-hop context **halo** models (the dimmed neighbors of a
    /// disconnected modified model, cute-dbt#428). Counted separately from
    /// `modified_count` so the descriptor stays truthful ("1 modified · 2
    /// context" rather than a misleading "3 modified"). Slice B (#429) owns
    /// the full engine-aware render; this count keeps the existing descriptor
    /// honest the moment the domain halo lands.
    pub halo_count: usize,
    /// Count of deleted models (the ghosts).
    pub deleted_count: usize,
    /// `true` when the node count exceeded the size-bound cap
    /// ([`DEFAULT_PR_DAG_NODE_CAP`](crate::domain::DEFAULT_PR_DAG_NODE_CAP)):
    /// the inline Mermaid graph is suppressed in favor of the summary line,
    /// while the graph data still rides the JSON payload.
    pub collapsed: bool,
    /// The **per-axis** pre-computed mini-DAG subgraphs (cute-dbt#430 — the
    /// #414 segmented-filter-reactive mini-DAG), keyed by the change-axis
    /// token (`"body"` / `"config"` / `"unit_test"`). Each entry is the
    /// mini-DAG **recomputed over the subset of modified models whose
    /// `axes.<token>` fired** — connectors + halo re-derived over that smaller
    /// seed set in Rust (the compute-in-Rust→toggle-in-JS architecture: the JS
    /// only shows/hides these pre-emitted sets, never re-deriving the topology).
    /// The `"all"` view IS the top-level [`graph`](Self::graph) + counts above,
    /// so it is intentionally absent from the map (the JS reads the top-level
    /// payload for `all`).
    ///
    /// Populated **only on the `--pr-diff` arm** (the one arm that carries the
    /// per-model [`ChangeAxes`] attribution); empty on the baseline arm, so the
    /// `#[serde(skip_serializing_if)]` keeps every baseline + pre-#430 golden
    /// byte-identical (the `axes` / `seed_tables` precedent).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub by_axis: BTreeMap<String, PrDagAxisView>,
}

/// The descriptor counts + collapse flag for one PR-scope mini-DAG view — the
/// shared shape of the top-level [`PrDagPayload`] *and* each per-axis
/// [`PrDagAxisView`] (cute-dbt#430), derived once from a graph's own nodes so a
/// count can never drift from what renders.
#[derive(Debug, Clone, Copy, Default)]
// The `_count` postfix is intentional — these fields are copied verbatim onto
// the public `PrDagPayload` / `PrDagAxisView` fields of the same names (the
// descriptor's "N modified · M connectors" tiers), so renaming them here would
// only obscure the 1:1 mapping.
#[allow(clippy::struct_field_names)]
struct PrDagTierCounts {
    modified_count: usize,
    connector_count: usize,
    halo_count: usize,
    deleted_count: usize,
}

impl PrDagTierCounts {
    /// Classify every node of `graph` into its descriptor tier. `is_connector`
    /// is the first branch (it wins over any placeholder state), then `is_halo`
    /// (the dimmed-context tier), then `Deleted`, else the emphasized
    /// modified tier (New + Modified collapse together).
    fn from_graph(graph: &PrDagGraph) -> Self {
        let mut counts = Self::default();
        for node in &graph.nodes {
            if node.is_connector {
                counts.connector_count += 1;
            } else if node.is_halo {
                counts.halo_count += 1;
            } else if node.state == crate::domain::PrDagState::Deleted {
                counts.deleted_count += 1;
            } else {
                counts.modified_count += 1;
            }
        }
        counts
    }
}

/// One per-axis pre-computed mini-DAG view (cute-dbt#430) — the recomputed
/// subgraph for the models whose selected change-axis fired, plus its own
/// descriptor counts and collapse flag. The JS swaps the rendered mini-DAG to
/// this view when the #414 axis filter selects the matching axis; the topology
/// (connectors + halo over the subset) is **already computed in Rust**, so the
/// JS only shows/hides — never re-derives.
#[derive(Debug, Clone, Serialize)]
pub struct PrDagAxisView {
    /// The recomputed mini-DAG over the axis-filtered modified subset.
    pub graph: PrDagGraph,
    /// Count of genuinely modified models in this axis subset.
    pub modified_count: usize,
    /// Count of connector models recomputed over the subset.
    pub connector_count: usize,
    /// Count of 1-hop context halo models recomputed over the subset.
    pub halo_count: usize,
    /// Count of deleted models in this view (always `0` on the pr-diff arm,
    /// which surfaces no deletion ghosts — kept for descriptor symmetry).
    pub deleted_count: usize,
    /// `true` when this subset's node count exceeded the size-bound cap.
    pub collapsed: bool,
}

impl PrDagAxisView {
    /// Build a per-axis view from a recomputed subset graph + the size-bound
    /// cap, deriving its descriptor counts from the graph's own nodes.
    #[must_use]
    pub fn from_graph(graph: PrDagGraph, node_cap: usize) -> Self {
        let counts = PrDagTierCounts::from_graph(&graph);
        let collapsed = graph.nodes.len() > node_cap;
        Self {
            graph,
            modified_count: counts.modified_count,
            connector_count: counts.connector_count,
            halo_count: counts.halo_count,
            deleted_count: counts.deleted_count,
            collapsed,
        }
    }
}

impl PrDagPayload {
    /// Build the render view from a computed [`PrDagGraph`] and the size-bound
    /// node cap. Derives the per-state descriptor counts from the graph's own
    /// nodes (so the counts can never drift from what renders) and sets
    /// `collapsed` when the node count exceeds `node_cap`. The per-axis
    /// `by_axis` map is left empty (the baseline-arm / no-axis-attribution
    /// shape, cute-dbt#430); [`from_graph_with_axes`](Self::from_graph_with_axes)
    /// is the pr-diff-arm constructor that fills it.
    #[must_use]
    pub fn from_graph(graph: PrDagGraph, node_cap: usize) -> Self {
        Self::from_graph_with_axes(graph, BTreeMap::new(), node_cap)
    }

    /// Build the render view with the per-axis pre-computed subgraphs
    /// (cute-dbt#430). `by_axis_graphs` maps each change-axis token (`"body"` /
    /// `"config"` / `"unit_test"`) to the mini-DAG **recomputed over the
    /// modified models whose `axes.<token>` fired** (the cli derives + computes
    /// these); this constructor wraps each in a [`PrDagAxisView`] with its own
    /// descriptor counts. The `"all"` view is the top-level graph + counts —
    /// callers pass the full modified set as `graph` and never key it under
    /// `"all"` in the map.
    #[must_use]
    pub fn from_graph_with_axes(
        graph: PrDagGraph,
        by_axis_graphs: BTreeMap<String, PrDagGraph>,
        node_cap: usize,
    ) -> Self {
        let counts = PrDagTierCounts::from_graph(&graph);
        let collapsed = graph.nodes.len() > node_cap;
        let by_axis = by_axis_graphs
            .into_iter()
            .map(|(axis, g)| (axis, PrDagAxisView::from_graph(g, node_cap)))
            .collect();
        Self {
            graph,
            modified_count: counts.modified_count,
            connector_count: counts.connector_count,
            halo_count: counts.halo_count,
            deleted_count: counts.deleted_count,
            collapsed,
            by_axis,
        }
    }
}

/// One seed's render view for the "Data tables" section (cute-dbt#350) — the
/// wire shape the report JS consumes (`DATA.seed_cards`).
///
/// Built by `build_seed_section` from a raw [`SeedCard`] plus the resolved
/// row cap. It differs from the gathered [`SeedCard`] in two render-time
/// ways: the current `table` is **truncated to the cap** (with the true
/// pre-cap total carried in [`total_rows`](Self::total_rows) so the JS can
/// label "showing N of M rows" honestly), while the cell-`diff` is carried
/// **in full** — a diff is intrinsically bounded by the edit size and
/// capping it would hide the very change under review. A seed whose data
/// could not be read carries `table: None` AND `diff: None` — the JS renders
/// the labeled "data unavailable" state, never a silent empty grid (the
/// cute-dbt#126 lesson).
#[derive(Debug, Clone, Serialize)]
pub struct SeedSectionCard {
    /// The seed's full manifest node id (`seed.<pkg>.<name>`).
    pub id: String,
    /// The seed's authored bare name — the handle a reviewer recognizes.
    pub name: String,
    /// The seed source file, project-relative (`seeds/<name>.csv`). `None`
    /// for a synthetic node that omits the path. Safe to inline (no
    /// home-path leak — unlike the node's absolute `root_path`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_file_path: Option<String>,
    /// The bare names of the DIRECT downstream models that `ref()` this
    /// seed (the immediate blast radius — direct consumers, not the
    /// transitive closure). Sorted; empty for an unreferenced seed.
    pub feeds_models: Vec<String>,
    /// The seed's `delimiter` config display string, when authored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delimiter: Option<String>,
    /// The seed's `quote_columns` config display string, when authored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote_columns: Option<String>,
    /// The seed's `column_types` config display string, when authored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_types: Option<String>,
    /// The seed's CURRENT data table, **capped** to the resolved row cap.
    /// `None` ⇒ the data could not be read (no `--project-root` / unreadable
    /// file) ⇒ the labeled "data unavailable" state. When `Some`, holds at
    /// most [`shown_rows`](Self::shown_rows) rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<FixtureTable>,
    /// The TRUE number of rows in the seed's current table BEFORE the cap —
    /// the `M` in "showing N of M rows". `0` when [`table`](Self::table) is
    /// `None` (no data read).
    pub total_rows: usize,
    /// The number of rows actually carried in [`table`](Self::table) after
    /// the cap — the `N` in "showing N of M rows". Equals
    /// [`total_rows`](Self::total_rows) when under the cap. `0` when
    /// `table` is `None`.
    pub shown_rows: usize,
    /// `true` when [`total_rows`](Self::total_rows) exceeds
    /// [`shown_rows`](Self::shown_rows) — the cap actually truncated rows,
    /// so the JS shows the "showing N of M rows" note. Precomputed in Rust
    /// so the (untrusted-name-free) JS only renders.
    pub capped: bool,
    /// The old→new cell-diff, carried in FULL (never capped). `Some` only on
    /// the pr-diff arm when the seed CSV's hunks reconstructed a diff; `None`
    /// on the baseline arm and when the data could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<FixtureTableDiff>,
}

/// Build the "Data tables" seed render views (cute-dbt#350) from the raw
/// gathered [`SeedCard`]s plus the resolved row `cap`.
///
/// Pure transform: per card, truncate the current table to `cap` rows
/// (recording the true pre-cap total for the honest "showing N of M" label),
/// pass the cell-diff through untouched (a diff is never capped — truncating
/// it would hide the change under review), and carry the degrade state
/// (`table: None`) verbatim. The cap is applied here, render-side, from the
/// `--config`-resolved value the cli threads in (default
/// [`DEFAULT_SEED_ROW_CAP`]).
fn build_seed_section(cards: &[SeedCard], cap: usize) -> Vec<SeedSectionCard> {
    cards
        .iter()
        .map(|card| seed_section_card(card, cap))
        .collect()
}

/// Cap a seed's current table to `cap` rows, returning the (possibly
/// truncated) table plus the honest `(total, shown, capped)` row-count
/// metadata.
///
/// Shared by the report's [`seed_section_card`] (cute-dbt#350) and the
/// explorer's seed-table side-map ([`crate::adapters::explore::seed_tables_by_id`],
/// cute-dbt#398) so the two surfaces apply the row cap identically. A `None`
/// input table (data could not be read) yields `(None, 0, 0, false)` — the
/// labeled "data unavailable" degrade (the cute-dbt#126 lesson).
pub(crate) fn cap_seed_table(
    table: Option<&FixtureTable>,
    cap: usize,
) -> (Option<FixtureTable>, usize, usize, bool) {
    match table {
        Some(t) => {
            let total = t.rows.len();
            let shown = total.min(cap);
            // Only clone-and-truncate when the cap actually bites; the common
            // under-cap path clones the table whole (cheap relative to the
            // render) so the wire shape is identical to the raw table.
            let capped_table = if shown < total {
                FixtureTable::new(t.columns.clone(), t.rows[..shown].to_vec())
            } else {
                t.clone()
            };
            (Some(capped_table), total, shown, shown < total)
        }
        None => (None, 0, 0, false),
    }
}

/// Transform one raw [`SeedCard`] into its capped render view.
fn seed_section_card(card: &SeedCard, cap: usize) -> SeedSectionCard {
    let (table, total_rows, shown_rows, capped) = cap_seed_table(card.table.as_ref(), cap);
    SeedSectionCard {
        id: card.id.as_str().to_owned(),
        name: card.name.clone(),
        original_file_path: card.original_file_path.clone(),
        feeds_models: card.feeds_models.clone(),
        delimiter: card.delimiter.clone(),
        quote_columns: card.quote_columns.clone(),
        column_types: card.column_types.clone(),
        table,
        total_rows,
        shown_rows,
        capped,
        diff: card.diff.clone(),
    }
}

/// The serialized source-PR reference (cute-dbt#346) — the JSON twin of
/// the server-rendered banner link. Mirrors the domain [`PrRef`] POD
/// (the renderer owns the wire shape; the domain owns the resolution).
#[derive(Debug, Clone, Serialize)]
pub struct PrRefPayload {
    /// The PR number (`PR #<n>`).
    pub number: u64,
    /// The PR title — escaped by askama in the DOM; carried verbatim here.
    pub title: String,
    /// The GitHub URL — the `<a href>` navigation target.
    pub url: String,
}

impl From<&PrRef> for PrRefPayload {
    fn from(pr: &PrRef) -> Self {
        Self {
            number: pr.number,
            title: pr.title.clone(),
            url: pr.url.clone(),
        }
    }
}

/// The macro perspective lens (cute-dbt#265, Slice B) — the "macro changed"
/// section facts.
///
/// Present (`Some`) exactly when the [`Experiment::MacroLens`](crate::domain::Experiment::MacroLens)
/// experiment is on AND at least one root-project macro changed in the PR.
/// Carries one [`ChangedMacroView`] per changed macro plus the per-arm
/// [`fidelity`](Self::fidelity) chip. Server-rendered into the template +
/// serialized into the JSON payload (the [`GovernanceFacts`] both-surfaces
/// precedent).
#[derive(Debug, Clone, Serialize)]
pub struct MacroLensPayload {
    /// Each changed root-project macro, in deterministic id order.
    pub macros: Vec<ChangedMacroView>,
    /// The fidelity of the change signal for THIS report's scope arm:
    /// `"exact"` on the `--baseline-manifest` arm (a direct `macro_sql`
    /// body comparison — fusion's `check_modified_macros` semantics) or
    /// `"heuristic"` on the `--pr-diff` arm (path-primary + name-fallback
    /// resolution against the diff). The chip states this plainly so the
    /// reviewer reads the section's confidence honestly (critique S2 — no
    /// `state:` borrowing, no false certainty).
    pub fidelity: &'static str,
}

/// One changed root-project macro in the [`MacroLensPayload`].
#[derive(Debug, Clone, Serialize)]
pub struct ChangedMacroView {
    /// The macro's bare name (the `X` of `{% macro X(...) %}`), or the
    /// macro `unique_id` leaf when no identity name is recorded.
    pub name: String,
    /// The macro's owning package — always the root project here (the
    /// blast radius and changed-macro detection both filter to it), shown
    /// for parity with the governance chips.
    pub package: String,
    /// The macro's declaring file, project-relative (e.g.
    /// `macros/data_quality/quarantine_filter.sql`). Empty when the
    /// manifest carries no `original_file_path` for this macro (the rare
    /// fusion null-fill).
    pub path: String,
    /// The reconstructed inline body diff (cute-dbt#111
    /// [`reconstruct_macro_sql_diff`]),
    /// present only on the `--pr-diff` arm when this macro's file was
    /// touched + aligned + substantively changed. `None` ⇒ the section
    /// shows the plain current body (`body_lines`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<BlockDiff>,
    /// The macro's CURRENT body as plain context lines — the fallback the
    /// section renders when [`diff`](Self::diff) is `None` (baseline arm,
    /// or a pr-diff macro whose body the diff did not substantively touch).
    /// Never empty when the macro has a body.
    pub body_lines: Vec<DiffLine>,
    /// The number of root-project models impacted by this macro change —
    /// the [`macro_blast_radius`] cardinality. Stated as a count first
    /// (critique S3 — the lightweight
    /// surface is the count + tree, never inline bodies in Slice B).
    pub impacted_count: usize,
    /// The impacted models as a flattened collapsible directory tree
    /// (founder D3), grouped by `original_file_path` directory structure.
    /// Pre-order traversal with explicit [`depth`](MacroTreeRow::depth) so
    /// the askama template renders the nesting without recursion. Empty
    /// when the blast radius is empty (a materialization macro, or an
    /// edit reaching no root-project model — the section states the
    /// honest zero).
    pub tree: Vec<MacroTreeRow>,
    /// The impacted models as a flat, per-model detail list (cute-dbt#265
    /// Slice C, founder D4) — the model-selector's option set + the
    /// server-rendered inline SQL and first-order call-site snippets each
    /// option reveals. In the same id order as the blast radius
    /// ([`BTreeSet`]). Empty when the blast radius is empty (the section
    /// shows only the honest zero, no selector). Distinct from
    /// [`tree`](Self::tree): the tree is the always-full lightweight
    /// directory view (critique S3), this is the heavy per-model surface
    /// the selector drives.
    ///
    /// **Slice D cap (founder D5):** the selector lists every entry (the
    /// list is cheap), but only the first
    /// [`inlined_count`](Self::inlined_count) entries (in id order) carry a
    /// server-rendered inline SQL + call-site panel
    /// ([`ImpactedModelView::inline_body`] `== true`). Past the cap, an
    /// entry's panel shows a "body not inlined" affordance (name + path
    /// only), bounding a widely-used macro's report size — the report is a
    /// single frozen file.
    pub impacted_models: Vec<ImpactedModelView>,
    /// How many of the [`impacted_models`](Self::impacted_models) carry a
    /// server-rendered inline body (`min(cap, impacted_count)`) — the "N"
    /// of the "showing N of M bodies" copy (cute-dbt#265 Slice D, founder
    /// D5). Equals [`impacted_count`](Self::impacted_count) when the cap is
    /// not exceeded (then the "showing N of M" affordance is omitted).
    pub inlined_count: usize,
    /// The macro-scoped lineage DAG (cute-dbt#431, epic #427) — this macro's
    /// impacted root-project models
    /// ([`MacroRole::User`](crate::adapters::explore::MacroRole::User)) + their
    /// `ref()`-downstream closure
    /// ([`MacroRole::Downstream`](crate::adapters::explore::MacroRole::Downstream)),
    /// role-dimmed,
    /// as a slim engine-aware DAG the Macros tab renders. Built by projecting
    /// the SAME [`build_macro_lineage_payload`](crate::adapters::explore::build_macro_lineage_payload)
    /// the explore macro page consumes (payload reuse, not topology
    /// re-derivation), narrowed to the `{id,name,role}` + edges the report
    /// tab-DAG renderer needs (the explore-only `cte_dags` / `project_pane` /
    /// path / badge fields stay out of the report payload). Empty when the
    /// blast radius is empty (a materialization macro, or an edit reaching no
    /// root-project model) — the tab then shows the honest "no impacted
    /// models" copy, never a DAG canvas. **Report-page Cytoscape contract:
    /// the first-party preset layout (`cyto-dag.js`), NEVER `cytoscape-dagre`
    /// (dagre is the explore page's layout only — AGENTS.md).**
    pub macro_dag: MacroDagPayload,
}

/// The slim macro-scoped lineage DAG one [`ChangedMacroView`] carries
/// (cute-dbt#431) — `{nodes,edges}` in the engine-aware report tab-DAG shape,
/// projected from the explore [`LineagePayload`](crate::adapters::explore::LineagePayload).
///
/// Deliberately NOT the full `LineagePayload`: the report only needs each
/// node's id/name/role + the dependency edges to draw the role-dimmed DAG, so
/// the explore-only surfaces (per-model CTE DAGs, the project pane, file paths,
/// the pre-formatted test badge) are dropped — keeping the report payload lean
/// and the two surfaces' shapes independent. `Serialize`-only render payload.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MacroDagPayload {
    /// The role-stamped DAG vertices (impacted models + downstream closure),
    /// in deterministic full-id order. Empty when the blast radius is empty.
    pub nodes: Vec<MacroDagNode>,
    /// Forward dependency edges between entries of `nodes`, ordered.
    pub edges: Vec<MacroDagEdge>,
}

/// One vertex of a [`MacroDagPayload`] (cute-dbt#431) — id, render label,
/// role, and the fail-open `not_compiled` flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MacroDagNode {
    /// Full manifest node id — the tab-DAG element id + the
    /// click→`#model-select` selection key (parity with the mini-DAG node).
    pub id: String,
    /// Rendered label — the bare model name (canvas-text / Mermaid label).
    pub name: String,
    /// The macro-DAG role: `"user"` (an impacted, macro-calling model —
    /// emphasized) or `"downstream"` (a `ref()`-downstream context node —
    /// dimmed). The slim string form of
    /// [`MacroRole`](crate::adapters::explore::MacroRole), composed in Rust so
    /// the JS stays a pure renderer.
    pub role: &'static str,
    /// The fail-open "not compiled" flag (cute-dbt#100) — rendered as a
    /// dashed node, never raised.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub not_compiled: bool,
}

/// One directed edge of a [`MacroDagPayload`] (producer → consumer), both
/// endpoints in the node set (cute-dbt#431).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MacroDagEdge {
    /// Full node id of the upstream (depended-on) model.
    pub from: String,
    /// Full node id of the downstream (depending) model.
    pub to: String,
}

/// Project the explore
/// [`build_macro_lineage_payload`](crate::adapters::explore::build_macro_lineage_payload)
/// role-stamped subgraph into the slim [`MacroDagPayload`] the report's
/// Macros-tab DAG renders (cute-dbt#431).
///
/// Reuses the explore lineage builder verbatim (the payload-reuse seam — the
/// macro focus set, the role classification, and the edge induction are all
/// shared), then narrows each node to `{id,name,role,not_compiled}` and each
/// `(from_index,to_index)` edge to a `{from,to}` id pair. An empty focus set
/// (a materialization macro / no root-project caller) yields an empty DAG, so
/// the tab shows the honest no-models copy.
fn build_macro_dag(current: &Manifest, macro_id: &str) -> MacroDagPayload {
    let focus = crate::domain::macro_focus_set(current, macro_id);
    let lineage = crate::adapters::explore::build_macro_lineage_payload(current, &focus);
    // `lineage` is an owned temporary, unused after this point, so consume it
    // with `into_iter()` — moves each id/name/from/to string out instead of
    // cloning it (cute-dbt#438 review).
    let nodes: Vec<MacroDagNode> = lineage
        .nodes
        .into_iter()
        .map(|n| MacroDagNode {
            id: n.id,
            name: n.name,
            role: match n.macro_role {
                Some(crate::adapters::explore::MacroRole::User) => "user",
                // Downstream is the default for any focus node that is not a
                // user; a node with no role (never produced by the focused
                // builder, but exhaustive) reads as context.
                _ => "downstream",
            },
            not_compiled: n.not_compiled,
        })
        .collect();
    let edges: Vec<MacroDagEdge> = lineage
        .edges
        .into_iter()
        .map(|e| MacroDagEdge {
            from: e.from,
            to: e.to,
        })
        .collect();
    MacroDagPayload { nodes, edges }
}

/// One impacted (macro-calling) root-project model in a
/// [`ChangedMacroView`] (cute-dbt#265 Slice C) — the model-selector's
/// option plus the server-rendered surfaces it reveals.
///
/// The model SQL ([`sql_lines`](Self::sql_lines)) and the first-order
/// call-site snippets ([`call_sites`](Self::call_sites)) are both
/// server-composed from the model's manifest `raw_code` (the
/// "display strings composed in Rust" house rule), so the report JS is a
/// pure renderer that toggles which model's pre-rendered surfaces are
/// visible — never a recompute, never a fetch.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactedModelView {
    /// The model's full node id — the stable `<option value>` + the
    /// `data-model` hook the selector matches a server-rendered panel by.
    pub model_id: String,
    /// The model's bare name (the `<option>` display text).
    pub name: String,
    /// The model's declaring file, project-relative (the panel's code-card
    /// header). Empty when the manifest carries no `original_file_path`.
    pub path: String,
    /// The model's CURRENT raw SQL as plain context lines (the
    /// `macro_body_context_lines` shape — one terminator stripped, one
    /// [`DiffLine`] per `\n`-split line). The inline-SQL panel the selector
    /// reveals. Empty when the manifest carries no `raw_code` for this
    /// model (the rare null-fill — the panel states the honest absence).
    pub sql_lines: Vec<DiffLine>,
    /// The FIRST-ORDER call sites of the changed macro in this model's
    /// `raw_code` (founder D6 — first-order in the report; full-downstream
    /// ref()-lineage is the explorer, #345). Each entry is one line of the
    /// model body that names the macro, capped at
    /// [`call_site_cap`](Self::call_site_cap) for the default reveal; the
    /// template shows the rest behind a "more" disclosure. Never longer
    /// than [`call_site_total`](Self::call_site_total).
    pub call_sites: Vec<CallSiteView>,
    /// The TOTAL number of call sites found in this model's `raw_code` —
    /// the honest count the "showing N of M" copy reads, even when the
    /// shown set is capped.
    pub call_site_total: usize,
    /// How many call sites the template shows before the "more" disclosure
    /// (`MACRO_CALL_SITE_CAP`) — carried on the payload so the JS reveal
    /// and the headless guard agree on the boundary.
    pub call_site_cap: usize,
    /// Whether this model's heavy surface (inline SQL + call sites) is
    /// server-rendered (cute-dbt#265 Slice D, founder D5). `true` for the
    /// first N impacted models (in id order, N = the gen-time
    /// `macro_body_cap`); `false` past the cap, where the template renders
    /// a compact "body not inlined" affordance (name + path only) instead
    /// of the SQL panel — bounding a widely-used macro's report size. The
    /// model-selector still lists every entry regardless of this flag.
    pub inline_body: bool,
}

/// One first-order call site of the changed macro in an impacted model's
/// `raw_code` (cute-dbt#265 Slice C).
#[derive(Debug, Clone, Serialize)]
pub struct CallSiteView {
    /// The 1-based line number of the call site in the model's `raw_code`
    /// (the panel's gutter label).
    pub line: usize,
    /// The full source line containing the macro call, leading/trailing
    /// whitespace trimmed (the snippet text — auto-escaped by askama, never
    /// trusted as markup).
    pub text: String,
}

/// One row of a [`ChangedMacroView`]'s flattened impacted-model directory
/// tree (cute-dbt#265 founder D3).
///
/// The tree is grouped by the impacted models' `original_file_path`
/// directory segments; each directory is a `dir` row and each model is a
/// `model` leaf. Flattened pre-order with a 0-based [`depth`](Self::depth)
/// (the askama template indents by depth — recursion-free rendering).
#[derive(Debug, Clone, Serialize)]
pub struct MacroTreeRow {
    /// `"dir"` for a directory grouping row, `"model"` for an impacted
    /// model leaf — the template's per-kind CSS hook + `data-kind`.
    pub kind: &'static str,
    /// The display label: the bare directory segment for a `dir` row, the
    /// bare model name for a `model` leaf.
    pub label: String,
    /// 0-based nesting depth — the template's indent driver (`dir` rows at
    /// each path segment, the model leaf one level past its directory).
    pub depth: usize,
    /// The model's full node id for a `model` leaf (the stable selector
    /// hook); empty for a `dir` row.
    pub model_id: String,
}

/// Which scope source produced this report — selects the diff-scope
/// banner's provenance clause.
///
/// [`ScopeSource::Baseline`] renders "vs baseline manifest `<label>`";
/// [`ScopeSource::PrDiff`] renders "from PR file diff" (there is no
/// baseline manifest to name on the PR-diff path — naming one would be a
/// false statement, cute-dbt#85).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeSource {
    /// Scoped via `--baseline-manifest` (dbt `state:modified`).
    Baseline,
    /// Scoped via `--pr-diff` (a PR's `git diff --unified=0`).
    PrDiff,
}

// ---------------------------------------------------------------------
// Project-definition panel view (cute-dbt#266)
// ---------------------------------------------------------------------

/// One server-rendered row of the project-definition panel.
struct ProjectPanelRowView {
    /// Snake-case category key — the row's CSS hook + chip class
    /// (`vars`, `config_tree`, `dispatch`, `hooks`, `paths`,
    /// `identity`, `other`).
    category_key: &'static str,
    /// Human category chip text.
    category_label: &'static str,
    /// The changed key's display path (the domain change's `label`).
    label: String,
    /// `old → new` / `added: v` / `removed: v`, values as compact JSON
    /// (type-faithful: `"1"` and `1` read differently). Empty on a
    /// hooks row whose inline SQL diff renders instead (cute-dbt#269) —
    /// the raw JSON arrays would only duplicate the diff.
    detail: String,
    /// Honesty note rendered inside the row (empty ⇒ none). Vars rows
    /// state plainly "blast radius not attributed" (attribution is a
    /// later slice — the copy never implies coming-soon); dispatch rows
    /// state the project-wide/not-attributable fact; hook rows state
    /// the manifest-side `operation.*` reality (cute-dbt#269).
    note: String,
    /// The affected-models sentence for a `models:`-section config-tree
    /// row (cute-dbt#267) — counts always explicit; names inline up to
    /// the R1b cap. Empty for every other row (no claim is made for
    /// sections this slice does not attribute).
    affected_text: String,
    /// R1b overflow (cute-dbt#267): the full affected-model name list
    /// when the count exceeds [`CONFIG_AFFECTED_CAP`] — rendered inside
    /// a collapsed `<details>` ("listed, not individually rendered").
    /// Empty when the names already ride inline in `affected_text`.
    affected_overflow: Vec<String>,
    /// Tier-chip label (cute-dbt#269) — empty ⇒ no chip. The dispatch
    /// banner carries `UNKNOWN` (the shaping's per-category tier).
    tier: &'static str,
    /// `true` ⇒ the row renders as a full-width banner (`is-banner`) —
    /// the dispatch row's project-wide-warning presentation.
    banner: bool,
    /// `true` ⇒ the row emits the `data-hook-slot` container the report
    /// JS fills with the #111-rendered hook SQL diff (cute-dbt#269).
    hook_slot: bool,
    /// Per-var attribution entries (cute-dbt#268) — non-empty exactly on
    /// vars rows whose [`VarChangeFacts`] were attached. When non-empty
    /// the row's `detail` is empty (each entry carries its own resolved
    /// old→new) and `note` carries the honest-UNKNOWN residual copy.
    var_entries: Vec<ProjectVarEntryView>,
}

/// One precedence-resolved var edit inside a vars panel row
/// (cute-dbt#268).
struct ProjectVarEntryView {
    /// The var's bare name.
    name: String,
    /// `package-scoped: {pkg}` for a `vars.{pkg}.{name}` edit; empty for
    /// a global entry.
    scope: String,
    /// `old → new` / `added: v` / `removed: v`, compact JSON.
    detail: String,
    /// One line per non-empty tier, strongest first.
    tier_lines: Vec<VarTierLineView>,
    /// Masked-package / insulated-test / dynamic-bucket / zero-hit
    /// statements, one per line.
    notes: Vec<String>,
}

/// One tier's affected-models line inside a var entry (cute-dbt#268).
struct VarTierLineView {
    /// `direct` / `config` / `macro` — the tier-chip CSS hook.
    tier_key: &'static str,
    /// `DIRECT` / `CONFIG` / `MACRO` — the chip text.
    tier_label: &'static str,
    /// The "at least N models …" sentence (names inline up to the R1b
    /// cap).
    text: String,
    /// R1b overflow: the full name list when the count exceeds the cap
    /// (rendered inside a collapsed `<details>` — "listed, not
    /// individually rendered").
    overflow: Vec<String>,
}

/// One raw diff line of the panel's Shape-A fallback row.
struct ProjectPanelLineView {
    /// `context` / `removed` / `added` — the CSS hook.
    kind: &'static str,
    /// The line text, sigil-free.
    text: String,
}

/// The server-rendered "Project definition changed" panel — built from
/// the domain [`ProjectChangePanel`] exactly when `dbt_project.yml` is in
/// the PR diff. Wording lives here (adapter); facts live in the domain
/// POD (the `ModelYamlOutcome` precedent).
struct ProjectPanelView {
    /// Categorized rows (empty in fallback mode and for a
    /// formatting-only edit).
    rows: Vec<ProjectPanelRowView>,
    /// `true` when categorization succeeded but found zero semantic
    /// changes — the panel says so instead of rendering nothing.
    is_empty_change: bool,
    /// Non-empty exactly in fallback mode: the explicit degrade copy
    /// ("could not categorize" / "could not reconstruct the previous
    /// version" / the absence note).
    fallback_copy: String,
    /// The fallback's raw diff lines (also carried on the absence-note
    /// arm — the hunks are known even when the file is not).
    fallback_lines: Vec<ProjectPanelLineView>,
}

/// Category key + chip label + per-row honesty note for one
/// [`ProjectChangeCategory`].
///
/// The dispatch note is the UNKNOWN-tier banner copy (cute-dbt#269),
/// written to the honest-UNKNOWN principles: it stays in-row (never a
/// report-global claim), enumerates why attribution is impossible, and
/// states what WAS checked. Hook rows get their note per-change from
/// [`hook_row_note`] (the manifest-side facts are per-row, not
/// per-category).
fn project_category_strings(
    category: ProjectChangeCategory,
) -> (&'static str, &'static str, &'static str) {
    match category {
        ProjectChangeCategory::Vars => (
            "vars",
            "vars",
            // Defensive fallback only: since cute-dbt#268 every
            // categorized vars row carries VarChangeFacts and
            // project_panel_row swaps this note for the honest-UNKNOWN
            // residual copy (vars_row_note). A facts-less row keeps the
            // locked interim statement (plain, never "coming soon").
            "blast radius not attributed",
        ),
        ProjectChangeCategory::ConfigTree => ("config_tree", "config tree", ""),
        ProjectChangeCategory::Dispatch => (
            "dispatch",
            "dispatch",
            "macro search order changed — a project-wide effect. Any model, test, \
             snapshot, or hook may resolve dispatched macros differently after this \
             edit; which ones cannot be attributed statically, because macro \
             resolution happens per call at compile time. Checked: the old and new \
             dispatch values in dbt_project.yml, shown in this row — no call-site \
             resolution was attempted (zero-compute).",
        ),
        ProjectChangeCategory::Hooks => ("hooks", "hooks", ""),
        ProjectChangeCategory::Paths => ("paths", "paths", ""),
        ProjectChangeCategory::Identity => ("identity", "identity", ""),
        ProjectChangeCategory::Other => ("other", "other", ""),
    }
}

/// The hook row's manifest-side honesty note (cute-dbt#269): names the
/// `operation.*` reality and — on every degrade — what was checked and
/// the enumerable causes, per the honest-UNKNOWN copy principles.
fn hook_row_note(facts: &HookChangeFacts) -> String {
    match facts.manifest {
        HookManifestPresence::Matched if facts.operation_ids.is_empty() => {
            "no operation nodes remain in the manifest — consistent with this removal".to_owned()
        }
        HookManifestPresence::Matched => {
            format!("runs in the manifest as {}", facts.operation_ids.join(", "))
        }
        HookManifestPresence::Absent => {
            "no matching operation.* nodes in the manifest (checked the manifest nodes \
             map for this project's hook names) — the manifest may predate this edit; \
             the diff is read from dbt_project.yml itself"
                .to_owned()
        }
        HookManifestPresence::Diverged => format!(
            "the manifest's operation nodes ({}) do not match the working-tree hooks \
             — manifest and working tree may be out of sync; the diff is read from \
             dbt_project.yml itself",
            facts.operation_ids.join(", "),
        ),
    }
}

/// A change's `detail` string: both sides present → `old → new`;
/// one-sided → `added:` / `removed:`. Values render as compact JSON.
fn project_change_detail(change: &ProjectChange) -> String {
    side_detail(change.old.as_ref(), change.new.as_ref())
}

/// The fallback arm's explicit copy — "could not categorize" /
/// "could not reconstruct the previous version" / the absence note.
fn project_fallback_copy(reason: ProjectFallbackReason) -> &'static str {
    match reason {
        ProjectFallbackReason::NewParseFailed => {
            "dbt_project.yml could not be parsed, so this change could not be \
             categorized — showing the raw diff."
        }
        ProjectFallbackReason::OldParseFailed => {
            "The previous version of dbt_project.yml could not be parsed, so this \
             change could not be categorized — showing the raw diff."
        }
        ProjectFallbackReason::OldNotReconstructable => {
            "Could not reconstruct the previous version of dbt_project.yml from \
             the diff, so this change could not be categorized — showing the raw diff."
        }
        ProjectFallbackReason::FileUnreadable => {
            "dbt_project.yml changed in this diff, but the file could not be read \
             from the project root — nothing to categorize. Showing the raw diff."
        }
    }
}

/// R1b presentation cap (cute-dbt#267): up to this many affected-model
/// names ride inline in the config-tree row's sentence; past it the
/// names collapse into a `<details>` listing with the count stated
/// explicitly ("listed, not individually rendered"). Calibrated from the
/// diff-showcase dogfood (its marts edit affects 21 models).
const CONFIG_AFFECTED_CAP: usize = 10;

/// Invert the per-model attribution map into per-(subtree-path, key)
/// affected-model NODE-ID sets — the panel rows' listing source. Full
/// node ids dedupe and count (dbt 1.6+ allows same-named models across
/// packages, and a section-root edit selects across packages — bare-name
/// dedupe would under-count exactly there); the human-facing names
/// derive at sentence-build time ([`affected_display_names`]),
/// disambiguating only on collision.
fn affected_models_by_leaf(
    attributions: &BTreeMap<String, Vec<ConfigAttribution>>,
) -> BTreeMap<(String, String), BTreeSet<String>> {
    let mut out: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for (node_id, entries) in attributions {
        for attribution in entries {
            out.entry((attribution.path.clone(), attribution.key.clone()))
                .or_default()
                .insert(node_id.clone());
        }
    }
    out
}

/// The sorted display names for one row's affected node-id set: the bare
/// model name (the selector vocabulary) wherever it is unique within the
/// set, the full node id where two packages collide on the same bare
/// name (so the listing never shows two indistinguishable entries).
fn affected_display_names(ids: &BTreeSet<String>) -> Vec<String> {
    let mut bare_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for id in ids {
        *bare_counts.entry(leaf_segment(id)).or_insert(0) += 1;
    }
    let mut names: Vec<String> = ids
        .iter()
        .map(|id| {
            let bare = leaf_segment(id);
            if bare_counts[bare] > 1 {
                id.clone()
            } else {
                bare.to_owned()
            }
        })
        .collect();
    names.sort();
    names
}

/// The affected-models sentence + R1b overflow list for one
/// `models:`-section config-tree row (cute-dbt#267). TOTAL-tier copy:
/// counts are exact — counted over full node ids (fusion's own
/// resolution; cross-package bare-name twins both count) — so a zero is
/// a truthful "affects 0 models in this manifest" (shadowed everywhere,
/// or no model's fqn descends through the edited path).
fn affected_models_strings(ids: &BTreeSet<String>) -> (String, Vec<String>) {
    let count = ids.len();
    if count == 0 {
        return ("affects 0 models in this manifest".to_owned(), Vec::new());
    }
    let noun = if count == 1 { "model" } else { "models" };
    let names = affected_display_names(ids);
    if count <= CONFIG_AFFECTED_CAP {
        (
            format!(
                "affects {count} {noun} — widened into report scope: {}",
                names.join(", ")
            ),
            Vec::new(),
        )
    } else {
        (
            format!("affects {count} {noun} — widened into report scope, listed below"),
            names,
        )
    }
}

// ---------------------------------------------------------------------
// Vars-row attribution presentation (cute-dbt#268)
// ---------------------------------------------------------------------

/// Inline-vs-overflow split for one affected list (the R1b cap, shared
/// with the cute-dbt#267 config-tree rows): up to [`CONFIG_AFFECTED_CAP`]
/// names ride inline; past it the names collapse into the row's
/// `<details>` and the sentence states the count only.
fn capped_names(names: Vec<String>) -> (String, Vec<String>) {
    if names.len() <= CONFIG_AFFECTED_CAP {
        (names.join(", "), Vec::new())
    } else {
        (String::new(), names)
    }
}

/// One tier's sentence: "at least N model(s) {claim}: a, b" — or, past
/// the R1b cap, "at least N models {claim} — listed, not individually
/// rendered" with the names in the overflow `<details>`. The claim
/// arrives in both verb agreements (`reads` / `read`).
fn var_tier_line(
    tier_key: &'static str,
    tier_label: &'static str,
    claim_one: &str,
    claim_many: &str,
    names: Vec<String>,
) -> Option<VarTierLineView> {
    if names.is_empty() {
        return None;
    }
    let count = names.len();
    let (noun, claim) = if count == 1 {
        ("model", claim_one)
    } else {
        ("models", claim_many)
    };
    let (inline, overflow) = capped_names(names);
    let text = if overflow.is_empty() {
        format!("at least {count} {noun} {claim}: {inline}")
    } else {
        format!("at least {count} {noun} {claim} \u{2014} listed, not individually rendered")
    };
    Some(VarTierLineView {
        tier_key,
        tier_label,
        text,
        overflow,
    })
}

/// Sorted display names (bare unless ambiguous) for a node-id list.
fn var_display_names(ids: &[String]) -> Vec<String> {
    affected_display_names(&ids.iter().cloned().collect::<BTreeSet<String>>())
}

/// MACRO-tier display names: each model (bare unless ambiguous within
/// the set — the [`affected_display_names`] collision rule) paired with
/// its mediating macro, sorted.
fn macro_tier_names(hits: &[crate::domain::MacroVarHit]) -> Vec<String> {
    let mut bare_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for hit in hits {
        *bare_counts.entry(leaf_segment(&hit.model)).or_insert(0) += 1;
    }
    let mut names: Vec<String> = hits
        .iter()
        .map(|hit| {
            let bare = leaf_segment(&hit.model);
            let display = if bare_counts[bare] > 1 {
                hit.model.as_str()
            } else {
                bare
            };
            format!("{display} (via {})", leaf_segment(&hit.via))
        })
        .collect();
    names.sort();
    names
}

/// The three tier lines of one var entry, strongest tier first
/// (cute-dbt#268). MACRO-tier names carry their mediating macro
/// (`fct_x (via add_dq_flags)`).
fn var_entry_tier_lines(entry: &VarAttribution) -> Vec<VarTierLineView> {
    let macro_names = macro_tier_names(&entry.via_macros);
    [
        var_tier_line(
            "direct",
            "DIRECT",
            "reads this var directly in SQL",
            "read this var directly in SQL",
            var_display_names(&entry.direct),
        ),
        var_tier_line(
            "config",
            "CONFIG",
            "carries config driven by this var",
            "carry config driven by this var",
            var_display_names(&entry.config),
        ),
        var_tier_line(
            "macro",
            "MACRO",
            "reads this var through its macro closure",
            "read this var through their macro closure",
            macro_names,
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The masked / insulated / dynamic / zero-hit note lines of one var
/// entry (cute-dbt#268) — each an explicit, in-row statement.
fn var_entry_notes(entry: &VarAttribution) -> Vec<String> {
    let mut notes = Vec::new();
    if !entry.masked_packages.is_empty() {
        notes.push(format!(
            "masked for {}: a package-scoped value pins this var there, so this edit \
             does not reach those models (package vars outrank global vars)",
            entry.masked_packages.join(", "),
        ));
    }
    if !entry.insulated_tests.is_empty() {
        let count = entry.insulated_tests.len();
        let noun = if count == 1 {
            "unit test pins"
        } else {
            "unit tests pin"
        };
        let names: Vec<&str> = entry
            .insulated_tests
            .iter()
            .map(|id| leaf_segment(id))
            .collect();
        notes.push(format!(
            "{count} {noun} this var in overrides.vars and {} insulated from this edit \
             (the override always wins): {}",
            if count == 1 { "is" } else { "are" },
            names.join(", "),
        ));
    }
    if !entry.dynamic.is_empty() {
        let count = entry.dynamic.len();
        let noun = if count == 1 {
            "model calls"
        } else {
            "models call"
        };
        let (inline, overflow) = capped_names(var_display_names(&entry.dynamic));
        notes.push(if overflow.is_empty() {
            format!("{count} {noun} var() with a computed name and cannot be ruled out: {inline}")
        } else {
            format!("{count} {noun} var() with a computed name and cannot be ruled out")
        });
    }
    if entry.direct.is_empty() && entry.config.is_empty() && entry.via_macros.is_empty() {
        notes.push("no referencing models found by the static scan".to_owned());
    }
    notes
}

/// Build the per-var entries of a vars row from its attached facts.
fn var_entry_views(facts: &VarChangeFacts) -> Vec<ProjectVarEntryView> {
    facts
        .entries
        .iter()
        .map(|entry| ProjectVarEntryView {
            name: entry.name.clone(),
            scope: entry
                .package
                .as_ref()
                .map(|pkg| format!("package-scoped: {pkg}"))
                .unwrap_or_default(),
            detail: side_detail(entry.old.as_ref(), entry.new.as_ref()),
            tier_lines: var_entry_tier_lines(entry),
            notes: var_entry_notes(entry),
        })
        .collect()
}

/// The honest-UNKNOWN residual copy of an attributed vars row
/// (cute-dbt#268), written to the locked principles: in-row (never a
/// report-global hedge), causes enumerated, what WAS checked stated,
/// "at least N" framing, the disabled-membership caveat, and the
/// contextualize-don't-widen statement. Scoped to the pr-diff arm by
/// construction (the panel only renders there).
fn vars_row_note(footprint: &VarScanFootprint) -> String {
    let python = if footprint.python_models > 0 {
        format!(
            " ({} Python models could not be scanned)",
            footprint.python_models,
        )
    } else {
        String::new()
    };
    let model_noun = if footprint.models_scanned == 1 {
        "model's"
    } else {
        "models'"
    };
    let macro_noun = if footprint.macros_scanned == 1 {
        "macro body"
    } else {
        "macro bodies"
    };
    format!(
        "Blast radius is not fully attributable statically \u{2014} the listed counts \
         are \u{201c}at least\u{201d}, never exact. Not attributable: dynamic var() \
         names, var-to-var value indirection, CLI --vars overrides (CLI values outrank \
         everything shown here), Python models. Checked: {} {model_noun} SQL and \
         configs plus {} {macro_noun}{python}. An inline var() default never overrides \
         a project value. Models disabled by this edit drop out of the manifest and \
         cannot be listed. Referencing models are contextualized here, never widened \
         into report scope.",
        footprint.models_scanned, footprint.macros_scanned,
    )
}

/// A side pair's compact display — `old → new` / `added:` / `removed:`
/// (the [`project_change_detail`] vocabulary over explicit sides).
fn side_detail(old: Option<&Value>, new: Option<&Value>) -> String {
    let compact = |v: &Value| serde_json::to_string(v).unwrap_or_else(|_| "null".to_owned());
    match (old, new) {
        (Some(old), Some(new)) => format!("{} \u{2192} {}", compact(old), compact(new)),
        (None, Some(new)) => format!("added: {}", compact(new)),
        (Some(old), None) => format!("removed: {}", compact(old)),
        (None, None) => String::new(),
    }
}

/// Build one categorized panel row, attaching the cute-dbt#267
/// affected-models listing to `models:`-section config-tree rows.
fn project_panel_row(
    change: &ProjectChange,
    affected: &BTreeMap<(String, String), BTreeSet<String>>,
) -> ProjectPanelRowView {
    let (category_key, category_label, note) = project_category_strings(change.category);
    let empty = BTreeSet::new();
    let (affected_text, affected_overflow) = change
        .tree
        .as_ref()
        .filter(|tree| tree.section == "models")
        .map_or((String::new(), Vec::new()), |tree| {
            let names = affected
                .get(&(tree.dotted(), tree.key.clone()))
                .unwrap_or(&empty);
            affected_models_strings(names)
        });
    // cute-dbt#269 — purpose-built rows: a hooks row with an inline SQL
    // diff drops the raw-JSON detail (the diff IS the old→new statement)
    // and emits the slot the JS fills; the dispatch row renders as the
    // UNKNOWN-tier banner.
    // cute-dbt#268 — a vars row with attached facts likewise drops the
    // raw detail (each entry carries its precedence-resolved old→new)
    // and swaps the interim note for the honest-UNKNOWN residual copy.
    let hook_diff = change
        .hook
        .as_ref()
        .is_some_and(|facts| facts.sql_diff.is_some());
    let is_dispatch = change.category == ProjectChangeCategory::Dispatch;
    let var_facts = change.vars.as_ref();
    ProjectPanelRowView {
        category_key,
        category_label,
        label: change.label.clone(),
        detail: if hook_diff || var_facts.is_some() {
            String::new()
        } else {
            project_change_detail(change)
        },
        note: match (change.hook.as_ref(), var_facts) {
            (Some(hook), _) => hook_row_note(hook),
            (None, Some(facts)) => vars_row_note(&facts.footprint),
            (None, None) => note.to_owned(),
        },
        affected_text,
        affected_overflow,
        tier: if is_dispatch { "UNKNOWN" } else { "" },
        banner: is_dispatch,
        hook_slot: hook_diff,
        var_entries: var_facts.map(var_entry_views).unwrap_or_default(),
    }
}

/// Build the server-rendered panel view from the domain panel POD plus
/// the cute-dbt#267 attribution map (the affected-models listings on
/// `models:`-section config-tree rows).
fn project_panel_view(
    panel: &ProjectChangePanel,
    attributions: &BTreeMap<String, Vec<ConfigAttribution>>,
) -> ProjectPanelView {
    match panel {
        ProjectChangePanel::Categorized { changes } => {
            let affected = affected_models_by_leaf(attributions);
            let rows = changes
                .iter()
                .map(|change| project_panel_row(change, &affected))
                .collect::<Vec<_>>();
            ProjectPanelView {
                is_empty_change: rows.is_empty(),
                rows,
                fallback_copy: String::new(),
                fallback_lines: Vec::new(),
            }
        }
        ProjectChangePanel::Fallback { reason, raw } => ProjectPanelView {
            rows: Vec::new(),
            is_empty_change: false,
            fallback_copy: project_fallback_copy(*reason).to_owned(),
            fallback_lines: raw
                .iter()
                .map(|line: &DiffLine| ProjectPanelLineView {
                    kind: match line.kind {
                        DiffLineKind::Context => "context",
                        DiffLineKind::Removed => "removed",
                        DiffLineKind::Added => "added",
                    },
                    text: line.text.clone(),
                })
                .collect(),
        },
    }
}

/// How many first-order call-site snippets the macro lens shows before the
/// "more" disclosure (cute-dbt#265 Slice C, founder D6 — a generous-default
/// low cap; the rest stay one click away). Carried onto every
/// [`ImpactedModelView`] so the JS reveal and the headless guard read the
/// same boundary. Distinct from the inline-model-body cap (Slice D, the
/// gen-time knob) — this bounds per-model call-site noise, not how many
/// model bodies inline.
const MACRO_CALL_SITE_CAP: usize = 3;

/// Build the macro perspective lens (cute-dbt#265, Slice B) from the
/// changed-macro set + the manifest.
///
/// `changed_macros` is the resolved set of changed root-project macro
/// `unique_id`s (the cli picks the `changed_macros_pr_diff` /
/// `changed_macros_baseline` arm by scope source). For each changed macro
/// this resolves its name/package/path/body from the manifest, reconstructs
/// the body diff on the `--pr-diff` arm (`index = Some(...)`), and walks the
/// reverse [`macro_blast_radius`] into a collapsible directory tree of the
/// impacted root-project models. `scope_source` selects the fidelity chip
/// (`Baseline` = exact body comparison, `PrDiff` = path/name heuristic).
///
/// `body_cap` (cute-dbt#265 Slice D, founder D5) bounds how many impacted
/// models carry a server-rendered inline SQL + call-site panel: the first
/// `body_cap` (in id order) inline their body
/// ([`ImpactedModelView::inline_body`] `== true`), the rest show a
/// tree-only "body not inlined" affordance — keeping a widely-used macro's
/// (single, frozen) report bounded. The cli resolves `body_cap` at the I/O
/// boundary ([`crate::domain::DEFAULT_MACRO_BODY_CAP`] default).
///
/// Returns `None` when `changed_macros` is empty — the cli only calls this
/// when [`Experiment::MacroLens`](crate::domain::Experiment::MacroLens) is
/// on, so `None` (no macro changed) and the off-gate (the cli passes the
/// `None` directly) both omit the section, keeping the non-macro goldens
/// byte-identical.
#[must_use]
pub fn build_macro_lens(
    current: &Manifest,
    changed_macros: &BTreeSet<String>,
    scope_source: ScopeSource,
    index: Option<&NormalizedDiffIndex>,
    body_cap: usize,
) -> Option<MacroLensPayload> {
    // Root-project filter: the lens is the REVIEWER's macros, never a
    // vendor package's. The pr-diff path-primary channel resolves any macro
    // whose file the PR touched regardless of package (a PR can vendor a
    // dependency), so a vendor macro edit can reach the changed set — drop
    // it here. A macro with no recorded `package_name` (the rare null-fill)
    // is kept (fail-open: the diff resolved it to a real id), unless a
    // project name is set and the macro's package explicitly differs.
    let project = current.metadata().project_name();
    let macros = changed_macros
        .iter()
        .filter(|macro_id| is_root_project_macro(current, macro_id, project))
        .map(|macro_id| changed_macro_view(current, macro_id, index, body_cap))
        .collect::<Vec<_>>();
    if macros.is_empty() {
        return None;
    }
    let fidelity = match scope_source {
        ScopeSource::Baseline => "exact",
        ScopeSource::PrDiff => "heuristic",
    };
    Some(MacroLensPayload { macros, fidelity })
}

/// Whether a changed macro belongs to the root project — the lens filter
/// (vendor-package macros are not the reviewer's concern). A macro whose
/// recorded `package_name` matches `project` passes; a macro with no
/// recorded package passes (fail-open — the diff resolved it to a real id);
/// a macro whose package explicitly differs from a known project name is
/// dropped.
fn is_root_project_macro(current: &Manifest, macro_id: &str, project: Option<&str>) -> bool {
    match (
        current
            .macro_identity()
            .get(macro_id)
            .and_then(MacroIdentity::package_name),
        project,
    ) {
        (Some(pkg), Some(proj)) => pkg == proj,
        // No recorded package, or no project name to compare against:
        // fail-open (keep) — the changed-macro detection already resolved
        // this id from the diff/baseline.
        _ => true,
    }
}

/// Resolve one changed macro into its [`ChangedMacroView`] — identity, body
/// (diff on the pr-diff arm, plain context lines otherwise), and the
/// flattened impacted-model directory tree.
fn changed_macro_view(
    current: &Manifest,
    macro_id: &str,
    index: Option<&NormalizedDiffIndex>,
    body_cap: usize,
) -> ChangedMacroView {
    let identity = current.macro_identity().get(macro_id);
    // Name: the recorded identity name, else the unique_id leaf.
    let name = identity
        .and_then(|i| i.name())
        .map_or_else(|| leaf_segment(macro_id).to_owned(), str::to_owned);
    let package = identity
        .and_then(|i| i.package_name())
        .or_else(|| current.metadata().project_name())
        .unwrap_or_default()
        .to_owned();
    let path = identity
        .and_then(|i| i.original_file_path())
        .unwrap_or_default()
        .to_owned();
    let body = current.macros().get(macro_id).map_or("", String::as_str);
    // The pr-diff arm reconstructs the body diff when the macro's file was
    // touched + aligned + substantively changed; baseline (no index) and an
    // untouched/stale macro fall back to the plain body context lines.
    let diff = index
        .filter(|_| !path.is_empty())
        .and_then(|idx| reconstruct_macro_sql_diff(body, &path, idx));
    let body_lines = macro_body_context_lines(body);
    let radius = macro_blast_radius(current, macro_id);
    let impacted_count = radius.len();
    let tree = impacted_model_tree(current, &radius);
    let impacted_models = impacted_model_views(current, &radius, &name, body_cap);
    // The inlined count is min(cap, impacted_count) — the "N" of the
    // "showing N of M bodies" copy. When it equals impacted_count the cap
    // is not exceeded and the template omits the over-cap affordance.
    let inlined_count = impacted_count.min(body_cap);
    // cute-dbt#431 — the macro-scoped lineage DAG for the Macros tab. Reuses
    // the explore `build_macro_lineage_payload` focus + role classification,
    // projected to the slim report tab-DAG shape. Independent of the body cap
    // (the DAG is bounded by the blast radius, already counted above).
    let macro_dag = build_macro_dag(current, macro_id);
    ChangedMacroView {
        name,
        package,
        path,
        diff,
        body_lines,
        impacted_count,
        tree,
        impacted_models,
        inlined_count,
        macro_dag,
    }
}

/// Build one [`ImpactedModelView`] per model in the blast radius
/// (cute-dbt#265 Slice C) — the model-selector option set with each
/// model's inline SQL + first-order call sites pre-rendered from the
/// manifest `raw_code` (no working-tree read — the model SQL is in the
/// manifest, matching the existing Model-SQL surface's source). In blast-
/// radius id order ([`BTreeSet`]), so the selector + the directory tree
/// agree on ordering and the golden is stable.
///
/// **Slice D cap (founder D5):** only the first `body_cap` models (in id
/// order) carry a server-rendered inline body — their `inline_body` is
/// `true` and the expensive SQL / call-site scan runs. Past the cap the
/// view carries identity only (`inline_body = false`, empty SQL + call
/// sites): the model-selector still lists it, but the panel shows a "body
/// not inlined" affordance, so a widely-used macro's report stays bounded.
fn impacted_model_views(
    current: &Manifest,
    radius: &BTreeSet<NodeId>,
    macro_name: &str,
    body_cap: usize,
) -> Vec<ImpactedModelView> {
    radius
        .iter()
        .enumerate()
        .map(|(rank, id)| {
            let node = current.node(id);
            let model_id = id.as_str().to_owned();
            let name = leaf_segment(id.as_str()).to_owned();
            let path = node
                .and_then(Node::original_file_path)
                .unwrap_or_default()
                .to_owned();
            // Past the cap: identity only, no inline body. Skipping the
            // raw_code scan keeps a 50-model macro's payload bounded — the
            // heavy surface is exactly the bytes the cap is meant to bound.
            if rank >= body_cap {
                return ImpactedModelView {
                    model_id,
                    name,
                    path,
                    sql_lines: Vec::new(),
                    call_sites: Vec::new(),
                    call_site_total: 0,
                    call_site_cap: MACRO_CALL_SITE_CAP,
                    inline_body: false,
                };
            }
            let raw = node.and_then(Node::raw_code).unwrap_or("");
            let call_sites = macro_call_sites(raw, macro_name);
            let call_site_total = call_sites.len();
            let shown = call_sites.into_iter().take(MACRO_CALL_SITE_CAP).collect();
            ImpactedModelView {
                model_id,
                name,
                path,
                sql_lines: macro_body_context_lines(raw),
                call_sites: shown,
                call_site_total,
                call_site_cap: MACRO_CALL_SITE_CAP,
                inline_body: true,
            }
        })
        .collect()
}

/// The FIRST-ORDER call sites of `macro_name` in a model's `raw_code`
/// (cute-dbt#265 Slice C) — every body line that invokes the macro,
/// 1-based line number + the trimmed source line.
///
/// A pure string scan over already-loaded source (zero-egress, zero new
/// I/O). A line is a call site when it contains the macro name immediately
/// followed by `(` (a Jinja `{{ macro_name(...) }}` / `{% set x =
/// macro_name(...) %}` invocation), with a non-identifier char (or the line
/// start) before the name so `my_macro_name(` / `other_macro_name(` do not
/// false-match. Whitespace between the name and `(` is tolerated
/// (`macro_name ()`), matching Jinja's lexer. The match is intentionally
/// first-order only — call sites in OTHER macros this model transitively
/// reaches are the explorer's job (#345), not the report's.
fn macro_call_sites(raw: &str, macro_name: &str) -> Vec<CallSiteView> {
    if raw.is_empty() || macro_name.is_empty() {
        return Vec::new();
    }
    raw.split('\n')
        .enumerate()
        .filter(|(_, line)| line_invokes_macro(line, macro_name))
        .map(|(i, line)| CallSiteView {
            line: i + 1,
            text: line.trim().to_owned(),
        })
        .collect()
}

/// Whether `line` invokes `macro_name` as a call (`macro_name(` with an
/// optional run of whitespace before the `(`), bounded so a longer
/// identifier ending in `macro_name` does not match.
///
/// An empty `macro_name` returns `false` defensively: `str::find("")`
/// yields `Some(0)` and a zero-length name never advances `from`, so the
/// scan would otherwise spin forever. The sole caller
/// ([`macro_call_sites`]) already guards the empty name, but this local
/// guard makes the helper safe regardless of caller invariants — a silent
/// hang is the worst latent failure.
fn line_invokes_macro(line: &str, macro_name: &str) -> bool {
    if macro_name.is_empty() {
        return false;
    }
    let bytes = line.as_bytes();
    let name_len = macro_name.len();
    let mut from = 0;
    while let Some(rel) = line[from..].find(macro_name) {
        let at = from + rel;
        from = at + name_len;
        // Reject a match preceded by an identifier char (`x_macro_name`).
        let preceded_by_ident = at
            .checked_sub(1)
            .and_then(|p| bytes.get(p))
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
        if preceded_by_ident {
            continue;
        }
        // The next non-whitespace byte after the name must be `(`.
        if bytes.get(skip_ws_bytes(bytes, from)) == Some(&b'(') {
            return true;
        }
    }
    false
}

/// Advance past ASCII whitespace from byte offset `i` (the
/// [`crate::domain::macro_lens`] `skip_ws` shape, kept local to the
/// adapter scanner).
fn skip_ws_bytes(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// One raw-Jinja control zone located in `raw_code` — the ADAPTER-internal
/// intermediate emitted by [`locate_raw_zones`], NOT a domain type. Mirrors
/// dbt-fusion's `JinjaLayoutEvent` shape (kind + source span + a depth-stack
/// `block_id` pairing start↔end) but owns ZERO dependency (cute-dbt#448, L1).
/// `raw_span` covers the FULL construct: the opener `{%`/`{%-` through the
/// closer `%}`/`-%}` of the matching `endif`/`endfor`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ZoneFact {
    kind: ZoneKind,
    raw_span: SourceSpan,
    /// The depth-stack pairing id (start↔end). Distinguishes nested
    /// constructs the way fusion's `block_id` linker does; carried for fact
    /// completeness even though resolution keys on `raw_span`.
    block_id: u32,
}

/// A `{%…%}` block-tag boundary parsed out of `raw_code` by [`scan_block_tags`]:
/// the half-open byte range `[open, close)` of the WHOLE tag (delimiters
/// included, whitespace-control dashes included) and its classified leading
/// keyword. Variable tags `{{…}}` and comments `{#…#}` are skipped at the scan
/// layer and never surface as a `BlockTag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockTag {
    open: usize,
    close: usize,
    role: TagRole,
}

/// The block-pairing kind tracked on the depth-stack. SUPERSET of the public
/// [`ZoneKind`] (which is `#[non_exhaustive]` and carries only the EMITTED
/// kinds): adds `PlainIf` so a non-incremental `{% if %}…{% endif %}` still
/// PAIRS correctly (preserving nesting) without emitting a v0.1 zone (L9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    /// `{% if is_incremental() %}` — emits an [`ZoneKind::IncrementalGuard`].
    IncrementalGuard,
    /// `{% for … %}` — emits an [`ZoneKind::ForLoop`].
    ForLoop,
    /// Any other `{% if … %}` — paired (so nesting is honored) but NOT a zone.
    PlainIf,
}

impl BlockKind {
    /// The emitted [`ZoneKind`], or `None` for a non-emitting opener
    /// (`PlainIf`). Drives whether a matched pair produces a [`ZoneFact`].
    fn zone_kind(self) -> Option<ZoneKind> {
        match self {
            Self::IncrementalGuard => Some(ZoneKind::IncrementalGuard),
            Self::ForLoop => Some(ZoneKind::ForLoop),
            Self::PlainIf => None,
        }
    }

    /// Whether `closer` (an `endif`/`endfor` family) closes THIS opener:
    /// `if`/`plain-if`/`incremental-guard` all close with `endif`; `for` closes
    /// with `endfor`.
    fn closes_with(self, closer: EndKind) -> bool {
        matches!(
            (self, closer),
            (Self::IncrementalGuard | Self::PlainIf, EndKind::If) | (Self::ForLoop, EndKind::For)
        )
    }
}

/// The family an `end…` tag closes (`endif` vs `endfor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndKind {
    /// `{% endif %}`.
    If,
    /// `{% endfor %}`.
    For,
}

/// The control-flow role of a `{%…%}` tag's leading keyword. `Other` is any tag
/// we do not pair (`set`, `macro`, `call`, …) — scanned-and-skipped, never a
/// zone boundary in v0.1 (L9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagRole {
    /// `{% if … %}` / `{% for … %}` — opens a block.
    Start { kind: BlockKind },
    /// `{% else %}` / `{% elif … %}` — a mid-block divider (consumed by the
    /// depth-stack, never its own zone).
    Mid,
    /// `{% endif %}` / `{% endfor %}` — closes a block.
    End { kind: EndKind },
    /// Any other `{%…%}` tag — skipped.
    Other,
}

/// Fuzz seam (cute-dbt#464, Z3): drive the hand-rolled raw-zone scanner
/// ([`locate_raw_zones`]) with adversarial `raw_code` text.
///
/// `raw_code` is cute-dbt's SECOND untrusted-input parser (after the
/// `--pr-diff` patch parser fuzzed in cute-dbt#383): a malicious manifest can
/// carry arbitrary bytes in a node's `raw_code`, fed straight into this
/// hand-rolled `{%…%}`/`{{…}}`/`{#…#}` tag-boundary scanner. The scanner owns
/// the whitespace-control / string-literal-aware / comment-swallowing edge
/// cases minijinja's lexer would have handled for free (the cost of hand-roll,
/// the design's §9 fuzz obligation), so it is exactly the org's named Q4 blind
/// spot.
///
/// This `#[doc(hidden)]` re-export lets the `tests/fuzz_zone_scanner` bolero
/// target (stable Rust, no nightly) feed it random bytes and assert the
/// FAIL-CLOSED contract: the scan never panics / hangs, and a
/// malformed/unbalanced tag stream degrades to an EMPTY result — never a
/// fabricated zone. It returns a flattened, fully-`Eq` POD per located zone
/// (`(kind_tag, raw_start_byte, raw_end_byte, block_id)`) so the fuzz target
/// can assert DETERMINISM (the same bytes scan identically) and the structural
/// invariant (every emitted span is non-empty + in-bounds) without reaching
/// the private adapter `ZoneFact`/`SourceSpan` types. Not part of the v0.x
/// public API surface — it exists solely so a test target outside the crate can
/// reach the private scanner, the same internal-reach motivation
/// [`crate::cli::fuzz_parse_unified_diff`] has.
///
/// See `.claude/rules/testing.md` (the **Fuzz** rung) for the Q4
/// bring-into-shape context.
#[doc(hidden)]
#[must_use]
pub fn fuzz_locate_raw_zones(raw_code: &str) -> Vec<(u8, u32, u32, u32)> {
    locate_raw_zones(raw_code)
        .into_iter()
        .map(|z| {
            // A stable tag per emitted ZoneKind: the wire never sees this; it
            // only has to be deterministic so the fuzz target can compare two
            // scans of the same input for equality.
            // `ZoneKind` is `#[non_exhaustive]` but defined in THIS crate, so an
            // exhaustive match here is intra-crate-complete; a future emitted
            // variant deliberately breaks this site so determinism stays
            // well-defined (the maintainer assigns its sentinel).
            let kind_tag = match z.kind {
                ZoneKind::IncrementalGuard => 0u8,
                ZoneKind::ForLoop => 1u8,
            };
            (
                kind_tag,
                z.raw_span.start.byte,
                z.raw_span.end.byte,
                z.block_id,
            )
        })
        .collect()
}

/// Hand-rolled tag-boundary scanner over `raw_code` (cute-dbt#448, Z1). Emits a
/// [`ZoneFact`] per matched `{% if … %}…{% endif %}` / `{% for … %}…{% endfor %}`
/// construct, pairing start↔end with a DEPTH-STACK (NOT first-`endif`-wins) so
/// nested zones bind correctly. Mirrors fusion's `JinjaLayoutEvent` model with
/// no dependency (L1–L4): lives in the ADAPTER beside [`macro_call_sites`], so
/// even a future parser dep cannot leak into `src/domain/`.
///
/// Honesty backstop (FAIL-CLOSED): a malformed/unterminated/unbalanced tag
/// stream degrades to an EMPTY vec for the offending construct — NEVER a panic,
/// hang, or fabricated zone (§2.4). The edge surface owned by hand-roll:
/// whitespace control (`{%-`/`-%}`), string-literal-aware `%}` skipping
/// (`{% … "%}" … %}` does not close early), and `{#…#}` comments swallowing
/// inner tags (`{# {% if %} #}` is NOT a zone).
fn locate_raw_zones(raw_code: &str) -> Vec<ZoneFact> {
    if raw_code.is_empty() {
        return Vec::new();
    }
    // A malformed/unterminated tag aborts the whole scan (fail-closed).
    let Some(tags) = scan_block_tags(raw_code) else {
        return Vec::new();
    };
    let mut stack: Vec<(BlockKind, usize, u32)> = Vec::new();
    let mut zones: Vec<ZoneFact> = Vec::new();
    let mut next_block_id: u32 = 0;
    for tag in &tags {
        match tag.role {
            TagRole::Start { kind } => {
                let block_id = next_block_id;
                next_block_id = next_block_id.wrapping_add(1);
                stack.push((kind, tag.open, block_id));
            }
            TagRole::End { kind: end_kind } => {
                match stack.pop() {
                    // The closer must close the opener's family (if↔endif,
                    // for↔endfor). A mismatch is unbalanced → fail-closed.
                    Some((open_kind, open_at, block_id)) if open_kind.closes_with(end_kind) => {
                        // A v0.1 zone is emitted ONLY for an incremental guard
                        // or a for-loop; a plain `{% if %}` pairs but emits no
                        // zone (L9). DELIBERATELY nested `if let` (NOT a
                        // `let`-chain): the prefix-scanning `byte_span` is
                        // skipped entirely for a non-emitting opener
                        // (`zone_kind()` is `None`) — a real perf win for plain
                        // `{% if %}` blocks — and it sidesteps the unstable
                        // `let_chains` feature question on MSRV 1.88. Collapsing
                        // to a `let`-chain would undo both, so the lint is
                        // suppressed at this site only.
                        #[allow(clippy::collapsible_if)]
                        if let Some(zone_kind) = open_kind.zone_kind() {
                            if let Some(raw_span) = byte_span(raw_code, open_at, tag.close) {
                                zones.push(ZoneFact {
                                    kind: zone_kind,
                                    raw_span,
                                    block_id,
                                });
                            }
                        }
                    }
                    _ => return Vec::new(),
                }
            }
            // A mid-block divider (`else`/`elif`) is valid ONLY inside an open
            // block — it must have a matching opener on the depth-stack. An
            // ORPHAN mid-tag (empty stack) is malformed Jinja → fail-closed
            // (empty vec, never letting a later zone emit from a broken stream).
            TagRole::Mid => {
                if stack.is_empty() {
                    return Vec::new();
                }
            }
            // Other tags are consumed without affecting pairing.
            TagRole::Other => {}
        }
    }
    // Unclosed openers ⇒ unbalanced ⇒ fail-closed.
    if !stack.is_empty() {
        return Vec::new();
    }
    zones
}

/// Scan `raw_code` for every `{%…%}` block tag, skipping `{{…}}` variable tags
/// and `{#…#}` comments wholesale (so an inner `{% … %}` inside a comment never
/// surfaces). Returns `None` on a malformed/unterminated tag (an opener with no
/// matching closer before EOF) — the fail-closed signal. Whitespace control
/// (`{%-`/`-%}`) and string-literal-aware `%}` skipping are handled here.
fn scan_block_tags(raw_code: &str) -> Option<Vec<BlockTag>> {
    let bytes = raw_code.as_bytes();
    let n = bytes.len();
    let mut tags: Vec<BlockTag> = Vec::new();
    let mut i = 0;
    while i < n {
        if bytes[i] == b'{' && i + 1 < n {
            match bytes[i + 1] {
                b'#' => {
                    // Comment: skip to the matching `#}` (no nesting in Jinja).
                    let close = find_close(bytes, i + 2, b'#')?;
                    i = close;
                }
                b'{' => {
                    // Variable tag: skip to `}}`, string-literal-aware.
                    let close = find_expr_close(bytes, i + 2, b'}')?;
                    i = close;
                }
                b'%' => {
                    // Block tag: find the closing `%}`, string-literal-aware.
                    let close = find_expr_close(bytes, i + 2, b'%')?;
                    let role = classify_block_tag(&raw_code[i..close]);
                    tags.push(BlockTag {
                        open: i,
                        close,
                        role,
                    });
                    i = close;
                }
                _ => i += 1,
            }
        } else {
            i += 1;
        }
    }
    Some(tags)
}

/// Find the byte index PAST a `<delim>}` closer (e.g. `#}`) starting at `from`,
/// scanning literally (comments do not respect string literals). Returns the
/// index after the closing `}`, or `None` if unterminated.
///
/// `pub(crate)` so the raw-span scanner (`raw_scan`) reuses this exact vetted
/// scanner rather than carrying a divergent copy (cute-dbt#469).
pub(crate) fn find_close(bytes: &[u8], from: usize, delim: u8) -> Option<usize> {
    let n = bytes.len();
    let mut i = from;
    while i + 1 < n {
        if bytes[i] == delim && bytes[i + 1] == b'}' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// Find the byte index PAST a `<delim>}` closer for an EXPRESSION tag (`%}` or
/// `}}`), honoring single/double-quoted string literals so a `%}`/`}}` INSIDE a
/// string does not close the tag early (§2.4). Returns the index after the
/// closing `}`, or `None` if unterminated. (The whitespace-control `-%}` is
/// covered: the `%` then `}` still close; the leading `-` is just preceding
/// content.)
///
/// `pub(crate)` so the raw-span scanner (`raw_scan`) reuses this exact vetted
/// scanner — including the Jinja backslash string-escape — rather than carrying
/// a divergent copy that previously DROPPED the escape (cute-dbt#469).
pub(crate) fn find_expr_close(bytes: &[u8], from: usize, delim: u8) -> Option<usize> {
    let n = bytes.len();
    let mut i = from;
    let mut quote: Option<u8> = None;
    while i < n {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
                // Jinja string escapes (`\'`): skip the escaped byte.
                else if b == b'\\' && i + 1 < n {
                    i += 1;
                }
            }
            None => {
                if b == b'\'' || b == b'"' {
                    quote = Some(b);
                } else if b == delim && i + 1 < n && bytes[i + 1] == b'}' {
                    return Some(i + 2);
                }
            }
        }
        i += 1;
    }
    None
}

/// Whether an `{% if … %}` condition contains an ACTUAL `is_incremental()` CALL
/// (cute-dbt#448) — an identifier-boundary, quote-aware token, NOT a bare
/// substring. The match requires:
/// - the literal token `is_incremental` with IDENTIFIER BOUNDARIES on BOTH sides
///   (the preceding/following char is not `[A-Za-z0-9_]`), so a larger
///   identifier like `some_is_incremental_flag` does NOT match;
/// - NOT inside a single/double-quoted string literal (a quoted
///   `'is_incremental'` is data, not a call);
/// - followed by optional whitespace then `(` — an actual call invocation.
///
/// A plain `{% if %}` whose condition merely embeds the substring must classify
/// as `PlainIf` (no v0.1 zone) — never a false `IncrementalGuard`.
fn mentions_is_incremental_call(cond: &str) -> bool {
    const TOKEN: &str = "is_incremental";
    let bytes = cond.as_bytes();
    // Iterate every literal occurrence of the token; return on the first that
    // is a real, unquoted, parenthesized call. Each predicate is a small
    // single-purpose helper so the boundary/quote/call logic stays low-CC.
    cond.match_indices(TOKEN).any(|(at, _)| {
        token_left_boundary_ok(bytes, at)
            && token_right_boundary_ok(bytes, at + TOKEN.len())
            && call_paren_follows(bytes, at + TOKEN.len())
            && !offset_in_string_literal(bytes, at)
    })
}

/// Whether `b` is an identifier (word) char `[A-Za-z0-9_]`.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// LEFT identifier boundary at token start `at`: the preceding byte (if any)
/// must NOT be a word char, so `some_is_incremental_flag` does not match.
fn token_left_boundary_ok(bytes: &[u8], at: usize) -> bool {
    at == 0 || !is_word_byte(bytes[at - 1])
}

/// RIGHT identifier boundary just past the token (`after`): the next byte (if
/// any) must NOT be a word char, so `is_incremental_flag` does not match.
fn token_right_boundary_ok(bytes: &[u8], after: usize) -> bool {
    after >= bytes.len() || !is_word_byte(bytes[after])
}

/// Whether an actual call `(` follows the token (skipping optional whitespace
/// past `after`) — distinguishing a call `is_incremental()` from a bare ident.
fn call_paren_follows(bytes: &[u8], after: usize) -> bool {
    let mut j = after;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    bytes.get(j) == Some(&b'(')
}

/// Whether byte offset `at` lies INSIDE a single/double-quoted string literal,
/// scanning from the start and honoring `\`-escapes — so a quoted
/// `'is_incremental'` is treated as data, not a call.
fn offset_in_string_literal(bytes: &[u8], at: usize) -> bool {
    let mut quote: Option<u8> = None;
    let mut i = 0usize;
    while i < at {
        i = step_string_scan(bytes, i, &mut quote);
    }
    quote.is_some()
}

/// Advance the quote-aware string scan one logical step from `i`, updating the
/// open-quote state, and return the next index. A `\`-escape inside a quote
/// consumes the escaped byte too.
fn step_string_scan(bytes: &[u8], i: usize, quote: &mut Option<u8>) -> usize {
    let b = bytes[i];
    match *quote {
        Some(q) => {
            if b == b'\\' && i + 1 < bytes.len() {
                return i + 2;
            }
            if b == q {
                *quote = None;
            }
        }
        None => {
            if b == b'\'' || b == b'"' {
                *quote = Some(b);
            }
        }
    }
    i + 1
}

/// Classify a full `{%…%}` tag slice by its leading keyword (after the `{%`
/// delimiter + optional `-` whitespace-control dash + whitespace). An `if`
/// whose condition makes an actual `is_incremental()` call is an
/// [`ZoneKind::IncrementalGuard`]; any other `if`/`for` is a generic
/// [`TagRole::Start`].
fn classify_block_tag(tag: &str) -> TagRole {
    // Strip `{%`, an optional `-`, and leading whitespace; strip the trailing
    // `%}`/`-%}` and trailing whitespace, to read the inner statement.
    let inner = tag
        .strip_prefix("{%")
        .unwrap_or(tag)
        .trim_start_matches('-')
        .trim_start();
    let inner = inner
        .strip_suffix("%}")
        .unwrap_or(inner)
        .trim_end()
        .trim_end_matches('-')
        .trim_end();
    // The leading keyword is the first whitespace-delimited token.
    let keyword = inner.split_whitespace().next().unwrap_or("");
    match keyword {
        "if" => {
            // An incremental guard emits a zone; any other `{% if %}` still
            // PAIRS (so nesting is honored) but emits no v0.1 zone (L9). Match an
            // ACTUAL `is_incremental()` CALL — an identifier-boundary, quote-aware
            // token — NOT a bare substring (`some_is_incremental_flag` and a
            // quoted `'is_incremental'` must NOT classify as a guard).
            let kind = if mentions_is_incremental_call(inner) {
                BlockKind::IncrementalGuard
            } else {
                BlockKind::PlainIf
            };
            TagRole::Start { kind }
        }
        "for" => TagRole::Start {
            kind: BlockKind::ForLoop,
        },
        "endif" => TagRole::End { kind: EndKind::If },
        "endfor" => TagRole::End { kind: EndKind::For },
        "else" | "elif" => TagRole::Mid,
        _ => TagRole::Other,
    }
}

/// Build a [`SourceSpan`] over the half-open byte range `[start, end)` of
/// `text`, computing honest 1-based line/col endpoints by counting newlines.
/// Returns `None` if the range is out of bounds or not on char boundaries
/// (fail-closed — the scanner never fabricates a span).
pub(crate) fn byte_span(text: &str, start: usize, end: usize) -> Option<SourceSpan> {
    if start > end || end > text.len() {
        return None;
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return None;
    }
    Some(SourceSpan {
        start: line_col_pos(text, start),
        end: line_col_pos(text, end),
    })
}

/// 1-based line / unicode-char col + 0-based byte for `offset` in `text`.
fn line_col_pos(text: &str, offset: usize) -> SourcePos {
    let prefix = &text[..offset];
    let line =
        u32::try_from(prefix.bytes().filter(|&b| b == b'\n').count() + 1).unwrap_or(u32::MAX);
    let last_line = prefix.rsplit('\n').next().unwrap_or("");
    let col = u32::try_from(last_line.chars().count() + 1).unwrap_or(u32::MAX);
    let byte = u32::try_from(offset).unwrap_or(u32::MAX);
    SourcePos { line, col, byte }
}

/// The macro's current body as plain context [`DiffLine`]s — the fallback
/// the section renders when no inline diff applies. One terminator stripped
/// (the engine-divergent normalization), then one Context line per `\n`-split
/// line. Empty body ⇒ no lines.
fn macro_body_context_lines(body: &str) -> Vec<DiffLine> {
    if body.is_empty() {
        return Vec::new();
    }
    let normalized = body.strip_suffix('\n').unwrap_or(body);
    normalized
        .split('\n')
        .map(|line| DiffLine {
            kind: DiffLineKind::Context,
            text: line.to_owned(),
            emphasis: None,
        })
        .collect()
}

/// Flatten the impacted-model blast radius into a collapsible directory
/// tree (founder D3), grouped by each model's `original_file_path`
/// directory segments.
///
/// Pre-order: every distinct directory prefix is a `dir` row (deepest level
/// per its segment depth), then the model leaf one level past its directory.
/// Deterministic — the radius is a [`BTreeSet`] (id order) and the directory
/// grouping is built over a sorted path key, so the same radius always
/// flattens to the same row sequence (golden-stable). A model with no
/// `original_file_path` groups under a synthetic `(unknown path)` directory
/// rather than being dropped (fail-open display).
fn impacted_model_tree(current: &Manifest, radius: &BTreeSet<NodeId>) -> Vec<MacroTreeRow> {
    // Sort by (directory-path, model-name) so the directory grouping is
    // stable and adjacent models in the same directory cluster.
    let mut entries: Vec<(Vec<String>, String, String)> = radius
        .iter()
        .map(|id| {
            let node = current.node(id);
            let ofp = node.and_then(Node::original_file_path);
            let (dirs, _file) = split_dir_path(ofp);
            let name = leaf_segment(id.as_str()).to_owned();
            (dirs, name, id.as_str().to_owned())
        })
        .collect();
    entries.sort();

    let mut rows = Vec::new();
    let mut open_dirs: Vec<String> = Vec::new();
    for (dirs, name, model_id) in entries {
        // Find the shared prefix with the currently-open directory stack;
        // close (drop) the divergent tail, then open the new segments.
        let shared = open_dirs
            .iter()
            .zip(dirs.iter())
            .take_while(|(a, b)| a == b)
            .count();
        open_dirs.truncate(shared);
        for segment in &dirs[shared..] {
            rows.push(MacroTreeRow {
                kind: "dir",
                label: segment.clone(),
                depth: open_dirs.len(),
                model_id: String::new(),
            });
            open_dirs.push(segment.clone());
        }
        rows.push(MacroTreeRow {
            kind: "model",
            label: name,
            depth: open_dirs.len(),
            model_id,
        });
    }
    rows
}

/// Split a model `original_file_path` into its directory segments and file
/// leaf. A `None` / empty path groups under a synthetic `(unknown path)`
/// directory (fail-open — an impacted model is never silently dropped).
fn split_dir_path(ofp: Option<&str>) -> (Vec<String>, String) {
    let path = ofp.unwrap_or("").trim_matches('/');
    if path.is_empty() {
        return (vec!["(unknown path)".to_owned()], String::new());
    }
    let mut segments: Vec<String> = path.split('/').map(str::to_owned).collect();
    let file = segments.pop().unwrap_or_default();
    (segments, file)
}

/// askama template binding for the v0.1 report.
///
/// Asset values are pinned `&'static str` constants from
/// [`asset_embed`](crate::adapters::asset_embed) so the renderer cannot
/// drift from the vendored bundle. `payload_json` is pre-escaped by
/// [`payload_json_for_html_script`] so its `|safe` interpolation cannot
/// terminate the `<script>` carrier.
#[derive(Template)]
#[template(path = "report.html", escape = "html")]
struct ReportTemplate<'a> {
    sakura_css: &'a str,
    datatables_css: &'a str,
    /// First-party chassis CSS (cute-dbt#177) — the semantic-token +
    /// theme + style-pack + density layer that fills the template's
    /// custom `<style>` block. [`REPORT_CSS`], not a vendored asset.
    report_css: &'a str,
    jquery_js: &'a str,
    datatables_js: &'a str,
    mermaid_js: &'a str,
    /// Vendored Cytoscape UMD bundle (cute-dbt#180) — the second graph
    /// engine behind the settings-panel Mermaid ⇄ Cytoscape picker
    /// (Mermaid stays the static default). [`CYTOSCAPE_JS`].
    cytoscape_js: &'a str,
    /// First-party report interaction engine (cute-dbt#178) — the
    /// model/test selectors, DAG, unified + split diff renderers, fixture
    /// grids and settings wiring that fill the template's engine
    /// `<script>` block. [`INTERACTION_JS`], not a vendored asset.
    interaction_js: &'a str,
    /// SHARED appearance engine (cute-dbt#242) — reads + applies the
    /// persisted `cute-dbt.appearance.v1` appearance on `<html>`;
    /// identical bytes on every page family. [`APPEARANCE_JS`], not a
    /// vendored asset.
    appearance_js: &'a str,
    /// Report-only appearance settings UI (cute-dbt#178, re-layered at
    /// cute-dbt#242) — the settings panel's controls + `DataTables`
    /// reflow + DAG-engine dispatch over the shared engine.
    /// [`THEME_JS`], not a vendored asset.
    theme_js: &'a str,
    /// First-party Cytoscape DAG engine (cute-dbt#180) — preset layout,
    /// canvas-text labels, hover card, click-to-highlight lineage.
    /// [`CYTO_DAG_JS`], not a vendored asset.
    cyto_dag_js: &'a str,
    favicon_data_uri: &'a str,
    /// Report title — substituted into both `<title>` (head) and
    /// `<h1>` (header). Resolved by the cli layer from
    /// `cli.config.report.title`, falling back to
    /// [`crate::domain::DEFAULT_REPORT_TITLE`] when no config is supplied.
    report_title: &'a str,
    /// Optional report subtitle (PR 14 / cute-dbt#24). When
    /// `Some(...)`, the template renders a new
    /// `<p class="report-subtitle">` element immediately after the
    /// `<h1>`. When `None`, the element is omitted entirely (no empty
    /// DOM node).
    report_subtitle: Option<&'a str>,
    /// Server-rendered banner text — a single contiguous string the
    /// `report_generation.feature` and `tests/run_loop.rs` assertions
    /// can grep against without the static HTML's span boundaries
    /// breaking the substring. JS may rewrite the `.diff-scope-text`
    /// element at boot, but the static fallback is the contract.
    banner_text: &'a str,
    /// Human-readable baseline reference (the `--baseline-manifest`
    /// path verbatim in v0.1) — rendered as plain text inside the
    /// diff-scope banner's `.diff-scope-baseline` element. Empty on the
    /// PR-diff path (the banner omits the baseline clause entirely).
    baseline_label: &'a str,
    /// `true` when the report was scoped from a PR file diff. Selects the
    /// banner's provenance clause: PR-diff → "from PR file diff";
    /// baseline → "vs baseline manifest `<label>`" (cute-dbt#85).
    is_pr_diff: bool,
    /// The source-PR reference (cute-dbt#346) — `Some` only on the PR-diff
    /// arm when a usable PR context was supplied. The banner then renders a
    /// linked `PR #<n> — <title>` clause (`<a href>` navigation, NOT a
    /// resource load — the zero-egress gate is unaffected; the title is
    /// askama-escaped). `None` ⇒ no clause, byte-identical to pre-#346.
    pr_ref: Option<&'a PrRef>,
    /// JSON payload, pre-escaped for safe interpolation inside
    /// `<script type="application/json">` via [`payload_json_for_html_script`].
    /// The template emits this with `|safe`; the safety property is the
    /// Rust-side escape, not askama's HTML filter.
    payload_json: &'a str,
    /// The server-rendered "Project definition changed" panel
    /// (cute-dbt#266) — `Some` exactly when `dbt_project.yml` is in the
    /// PR diff; `None` keeps the section out of the DOM entirely.
    project_panel: Option<ProjectPanelView>,
    /// `true` when this report emitted any VISIBLE project-state surface
    /// (cute-dbt#292): the panel, per-model config-provenance chips, or
    /// var-reference chips. Gates the settings panel's project-state
    /// display-toggle row (the `is_pr_diff` askama-conditional
    /// precedent) — a report with nothing to toggle renders no row.
    /// Standing `definition` metadata alone stays `false`: it is
    /// payload-only (no DOM), so a display toggle would be inert.
    has_project_state: bool,
    /// `true` when the governance facts carry any visible surface
    /// (cute-dbt#260) — a group chip (Slice 0) or a blast-radius
    /// statement (Slice 1). Gates the `{%- if has_governance %}` section
    /// so an empty payload (the off-gate default) emits zero DOM, keeping
    /// the non-experimental golden byte-identical.
    has_governance: bool,
    /// The server-rendered governance facts (cute-dbt#260): the
    /// group/owner header chips (Slice 0) + the reverse-reachability
    /// blast-radius statements (Slice 1). The `{%- if has_governance %}`
    /// section reads `governance.group_chips` + `governance.blast_radius`;
    /// the same struct rides the JSON payload for downstream consumers +
    /// headless assertions.
    governance: GovernanceFacts,
    /// The macro perspective lens (cute-dbt#265, Slice B) — `Some` exactly
    /// when [`Experiment::MacroLens`](crate::domain::Experiment::MacroLens)
    /// is on AND a root-project macro changed. The `{% if macro_lens %}`
    /// section reads each `macros[i]` view (the body diff/lines + the
    /// impacted-model directory tree + the count + the fidelity chip);
    /// `None` emits zero bytes (the off-gate default), keeping the
    /// non-macro goldens byte-identical. Rides the JSON payload too (the
    /// `governance` both-surfaces precedent).
    macro_lens: Option<MacroLensPayload>,
    /// The PR-scope lineage mini-DAG (cute-dbt#404, epic #352) — `Some`
    /// exactly when [`Experiment::PrScopeMiniDag`](crate::domain::Experiment::PrScopeMiniDag)
    /// is on AND the scope set is non-empty. The `{% match pr_dag %}` section
    /// reads the per-state counts (the descriptor line) + the `collapsed` flag
    /// (the size-bound: a summary line replaces the inline graph). The graph
    /// itself renders client-side (Mermaid, the static default) from the JSON
    /// payload (`DATA.pr_dag.graph`); the server side carries the descriptor +
    /// the static Mermaid host. `None` emits zero bytes (the off-gate default),
    /// keeping the non-experimental goldens byte-identical (the `macro_lens`
    /// precedent). Rides the JSON payload too. The settings panel's mini-DAG
    /// display-toggle row gates on `pr_dag.is_some()` directly (an askama
    /// method call), so no separate `has_pr_dag` bool is needed — keeping the
    /// template's bool count under the clippy `struct_excessive_bools` ceiling.
    pr_dag: Option<PrDagPayload>,
}

/// Serialize `payload` to JSON for safe embedding inside an HTML
/// `<script type="application/json">` block.
///
/// HTML5's script-data state terminates on `</` followed by an ASCII
/// alpha character; the script-data-double-escape state begins on
/// `<!--`. A naive `serde_json` serialization of a payload containing
/// `</script>` or `<!--` would allow a manifest-derived string to
/// break out of the JSON carrier.
///
/// Escape `<` to its `<` Unicode form whenever it is followed by
/// `/`, `!`, `?`, `=`, or an ASCII letter — every tag-opening shape
/// plus the scanner-hostile `<=`. `</` and `<!--` are the sequences
/// that matter under HTML5's script-data state machine; the `<letter` /
/// `<?` forms are inert in a real browser but read as markup to
/// non-HTML5 tag scanners (cute-dbt#170: the check-spec catalog put
/// prose like `WHERE <right>.<key> IS NULL` on the wire, which the
/// `tl`-based test extractors parsed as a tag, corrupting payload
/// extraction). `<=` joined the set at cute-dbt#200: authored model /
/// column descriptions carry SQL-ish prose like `encounter_start_at <=
/// current_timestamp` (the committed playground fixture), which `tl`
/// also mis-scans. A bare `<` followed by a space or digit
/// (compiled-SQL comparisons) stays raw. The `\uXXXX` form is a
/// documented JSON escape (RFC 8259 §7) so the output remains a valid
/// JSON document that `JSON.parse(...)` decodes back to the original
/// characters.
///
/// # Errors
///
/// Returns the underlying [`serde_json::Error`] if serialization
/// fails — unreachable in practice for the payload shapes this module
/// emits (all fields are concrete `Serialize` types known at compile
/// time).
fn payload_json_for_html_script(payload: &ReportPayload) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string(payload)?;
    let mut out = String::with_capacity(json.len() + 16);
    let mut chars = json.chars().peekable();
    while let Some(c) = chars.next() {
        let tag_opener = matches!(
            chars.peek(),
            Some('/' | '!' | '?' | '=' | 'a'..='z' | 'A'..='Z')
        );
        if c == '<' && tag_opener {
            out.push_str("\\u003c");
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

/// Compose the banner text rendered into the diff-scope section.
///
/// Empty `models_in_scope` → exactly `BANNER_EMPTY_SCOPE` (the locked
/// `"0 unit tests in scope"` contract string from
/// [`crate::domain::state`]). Non-empty → `"Showing N unit test(s) in scope"`
/// — the same wording the design's JS produces at boot.
#[must_use]
pub fn compose_banner_text(in_scope: &InScopeSet) -> String {
    if in_scope.is_empty() {
        BANNER_EMPTY_SCOPE.to_owned()
    } else {
        let n = in_scope.len();
        let noun = if n == 1 { "unit test" } else { "unit tests" };
        format!("Showing {n} {noun} in scope")
    }
}

/// Build the full report payload from the run loop's outputs.
///
/// `baseline_label` is interpolated verbatim into the diff-scope banner
/// (e.g. the `--baseline-manifest` path string). The payload's
/// `models` list mirrors `models_in_scope` order (deterministic
/// `BTreeSet` traversal). Each in-scope model carries **all** the unit
/// tests targeting it (via [`index_tests_for_models`]), not just the
/// in-scope ones — so the report's All-tests mode and per-model total
/// count work (cute-dbt#91 widening). Each test is tagged `changed`
/// (updated) when its id is in the `changed` set, else context. A model
/// not present in `current.nodes()` is silently skipped — the comparator
/// should not have surfaced it, but belt-and-braces.
///
/// `changed` sits in position 2 (where the dropped in-scope set used to
/// be) so the widening leaves existing call sites textually stable; the
/// in-scope set is no longer consumed here (the banner reads it in
/// [`render_report`]).
#[must_use]
// `authoring_yaml` is always built by the cli layer with the default
// hasher; clippy::implicit_hasher would have us generalize over
// BuildHasher for no real-world benefit.
#[allow(clippy::implicit_hasher)]
pub fn build_payload(
    current: &Manifest,
    changed: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    model_yaml: &HashMap<String, ModelYamlOutcome>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    baseline_label: &str,
) -> ReportPayload {
    build_payload_with_externals(
        current,
        changed,
        models_in_scope,
        authoring_yaml,
        yaml_diffs,
        sql_diffs,
        model_yaml,
        data_diffs,
        &HashMap::new(),
        baseline_label,
        &CheckPolicy::default(),
        &ProjectFacts::default(),
        // cute-dbt#413 — the no-axes convenience: baseline mode + every
        // explore/test path with no per-axis attribution passes an empty
        // map, so `ModelPayload::axes` / `config_file` stay omitted.
        &BTreeMap::new(),
        // cute-dbt#416 — the no-state convenience: empty `model_states` map,
        // so `ModelPayload::state` stays omitted (the `axes` precedent).
        &BTreeMap::new(),
    )
}

/// Like [`build_payload`] but inlines any external fixture files read for
/// each in-scope test (cute-dbt#126). `external_fixtures` is keyed by test
/// id; an absent entry (or an absent given ordinal within it) leaves that
/// given/expect on its inline-manifest path. The cli's run loop builds the
/// map from the `ProjectFileReader`; baseline mode + every render path with
/// no external `fixture:` files use the [`build_payload`] convenience.
///
/// `check_policy` (cute-dbt#171) is the resolved display policy applied to
/// each model's findings AFTER supersedes resolution (the cli builds it
/// from `--config` `[checks]` + scanned SQL pragmas; the [`build_payload`]
/// convenience passes `CheckPolicy::default()` — everything displayed,
/// nothing suppressed).
///
/// `project_facts` (cute-dbt#266) carries the parsed `dbt_project.yml`
/// (standing metadata) + the diff-gated panel content; the
/// [`build_payload`] convenience passes `ProjectFacts::default()` (both
/// absent — the pre-#266 payload shape, byte-identical).
#[must_use]
// `too_many_arguments`: see `render_report` — the composition root threads
// the run loop's already-built artifacts straight through.
#[allow(clippy::implicit_hasher, clippy::too_many_arguments)]
pub fn build_payload_with_externals(
    current: &Manifest,
    changed: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    model_yaml: &HashMap<String, ModelYamlOutcome>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    external_fixtures: &HashMap<String, ExternalFixtures>,
    baseline_label: &str,
    check_policy: &CheckPolicy<HeuristicId>,
    project_facts: &ProjectFacts,
    axes: &BTreeMap<NodeId, ChangeAxes>,
    model_states: &BTreeMap<NodeId, ModelState>,
) -> ReportPayload {
    let model_tests = index_tests_for_models(current, models_in_scope);
    let empty: Vec<(&str, &UnitTest)> = Vec::new();
    let mut models = Vec::new();
    for model_id in models_in_scope.iter() {
        let Some(model) = current.node(model_id) else {
            continue;
        };
        let tests = model_tests.get(model_id).unwrap_or(&empty).as_slice();
        let mut model_payload = build_model_payload(
            current,
            model,
            tests,
            changed,
            authoring_yaml,
            yaml_diffs,
            sql_diffs,
            model_yaml,
            data_diffs,
            external_fixtures,
            check_policy,
        );
        // cute-dbt#267 — the model-row provenance chips: which
        // dbt_project.yml subtree edit put (or kept) this model in scope.
        if let Some(attributions) = project_facts.config_attributions.get(model_id.as_str()) {
            model_payload.config_attributions.clone_from(attributions);
        }
        // cute-dbt#268 — the var-reference chips: which edited vars this
        // (already in-scope) model references. Context, never scope.
        if let Some(references) = project_facts.var_references.get(model_id.as_str()) {
            model_payload.var_references.clone_from(references);
        }
        // cute-dbt#413 — the per-model axis chips + the config-file
        // (optgroup grouping key). Present only on the `--pr-diff` arm: the
        // baseline arm produces an empty `axes` map (the documented
        // Option-A gap in scope.rs), so this whole block is skipped and the
        // baseline goldens stay byte-identical. The config-file is gated to
        // the SAME axes-present models — a model's `patch_path` is read off
        // the manifest node (the model_yaml `path` precedent), but only
        // surfaced as the grouping key when the model carries axis
        // attribution, so the new fields never appear on a baseline payload.
        if let Some(model_axes) = axes.get(model_id) {
            model_payload.axes = Some(AxesPayload::from(*model_axes));
            model_payload.config_file = model.patch_path().map(str::to_owned);
        }
        // cute-dbt#416 — the per-model NEW/MODIFIED state chip. Present only
        // on the `--pr-diff` arm (the `model_states` map is empty in baseline
        // mode, the `axes` Option-A gap), so this is skipped and baseline
        // goldens stay byte-identical. NEW renders an extra state chip;
        // MODIFIED carries no chip (the axis chips already say MODIFIED), but
        // is serialized so the wire form is unambiguous.
        if let Some(state) = model_states.get(model_id) {
            model_payload.state = Some(model_state_wire(*state));
        }
        models.push(model_payload);
    }
    // cute-dbt#170 — the spec catalog covers exactly the checks that
    // fired (suppressed findings included: they render, quietly).
    let mut check_specs = BTreeMap::new();
    for model in &models {
        for finding in &model.findings {
            let id = finding.finding.check;
            check_specs
                .entry(id.as_str().to_owned())
                .or_insert_with(|| check_spec_payload(id));
        }
    }
    ReportPayload {
        baseline: baseline_label.to_owned(),
        models,
        // cute-dbt#200 — the model-context lookup for the shelf/hover
        // cards, scoped to the in-scope models + their tests' ref()-ed
        // upstreams.
        manifest_nodes: build_manifest_nodes(current, models_in_scope, &model_tests),
        check_specs,
        // cute-dbt#266 — standing metadata + the diff-gated panel facts.
        project_definition: project_facts.definition.clone(),
        project_change_panel: project_facts.panel.clone(),
        // cute-dbt#260 — governance facts default empty here; the gated
        // value is threaded in by `render_report_with_externals` (the
        // group chips are payload-level, not per-model, so they assemble
        // at the composition root, not inside the per-model walk).
        governance: GovernanceFacts::default(),
        // cute-dbt#265 — macro lens defaults `None` here; the gated value
        // is threaded in by `render_report_with_externals` (the macro
        // section is payload-level, not per-model — it assembles at the
        // composition root from the changed-macro set + blast radius).
        macro_lens: None,
        // cute-dbt#346 — the source-PR ref defaults `None` here; the
        // resolved value (PrDiff arm only) is threaded in by
        // `render_report_with_externals` from the cli's `--pr-*` / `[pr]` /
        // `review`-derived inputs.
        pr_ref: None,
        // cute-dbt#350 — seed cards default empty here; the gated value
        // (the CLI's working-tree-read `gather_seeds` output) is threaded
        // in by `render_report_with_externals`, mirroring `governance` /
        // `macro_lens` / `pr_ref`. Empty ⇒ omitted from JSON, so seed-free
        // payloads stay byte-identical.
        seed_cards: Vec::new(),
        // cute-dbt#404 — the PR-scope mini-DAG defaults `None` here; the gated
        // value (the cli's `gather_pr_dag` output) is threaded in by
        // `render_report_with_externals`, mirroring `macro_lens`. `None` ⇒
        // omitted from JSON + zero DOM via `{% match pr_dag %}`, keeping the
        // non-experimental goldens byte-identical.
        pr_dag: None,
        // cute-dbt#416 — the REMOVED model paths default empty here; the
        // value (the cli's `removed_models`) is threaded in by
        // `render_report_with_externals` (it is a payload-level summary, not
        // per-model — removed models are node-less). Empty ⇒ omitted from JSON,
        // keeping non-removal goldens byte-identical.
        removed_models: Vec::new(),
        // cute-dbt#419 — the PR review-comments view defaults None here; the
        // value (the cli's `gather_pr_comments` → `group_comment_threads`
        // output) is threaded in by `render_report_with_externals` exactly
        // like `pr_dag`. None ⇒ omitted from JSON, keeping the
        // non-experimental / no-PR-context / no-comments goldens
        // byte-identical.
        pr_comments: None,
    }
}

/// The wire form of a [`ModelState`] for the JSON payload (cute-dbt#416) —
/// render owns the vocabulary (the `AxesPayload` precedent). `Removed` is
/// never serialized per-model (removed models are node-less and surfaced via
/// [`ReportPayload::removed_models`]); it maps to `"removed"` only for
/// completeness should a future baseline-arm slice attribute a removed node.
fn model_state_wire(state: ModelState) -> &'static str {
    match state {
        ModelState::New => "new",
        ModelState::Modified => "modified",
        ModelState::Removed => "removed",
    }
}

/// Render the report payload + asset bundle to `out`.
///
/// Returns the underlying [`io::Error`] when the output path cannot be
/// written. Template rendering itself is infallible at runtime — askama
/// generates compile-time-checked code — so a render failure surfaces
/// as a build failure, not a runtime branch.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when the rendered HTML cannot be
/// written to `out`. A template-rendering failure would surface as an
/// `io::Error` with [`io::ErrorKind::Other`] carrying the askama error
/// string; in practice the template is statically checked so this
/// branch is unreachable in v0.1 — the explicit mapping exists to keep
/// the run-loop signature monomorphic on `io::Error`.
// See `build_payload` for the rationale on the implicit-hasher allow.
// `too_many_arguments`: the render composition root threads the run-loop's
// already-built artifacts (manifest, scope sets, authoring YAML, inline
// diffs, banner) straight through; a params struct would add indirection
// that buys nothing at this single call site.
#[allow(clippy::implicit_hasher, clippy::too_many_arguments)]
pub fn render_report(
    out: &Path,
    current: &Manifest,
    in_scope: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    changed: &InScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    model_yaml: &HashMap<String, ModelYamlOutcome>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    baseline_label: &str,
    scope_source: ScopeSource,
    report_title: &str,
    report_subtitle: Option<&str>,
) -> io::Result<()> {
    render_report_with_externals(
        out,
        current,
        in_scope,
        models_in_scope,
        changed,
        authoring_yaml,
        yaml_diffs,
        sql_diffs,
        model_yaml,
        data_diffs,
        &HashMap::new(),
        baseline_label,
        scope_source,
        report_title,
        report_subtitle,
        &CheckPolicy::default(),
        &ProjectFacts::default(),
        &GovernanceFacts::default(),
        None,
        None,
        &[],
        // No seed cards flow through this convenience wrapper, so the cap is
        // inert — pass the default (cute-dbt#350).
        DEFAULT_SEED_ROW_CAP,
        // No PR-scope mini-DAG through this convenience wrapper (cute-dbt#404).
        None,
        // No per-model axis attribution through this convenience wrapper
        // (cute-dbt#413) — baseline mode + the headless render helpers pass
        // the empty map, so `axes` / `config_file` stay omitted.
        &BTreeMap::new(),
        // No per-model state + no REMOVED model paths through this convenience
        // wrapper (cute-dbt#416) — the empty map + empty slice keep `state` /
        // `removed_models` omitted (the `axes` precedent).
        &BTreeMap::new(),
        &[],
        // No PR review-comments through this convenience wrapper
        // (cute-dbt#419) — `None` keeps `pr_comments` omitted (the `pr_dag`
        // precedent).
        None,
    )
}

/// Like [`render_report`] but inlines any external fixture files read for
/// the in-scope tests (cute-dbt#126). The cli's run loop calls this with
/// the `gather_external_fixtures` map; [`render_report`] is the
/// no-external-fixtures convenience used by baseline mode + the tests.
///
/// # Errors
///
/// Same as [`render_report`]: the underlying [`io::Error`] when the
/// rendered HTML cannot be written to `out`.
#[allow(clippy::implicit_hasher, clippy::too_many_arguments)]
pub fn render_report_with_externals(
    out: &Path,
    current: &Manifest,
    in_scope: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    changed: &InScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    model_yaml: &HashMap<String, ModelYamlOutcome>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    external_fixtures: &HashMap<String, ExternalFixtures>,
    baseline_label: &str,
    scope_source: ScopeSource,
    report_title: &str,
    report_subtitle: Option<&str>,
    check_policy: &CheckPolicy<HeuristicId>,
    project_facts: &ProjectFacts,
    governance: &GovernanceFacts,
    macro_lens: Option<&MacroLensPayload>,
    pr_ref: Option<&PrRef>,
    seed_cards: &[SeedCard],
    seed_row_cap: usize,
    pr_dag: Option<&PrDagPayload>,
    axes: &BTreeMap<NodeId, ChangeAxes>,
    model_states: &BTreeMap<NodeId, ModelState>,
    removed_models: &[String],
    pr_comments: Option<&CommentsView>,
) -> io::Result<()> {
    let mut payload = build_payload_with_externals(
        current,
        changed,
        models_in_scope,
        authoring_yaml,
        yaml_diffs,
        sql_diffs,
        model_yaml,
        data_diffs,
        external_fixtures,
        baseline_label,
        check_policy,
        project_facts,
        axes,
        model_states,
    );
    // cute-dbt#260 — the gated governance facts (group chips + blast
    // radius + Slice 2's contract classifications, all built in
    // `gather_governance`). Empty (the off-gate default) ⇒ omitted from
    // JSON + zero DOM via `{%- if has_governance %}`, keeping the
    // non-experimental golden byte-identical.
    payload.governance = governance.clone();
    // cute-dbt#265 — the gated macro lens (changed macros + body diffs +
    // impacted-model trees, all built in `build_macro_lens`). `None` (the
    // off-gate default, or no macro changed) ⇒ omitted from JSON + zero DOM
    // via `{% if macro_lens %}`, keeping the non-macro goldens byte-identical.
    payload.macro_lens = macro_lens.cloned();
    // cute-dbt#346 — the source-PR ref is a PR-DIFF banner clause only: the
    // baseline arm has no "from PR file diff" provenance to anchor a PR link
    // to. Gating here (not at the cli) keeps the link strictly tied to the
    // banner arm that renders it, so a stray `[pr]` config on a baseline run
    // is inert rather than rendering a dangling link with no diff context.
    let pr_ref = pr_ref.filter(|_| scope_source == ScopeSource::PrDiff);
    payload.pr_ref = pr_ref.map(PrRefPayload::from);
    // cute-dbt#350 — the "Data tables" seed render views, built from the raw
    // cards the CLI gathered (identity + lineage + working-tree CSV table +
    // pr-diff cell-diff) plus the `--config`-resolved row cap. The cap bounds
    // the current table render-side here (the cell-diff is never capped); the
    // honest "showing N of M rows" label is precomputed per card. Gated
    // behind the `seeds` experiment: the cli passes an EMPTY raw vec when off,
    // so this view is empty ⇒ omitted from JSON ⇒ the section renders zero DOM
    // and every seed-free golden stays byte-identical (the macro_lens / governance
    // precedent).
    payload.seed_cards = build_seed_section(seed_cards, seed_row_cap);
    // cute-dbt#404 — the gated PR-scope mini-DAG (modified ∪ connectors ∪
    // deleted, with per-node line counts + the size-bound collapse decision,
    // all built in the cli's `gather_pr_dag`). `None` (the off-gate default,
    // or an empty scope) ⇒ omitted from JSON + zero DOM via `{% match pr_dag %}`,
    // keeping the non-experimental goldens byte-identical (the `macro_lens`
    // precedent).
    payload.pr_dag = pr_dag.cloned();
    // cute-dbt#416 — the node-less REMOVED model paths, threaded in here (a
    // payload-level summary, not per-model). Empty on the baseline arm + any
    // addition/removal-free PR ⇒ omitted from JSON, keeping non-removal
    // goldens byte-identical (the `seed_cards` / `pr_dag` precedent).
    payload.removed_models = removed_models.to_vec();
    // cute-dbt#419 — the gated PR review-comments view (anchored review
    // threads grouped per model, built in the cli's `gather_pr_comments`).
    // `None` (the off-gate default, no PR context, or no comments) ⇒ omitted
    // from JSON, so the static count container stays empty (the JS never fills
    // it) and every default golden stays byte-identical (the `pr_dag` /
    // `seed_cards` precedent). The whole comment surface renders client-side
    // from `DATA.pr_comments`, so there is no server-rendered template field.
    payload.pr_comments = pr_comments.cloned();
    // The empty-scope banner contract reads the TRUE in-scope set, not the
    // widened render set or the changed subset (cute-dbt#91).
    let banner_text = compose_banner_text(in_scope);
    let payload_json = payload_json_for_html_script(&payload)
        .map_err(|err| io::Error::other(format!("payload serialization: {err}")))?;
    let template = ReportTemplate {
        sakura_css: SAKURA_CSS,
        datatables_css: DATATABLES_CSS,
        report_css: REPORT_CSS,
        jquery_js: JQUERY_JS,
        datatables_js: DATATABLES_JS,
        mermaid_js: MERMAID_JS,
        cytoscape_js: CYTOSCAPE_JS,
        interaction_js: INTERACTION_JS,
        appearance_js: APPEARANCE_JS,
        theme_js: THEME_JS,
        cyto_dag_js: CYTO_DAG_JS,
        favicon_data_uri: FAVICON_DATA_URI,
        report_title,
        report_subtitle,
        banner_text: &banner_text,
        baseline_label,
        is_pr_diff: scope_source == ScopeSource::PrDiff,
        pr_ref,
        payload_json: &payload_json,
        project_panel: project_facts
            .panel
            .as_ref()
            .map(|panel| project_panel_view(panel, &project_facts.config_attributions)),
        has_project_state: project_facts.panel.is_some()
            || !project_facts.config_attributions.is_empty()
            || !project_facts.var_references.is_empty(),
        has_governance: governance.has_content(),
        governance: governance.clone(),
        macro_lens: macro_lens.cloned(),
        pr_dag: pr_dag.cloned(),
    };
    let html = template
        .render()
        .map_err(|err| io::Error::other(format!("render: {err}")))?;
    fs::write(out, html)
}

/// Build a [`ModelPayload`] for one in-scope model. `current` is the
/// whole manifest — needed to resolve each given's input model and the
/// column-scoped tests for the cute-dbt#165 column-header metadata.
#[allow(clippy::too_many_arguments)] // mirrors render_report's rationale
fn build_model_payload(
    current: &Manifest,
    model: &Node,
    tests: &[(&str, &UnitTest)],
    changed: &InScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    model_yaml: &HashMap<String, ModelYamlOutcome>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    external_fixtures: &HashMap<String, ExternalFixtures>,
    check_policy: &CheckPolicy<HeuristicId>,
) -> ModelPayload {
    let bare_name = leaf_segment(model.id().as_str()).to_owned();
    let compiled_code = model.compiled_code().unwrap_or_default();
    let graph = parse_cte_graph(compiled_code).unwrap_or_default();
    let is_recursive = graph.is_recursive();
    let nodes = build_node_payloads(&graph, &bare_name);
    let edges = build_edge_payloads(&graph);
    // cute-dbt#445 — the source map is the single source of truth: assemble it
    // once from the parsed graph + the faithful compiled text, then DERIVE
    // `compiled_sql` (the per-node slices) and project `code_map` from it. The
    // domain assembler emits one CteBody entry per node (and synthesizes the
    // lone terminal entry for a WITH-less model), so the derived keys agree
    // with the DAG by construction.
    let mut source_map = SourceMap::from_cte_graph(&graph, compiled_code, TERMINAL_NODE_NAME);
    // cute-dbt#448 (Z1+Z2) — raw-Jinja zones: scan the model's raw_code for
    // {% if is_incremental() %} / {% for %} control zones, resolve each into a
    // SpanRole::Zone SourceMapEntry (compiled-span by honest token-location,
    // None ⇒ pruned this build), and append to the spine. The presence verdict
    // + node back-ref are derived downstream in gather_raw_zones. Only runs when
    // there is both a source map (compiled present) AND raw_code to scan.
    // DELIBERATELY nested `if let` (NOT a `let`-chain) — the model's `raw_code`
    // is only read when a source map exists, and this sidesteps the unstable
    // `let_chains` feature question on MSRV 1.88 while keeping behavior
    // identical. Collapsing to a `let`-chain would reintroduce that question, so
    // the lint is suppressed at this site only.
    #[allow(clippy::collapsible_if)]
    if let Some(sm) = source_map.as_mut() {
        if let Some(raw) = model.raw_code().filter(|s| !s.is_empty()) {
            append_zone_entries(sm, raw, compiled_code);
            // cute-dbt#469 (S1) — fill the RAW span of every NON-zone CteBody /
            // Column entry whose name resolves to a UNIQUE lexical anchor in the
            // Jinja-masked raw text (the raw twin of the compiled node_spans /
            // column_spans). Runs AFTER append_zone_entries so the zone path's
            // raw spans are never overwritten (fill_raw_spans skips any entry
            // that already carries a raw span); fail-closed on malformed Jinja.
            crate::adapters::raw_scan::fill_raw_spans(sm, raw);
        }
    }
    let compiled_sql = build_compiled_sql(source_map.as_ref());
    let code_map = source_map.as_ref().map(CodeMapPayload::from_source_map);
    // cute-dbt#446 (CLL-1) — the Tier-2 column-lineage context: a pure
    // manifest fold (per-column definition + tested-by). `context`-only
    // (no edges yet); omitted from the wire when the model has no documented
    // or tested columns, so pre-#446 goldens stay byte-stable.
    // cute-dbt#446 (CLL-1) context + cute-dbt#447 (CLL-2) edges. The edges are
    // the engine's intra-model column-provenance facts (pass-through / rename),
    // read off the already-parsed graph — never a second parse. The whole
    // section is omitted when BOTH halves are empty so pre-#446 goldens stay
    // byte-stable.
    let column_lineage = {
        let context = column_contexts(current, model);
        let edges = graph.column_edges().to_vec();
        if context.is_empty() && edges.is_empty() {
            None
        } else {
            Some(ColumnLineagePayload { context, edges })
        }
    };
    let raw_sql = model
        .raw_code()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    // SQL diff is keyed by the model's FULL node id (the
    // `reconstruct_model_sql_diffs` key), not the bare name.
    let sql_diff = sql_diffs.get(model.id().as_str()).cloned();
    // cute-dbt#247 — the Model-YAML gather outcome is keyed by the FULL
    // node id too (the `gather_model_yaml` key); the payload mapping
    // resolves it to the sliced block or a truthful degrade placeholder.
    let model_yaml = model_yaml_payload(model_yaml.get(model.id().as_str()), &bare_name);
    // cute-dbt#165 — the target model's full column-metadata map, built
    // once and shared by every test's expect table (and `this` givens).
    let target_column_meta = column_meta_for_model(current, model);
    let test_payloads = tests
        .iter()
        .map(|(id, ut)| {
            build_test_payload(
                id,
                ut,
                &graph,
                changed,
                authoring_yaml.get(*id),
                yaml_diffs.get(*id),
                data_diffs.get(*id),
                external_fixtures.get(*id),
                current,
                &target_column_meta,
            )
        })
        .collect();
    ModelPayload {
        name: bare_name,
        // cute-dbt#179 — the full project-relative source path for the
        // Model-SQL code-card header (None on synthetic manifests; the
        // template JS falls back to `<name>.sql`).
        path: model.original_file_path().map(str::to_owned),
        // cute-dbt#200 — the authored model description (None — key
        // omitted — for undescribed models, so pre-#200 payloads stay
        // byte-stable).
        description: model.description().map(str::to_owned),
        dag: DagPayload { nodes, edges },
        compiled_sql,
        // cute-dbt#445 — the source-map projection (None for a model with no
        // compiled code; the key is omitted so older fixtures stay byte-stable).
        code_map,
        // cute-dbt#446 — the Tier-2 column-lineage context (None when the
        // model has no documented or tested columns; key omitted so pre-#446
        // goldens stay byte-stable).
        column_lineage,
        raw_sql,
        sql_diff,
        model_yaml,
        tests: test_payloads,
        is_recursive,
        is_incremental: model.config().materialized() == Some("incremental"),
        // cute-dbt#169 — the check engine runs here, during payload
        // assembly over models_in_scope (the parse_ctes precedent: the
        // run loop's per-model work happens one stage downstream). The
        // already-parsed graph rides along so graph-fact checks
        // (cute-dbt#172) reuse the single parse pass.
        // cute-dbt#171 — the display policy applies strictly AFTER
        // model_findings' evaluate-all → resolve-supersedes pipeline:
        // selection removes, suppression marks (reason rides into the
        // payload). The default policy is a no-op.
        // cute-dbt#170 — each finding is then wrapped into its render
        // shape (pin target resolved against the parsed graph; sketch
        // evidence lifted into copyable code blocks).
        findings: apply_check_policy(model_findings(current, model, Some(&graph)), check_policy)
            .into_iter()
            .map(|finding| finding_payload(&graph, finding))
            .collect(),
        // cute-dbt#267 / #268 — attached by build_payload_with_externals
        // from ProjectFacts (this builder never sees the gather stage's
        // facts).
        config_attributions: Vec::new(),
        var_references: Vec::new(),
        // cute-dbt#413 — the axis attribution + config-file are attached by
        // build_payload_with_externals from the threaded `axes` map (this
        // builder never sees the scope selection's per-axis record).
        axes: None,
        config_file: None,
        // cute-dbt#416 — the NEW/MODIFIED state is attached by
        // build_payload_with_externals from the threaded `model_states` map.
        state: None,
    }
}

/// Build [`NodePayload`]s for every graph node.
///
/// `id` is always the engine's node name (stable + unique within a model);
/// the terminal node's display [`label`](NodePayload::label) is the model's
/// file name (`<model>.sql`), never its id — so a CTE that shares the
/// model's name neither collapses into the terminal (distinct ids) nor
/// reads ambiguously on the graph (the `.sql` suffix marks the model's own
/// final select apart from a same-named import CTE) (cute-dbt#155).
fn build_node_payloads(graph: &CteGraph, model_name: &str) -> Vec<NodePayload> {
    graph
        .nodes()
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            let role = classify_node_role(graph, idx);
            let label = (role == NodeRole::Final).then(|| format!("{model_name}.sql"));
            NodePayload {
                id: node.name().to_owned(),
                label,
                role,
            }
        })
        .collect()
}

/// Build [`EdgePayload`]s, keyed by each endpoint's stable node id.
fn build_edge_payloads(graph: &CteGraph) -> Vec<EdgePayload> {
    graph
        .edges()
        .iter()
        .map(|edge| EdgePayload {
            from: endpoint_id(graph, edge.from()),
            to: endpoint_id(graph, edge.to()),
            edge_type: edge.edge_type(),
        })
        .collect()
}

/// Resolve a graph-node index to its stable rendered id — the engine's
/// node name (a CTE alias, or [`TERMINAL_NODE_NAME`] for the terminal).
fn endpoint_id(graph: &CteGraph, index: usize) -> String {
    graph
        .nodes()
        .get(index)
        .map(|node| node.name().to_owned())
        .unwrap_or_default()
}

/// Build the `compiled_sql` map: per-node compiled SQL keyed by the stable
/// node id (a CTE alias, or [`TERMINAL_NODE_NAME`] for the terminal).
///
/// DERIVED (cute-dbt#445) from the per-model [`SourceMap`] — the single source
/// of truth: `compiled_sql[id]` is `compiled[node_spans[id].byte_range()]`,
/// byte-equal to the legacy per-node `raw_sql` slice by the S1 contract. The
/// WITH-less model is ONE terminal `CteBody` entry over the whole text, so it
/// keys by [`TERMINAL_NODE_NAME`] (NOT the model's bare name as v1's
/// empty-graph branch did — the cute-dbt#445 key fix).
///
/// `source_map` is `None` ONLY when the model has no compiled code at all
/// (a seed/source — [`SourceMap::from_cte_graph`] returns `None` solely on an
/// empty compiled string), so there is no slice to surface: the map is empty.
fn build_compiled_sql(source_map: Option<&SourceMap>) -> BTreeMap<String, String> {
    match source_map {
        Some(sm) => sm.compiled_slices(),
        None => BTreeMap::new(),
    }
}

/// One external fixture file loaded for a given/expect (cute-dbt#126).
///
/// Produced by the cli `gather_external_fixtures` step from the
/// `ProjectFileReader` port; consumed by `build_test_payload` to
/// **inline** the file content into the render payload so an external
/// fixture renders identically to an inline one. `text` is the raw file
/// body (becomes the payload `rows` String — drives the sql code-block
/// fallback + suppresses the external-fixture affordance); `table` is the
/// parsed grid POD (`None` for a non-literal sql file → code block);
/// `format` is the *effective* format (manifest `format:`, else derived
/// from the path extension) so the template's format-aware branches behave
/// even when an engine omits `format` on an external fixture.
#[derive(Debug, Clone)]
pub struct LoadedFixture {
    /// Raw file body read from the working tree.
    pub text: String,
    /// Effective `format` (manifest value or extension-derived).
    pub format: Option<String>,
    /// Parsed grid, or `None` for a non-literal-sql / non-tabulatable file.
    pub table: Option<FixtureTable>,
}

/// The external fixtures successfully READ for one unit test (cute-dbt#126).
///
/// `given` is keyed by the given's **source ordinal** (its position in the
/// test's `given:` list) — the same identity the cell-diff binds on
/// (cute-dbt#131) — because a test may mix inline and external givens, and
/// two givens may share a fixture path. Only successfully-read external
/// fixtures appear: an unreadable one is simply absent, so the payload
/// keeps `rows: null` + `fixture` and the template shows the #98 affordance.
#[derive(Debug, Clone, Default)]
pub struct ExternalFixtures {
    /// Loaded external givens, keyed by source ordinal.
    pub given: BTreeMap<usize, LoadedFixture>,
    /// The loaded external `expect`, when present.
    pub expect: Option<LoadedFixture>,
}

/// Resolve a given/expect's `(rows, table, format)` payload triple —
/// inlining a loaded external fixture (cute-dbt#126) when one was read,
/// else the inline manifest values. With a [`LoadedFixture`] the payload
/// `rows` carries the file text (so the template renders it like an inline
/// fixture and the external affordance is suppressed) and `table`/`format`
/// come from the load; the caller retains `fixture` as provenance.
fn resolve_fixture_payload(
    rows: &Value,
    format: Option<&str>,
    fixture: Option<&str>,
    loaded: Option<&LoadedFixture>,
) -> (Value, Option<FixtureTable>, Option<String>) {
    match loaded {
        Some(lf) => (
            Value::String(lf.text.clone()),
            lf.table.clone(),
            lf.format.clone(),
        ),
        None => (
            rows.clone(),
            current_view_table(rows, format, fixture),
            format.map(str::to_owned),
        ),
    }
}

/// Build a single test's payload, including import-CTE binding for each
/// given. `changed` is the set of updated test ids — `id`'s membership
/// sets [`TestPayload::changed`] (cute-dbt#91). `external` carries any
/// external fixture files read for this test (cute-dbt#126), inlined into
/// the given/expect payloads. `current` + `target_column_meta` feed the
/// cute-dbt#165 column-header metadata: the expect table (and a `this`
/// given) read the target model's map; a `ref(...)` given resolves its
/// own input model's map.
#[allow(clippy::too_many_arguments)] // mirrors render_report's rationale
fn build_test_payload(
    id: &str,
    unit_test: &UnitTest,
    graph: &CteGraph,
    changed: &InScopeSet,
    authoring_yaml: Option<&UnitTestYamlBlock>,
    yaml_diff: Option<&BlockDiff>,
    data_diff: Option<&UnitTestDataDiff>,
    external: Option<&ExternalFixtures>,
    current: &Manifest,
    target_column_meta: &BTreeMap<String, ColumnMetaPayload>,
) -> TestPayload {
    let given = unit_test
        .given()
        .iter()
        .enumerate()
        .map(|(ordinal, g)| {
            // Bind the given to a leaf CTE: `ref(...)` matches directly
            // by model name; `source(...)` resolves through the
            // manifest's sources map first (cute-dbt#57) because dbt
            // pre-resolves source refs at compile time. The two arms are
            // disjoint by construction (an input parses as at most one
            // shape), so `or_else` is chain order, not precedence.
            let bound_to_node = parse_ref_name(g.input())
                .and_then(|ref_name| find_import_node_id(graph, ref_name))
                .or_else(|| {
                    parse_source_ref(g.input()).and_then(|(source_name, table_name)| {
                        find_source_import_node_id(current, graph, source_name, table_name)
                    })
                });
            let loaded = external.and_then(|e| e.given.get(&ordinal));
            let (rows, table, format) =
                resolve_fixture_payload(g.rows(), g.format(), g.fixture(), loaded);
            let is_this = g.input() == "this";
            // cute-dbt#165/#235 — the node that OWNS this given's
            // columns: the target model for `this` (prior model state),
            // the resolved refable node (model / seed / snapshot) for a
            // `ref(...)` input, or the manifest source for a
            // `source(...)` input. Unresolvable inputs contribute
            // nothing (empty map → key omitted → no trigger, never an
            // empty bubble).
            let column_meta = if is_this {
                column_meta_for_table(target_column_meta, table.as_ref())
            } else if let Some(ref_name) = parse_ref_name(g.input()) {
                resolve_given_ref_node(current, ref_name)
                    .map(|input_node| {
                        let meta = column_meta_for_model(current, input_node);
                        column_meta_for_table(&meta, table.as_ref())
                    })
                    .unwrap_or_default()
            } else if let Some((source_name, table_name)) = parse_source_ref(g.input()) {
                current
                    .source_by_name(source_name, table_name)
                    .map(|source| {
                        let meta = column_meta_for_source(current, source);
                        column_meta_for_table(&meta, table.as_ref())
                    })
                    .unwrap_or_default()
            } else {
                BTreeMap::new()
            };
            GivenPayload {
                input: g.input().to_owned(),
                bound_to_node,
                table,
                rows,
                format,
                fixture: g.fixture().map(str::to_owned),
                is_this,
                column_meta,
            }
        })
        .collect();
    let (expect_rows, expect_table, expect_format) = resolve_fixture_payload(
        unit_test.expect().rows(),
        unit_test.expect().format(),
        unit_test.expect().fixture(),
        external.and_then(|e| e.expect.as_ref()),
    );
    let expect_column_meta = column_meta_for_table(target_column_meta, expect_table.as_ref());
    TestPayload {
        id: id.to_owned(),
        name: unit_test.name().to_owned(),
        target_model: unit_test.model().as_str().to_owned(),
        changed: changed.contains(id),
        description: unit_test.description().map(str::to_owned),
        tags: unit_test.tags().map(<[String]>::to_vec),
        meta: unit_test.meta().cloned(),
        defined_in: unit_test.original_file_path().map(str::to_owned),
        authoring_yaml: authoring_yaml.map(|b| b.raw.clone()),
        yaml_diff: yaml_diff.cloned(),
        data_diff: data_diff.cloned(),
        is_incremental_mode: unit_test.is_incremental_mode(),
        // cute-dbt#200 — the full grouped overrides blob, native scalar
        // values preserved (the adapter already dropped empty groups).
        overrides: unit_test.overrides().cloned(),
        given,
        expected: ExpectedPayload {
            table: expect_table,
            rows: expect_rows,
            format: expect_format,
            fixture: unit_test.expect().fixture().map(str::to_owned),
            column_meta: expect_column_meta,
        },
    }
}

/// Compute the Current-view [`FixtureTable`] POD for a fixture's `rows` +
/// `format` (cute-dbt#138): the authoritative tabulated cells the template
/// renders directly, so the JS never parses csv/dict.
///
/// Returns `None` for the two non-tabulatable cases so the JS falls back to
/// its sql / external-fixture affordances:
///
/// 1. **External fixture** — `fixture:` is set AND `rows` is `Value::Null`
///    (the data is not in the manifest). A `Null` `rows` would otherwise
///    normalize to the *empty* table, hiding the "data in external file"
///    affordance behind a silently-empty grid (cute-dbt#126). We gate on the
///    `fixture:`+`Null` pair (NOT on `rows == Null` alone — a genuinely empty
///    inline fixture still tabulates to the empty grid).
/// 2. **non-literal sql / opaque** — [`table_from_manifest_rows`] returns
///    `None` for a sql `rows` that is NOT a literal-row `SELECT … UNION ALL`
///    (cute-dbt#137); the JS renders the sql code block. A literal-row sql
///    tabulates here just like dict/csv.
fn current_view_table(
    rows: &Value,
    format: Option<&str>,
    fixture: Option<&str>,
) -> Option<FixtureTable> {
    if fixture.is_some() && rows.is_null() {
        return None; // external fixture → JS affordance, not an empty grid
    }
    table_from_manifest_rows(rows, format)
}

/// Locate the leaf CTE node that binds to `ref_name`.
///
/// Two-pass match — both case-insensitive:
///
/// 1. **Name match** (the design's sample-data convention): an
///    import-CTE whose own name equals `ref_name`. Pass-1 is strict —
///    the node must also classify as [`NodeRole::Import`], so a
///    transform CTE that happens to share a name with the queried
///    `ref()` cannot spuriously bind.
/// 2. **Body match** (dbt's idiomatic compiled-SQL shape and the
///    multi-ref messy-import case): a leaf CTE (no incoming edges,
///    not the terminal) whose body references a table whose leaf
///    identifier equals `ref_name`. dbt-compiled SQL commonly carries
///    `with source as (select * from "db"."schema"."MODEL")`, where
///    the CTE name is the unwrapper convention (`source`, `src_*`,
///    etc.) and the model name lives only inside the body. The messy
///    import case (cute-dbt#34) further generalises this: a single
///    CTE body may reference multiple `ref()` targets (e.g. via
///    `UNION ALL` or `JOIN`), which the engine classifies as
///    `Transform` rather than `Import` because the body is not a
///    single `SELECT … FROM <relation>`. Pass-2 widens the gate to any
///    leaf node with engine-extracted
///    [`body_leaf_table_refs`](crate::domain::CteNode::body_leaf_table_refs);
///    the presence of those refs is the binding signal regardless of
///    the structural [`NodeRole`] badge the DAG renders.
///
/// Returns the matching node's name (the payload's stable node id),
/// or `None` when neither pass matches. The renderer (and the
/// template JS) call this once per `given[].input` so a unit test
/// whose multiple `ref()` givens all live inside one CTE body see
/// every given bound to that single node id (cute-dbt#34).
fn find_import_node_id(graph: &CteGraph, ref_name: &str) -> Option<String> {
    let target = ref_name.to_ascii_lowercase();
    // Pass 1: name match (design's convention; strict role gate).
    if let Some((_, node)) = graph.nodes().iter().enumerate().find(|(idx, node)| {
        node.name().eq_ignore_ascii_case(&target)
            && classify_node_role(graph, *idx) == NodeRole::Import
    }) {
        return Some(node.name().to_owned());
    }
    // Pass 2: body match — any leaf CTE with the ref in its
    // engine-extracted body_leaf_table_refs (catches dbt's `source`
    // unwrapper shape AND the messy multi-ref shape).
    for (idx, node) in graph.nodes().iter().enumerate() {
        if !is_leaf_binding_candidate(graph, idx) {
            continue;
        }
        // The engine already lowercases body_leaf_table_refs at extract
        // time (cte_engine.rs::push_leaf), so the case-fold here is
        // belt-and-braces — defends pass-2 against any future engine
        // change that ships raw-case refs, and keeps the contract
        // symmetric with pass-1's `eq_ignore_ascii_case` (Gemini PR 17).
        if node
            .body_leaf_table_refs()
            .iter()
            .any(|t| t.eq_ignore_ascii_case(&target))
        {
            return Some(node.name().to_owned());
        }
    }
    None
}

/// Locate the leaf CTE node that binds to a `source('a', 'b')` given
/// (cute-dbt#57) — the source-side sibling of [`find_import_node_id`].
///
/// dbt compiles `{{ source('a', 'b') }}` into the resolved relation at
/// `dbt compile` time, so the compiled SQL the CTE engine sees never
/// carries the literal `source(...)` form — only the resolved relation
/// (`"db"."schema"."table"`). Binding therefore walks the manifest's
/// `sources` map:
///
/// 1. Resolve `(source_name, table_name)` — the two authored `source()`
///    arguments — against [`Manifest::source_by_name`]. The lookup is on
///    `source_name` + `name`, **not** `identifier` (the args are the
///    YAML names; `identifier` may be overridden to differ).
/// 2. Derive the match token from the hit: the physical `identifier`
///    (quote-stripped — dbt preserves embedded quote characters
///    verbatim, the reserved-word `"GROUP"` case), falling back to the
///    last dot-segment of `relation_name`, falling back to `name`
///    (dbt's own identifier default).
/// 3. Feed the token through the existing two-pass
///    [`find_import_node_id`]: the CTE engine already reduces every
///    FROM/JOIN leaf to the lowercased, quote-stripped **last
///    identifier** of the relation, and the compiled-SQL relation text
///    and the manifest `relation_name` render from the same relation
///    object in both engines — so the leaf token match is exactly the
///    normalization the ref path already uses. No FQN machinery.
///
/// Returns `None` when the pair is missing from the sources map or no
/// leaf CTE references the relation — the same fail-open empty-state as
/// an unresolvable `ref` (sources need no preflight: they are referenced
/// by models, never analyzed themselves).
fn find_source_import_node_id(
    manifest: &Manifest,
    graph: &CteGraph,
    source_name: &str,
    table_name: &str,
) -> Option<String> {
    let source = manifest.source_by_name(source_name, table_name)?;
    let token = source_relation_token(source);
    if token.is_empty() {
        return None;
    }
    find_import_node_id(graph, token)
}

/// The leaf table identifier a resolved [`SourceNode`] is expected to
/// appear as inside a compiled CTE body: `identifier` when present and
/// non-empty (quote-stripped), else the last dot-segment of
/// `relation_name` (quote-stripped; naive split — a dot embedded in a
/// quoted identifier is out of scope, the identifier-first chain makes
/// this fallback rare), else the source's `name`. Returns a borrowed
/// slice of the [`SourceNode`]'s own fields — every branch is a
/// quote-strip view, so no allocation is needed (Gemini PR 204).
fn source_relation_token(source: &SourceNode) -> &str {
    let identifier = source
        .identifier()
        .map(strip_ident_quotes)
        .filter(|ident| !ident.is_empty());
    let from_relation = || {
        source
            .relation_name()
            .and_then(|relation| relation.rsplit('.').next())
            .map(strip_ident_quotes)
            .filter(|segment| !segment.is_empty())
    };
    identifier
        .or_else(from_relation)
        .unwrap_or_else(|| strip_ident_quotes(source.name()))
}

/// Strip the common SQL identifier quoting characters from both ends of
/// `s`: double quotes (ANSI / dbt `relation_name`), backticks (`BigQuery`
/// / `MySQL`), and square brackets (T-SQL). Interior characters are kept
/// verbatim; lowercasing is the caller's concern
/// ([`find_import_node_id`] case-folds both sides).
fn strip_ident_quotes(s: &str) -> &str {
    s.trim()
        .trim_matches(|c| c == '"' || c == '`')
        .trim_start_matches('[')
        .trim_end_matches(']')
}

/// `true` when the node at `index` is a candidate for pass-2 binding —
/// i.e. a leaf CTE that may carry engine-extracted body-leaf refs.
///
/// A binding candidate is any non-terminal node with zero incoming
/// edges. We deliberately do not require [`NodeRole::Import`] here:
/// the import classification narrows to single-source bodies, which
/// excludes the multi-ref shapes this PR (cute-dbt#34) needs to bind
/// (`UNION ALL`, `JOIN`, derived subqueries). Leaf-ness alone is the
/// binding contract; structural shape is the DAG-badge contract.
fn is_leaf_binding_candidate(graph: &CteGraph, index: usize) -> bool {
    let Some(node) = graph.nodes().get(index) else {
        return false;
    };
    if node.name() == TERMINAL_NODE_NAME {
        return false;
    }
    !graph.edges().iter().any(|edge| edge.to() == index)
}

/// Build a map from in-scope model id to **all** unit tests targeting it
/// in the current manifest (cute-dbt#91 widening).
///
/// Resolved via [`resolve_tested_model`] (the engine-resolved
/// `tested_node_unique_id` when present, the bare `model:` name
/// otherwise — cute-dbt#254). Unlike the prior in-scope-only indexer, this
/// enumerates every unit test whose resolved target is one of
/// `models_in_scope`, regardless of whether the test is itself in scope —
/// so a model that entered scope solely via a changed test also carries
/// its non-updated (context) siblings, making the report's All-tests mode
/// and per-model total count work.
///
/// `current.unit_tests()` is a `HashMap` with no inherent order, so each
/// model's list is sorted by unit-test id — the deterministic
/// `BTreeSet`-over-id order the prior indexer produced, which the golden
/// snapshot and the example byte-identity gate depend on.
#[must_use]
pub fn index_tests_for_models<'m>(
    current: &'m Manifest,
    models_in_scope: &ModelInScopeSet,
) -> HashMap<NodeId, Vec<(&'m str, &'m UnitTest)>> {
    let mut map: HashMap<NodeId, Vec<(&'m str, &'m UnitTest)>> = HashMap::new();
    for (test_id, unit_test) in current.unit_tests() {
        let Some(model) = resolve_tested_model(current, unit_test) else {
            continue;
        };
        if models_in_scope.contains(model.id()) {
            map.entry(model.id().clone())
                .or_default()
                .push((test_id.as_str(), unit_test));
        }
    }
    for tests in map.values_mut() {
        tests.sort_by(|a, b| a.0.cmp(b.0));
    }
    map
}

/// Final `.`-delimited segment of a node id (`model.shop.x` → `x`).
fn leaf_segment(id: &str) -> &str {
    id.rsplit('.').next().unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        BlastRadius, Checksum, ColumnMetaTags, ContractClass, ContractColumnDiff, CteEdge, CteNode,
        DEFAULT_MACRO_BODY_CAP, DEFAULT_REPORT_TITLE, DependsOn, DiffLine, DiffLineKind, EdgeType,
        FileHunks, GovChip, Group, GroupChip, Hunk, Manifest, ManifestMetadata, MetaPair,
        ModelMetaTags, NodeConfig, NodeId, Owner, PrDiff, UnitTest, UnitTestExpect, UnitTestGiven,
    };
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    // ===== parse_ref_name =====

    #[test]
    fn parse_ref_name_extracts_single_quoted_name() {
        assert_eq!(parse_ref_name("ref('stg_orders')"), Some("stg_orders"));
    }

    #[test]
    fn parse_ref_name_tolerates_surrounding_whitespace() {
        assert_eq!(parse_ref_name("  ref('a')  "), Some("a"));
    }

    #[test]
    fn parse_ref_name_accepts_case_variant_keyword() {
        assert_eq!(parse_ref_name("REF('A')"), Some("A"));
        assert_eq!(parse_ref_name("Ref('b')"), Some("b"));
        // Any byte casing matches the keyword now (case-insensitive).
        assert_eq!(parse_ref_name("rEf('c')"), Some("c"));
    }

    #[test]
    fn parse_ref_name_tolerates_whitespace_between_ref_and_paren() {
        // Jinja's `{{ ref(...) }}` macro accepts whitespace between the
        // keyword and the opening paren; the YAML-stored verbatim form
        // may carry it.
        assert_eq!(parse_ref_name("ref ('x')"), Some("x"));
        assert_eq!(parse_ref_name("REF\t('Y')"), Some("Y"));
        assert_eq!(parse_ref_name("ref   ('z')"), Some("z"));
    }

    #[test]
    fn parse_ref_name_returns_none_on_missing_parens() {
        assert_eq!(parse_ref_name("ref'stg_orders'"), None);
    }

    #[test]
    fn parse_ref_name_returns_none_on_empty_inner() {
        assert_eq!(parse_ref_name("ref('')"), None);
        assert_eq!(parse_ref_name("ref(\"\")"), None);
    }

    #[test]
    fn parse_ref_name_accepts_double_quoted_name() {
        // dbt accepts both quote styles in an authored given `input:`
        // and fusion ships the authored string VERBATIM on the manifest
        // wire (cute-dbt#245 — verified against a real fusion
        // 2.0.0-preview.177 compile of the dogfood project).
        assert_eq!(parse_ref_name("ref(\"x\")"), Some("x"));
        assert_eq!(
            parse_ref_name("ref(\"stg_payments\")"),
            Some("stg_payments")
        );
        // Same keyword/whitespace tolerances as the single-quoted form.
        assert_eq!(parse_ref_name("REF (\"Y\")"), Some("Y"));
        assert_eq!(parse_ref_name("  ref(\"a\")  "), Some("a"));
    }

    #[test]
    fn parse_ref_name_returns_none_on_mixed_quotes() {
        // Open/close must be the SAME character — a mixed pair is not a
        // valid Python/Jinja string literal. Fail-open (cute-dbt#245).
        assert_eq!(parse_ref_name("ref(\"x')"), None);
        assert_eq!(parse_ref_name("ref('x\")"), None);
    }

    #[test]
    fn parse_ref_name_returns_none_on_unmatched_quotes() {
        assert_eq!(parse_ref_name("ref('x"), None);
        assert_eq!(parse_ref_name("ref(x')"), None);
        assert_eq!(parse_ref_name("ref(\"x"), None);
        assert_eq!(parse_ref_name("ref(x\")"), None);
    }

    #[test]
    fn parse_ref_name_returns_none_on_non_ref_input() {
        assert_eq!(parse_ref_name("source('a', 'b')"), None);
        assert_eq!(parse_ref_name(""), None);
        assert_eq!(parse_ref_name("plain_table"), None);
    }

    // ===== parse_source_ref (cute-dbt#57) =====

    #[test]
    fn parse_source_ref_extracts_both_single_quoted_names() {
        assert_eq!(
            parse_source_ref("source('synthea_raw', 'patients')"),
            Some(("synthea_raw", "patients")),
        );
    }

    #[test]
    fn parse_source_ref_tolerates_whitespace_variants() {
        // Surrounding, keyword↔paren, and around-the-comma whitespace —
        // mirrors parse_ref_name's Jinja-tolerance.
        assert_eq!(parse_source_ref("  source('a', 'b')  "), Some(("a", "b")));
        assert_eq!(parse_source_ref("source ('a','b')"), Some(("a", "b")));
        assert_eq!(parse_source_ref("SOURCE\t('a' , 'b')"), Some(("a", "b")));
        assert_eq!(parse_source_ref("source( 'a','b' )"), Some(("a", "b")));
    }

    #[test]
    fn parse_source_ref_accepts_case_variant_keyword() {
        assert_eq!(parse_source_ref("SOURCE('A', 'B')"), Some(("A", "B")));
        assert_eq!(parse_source_ref("Source('a', 'b')"), Some(("a", "b")));
        assert_eq!(parse_source_ref("sOuRcE('a', 'b')"), Some(("a", "b")));
    }

    #[test]
    fn parse_source_ref_returns_none_on_missing_parens_or_arg() {
        assert_eq!(parse_source_ref("source'a','b'"), None);
        assert_eq!(
            parse_source_ref("source('a')"),
            None,
            "one arg is not a source ref"
        );
        assert_eq!(parse_source_ref("source('a', 'b'"), None);
    }

    #[test]
    fn parse_source_ref_returns_none_on_empty_names() {
        assert_eq!(parse_source_ref("source('', 'b')"), None);
        assert_eq!(parse_source_ref("source('a', '')"), None);
        assert_eq!(parse_source_ref("source('', '')"), None);
        assert_eq!(parse_source_ref("source(\"\", \"b\")"), None);
        assert_eq!(parse_source_ref("source(\"a\", \"\")"), None);
    }

    #[test]
    fn parse_source_ref_accepts_double_quoted_names() {
        // Same cute-dbt#245 evidence as parse_ref_name: both quote
        // styles are engine-valid authored forms and ship verbatim on
        // the manifest wire.
        assert_eq!(
            parse_source_ref("source(\"synthea_raw\", \"patients\")"),
            Some(("synthea_raw", "patients")),
        );
        assert_eq!(parse_source_ref("SOURCE (\"a\", \"b\")"), Some(("a", "b")));
    }

    #[test]
    fn parse_source_ref_accepts_mixed_style_args() {
        // The matching-quote rule is PER ARGUMENT — each arg is its own
        // string literal, so the two args may use different styles.
        assert_eq!(parse_source_ref("source(\"a\", 'b')"), Some(("a", "b")));
        assert_eq!(parse_source_ref("source('a', \"b\")"), Some(("a", "b")));
    }

    #[test]
    fn parse_source_ref_returns_none_on_three_argument_calls() {
        // dbt's source() takes exactly two arguments — a malformed
        // 3-arg call must reject outright, never strip the second
        // fragment to a garbage pair like ("a", "b','c") (CodeRabbit
        // PR #248). Pinned for BOTH quote styles.
        assert_eq!(parse_source_ref("source('a','b','c')"), None);
        assert_eq!(parse_source_ref("source(\"a\",\"b\",\"c\")"), None);
        assert_eq!(parse_source_ref("source('a', 'b', 'c')"), None);
        // Trailing comma = an empty third part — same rejection.
        assert_eq!(parse_source_ref("source('a','b',)"), None);
    }

    #[test]
    fn parse_source_ref_returns_none_on_mixed_quotes_within_an_arg() {
        // A mixed open/close pair inside EITHER argument fails that
        // arg's matching-quote strip — fail-open (cute-dbt#245).
        assert_eq!(parse_source_ref("source(\"a', 'b')"), None);
        assert_eq!(parse_source_ref("source('a\", 'b')"), None);
        assert_eq!(parse_source_ref("source('a', \"b')"), None);
        assert_eq!(parse_source_ref("source('a', 'b\")"), None);
    }

    #[test]
    fn parse_source_ref_returns_none_on_unmatched_quotes() {
        // A comma inside a quoted name leaves the first split fragment
        // with an unbalanced quote — both quote styles fail the strip
        // (the fail-open-by-construction property, cute-dbt#245).
        assert_eq!(parse_source_ref("source('a, 'b')"), None);
        assert_eq!(parse_source_ref("source('a', b')"), None);
        assert_eq!(parse_source_ref("source(\"a, \"b\")"), None);
        assert_eq!(parse_source_ref("source(\"a\", b\")"), None);
        assert_eq!(parse_source_ref("source(\"a,b\", \"c\")"), None);
    }

    #[test]
    fn parse_source_ref_returns_none_on_non_source_input() {
        assert_eq!(parse_source_ref("ref('stg_orders')"), None);
        assert_eq!(parse_source_ref("this"), None);
        assert_eq!(parse_source_ref(""), None);
        assert_eq!(parse_source_ref("plain_table"), None);
    }

    // ===== double-quoted given rendering (cute-dbt#245) =====

    #[test]
    fn render_report_binds_double_quoted_given_and_populates_column_meta() {
        // cute-dbt#245 AC4: fusion ships an authored `ref("…")` given
        // input VERBATIM on the manifest wire; the rendered report must
        // bind it to its import CTE (`bound_to_node`) and resolve the
        // input model's column metadata exactly like the single-quoted
        // form. Asserted end-to-end through render_report so the proof
        // covers the full payload-into-HTML path, not just the parser.
        let compiled = "with stg_payments_src as (select * from raw_payments) \
                        select * from stg_payments_src";
        let target = model_node("model.shop.orders", "body", Some(compiled));
        let mut src_desc = BTreeMap::new();
        src_desc.insert(
            "payment_id".to_owned(),
            "Double-quoted given column marker".to_owned(),
        );
        let src = model_node("model.shop.stg_payments_src", "s", Some("select 1"))
            .with_column_descriptions(src_desc);
        let ut = UnitTest::new(
            "test_dq",
            NodeId::new("orders"),
            vec![UnitTestGiven::new(
                "ref(\"stg_payments_src\")",
                json!([{ "payment_id": 1 }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![target, src], vec![("unit_test.shop.test_dq", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_dq".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_double_quoted_given_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &in_scope,
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            html.contains("\"bound_to_node\":\"stg_payments_src\""),
            "a double-quoted given binds to its import CTE in the rendered payload",
        );
        assert!(
            html.contains("Double-quoted given column marker"),
            "a double-quoted given resolves its input model's column_meta",
        );
        let _ = std::fs::remove_file(&tmp);
    }

    // ===== source_relation_token / strip_ident_quotes (cute-dbt#57) =====

    fn source_node(
        identifier: Option<&str>,
        relation_name: Option<&str>,
        name: &str,
    ) -> SourceNode {
        SourceNode::new(
            NodeId::new(format!("source.shop.raw.{name}")),
            "raw",
            name,
            identifier.map(str::to_owned),
            "main",
            Some("memory".to_owned()),
            relation_name.map(str::to_owned),
        )
    }

    #[test]
    fn source_relation_token_prefers_the_identifier() {
        let s = source_node(
            Some("patients_v2"),
            Some("\"memory\".\"main\".\"patients_v2\""),
            "patients",
        );
        assert_eq!(source_relation_token(&s), "patients_v2");
    }

    #[test]
    fn source_relation_token_strips_embedded_identifier_quotes() {
        // dbt preserves a reserved-word identifier verbatim INCLUDING
        // its quote characters (the zendesk `"GROUP"` case).
        let s = source_node(Some("\"GROUP\""), None, "group");
        assert_eq!(source_relation_token(&s), "GROUP");
    }

    #[test]
    fn source_relation_token_falls_back_to_relation_name_last_segment() {
        let s = source_node(
            None,
            Some("\"memory\".\"main\".\"patients\""),
            "patients_yaml",
        );
        assert_eq!(source_relation_token(&s), "patients");
    }

    #[test]
    fn source_relation_token_falls_back_to_the_source_name_field() {
        // The fusion-minimal entry: no identifier, no relation_name —
        // dbt defaults the physical identifier to `name`.
        let s = source_node(None, None, "patients");
        assert_eq!(source_relation_token(&s), "patients");
    }

    #[test]
    fn strip_ident_quotes_handles_each_quoting_dialect() {
        assert_eq!(strip_ident_quotes("\"orders\""), "orders");
        assert_eq!(strip_ident_quotes("`orders`"), "orders");
        assert_eq!(strip_ident_quotes("[orders]"), "orders");
        assert_eq!(strip_ident_quotes("  orders  "), "orders");
        assert_eq!(strip_ident_quotes("orders"), "orders");
    }

    // ===== bind_import_to_given =====

    fn sample_given(input: &str) -> UnitTestGiven {
        UnitTestGiven::new(input, json!([]), None, None)
    }

    fn unit_test_with_givens(givens: Vec<UnitTestGiven>) -> UnitTest {
        UnitTest::new(
            "t",
            NodeId::new("model.shop.x"),
            givens,
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
    }

    #[test]
    fn bind_import_matches_ref_name_to_cte_name() {
        let ut = unit_test_with_givens(vec![sample_given("ref('stg_orders')")]);
        let bound = bind_import_to_given(&ut, "stg_orders");
        assert!(bound.is_some());
        assert_eq!(bound.unwrap().input(), "ref('stg_orders')");
    }

    #[test]
    fn bind_import_is_case_insensitive() {
        let ut = unit_test_with_givens(vec![sample_given("ref('Stg_Orders')")]);
        assert!(bind_import_to_given(&ut, "stg_orders").is_some());
        assert!(bind_import_to_given(&ut, "STG_ORDERS").is_some());
    }

    #[test]
    fn bind_import_returns_none_on_no_match() {
        let ut = unit_test_with_givens(vec![sample_given("ref('stg_orders')")]);
        assert!(bind_import_to_given(&ut, "stg_customers").is_none());
    }

    #[test]
    fn bind_import_returns_none_on_empty_givens() {
        let ut = unit_test_with_givens(vec![]);
        assert!(bind_import_to_given(&ut, "any").is_none());
    }

    #[test]
    fn bind_import_picks_first_match_when_multiple() {
        let ut = unit_test_with_givens(vec![
            sample_given("ref('stg_orders')"),
            sample_given("ref('stg_orders')"),
        ]);
        let bound = bind_import_to_given(&ut, "stg_orders");
        assert!(bound.is_some());
    }

    // ===== classify_node_role =====

    /// Construct a transform-shaped `CteNode` for fixtures: shape facts
    /// default to `is_simple_from_shape = false` and empty refs.
    fn cte_node(name: &str, raw_sql: Option<&str>) -> CteNode {
        CteNode::new(name, None, raw_sql.map(str::to_owned), None)
    }

    /// Construct an import-shaped `CteNode` for fixtures — the renderer
    /// classifies these as [`NodeRole::Import`] when they have no
    /// incoming edges. The engine populates these facts via AST walk;
    /// tests pin them directly (cute-dbt#40).
    fn import_cte_node(name: &str, raw_sql: &str, body_leaf_refs: &[&str]) -> CteNode {
        cte_node(name, Some(raw_sql)).with_shape_facts(
            true,
            body_leaf_refs.iter().map(|s| (*s).to_owned()).collect(),
        )
    }

    #[test]
    fn classify_terminal_node_as_final() {
        let graph = CteGraph::new(
            vec![
                cte_node("stg_orders", Some("select * from x")),
                cte_node(TERMINAL_NODE_NAME, Some("select * from stg_orders")),
            ],
            vec![CteEdge::new(0, 1, EdgeType::From)],
        );
        assert_eq!(classify_node_role(&graph, 1), NodeRole::Final);
    }

    #[test]
    fn classify_simple_from_select_with_no_incoming_as_import() {
        let graph = CteGraph::new(
            vec![import_cte_node(
                "stg_orders",
                "select id from raw.orders",
                &["orders"],
            )],
            vec![],
        );
        assert_eq!(classify_node_role(&graph, 0), NodeRole::Import);
    }

    #[test]
    fn classify_node_with_incoming_edges_as_transform() {
        // Even when the node carries simple-from-shape facts, an
        // incoming edge takes precedence — the node is consumed by
        // another CTE, so it is part of the transform pipeline.
        let graph = CteGraph::new(
            vec![
                import_cte_node("a", "select 1 from x", &["x"]),
                import_cte_node("b", "select * from a", &["a"]),
            ],
            vec![CteEdge::new(0, 1, EdgeType::From)],
        );
        assert_eq!(classify_node_role(&graph, 1), NodeRole::Transform);
    }

    #[test]
    fn classify_node_with_join_as_transform() {
        // JOIN body — engine would set is_simple_from_shape=false;
        // the default `cte_node` helper preserves that fact.
        let graph = CteGraph::new(
            vec![cte_node(
                "with_join",
                Some("select * from a join b on a.k = b.k"),
            )],
            vec![],
        );
        assert_eq!(classify_node_role(&graph, 0), NodeRole::Transform);
    }

    #[test]
    fn classify_out_of_bounds_index_defaults_to_transform() {
        let graph = CteGraph::default();
        assert_eq!(classify_node_role(&graph, 0), NodeRole::Transform);
        assert_eq!(classify_node_role(&graph, 99), NodeRole::Transform);
    }

    #[test]
    fn classify_node_without_simple_from_shape_is_transform() {
        // A node whose engine-computed `is_simple_from_shape` is `false`
        // (the default for the bare constructor — covers nodes without
        // raw_sql and nodes whose body the engine did not classify as
        // single-source) falls back to `Transform`.
        let graph = CteGraph::new(vec![cte_node("stg_x", None)], vec![]);
        assert_eq!(classify_node_role(&graph, 0), NodeRole::Transform);
    }

    // ===== edge_type_wire_key =====

    #[test]
    fn edge_type_wire_key_matches_serde_snake_case_for_every_variant() {
        // Pin every variant's wire key against the serde-emitted
        // snake_case string. If a new EdgeType variant lands and serde
        // gives it a different snake_case shape than the JS expects, this
        // test fails before the rendered report ships a broken color
        // legend.
        for et in [
            EdgeType::From,
            EdgeType::Inner,
            EdgeType::Left,
            EdgeType::Right,
            EdgeType::Full,
            EdgeType::Cross,
            EdgeType::UnionAll,
            EdgeType::UnionDistinct,
        ] {
            let key = edge_type_wire_key(et);
            let serde_value: String = serde_json::to_string(&et)
                .expect("EdgeType serializes")
                .trim_matches('"')
                .to_owned();
            assert_eq!(key, serde_value, "wire-key drift for {et:?}");
        }
    }

    // ===== compose_banner_text =====

    #[test]
    fn compose_banner_text_empty_scope_is_the_locked_constant() {
        assert_eq!(compose_banner_text(&InScopeSet::new()), BANNER_EMPTY_SCOPE);
    }

    #[test]
    fn compose_banner_text_single_test_uses_singular_noun() {
        let one = InScopeSet::from_iter(["unit_test.shop.a".to_owned()]);
        assert_eq!(compose_banner_text(&one), "Showing 1 unit test in scope");
    }

    #[test]
    fn compose_banner_text_multiple_tests_use_plural_noun() {
        let many =
            InScopeSet::from_iter(["unit_test.shop.a".to_owned(), "unit_test.shop.b".to_owned()]);
        assert_eq!(compose_banner_text(&many), "Showing 2 unit tests in scope");
    }

    // ===== build_payload + render integration =====

    fn checksum(value: &str) -> Checksum {
        Checksum::new("sha256", value)
    }

    fn model_node(id: &str, body: &str, compiled: Option<&str>) -> Node {
        model_node_with_raw(id, body, compiled, None)
    }

    fn model_node_with_raw(
        id: &str,
        body: &str,
        compiled: Option<&str>,
        raw: Option<&str>,
    ) -> Node {
        Node::new(
            NodeId::new(id),
            "model",
            checksum(body),
            compiled.map(str::to_owned),
            raw.map(str::to_owned),
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
    }

    fn simple_unit_test(model_bare: &str, name: &str) -> UnitTest {
        UnitTest::new(
            name,
            NodeId::new(model_bare),
            vec![UnitTestGiven::new(
                format!("ref('{model_bare}_src')"),
                json!([]),
                None,
                None,
            )],
            UnitTestExpect::new(json!([]), None, None),
            Some("a description".to_owned()),
            DependsOn::default(),
            Some(vec!["finance".to_owned()]),
            Some(json!({"owner": "team"})),
            Some("models/x.yml".to_owned()),
        )
    }

    fn manifest_for(nodes: Vec<Node>, tests: Vec<(&str, UnitTest)>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            tests.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
            HashMap::new(),
        )
    }

    fn manifest_with_sources(
        nodes: Vec<Node>,
        tests: Vec<(&str, UnitTest)>,
        sources: Vec<SourceNode>,
    ) -> Manifest {
        manifest_for(nodes, tests)
            .with_sources(sources.into_iter().map(|s| (s.id().clone(), s)).collect())
    }

    // ===== cute-dbt#267 — config-tree attribution render surfaces =====

    #[test]
    fn build_payload_attaches_config_attribution_chips_to_widened_models() {
        let manifest = manifest_for(
            vec![
                model_node("model.shop.fct_orders", "b1", Some("select 1")),
                model_node("model.shop.stg_raw", "b2", Some("select 1")),
            ],
            vec![],
        );
        let models = ModelInScopeSet::from_iter([
            NodeId::new("model.shop.fct_orders"),
            NodeId::new("model.shop.stg_raw"),
        ]);
        let facts = ProjectFacts {
            definition: None,
            panel: None,
            config_attributions: BTreeMap::from([(
                "model.shop.fct_orders".to_owned(),
                vec![ConfigAttribution {
                    key: "materialized".to_owned(),
                    path: "models.shop.marts".to_owned(),
                }],
            )]),
            var_references: BTreeMap::new(),
        };
        let payload = build_payload_with_externals(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            &CheckPolicy::default(),
            &facts,
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let fct = payload
            .models
            .iter()
            .find(|m| m.name == "fct_orders")
            .expect("widened model renders");
        assert_eq!(
            fct.config_attributions,
            vec![ConfigAttribution {
                key: "materialized".to_owned(),
                path: "models.shop.marts".to_owned(),
            }],
        );
        let stg = payload
            .models
            .iter()
            .find(|m| m.name == "stg_raw")
            .expect("unattributed model renders");
        assert!(stg.config_attributions.is_empty());
        // The unattributed model's JSON omits the key entirely (additive
        // payload shape — pre-#267 byte stability). The field is a Vec —
        // exactly two wire states, both pinned here: present-with-content
        // (the exact serialized shape the report JS consumes) and absent.
        let json = serde_json::to_string(&payload).expect("serialize");
        assert_eq!(json.matches("config_attributions").count(), 1, "{json}");
        assert!(
            json.contains(
                r#""config_attributions":[{"key":"materialized","path":"models.shop.marts"}]"#
            ),
            "the attributed model serializes the full wire shape: {json}",
        );
    }

    // ===== cute-dbt#413 — per-model axis chips + config-file =====

    #[test]
    fn build_payload_attaches_axes_and_config_file_to_in_scope_models() {
        // Two models patched by the SAME schema.yml: `fct_orders` fires
        // body+config (its `.sql` changed AND its schema.yml changed),
        // `stg_raw` fires config+unit_test (only its schema.yml changed,
        // and it hosts an in-scope test). Both carry the same `config_file`
        // (the grouping key the optgroup uses).
        let schema = "models/shop/_shop__models.yml";
        let manifest = manifest_for(
            vec![
                model_node("model.shop.fct_orders", "b1", Some("select 1"))
                    .with_patch_path(Some(schema.to_owned())),
                model_node("model.shop.stg_raw", "b2", Some("select 1"))
                    .with_patch_path(Some(schema.to_owned())),
            ],
            vec![],
        );
        let models = ModelInScopeSet::from_iter([
            NodeId::new("model.shop.fct_orders"),
            NodeId::new("model.shop.stg_raw"),
        ]);
        let axes = BTreeMap::from([
            (
                NodeId::new("model.shop.fct_orders"),
                ChangeAxes {
                    body: true,
                    config: true,
                    unit_test: false,
                },
            ),
            (
                NodeId::new("model.shop.stg_raw"),
                ChangeAxes {
                    body: false,
                    config: true,
                    unit_test: true,
                },
            ),
        ]);
        let payload = build_payload_with_externals(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            &CheckPolicy::default(),
            &ProjectFacts::default(),
            &axes,
            &BTreeMap::new(),
        );
        let fct = payload
            .models
            .iter()
            .find(|m| m.name == "fct_orders")
            .expect("model renders");
        let fct_axes = fct.axes.expect("axes present in pr-diff mode");
        assert!(fct_axes.body && fct_axes.config && !fct_axes.unit_test);
        assert_eq!(fct.config_file.as_deref(), Some(schema));
        let stg = payload
            .models
            .iter()
            .find(|m| m.name == "stg_raw")
            .expect("model renders");
        let stg_axes = stg.axes.expect("axes present in pr-diff mode");
        assert!(!stg_axes.body && stg_axes.config && stg_axes.unit_test);
        // Same schema.yml ⇒ same grouping key for both models.
        assert_eq!(stg.config_file.as_deref(), Some(schema));
    }

    #[test]
    fn build_payload_omits_axes_and_config_file_in_baseline_mode() {
        // The baseline arm produces an empty `axes` map (the documented
        // Option-A gap). Every model's `axes` / `config_file` is then
        // absent, so the JSON omits both keys — baseline goldens stay
        // byte-identical even when the model carries a `patch_path`.
        let manifest = manifest_for(
            vec![
                model_node("model.shop.fct_orders", "b1", Some("select 1"))
                    .with_patch_path(Some("models/shop/_shop__models.yml".to_owned())),
            ],
            vec![],
        );
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.fct_orders")]);
        let payload = build_payload_with_externals(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
            &CheckPolicy::default(),
            &ProjectFacts::default(),
            // Empty axes map ⇒ the baseline-mode shape.
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let fct = &payload.models[0];
        assert!(fct.axes.is_none(), "baseline mode carries no axes");
        assert!(
            fct.config_file.is_none(),
            "config_file is gated to axes-present models"
        );
        let json = serde_json::to_string(&payload).expect("serialize");
        assert_eq!(json.matches("\"axes\"").count(), 0, "{json}");
        assert_eq!(json.matches("config_file").count(), 0, "{json}");
    }

    #[test]
    fn axes_payload_serializes_the_three_axis_bits() {
        // The exact wire shape the report JS reads for the axis chips.
        let payload = AxesPayload::from(ChangeAxes {
            body: true,
            config: false,
            unit_test: true,
        });
        let json = serde_json::to_string(&payload).expect("serialize");
        assert_eq!(json, r#"{"body":true,"config":false,"unit_test":true}"#);
    }

    // ===== cute-dbt#416 — NEW/MODIFIED state + REMOVED model paths =====

    #[test]
    fn build_payload_attaches_new_and_modified_state_to_in_scope_models() {
        // A NEW model carries `state: "new"`; a MODIFIED model carries
        // `state: "modified"`. Both serialize their wire form.
        let manifest = manifest_for(
            vec![
                model_node("model.shop.fct_new", "b1", Some("select 1")),
                model_node("model.shop.dim_mod", "b2", Some("select 1")),
            ],
            vec![],
        );
        let models = ModelInScopeSet::from_iter([
            NodeId::new("model.shop.fct_new"),
            NodeId::new("model.shop.dim_mod"),
        ]);
        let states = BTreeMap::from([
            (NodeId::new("model.shop.fct_new"), ModelState::New),
            (NodeId::new("model.shop.dim_mod"), ModelState::Modified),
        ]);
        let payload = build_payload_with_externals(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            &CheckPolicy::default(),
            &ProjectFacts::default(),
            &BTreeMap::new(),
            &states,
        );
        let new_model = payload
            .models
            .iter()
            .find(|m| m.name == "fct_new")
            .expect("model renders");
        assert_eq!(new_model.state, Some("new"));
        let mod_model = payload
            .models
            .iter()
            .find(|m| m.name == "dim_mod")
            .expect("model renders");
        assert_eq!(mod_model.state, Some("modified"));
        let json = serde_json::to_string(&payload).expect("serialize");
        assert!(json.contains(r#""state":"new""#), "{json}");
        assert!(json.contains(r#""state":"modified""#), "{json}");
    }

    #[test]
    fn build_payload_omits_state_in_baseline_mode() {
        // An empty `model_states` map (the baseline arm) ⇒ no `state` key on
        // any model, so baseline goldens stay byte-identical.
        let manifest = manifest_for(
            vec![model_node("model.shop.fct_orders", "b1", Some("select 1"))],
            vec![],
        );
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.fct_orders")]);
        let payload = build_payload_with_externals(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
            &CheckPolicy::default(),
            &ProjectFacts::default(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(payload.models[0].state.is_none());
        let json = serde_json::to_string(&payload).expect("serialize");
        assert_eq!(json.matches("\"state\"").count(), 0, "{json}");
    }

    #[test]
    fn model_state_wire_maps_every_variant() {
        // Exhaustive enumeration of the wire vocabulary (house style).
        assert_eq!(model_state_wire(ModelState::New), "new");
        assert_eq!(model_state_wire(ModelState::Modified), "modified");
        assert_eq!(model_state_wire(ModelState::Removed), "removed");
    }

    #[test]
    fn render_threads_removed_models_into_the_payload() {
        // The REMOVED model paths flow through render_report_with_externals
        // into the payload, sorted, and serialize; an empty slice omits the
        // key (baseline + non-removal goldens stay byte-identical).
        let manifest = manifest_for(
            vec![model_node("model.shop.dim_kept", "b1", Some("select 1"))],
            vec![],
        );
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_kept")]);
        let tmp = std::env::temp_dir().join(format!(
            "cute-dbt-removed-{}-{}.html",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        ));
        render_report_with_externals(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            ScopeSource::PrDiff,
            "t",
            None,
            &CheckPolicy::default(),
            &ProjectFacts::default(),
            &GovernanceFacts::default(),
            None,
            None,
            &[],
            DEFAULT_SEED_ROW_CAP,
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &["models/marts/dim_gone.sql".to_owned()],
            None,
        )
        .expect("report renders");
        let html = std::fs::read_to_string(&tmp).expect("read report");
        let _ = std::fs::remove_file(&tmp);
        assert!(
            html.contains("models/marts/dim_gone.sql"),
            "the removed model path is inlined into the payload",
        );
        assert!(
            html.contains("removed_models"),
            "the removed_models key serializes when non-empty",
        );
    }

    // ===== cute-dbt#268 — vars attribution render surfaces =====

    /// One attributed var entry with every surface populated.
    fn full_var_attribution() -> VarAttribution {
        VarAttribution {
            name: "dq_threshold".to_owned(),
            package: None,
            old: Some(json!(10)),
            new: Some(json!(5)),
            direct: vec!["model.shop.mart_dq".to_owned()],
            config: vec!["model.shop.grid".to_owned()],
            via_macros: vec![crate::domain::MacroVarHit {
                model: "model.shop.stg_enc".to_owned(),
                via: "macro.shop.add_dq_flags".to_owned(),
            }],
            dynamic: vec!["model.shop.dyn_caller".to_owned()],
            masked_packages: vec!["dbt_utils".to_owned()],
            insulated_tests: vec!["unit_test.shop.mart_dq.test_pins_threshold".to_owned()],
        }
    }

    fn vars_change_with_facts(entries: Vec<VarAttribution>) -> ProjectChange {
        ProjectChange {
            category: ProjectChangeCategory::Vars,
            label: "dq_threshold".to_owned(),
            old: Some(json!(10)),
            new: Some(json!(5)),
            hook: None,
            tree: None,
            vars: Some(VarChangeFacts {
                entries,
                footprint: VarScanFootprint {
                    models_scanned: 469,
                    macros_scanned: 910,
                    python_models: 2,
                },
            }),
        }
    }

    #[test]
    fn vars_row_with_facts_builds_entries_and_swaps_in_the_honest_note() {
        let panel = ProjectChangePanel::Categorized {
            changes: vec![vars_change_with_facts(vec![full_var_attribution()])],
        };
        let view = project_panel_view(&panel, &BTreeMap::new());
        let row = &view.rows[0];
        assert!(row.detail.is_empty(), "entries carry their own old→new");
        // The honest-UNKNOWN residual copy: causes enumerated, footprint
        // stated, at-least framing, disabled caveat, never-widen statement.
        for fragment in [
            "are \u{201c}at least\u{201d}, never exact",
            "dynamic var() names",
            "var-to-var value indirection",
            "CLI --vars overrides",
            "Python models",
            "Checked: 469 models' SQL and configs plus 910 macro bodies",
            "(2 Python models could not be scanned)",
            "An inline var() default never overrides a project value",
            "disabled by this edit drop out of the manifest and cannot be listed",
            "contextualized here, never widened into report scope",
        ] {
            assert!(
                row.note.contains(fragment),
                "note must state {fragment:?}: {}",
                row.note
            );
        }
        let entry = &row.var_entries[0];
        assert_eq!(entry.name, "dq_threshold");
        assert!(
            entry.scope.is_empty(),
            "a global entry carries no scope label"
        );
        assert_eq!(entry.detail, "10 \u{2192} 5");
        let lines: Vec<(&str, &str)> = entry
            .tier_lines
            .iter()
            .map(|l| (l.tier_key, l.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            vec![
                (
                    "direct",
                    "at least 1 model reads this var directly in SQL: mart_dq",
                ),
                (
                    "config",
                    "at least 1 model carries config driven by this var: grid",
                ),
                (
                    "macro",
                    "at least 1 model reads this var through its macro closure: \
                     stg_enc (via add_dq_flags)",
                ),
            ],
        );
        assert_eq!(
            entry.notes,
            vec![
                "masked for dbt_utils: a package-scoped value pins this var there, so \
                 this edit does not reach those models (package vars outrank global vars)"
                    .to_owned(),
                "1 unit test pins this var in overrides.vars and is insulated from this \
                 edit (the override always wins): test_pins_threshold"
                    .to_owned(),
                "1 model calls var() with a computed name and cannot be ruled out: \
                 dyn_caller"
                    .to_owned(),
            ],
        );
    }

    #[test]
    fn vars_row_package_scoped_entry_carries_the_scope_label() {
        let entry = VarAttribution {
            package: Some("dbt_utils".to_owned()),
            masked_packages: Vec::new(),
            ..full_var_attribution()
        };
        let panel = ProjectChangePanel::Categorized {
            changes: vec![vars_change_with_facts(vec![entry])],
        };
        let view = project_panel_view(&panel, &BTreeMap::new());
        assert_eq!(
            view.rows[0].var_entries[0].scope,
            "package-scoped: dbt_utils",
        );
    }

    #[test]
    fn vars_row_zero_hit_entry_states_the_empty_scan_explicitly() {
        let entry = VarAttribution {
            name: "unreferenced".to_owned(),
            old: Some(json!("a")),
            new: Some(json!("b")),
            ..VarAttribution::default()
        };
        let panel = ProjectChangePanel::Categorized {
            changes: vec![vars_change_with_facts(vec![entry])],
        };
        let view = project_panel_view(&panel, &BTreeMap::new());
        let rendered = &view.rows[0].var_entries[0];
        assert!(rendered.tier_lines.is_empty());
        assert_eq!(
            rendered.notes,
            vec!["no referencing models found by the static scan".to_owned()],
        );
    }

    #[test]
    fn var_tier_lines_apply_the_r1b_cap() {
        // Past the cap the sentence keeps the explicit count, drops the
        // inline names, and the full list rides the <details> overflow
        // ("listed, not individually rendered").
        let entry = VarAttribution {
            direct: (0..=CONFIG_AFFECTED_CAP)
                .map(|i| format!("model.shop.m{i:02}"))
                .collect(),
            config: Vec::new(),
            via_macros: Vec::new(),
            dynamic: Vec::new(),
            masked_packages: Vec::new(),
            insulated_tests: Vec::new(),
            ..full_var_attribution()
        };
        let lines = var_entry_tier_lines(&entry);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].text,
            "at least 11 models read this var directly in SQL \u{2014} listed, not \
             individually rendered",
        );
        assert_eq!(lines[0].overflow.len(), 11);
        assert_eq!(lines[0].overflow[0], "m00");
        // At the cap: names inline, no overflow.
        let at_cap = VarAttribution {
            direct: (0..CONFIG_AFFECTED_CAP)
                .map(|i| format!("model.shop.m{i:02}"))
                .collect(),
            ..entry
        };
        let lines = var_entry_tier_lines(&at_cap);
        assert!(lines[0].text.contains("m00"), "{}", lines[0].text);
        assert!(lines[0].overflow.is_empty());
    }

    #[test]
    fn vars_row_html_renders_entries_tier_chips_and_notes() {
        let facts = ProjectFacts {
            definition: None,
            panel: Some(ProjectChangePanel::Categorized {
                changes: vec![vars_change_with_facts(vec![full_var_attribution()])],
            }),
            config_attributions: BTreeMap::new(),
            var_references: BTreeMap::new(),
        };
        let html = render_html_with_project_facts("cute_dbt_render_vars_row_test.html", &facts);
        assert!(
            html.contains(r#"data-testid="project-def-var-entry""#),
            "the var entry block renders",
        );
        assert!(
            html.contains(r#"<code class="project-def-var-name">dq_threshold</code>"#),
            "the var name renders",
        );
        for chip in [
            r#"<span class="tier-chip tier-direct">DIRECT</span>"#,
            r#"<span class="tier-chip tier-config">CONFIG</span>"#,
            r#"<span class="tier-chip tier-macro">MACRO</span>"#,
        ] {
            assert!(html.contains(chip), "tier chip renders: {chip}");
        }
        assert!(
            html.contains("at least 1 model reads this var directly in SQL: mart_dq"),
            "the DIRECT tier line renders",
        );
        assert!(
            html.contains("insulated from this\n                 edit")
                || html.contains("insulated from this edit"),
            "the insulated-tests note renders",
        );
        assert!(
            html.contains("never widened\n         into report scope")
                || html.contains("never widened into report scope"),
            "the contextualize-don't-widen statement renders",
        );
    }

    #[test]
    fn build_payload_attaches_var_reference_chips_to_in_scope_models() {
        let manifest = manifest_for(
            vec![
                model_node("model.shop.mart_dq", "b1", Some("select 1")),
                model_node("model.shop.stg_raw", "b2", Some("select 1")),
            ],
            vec![],
        );
        let models = ModelInScopeSet::from_iter([
            NodeId::new("model.shop.mart_dq"),
            NodeId::new("model.shop.stg_raw"),
        ]);
        let facts = ProjectFacts {
            definition: None,
            panel: None,
            config_attributions: BTreeMap::new(),
            var_references: BTreeMap::from([(
                "model.shop.mart_dq".to_owned(),
                vec![VarReference {
                    name: "dq_threshold".to_owned(),
                    tier: crate::domain::VarTier::Direct,
                    via: None,
                }],
            )]),
        };
        let payload = build_payload_with_externals(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            &CheckPolicy::default(),
            &facts,
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let mart = payload
            .models
            .iter()
            .find(|m| m.name == "mart_dq")
            .expect("referencing model renders");
        assert_eq!(mart.var_references.len(), 1);
        assert_eq!(mart.var_references[0].name, "dq_threshold");
        let stg = payload
            .models
            .iter()
            .find(|m| m.name == "stg_raw")
            .expect("unreferencing model renders");
        assert!(stg.var_references.is_empty());
        // Additive wire shape: absent ⇒ the key is omitted entirely.
        let json = serde_json::to_string(&payload).expect("serialize");
        assert_eq!(json.matches("var_references").count(), 1, "{json}");
        assert!(
            json.contains(r#""var_references":[{"name":"dq_threshold","tier":"direct"}]"#),
            "the referencing model serializes the full wire shape: {json}",
        );
    }

    #[test]
    fn affected_models_strings_inline_up_to_the_cap_and_collapse_past_it() {
        // Zero — a truthful TOTAL-tier zero, no name list.
        let (text, overflow) = affected_models_strings(&BTreeSet::new());
        assert_eq!(text, "affects 0 models in this manifest");
        assert!(overflow.is_empty());

        // At the cap — names ride inline, no overflow.
        let at_cap: BTreeSet<String> = (0..CONFIG_AFFECTED_CAP)
            .map(|i| format!("m{i:02}"))
            .collect();
        let (text, overflow) = affected_models_strings(&at_cap);
        assert!(
            text.contains("affects 10 models — widened into report scope: m00,"),
            "{text}",
        );
        assert!(text.contains("m09"), "every name rides inline: {text}");
        assert!(overflow.is_empty());

        // Past the cap (R1b) — explicit count, names collapse to overflow.
        let over: BTreeSet<String> = (0..=CONFIG_AFFECTED_CAP)
            .map(|i| format!("m{i:02}"))
            .collect();
        let (text, overflow) = affected_models_strings(&over);
        assert_eq!(
            text,
            "affects 11 models — widened into report scope, listed below",
        );
        assert_eq!(overflow.len(), 11);
        assert!(!text.contains("m00"), "no name leaks inline past the cap");
    }

    #[test]
    fn affected_models_count_by_node_id_and_disambiguate_bare_name_twins() {
        // Two packages each carrying a model named `dim_x` (legal since
        // dbt 1.6 two-arg ref), both selected by a section-root edit:
        // the count is over full node ids — never a bare-name collapse —
        // and the colliding entries display their full ids so the
        // listing never shows two indistinguishable names. The unique
        // name keeps its bare (selector-vocabulary) form.
        let ids: BTreeSet<String> = [
            "model.shop.dim_x".to_owned(),
            "model.pkg_two.dim_x".to_owned(),
            "model.shop.fct_solo".to_owned(),
        ]
        .into_iter()
        .collect();
        let (text, overflow) = affected_models_strings(&ids);
        assert_eq!(
            text,
            "affects 3 models — widened into report scope: \
             fct_solo, model.pkg_two.dim_x, model.shop.dim_x",
        );
        assert!(overflow.is_empty());
    }

    #[test]
    fn project_panel_view_attaches_affected_listing_to_models_tree_rows_only() {
        let models_leaf = crate::domain::ConfigLeafPath {
            section: "models".to_owned(),
            segments: vec!["shop".to_owned(), "marts".to_owned()],
            key: "materialized".to_owned(),
        };
        let seeds_leaf = crate::domain::ConfigLeafPath {
            section: "seeds".to_owned(),
            segments: Vec::new(),
            key: "quote_columns".to_owned(),
        };
        let panel = ProjectChangePanel::Categorized {
            changes: vec![
                ProjectChange {
                    category: ProjectChangeCategory::Vars,
                    label: "flag".to_owned(),
                    old: Some(json!(1)),
                    new: Some(json!(2)),
                    hook: None,
                    tree: None,
                    vars: None,
                },
                ProjectChange {
                    category: ProjectChangeCategory::ConfigTree,
                    label: models_leaf.label(),
                    old: Some(json!("view")),
                    new: Some(json!("table")),
                    hook: None,
                    tree: Some(models_leaf),
                    vars: None,
                },
                ProjectChange {
                    category: ProjectChangeCategory::ConfigTree,
                    label: seeds_leaf.label(),
                    old: None,
                    new: Some(json!(false)),
                    hook: None,
                    tree: Some(seeds_leaf),
                    vars: None,
                },
            ],
        };
        let attributions = BTreeMap::from([
            (
                "model.shop.dim_a".to_owned(),
                vec![ConfigAttribution {
                    key: "materialized".to_owned(),
                    path: "models.shop.marts".to_owned(),
                }],
            ),
            (
                "model.shop.fct_b".to_owned(),
                vec![ConfigAttribution {
                    key: "materialized".to_owned(),
                    path: "models.shop.marts".to_owned(),
                }],
            ),
        ]);
        let view = project_panel_view(&panel, &attributions);
        assert_eq!(view.rows.len(), 3);
        let vars_row = &view.rows[0];
        assert!(
            vars_row.affected_text.is_empty(),
            "vars rows make no affected-models claim (contextualize, never widen)",
        );
        let models_row = &view.rows[1];
        assert_eq!(
            models_row.affected_text,
            "affects 2 models — widened into report scope: dim_a, fct_b",
        );
        assert!(models_row.affected_overflow.is_empty());
        let seeds_row = &view.rows[2];
        assert!(
            seeds_row.affected_text.is_empty(),
            "non-models sections make no claim (this slice attributes models: only)",
        );
    }

    #[test]
    fn project_panel_view_reports_a_truthful_zero_for_a_shadowed_models_edit() {
        let leaf = crate::domain::ConfigLeafPath {
            section: "models".to_owned(),
            segments: vec!["shop".to_owned()],
            key: "materialized".to_owned(),
        };
        let panel = ProjectChangePanel::Categorized {
            changes: vec![ProjectChange {
                category: ProjectChangeCategory::ConfigTree,
                label: leaf.label(),
                old: Some(json!("view")),
                new: Some(json!("table")),
                hook: None,
                tree: Some(leaf),
                vars: None,
            }],
        };
        let view = project_panel_view(&panel, &BTreeMap::new());
        assert_eq!(
            view.rows[0].affected_text,
            "affects 0 models in this manifest",
        );
    }

    #[test]
    fn build_payload_threads_baseline_label_through() {
        let manifest = manifest_for(vec![], vec![]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &ModelInScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "main@a1f3c7e",
        );
        assert_eq!(payload.baseline, "main@a1f3c7e");
        assert!(payload.models.is_empty());
    }

    #[test]
    fn build_payload_emits_one_model_entry_per_model_in_scope() {
        let compiled = "with stg_orders_src as (select * from raw_orders) \
                        select * from stg_orders_src";
        let node = model_node("model.shop.stg_orders", "body", Some(compiled));
        let ut = simple_unit_test("stg_orders", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_orders")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        let model = &payload.models[0];
        assert_eq!(model.name, "stg_orders");
        assert_eq!(model.tests.len(), 1);
        assert_eq!(model.tests[0].name, "test_one");
        assert!(
            model.tests[0].changed,
            "test_one is in the changed set → tagged updated",
        );
    }

    #[test]
    fn model_payload_threads_the_models_original_file_path() {
        // cute-dbt#179 — the Model-SQL code-card header shows the model's
        // full project-relative path (`models/…/x.sql`, never just the
        // filename). `path` rides `Node::original_file_path`.
        let node = Node::new(
            NodeId::new("model.shop.stg_orders"),
            "model",
            checksum("body"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            Some("models/staging/stg_orders.sql".to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        );
        let ut = simple_unit_test("stg_orders", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_orders")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(
            payload.models[0].path.as_deref(),
            Some("models/staging/stg_orders.sql"),
            "ModelPayload.path carries the manifest original_file_path verbatim",
        );
        let json = serde_json::to_string(&payload).expect("payload serializes");
        assert!(
            json.contains(r#""path":"models/staging/stg_orders.sql""#),
            "the full path is on the wire for the code-card header",
        );
    }

    #[test]
    fn model_payload_emits_column_lineage_context_for_documented_and_tested_columns() {
        // cute-dbt#446 (CLL-1) — a documented + tested column rides the wire
        // as `column_lineage.context.<col>` with data_type, description,
        // documented:true, and its tested-by facts. The Tier-1 `edges` array
        // does NOT exist yet (context-only).
        let model = Node::new(
            NodeId::new("model.shop.customers"),
            "model",
            checksum("body"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::from([("customer_id".to_owned(), Some("integer".to_owned()))]),
        )
        .with_column_descriptions(BTreeMap::from([(
            "customer_id".to_owned(),
            "PK of the customer".to_owned(),
        )]));
        let not_null = column_test_node(
            "test.shop.not_null_customers_customer_id",
            "model.shop.customers",
            "customer_id",
            TestMetadata::new("not_null", None, Value::Null),
        );
        let ut = simple_unit_test("customers", "test_one");
        let manifest = manifest_for(vec![model, not_null], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.customers")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let cl = payload.models[0]
            .column_lineage
            .as_ref()
            .expect("documented+tested model carries column_lineage");
        let col = cl.context.get("customer_id").expect("column present");
        assert!(col.documented);
        assert_eq!(col.data_type.as_deref(), Some("integer"));
        assert_eq!(col.description.as_deref(), Some("PK of the customer"));
        assert_eq!(col.tests.len(), 1);
        assert_eq!(col.tests[0].kind, "not_null");

        let json = serde_json::to_value(&payload).expect("payload serializes");
        let ctx = &json["models"][0]["column_lineage"]["context"]["customer_id"];
        assert_eq!(ctx["data_type"], "integer");
        assert_eq!(ctx["documented"], true);
        assert_eq!(ctx["tests"][0]["kind"], "not_null");
        // CLL-1 ships context-only — no `edges` key anywhere on the wire.
        assert!(
            json["models"][0]["column_lineage"].get("edges").is_none(),
            "CLL-1 is context-only; edges land with CLL-2 (#447)",
        );
    }

    #[test]
    fn model_payload_omits_column_lineage_when_no_documented_or_tested_columns() {
        // cute-dbt#446 — an undocumented, untested model omits the whole
        // `column_lineage` key so every pre-#446 golden stays byte-stable.
        let node = model_node("model.shop.stg_orders", "body", Some("select 1"));
        let ut = simple_unit_test("stg_orders", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_orders")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert!(payload.models[0].column_lineage.is_none());
        let json = serde_json::to_string(&payload).expect("payload serializes");
        assert!(
            !json.contains(r#""column_lineage""#),
            "no documented/tested columns ⇒ no column_lineage key (older goldens stay stable)",
        );
    }

    #[test]
    fn model_payload_path_is_omitted_when_the_manifest_carries_none() {
        // cute-dbt#179 — synthetic / pre-1.8 manifests carry no
        // original_file_path: the key is omitted from the wire (the JS
        // falls back to `<name>.sql`, the cute-dbt#155 terminal label).
        let node = model_node("model.shop.stg_orders", "body", Some("select 1"));
        let ut = simple_unit_test("stg_orders", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_orders")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert!(payload.models[0].path.is_none());
        let json = serde_json::to_string(&payload).expect("payload serializes");
        assert!(
            !json.contains(r#""path""#),
            "absent original_file_path ⇒ no path key on the wire (older fixtures stay stable)",
        );
    }

    #[test]
    fn build_payload_carries_findings_for_an_unbacked_unique_key() {
        // cute-dbt#169 — the check engine runs during payload assembly:
        // an in-scope model declaring config.unique_key with no backing
        // uniqueness test surfaces an UNCOVERED grain finding on its
        // ModelPayload, serialized with the dotted check id.
        let mut config = BTreeMap::new();
        config.insert("materialized".to_owned(), json!("incremental"));
        config.insert("unique_key".to_owned(), json!("order_id"));
        let node = Node::new(
            NodeId::new("model.shop.orders_rollup"),
            "model",
            checksum("body"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        );
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders_rollup")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        assert_eq!(json["findings"][0]["check"], "grain.unique-key-unbacked");
        assert_eq!(json["findings"][0]["verdict"]["status"], "uncovered");
        assert_eq!(json["findings"][0]["model_id"], "model.shop.orders_rollup");
    }

    #[test]
    fn build_payload_flattens_degraded_backing_onto_the_finding_wire() {
        // cute-dbt#259 — FindingPayload flattens the domain Finding, so
        // the `degraded` per-test cue list rides the wire beside the
        // covered verdict's attribution (the findings panel renders the
        // chip + cause list from exactly these keys).
        let mut config = BTreeMap::new();
        config.insert("unique_key".to_owned(), json!("order_id"));
        let node = Node::new(
            NodeId::new("model.shop.orders_rollup"),
            "model",
            checksum("body"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        );
        let mut test_config = BTreeMap::new();
        test_config.insert("severity".to_owned(), json!("warn"));
        let test = Node::new(
            NodeId::new("test.shop.unique_orders_rollup_order_id"),
            "test",
            checksum("t"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(test_config, false),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            Some("order_id".to_owned()),
            Some(NodeId::new("model.shop.orders_rollup")),
            Some(TestMetadata::new(
                "unique",
                None,
                json!({ "column_name": "order_id" }),
            )),
        );
        let manifest = manifest_for(vec![node, test], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders_rollup")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        assert_eq!(json["findings"][0]["verdict"]["status"], "covered");
        assert_eq!(
            json["findings"][0]["degraded"][0]["by"],
            "test.shop.unique_orders_rollup_order_id"
        );
        assert!(
            json["findings"][0]["degraded"][0]["causes"][0]
                .as_str()
                .is_some_and(|c| c.starts_with("severity: warn")),
            "the domain-composed cause copy rides the wire: {json}"
        );
    }

    #[test]
    fn build_payload_omits_the_findings_key_when_no_check_fires() {
        // The serde skip keeps every pre-#169 payload byte-stable: a
        // model tripping no check carries NO `findings` key at all.
        let node = model_node("model.shop.plain", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.plain")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        assert!(
            json.get("findings").is_none(),
            "empty findings must be serde-skipped; got {json}"
        );
    }

    /// A model tripping BOTH registered checks: `config.unique_key` with
    /// no backing uniqueness test (grain, UNCOVERED) and a UNION whose
    /// arms no unit test feeds (union, UNCOVERED with sketches).
    fn findings_surface_payload() -> ReportPayload {
        let mut config = BTreeMap::new();
        config.insert("unique_key".to_owned(), json!("event_id"));
        let compiled = "with arm_a as (select * from src_a), \
                        arm_b as (select * from src_b), \
                        unioned as (select * from arm_a union all select * from arm_b) \
                        select * from unioned";
        let node = Node::new(
            NodeId::new("model.shop.events"),
            "model",
            checksum("body"),
            Some(compiled.to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        );
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.events")]);
        build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        )
    }

    #[test]
    fn finding_payload_pins_bracketed_constructs_to_the_named_node() {
        // cute-dbt#170 — `union[unioned]` resolves to the `unioned` CTE
        // node; the model-level grain construct pins the terminal node.
        let payload = findings_surface_payload();
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        let findings = json["findings"].as_array().expect("findings present");
        let union = findings
            .iter()
            .find(|f| f["check"] == "union.arm-coverage")
            .expect("union finding fires");
        assert_eq!(union["pin_node"], "unioned");
        let grain = findings
            .iter()
            .find(|f| f["check"] == "grain.unique-key-unbacked")
            .expect("grain finding fires");
        assert_eq!(
            grain["pin_node"], TERMINAL_NODE_NAME,
            "model-level constructs pin the terminal node"
        );
    }

    #[test]
    fn finding_payload_pins_qualified_join_constructs_to_the_consumer_node() {
        // cute-dbt#173 constructs are `left_join[<consumer>:<right>]` —
        // the consumer CTE is the pin target.
        let compiled = "with customers as (select * from src_customers), \
                        orders as (select * from src_orders), \
                        joined as (select orders.id, customers.email from orders \
                        left join customers on orders.customer_id = customers.id) \
                        select * from joined";
        let node = model_node("model.shop.order_emails", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.order_emails")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        let join = json["findings"]
            .as_array()
            .expect("findings present")
            .iter()
            .find(|f| {
                f["construct"]
                    .as_str()
                    .is_some_and(|c| c.starts_with("left_join["))
            })
            .cloned()
            .expect("a join finding fires on the LEFT JOIN model");
        assert_eq!(
            join["pin_node"], "joined",
            "the qualified construct pins the consumer CTE: {join}"
        );
    }

    #[test]
    fn finding_payload_omits_pin_node_when_the_graph_is_empty() {
        // A `select 1` model has no CTE graph — no pin affordance.
        let mut config = BTreeMap::new();
        config.insert("unique_key".to_owned(), json!("order_id"));
        let node = Node::new(
            NodeId::new("model.shop.flat"),
            "model",
            checksum("body"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        );
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.flat")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        assert!(
            json["findings"][0].get("pin_node").is_none(),
            "empty graph ⇒ pin_node omitted; got {json}"
        );
    }

    #[test]
    fn finding_payload_lifts_suggested_given_sketches_out_of_evidence() {
        // cute-dbt#170 — the union check's `suggested given` evidence
        // entries become the copyable `sketches` array; the remaining
        // evidence list never carries that label.
        let payload = findings_surface_payload();
        let json = serde_json::to_value(&payload.models[0]).expect("serialize");
        let union = json["findings"]
            .as_array()
            .expect("findings present")
            .iter()
            .find(|f| f["check"] == "union.arm-coverage")
            .cloned()
            .expect("union finding fires");
        let sketches = union["sketches"].as_array().expect("sketches lifted");
        assert!(!sketches.is_empty(), "uncovered arms carry sketches");
        assert!(
            sketches
                .iter()
                .all(|s| s.as_str().is_some_and(|s| s.starts_with("- input: ref('"))),
            "each sketch is the copy-pasteable given-row YAML: {sketches:?}"
        );
        let labels: Vec<&str> = union["evidence"]
            .as_array()
            .expect("evidence stays present")
            .iter()
            .filter_map(|e| e["label"].as_str())
            .collect();
        assert!(
            !labels.contains(&"suggested given"),
            "sketch entries are LIFTED, not duplicated: {labels:?}"
        );
    }

    #[test]
    fn report_payload_carries_the_check_spec_catalog_for_fired_checks() {
        // cute-dbt#170 — the rationale drawer renders offline from
        // `check_specs`; the book link is a plain click-only href.
        let payload = findings_surface_payload();
        let json = serde_json::to_value(&payload).expect("serialize");
        let specs = json["check_specs"]
            .as_object()
            .expect("check_specs present when findings fired");
        assert_eq!(specs.len(), 2, "exactly the fired checks: {specs:?}");
        let grain = &specs["grain.unique-key-unbacked"];
        assert_eq!(grain["tier"], "total");
        assert_eq!(grain["instrument"], "data-test");
        assert!(
            grain["rationale"].as_str().is_some_and(|r| !r.is_empty()),
            "rationale embeds inline (zero-egress)"
        );
        assert!(
            grain["conditions"]
                .as_array()
                .is_some_and(|c| !c.is_empty()),
            "conditions embed inline"
        );
        assert_eq!(
            grain["book_href"],
            "https://breezy-bays-labs.github.io/cute-dbt/checks/grain.unique-key-unbacked.html",
        );
        assert_eq!(
            specs["union.arm-coverage"]["tier"], "high",
            "tier vocabulary rides per check, never blended"
        );
    }

    #[test]
    fn report_payload_omits_check_specs_when_no_finding_fires() {
        // Findings-free payloads (the jaffle-shop golden) stay
        // byte-stable: no `check_specs` key at all.
        let node = model_node("model.shop.plain", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.plain")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        let json = serde_json::to_value(&payload).expect("serialize");
        assert!(
            json.get("check_specs").is_none(),
            "no findings ⇒ no check_specs key; got {json}"
        );
    }

    #[test]
    fn build_payload_carries_raw_sql_when_node_has_raw_code() {
        // cute-dbt#47 — verify the per-model payload surfaces `raw_code`
        // verbatim into `raw_sql` so the template's Model SQL section can
        // render it. Jinja content (refs + comments) is preserved.
        let raw = "{# header #}\nselect * from {{ ref('upstream') }}";
        let compiled = "select * from raw_upstream";
        let node = model_node_with_raw("model.shop.stg_x", "body", Some(compiled), Some(raw));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_x")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        assert_eq!(payload.models[0].raw_sql.as_deref(), Some(raw));
    }

    #[test]
    fn build_payload_carries_sql_diff_keyed_by_full_model_id() {
        // cute-dbt#111 — a model whose full node id is in the `sql_diffs`
        // map surfaces `ModelPayload.sql_diff`. Keyed by the FULL id (the
        // `reconstruct_model_sql_diffs` key), not the bare name.
        let node = model_node("model.shop.dim_x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_x")]);
        let mut sql_diffs = HashMap::new();
        sql_diffs.insert(
            "model.shop.dim_x".to_owned(),
            BlockDiff {
                lines: vec![DiffLine {
                    kind: DiffLineKind::Added,
                    text: "select 1".to_owned(),
                    emphasis: None,
                }],
            },
        );
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &sql_diffs,
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        let sd = payload.models[0]
            .sql_diff
            .as_ref()
            .expect("sql_diff present for the keyed model");
        assert_eq!(sd.lines.len(), 1);
        assert_eq!(sd.lines[0].kind, DiffLineKind::Added);
    }

    #[test]
    fn build_payload_sql_diff_is_none_when_model_not_in_diff_map() {
        // No entry for this model → sql_diff omitted (skip_serializing_if).
        // The baseline path passes an empty map, so baseline reports never
        // carry a sql_diff key — the manifest-digest snapshot is unmoved.
        let node = model_node("model.shop.dim_y", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_y")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        assert!(payload.models[0].sql_diff.is_none());
        // And it is omitted from the JSON wire (not `"sql_diff":null`).
        let json = serde_json::to_string(&payload.models[0]).expect("serialize");
        assert!(
            !json.contains("sql_diff"),
            "absent sql_diff must be omitted from the wire; got {json}",
        );
    }

    // ----- cute-dbt#247: the Model-YAML section payload -----

    #[test]
    fn build_payload_carries_model_yaml_found_keyed_by_full_model_id() {
        // A Found gather outcome (keyed by the FULL node id, the
        // `gather_model_yaml` key) surfaces the sliced block + the schema
        // file path; no degrade copy.
        let node = model_node("model.shop.dim_x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_x")]);
        let raw_block = "  - name: dim_x\n    description: a model";
        let mut model_yaml = HashMap::new();
        model_yaml.insert(
            "model.shop.dim_x".to_owned(),
            crate::domain::ModelYamlOutcome::Found {
                path: "models/schema.yml".to_owned(),
                block: UnitTestYamlBlock::new(raw_block.to_owned(), 2, 2, 3),
                diff: None,
            },
        );
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &model_yaml,
            &HashMap::new(),
            "baseline.json",
        );
        let my = payload.models[0]
            .model_yaml
            .as_ref()
            .expect("model_yaml present for the keyed model");
        assert_eq!(my.path.as_deref(), Some("models/schema.yml"));
        assert_eq!(my.raw.as_deref(), Some(raw_block));
        assert!(my.diff.is_none());
        assert!(my.missing.is_none());
    }

    #[test]
    fn build_payload_model_yaml_carries_the_attached_diff() {
        // A Found outcome with an attached inline diff (the PrDiff arm's
        // `attach_model_yaml_diffs`) rides into the payload — the section's
        // Diff view.
        let node = model_node("model.shop.dim_x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_x")]);
        let mut model_yaml = HashMap::new();
        model_yaml.insert(
            "model.shop.dim_x".to_owned(),
            crate::domain::ModelYamlOutcome::Found {
                path: "models/schema.yml".to_owned(),
                block: UnitTestYamlBlock::new("  - name: dim_x".to_owned(), 2, 2, 2),
                diff: Some(BlockDiff {
                    lines: vec![DiffLine {
                        kind: DiffLineKind::Added,
                        text: "    description: new".to_owned(),
                        emphasis: None,
                    }],
                }),
            },
        );
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &model_yaml,
            &HashMap::new(),
            "",
        );
        let my = payload.models[0].model_yaml.as_ref().expect("model_yaml");
        let diff = my.diff.as_ref().expect("attached diff rides the payload");
        assert_eq!(diff.lines[0].kind, DiffLineKind::Added);
    }

    #[test]
    fn build_payload_model_yaml_is_omitted_without_a_gather_entry() {
        // No gather outcome for this model (a direct render / explore
        // path) → key omitted from the wire, the template hides the
        // section; pre-#247 payload shapes stay byte-stable.
        let node = model_node("model.shop.dim_y", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_y")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert!(payload.models[0].model_yaml.is_none());
        let json = serde_json::to_string(&payload.models[0]).expect("serialize");
        assert!(
            !json.contains("model_yaml"),
            "absent model_yaml must be omitted from the wire; got {json}",
        );
    }

    #[test]
    fn model_yaml_payload_degrades_truthfully_per_outcome() {
        // Every degrade arm yields a `missing` copy naming exactly what is
        // absent — the section never renders empty or misleading. The
        // wording lives HERE (Rust computes, JS only renders), so these
        // facts pin it.
        use crate::domain::ModelYamlOutcome as O;

        let no_patch = model_yaml_payload(Some(&O::NoPatchPath), "dim_x").expect("payload");
        assert!(no_patch.raw.is_none() && no_patch.path.is_none());
        let msg = no_patch.missing.expect("degrade copy");
        assert!(
            msg.contains("No schema file declares this model"),
            "names the absence: {msg}"
        );

        let no_root = model_yaml_payload(
            Some(&O::NoProjectRoot {
                path: "models/schema.yml".to_owned(),
            }),
            "dim_x",
        )
        .expect("payload");
        assert_eq!(no_root.path.as_deref(), Some("models/schema.yml"));
        let msg = no_root.missing.expect("degrade copy");
        assert!(msg.contains("models/schema.yml"), "names the file: {msg}");
        assert!(
            msg.contains("--project-root"),
            "names the remediation flag: {msg}"
        );

        let missing_file = model_yaml_payload(
            Some(&O::FileMissing {
                path: "models/gone.yml".to_owned(),
            }),
            "dim_x",
        )
        .expect("payload");
        let msg = missing_file.missing.expect("degrade copy");
        assert!(msg.contains("models/gone.yml"), "names the file: {msg}");
        assert!(msg.contains("not found"), "states the failure: {msg}");

        let unreadable = model_yaml_payload(
            Some(&O::Unreadable {
                path: "models/locked.yml".to_owned(),
            }),
            "dim_x",
        )
        .expect("payload");
        let msg = unreadable.missing.expect("degrade copy");
        assert!(msg.contains("models/locked.yml"), "names the file: {msg}");
        assert!(
            msg.contains("could not be read"),
            "states the failure: {msg}"
        );

        let not_found = model_yaml_payload(
            Some(&O::EntryNotFound {
                path: "models/schema.yml".to_owned(),
            }),
            "dim_x",
        )
        .expect("payload");
        let msg = not_found.missing.expect("degrade copy");
        assert!(msg.contains("dim_x"), "names the model: {msg}");
        assert!(msg.contains("models/schema.yml"), "names the file: {msg}");

        assert!(
            model_yaml_payload(None, "dim_x").is_none(),
            "no gather outcome → no payload (section hidden)"
        );
    }

    #[test]
    fn build_payload_raw_sql_is_none_when_node_has_no_raw_code() {
        // Defensive — older manifests / hand-crafted fixtures lacking
        // `raw_code` produce `raw_sql = None`, which the template handler
        // hides the section for.
        let node = model_node("model.shop.stg_y", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_y")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        assert!(payload.models[0].raw_sql.is_none());
    }

    #[test]
    fn build_payload_raw_sql_is_none_when_raw_code_is_empty_string() {
        // dbt populates `raw_code: ""` for some node types (e.g. seeds);
        // treat empty string identically to `None` so the template doesn't
        // render an empty Model SQL section.
        let node = model_node_with_raw("model.shop.stg_z", "body", Some("select 1"), Some(""));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_z")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        assert!(payload.models[0].raw_sql.is_none());
    }

    #[test]
    fn build_payload_emits_empty_tests_for_modified_model_with_no_unit_tests() {
        let compiled = "select 1";
        let node = model_node("model.shop.no_test", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let in_scope = InScopeSet::new();
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.no_test")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
        );
        assert_eq!(payload.models.len(), 1);
        assert!(payload.models[0].tests.is_empty());
    }

    #[test]
    fn build_payload_skips_a_model_missing_from_manifest_nodes() {
        let manifest = manifest_for(vec![], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.ghost")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert!(payload.models.is_empty());
    }

    #[test]
    fn build_payload_widens_to_all_tests_on_in_scope_model_tagging_changed() {
        // cute-dbt#91 widening (replaces the obsolete in-scope-id-missing-
        // from-manifest test — the indexer is now models-driven, so that
        // defensive path no longer exists). A model in scope carries EVERY
        // unit test targeting it — both the updated (changed) ones and the
        // context (unchanged) siblings — each tagged via the `changed`
        // set. This is the modest render-scope widening that makes the
        // report's All-tests mode + per-model total count work.
        let node = model_node("model.shop.dim_x", "body", Some("select 1"));
        let updated = simple_unit_test("dim_x", "test_updated");
        let context = simple_unit_test("dim_x", "test_context");
        let manifest = manifest_for(
            vec![node],
            vec![
                ("unit_test.shop.test_updated", updated),
                ("unit_test.shop.test_context", context),
            ],
        );
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_x")]);
        // Only test_updated is changed; test_context is a non-updated
        // sibling carried purely by the widening.
        let changed = InScopeSet::from_iter(["unit_test.shop.test_updated".to_owned()]);
        let payload = build_payload(
            &manifest,
            &changed,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert_eq!(payload.models.len(), 1);
        let tests = &payload.models[0].tests;
        assert_eq!(
            tests.len(),
            2,
            "both the updated test and its context sibling are carried",
        );
        let updated_p = tests
            .iter()
            .find(|t| t.name == "test_updated")
            .expect("updated test present");
        let context_p = tests
            .iter()
            .find(|t| t.name == "test_context")
            .expect("context sibling present");
        assert!(updated_p.changed, "the updated test is tagged changed");
        assert!(
            !context_p.changed,
            "the context sibling is tagged not-changed"
        );
    }

    #[test]
    fn build_payload_skips_an_in_scope_test_whose_target_model_is_missing() {
        // Defensive `index_in_scope_tests_by_model` path: a unit test
        // exists in the manifest but its `model:` selector resolves to
        // None (target model not in manifest). The indexer skips it.
        let ut = UnitTest::new(
            "test_ghost",
            NodeId::new("ghost_model"),
            vec![],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![], vec![("unit_test.shop.test_ghost", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_ghost".to_owned()]);
        let models = ModelInScopeSet::new();
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        // No models in scope → no payload entries; the indexer's None
        // branch simply doesn't contribute.
        assert!(payload.models.is_empty());
    }

    #[test]
    fn build_payload_terminal_node_carries_model_name_as_label_not_id() {
        // cute-dbt#155: the terminal node's *id* is the stable engine name
        // (`TERMINAL_NODE_NAME`); the model's bare name rides as a *display
        // label* only. Keeping the model name out of the id is what stops a
        // self-named import CTE from colliding with the terminal.
        let compiled = "with src as (select * from raw_x) select * from src";
        let node = model_node("model.shop.final_one", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.final_one")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let model = &payload.models[0];
        let terminal = model
            .dag
            .nodes
            .iter()
            .find(|n| n.role == NodeRole::Final)
            .expect("terminal node present");
        assert_eq!(
            terminal.id, TERMINAL_NODE_NAME,
            "terminal id is the stable engine name, not the model name",
        );
        assert_eq!(
            terminal.label.as_deref(),
            Some("final_one.sql"),
            "the terminal's DISPLAY label is the model's `.sql` file name",
        );
    }

    #[test]
    fn build_payload_compiled_sql_keyed_by_node_id() {
        let compiled = "with stg_x_src as (select * from raw_x) select * from stg_x_src";
        let node = model_node("model.shop.x", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let model = &payload.models[0];
        assert!(
            model.compiled_sql.contains_key("stg_x_src"),
            "import node compiled SQL keyed by CTE name: {:?}",
            model.compiled_sql.keys().collect::<Vec<_>>(),
        );
        assert!(
            model.compiled_sql.contains_key(TERMINAL_NODE_NAME),
            "terminal node compiled SQL keyed by the stable terminal id (cute-dbt#155)",
        );
    }

    /// cute-dbt#445 TDD #2 (render level): the derived `compiled_sql[id]`
    /// byte-equals `code_map.compiled[node_spans[id].byte_range()]` for every
    /// node — the source map is the single source of truth.
    #[test]
    fn compiled_sql_byte_equals_code_map_slice_for_every_node() {
        let compiled = "with a as (select 1), b as (select * from a) select * from b";
        let node = model_node("model.shop.x", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let model = &payload.models[0];
        let code_map = model
            .code_map
            .as_ref()
            .expect("compiled model has a code map");
        // The faithful text is the model's compiled code verbatim.
        assert_eq!(code_map.compiled, compiled);
        // Every node-span slices the faithful text to the compiled_sql value.
        assert_eq!(
            code_map.node_spans.keys().collect::<Vec<_>>(),
            model.compiled_sql.keys().collect::<Vec<_>>(),
            "node_spans.keys() == compiled_sql.keys()"
        );
        for (id, span) in &code_map.node_spans {
            assert_eq!(
                &code_map.compiled[span.byte_range()],
                model.compiled_sql.get(id).unwrap(),
                "compiled_sql[{id}] byte-equals the faithful slice"
            );
        }
        // The terminal slice byte-equals the legacy trimmed terminal text
        // (Blocker-1): it must NOT carry the leading `) ` glue.
        let terminal = model.compiled_sql.get(TERMINAL_NODE_NAME).unwrap();
        assert!(
            terminal.starts_with("select"),
            "terminal slice is post-trim (no leading glue): {terminal:?}"
        );
    }

    /// cute-dbt#445 TDD #6 (the fitness function, here as a unit-level
    /// structural check): every `dag.nodes[].id` has a `CteBody` entry in the
    /// model's `code_map.node_spans`, or the model has no compiled code. The
    /// always-on `source-map-completeness` CI gate (`.github/workflows/ci.yml`)
    /// enforces the same invariant over the committed example payloads; this
    /// pins it on the renderer at unit level.
    #[test]
    fn every_dag_node_has_a_code_map_entry() {
        let compiled = "with a as (select 1), b as (select * from a) select * from b";
        let node = model_node("model.shop.x", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let model = &payload.models[0];
        let code_map = model
            .code_map
            .as_ref()
            .expect("compiled model has a code map");
        for n in &model.dag.nodes {
            assert!(
                code_map.node_spans.contains_key(&n.id),
                "dag node {:?} has a CteBody entry",
                n.id
            );
        }
    }

    /// cute-dbt#448: `gather_raw_zones` projects `Zone` entries into
    /// `raw_zones` with the derived 3-state `presence` string + node back-ref.
    /// A `compiled: None` zone (the incremental-guard compiled-OUT case) is
    /// `compiled_out` with `node_id: None` — type-incapable of an edge.
    #[test]
    fn from_source_map_projects_zone_entries_into_raw_zones() {
        let sm = SourceMap {
            compiled: "select 1".to_owned(),
            entries: vec![
                SourceMapEntry {
                    role: SpanRole::CteBody {
                        node_id: TERMINAL_NODE_NAME.to_owned(),
                    },
                    raw: None,
                    compiled: Some(SourceSpan {
                        start: SourcePos {
                            line: 1,
                            col: 1,
                            byte: 0,
                        },
                        end: SourcePos {
                            line: 1,
                            col: 9,
                            byte: 8,
                        },
                    }),
                },
                SourceMapEntry {
                    role: SpanRole::Zone {
                        kind: ZoneKind::IncrementalGuard,
                    },
                    raw: Some(SourceSpan {
                        start: SourcePos {
                            line: 1,
                            col: 1,
                            byte: 0,
                        },
                        end: SourcePos {
                            line: 1,
                            col: 5,
                            byte: 4,
                        },
                    }),
                    compiled: None,
                },
            ],
        };
        let payload = CodeMapPayload::from_source_map(&sm);
        // The CteBody entry projects into node_spans (not raw_zones).
        assert_eq!(payload.node_spans.len(), 1);
        assert!(payload.node_spans.contains_key(TERMINAL_NODE_NAME));
        // The Zone entry projects into raw_zones with kind + raw start/end +
        // the derived presence string + node back-ref.
        assert_eq!(payload.raw_zones.len(), 1, "the Zone arm is projected");
        assert_eq!(payload.raw_zones[0].kind, ZoneKind::IncrementalGuard);
        assert_eq!(payload.raw_zones[0].start.byte, 0);
        assert_eq!(payload.raw_zones[0].end.byte, 4);
        assert_eq!(
            payload.raw_zones[0].presence, "compiled_out",
            "compiled: None ⇒ the honest pruned verdict"
        );
        assert!(
            payload.raw_zones[0].node_id.is_none(),
            "compiled_out ⇒ no node edge (never-a-false-claim)"
        );
    }

    // ── cute-dbt#448 (Z1): the hand-rolled tag-boundary scanner ──

    #[test]
    fn scan_locates_an_incremental_guard() {
        let raw = "select *\nfrom events\n{% if is_incremental() %}\nwhere ts > 0\n{% endif %}";
        let zones = locate_raw_zones(raw);
        assert_eq!(zones.len(), 1, "one incremental-guard zone");
        assert_eq!(zones[0].kind, ZoneKind::IncrementalGuard);
        // The raw span covers the whole {% if … %}…{% endif %} construct.
        let covered = &raw[zones[0].raw_span.byte_range()];
        assert!(covered.starts_with("{% if is_incremental()"));
        assert!(covered.ends_with("{% endif %}"));
    }

    #[test]
    fn scan_locates_a_for_loop() {
        let raw = "case status\n{% for s in statuses %}when {{ s }} then 1\n{% endfor %}\nend";
        let zones = locate_raw_zones(raw);
        assert_eq!(zones.len(), 1, "one for-loop zone");
        assert_eq!(zones[0].kind, ZoneKind::ForLoop);
        let covered = &raw[zones[0].raw_span.byte_range()];
        assert!(covered.starts_with("{% for s in statuses %}"));
        assert!(covered.ends_with("{% endfor %}"));
    }

    #[test]
    fn scan_pairs_nested_if_inside_for_by_depth_stack() {
        // {% for %} containing {% if is_incremental() %}: the depth-stack pairs
        // the inner endif to the inner if, NOT first-endif-wins.
        let raw = "{% for x in xs %}{% if is_incremental() %}a{% endif %}b{% endfor %}";
        let zones = locate_raw_zones(raw);
        // Two zones: the inner incremental guard AND the outer for-loop.
        assert_eq!(zones.len(), 2, "inner guard + outer for-loop");
        let kinds: Vec<ZoneKind> = zones.iter().map(|z| z.kind).collect();
        assert!(kinds.contains(&ZoneKind::IncrementalGuard));
        assert!(kinds.contains(&ZoneKind::ForLoop));
        // The inner guard closes FIRST (popped before the for-loop).
        assert_eq!(
            zones[0].kind,
            ZoneKind::IncrementalGuard,
            "inner endif pairs to the inner if (depth-stack, not first-endif-wins)"
        );
        // Inner guard's raw span is strictly inside the for-loop's raw span.
        let outer = zones
            .iter()
            .find(|z| z.kind == ZoneKind::ForLoop)
            .unwrap()
            .raw_span;
        assert!(outer.contains_range(&zones[0].raw_span));
    }

    #[test]
    fn scan_handles_whitespace_control_trim_markers() {
        // {%- … -%} whitespace control: the raw span stays anchored on the raw
        // delimiters (the {%- is included).
        let raw = "x\n{%- if is_incremental() -%}\ny\n{%- endif -%}\nz";
        let zones = locate_raw_zones(raw);
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].kind, ZoneKind::IncrementalGuard);
        let covered = &raw[zones[0].raw_span.byte_range()];
        assert!(covered.starts_with("{%- if is_incremental()"));
        assert!(covered.ends_with("-%}"));
    }

    #[test]
    fn scan_comment_swallows_an_inner_tag() {
        // {# {% if %} #}: the comment swallows the inner tag — NO zone, and no
        // dangling opener to unbalance the scan.
        let raw = "select 1 {# {% if is_incremental() %} #} from t";
        assert!(
            locate_raw_zones(raw).is_empty(),
            "a tag inside a comment is not a zone"
        );
    }

    #[test]
    fn scan_string_literal_does_not_close_the_tag_early() {
        // A `%}` inside a string literal must NOT close the {% … %} tag.
        let raw = "{% if is_incremental() and x == '%}' %}body{% endif %}";
        let zones = locate_raw_zones(raw);
        assert_eq!(
            zones.len(),
            1,
            "a percent-brace inside a string literal does not close the tag early"
        );
        assert_eq!(zones[0].kind, ZoneKind::IncrementalGuard);
        let covered = &raw[zones[0].raw_span.byte_range()];
        assert!(covered.ends_with("{% endif %}"));
    }

    #[test]
    fn scan_unbalanced_yields_empty_fail_closed() {
        // An opener with no closer → empty vec (fail-closed), never a panic.
        assert!(locate_raw_zones("{% if is_incremental() %}body").is_empty());
        // A closer with no opener → empty.
        assert!(locate_raw_zones("body{% endif %}").is_empty());
        // A mismatched pair (if…endfor) → empty.
        assert!(locate_raw_zones("{% if is_incremental() %}x{% endfor %}").is_empty());
        // An unterminated tag → empty (never hangs).
        assert!(locate_raw_zones("{% if is_incremental() ").is_empty());
        // Empty input → empty.
        assert!(locate_raw_zones("").is_empty());
    }

    #[test]
    fn scan_plain_if_pairs_but_emits_no_zone() {
        // A non-incremental {% if %} pairs correctly (so it does not unbalance a
        // sibling zone) but emits NO v0.1 zone (L9).
        let raw = "{% if foo %}a{% endif %}\n{% for x in xs %}b{% endfor %}";
        let zones = locate_raw_zones(raw);
        assert_eq!(zones.len(), 1, "only the for-loop is a zone");
        assert_eq!(zones[0].kind, ZoneKind::ForLoop);
    }

    // ── cute-dbt#448 (Z2): zone facts → SpanRole::Zone entries + presence ──

    #[test]
    fn append_zone_entries_incremental_guard_pruned_is_compiled_out() {
        // The {% if is_incremental() %} guard's body is ABSENT from a fresh
        // compile (is_incremental()==false) → compiled: None, presence
        // compiled_out, node_id: null.
        let raw = "select *\nfrom events\n{% if is_incremental() %}\nwhere ts > (select max(ts) from this)\n{% endif %}";
        let compiled = "select *\nfrom events"; // the guard body is pruned
        let mut sm = SourceMap {
            compiled: compiled.to_owned(),
            entries: vec![SourceMapEntry {
                role: SpanRole::CteBody {
                    node_id: TERMINAL_NODE_NAME.to_owned(),
                },
                raw: None,
                compiled: byte_span(compiled, 0, compiled.len()),
            }],
        };
        append_zone_entries(&mut sm, raw, compiled);
        let payload = CodeMapPayload::from_source_map(&sm);
        assert_eq!(payload.raw_zones.len(), 1);
        let z = &payload.raw_zones[0];
        assert_eq!(z.kind, ZoneKind::IncrementalGuard);
        assert_eq!(z.presence, "compiled_out", "guard pruned this build");
        assert!(z.node_id.is_none(), "compiled_out ⇒ no node edge");
    }

    #[test]
    fn append_zone_entries_shape_a_for_loop_is_structural() {
        // A {% for %} expanding columns INSIDE one CTE body: the loop's literal
        // tokens land inside the terminal CteBody span → Structural, bound to
        // the containing node. The loop body carries a UNIQUE literal anchor
        // ("when unique_status_marker then") that appears exactly once in the
        // unrolled compiled output, so the ambiguity-safe resolver (FIX B) can
        // soundly bind it (a loop whose body literal REPEATS verbatim per
        // iteration is genuinely unbindable — see
        // `resolve_zone_compiled_ambiguous_anchor_is_none_not_a_false_bind`).
        let raw = "select\n  case status\n  {% for s in statuses %}when unique_status_marker then {{ s }}\n  {% endfor %}\n  end as label\nfrom events";
        // The compiled text contains the loop's UNIQUE literal fragment
        // ("when unique_status_marker then") inside the (whole-text) terminal
        // body, exactly once.
        let compiled = "select\n  case status\n  when unique_status_marker then 1\n  end as label\nfrom events";
        let mut sm = SourceMap {
            compiled: compiled.to_owned(),
            entries: vec![SourceMapEntry {
                role: SpanRole::CteBody {
                    node_id: TERMINAL_NODE_NAME.to_owned(),
                },
                raw: None,
                compiled: byte_span(compiled, 0, compiled.len()),
            }],
        };
        append_zone_entries(&mut sm, raw, compiled);
        let payload = CodeMapPayload::from_source_map(&sm);
        assert_eq!(payload.raw_zones.len(), 1, "one for-loop zone");
        let z = &payload.raw_zones[0];
        assert_eq!(z.kind, ZoneKind::ForLoop);
        assert_eq!(
            z.presence, "structural",
            "the loop expands inside one CTE body (strictly nested)"
        );
        assert_eq!(
            z.node_id.as_deref(),
            Some(TERMINAL_NODE_NAME),
            "structural zone binds to its containing node"
        );
    }

    #[test]
    fn longest_literal_fragment_skips_jinja_constructs() {
        // The anchor is the longest literal run with Jinja stripped.
        let body = "{% if x %}select distinct id from {{ ref('t') }} where active{% endif %}";
        let frag = longest_literal_fragment(body).expect("a literal fragment");
        assert_eq!(frag, "select distinct id from");
    }

    #[test]
    fn resolve_zone_compiled_returns_none_when_anchor_absent() {
        // A zone whose literal anchor is not in the compiled text → None
        // (degrade to absence, never a fabricated Some).
        let raw = "{% if is_incremental() %}where unique_marker_xyz > 0{% endif %}";
        let zone = &locate_raw_zones(raw)[0];
        let compiled = "select * from events"; // anchor absent
        assert!(resolve_zone_compiled(raw, &zone.raw_span, compiled).is_none());
    }

    // ── cute-dbt#448 FIX B (CodeRabbit Major, never-a-false-claim): the zone
    // anchor must resolve UNAMBIGUOUSLY. A pruned guard whose literal also
    // appears OUTSIDE the actual zone region must NOT fabricate a Some — it
    // degrades to None (honest CompiledOut), never a false bind. ──

    #[test]
    fn resolve_zone_compiled_ambiguous_anchor_is_none_not_a_false_bind() {
        // The guard's longest literal anchor ("where status = 'active'") ALSO
        // appears verbatim elsewhere in `compiled` (the guard itself was pruned
        // this build). Binding the FIRST occurrence would claim CompiledIn at a
        // region OUTSIDE the zone — a false claim. The ambiguity-safe resolver
        // refuses to bind and returns None (honest CompiledOut).
        let raw = "{% if is_incremental() %}where status = 'active'{% endif %}";
        let zone = &locate_raw_zones(raw)[0];
        // The anchor "where status = 'active'" occurs TWICE in compiled, and
        // neither is the (pruned) zone region — ambiguous, must not bind.
        let compiled = "select * from a where status = 'active'\n\
                        union all\n\
                        select * from b where status = 'active'";
        assert!(
            resolve_zone_compiled(raw, &zone.raw_span, compiled).is_none(),
            "a multiply-occurring anchor must NOT fabricate a Some (never-a-false-claim)"
        );
    }

    #[test]
    fn append_zone_entries_ambiguous_anchor_resolves_to_compiled_out() {
        // End-to-end through the projection: the same ambiguous-anchor guard →
        // presence compiled_out, node_id null (no fabricated edge), NOT a false
        // structural/compiled_in.
        let raw =
            "select *\nfrom events\n{% if is_incremental() %}where status = 'active'{% endif %}";
        let compiled = "select * from a where status = 'active'\n\
                        union all\n\
                        select * from b where status = 'active'";
        let mut sm = SourceMap {
            compiled: compiled.to_owned(),
            entries: vec![SourceMapEntry {
                role: SpanRole::CteBody {
                    node_id: TERMINAL_NODE_NAME.to_owned(),
                },
                raw: None,
                compiled: byte_span(compiled, 0, compiled.len()),
            }],
        };
        append_zone_entries(&mut sm, raw, compiled);
        let payload = CodeMapPayload::from_source_map(&sm);
        assert_eq!(payload.raw_zones.len(), 1);
        let z = &payload.raw_zones[0];
        assert_eq!(
            z.presence, "compiled_out",
            "ambiguous anchor ⇒ honest absence, never a fabricated presence"
        );
        assert!(
            z.node_id.is_none(),
            "no fabricated node edge from an ambiguous anchor"
        );
    }

    #[test]
    fn resolve_zone_compiled_prefers_a_unique_shorter_anchor_over_an_ambiguous_longer_one() {
        // The longest literal ("where status = 'active'") is ambiguous (appears
        // twice), but a shorter UNIQUE literal fragment ("and rare_marker_q > 0")
        // is present exactly once → bind that one (degrade gracefully to the
        // next-best unambiguous candidate, never lie, never give up early).
        let raw = "{% if is_incremental() %}where status = 'active'\n\
                   {{ adapter }}and rare_marker_q > 0{% endif %}";
        let zone = &locate_raw_zones(raw)[0];
        let compiled = "select * from a where status = 'active'\n\
                        union all\n\
                        select * from b where status = 'active' and rare_marker_q > 0";
        let span = resolve_zone_compiled(raw, &zone.raw_span, compiled)
            .expect("the unique shorter fragment resolves");
        let matched = &compiled[span.byte_range()];
        assert_eq!(
            matched, "and rare_marker_q > 0",
            "binds the UNIQUE fragment, not the first occurrence of the ambiguous one"
        );
    }

    // ── cute-dbt#469 (S1): raw_node_spans / raw_column_spans projection ──

    #[test]
    fn from_source_map_projects_raw_node_spans_for_a_verbatim_cte() {
        // A verbatim WITH CTE: the raw_scan adapter fills its raw span; the
        // projection surfaces it under raw_node_spans, keyed by node id. The
        // terminal node has no verbatim name token → omitted in S1.
        let raw = "with stg as (select 1 as id) select id from stg";
        let compiled = "with stg as (select 1 as id) select id from stg";
        let mut sm = SourceMap {
            compiled: compiled.to_owned(),
            entries: vec![
                SourceMapEntry {
                    role: SpanRole::CteBody {
                        node_id: "stg".to_owned(),
                    },
                    raw: None,
                    compiled: byte_span(compiled, 5, 28),
                },
                SourceMapEntry {
                    role: SpanRole::CteBody {
                        node_id: TERMINAL_NODE_NAME.to_owned(),
                    },
                    raw: None,
                    compiled: byte_span(compiled, 29, compiled.len()),
                },
            ],
        };
        crate::adapters::raw_scan::fill_raw_spans(&mut sm, raw);
        let payload = CodeMapPayload::from_source_map(&sm);
        let stg = payload
            .raw_node_spans
            .get("stg")
            .expect("verbatim CTE emits a raw_node_spans entry");
        assert_eq!(&raw[stg.byte_range()], "stg as (select 1 as id)");
        assert!(
            !payload.raw_node_spans.contains_key(TERMINAL_NODE_NAME),
            "terminal node has no verbatim raw name token ⇒ omitted in S1"
        );
    }

    #[test]
    fn from_source_map_omits_raw_column_span_for_a_templated_column() {
        // A column whose name appears ONLY inside a Jinja region is masked away
        // → no unique anchor → the raw_column_spans key is OMITTED (never a
        // fabricated offset). A sibling verbatim column DOES anchor.
        let raw = "select {{ macro_col('templated_col') }}, plain_col from t";
        let compiled = "select rendered, plain_col from t";
        let key = |c: &str| format!("{TERMINAL_NODE_NAME}\u{1f}{c}");
        let mut sm = SourceMap {
            compiled: compiled.to_owned(),
            entries: vec![
                SourceMapEntry {
                    role: SpanRole::Column {
                        node_id: TERMINAL_NODE_NAME.to_owned(),
                        column: "templated_col".to_owned(),
                    },
                    raw: None,
                    compiled: byte_span(compiled, 7, 15),
                },
                SourceMapEntry {
                    role: SpanRole::Column {
                        node_id: TERMINAL_NODE_NAME.to_owned(),
                        column: "plain_col".to_owned(),
                    },
                    raw: None,
                    compiled: byte_span(compiled, 17, 26),
                },
            ],
        };
        crate::adapters::raw_scan::fill_raw_spans(&mut sm, raw);
        let payload = CodeMapPayload::from_source_map(&sm);
        assert!(
            !payload.raw_column_spans.contains_key(&key("templated_col")),
            "a templated column (masked away) ⇒ no raw_column_spans key"
        );
        let plain = payload
            .raw_column_spans
            .get(&key("plain_col"))
            .expect("a verbatim column anchors uniquely");
        assert_eq!(&raw[plain.byte_range()], "plain_col");
    }

    #[test]
    fn from_source_map_raw_spans_empty_when_no_scanner_run() {
        // Honest absence: from_cte_graph leaves every raw: None and (with no
        // fill_raw_spans call) both raw projections are empty, so the wire (and
        // the goldens) stay byte-stable for models without resolvable raw spans.
        let compiled = "with a as (select 1) select * from a";
        let sm = SourceMap {
            compiled: compiled.to_owned(),
            entries: vec![SourceMapEntry {
                role: SpanRole::CteBody {
                    node_id: "a".to_owned(),
                },
                raw: None,
                compiled: byte_span(compiled, 5, 20),
            }],
        };
        let payload = CodeMapPayload::from_source_map(&sm);
        assert!(payload.raw_node_spans.is_empty());
        assert!(payload.raw_column_spans.is_empty());
        // The omit-when-empty serde keeps the key off the wire entirely.
        let json = serde_json::to_string(&payload).unwrap();
        assert!(!json.contains("raw_node_spans"));
        assert!(!json.contains("raw_column_spans"));
    }

    // ── cute-dbt#448 FIX C (CodeRabbit Major, fail-closed): an orphan mid-block
    // tag (`{% else %}` / `{% elif %}` with no opener on the depth-stack) is
    // malformed Jinja → empty vec, never letting a later zone emit. ──

    #[test]
    fn scan_orphan_else_yields_empty_fail_closed() {
        // An `{% else %}` with NO `{% if %}` opener is malformed — fail-closed.
        assert!(
            locate_raw_zones("select 1\n{% else %}\nselect 2").is_empty(),
            "an orphan else (no opener) ⇒ empty (fail-closed)"
        );
        // An orphan `{% elif %}` is equally malformed.
        assert!(
            locate_raw_zones("select 1\n{% elif x %}\nselect 2").is_empty(),
            "an orphan elif (no opener) ⇒ empty (fail-closed)"
        );
        // And it must NOT let a LATER well-formed zone emit from a broken stream.
        assert!(
            locate_raw_zones("{% else %}\n{% for x in xs %}a{% endfor %}").is_empty(),
            "an orphan else must abort the whole scan, not just its own region"
        );
    }

    #[test]
    fn scan_well_placed_else_inside_a_block_still_pairs() {
        // A WELL-PLACED `{% else %}` (inside an open block) is fine — the block
        // still pairs and emits its zone. The orphan guard must not regress this.
        let raw = "{% if is_incremental() %}a{% else %}b{% endif %}";
        let zones = locate_raw_zones(raw);
        assert_eq!(zones.len(), 1, "a well-placed else does not break pairing");
        assert_eq!(zones[0].kind, ZoneKind::IncrementalGuard);
    }

    // ── cute-dbt#448 FIX D (CodeRabbit Major, classifier correctness): the
    // IncrementalGuard classification matches an ACTUAL `is_incremental()` CALL,
    // not a bare substring. ──

    #[test]
    fn classify_is_incremental_matches_an_actual_call_not_a_substring() {
        // A real `is_incremental()` call IS a guard (zone emitted).
        let raw = "{% if is_incremental() %}a{% endif %}";
        let zones = locate_raw_zones(raw);
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].kind, ZoneKind::IncrementalGuard);

        // A larger identifier merely CONTAINING `is_incremental` is NOT a guard
        // (it is a plain `{% if %}` — pairs, emits no zone).
        assert!(
            locate_raw_zones("{% if some_is_incremental_flag %}a{% endif %}").is_empty(),
            "`some_is_incremental_flag` is a plain if, not an incremental guard"
        );

        // A QUOTED string containing `is_incremental` is data, not a call.
        assert!(
            locate_raw_zones("{% if x == 'is_incremental' %}a{% endif %}").is_empty(),
            "a quoted 'is_incremental' literal is not a call ⇒ plain if, no zone"
        );

        // The bare token with no call parens is also not a call.
        assert!(
            locate_raw_zones("{% if is_incremental %}a{% endif %}").is_empty(),
            "`is_incremental` with no `(` is not a call ⇒ plain if, no zone"
        );

        // Whitespace between the token and `(` is still a call.
        let zones = locate_raw_zones("{% if is_incremental () %}a{% endif %}");
        assert_eq!(zones.len(), 1, "whitespace before `(` is still a call");
        assert_eq!(zones[0].kind, ZoneKind::IncrementalGuard);
    }

    // ── cute-dbt#448: per-helper unit coverage for the decomposed
    // `mentions_is_incremental_call` predicates (the char-scanning broken into
    // small single-purpose fns so each stays well under the strict CC gate). ──

    #[test]
    fn is_word_byte_classifies_identifier_chars() {
        for b in [b'a', b'Z', b'0', b'9', b'_'] {
            assert!(is_word_byte(b), "{} is a word char", b as char);
        }
        for b in [b' ', b'(', b'\'', b'-', b'.'] {
            assert!(!is_word_byte(b), "{} is not a word char", b as char);
        }
    }

    #[test]
    fn token_boundary_helpers_honor_identifier_edges() {
        // `xis_incremental` — left byte is a word char ⇒ left boundary fails.
        let b = b"xis_incremental";
        assert!(!token_left_boundary_ok(b, 1));
        // Token at the very start ⇒ left boundary ok.
        assert!(token_left_boundary_ok(b"is_incremental", 0));
        // Right boundary: next byte `_` is a word char ⇒ fails.
        assert!(!token_right_boundary_ok(b"is_incremental_x", 14));
        // Right boundary at end-of-string ⇒ ok.
        assert!(token_right_boundary_ok(b"is_incremental", 14));
        // Right boundary: next byte `(` is not a word char ⇒ ok.
        assert!(token_right_boundary_ok(b"is_incremental(", 14));
    }

    #[test]
    fn call_paren_follows_skips_whitespace_then_requires_open_paren() {
        assert!(call_paren_follows(b"(", 0));
        assert!(call_paren_follows(b"   (", 0), "leading ws then `(`");
        assert!(!call_paren_follows(b"   ", 0), "ws only, no `(`");
        assert!(!call_paren_follows(b"x", 0), "non-`(` byte");
        assert!(!call_paren_follows(b"", 0), "empty ⇒ no `(`");
    }

    #[test]
    fn offset_in_string_literal_detects_quote_state() {
        // Inside a single-quoted literal.
        let s = b"x == 'is_incremental'";
        assert!(
            offset_in_string_literal(s, 6),
            "offset 6 is inside the quote"
        );
        // The same buffer, AFTER the closing quote ⇒ not inside.
        assert!(!offset_in_string_literal(s, 21));
        // Outside any quote.
        assert!(!offset_in_string_literal(b"is_incremental()", 0));
        // Inside a DOUBLE-quoted literal (the other quote arm).
        assert!(offset_in_string_literal(b"x == \"abc\"", 6));
        // A `\`-escape inside a quote does not prematurely close it: the
        // escaped `'` is consumed, so the offset past it is still inside.
        assert!(
            offset_in_string_literal(b"'a\\'b'c", 4),
            "escaped quote keeps the literal open"
        );
        // A trailing lone backslash at end-of-buffer is handled (no overrun).
        assert!(offset_in_string_literal(b"'a\\", 3), "unterminated escape");
    }

    #[test]
    fn step_string_scan_advances_and_tracks_quotes() {
        // Opening a quote from the neutral state.
        let mut q: Option<u8> = None;
        let next = step_string_scan(b"'a'", 0, &mut q);
        assert_eq!((next, q), (1, Some(b'\'')));
        // Closing the matching quote.
        let mut q = Some(b'\'');
        let next = step_string_scan(b"'a'", 2, &mut q);
        assert_eq!((next, q), (3, None));
        // An escape inside a quote skips two bytes and keeps the quote open.
        let mut q = Some(b'\'');
        let next = step_string_scan(b"\\'", 0, &mut q);
        assert_eq!((next, q), (2, Some(b'\'')));
        // A double-quote opener (the other neutral-state arm).
        let mut q: Option<u8> = None;
        let next = step_string_scan(b"\"x", 0, &mut q);
        assert_eq!((next, q), (1, Some(b'"')));
        // A neutral, non-quote byte just advances by one.
        let mut q: Option<u8> = None;
        let next = step_string_scan(b"ab", 0, &mut q);
        assert_eq!((next, q), (1, None));
    }

    /// cute-dbt#445 TDD #4 (the inlined-data assertion, without a browser): the
    /// serialized model payload — the data script the headless report reads —
    /// carries `code_map.compiled` + `node_spans`. The headless leg
    /// (cute-dbt#446) drives the live `<pre>` sync; this pins the wire shape.
    #[test]
    fn serialized_payload_carries_code_map_compiled_and_node_spans() {
        let compiled = "with a as (select 1) select * from a";
        let node = model_node("model.shop.x", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let json = serde_json::to_string(&payload.models[0]).unwrap();
        assert!(json.contains("\"code_map\""), "code_map present: {json}");
        assert!(
            json.contains("\"node_spans\""),
            "node_spans present: {json}"
        );
        // The faithful compiled text is embedded under code_map.compiled.
        assert!(
            json.contains("with a as (select 1) select * from a"),
            "faithful compiled text embedded: {json}"
        );
    }

    /// cute-dbt#445: a model with NO compiled code (a `dbt parse` shape /
    /// seed-like node) omits `code_map` entirely — the key is absent so older
    /// fixtures without the field stay byte-stable.
    #[test]
    fn no_compiled_code_omits_code_map() {
        // model_node with None compiled → no compiled_code.
        let node = model_node("model.shop.uncompiled", "body", None);
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.uncompiled")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let model = &payload.models[0];
        assert!(model.code_map.is_none(), "no compiled code ⇒ no code_map");
        let json = serde_json::to_string(model).unwrap();
        assert!(
            !json.contains("code_map"),
            "code_map key omitted from the wire: {json}"
        );
    }

    #[test]
    fn self_named_import_cte_does_not_collapse_into_the_terminal() {
        // cute-dbt#155 regression: a model named `orders` whose import CTE
        // is *also* named `orders` (the idiomatic `with orders as (...)`)
        // must render TWO distinct DAG nodes — not collapse into one node
        // with a spurious `orders ↔ final` cycle and the terminal's SQL
        // clobbering the import CTE's body in the compiled_sql map.
        let compiled = "with orders as (select * from raw_orders), \
                              final as (select * from orders) \
                         select * from final";
        let node = model_node("model.shop.orders", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let model = &payload.models[0];

        // (1) every node id is unique — the core invariant the bug violated.
        let ids: Vec<&str> = model.dag.nodes.iter().map(|n| n.id.as_str()).collect();
        let unique: std::collections::BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(
            ids.len(),
            unique.len(),
            "node ids must be unique, got {ids:?}"
        );

        // (2) the import CTE and the terminal are SEPARATE nodes with the
        //     correct roles (no Import/Final inversion).
        let import = model
            .dag
            .nodes
            .iter()
            .find(|n| n.id == "orders")
            .expect("import CTE node keyed by its own name");
        assert_eq!(
            import.role,
            NodeRole::Import,
            "import keeps the Import role"
        );
        let terminal = model
            .dag
            .nodes
            .iter()
            .find(|n| n.role == NodeRole::Final)
            .expect("terminal node present");
        assert_ne!(
            terminal.id, "orders",
            "terminal id must NOT collide with the import CTE",
        );
        assert_eq!(
            terminal.label.as_deref(),
            Some("orders.sql"),
            "terminal DISPLAYS the model's `.sql` file name — visually distinct \
             from the same-named import CTE",
        );

        // (3) no self-cycle: no edge whose endpoints are the same id, and
        //     specifically not the spurious `final -> orders` back-edge.
        assert!(
            model.dag.edges.iter().all(|e| e.from != e.to),
            "no edge may point a node at itself: {:?}",
            model
                .dag
                .edges
                .iter()
                .map(|e| (&e.from, &e.to))
                .collect::<Vec<_>>(),
        );
        assert!(
            !model
                .dag
                .edges
                .iter()
                .any(|e| e.from == "final" && e.to == "orders"),
            "the spurious final->orders back-edge must be gone",
        );

        // (4) the import node shows ITS OWN body, not the terminal's.
        let import_sql = &model.compiled_sql["orders"];
        assert!(
            import_sql.contains("raw_orders"),
            "import node keeps its own SQL: {import_sql}",
        );
        assert!(
            !import_sql.contains("from final"),
            "import node SQL must not be overwritten by the terminal's: {import_sql}",
        );
    }

    #[test]
    fn build_payload_no_with_model_is_one_terminal_cte_body_entry() {
        // A `select 1` body has no WITH clause → empty CteGraph (the engine
        // emits no nodes). cute-dbt#445: the source map synthesizes ONE
        // terminal CteBody entry over the whole text, so compiled_sql keys by
        // the stable terminal id (NOT the bare model name as v1 did) and
        // node_spans.keys() == compiled_sql.keys() by construction.
        let compiled = "select 1";
        let node = model_node("model.shop.flat", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.flat")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let model = &payload.models[0];
        // The single terminal entry carries the whole body, keyed by the
        // stable terminal id.
        assert_eq!(
            model.compiled_sql.get(TERMINAL_NODE_NAME).unwrap(),
            compiled,
            "no-WITH body keyed by the stable terminal id, not the model name"
        );
        assert!(
            !model.compiled_sql.contains_key("flat"),
            "no longer keyed by the bare model name (cute-dbt#445 key fix)"
        );
        // The code_map projection carries the same body + node-span table.
        let code_map = model
            .code_map
            .as_ref()
            .expect("compiled model has a code map");
        assert_eq!(code_map.compiled, compiled);
        // node_spans.keys() == compiled_sql.keys() by construction.
        let node_keys: Vec<&String> = code_map.node_spans.keys().collect();
        let slice_keys: Vec<&String> = model.compiled_sql.keys().collect();
        assert_eq!(node_keys, slice_keys);
        assert_eq!(node_keys, vec![&TERMINAL_NODE_NAME.to_owned()]);
    }

    #[test]
    fn build_payload_handles_unparseable_compiled_code_gracefully() {
        // The engine returns CteError::Parse for garbage SQL. The renderer
        // treats that as an empty graph, NOT a hard failure — the report
        // still ships, the model card just has no DAG. cute-dbt#445: the
        // faithful (unparseable) text still surfaces as the single terminal
        // CteBody entry over the whole text.
        let node = model_node("model.shop.broken", "body", Some("not valid sql {"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.broken")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert_eq!(payload.models.len(), 1);
        let model = &payload.models[0];
        // Empty graph → empty nodes/edges, but the faithful body still
        // surfaces, keyed by the stable terminal id.
        assert!(model.dag.nodes.is_empty());
        assert_eq!(
            model.compiled_sql.get(TERMINAL_NODE_NAME).unwrap(),
            "not valid sql {",
        );
        // The code_map carries the faithful (even unparseable) text verbatim.
        assert_eq!(model.code_map.as_ref().unwrap().compiled, "not valid sql {",);
    }

    #[test]
    fn build_payload_test_carries_metadata_fields() {
        let compiled = "select 1";
        let node = model_node("model.shop.m", "body", Some(compiled));
        let ut = simple_unit_test("m", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(test.description.as_deref(), Some("a description"));
        assert_eq!(test.tags.as_ref().unwrap(), &vec!["finance".to_owned()]);
        assert!(test.meta.is_some());
        assert_eq!(test.defined_in.as_deref(), Some("models/x.yml"));
    }

    #[test]
    fn build_payload_given_binds_import_cte_when_ref_matches() {
        let compiled = "with stg_orders_src as (select * from raw_orders) \
                        select * from stg_orders_src";
        let node = model_node("model.shop.stg_orders", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("stg_orders"),
            vec![UnitTestGiven::new(
                "ref('stg_orders_src')",
                json!([]),
                None,
                None,
            )],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_orders")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(
            test.given[0].bound_to_node.as_deref(),
            Some("stg_orders_src"),
            "ref('stg_orders_src') binds to its matching import-CTE node",
        );
    }

    #[test]
    fn build_payload_given_binds_import_cte_via_body_table_reference() {
        // The dbt-idiomatic shape: import CTE is named `source` (the
        // unwrapper convention), but its body references the model the
        // unit test mocks. The renderer's pass-2 body match must catch
        // this — pass-1 name match misses it.
        let compiled = "with source as (\
                          select * from \"jaffle_shop\".\"main\".\"raw_customers\"\
                        ) select customer_id, first_name from source";
        let node = model_node("model.shop.stg_customers", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("stg_customers"),
            vec![UnitTestGiven::new(
                "ref('raw_customers')",
                json!([]),
                None,
                None,
            )],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_customers")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(
            test.given[0].bound_to_node.as_deref(),
            Some("source"),
            "ref('raw_customers') binds to the import-CTE `source` via its body table reference",
        );
    }

    #[test]
    fn build_payload_messy_multi_ref_cte_binds_every_given_to_one_node() {
        // cute-dbt#34 medium scope: a single CTE whose body references
        // MULTIPLE `ref()` targets (here via UNION ALL) must surface ALL
        // matching unit-test givens, not just the first. The engine
        // classifies this body as Transform (multi-source, not the
        // import-CTE shape) but populates body_leaf_table_refs with both
        // leaves; pass-2 binds against any leaf node with those refs.
        let compiled = "with raw_union as (\
                          select id, kind from \"db\".\"schema\".\"raw_orders\" \
                          union all \
                          select id, kind from \"db\".\"schema\".\"raw_returns\"\
                        ) select * from raw_union";
        let node = model_node("model.shop.union_model", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("union_model"),
            vec![
                UnitTestGiven::new("ref('raw_orders')", json!([{"id": 1}]), None, None),
                UnitTestGiven::new("ref('raw_returns')", json!([{"id": 9}]), None, None),
            ],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.union_model")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(
            test.given[0].bound_to_node.as_deref(),
            Some("raw_union"),
            "ref('raw_orders') binds to the multi-ref CTE via body-leaf match; \
             got bound_to_node={:?}",
            test.given[0].bound_to_node,
        );
        assert_eq!(
            test.given[1].bound_to_node.as_deref(),
            Some("raw_union"),
            "ref('raw_returns') binds to the SAME multi-ref CTE; \
             got bound_to_node={:?}",
            test.given[1].bound_to_node,
        );
    }

    #[test]
    fn build_payload_messy_join_cte_binds_both_givens() {
        // Variant of the multi-ref case where the messy CTE body is a
        // single SELECT with a JOIN — also not the import shape, also
        // populates body_leaf_table_refs with both joined leaves.
        let compiled = "with joined_src as (\
                          select o.id, c.name \
                          from \"db\".\"schema\".\"raw_orders\" o \
                          inner join \"db\".\"schema\".\"raw_customers\" c on c.id = o.cid\
                        ) select * from joined_src";
        let node = model_node("model.shop.join_model", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("join_model"),
            vec![
                UnitTestGiven::new("ref('raw_orders')", json!([]), None, None),
                UnitTestGiven::new("ref('raw_customers')", json!([]), None, None),
            ],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.join_model")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(test.given[0].bound_to_node.as_deref(), Some("joined_src"));
        assert_eq!(test.given[1].bound_to_node.as_deref(), Some("joined_src"));
    }

    #[test]
    fn build_payload_pass_1_requires_both_name_match_and_import_role() {
        // Pass-1 matching is `name == target AND role == Import`. If a
        // graph contains an import CTE with the WRONG name and a
        // transform CTE with the RIGHT name, neither pass-1 candidate
        // should bind: the import is wrong-named, and the transform's
        // role disqualifies it even though the name matches. With the
        // role check loosened (e.g. `||` instead of `&&`) the wrong-name
        // import would spuriously bind.
        //
        // Body match must also fail here so the test isolates pass-1:
        // the import's raw_sql references an unrelated table, and the
        // transform CTE's body refers only to other CTEs.
        let compiled = "with not_target as (select * from \"db\".\"schema\".\"unrelated\"), \
             target as (select * from not_target) \
             select * from target";
        let node = model_node("model.shop.x", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("x"),
            vec![UnitTestGiven::new("ref('target')", json!([]), None, None)],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert!(
            test.given[0].bound_to_node.is_none(),
            "matching name on a non-import node must NOT bind (role gate honored); \
             got bound_to_node={:?}",
            test.given[0].bound_to_node,
        );
    }

    #[test]
    fn build_payload_given_does_not_bind_when_no_matching_import_cte() {
        let compiled = "select 1";
        let node = model_node("model.shop.flat", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("flat"),
            vec![UnitTestGiven::new(
                "ref('nonexistent')",
                json!([]),
                None,
                None,
            )],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.flat")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert!(test.given[0].bound_to_node.is_none());
    }

    // ===== source() given binding (cute-dbt#57) =====

    /// The canonical staging-model shape: `with source as (select * from
    /// {{ source('synthea_raw', 'patients') }})` compiles to a quoted
    /// three-part relation inside an import CTE named `source`.
    fn patients_source_model() -> Node {
        let compiled = "with source as (\
                          select * from \"memory\".\"main\".\"patients\"\
                        ) select id, name from source";
        model_node("model.shop.stg_patients", "body", Some(compiled))
    }

    fn source_given_unit_test(input: &str) -> UnitTest {
        UnitTest::new(
            "test_one",
            NodeId::new("stg_patients"),
            vec![UnitTestGiven::new(input, json!([]), None, None)],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
    }

    fn bound_node_for(manifest: &Manifest) -> Option<String> {
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_patients")]);
        let payload = build_payload(
            manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        payload.models[0].tests[0].given[0].bound_to_node.clone()
    }

    #[test]
    fn build_payload_source_given_binds_via_resolved_relation_body_match() {
        // The cute-dbt#57 vertical: `source('synthea_raw','patients')`
        // resolves through the manifest sources map to the physical
        // identifier `patients`, which pass-2 finds in the `source`
        // CTE's body-leaf refs (the compiled relation and the manifest
        // relation_name render from the same relation object).
        let manifest = manifest_with_sources(
            vec![patients_source_model()],
            vec![(
                "unit_test.shop.test_one",
                source_given_unit_test("source('synthea_raw', 'patients')"),
            )],
            vec![SourceNode::new(
                NodeId::new("source.shop.synthea_raw.patients"),
                "synthea_raw",
                "patients",
                Some("patients".to_owned()),
                "main",
                Some("memory".to_owned()),
                Some("\"memory\".\"main\".\"patients\"".to_owned()),
            )],
        );
        assert_eq!(
            bound_node_for(&manifest).as_deref(),
            Some("source"),
            "source('synthea_raw','patients') binds to the import CTE via the resolved relation",
        );
    }

    #[test]
    fn build_payload_source_given_binds_through_an_identifier_override() {
        // The YAML `name` and the physical `identifier` differ — the
        // lookup must run on (source_name, name) while the body match
        // must run on `identifier`.
        let compiled = "with src as (\
                          select * from \"memory\".\"main\".\"patients_v2\"\
                        ) select id from src";
        let node = model_node("model.shop.stg_patients", "body", Some(compiled));
        let manifest = manifest_with_sources(
            vec![node],
            vec![(
                "unit_test.shop.test_one",
                source_given_unit_test("source('synthea_raw', 'patients')"),
            )],
            vec![SourceNode::new(
                NodeId::new("source.shop.synthea_raw.patients"),
                "synthea_raw",
                "patients",
                Some("patients_v2".to_owned()),
                "main",
                Some("memory".to_owned()),
                Some("\"memory\".\"main\".\"patients_v2\"".to_owned()),
            )],
        );
        assert_eq!(bound_node_for(&manifest).as_deref(), Some("src"));
    }

    #[test]
    fn build_payload_source_given_binds_with_a_fusion_minimal_source_entry() {
        // Fusion-style entry: identifier and relation_name keys absent.
        // The token falls back to the source's `name` (dbt's identifier
        // default), which still matches the compiled body leaf.
        let manifest = manifest_with_sources(
            vec![patients_source_model()],
            vec![(
                "unit_test.shop.test_one",
                source_given_unit_test("source('synthea_raw', 'patients')"),
            )],
            vec![SourceNode::new(
                NodeId::new("source.shop.synthea_raw.patients"),
                "synthea_raw",
                "patients",
                None,
                "main",
                None,
                None,
            )],
        );
        assert_eq!(bound_node_for(&manifest).as_deref(), Some("source"));
    }

    #[test]
    fn build_payload_source_given_does_not_bind_when_pair_is_unresolvable() {
        // No matching (source_name, name) in the sources map — the given
        // stays unbound and the node-detail panel keeps its empty-state
        // copy (fail-open, no PreflightError; sources need no preflight).
        let manifest = manifest_with_sources(
            vec![patients_source_model()],
            vec![(
                "unit_test.shop.test_one",
                source_given_unit_test("source('other_block', 'patients')"),
            )],
            vec![SourceNode::new(
                NodeId::new("source.shop.synthea_raw.patients"),
                "synthea_raw",
                "patients",
                Some("patients".to_owned()),
                "main",
                Some("memory".to_owned()),
                Some("\"memory\".\"main\".\"patients\"".to_owned()),
            )],
        );
        assert!(bound_node_for(&manifest).is_none());
    }

    #[test]
    fn build_payload_source_given_does_not_bind_without_a_sources_map() {
        // Pre-#57 shape: the manifest carries no sources block at all.
        let manifest = manifest_for(
            vec![patients_source_model()],
            vec![(
                "unit_test.shop.test_one",
                source_given_unit_test("source('synthea_raw', 'patients')"),
            )],
        );
        assert!(bound_node_for(&manifest).is_none());
    }

    #[test]
    fn build_payload_binds_a_ref_given_and_a_source_given_in_one_test() {
        // The cute-dbt#57 AC shape: one model with BOTH a ref()-based
        // import CTE and a source()-based import CTE; the unit test
        // mocks both inputs and each given binds to its own CTE node.
        let compiled = "with orders as (\
                          select * from \"memory\".\"main\".\"raw_orders\"\
                        ), patients as (\
                          select * from \"memory\".\"raw\".\"patients\"\
                        ) select o.id, p.name from orders o join patients p on p.id = o.pid";
        let node = model_node("model.shop.mixed_inputs", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("mixed_inputs"),
            vec![
                UnitTestGiven::new("ref('raw_orders')", json!([{"id": 1}]), None, None),
                UnitTestGiven::new(
                    "source('synthea_raw', 'patients')",
                    json!([{"id": 1, "name": "x"}]),
                    None,
                    None,
                ),
            ],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_with_sources(
            vec![node],
            vec![("unit_test.shop.test_one", ut)],
            vec![SourceNode::new(
                NodeId::new("source.shop.synthea_raw.patients"),
                "synthea_raw",
                "patients",
                Some("patients".to_owned()),
                "raw",
                Some("memory".to_owned()),
                Some("\"memory\".\"raw\".\"patients\"".to_owned()),
            )],
        );
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.mixed_inputs")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(
            test.given[0].bound_to_node.as_deref(),
            Some("orders"),
            "the ref() given binds to the ref-based import CTE",
        );
        assert_eq!(
            test.given[1].bound_to_node.as_deref(),
            Some("patients"),
            "the source() given binds to the source-based import CTE",
        );
    }

    #[test]
    fn build_payload_propagates_is_recursive_flag() {
        // A WITH RECURSIVE query: the engine flags the graph; the
        // renderer threads that to the model payload so the template can
        // surface a "recursive arm omitted" banner.
        let compiled = "with recursive r(n) as (select 1 union all select n+1 from r) \
                        select * from r";
        let node = model_node("model.shop.rec", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.rec")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert!(payload.models[0].is_recursive);
    }

    // ===== cute-dbt#145: incremental-model unit-test semantics =====

    /// Build a model node whose `config.materialized == "incremental"`.
    fn incremental_model_node(id: &str, compiled: Option<&str>) -> Node {
        let mut cfg = BTreeMap::new();
        cfg.insert("materialized".to_owned(), json!("incremental"));
        Node::new(
            NodeId::new(id),
            "model",
            checksum("body"),
            compiled.map(str::to_owned),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(cfg, false),
            None,
            BTreeMap::new(),
        )
    }

    #[test]
    fn build_payload_marks_incremental_model_and_not_table() {
        // Incremental model ⇒ is_incremental true.
        let inc = incremental_model_node("model.shop.order_events", Some("select 1"));
        let manifest = manifest_for(vec![inc], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.order_events")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert!(
            payload.models[0].is_incremental,
            "config.materialized==incremental ⇒ ModelPayload.is_incremental"
        );

        // Table model (NodeConfig::default — no materialized key) ⇒ false.
        let tbl = model_node("model.shop.orders", "body", Some("select 1"));
        let manifest = manifest_for(vec![tbl], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert!(
            !payload.models[0].is_incremental,
            "table model ⇒ not incremental"
        );
    }

    #[test]
    fn build_payload_threads_incremental_mode_and_this_given() {
        let node = incremental_model_node("model.shop.order_events", Some("select 1"));
        let ut = UnitTest::new(
            "test_order_events_incremental",
            NodeId::new("order_events"),
            vec![sample_given("this"), sample_given("ref('stg_orders')")],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
        .with_incremental_mode(Some(true));
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.t", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.t".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.order_events")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(
            test.is_incremental_mode,
            Some(true),
            "overrides mode threaded onto TestPayload"
        );
        assert!(test.given[0].is_this, "input 'this' ⇒ is_this");
        assert!(!test.given[1].is_this, "ref(...) given ⇒ not is_this");
    }

    /// The Rust→JS key contract (advisor-flagged blind spot): the template's
    /// JS reads `m.is_incremental`, `t.is_incremental_mode`, and
    /// `given.is_this` by those EXACT `snake_case` keys — no `#[serde(rename)]`.
    /// An incremental-mode test's payload emits all three; a plain table/ref
    /// payload OMITS them (`skip_serializing_if`), so non-incremental
    /// fixtures stay byte-identical on the wire.
    #[test]
    fn payload_serde_wire_shape_for_incremental_keys() {
        // Incremental model + incremental-mode test + `this` given ⇒ keys present.
        let node = incremental_model_node("model.shop.order_events", Some("select 1"));
        let ut = UnitTest::new(
            "t",
            NodeId::new("order_events"),
            vec![sample_given("this")],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
        .with_incremental_mode(Some(true));
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.t", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.t".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.order_events")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let json = serde_json::to_string(&payload.models[0]).unwrap();
        assert!(
            json.contains("\"is_incremental\":true"),
            "model key: {json}"
        );
        assert!(
            json.contains("\"is_incremental_mode\":true"),
            "test key: {json}"
        );
        assert!(json.contains("\"is_this\":true"), "given key: {json}");

        // Non-incremental: table model + ref given + no override ⇒ all three OMITTED.
        let tbl = model_node("model.shop.orders", "body", Some("select 1"));
        let ut2 = UnitTest::new(
            "t2",
            NodeId::new("orders"),
            vec![sample_given("ref('stg_orders')")],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        ); // no with_incremental_mode ⇒ None
        let manifest = manifest_for(vec![tbl], vec![("unit_test.shop.t2", ut2)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.t2".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let payload = build_payload(
            &manifest,
            &in_scope,
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let json = serde_json::to_string(&payload.models[0]).unwrap();
        // "is_incremental" is a substring of "is_incremental_mode", so its
        // absence guards both model + test keys at once.
        assert!(
            !json.contains("is_incremental"),
            "non-incremental model omits is_incremental + is_incremental_mode: {json}"
        );
        assert!(
            !json.contains("is_this"),
            "ref(...) given omits is_this: {json}"
        );
    }

    // ===== payload_json_for_html_script =====

    #[test]
    fn payload_json_escapes_closing_script_tag_via_unicode() {
        let payload = ReportPayload {
            baseline: "</script><script>alert(1)</script>".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
            governance: GovernanceFacts::default(),
            macro_lens: None,
            pr_ref: None,
            seed_cards: Vec::new(),
            pr_dag: None,
            removed_models: Vec::new(),
            pr_comments: None,
        };
        let serialized = payload_json_for_html_script(&payload).unwrap();
        assert!(
            !serialized.contains("</script>"),
            "no raw </script> survives in: {serialized}",
        );
        assert!(
            serialized.contains("\\u003c/script>"),
            "`</` is replaced with `\\u003c/` in: {serialized}",
        );
    }

    #[test]
    fn payload_json_escapes_html_comment_open_via_unicode() {
        let payload = ReportPayload {
            baseline: "x<!--hostile-->y".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
            governance: GovernanceFacts::default(),
            macro_lens: None,
            pr_ref: None,
            seed_cards: Vec::new(),
            pr_dag: None,
            removed_models: Vec::new(),
            pr_comments: None,
        };
        let serialized = payload_json_for_html_script(&payload).unwrap();
        assert!(
            !serialized.contains("<!--"),
            "no raw <!-- survives in: {serialized}",
        );
        assert!(
            serialized.contains("\\u003c!--"),
            "`<!` is replaced with `\\u003c!` in: {serialized}",
        );
    }

    #[test]
    fn payload_json_leaves_bare_left_angle_alone() {
        // Only `</` and `<!` are dangerous in HTML5 script-data state;
        // a bare `<` followed by a space or other char is fine.
        let payload = ReportPayload {
            baseline: "a < b".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
            governance: GovernanceFacts::default(),
            macro_lens: None,
            pr_ref: None,
            seed_cards: Vec::new(),
            pr_dag: None,
            removed_models: Vec::new(),
            pr_comments: None,
        };
        let serialized = payload_json_for_html_script(&payload).unwrap();
        assert!(serialized.contains("a < b"), "bare `<` is preserved");
    }

    #[test]
    fn payload_json_escapes_le_comparisons_for_sloppy_scanners() {
        // cute-dbt#200 — authored model/column descriptions carry SQL-ish
        // prose like `encounter_start_at <= current_timestamp` (the
        // committed playground fixture); `tl`-based extractors mis-scan a
        // raw `<=`, so it joins the #170 escape set. Round-trips intact.
        let payload = ReportPayload {
            baseline: "encounter_start_at <= current_timestamp".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
            governance: GovernanceFacts::default(),
            macro_lens: None,
            pr_ref: None,
            seed_cards: Vec::new(),
            pr_dag: None,
            removed_models: Vec::new(),
            pr_comments: None,
        };
        let serialized = payload_json_for_html_script(&payload).unwrap();
        assert!(
            !serialized.contains("<="),
            "no raw <= survives: {serialized}"
        );
        assert!(
            serialized.contains("\\u003c="),
            "`<=` escapes to `\\u003c=`: {serialized}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&serialized).expect("escaped output is valid JSON");
        assert_eq!(
            parsed["baseline"],
            serde_json::Value::String("encounter_start_at <= current_timestamp".to_owned()),
        );
    }

    #[test]
    fn payload_json_escapes_tag_like_angle_brackets() {
        // cute-dbt#170 — check-spec prose like `WHERE <right>.<key> IS
        // NULL` rides the payload now; `<letter` shapes must not read as
        // markup to tag scanners (the tl-based test extractors choked on
        // them), while staying JSON-decodable to the original text.
        let payload = ReportPayload {
            baseline: "filters WHERE <right>.<key> IS NULL <?".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
            governance: GovernanceFacts::default(),
            macro_lens: None,
            pr_ref: None,
            seed_cards: Vec::new(),
            pr_dag: None,
            removed_models: Vec::new(),
            pr_comments: None,
        };
        let serialized = payload_json_for_html_script(&payload).unwrap();
        assert!(
            !serialized.contains("<right>") && !serialized.contains("<key>"),
            "no raw tag-like sequence survives: {serialized}"
        );
        assert!(
            serialized.contains("\\u003cright>") && serialized.contains("\\u003c?"),
            "tag openers escape to \\u003c: {serialized}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&serialized).expect("escaped output is valid JSON");
        assert_eq!(
            parsed["baseline"],
            serde_json::Value::String("filters WHERE <right>.<key> IS NULL <?".to_owned()),
        );
    }

    #[test]
    fn payload_json_output_is_round_trippable_through_json_parse() {
        // The Unicode escape must remain valid JSON; serde_json round-trips
        // it back to the original string.
        let original = ReportPayload {
            baseline: "</script><!--end".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
            governance: GovernanceFacts::default(),
            macro_lens: None,
            pr_ref: None,
            seed_cards: Vec::new(),
            pr_dag: None,
            removed_models: Vec::new(),
            pr_comments: None,
        };
        let serialized = payload_json_for_html_script(&original).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&serialized).expect("escaped output is valid JSON");
        assert_eq!(
            parsed["baseline"],
            serde_json::Value::String("</script><!--end".to_owned()),
            "round-trip recovers the original baseline value",
        );
    }

    // ===== seed cards payload + "Data tables" render view (cute-dbt#350) =====
    // `Cell` / `CellValue` / the diff types are imported by the cute-dbt#98
    // block below (same `tests` module); only `TableRow` is new here.

    use crate::domain::TableRow;

    fn payload_with_seed_section(seed_cards: Vec<SeedSectionCard>) -> ReportPayload {
        ReportPayload {
            baseline: "baseline.json".to_owned(),
            models: vec![],
            manifest_nodes: BTreeMap::new(),
            check_specs: BTreeMap::new(),
            project_definition: None,
            project_change_panel: None,
            governance: GovernanceFacts::default(),
            macro_lens: None,
            pr_ref: None,
            seed_cards,
            pr_dag: None,
            removed_models: Vec::new(),
            pr_comments: None,
        }
    }

    /// A `FixtureTable` with one `id` column and `n` integer rows (0..n).
    fn seed_table(n: usize) -> FixtureTable {
        let rows = (0..n)
            .map(|i| TableRow::new(vec![Cell::new(CellValue::Number(i.to_string()))]))
            .collect();
        FixtureTable::new(vec!["id".to_owned()], rows)
    }

    /// A raw `SeedCard` carrying `table` with `n` rows (no diff).
    fn raw_seed_card_with_rows(n: usize) -> SeedCard {
        let mut card = SeedCard::new(
            NodeId::new("seed.shop.raw_customers"),
            "raw_customers",
            Some("seeds/raw_customers.csv".to_owned()),
            vec!["stg_customers".to_owned()],
        );
        card.table = Some(seed_table(n));
        card
    }

    #[test]
    fn empty_seed_section_is_omitted_from_the_json_payload() {
        // The byte-identity guard: no seed in scope (or the experiment off) ⇒
        // the `seed_cards` key is absent from JSON, so every seed-free golden
        // stays identical. `build_seed_section(&[], _)` yields an empty vec.
        let view = build_seed_section(&[], DEFAULT_SEED_ROW_CAP);
        let serialized =
            payload_json_for_html_script(&payload_with_seed_section(view)).expect("serialize");
        assert!(
            !serialized.contains("seed_cards"),
            "an empty seed section must not appear in the JSON: {serialized}",
        );
    }

    #[test]
    fn build_seed_section_carries_identity_lineage_and_under_cap_table() {
        // Under-cap: the whole table crosses; total == shown; not capped.
        let view = build_seed_section(&[raw_seed_card_with_rows(3)], 500);
        assert_eq!(view.len(), 1);
        let c = &view[0];
        assert_eq!(c.id, "seed.shop.raw_customers");
        assert_eq!(c.name, "raw_customers");
        assert_eq!(c.feeds_models, vec!["stg_customers".to_owned()]);
        assert_eq!(c.total_rows, 3);
        assert_eq!(c.shown_rows, 3);
        assert!(!c.capped);
        assert_eq!(c.table.as_ref().expect("table present").rows.len(), 3);
    }

    #[test]
    fn build_seed_section_caps_the_current_table_and_records_the_true_total() {
        // Over-cap: the table truncates to `cap` rows, but `total_rows` keeps
        // the TRUE pre-cap count so the JS can label "showing N of M rows"
        // honestly. `capped` flags the truncation.
        let view = build_seed_section(&[raw_seed_card_with_rows(10)], 4);
        let c = &view[0];
        assert_eq!(c.total_rows, 10);
        assert_eq!(c.shown_rows, 4);
        assert!(c.capped);
        assert_eq!(c.table.as_ref().expect("table present").rows.len(), 4);
        // The truncation keeps the FIRST `cap` rows in source order.
        let first = &c.table.as_ref().unwrap().rows[0].cells[0];
        assert_eq!(first.display, "0");
    }

    #[test]
    fn build_seed_section_cap_zero_renders_no_data_rows_but_keeps_the_header() {
        // cap = 0 is legal (header + "showing 0 of M rows" note only).
        let view = build_seed_section(&[raw_seed_card_with_rows(5)], 0);
        let c = &view[0];
        assert_eq!(c.total_rows, 5);
        assert_eq!(c.shown_rows, 0);
        assert!(c.capped);
        let table = c
            .table
            .as_ref()
            .expect("an empty-rows table still renders the header");
        assert!(table.rows.is_empty());
        assert_eq!(table.columns, vec!["id".to_owned()]);
    }

    #[test]
    fn build_seed_section_degrades_truthfully_when_table_is_none() {
        // The #126 lesson: a seed whose data could not be read keeps
        // `table: None` AND zero counts — the JS renders the labeled
        // "data unavailable" state, never a silent empty grid.
        let card = SeedCard::new(
            NodeId::new("seed.shop.lonely"),
            "lonely",
            Some("seeds/lonely.csv".to_owned()),
            Vec::new(),
        );
        let view = build_seed_section(&[card], 500);
        let c = &view[0];
        assert!(c.table.is_none());
        assert_eq!(c.total_rows, 0);
        assert_eq!(c.shown_rows, 0);
        assert!(!c.capped);
    }

    #[test]
    fn build_seed_section_never_caps_the_cell_diff() {
        // The diff is intrinsically bounded by the edit size; capping it would
        // hide the change under review. A card with a 6-row diff and a low cap
        // keeps every diff row even as its current table truncates.
        let mut card = raw_seed_card_with_rows(6);
        let diff_rows: Vec<RowChange> = (0..6)
            .map(|i| RowChange {
                kind: RowChangeKind::Added,
                cells: vec![CellChange {
                    old: Cell::new(CellValue::Absent),
                    new: Cell::new(CellValue::Number(i.to_string())),
                    changed: true,
                }],
            })
            .collect();
        card.diff = Some(FixtureTableDiff {
            columns: vec![DiffColumn {
                name: "id".to_owned(),
                status: ColumnStatus::Present,
            }],
            rows: diff_rows,
        });
        let view = build_seed_section(&[card], 2);
        let c = &view[0];
        // Current table capped to 2…
        assert_eq!(c.shown_rows, 2);
        assert!(c.capped);
        // …but the diff carries all 6 rows.
        assert_eq!(c.diff.as_ref().expect("diff present").rows.len(), 6);
    }

    #[test]
    fn populated_seed_section_serializes_with_the_render_view_shape() {
        // The wire-shape proof: the JS reads name/feeds_models/total_rows/
        // shown_rows/capped + the capped table. Pins the keys the report JS
        // branches on.
        let view = build_seed_section(&[raw_seed_card_with_rows(10)], 4);
        let serialized =
            payload_json_for_html_script(&payload_with_seed_section(view)).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&serialized).expect("valid JSON");
        let cards = parsed["seed_cards"].as_array().expect("array");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["name"], serde_json::json!("raw_customers"));
        assert_eq!(
            cards[0]["feeds_models"],
            serde_json::json!(["stg_customers"])
        );
        assert_eq!(cards[0]["total_rows"], serde_json::json!(10));
        assert_eq!(cards[0]["shown_rows"], serde_json::json!(4));
        assert_eq!(cards[0]["capped"], serde_json::json!(true));
        assert_eq!(
            cards[0]["table"]["rows"]
                .as_array()
                .expect("rows array")
                .len(),
            4
        );
    }

    // ===== governance surfaces (cute-dbt#260, Slice 0) =====

    /// Render a one-model report carrying `governance`, returning the HTML.
    /// The model `model.shop.x` declares `group: finance`; the manifest
    /// carries a `finance` group with an owner so the chip can resolve.
    fn render_html_with_governance(filename: &str, governance: &GovernanceFacts) -> String {
        let node = model_node("model.shop.x", "body", Some("select 1"))
            .with_governance(Some("finance".to_owned()), None);
        let mut groups = HashMap::new();
        groups.insert(
            "group.shop.finance".to_owned(),
            Group::new(
                "finance",
                Some(Owner::new(
                    Some("Finance Team".to_owned()),
                    vec!["finance@corp.example".to_owned()],
                )),
            ),
        );
        let manifest = manifest_for(vec![node], vec![]).with_groups(groups);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join(filename);
        let _ = std::fs::remove_file(&tmp);
        render_report_with_externals(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            ScopeSource::PrDiff,
            "t",
            None,
            &CheckPolicy::default(),
            &ProjectFacts::default(),
            governance,
            None,
            None,
            &[],
            DEFAULT_SEED_ROW_CAP,
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &[],
            None,
        )
        .expect("report renders");
        std::fs::read_to_string(&tmp).expect("read rendered report")
    }

    #[test]
    fn governance_off_emits_zero_governance_dom() {
        // The off-gate render: an empty GovernanceFacts (the cli passes
        // this when the experiment is disabled) must add NO governance
        // DOM — the byte-identity property the golden gate depends on.
        let html = render_html_with_governance(
            "cute_dbt_render_governance_off.html",
            &GovernanceFacts::default(),
        );
        // Match the actual DOM nodes, not the bare class tokens: a later
        // slice / the Design pass inlines `.governance-panel { … }` CSS,
        // which would false-PASS a `!contains("governance-panel")` check
        // even if the section wrongly rendered (CodeRabbit on #334).
        assert!(
            !html.contains(r#"<section class="governance-panel""#),
            "no governance section element in the DOM when the payload is empty",
        );
        assert!(
            !html.contains(r#"data-testid="governance-panel""#),
            "no governance panel test hook when the payload is empty",
        );
        assert!(
            !html.contains(r#"data-testid="gov-group-chip""#),
            "no group chip in the DOM when the payload is empty",
        );
    }

    #[test]
    fn governance_on_renders_the_group_owner_chip() {
        let facts = GovernanceFacts {
            group_chips: vec![GroupChip {
                group: "finance".to_owned(),
                owner_name: Some("Finance Team".to_owned()),
                owner_email: Some("finance@corp.example".to_owned()),
            }],
            ..GovernanceFacts::default()
        };
        let html = render_html_with_governance("cute_dbt_render_governance_on.html", &facts);
        assert!(
            html.contains(r#"<section class="governance-panel""#),
            "the governance section renders when the payload has content",
        );
        // Anchor the chip text to the actual chip node (the rendered
        // <span class="gov-group-chip" … data-group="finance">…), not to
        // bare substrings that could appear elsewhere in the DOM.
        assert!(
            html.contains(
                r#"<span class="gov-group-chip" data-testid="gov-group-chip" data-group="finance">group finance &middot; owner Finance Team &lt;finance@corp.example&gt;</span>"#
            ),
            "the group chip renders the composed group/owner/email label: {html}",
        );
    }

    #[test]
    fn governance_on_with_empty_facts_still_emits_zero_dom() {
        // The gate flows through the payload, not a config flag: even on
        // the "enabled" call path, an empty facts payload renders no DOM
        // (has_governance is content-derived, not experiment-derived).
        let html = render_html_with_governance(
            "cute_dbt_render_governance_empty_on.html",
            &GovernanceFacts::default(),
        );
        // DOM-node-targeted (CodeRabbit on #334): inlined `.governance-panel`
        // CSS must not let this false-PASS.
        assert!(!html.contains(r#"<section class="governance-panel""#));
        assert!(!html.contains(r#"data-testid="gov-group-chip""#));
    }

    // ----- cute-dbt#260 Slice 1: blast-radius panel statement -----

    #[test]
    fn governance_blast_radius_renders_the_panel_statement() {
        let facts = GovernanceFacts {
            blast_radius: vec![BlastRadius {
                exposure_label: "provider_dashboard".to_owned(),
                exposure_type: Some("dashboard".to_owned()),
                owner_name: Some("Clinical Team".to_owned()),
                owner_email: Some("clinical@corp.example".to_owned()),
                in_scope_model_count: 3,
            }],
            ..GovernanceFacts::default()
        };
        let html = render_html_with_governance("cute_dbt_render_governance_blast.html", &facts);
        // DOM-node-targeted (the #334 hardening pattern): assert the real
        // <p> node + its full composed statement, not a bare token.
        assert!(
            html.contains(r#"<section class="governance-panel""#),
            "the governance section renders for a blast-radius statement",
        );
        assert!(
            html.contains(
                r#"<p class="gov-blast" data-testid="gov-blast" data-exposure="provider_dashboard">Touches 3 models feeding <strong>provider_dashboard</strong> (dashboard) &mdash; owner Clinical Team &lt;clinical@corp.example&gt;</p>"#
            ),
            "the blast statement names the count, exposure, type, and owner: {html}",
        );
    }

    #[test]
    fn governance_blast_radius_singular_model_when_count_is_one() {
        let facts = GovernanceFacts {
            blast_radius: vec![BlastRadius {
                exposure_label: "dash".to_owned(),
                exposure_type: Some("dashboard".to_owned()),
                owner_name: None,
                owner_email: None,
                in_scope_model_count: 1,
            }],
            ..GovernanceFacts::default()
        };
        let html =
            render_html_with_governance("cute_dbt_render_governance_blast_singular.html", &facts);
        // "1 model" (singular), no owner clause when the owner is absent.
        assert!(
            html.contains(
                r#"<p class="gov-blast" data-testid="gov-blast" data-exposure="dash">Touches 1 model feeding <strong>dash</strong> (dashboard)</p>"#
            ),
            "singular 'model' and no owner clause: {html}",
        );
    }

    #[test]
    fn governance_blast_radius_owner_email_only_renders_bare_address() {
        let facts = GovernanceFacts {
            blast_radius: vec![BlastRadius {
                exposure_label: "dash".to_owned(),
                exposure_type: None,
                owner_name: None,
                owner_email: Some("ops@corp.example".to_owned()),
                in_scope_model_count: 2,
            }],
            ..GovernanceFacts::default()
        };
        let html =
            render_html_with_governance("cute_dbt_render_governance_blast_email_only.html", &facts);
        // No type clause, owner clause is the bare email (name absent).
        assert!(
            html.contains(
                r#"<p class="gov-blast" data-testid="gov-blast" data-exposure="dash">Touches 2 models feeding <strong>dash</strong> &mdash; owner &lt;ops@corp.example&gt;</p>"#
            ),
            "no type clause + bare-email owner clause: {html}",
        );
    }

    #[test]
    fn governance_off_emits_no_blast_radius_dom() {
        let html = render_html_with_governance(
            "cute_dbt_render_governance_no_blast.html",
            &GovernanceFacts::default(),
        );
        assert!(!html.contains(r#"data-testid="gov-blast""#));
    }

    // ----- cute-dbt#260 Slice 2: contract-class drawer -----

    #[test]
    fn governance_breaking_contract_renders_the_drawer() {
        let facts = GovernanceFacts {
            contract_classes: vec![ContractClass {
                model: "dim_orders".to_owned(),
                verdict: "breaking".to_owned(),
                chip: "Contract: enforced · v2 of 3 · access: public · group finance".to_owned(),
                column_diffs: vec![ContractColumnDiff {
                    name: "amount".to_owned(),
                    old: "int".to_owned(),
                    new: "string".to_owned(),
                    verdict: "breaking".to_owned(),
                }],
                reasons: vec!["Columns removed: legacy_id.".to_owned()],
            }],
            ..GovernanceFacts::default()
        };
        let html = render_html_with_governance("cute_dbt_render_contract_breaking.html", &facts);
        assert!(
            html.contains(r#"<section class="governance-panel""#),
            "the governance section renders for a contract class",
        );
        // DOM-node-targeted (the #334 hardening pattern): the drawer + its
        // verdict, the chip, the column-diff row + its verdict, the reason.
        assert!(
            html.contains(
                r#"<div class="gov-contract" data-testid="gov-contract" data-model="dim_orders" data-verdict="breaking">"#
            ),
            "the drawer carries the model + breaking verdict: {html}",
        );
        assert!(
            html.contains(
                r#"<span class="gov-contract-chip" data-testid="gov-contract-chip">dim_orders &middot; Contract: enforced · v2 of 3 · access: public · group finance</span>"#
            ),
            "the chip names the model + contract metadata",
        );
        assert!(
            html.contains(
                r#"<div class="gov-contract-col" data-testid="gov-contract-col" data-verdict="breaking">amount: int &rarr; string [breaking]</div>"#
            ),
            "the column-diff row renders old → new + the verdict tag",
        );
        assert!(
            html.contains(
                r#"<p class="gov-contract-reason" data-testid="gov-contract-reason">Columns removed: legacy_id.</p>"#
            ),
            "the non-column reason line renders",
        );
    }

    #[test]
    fn governance_safe_contract_renders_the_safe_verdict() {
        let facts = GovernanceFacts {
            contract_classes: vec![ContractClass {
                model: "dim_orders".to_owned(),
                verdict: "safe".to_owned(),
                chip: "Contract: enforced".to_owned(),
                column_diffs: Vec::new(),
                reasons: vec!["Contract is now enforced (newly contracted).".to_owned()],
            }],
            ..GovernanceFacts::default()
        };
        let html = render_html_with_governance("cute_dbt_render_contract_safe.html", &facts);
        assert!(
            html.contains(
                r#"data-testid="gov-contract" data-model="dim_orders" data-verdict="safe""#
            ),
            "the drawer carries the safe verdict: {html}",
        );
    }

    #[test]
    fn governance_off_emits_no_contract_drawer_dom() {
        let html = render_html_with_governance(
            "cute_dbt_render_governance_no_contract.html",
            &GovernanceFacts::default(),
        );
        assert!(!html.contains(r#"data-testid="gov-contract""#));
    }

    // ----- cute-dbt#260 Slice 4: lifecycle chips -----

    #[test]
    fn governance_lifecycle_chip_renders_with_kind_and_label() {
        let facts = GovernanceFacts {
            lifecycle_chips: vec![GovChip {
                kind: "group-owner-touch".to_owned(),
                label: "Touches group finance (owner: fin@corp.example)".to_owned(),
                severity: None,
            }],
            ..GovernanceFacts::default()
        };
        let html = render_html_with_governance("cute_dbt_render_chip_basic.html", &facts);
        // DOM-targeted (the #334 hardening pattern): the real <span> node +
        // the kind hook + the label; no severity attr when severity is None.
        assert!(
            html.contains(
                r#"<span class="gov-chip" data-testid="gov-chip" data-chip-kind="group-owner-touch">Touches group finance (owner: fin@corp.example)</span>"#
            ),
            "the chip renders its kind + label, no severity attr: {html}",
        );
    }

    #[test]
    fn governance_dual_state_chip_renders_severity_attr() {
        let facts = GovernanceFacts {
            lifecycle_chips: vec![
                GovChip {
                    kind: "ref-to-deprecated".to_owned(),
                    label: "Refs deprecated old_dim (deprecated 2020-01-01)".to_owned(),
                    severity: Some("danger".to_owned()),
                },
                GovChip {
                    kind: "ref-to-deprecated".to_owned(),
                    label: "Refs deprecated future_dim (deprecated 2099-01-01)".to_owned(),
                    severity: Some("info".to_owned()),
                },
            ],
            ..GovernanceFacts::default()
        };
        let html = render_html_with_governance("cute_dbt_render_chip_dual.html", &facts);
        assert!(
            html.contains(
                r#"<span class="gov-chip" data-testid="gov-chip" data-chip-kind="ref-to-deprecated" data-chip-severity="danger">Refs deprecated old_dim (deprecated 2020-01-01)</span>"#
            ),
            "the elapsed chip carries data-chip-severity=danger: {html}",
        );
        assert!(
            html.contains(r#"data-chip-severity="info""#),
            "the scheduled chip carries data-chip-severity=info",
        );
    }

    #[test]
    fn governance_off_emits_no_lifecycle_chip_dom() {
        let html = render_html_with_governance(
            "cute_dbt_render_no_chip.html",
            &GovernanceFacts::default(),
        );
        assert!(!html.contains(r#"data-testid="gov-chip""#));
        assert!(!html.contains("data-chip-kind"));
    }

    // ----- cute-dbt#348: config-driven meta + tags -----

    #[test]
    fn governance_model_tags_render_one_chip_each() {
        let facts = GovernanceFacts {
            meta_tags: vec![ModelMetaTags {
                model: "dim_payers".to_owned(),
                tags: vec!["analytics".to_owned(), "dimension".to_owned()],
                meta: Vec::new(),
                columns: Vec::new(),
            }],
            ..GovernanceFacts::default()
        };
        let html = render_html_with_governance("cute_dbt_render_meta_tags_chips.html", &facts);
        assert!(
            html.contains(
                r#"<span class="gov-tag-chip" data-testid="gov-tag-chip" data-tag="analytics">🏷 analytics</span>"#
            ),
            "one chip per tag, tag-hooked: {html}",
        );
        assert!(
            html.contains(r#"data-tag="dimension">🏷 dimension</span>"#),
            "the second tag chip renders too",
        );
        // No meta ⇒ no aggregate-meta chip.
        assert!(!html.contains(r#"data-testid="gov-meta-chip""#));
    }

    #[test]
    fn governance_meta_chip_is_a_focusable_tooltip_button() {
        // The aggregate-meta chip MUST be a focusable <button> carrying the
        // reused .expect-tooltip pattern + an aria-hidden bubble enumerating
        // every key:value pair — never a native `title` (cute-dbt#146).
        let facts = GovernanceFacts {
            meta_tags: vec![ModelMetaTags {
                model: "dim_payers".to_owned(),
                tags: Vec::new(),
                meta: vec![
                    MetaPair {
                        key: "contains_pii".to_owned(),
                        value: "false".to_owned(),
                    },
                    MetaPair {
                        key: "owner".to_owned(),
                        value: "clinical-quality".to_owned(),
                    },
                ],
                columns: Vec::new(),
            }],
            ..GovernanceFacts::default()
        };
        let html = render_html_with_governance("cute_dbt_render_meta_tooltip.html", &facts);
        // Focusable button + the reused .expect-tooltip class + aria-label
        // for AT (summarizes the keys), label shows the count.
        assert!(
            html.contains(
                r#"<button type="button" class="expect-tooltip gov-meta-chip" data-testid="gov-meta-chip" aria-label="2 meta entries: contains_pii, owner">meta (2)"#
            ),
            "the meta chip is a focusable button with the expect-tooltip pattern + aria-label summary: {html}",
        );
        // The bubble is aria-hidden (not announced twice) and enumerates pairs.
        assert!(
            html.contains(
                r#"<span class="expect-tooltip-bubble gov-meta-bubble" data-testid="gov-meta-bubble" aria-hidden="true">"#
            ),
            "the bubble reuses .expect-tooltip-bubble and is aria-hidden: {html}",
        );
        assert!(
            html.contains(
                r#"<span class="gov-meta-row" data-testid="gov-meta-row"><code class="gov-meta-key">owner</code>: <span class="gov-meta-val">clinical-quality</span></span>"#
            ),
            "the bubble enumerates each key:value pair: {html}",
        );
        // Never a native title attribute on the chip.
        assert!(
            !html.contains(r#"data-testid="gov-meta-chip" title="#),
            "the meta chip must not use a native title (load-bearing-tooltip rule)",
        );
    }

    #[test]
    fn governance_meta_values_are_auto_escaped() {
        // A meta value carrying markup-ish characters must be HTML-escaped
        // as text — never interpreted (askama escapes by default).
        let facts = GovernanceFacts {
            meta_tags: vec![ModelMetaTags {
                model: "dim_payers".to_owned(),
                tags: Vec::new(),
                meta: vec![MetaPair {
                    key: "note".to_owned(),
                    value: r#"<script>alert("x")</script> & "quoted""#.to_owned(),
                }],
                columns: Vec::new(),
            }],
            ..GovernanceFacts::default()
        };
        let html = render_html_with_governance("cute_dbt_render_meta_escape.html", &facts);
        assert!(
            !html.contains("<script>alert"),
            "meta value markup must not appear raw in the DOM: {html}",
        );
        // askama escapes to numeric entities by default (&#60; not &lt;).
        assert!(
            html.contains(
                "&#60;script&#62;alert(&#34;x&#34;)&#60;/script&#62; &#38; &#34;quoted&#34;"
            ),
            "meta value is HTML-escaped as text: {html}",
        );
    }

    #[test]
    fn governance_column_meta_tags_render_in_the_drawer() {
        let facts = GovernanceFacts {
            meta_tags: vec![ModelMetaTags {
                model: "dim_payers".to_owned(),
                tags: Vec::new(),
                meta: Vec::new(),
                columns: vec![ColumnMetaTags {
                    column: "payer_key".to_owned(),
                    tags: vec!["dimension_key".to_owned()],
                    meta: vec![MetaPair {
                        key: "pii".to_owned(),
                        value: "false".to_owned(),
                    }],
                }],
            }],
            ..GovernanceFacts::default()
        };
        let html = render_html_with_governance("cute_dbt_render_col_meta.html", &facts);
        assert!(
            html.contains(
                r#"<div class="gov-meta-col" data-testid="gov-meta-col" data-column="payer_key">"#
            ),
            "the per-column drawer row renders with its column hook: {html}",
        );
        assert!(
            html.contains(
                r#"data-testid="gov-col-tag-chip" data-tag="dimension_key">🏷 dimension_key</span>"#
            ),
            "the column-level tag chip renders inside the drawer row",
        );
        assert!(
            html.contains(r#"data-testid="gov-col-meta-row"><code class="gov-meta-key">pii</code>: <span class="gov-meta-val">false</span>"#),
            "the column-level meta pair renders inside the drawer row",
        );
    }

    #[test]
    fn governance_off_emits_no_meta_tags_dom() {
        let html = render_html_with_governance(
            "cute_dbt_render_no_meta_tags.html",
            &GovernanceFacts::default(),
        );
        assert!(!html.contains(r#"data-testid="gov-meta""#));
        assert!(!html.contains(r#"data-testid="gov-tag-chip""#));
        assert!(!html.contains(r#"data-testid="gov-meta-chip""#));
    }

    // ===== macro lens (cute-dbt#265, Slice B) =====

    /// A root-project `model` node calling `direct_macros`, declaring file
    /// `ofp`. Package = `shop` (the blast-radius root-project filter).
    fn macro_model(id: &str, ofp: &str, direct_macros: &[&str]) -> Node {
        Node::new(
            NodeId::new(id),
            "model",
            Checksum::new("sha256", "abc"),
            Some("select 1".to_owned()),
            None,
            DependsOn::new(
                direct_macros.iter().map(|m| (*m).to_owned()).collect(),
                vec![],
            ),
            Some(ofp.to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_identity(None, Some("shop".to_owned()))
    }

    /// A manifest with `shop` root project, the given macro-calling models,
    /// and ONE macro (`macro.shop.add_dq_flags`, file `macros/dq.sql`).
    fn macro_lens_manifest(models: Vec<Node>) -> Manifest {
        let mut macros = HashMap::new();
        macros.insert(
            "macro.shop.add_dq_flags".to_owned(),
            "{% macro add_dq_flags(col) %}\n  case when {{ col }} then 1 end\n{% endmacro %}"
                .to_owned(),
        );
        let mut identity = BTreeMap::new();
        identity.insert(
            "macro.shop.add_dq_flags".to_owned(),
            crate::domain::MacroIdentity::new(
                Some("macros/dq.sql".to_owned()),
                Some("add_dq_flags".to_owned()),
                Some("shop".to_owned()),
            ),
        );
        Manifest::new(
            ManifestMetadata::new("v12").with_project_name(Some("shop".to_owned())),
            models.into_iter().map(|n| (n.id().clone(), n)).collect(),
            HashMap::new(),
            macros,
        )
        .with_macro_identity(identity)
    }

    /// A root-project `model` calling `direct_macros` AND consuming
    /// `producers` (its `ref()` parents) — the macro-DAG downstream edges
    /// ride `depends_on.nodes`. File `<id-leaf>.sql`, package `shop`.
    fn macro_model_with_refs(id: &str, direct_macros: &[&str], producers: &[&str]) -> Node {
        let leaf = id.rsplit('.').next().unwrap_or(id);
        Node::new(
            NodeId::new(id),
            "model",
            Checksum::new("sha256", "abc"),
            Some("select 1".to_owned()),
            None,
            DependsOn::new(
                direct_macros.iter().map(|m| (*m).to_owned()).collect(),
                producers.iter().map(|p| NodeId::new(*p)).collect(),
            ),
            Some(format!("models/{leaf}.sql")),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_identity(None, Some("shop".to_owned()))
    }

    #[test]
    fn build_macro_dag_role_stamps_users_and_downstream() {
        // add_dq_flags is called by `staged` (a User). `mart` consumes
        // `staged` via ref() ⇒ `mart` is in the downstream closure
        // (Downstream). `island` calls no macro and is unrelated ⇒ absent.
        let manifest = macro_lens_manifest(vec![
            macro_model_with_refs("model.shop.staged", &["macro.shop.add_dq_flags"], &[]),
            macro_model_with_refs("model.shop.mart", &[], &["model.shop.staged"]),
            macro_model_with_refs("model.shop.island", &[], &[]),
        ]);
        let dag = build_macro_dag(&manifest, "macro.shop.add_dq_flags");
        let by_id: std::collections::BTreeMap<&str, &MacroDagNode> =
            dag.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        assert_eq!(
            by_id.get("model.shop.staged").map(|n| n.role),
            Some("user"),
            "the macro-calling model is a User",
        );
        assert_eq!(
            by_id.get("model.shop.mart").map(|n| n.role),
            Some("downstream"),
            "the ref()-downstream model is Downstream context",
        );
        assert!(
            !by_id.contains_key("model.shop.island"),
            "an unrelated model is not in the focus set",
        );
        // The dependency edge staged -> mart is induced in the DAG.
        assert!(
            dag.edges
                .iter()
                .any(|e| e.from == "model.shop.staged" && e.to == "model.shop.mart"),
            "the staged -> mart ref() edge is induced: {:?}",
            dag.edges,
        );
    }

    #[test]
    fn build_macro_dag_empty_when_no_root_model_calls_the_macro() {
        // No model calls the macro ⇒ empty blast radius ⇒ empty focus ⇒ empty
        // DAG (the Macros tab shows the honest no-models copy).
        let manifest =
            macro_lens_manifest(vec![macro_model_with_refs("model.shop.island", &[], &[])]);
        let dag = build_macro_dag(&manifest, "macro.shop.add_dq_flags");
        assert!(dag.nodes.is_empty(), "no caller ⇒ no DAG nodes");
        assert!(dag.edges.is_empty(), "no caller ⇒ no DAG edges");
    }

    #[test]
    fn changed_macro_view_carries_the_macro_dag() {
        // The macro DAG rides on the ChangedMacroView (so the Macros tab can
        // render it from the JSON payload).
        let manifest = macro_lens_manifest(vec![macro_model_with_refs(
            "model.shop.staged",
            &["macro.shop.add_dq_flags"],
            &[],
        )]);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, usize::MAX)
            .expect("a changed macro builds the lens");
        let mac = &lens.macros[0];
        assert!(
            mac.macro_dag
                .nodes
                .iter()
                .any(|n| n.id == "model.shop.staged"),
            "the impacted model is in the macro DAG",
        );
    }

    #[test]
    fn build_macro_lens_empty_changed_set_is_none() {
        // The off-gate / no-macro-changed contract: an empty set ⇒ None ⇒
        // the section omits entirely (byte-identical non-macro golden).
        let manifest = macro_lens_manifest(vec![macro_model(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.shop.add_dq_flags"],
        )]);
        assert!(
            build_macro_lens(
                &manifest,
                &BTreeSet::new(),
                ScopeSource::PrDiff,
                None,
                usize::MAX
            )
            .is_none()
        );
    }

    #[test]
    fn build_macro_lens_baseline_arm_is_exact_fidelity() {
        let manifest = macro_lens_manifest(vec![macro_model(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.shop.add_dq_flags"],
        )]);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::Baseline, None, usize::MAX)
            .expect("a changed macro builds the lens");
        assert_eq!(lens.fidelity, "exact");
        assert_eq!(lens.macros.len(), 1);
        let mac = &lens.macros[0];
        assert_eq!(mac.name, "add_dq_flags");
        assert_eq!(mac.package, "shop");
        assert_eq!(mac.impacted_count, 1);
        // Baseline arm: no diff index ⇒ plain body context lines.
        assert!(mac.diff.is_none());
        assert!(!mac.body_lines.is_empty());
        assert!(
            mac.body_lines
                .iter()
                .all(|l| l.kind == DiffLineKind::Context)
        );
    }

    #[test]
    fn build_macro_lens_pr_diff_arm_is_heuristic_with_body_diff() {
        let manifest = macro_lens_manifest(vec![macro_model(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.shop.add_dq_flags"],
        )]);
        // The diff touches the macro file's line 2; the `+` matches the
        // working-tree body so the reconstruction splices the old line.
        let diff = PrDiff {
            renames: Vec::new(),
            deleted: Vec::new(),
            added: Vec::new(),
            files: vec![FileHunks {
                path: "macros/dq.sql".to_owned(),
                hunks: vec![Hunk {
                    new_start: 2,
                    new_len: 1,
                    removed_lines: vec!["  case when {{ col }} then 0 end".to_owned()],
                    added_lines: vec!["  case when {{ col }} then 1 end".to_owned()],
                }],
            }],
        };
        let index = NormalizedDiffIndex::new(&diff, None);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(
            &manifest,
            &changed,
            ScopeSource::PrDiff,
            Some(&index),
            usize::MAX,
        )
        .expect("a changed macro builds the lens");
        assert_eq!(lens.fidelity, "heuristic");
        let mac = &lens.macros[0];
        let body_diff = mac.diff.as_ref().expect("the touched macro body diffs");
        assert!(body_diff.has_real_change());
        assert!(
            body_diff
                .lines
                .iter()
                .any(|l| l.kind == DiffLineKind::Removed),
            "the old macro line is spliced in",
        );
    }

    #[test]
    fn build_macro_lens_groups_impacted_models_into_a_directory_tree() {
        // Two models in DIFFERENT directory subtrees both reach the macro:
        // the tree must group each under its directory segments (founder D3).
        let manifest = macro_lens_manifest(vec![
            macro_model(
                "model.shop.stg_orders",
                "models/staging/stg_orders.sql",
                &["macro.shop.add_dq_flags"],
            ),
            macro_model(
                "model.shop.fct_orders",
                "models/marts/core/fct_orders.sql",
                &["macro.shop.add_dq_flags"],
            ),
        ]);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::Baseline, None, usize::MAX)
            .expect("lens builds");
        let mac = &lens.macros[0];
        assert_eq!(mac.impacted_count, 2);
        // Directory rows: models/, marts/, core/, staging/ — and 2 model
        // leaves. Every model leaf carries its full node id.
        let dirs: Vec<&str> = mac
            .tree
            .iter()
            .filter(|r| r.kind == "dir")
            .map(|r| r.label.as_str())
            .collect();
        assert!(
            dirs.contains(&"models"),
            "models/ dir row present: {dirs:?}"
        );
        assert!(dirs.contains(&"marts"), "marts/ dir row present: {dirs:?}");
        assert!(
            dirs.contains(&"staging"),
            "staging/ dir row present: {dirs:?}"
        );
        let model_leaves: Vec<&str> = mac
            .tree
            .iter()
            .filter(|r| r.kind == "model")
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(model_leaves.len(), 2);
        assert!(model_leaves.contains(&"fct_orders"));
        assert!(model_leaves.contains(&"stg_orders"));
        // A model leaf nests one level deeper than its deepest directory.
        let fct_leaf = mac
            .tree
            .iter()
            .find(|r| r.kind == "model" && r.label == "fct_orders")
            .expect("fct leaf");
        assert_eq!(fct_leaf.depth, 3, "models/marts/core/<model> → depth 3");
        assert_eq!(fct_leaf.model_id, "model.shop.fct_orders");
    }

    #[test]
    fn build_macro_lens_filters_out_vendor_package_macros() {
        // A changed macro whose recorded package differs from the root
        // project is dropped (vendor macros are not the reviewer's concern,
        // even when the pr-diff path channel resolved one). With only a
        // vendor macro in the set, the lens is None.
        let manifest = macro_lens_manifest(vec![macro_model(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.dbt_utils.helper"],
        )])
        .with_macro_identity({
            let mut id = BTreeMap::new();
            id.insert(
                "macro.dbt_utils.helper".to_owned(),
                crate::domain::MacroIdentity::new(
                    Some("macros/u.sql".to_owned()),
                    Some("helper".to_owned()),
                    Some("dbt_utils".to_owned()),
                ),
            );
            id
        });
        let changed = BTreeSet::from(["macro.dbt_utils.helper".to_owned()]);
        assert!(
            build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, usize::MAX).is_none(),
            "a vendor-only changed set yields no lens",
        );
    }

    #[test]
    fn build_macro_lens_empty_blast_radius_yields_an_empty_tree() {
        // A macro no model calls (e.g. a materialization macro) ⇒ count 0 +
        // empty tree (the template states the honest zero, never a false
        // claim).
        let manifest = macro_lens_manifest(vec![macro_model(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.shop.unrelated"],
        )]);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::Baseline, None, usize::MAX)
            .expect("lens builds");
        assert_eq!(lens.macros[0].impacted_count, 0);
        assert!(lens.macros[0].tree.is_empty());
        // Slice C: an empty radius ⇒ no impacted-model views (no selector).
        assert!(lens.macros[0].impacted_models.is_empty());
    }

    // ===== macro lens Slice C (cute-dbt#265) — selector + call sites =====

    /// A root-project `model` node calling `direct_macros`, declaring file
    /// `ofp`, AND carrying a real `raw_code` body (the call-site +
    /// inline-SQL source). Distinct from [`macro_model`], which leaves
    /// `raw_code` `None`.
    fn macro_model_with_raw(id: &str, ofp: &str, direct_macros: &[&str], raw: &str) -> Node {
        Node::new(
            NodeId::new(id),
            "model",
            Checksum::new("sha256", "abc"),
            Some("select 1".to_owned()),
            Some(raw.to_owned()),
            DependsOn::new(
                direct_macros.iter().map(|m| (*m).to_owned()).collect(),
                vec![],
            ),
            Some(ofp.to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_identity(None, Some("shop".to_owned()))
    }

    #[test]
    fn line_invokes_macro_matches_a_call_not_a_substring() {
        assert!(line_invokes_macro("  {{ add_dq_flags() }}", "add_dq_flags"));
        assert!(line_invokes_macro(
            "{% set x = add_dq_flags(col) %}",
            "add_dq_flags"
        ));
        // Whitespace between name and `(` is tolerated (Jinja lexer).
        assert!(line_invokes_macro("{{ add_dq_flags () }}", "add_dq_flags"));
        // A longer identifier ending in the name does NOT match.
        assert!(!line_invokes_macro(
            "{{ my_add_dq_flags() }}",
            "add_dq_flags"
        ));
        // The name without a following `(` is not a call (a bare reference).
        assert!(!line_invokes_macro(
            "-- see add_dq_flags for details",
            "add_dq_flags"
        ));
    }

    #[test]
    fn line_invokes_macro_empty_name_is_false_and_does_not_hang() {
        // gemini #358 — an empty macro_name makes `str::find("")` return
        // Some(0) with a zero-length advance: the defensive top guard must
        // return false rather than spin forever. (The test completing IS
        // the no-hang assertion.)
        assert!(!line_invokes_macro("select foo()", ""));
        assert!(!line_invokes_macro("", ""));
    }

    #[test]
    fn macro_call_sites_collects_each_invoking_line_with_its_number() {
        let raw = "select *\nfrom t\n  {{ add_dq_flags() }}\nwhere 1=1\n  {{ add_dq_flags(x) }}";
        let sites = macro_call_sites(raw, "add_dq_flags");
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0].line, 3);
        assert_eq!(sites[0].text, "{{ add_dq_flags() }}");
        assert_eq!(sites[1].line, 5);
        assert_eq!(sites[1].text, "{{ add_dq_flags(x) }}");
    }

    #[test]
    fn macro_call_sites_empty_when_no_invocation() {
        assert!(macro_call_sites("select 1", "add_dq_flags").is_empty());
        assert!(macro_call_sites("", "add_dq_flags").is_empty());
        assert!(macro_call_sites("{{ add_dq_flags() }}", "").is_empty());
    }

    #[test]
    fn build_macro_lens_carries_per_model_sql_and_call_sites() {
        // Slice C: each impacted model gets an ImpactedModelView with its
        // raw SQL (as context lines) + its first-order call sites of the
        // macro, in blast-radius id order.
        let raw_a = "select *\nfrom orders\n  {{ add_dq_flags() }}";
        let raw_b = "select *\nfrom items\n  {{ add_dq_flags(c) }}\n  {{ add_dq_flags(d) }}";
        let manifest = macro_lens_manifest(vec![
            macro_model_with_raw(
                "model.shop.stg_orders",
                "models/staging/stg_orders.sql",
                &["macro.shop.add_dq_flags"],
                raw_a,
            ),
            macro_model_with_raw(
                "model.shop.fct_orders",
                "models/marts/core/fct_orders.sql",
                &["macro.shop.add_dq_flags"],
                raw_b,
            ),
        ]);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::Baseline, None, usize::MAX)
            .expect("lens builds");
        let mac = &lens.macros[0];
        assert_eq!(mac.impacted_models.len(), 2);
        // BTreeSet id order: fct_orders before stg_orders.
        let fct = &mac.impacted_models[0];
        assert_eq!(fct.model_id, "model.shop.fct_orders");
        assert_eq!(fct.name, "fct_orders");
        assert_eq!(fct.path, "models/marts/core/fct_orders.sql");
        assert_eq!(fct.call_site_total, 2, "two call sites in fct_orders");
        assert_eq!(fct.call_sites.len(), 2);
        assert_eq!(fct.call_site_cap, MACRO_CALL_SITE_CAP);
        // The inline SQL renders as plain context lines (the model body).
        assert!(!fct.sql_lines.is_empty());
        assert!(
            fct.sql_lines
                .iter()
                .all(|l| l.kind == DiffLineKind::Context),
            "inline model SQL is plain context, never a diff",
        );
        let stg = &mac.impacted_models[1];
        assert_eq!(stg.model_id, "model.shop.stg_orders");
        assert_eq!(stg.call_site_total, 1);
    }

    #[test]
    fn build_macro_lens_caps_shown_call_sites_at_the_default() {
        // A model that calls the macro more than the cap: call_site_total
        // reports the honest count, but call_sites is bounded by the cap.
        let mut raw = String::from("select *\nfrom t\n");
        for _ in 0..(MACRO_CALL_SITE_CAP + 4) {
            raw.push_str("  {{ add_dq_flags() }}\n");
        }
        let manifest = macro_lens_manifest(vec![macro_model_with_raw(
            "model.shop.busy",
            "models/staging/busy.sql",
            &["macro.shop.add_dq_flags"],
            &raw,
        )]);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::Baseline, None, usize::MAX)
            .expect("lens builds");
        let im = &lens.macros[0].impacted_models[0];
        assert_eq!(im.call_site_total, MACRO_CALL_SITE_CAP + 4);
        assert_eq!(
            im.call_sites.len(),
            MACRO_CALL_SITE_CAP,
            "shown call sites are capped",
        );
        assert!(im.call_site_total > im.call_sites.len(), "the 'more' case");
    }

    #[test]
    fn build_macro_lens_impacted_model_with_no_raw_code_has_empty_sql_and_no_call_sites() {
        // A model with raw_code None (the rare null-fill) still appears in
        // the impacted set (the radius is depends_on-based), but its SQL +
        // call-site surfaces are empty (the template states the absence).
        let manifest = macro_lens_manifest(vec![macro_model(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.shop.add_dq_flags"],
        )]);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::Baseline, None, usize::MAX)
            .expect("lens builds");
        let im = &lens.macros[0].impacted_models[0];
        assert_eq!(im.call_site_total, 0);
        assert!(im.call_sites.is_empty());
        assert!(im.sql_lines.is_empty());
    }

    #[test]
    fn macro_lens_none_contributes_zero_payload_bytes() {
        // The byte-identity invariant: serializing a payload with
        // macro_lens == None must add ZERO bytes vs the same payload — the
        // `skip_serializing_if = Option::is_none` contract that keeps
        // non-macro goldens byte-identical.
        let manifest = macro_lens_manifest(vec![macro_model(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.shop.add_dq_flags"],
        )]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let mut payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
        );
        payload.macro_lens = None;
        let without = serde_json::to_string(&payload).expect("serialize");
        assert!(
            !without.contains("macro_lens"),
            "macro_lens == None must not appear in the JSON: {without}",
        );
    }

    /// Render a one-model PR-diff report carrying `macro_lens`, returning the
    /// HTML — the macro-section render-integration harness.
    fn render_html_with_macro_lens(
        filename: &str,
        macro_lens: Option<&MacroLensPayload>,
    ) -> String {
        let node = macro_model(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.shop.add_dq_flags"],
        );
        let manifest = macro_lens_manifest(vec![node]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let tmp = std::env::temp_dir().join(filename);
        let _ = std::fs::remove_file(&tmp);
        render_report_with_externals(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            ScopeSource::PrDiff,
            "t",
            None,
            &CheckPolicy::default(),
            &ProjectFacts::default(),
            &GovernanceFacts::default(),
            macro_lens,
            None,
            &[],
            DEFAULT_SEED_ROW_CAP,
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &[],
            None,
        )
        .expect("report renders");
        std::fs::read_to_string(&tmp).expect("read rendered report")
    }

    #[test]
    fn macro_lens_off_emits_zero_macro_dom() {
        // The off-gate render: macro_lens == None must add NO macro section
        // markup — the byte-identity-with-non-macro-goldens contract.
        let html = render_html_with_macro_lens("macro_off.html", None);
        assert!(!html.contains(r#"data-testid="macro-lens-panel""#));
        assert!(!html.contains("Macro changed"));
    }

    #[test]
    fn macro_lens_on_renders_the_section_with_tree_and_count() {
        let manifest = macro_lens_manifest(vec![macro_model(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.shop.add_dq_flags"],
        )]);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, usize::MAX)
            .expect("lens builds");
        let html = render_html_with_macro_lens("macro_on.html", Some(&lens));
        assert!(html.contains(r#"data-testid="macro-lens-panel""#));
        assert!(html.contains("Macro changed"));
        assert!(html.contains(r#"data-testid="macro-lens-experimental""#));
        assert!(html.contains(r#"data-testid="macro-lens-tree""#));
        assert!(html.contains(r#"data-testid="macro-lens-count""#));
        // Honest naming (critique S2): never a `state:` selector name.
        assert!(!html.contains("state:modified.macros"));
        // cute-dbt#424 — the macro-lens content lives in the Macros lens
        // panel, NOT in the Models panel. The Macros panel opens before the
        // macro-lens section, and the Project panel opens after it (so the
        // macro section sits inside the Macros panel by document order).
        let macros_open = html
            .find(r#"id="lens-panel-macros""#)
            .expect("Macros panel present");
        let macro_section = html
            .find(r#"data-testid="macro-lens-panel""#)
            .expect("macro-lens section present");
        let project_open = html
            .find(r#"id="lens-panel-project""#)
            .expect("Project panel present");
        let models_open = html
            .find(r#"id="lens-panel-models""#)
            .expect("Models panel present");
        let test_selection = html
            .find(r#"<section class="test-selection""#)
            .expect("test-selection present");
        assert!(
            macros_open < macro_section && macro_section < project_open,
            "the macro-lens section is nested inside the Macros lens panel \
             (macros_open={macros_open} macro_section={macro_section} project_open={project_open})",
        );
        // ...and it is NOT in the Models panel (which holds test-selection).
        assert!(
            models_open < test_selection && test_selection < macros_open,
            "the macro section is after the Models panel's report, not inside it",
        );
        // cute-dbt#424 — the working top-level macro picker renders in the
        // Macros panel with one option per changed macro.
        let picker = html
            .find(r#"data-testid="macro-select""#)
            .expect("macro picker present");
        assert!(
            macros_open < picker && picker < project_open,
            "the macro picker renders inside the Macros panel",
        );
    }

    #[test]
    fn macro_lens_on_renders_the_model_selector_and_call_sites() {
        // Slice C: a model with raw_code calling the macro inline ⇒ the
        // section carries the impacted-model selector + a server-rendered
        // per-model panel with the inline SQL + the first-order call sites.
        let raw = "select *\nfrom orders\n  {{ add_dq_flags() }}";
        let manifest = macro_lens_manifest(vec![macro_model_with_raw(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.shop.add_dq_flags"],
            raw,
        )]);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, usize::MAX)
            .expect("lens builds");
        let html = render_html_with_macro_lens("macro_selector.html", Some(&lens));
        // The selector (#91 idiom reuse) + its model option.
        assert!(html.contains(r#"data-testid="macro-lens-model-select""#));
        assert!(html.contains(r#"<option value="model.shop.orders""#));
        // The per-model panel, the inline SQL, and the call site.
        assert!(html.contains(r#"data-testid="macro-lens-model-panel""#));
        assert!(html.contains(r#"data-model="model.shop.orders""#));
        assert!(html.contains(r#"data-testid="macro-lens-model-sql""#));
        assert!(html.contains(r#"data-testid="macro-lens-callsites""#));
        assert!(html.contains(r#"data-testid="macro-lens-callsite""#));
        assert!(
            html.contains("{{ add_dq_flags() }}"),
            "the call-site line renders",
        );
        // The call-site count panel reads the honest total (1 here).
        assert!(html.contains(r#"data-testid="macro-lens-callsite-count""#));
    }

    #[test]
    fn macro_lens_on_renders_the_more_disclosure_when_call_sites_exceed_the_cap() {
        // Slice C: a model calling the macro more than the cap renders a
        // "Showing N of M" copy (the "more" case), and only the cap is shown.
        let mut raw = String::from("select *\n");
        for _ in 0..(MACRO_CALL_SITE_CAP + 2) {
            raw.push_str("  {{ add_dq_flags() }}\n");
        }
        let manifest = macro_lens_manifest(vec![macro_model_with_raw(
            "model.shop.busy",
            "models/staging/busy.sql",
            &["macro.shop.add_dq_flags"],
            &raw,
        )]);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, usize::MAX)
            .expect("lens builds");
        let total = MACRO_CALL_SITE_CAP + 2;
        let html = render_html_with_macro_lens("macro_more.html", Some(&lens));
        assert!(
            html.contains(&format!(
                "Showing {MACRO_CALL_SITE_CAP} of {total} call sites"
            )),
            "the over-cap copy states the honest 'showing N of M'",
        );
        // Exactly cap call-site rows render (the rest are the disclosed
        // remainder the count names but the shown list omits).
        let shown = html.matches(r#"data-testid="macro-lens-callsite""#).count();
        assert_eq!(shown, MACRO_CALL_SITE_CAP, "only the cap is shown");
    }

    // ===== macro lens: the inline-body cap (cute-dbt#265 Slice D) =====

    /// A heavy-macro manifest: `count` root-project models in distinct
    /// directory subtrees, each calling `macro.shop.add_dq_flags` inline
    /// (so an inlined panel has real SQL + a call site). Zero-padded names
    /// keep the [`BTreeSet`] id order deterministic (`m00`..`m24`), so the
    /// cap's "first N" selection is golden-stable. Each model body is a
    /// chunky ~40-line SQL block so the cap's byte-bounding effect is
    /// realistic (an inlined body is a few KiB; an over-cap model is a few
    /// hundred bytes of identity-only markup).
    fn heavy_macro_manifest(count: usize) -> Manifest {
        use std::fmt::Write as _;
        // A multi-line SQL body of realistic heft — the bytes the cap bounds.
        let mut raw = String::from("with base as (\n  select *\n  from upstream\n)\n");
        for line in 0..36 {
            let _ = writeln!(raw, "  , col_{line} as (select {line} from base)");
        }
        raw.push_str("select *\nfrom base\n  {{ add_dq_flags() }}");
        let models = (0..count)
            .map(|i| {
                macro_model_with_raw(
                    &format!("model.shop.m{i:02}"),
                    &format!("models/marts/m{i:02}.sql"),
                    &["macro.shop.add_dq_flags"],
                    &raw,
                )
            })
            .collect();
        macro_lens_manifest(models)
    }

    #[test]
    fn build_macro_lens_caps_inline_bodies_at_the_knob() {
        // Slice D (founder D5): with 25 impacted models and a cap of 10,
        // exactly the first 10 (id order m00..m09) inline their body
        // (inline_body == true); the rest carry identity only
        // (inline_body == false, empty SQL + call sites). The selector still
        // lists ALL 25 (every view is present), but the heavy surface is
        // bounded.
        let manifest = heavy_macro_manifest(25);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, 10)
            .expect("lens builds");
        let mac = &lens.macros[0];
        assert_eq!(mac.impacted_count, 25, "all 25 models are in the radius");
        assert_eq!(mac.inlined_count, 10, "only the cap inlines a body");
        assert_eq!(
            mac.impacted_models.len(),
            25,
            "the selector still lists every model (the list is cheap)",
        );
        let inlined = mac
            .impacted_models
            .iter()
            .filter(|im| im.inline_body)
            .count();
        assert_eq!(inlined, 10, "exactly the cap carries an inline body");
        // The first 10 (id order) are the inlined ones; they carry SQL.
        for im in mac.impacted_models.iter().take(10) {
            assert!(im.inline_body, "{} inlines", im.model_id);
            assert!(!im.sql_lines.is_empty(), "{} has SQL", im.model_id);
        }
        // Past the cap: identity only, no SQL, no call sites (the bytes the
        // cap is meant to bound).
        for im in mac.impacted_models.iter().skip(10) {
            assert!(!im.inline_body, "{} is not inlined", im.model_id);
            assert!(im.sql_lines.is_empty(), "{} has no SQL", im.model_id);
            assert!(
                im.call_sites.is_empty(),
                "{} has no call sites",
                im.model_id
            );
            assert_eq!(im.call_site_total, 0, "{} reports zero", im.model_id);
        }
    }

    #[test]
    fn build_macro_lens_below_cap_inlines_every_body() {
        // At or below the cap every impacted model inlines — inlined_count
        // == impacted_count, so the template omits the "showing N of M"
        // affordance entirely (the Slice C behaviour, unchanged).
        let manifest = heavy_macro_manifest(3);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, 10)
            .expect("lens builds");
        let mac = &lens.macros[0];
        assert_eq!(mac.impacted_count, 3);
        assert_eq!(mac.inlined_count, 3, "below the cap: all inline");
        assert!(
            mac.impacted_models.iter().all(|im| im.inline_body),
            "every model inlines below the cap",
        );
    }

    #[test]
    fn build_macro_lens_cap_zero_inlines_no_body() {
        // A cap of 0 is legal (tree-only): the selector still lists every
        // model, but none inline a body — the maximally-bounded report.
        let manifest = heavy_macro_manifest(4);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, 0)
            .expect("lens builds");
        let mac = &lens.macros[0];
        assert_eq!(mac.inlined_count, 0);
        assert!(
            mac.impacted_models.iter().all(|im| !im.inline_body),
            "a cap of 0 inlines nothing",
        );
        assert_eq!(mac.impacted_models.len(), 4, "the selector still lists all");
    }

    #[test]
    fn macro_lens_renders_the_over_cap_affordance() {
        // Slice D: the rendered HTML shows the "showing N of M bodies" line
        // and the per-panel "body not inlined" affordance for over-cap
        // models, while inlined models keep their SQL panel.
        let manifest = heavy_macro_manifest(25);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, 10)
            .expect("lens builds");
        let html = render_html_with_macro_lens("macro_cap.html", Some(&lens));
        // The "showing N of M bodies" affordance.
        assert!(
            html.contains(r#"data-testid="macro-lens-body-cap""#),
            "the body-cap line renders",
        );
        assert!(
            html.contains(r#"data-inlined="10""#) && html.contains(r#"data-total="25""#),
            "the body-cap line states 10 of 25",
        );
        // The over-cap "body not inlined" affordance renders.
        assert!(
            html.contains(r#"data-testid="macro-lens-model-uninlined""#),
            "the over-cap panel shows the uninlined affordance",
        );
        // The selector still lists all 25 models (every option present).
        let options = html.matches("<option value=\"model.shop.m").count();
        assert_eq!(options, 25, "the selector lists every impacted model");
        // Exactly 10 inlined SQL panels render (the bounded heavy surface).
        let sql_panels = html
            .matches(r#"data-testid="macro-lens-model-sql""#)
            .count();
        assert_eq!(sql_panels, 10, "only the cap's bodies inline");
    }

    #[test]
    fn macro_lens_below_cap_omits_the_body_cap_line() {
        // Below the cap: the "showing N of M bodies" line is absent (the cap
        // is not exceeded) and no panel carries the uninlined affordance.
        let manifest = heavy_macro_manifest(3);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, 10)
            .expect("lens builds");
        let html = render_html_with_macro_lens("macro_under_cap.html", Some(&lens));
        assert!(
            !html.contains(r#"data-testid="macro-lens-body-cap""#),
            "no body-cap line below the cap",
        );
        assert!(
            !html.contains(r#"data-testid="macro-lens-model-uninlined""#),
            "no uninlined panel below the cap",
        );
    }

    #[test]
    fn macro_lens_worst_case_heavy_macro_report_stays_under_a_byte_budget() {
        // Slice D golden-size assertion (the cap's RAISON D'ÊTRE): a
        // widely-used macro (200 impacted models, each a chunky ~40-line
        // body) rendered at the default cap must produce a BOUNDED report.
        // The inlined heavy surface is fixed at the cap (10 SQL panels);
        // past it each model contributes only a lightweight selector option
        // + tree row + identity panel. The whole report (asset bundle +
        // section) must stay under the budget.
        //
        // The base asset bundle (Sakura + jQuery + DataTables + Mermaid +
        // Cytoscape, all inlined) is ~4.2 MiB by itself, so the budget is a
        // full-report ceiling. 6 MiB sits comfortably above the measured
        // capped size while failing loudly if the cap regresses and all 200
        // full bodies inline (which the companion test proves is materially
        // larger). The cap is the bound that keeps the marginal per-over-cap
        // model cost a few hundred bytes, not a few KiB.
        const BYTE_BUDGET: usize = 6 * 1024 * 1024;
        let manifest = heavy_macro_manifest(200);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let lens = build_macro_lens(
            &manifest,
            &changed,
            ScopeSource::PrDiff,
            None,
            DEFAULT_MACRO_BODY_CAP,
        )
        .expect("lens builds");
        let mac = &lens.macros[0];
        assert_eq!(mac.impacted_count, 200, "all 200 models impacted");
        assert_eq!(
            mac.inlined_count, DEFAULT_MACRO_BODY_CAP,
            "only the default cap inlines",
        );
        let html = render_html_with_macro_lens("macro_heavy_budget.html", Some(&lens));
        assert!(
            html.len() < BYTE_BUDGET,
            "worst-case heavy-macro report ({} bytes) must stay under the \
             {BYTE_BUDGET}-byte budget — the cap bounds the heavy surface",
            html.len(),
        );
        // Exactly the cap's SQL panels inline — the bound that makes the
        // budget hold regardless of model count.
        let sql_panels = html
            .matches(r#"data-testid="macro-lens-model-sql""#)
            .count();
        assert_eq!(
            sql_panels, DEFAULT_MACRO_BODY_CAP,
            "the cap bounds the bodies"
        );
    }

    #[test]
    fn macro_lens_cap_meaningfully_shrinks_a_heavy_report() {
        // The cap must actually bound the size: a capped render of a heavy
        // macro is materially smaller than an uncapped one (every body
        // inlined). This pins the cap's value — not just that it renders.
        let manifest = heavy_macro_manifest(60);
        let changed = BTreeSet::from(["macro.shop.add_dq_flags".to_owned()]);
        let capped = build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, 10)
            .expect("capped lens builds");
        let uncapped = build_macro_lens(&manifest, &changed, ScopeSource::PrDiff, None, usize::MAX)
            .expect("uncapped lens builds");
        let capped_html = render_html_with_macro_lens("macro_capped.html", Some(&capped));
        let uncapped_html = render_html_with_macro_lens("macro_uncapped.html", Some(&uncapped));
        assert!(
            capped_html.len() < uncapped_html.len(),
            "the capped report ({}) must be smaller than the uncapped one ({})",
            capped_html.len(),
            uncapped_html.len(),
        );
    }

    // ===== subject-lens tab shell (cute-dbt#402, epic #360) =====

    /// Render a one-model report, returning the HTML. Reuses the macro-lens
    /// manifest helper for a minimal in-scope model so the Models lens panel
    /// has real content to wrap. The tab shell is an UNGATED part of the
    /// report (cute-dbt#402), so every report carries it.
    fn render_html_for_lens_shell(filename: &str) -> String {
        let node = macro_model(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.shop.add_dq_flags"],
        );
        let manifest = macro_lens_manifest(vec![node]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let tmp = std::env::temp_dir().join(filename);
        let _ = std::fs::remove_file(&tmp);
        render_report_with_externals(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            ScopeSource::PrDiff,
            "t",
            None,
            &CheckPolicy::default(),
            &ProjectFacts::default(),
            &GovernanceFacts::default(),
            None,
            None,
            &[],
            DEFAULT_SEED_ROW_CAP,
            // cute-dbt#404 — no PR-scope mini-DAG in this lens-shell helper.
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &[],
            None,
        )
        .expect("report renders");
        std::fs::read_to_string(&tmp).expect("read rendered report")
    }

    #[test]
    fn lens_shell_renders_three_subject_tabs_with_models_active() {
        let html = render_html_for_lens_shell("lensshell_tabs.html");
        // The tab strip with all three subject lenses.
        assert!(
            html.contains(r#"data-testid="lens-tabs""#),
            "the tab strip renders on every report",
        );
        assert!(
            html.contains(r#"data-testid="lens-tab-models""#)
                && html.contains(r#"data-testid="lens-tab-macros""#)
                && html.contains(r#"data-testid="lens-tab-project""#),
            "all three subject-lens tabs render",
        );
        // Models is the active tab by default.
        assert!(
            html.contains(
                r#"<button type="button" class="lens-tab is-active" role="tab" id="lens-tab-models" aria-selected="true""#
            ),
            "the Models tab is active by default",
        );
        // WAI-ARIA roving tabindex: the active (Models) tab is the single
        // Tab-order stop (`tabindex="0"`); the inactive tabs are `-1`,
        // reachable only via the Arrow/Home/End keyboard pattern.
        assert!(
            html.contains(
                r#"id="lens-tab-models" aria-selected="true" aria-controls="lens-panel-models" tabindex="0""#
            ),
            "the active Models tab carries the roving tabindex=0",
        );
        assert!(
            html.contains(r#"id="lens-tab-macros" aria-selected="false" aria-controls="lens-panel-macros" tabindex="-1""#)
                && html.contains(r#"id="lens-tab-project" aria-selected="false" aria-controls="lens-panel-project" tabindex="-1""#),
            "the inactive tabs are removed from the Tab order (tabindex=-1)",
        );
        // The Models lens panel is the active panel; Macros/Project are hidden.
        assert!(
            html.contains(
                r#"<div class="lens-panel is-active" role="tabpanel" id="lens-panel-models""#
            ),
            "the Models lens panel is active",
        );
        assert!(
            html.contains(r#"id="lens-panel-macros" aria-labelledby="lens-tab-macros" data-lens="macros" data-testid="lens-panel-macros" hidden"#),
            "the Macros lens panel is present and hidden by default",
        );
        assert!(
            html.contains(r#"id="lens-panel-project" aria-labelledby="lens-tab-project" data-lens="project" data-testid="lens-panel-project" hidden"#),
            "the Project lens panel is present and hidden by default",
        );
    }

    #[test]
    fn lens_shell_nests_the_existing_report_sections_inside_models() {
        // The Models lens = today's whole report: the existing sections render
        // UNCHANGED inside the Models panel. Assert the nesting order — the
        // Models panel opens before the test-selection section, and the Macros
        // panel opens after it (so the existing report sits between the two).
        let html = render_html_for_lens_shell("lensshell_nesting.html");
        let models_open = html
            .find(r#"id="lens-panel-models""#)
            .expect("Models panel present");
        let test_selection = html
            .find(r#"<section class="test-selection""#)
            .expect("test-selection section present");
        let macros_open = html
            .find(r#"id="lens-panel-macros""#)
            .expect("Macros panel present");
        assert!(
            models_open < test_selection && test_selection < macros_open,
            "the existing report sections are nested inside the Models lens panel \
             (models_open={models_open} test_selection={test_selection} macros_open={macros_open})",
        );
    }

    #[test]
    fn lens_shell_macros_and_project_are_empty_when_no_lens_content() {
        // cute-dbt#424 — the Macros + Project lenses now hold their own
        // subject content (the relocated macro-lens / project-definition
        // panels), gated `{% match %}` blocks that emit ZERO bytes when the
        // payload is `None`. With no macro_lens and no project_panel (the
        // helper passes `None` for both), the panels degrade to a sane EMPTY
        // tab: present + `hidden`, carrying no macro-lens / project-def
        // content (and no leftover scaffold copy — the empty-state COPY is
        // the #365 design-phase work). The byte-identity / zero-egress
        // posture of the off-gate render is unchanged.
        let html = render_html_for_lens_shell("lensshell_empty.html");
        // Both panels are present + hidden by default.
        assert!(
            html.contains(r#"data-testid="lens-panel-macros""#)
                && html.contains(r#"data-testid="lens-panel-project""#),
            "both subject-lens panels are present",
        );
        // With no lens content, neither subject panel carries its content
        // section — the empty tab shows nothing (the AC's sane degradation).
        assert!(
            !html.contains(r#"data-testid="macro-lens-panel""#),
            "no macro-lens content when macro_lens is None",
        );
        assert!(
            !html.contains(r#"data-testid="project-def-panel""#),
            "no project-definition content when project_panel is None",
        );
        // The retired scaffold hooks are gone (the relocation replaced them).
        assert!(
            !html.contains(r#"data-testid="lens-macros-empty""#)
                && !html.contains(r#"data-testid="lens-project-empty""#),
            "the placeholder scaffold copy is retired",
        );
    }

    // ===== change-context banner PR link (cute-dbt#346) =====

    /// Render a one-model report carrying `pr_ref` on the given scope arm,
    /// returning the HTML. Reuses the macro-lens manifest helper for a
    /// minimal in-scope model.
    fn render_html_with_pr_ref(
        filename: &str,
        scope_source: ScopeSource,
        pr_ref: Option<&PrRef>,
    ) -> String {
        let node = macro_model(
            "model.shop.orders",
            "models/staging/orders.sql",
            &["macro.shop.add_dq_flags"],
        );
        let manifest = macro_lens_manifest(vec![node]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.orders")]);
        let tmp = std::env::temp_dir().join(filename);
        let _ = std::fs::remove_file(&tmp);
        render_report_with_externals(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            scope_source,
            "t",
            None,
            &CheckPolicy::default(),
            &ProjectFacts::default(),
            &GovernanceFacts::default(),
            None,
            pr_ref,
            &[],
            DEFAULT_SEED_ROW_CAP,
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &[],
            None,
        )
        .expect("report renders");
        std::fs::read_to_string(&tmp).expect("read rendered report")
    }

    fn sample_pr_ref() -> PrRef {
        PrRef {
            number: 123,
            title: "Add customer churn model".to_owned(),
            url: "https://github.com/acme/shop/pull/123".to_owned(),
        }
    }

    #[test]
    fn pr_ref_renders_a_linked_clause_on_the_pr_diff_arm() {
        let pr = sample_pr_ref();
        let html = render_html_with_pr_ref("pr_ref_on.html", ScopeSource::PrDiff, Some(&pr));
        // The link points at the PR url (navigation, NOT a resource load).
        assert!(
            html.contains(r#"<a class="diff-scope-pr-link" href="https://github.com/acme/shop/pull/123">PR #123</a>"#),
            "the banner carries the linked PR token: {html}",
        );
        // The title renders as adjacent text inside the clause.
        assert!(
            html.contains("Add customer churn model"),
            "the title renders beside the link",
        );
        // The provenance clause still reads truthfully.
        assert!(html.contains("from PR file diff"));
    }

    #[test]
    fn pr_ref_title_is_html_escaped() {
        // An untrusted PR title with HTML metacharacters must be
        // askama-escaped — no raw tag injection from a title.
        let pr = PrRef {
            number: 7,
            title: "<img src=x onerror=alert(1)> & \"quotes\"".to_owned(),
            url: "https://github.com/acme/shop/pull/7".to_owned(),
        };
        let html = render_html_with_pr_ref("pr_ref_xss.html", ScopeSource::PrDiff, Some(&pr));
        assert!(
            !html.contains("<img src=x onerror=alert(1)>"),
            "the raw tag must NOT survive: {html}",
        );
        assert!(
            html.contains("&#60;img") || html.contains("&lt;img"),
            "the `<` is escaped",
        );
        // The url is a trusted-shape attribute; the link still resolves.
        assert!(html.contains(r#"href="https://github.com/acme/shop/pull/7""#));
    }

    #[test]
    fn pr_ref_absent_emits_no_link_and_no_clause() {
        // Graceful degradation: no PR ref ⇒ the banner renders exactly as
        // today (no link, no dangling dash). Byte-identity precondition.
        let html = render_html_with_pr_ref("pr_ref_off.html", ScopeSource::PrDiff, None);
        assert!(!html.contains("diff-scope-pr-link"));
        // The banner line ends right after the provenance clause — no PR
        // clause appended (the `(<a ...>PR #...` shape is absent).
        assert!(!html.contains("from PR file diff (<a"));
        assert!(html.contains("from PR file diff"));
    }

    #[test]
    fn pr_ref_is_inert_on_the_baseline_arm() {
        // A PR ref supplied on a baseline run is gated out by the renderer
        // (the baseline banner has no "from PR file diff" clause to anchor
        // a PR link to). The baseline banner is unchanged.
        let pr = sample_pr_ref();
        let html =
            render_html_with_pr_ref("pr_ref_baseline.html", ScopeSource::Baseline, Some(&pr));
        assert!(!html.contains("diff-scope-pr-link"));
        assert!(!html.contains("PR #123"));
        assert!(html.contains("vs baseline manifest"));
    }

    #[test]
    fn pr_ref_rides_the_json_payload_on_the_pr_diff_arm() {
        // The wire twin (the governance/macro-lens both-surfaces precedent):
        // the ref serializes into the embedded JSON payload too.
        let pr = sample_pr_ref();
        let html = render_html_with_pr_ref("pr_ref_payload.html", ScopeSource::PrDiff, Some(&pr));
        assert!(
            html.contains(r#""pr_ref""#),
            "the pr_ref key is in the payload"
        );
        assert!(html.contains(r#""number":123"#));
    }

    #[test]
    fn pr_ref_is_omitted_from_the_payload_when_absent() {
        let html = render_html_with_pr_ref("pr_ref_payload_off.html", ScopeSource::PrDiff, None);
        assert!(
            !html.contains(r#""pr_ref""#),
            "the skip-when-None contract keeps it out of the JSON",
        );
    }

    // ===== project panel: hooks + dispatch rows (cute-dbt#269) =====

    /// Render a one-model PR-diff report carrying `facts`, returning the
    /// HTML.
    fn render_html_with_project_facts(filename: &str, facts: &ProjectFacts) -> String {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join(filename);
        let _ = std::fs::remove_file(&tmp);
        render_report_with_externals(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            ScopeSource::PrDiff,
            "t",
            None,
            &CheckPolicy::default(),
            facts,
            &GovernanceFacts::default(),
            None,
            None,
            &[],
            DEFAULT_SEED_ROW_CAP,
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &[],
            None,
        )
        .expect("report renders");
        std::fs::read_to_string(&tmp).expect("read rendered report")
    }

    #[test]
    fn project_panel_renders_inside_the_project_lens_tab() {
        // cute-dbt#424 — the project-definition panel lives in the Project
        // lens panel, NOT in the Models panel. Assert by document order: the
        // Project panel opens before the project-def section, and the
        // project-def section comes AFTER the Models panel's test-selection
        // (so it is not nested in Models).
        let facts = ProjectFacts {
            definition: None,
            panel: Some(ProjectChangePanel::Categorized {
                changes: vec![ProjectChange {
                    category: ProjectChangeCategory::Vars,
                    label: "dq_threshold".to_owned(),
                    old: Some(serde_json::json!(10)),
                    new: Some(serde_json::json!(5)),
                    hook: None,
                    tree: None,
                    vars: None,
                }],
            }),
            config_attributions: BTreeMap::new(),
            var_references: BTreeMap::new(),
        };
        let html = render_html_with_project_facts("cute_dbt_project_in_tab.html", &facts);
        let project_open = html
            .find(r#"id="lens-panel-project""#)
            .expect("Project panel present");
        let project_section = html
            .find(r#"data-testid="project-def-panel""#)
            .expect("project-def section present");
        let test_selection = html
            .find(r#"<section class="test-selection""#)
            .expect("test-selection present");
        assert!(
            project_open < project_section,
            "the project-def section is nested inside the Project lens panel \
             (project_open={project_open} project_section={project_section})",
        );
        assert!(
            test_selection < project_section,
            "the project-def section is AFTER the Models panel's report, not inside it",
        );
    }

    fn hooks_change_with_facts(presence: HookManifestPresence, ids: Vec<String>) -> ProjectChange {
        ProjectChange {
            category: ProjectChangeCategory::Hooks,
            label: "on-run-start".to_owned(),
            old: Some(serde_json::json!(["grant usage on schema x"])),
            new: Some(serde_json::json!(["grant select on schema x"])),
            hook: Some(HookChangeFacts {
                sql_diff: Some(BlockDiff {
                    lines: vec![
                        DiffLine {
                            kind: DiffLineKind::Removed,
                            text: "grant usage on schema x".to_owned(),
                            emphasis: Some((6, 11)),
                        },
                        DiffLine {
                            kind: DiffLineKind::Added,
                            text: "grant select on schema x".to_owned(),
                            emphasis: Some((6, 12)),
                        },
                    ],
                }),
                operation_ids: ids,
                manifest: presence,
            }),
            tree: None,
            vars: None,
        }
    }

    #[test]
    fn hooks_row_emits_slot_and_manifest_note_and_drops_json_detail() {
        let facts = ProjectFacts {
            definition: None,
            panel: Some(ProjectChangePanel::Categorized {
                changes: vec![hooks_change_with_facts(
                    HookManifestPresence::Matched,
                    vec!["operation.shop.shop-on-run-start-0".to_owned()],
                )],
            }),
            config_attributions: BTreeMap::new(),
            var_references: BTreeMap::new(),
        };
        let html = render_html_with_project_facts("cute_dbt_render_hooks_row_test.html", &facts);
        assert!(
            html.contains(r#"data-hook-slot="on-run-start""#),
            "the hooks row emits the JS-fill slot",
        );
        assert!(
            html.contains("runs in the manifest as operation.shop.shop-on-run-start-0"),
            "the note states the manifest-side operation reality",
        );
        assert!(
            !html.contains("[&#34;grant usage on schema x&#34;]"),
            "the raw JSON detail is dropped when the SQL diff renders",
        );
    }

    #[test]
    fn hooks_row_absent_note_enumerates_causes_and_what_was_checked() {
        let mut change = hooks_change_with_facts(HookManifestPresence::Absent, Vec::new());
        change.hook.as_mut().unwrap().sql_diff = None;
        let facts = ProjectFacts {
            definition: None,
            panel: Some(ProjectChangePanel::Categorized {
                changes: vec![change],
            }),
            config_attributions: BTreeMap::new(),
            var_references: BTreeMap::new(),
        };
        let html = render_html_with_project_facts("cute_dbt_render_hooks_absent_test.html", &facts);
        assert!(
            html.contains("no matching operation.* nodes in the manifest"),
            "the absent verdict is stated in-row",
        );
        assert!(
            html.contains("checked the manifest nodes"),
            "the note states what WAS checked",
        );
        // (the bare attribute name also appears in the embedded JS
        // source, so the assertion targets the labelled slot markup.)
        assert!(
            !html.contains(r#"data-hook-slot="on-run-start""#),
            "no slot without a diff to fill",
        );
        assert!(
            html.contains("[&#34;grant select on schema x&#34;]"),
            "without a diff the row keeps its plain detail",
        );
    }

    #[test]
    fn dispatch_row_renders_unknown_tier_banner_with_honest_copy() {
        let facts = ProjectFacts {
            definition: None,
            panel: Some(ProjectChangePanel::Categorized {
                changes: vec![ProjectChange {
                    category: ProjectChangeCategory::Dispatch,
                    label: "dispatch".to_owned(),
                    old: Some(serde_json::json!([{ "macro_namespace": "dbt_utils",
                        "search_order": ["dbt_utils", "shop"] }])),
                    new: Some(serde_json::json!([{ "macro_namespace": "dbt_utils",
                        "search_order": ["shop", "dbt_utils"] }])),
                    hook: None,
                    tree: None,
                    vars: None,
                }],
            }),
            config_attributions: BTreeMap::new(),
            var_references: BTreeMap::new(),
        };
        let html = render_html_with_project_facts("cute_dbt_render_dispatch_row_test.html", &facts);
        assert!(
            html.contains(r#"class="project-def-row is-banner" data-category="dispatch""#),
            "the dispatch row renders as a banner",
        );
        assert!(
            html.contains(r#"<span class="tier-chip tier-unknown">UNKNOWN</span>"#),
            "the UNKNOWN tier chip renders",
        );
        // The honest-UNKNOWN copy: in-row, causes enumerated, what was
        // checked stated.
        assert!(html.contains("cannot be attributed statically"));
        assert!(html.contains("macro resolution happens per call at compile time"));
        assert!(html.contains("Checked: the old and new dispatch values"));
        // The old → new values still render (what WAS checked, shown).
        assert!(html.contains("search_order"));
    }

    // ===== render_report: report_title + report_subtitle threading =====

    #[test]
    fn render_report_default_title_renders_into_title_and_h1() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_title_default_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            html.contains("<title>cute-dbt report</title>"),
            "default title in <title>: {}",
            html.lines()
                .find(|l| l.contains("<title>"))
                .unwrap_or("<not found>"),
        );
        assert!(
            html.contains("<h1>cute-dbt report</h1>"),
            "default title in <h1>: {}",
            html.lines()
                .find(|l| l.contains("<h1>"))
                .unwrap_or("<not found>"),
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_custom_title_overrides_both_surfaces() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_title_custom_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            "Q3 unit test review",
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            html.contains("<title>Q3 unit test review</title>"),
            "custom title in <title>: {}",
            html.lines()
                .find(|l| l.contains("<title>"))
                .unwrap_or("<not found>"),
        );
        assert!(
            html.contains("<h1>Q3 unit test review</h1>"),
            "custom title in <h1>: {}",
            html.lines()
                .find(|l| l.contains("<h1>"))
                .unwrap_or("<not found>"),
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_absent_subtitle_omits_the_subtitle_element() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_no_subtitle_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            !html.contains("class=\"report-subtitle\""),
            "subtitle element omitted when None"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_present_subtitle_renders_the_subtitle_element() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_with_subtitle_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            "Q3 review",
            Some("PR 1234 / staging diff"),
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            html.contains("<p class=\"report-subtitle\">PR 1234 / staging diff</p>"),
            "subtitle element rendered with text: {}",
            html.lines()
                .find(|l| l.contains("report-subtitle"))
                .unwrap_or("<not found>"),
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_xss_in_title_is_html_escaped() {
        // askama's `html` escape filter (template default) prevents a
        // hostile title containing `<script>` from breaking out of the
        // <title> / <h1> text nodes.
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_xss_title_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            "<script>alert(1)</script>",
            None,
        )
        .expect("render writes");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        // The escaped form appears; the raw form does NOT appear in the
        // chrome (it may appear inside inlined script bodies — strip
        // those first, mirroring the egress test pattern).
        let mut chrome = html.clone();
        for asset in [
            SAKURA_CSS,
            DATATABLES_CSS,
            JQUERY_JS,
            DATATABLES_JS,
            MERMAID_JS,
        ] {
            chrome = chrome.replace(asset, "<<inlined-asset>>");
        }
        assert!(
            !chrome.contains("<script>alert(1)</script>"),
            "raw <script> never appears in the chrome"
        );
        // askama's html escape may use entity names (&lt;) or numeric
        // (&#60; / &#x3c;); accept any escaped form. The title literal
        // `alert(1)` must still appear (escapes leave text alone) so
        // the title is reachable in the rendered output.
        assert!(
            chrome.contains("alert(1)"),
            "escaped title still carries its text payload"
        );
        let has_escaped_lt =
            chrome.contains("&lt;") || chrome.contains("&#60;") || chrome.contains("&#x3c;");
        assert!(
            has_escaped_lt,
            "some escaped < entity appears in the chrome (askama html filter)"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    // ===== render_report: diff-scope banner provenance (cute-dbt#85) =====

    #[test]
    fn render_report_baseline_banner_names_the_baseline_manifest() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_banner_baseline_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            html.contains("vs baseline manifest"),
            "baseline banner names the baseline manifest",
        );
        assert!(
            !html.contains("from PR file diff"),
            "baseline banner does not claim a PR-diff provenance",
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_pr_diff_banner_omits_the_baseline_manifest_clause() {
        // On the PR-diff path there is no baseline manifest — rendering
        // "vs baseline manifest …" would be a false statement (cute-dbt#85).
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_banner_pr_diff_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "",
            ScopeSource::PrDiff,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(
            !html.contains("vs baseline manifest"),
            "PR-diff banner must NOT name a baseline manifest",
        );
        assert!(
            html.contains("from PR file diff"),
            "PR-diff banner states its provenance",
        );
        let _ = std::fs::remove_file(&tmp);
    }

    // ===== render_report end-to-end =====

    #[test]
    fn render_report_writes_valid_html_with_inlined_assets() {
        let compiled = "with src as (select * from raw_x) select * from src";
        let node = model_node("model.shop.x", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "baseline.json",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        assert!(html.starts_with("<!DOCTYPE html>") || html.starts_with("<!doctype html>"));
        assert!(html.contains(SAKURA_CSS), "Sakura inlined");
        assert!(html.contains(JQUERY_JS), "jQuery inlined");
        assert!(html.contains(DATATABLES_JS), "DataTables JS inlined");
        assert!(html.contains(DATATABLES_CSS), "DataTables CSS inlined");
        assert!(html.contains(MERMAID_JS), "Mermaid inlined");
        assert!(
            html.contains("href=\"data:,\""),
            "favicon is data: URI; got: {}",
            html.lines().find(|l| l.contains("favicon")).unwrap_or(""),
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_inlines_payload_as_application_json_script() {
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_json_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "lab1@aaaaaaa",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes");
        let html = std::fs::read_to_string(&tmp).unwrap();
        assert!(
            html.contains("id=\"cute-dbt-data\""),
            "json blob carrier present",
        );
        assert!(
            html.contains("lab1@aaaaaaa"),
            "baseline label visible in HTML",
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_does_not_emit_external_resource_constructs() {
        // Local belt-and-braces guard for the zero-egress invariant.
        // The canonical proof is the structured resource-ref lint job
        // plus the headless-browser network-block test tracked at
        // `breezy-bays-labs/cute-dbt#12`; this test is the fast local
        // signal that runs on every `cargo test` until that lands.
        //
        // Patterns cover the loading constructs the structured lint
        // will reject: `<script src>`, `<link href>`, `<img src>`,
        // CSS `@import`, CSS `url(`, protocol-relative `//`, and bare
        // `http://` / `https://`. The chrome is measured AFTER
        // stripping the six inlined vendored asset bodies so we don't
        // false-positive on the bundles' inert URL literals.
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_egress_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &InScopeSet::new(),
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes");
        let mut chrome = std::fs::read_to_string(&tmp).unwrap();
        for asset in [
            SAKURA_CSS,
            DATATABLES_CSS,
            JQUERY_JS,
            DATATABLES_JS,
            MERMAID_JS,
            CYTOSCAPE_JS,
        ] {
            chrome = chrome.replace(asset, "<<inlined-asset>>");
        }
        assert!(!chrome.contains("<script src"), "no <script src> in chrome");
        assert!(!chrome.contains("<link href"), "no <link href> in chrome");
        assert!(
            !chrome.contains("<img"),
            "no <img> in chrome (we emit no images)",
        );
        assert!(!chrome.contains(" src=\""), "no src= attribute in chrome");
        assert!(!chrome.contains("@import"), "no CSS @import in chrome");
        assert!(!chrome.contains("url("), "no CSS url() in chrome");
        assert!(!chrome.contains("http://"), "no http URL in chrome");
        assert!(!chrome.contains("https://"), "no https URL in chrome");
        assert!(!chrome.contains("\"//"), "no protocol-relative reference");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn render_report_xss_payload_in_unit_test_tag_does_not_escape_script_block() {
        // A hostile string in a unit-test's `tags` (which is verbatim YAML
        // metadata in the dbt manifest — no SQL parser sits between it
        // and the payload) must NOT break out of the JSON
        // `<script type="application/json">` carrier. askama's `| json`
        // filter escapes `<`, `>`, `&`, and `'`; this test pins that the
        // payload-side injection is structurally prevented.
        let hostile = "</script><script>alert(1)</script>";
        let compiled = "select 1";
        let node = model_node("model.shop.x", "body", Some(compiled));
        let ut = UnitTest::new(
            "test_one",
            NodeId::new("x"),
            vec![],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            Some(vec![hostile.to_owned()]),
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_xss_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(
            &tmp,
            &manifest,
            &in_scope,
            &models,
            &InScopeSet::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
            ScopeSource::Baseline,
            DEFAULT_REPORT_TITLE,
            None,
        )
        .expect("render writes the report");
        let html = std::fs::read_to_string(&tmp).expect("report exists");
        // `</script>` legitimately appears inside the inlined asset
        // bodies; strip those before scanning the chrome.
        let mut chrome = html.clone();
        for asset in [
            SAKURA_CSS,
            DATATABLES_CSS,
            JQUERY_JS,
            DATATABLES_JS,
            MERMAID_JS,
        ] {
            chrome = chrome.replace(asset, "<<inlined-asset>>");
        }
        assert!(
            !chrome.contains("<script>alert(1)</script>"),
            "hostile script tag must not survive into the chrome: {chrome}",
        );
        // The payload carrier is exactly one `<script type="application/json">`
        // …`</script>` block; no second `</script>` smuggled in by the
        // hostile tag value can close it early.
        let payload_open = "<script type=\"application/json\" id=\"cute-dbt-data\">";
        let payload_count = chrome.matches(payload_open).count();
        assert_eq!(payload_count, 1, "exactly one payload carrier open tag");
        let _ = std::fs::remove_file(&tmp);
    }

    // ===== leaf_segment =====

    #[test]
    fn leaf_segment_strips_qualifying_prefix() {
        assert_eq!(leaf_segment("model.shop.x"), "x");
        assert_eq!(leaf_segment("x"), "x");
        assert_eq!(leaf_segment(""), "");
    }

    // ===== cute-dbt#98 — data_diff + fixture wire shape =====

    use crate::domain::{
        Cell, CellChange, CellValue, ColumnStatus, DiffColumn, FixtureTableDiff, NamedTableDiff,
        RowChange, RowChangeKind, UnitTestDataDiff,
    };

    /// A `TestPayload` carrying a `data_diff` serializes the cell-diff with the
    /// EXACT JS contract the renderer will branch on (cute-dbt#138): each cell
    /// side is a `Cell` carrying BOTH `display` (the authored token) AND `key`
    /// (an adjacently-tagged `{"t": <type>, "v": <value>}` `CellValue`). The
    /// `Absent` key is `{"t":"absent"}` (no `"v"`), and the row/column enums
    /// are lowercase tokens. This pins the dual-axis wire shape independently
    /// of the domain's own round-trip test (which proves Rust↔Rust; this proves
    /// the JSON the template consumes) — cute-dbt#139 reads both axes here.
    #[test]
    fn data_diff_payload_wire_ships_both_display_and_key_axes() {
        let data_diff = UnitTestDataDiff {
            given: vec![NamedTableDiff {
                ordinal: 0,
                input: "ref('a')".into(),
                diff: FixtureTableDiff {
                    columns: vec![
                        DiffColumn {
                            name: "id".into(),
                            status: ColumnStatus::Present,
                        },
                        DiffColumn {
                            name: "city".into(),
                            status: ColumnStatus::Added,
                        },
                    ],
                    rows: vec![RowChange {
                        kind: RowChangeKind::Modified,
                        cells: vec![
                            // A format-only cell: authored "1.00" on the NEW
                            // side, "1" on the OLD side, but BOTH key to
                            // Number("1") → changed: false (the headline #138
                            // case: the diff shows the authored value yet does
                            // NOT flag).
                            CellChange {
                                old: Cell::with_display("1".into(), CellValue::Number("1".into())),
                                new: Cell::with_display(
                                    "1.00".into(),
                                    CellValue::Number("1".into()),
                                ),
                                changed: false,
                            },
                            CellChange {
                                old: Cell::new(CellValue::Absent),
                                new: Cell::new(CellValue::Str("NYC".into())),
                                changed: true,
                            },
                        ],
                    }],
                },
            }],
            expect: None,
        };
        let json = serde_json::to_value(&data_diff).expect("data_diff serializes");
        let cells = &json["given"][0]["diff"]["rows"][0]["cells"];
        // BOTH axes ship: `display` is the authored token; `key` is the
        // adjacently-tagged CellValue {"t":"number","v":"1"}.
        assert_eq!(
            cells[0]["new"]["display"], "1.00",
            "authored token survives"
        );
        assert_eq!(cells[0]["new"]["key"]["t"], "number");
        assert_eq!(cells[0]["new"]["key"]["v"], "1", "key is canonical");
        assert_eq!(cells[0]["old"]["display"], "1");
        assert_eq!(cells[0]["old"]["key"]["v"], "1");
        // A format-only change is NOT flagged (keys equal) yet shows the
        // authored NEW display.
        assert_eq!(cells[0]["changed"], false);
        // Str cell: display mirrors the key string verbatim.
        assert_eq!(cells[1]["new"]["display"], "NYC");
        assert_eq!(cells[1]["new"]["key"]["t"], "str");
        assert_eq!(cells[1]["new"]["key"]["v"], "NYC");
        // Absent key is a bare unit variant — tag only, NO "v" key; its
        // display is the empty string.
        assert_eq!(cells[1]["old"]["key"]["t"], "absent");
        assert!(
            cells[1]["old"]["key"].get("v").is_none(),
            "Absent key serializes with no \"v\" key; got {}",
            cells[1]["old"]["key"],
        );
        assert_eq!(cells[1]["old"]["display"], "");
        // Row-kind + column-status enums are lowercase tokens.
        assert_eq!(json["given"][0]["diff"]["rows"][0]["kind"], "modified");
        assert_eq!(json["given"][0]["diff"]["columns"][0]["status"], "present");
        assert_eq!(json["given"][0]["diff"]["columns"][1]["status"], "added");
        // The precomputed `changed` verdict rides on each cell.
        assert_eq!(cells[1]["changed"], true);
    }

    /// `build_test_payload` sets `data_diff` from the threaded map; the field
    /// appears on the wire only when present (`skip_serializing_if` mirror of
    /// `yaml_diff` / `sql_diff`). Absent → the JSON omits the key entirely,
    /// keeping baseline-mode reports byte-stable.
    #[test]
    fn build_test_payload_omits_data_diff_when_absent() {
        let ut = simple_unit_test("m", "t");
        let graph = CteGraph::default();
        let changed = InScopeSet::new();
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &graph,
            &changed,
            None,
            None,
            None,
            None,
            &manifest_for(vec![], vec![]),
            &BTreeMap::new(),
        );
        assert!(payload.data_diff.is_none());
        let json = serde_json::to_string(&payload).expect("payload serializes");
        assert!(
            !json.contains("data_diff"),
            "absent data_diff must be omitted from the wire; got {json}",
        );
    }

    /// When a `data_diff` is threaded, `build_test_payload` carries it and the
    /// wire key appears.
    #[test]
    fn build_test_payload_carries_data_diff_when_present() {
        let ut = simple_unit_test("m", "t");
        let graph = CteGraph::default();
        let changed = InScopeSet::new();
        let data_diff = UnitTestDataDiff {
            given: vec![],
            expect: Some(FixtureTableDiff {
                columns: vec![DiffColumn {
                    name: "id".into(),
                    status: ColumnStatus::Present,
                }],
                rows: vec![RowChange {
                    kind: RowChangeKind::Added,
                    cells: vec![CellChange {
                        old: Cell::new(CellValue::Absent),
                        new: Cell::new(CellValue::Number("9".into())),
                        changed: true,
                    }],
                }],
            }),
        };
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &graph,
            &changed,
            None,
            None,
            Some(&data_diff),
            None,
            &manifest_for(vec![], vec![]),
            &BTreeMap::new(),
        );
        assert_eq!(payload.data_diff.as_ref(), Some(&data_diff));
        let json = serde_json::to_string(&payload).expect("payload serializes");
        assert!(
            json.contains("data_diff"),
            "present data_diff is on the wire"
        );
    }

    // ===== cute-dbt#165 — column-header metadata =====

    /// A column-scoped generic-test node attached to `model_id`.
    fn column_test_node(id: &str, model_id: &str, column: &str, tm: TestMetadata) -> Node {
        Node::new(
            NodeId::new(id),
            "test",
            checksum("t"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            Some(column.to_owned()),
            Some(NodeId::new(model_id)),
            Some(tm),
        )
    }

    /// Shorthand: a name-only `ColumnTestPayload`.
    fn bare_test(name: &str) -> ColumnTestPayload {
        ColumnTestPayload {
            name: name.to_owned(),
            ..ColumnTestPayload::default()
        }
    }

    #[test]
    fn column_test_payload_maps_names_and_accepted_values() {
        // cute-dbt#178 — the handoff README §2.2 display-string mapping.
        // Built-ins, bare prose names.
        assert_eq!(
            column_test_payload(&TestMetadata::new("unique", None, Value::Null)),
            bare_test("unique")
        );
        assert_eq!(
            column_test_payload(&TestMetadata::new("not_null", None, Value::Null)),
            bare_test("not null")
        );
        // Unknown package test → package-qualified RAW identifier, no
        // values/detail (open-ended arg vocabularies stay uninterpreted).
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "expect_column_values_to_be_between",
                Some("dbt_expectations".to_owned()),
                json!({ "min_value": 0 }),
            )),
            bare_test("dbt_expectations.expect_column_values_to_be_between")
        );
        // accepted_values → the authored values list as distinct chips.
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_values",
                None,
                json!({ "values": ["placed", "shipped", 3] }),
            )),
            ColumnTestPayload {
                name: "accepted values".to_owned(),
                values: vec!["placed".to_owned(), "shipped".to_owned(), "3".to_owned()],
                detail: None,
            }
        );
        // accepted_values with no / empty values degrades to the bare name.
        assert_eq!(
            column_test_payload(&TestMetadata::new("accepted_values", None, Value::Null)),
            bare_test("accepted values")
        );
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_values",
                None,
                json!({ "values": [] })
            )),
            bare_test("accepted values")
        );
    }

    #[test]
    fn column_test_payload_maps_relationships_and_accepted_range_details() {
        // cute-dbt#178 — the README §2.2 detail mappings.
        // relationships → "model.field" detail (ref('…') unwrapped).
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "relationships",
                None,
                json!({ "to": "ref('customers')", "field": "customer_id" }),
            )),
            ColumnTestPayload {
                name: "relationships".to_owned(),
                values: Vec::new(),
                detail: Some("customers.customer_id".to_owned()),
            }
        );
        // relationships with a missing field still names the target; a
        // source('…','…') unwraps to its LAST quoted argument.
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "relationships",
                None,
                json!({ "to": "source('raw', 'customers')" }),
            ))
            .detail
            .as_deref(),
            Some("customers")
        );
        // a non-call `to` target renders verbatim.
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "relationships",
                None,
                json!({ "to": "customers", "field": "id" }),
            ))
            .detail
            .as_deref(),
            Some("customers.id")
        );
        // accepted_range → range detail; maps by bare name regardless of
        // namespace (it usually ships as dbt_utils.accepted_range).
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_range",
                Some("dbt_utils".to_owned()),
                json!({ "min_value": 0, "max_value": 100 }),
            )),
            ColumnTestPayload {
                name: "accepted range".to_owned(),
                values: Vec::new(),
                detail: Some("0\u{2013}100".to_owned()),
            }
        );
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_range",
                Some("dbt_utils".to_owned()),
                json!({ "min_value": 0 }),
            ))
            .detail
            .as_deref(),
            Some("\u{2265} 0")
        );
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_range",
                Some("dbt_utils".to_owned()),
                json!({ "max_value": 1 }),
            ))
            .detail
            .as_deref(),
            Some("\u{2264} 1")
        );
        assert_eq!(
            column_test_payload(&TestMetadata::new(
                "accepted_range",
                Some("dbt_utils".to_owned()),
                Value::Null,
            )),
            bare_test("accepted range")
        );
    }

    #[test]
    fn column_meta_for_model_merges_descriptions_and_column_scoped_tests() {
        let mut descriptions = BTreeMap::new();
        descriptions.insert("id".to_owned(), "Primary key".to_owned());
        descriptions.insert("note".to_owned(), "Free text".to_owned());
        let model = model_node("model.shop.dim_x", "x", Some("select 1"))
            .with_column_descriptions(descriptions);
        let manifest = manifest_for(
            vec![
                model.clone(),
                column_test_node(
                    "test.shop.unique_dim_x_id",
                    "model.shop.dim_x",
                    "id",
                    TestMetadata::new("unique", None, Value::Null),
                ),
                column_test_node(
                    "test.shop.not_null_dim_x_id",
                    "model.shop.dim_x",
                    "id",
                    TestMetadata::new("not_null", None, Value::Null),
                ),
                // tested-but-undescribed column → tests-only entry
                column_test_node(
                    "test.shop.not_null_dim_x_status",
                    "model.shop.dim_x",
                    "status",
                    TestMetadata::new("not_null", None, Value::Null),
                ),
                // attached to ANOTHER model → excluded
                column_test_node(
                    "test.shop.unique_other_id",
                    "model.shop.other",
                    "id",
                    TestMetadata::new("unique", None, Value::Null),
                ),
                // model-level test (no column_name) → excluded (v1 scope)
                Node::new(
                    NodeId::new("test.shop.model_level"),
                    "test",
                    checksum("t"),
                    None,
                    None,
                    DependsOn::default(),
                    None,
                    NodeConfig::default(),
                    None,
                    BTreeMap::new(),
                )
                .with_test_attachment(
                    None,
                    Some(NodeId::new("model.shop.dim_x")),
                    Some(TestMetadata::new("custom_model_check", None, Value::Null)),
                ),
                // singular test (no test_metadata) → excluded
                Node::new(
                    NodeId::new("test.shop.singular"),
                    "test",
                    checksum("t"),
                    None,
                    None,
                    DependsOn::default(),
                    None,
                    NodeConfig::default(),
                    None,
                    BTreeMap::new(),
                )
                .with_test_attachment(
                    Some("id".to_owned()),
                    Some(NodeId::new("model.shop.dim_x")),
                    None,
                ),
            ],
            vec![],
        );
        let model_ref = manifest.node(&NodeId::new("model.shop.dim_x")).unwrap();
        let meta = column_meta_for_model(&manifest, model_ref);

        let id_meta = meta.get("id").expect("id has metadata");
        assert_eq!(id_meta.description.as_deref(), Some("Primary key"));
        // Sorted by display name → deterministic regardless of HashMap order.
        assert_eq!(
            id_meta.tests,
            vec![bare_test("not null"), bare_test("unique")]
        );

        let note_meta = meta.get("note").expect("described-only column present");
        assert_eq!(note_meta.description.as_deref(), Some("Free text"));
        assert!(note_meta.tests.is_empty());

        let status_meta = meta.get("status").expect("tested-only column present");
        assert!(status_meta.description.is_none());
        assert_eq!(status_meta.tests, vec![bare_test("not null")]);

        assert_eq!(meta.len(), 3, "no entries beyond id/note/status: {meta:?}");
    }

    #[test]
    fn build_test_payload_attaches_column_meta_per_table_owner() {
        // Target model dim_x (expect cols id/status) + input model stg_src
        // (given col src_id). The expect map must come from dim_x, the
        // given map from stg_src, each filtered to its own table's columns.
        let mut target_desc = BTreeMap::new();
        target_desc.insert("id".to_owned(), "Primary key".to_owned());
        target_desc.insert("not_in_fixture".to_owned(), "never rendered".to_owned());
        let target = model_node("model.shop.dim_x", "x", Some("select 1"))
            .with_column_descriptions(target_desc);
        let mut src_desc = BTreeMap::new();
        src_desc.insert("src_id".to_owned(), "Source key".to_owned());
        let src = model_node("model.shop.stg_src", "s", Some("select 1"))
            .with_column_descriptions(src_desc);
        let manifest = manifest_for(
            vec![
                target.clone(),
                src,
                column_test_node(
                    "test.shop.unique_dim_x_id",
                    "model.shop.dim_x",
                    "id",
                    TestMetadata::new("unique", None, Value::Null),
                ),
            ],
            vec![],
        );
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![UnitTestGiven::new(
                "ref('stg_src')",
                json!([{ "src_id": 1, "unknown_col": 2 }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(
                json!([{ "id": 1, "status": "ok" }]),
                Some("dict".to_owned()),
                None,
            ),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let target_meta = column_meta_for_model(&manifest, &target);
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest,
            &target_meta,
        );

        // Expect side: dim_x meta filtered to the expect table's columns —
        // `id` (described + tested) is present; `status` (no meta) and
        // `not_in_fixture` (not a table column) are absent.
        let expect_meta = &payload.expected.column_meta;
        assert_eq!(
            expect_meta.get("id").and_then(|m| m.description.as_deref()),
            Some("Primary key")
        );
        assert_eq!(
            expect_meta.get("id").map(|m| m.tests.as_slice()),
            Some(&[bare_test("unique")][..])
        );
        assert!(!expect_meta.contains_key("status"));
        assert!(!expect_meta.contains_key("not_in_fixture"));

        // Given side: stg_src meta — its own description, never dim_x's.
        let given_meta = &payload.given[0].column_meta;
        assert_eq!(
            given_meta
                .get("src_id")
                .and_then(|m| m.description.as_deref()),
            Some("Source key")
        );
        assert!(!given_meta.contains_key("unknown_col"));
        assert!(!given_meta.contains_key("id"));
    }

    #[test]
    fn build_test_payload_this_given_uses_target_model_meta() {
        let mut target_desc = BTreeMap::new();
        target_desc.insert("id".to_owned(), "Primary key".to_owned());
        let target = model_node("model.shop.dim_x", "x", Some("select 1"))
            .with_column_descriptions(target_desc);
        let manifest = manifest_for(vec![target.clone()], vec![]);
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![UnitTestGiven::new(
                "this",
                json!([{ "id": 1 }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(json!([{ "id": 2 }]), Some("dict".to_owned()), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let target_meta = column_meta_for_model(&manifest, &target);
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest,
            &target_meta,
        );
        assert_eq!(
            payload.given[0]
                .column_meta
                .get("id")
                .and_then(|m| m.description.as_deref()),
            Some("Primary key"),
            "a `this` given is the model's own prior state — target meta applies",
        );
    }

    #[test]
    fn resolve_given_ref_node_resolves_a_versioned_model_by_ingested_name() {
        // cute-dbt#256 (the #254 handoff): a versioned model's leaf
        // segment is the VERSION SUFFIX ("v2") — pre-#256 a given's
        // ref('dim_customers') could never bind it. The ingested wire
        // `name` is the truthful handle; among version siblings the node
        // whose version equals latest_version wins (dbt's unpinned-ref
        // resolution rule).
        let v1 = model_node("model.shop.dim_customers.v1", "v1", Some("select 1"))
            .with_identity(Some("dim_customers".to_owned()), Some("shop".to_owned()))
            .with_versions(Some("1".to_owned()), Some("2".to_owned()), None);
        let v2 = model_node("model.shop.dim_customers.v2", "v2", Some("select 2"))
            .with_identity(Some("dim_customers".to_owned()), Some("shop".to_owned()))
            .with_versions(Some("2".to_owned()), Some("2".to_owned()), None);
        let manifest = manifest_for(vec![v1, v2], vec![]);
        let resolved = resolve_given_ref_node(&manifest, "dim_customers")
            .expect("the authored name binds versioned nodes");
        assert_eq!(
            resolved.id().as_str(),
            "model.shop.dim_customers.v2",
            "the latest version wins for an unpinned ref",
        );
    }

    #[test]
    fn resolve_given_ref_node_keeps_the_leaf_fallback_and_determinism() {
        // Pre-#256 synthetic manifests carry no `name` — the leaf
        // fallback (via Node::bare_name) preserves the old behavior,
        // including smallest-id determinism under a leaf collision.
        let model = model_node("model.shop.raw_orders", "m", Some("select 1"));
        let seed = Node::new(
            NodeId::new("seed.shop.raw_orders"),
            "seed",
            checksum("s"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        );
        let manifest = manifest_for(vec![model, seed], vec![]);
        let resolved =
            resolve_given_ref_node(&manifest, "raw_orders").expect("leaf fallback resolves");
        assert_eq!(
            resolved.id().as_str(),
            "model.shop.raw_orders",
            "model.* sorts before seed.* — unchanged determinism",
        );
    }

    #[test]
    fn build_test_payload_attaches_seed_column_meta_for_a_seed_ref_given() {
        // cute-dbt#235 — dbt's ref() resolves over the refable set
        // (models, seeds, snapshots), and a unit-test given may ref a
        // seed (the committed jaffle-shop fixture's
        // `ref('raw_customers')` is exactly this shape). The given's
        // column tooltips must ride the SEED's declared metadata the
        // same way a model-ref given rides its input model's.
        let mut seed_desc = BTreeMap::new();
        seed_desc.insert("customer_id".to_owned(), "Seed primary key".to_owned());
        let seed = Node::new(
            NodeId::new("seed.shop.raw_customers"),
            "seed",
            checksum("seed"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_column_descriptions(seed_desc);
        let target = model_node("model.shop.dim_x", "x", Some("select 1"));
        let manifest = manifest_for(
            vec![
                target,
                seed,
                column_test_node(
                    "test.shop.unique_raw_customers_customer_id",
                    "seed.shop.raw_customers",
                    "customer_id",
                    TestMetadata::new("unique", None, Value::Null),
                ),
            ],
            vec![],
        );
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![UnitTestGiven::new(
                "ref('raw_customers')",
                json!([{ "customer_id": 1, "undocumented": 2 }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(json!([{ "id": 1 }]), Some("dict".to_owned()), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest,
            &BTreeMap::new(),
        );
        let given_meta = &payload.given[0].column_meta;
        assert_eq!(
            given_meta
                .get("customer_id")
                .and_then(|m| m.description.as_deref()),
            Some("Seed primary key"),
            "a seed-ref given resolves the seed's column descriptions",
        );
        assert_eq!(
            given_meta.get("customer_id").map(|m| m.tests.as_slice()),
            Some(&[bare_test("unique")][..]),
            "column-scoped tests attached to the seed ride along",
        );
        assert!(
            !given_meta.contains_key("undocumented"),
            "honest degrade: an undeclared fixture column carries no meta",
        );
    }

    #[test]
    fn build_test_payload_attaches_source_column_meta_for_a_source_given() {
        // cute-dbt#235 — a `source('a','b')` given resolves the SOURCE's
        // declared column descriptions (fusion `ManifestSource.columns`,
        // verified on the committed playground fixture's
        // `synthea_raw.patients.Id`) plus any column-scoped data tests
        // attached to the source node.
        let mut src_desc = BTreeMap::new();
        src_desc.insert("Id".to_owned(), "Unique patient identifier".to_owned());
        let source = SourceNode::new(
            NodeId::new("source.shop.raw.patients"),
            "raw",
            "patients",
            None,
            "main",
            None,
            None,
        )
        .with_column_descriptions(src_desc);
        let target = model_node("model.shop.dim_x", "x", Some("select 1"));
        let manifest = manifest_with_sources(
            vec![
                target,
                column_test_node(
                    "test.shop.not_null_raw_patients_Id",
                    "source.shop.raw.patients",
                    "Id",
                    TestMetadata::new("not_null", None, Value::Null),
                ),
            ],
            vec![],
            vec![source],
        );
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![UnitTestGiven::new(
                "source('raw', 'patients')",
                json!([{ "Id": "a-1", "FIRST": "Ada" }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(json!([{ "id": 1 }]), Some("dict".to_owned()), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest,
            &BTreeMap::new(),
        );
        let given_meta = &payload.given[0].column_meta;
        assert_eq!(
            given_meta.get("Id").and_then(|m| m.description.as_deref()),
            Some("Unique patient identifier"),
            "a source given resolves the source's column descriptions",
        );
        assert_eq!(
            given_meta.get("Id").map(|m| m.tests.as_slice()),
            Some(&[bare_test("not null")][..]),
            "column-scoped tests attached to the source ride along",
        );
        assert!(
            !given_meta.contains_key("FIRST"),
            "honest degrade: an undescribed source column carries no meta",
        );
    }

    #[test]
    fn source_given_with_no_declared_columns_keeps_column_meta_empty() {
        // cute-dbt#235 honest degrade at the payload level: a source
        // with no declared columns contributes NOTHING — the empty map
        // is omitted from the wire and the JS renders no trigger
        // (never an empty bubble).
        let source = SourceNode::new(
            NodeId::new("source.shop.raw.events"),
            "raw",
            "events",
            None,
            "main",
            None,
            None,
        );
        let manifest = manifest_with_sources(
            vec![model_node("model.shop.dim_x", "x", Some("select 1"))],
            vec![],
            vec![source],
        );
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![UnitTestGiven::new(
                "source('raw', 'events')",
                json!([{ "event_id": 1 }]),
                Some("dict".to_owned()),
                None,
            )],
            UnitTestExpect::new(json!([{ "id": 1 }]), Some("dict".to_owned()), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest,
            &BTreeMap::new(),
        );
        assert!(
            payload.given[0].column_meta.is_empty(),
            "no declared source columns → empty (omitted) column_meta",
        );
    }

    #[test]
    fn column_meta_is_omitted_from_the_wire_when_empty() {
        // A model with NO column metadata: the payload key must be absent
        // (skip_serializing_if) so pre-#165 reports stay byte-stable in
        // shape and the template's `colMeta || {}` read stays minimal.
        let ut = simple_unit_test("m", "t");
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &CteGraph::default(),
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest_for(vec![], vec![]),
            &BTreeMap::new(),
        );
        let json = serde_json::to_string(&payload).expect("serialize");
        assert!(
            !json.contains("column_meta"),
            "empty column_meta must be omitted from the wire; got {json}",
        );
    }

    /// The external-fixture guard signal (cute-dbt#98, #126): an external
    /// `given`/`expect` carries `rows: null` + a `fixture` name. The payload
    /// surfaces `fixture` (so the JS can show the affordance + YAML fallback)
    /// and `rows` is JSON `null` — the two facts the JS needs to AVOID
    /// rendering a silently-empty grid. An inline-rows given omits `fixture`.
    #[test]
    fn external_fixture_given_carries_fixture_name_and_null_rows() {
        let ut = UnitTest::new(
            "t",
            NodeId::new("model.shop.m"),
            vec![UnitTestGiven::new(
                "ref('a')",
                Value::Null,
                Some("csv".to_owned()),
                Some("stg_a_fixture".to_owned()),
            )],
            UnitTestExpect::new(json!([{"id": 1}]), Some("dict".to_owned()), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let graph = CteGraph::default();
        let payload = build_test_payload(
            "unit_test.shop.t",
            &ut,
            &graph,
            &InScopeSet::new(),
            None,
            None,
            None,
            None,
            &manifest_for(vec![], vec![]),
            &BTreeMap::new(),
        );
        // The external given: fixture name present, rows null.
        assert_eq!(payload.given[0].fixture.as_deref(), Some("stg_a_fixture"));
        assert!(
            payload.given[0].rows.is_null(),
            "external-fixture given has null rows (data not in manifest)",
        );
        // The inline expect: fixture omitted, rows present.
        assert!(payload.expected.fixture.is_none());
        assert!(payload.expected.rows.is_array());
        // Wire shape: fixture key present on the given, absent on the expect.
        let json = serde_json::to_string(&payload).expect("serialize");
        assert!(
            json.contains("stg_a_fixture"),
            "given fixture is on the wire"
        );
        let expect_json = serde_json::to_string(&payload.expected).expect("serialize expect");
        assert!(
            !expect_json.contains("fixture"),
            "inline expect omits the fixture key; got {expect_json}",
        );
    }

    // ===== cute-dbt#200 — manifest_nodes + overrides + description =====

    /// A model-level (no `column_name`) generic-test node attached to
    /// `model_id`.
    fn model_test_node(id: &str, model_id: &str, tm: TestMetadata) -> Node {
        Node::new(
            NodeId::new(id),
            "test",
            checksum("t"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(None, Some(NodeId::new(model_id)), Some(tm))
    }

    /// A described + tagged + typed-columns model node for the
    /// `manifest_nodes` tests.
    fn context_rich_model(id: &str) -> Node {
        let mut config = BTreeMap::new();
        config.insert("materialized".to_owned(), Value::from("incremental"));
        let mut columns = BTreeMap::new();
        columns.insert("id".to_owned(), Some("bigint".to_owned()));
        columns.insert("status".to_owned(), None);
        let mut descriptions = BTreeMap::new();
        descriptions.insert("id".to_owned(), "Primary key".to_owned());
        Node::new(
            NodeId::new(id),
            "model",
            checksum("body"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            columns,
        )
        .with_column_descriptions(descriptions)
        .with_model_metadata(
            Some("One row per payer.".to_owned()),
            vec!["marts".to_owned(), "finance".to_owned()],
        )
    }

    /// The demo-payload-shaped grouped overrides (native scalars).
    fn grouped_overrides() -> crate::domain::UnitTestOverrides {
        let mut overrides = crate::domain::UnitTestOverrides::new();
        overrides.insert(
            "macros".to_owned(),
            [("is_incremental".to_owned(), json!(true))].into(),
        );
        overrides.insert(
            "vars".to_owned(),
            [
                ("encounter_lookback_days".to_owned(), json!(7)),
                ("dq_quarantine_threshold".to_owned(), json!(0.05)),
            ]
            .into(),
        );
        overrides
    }

    #[test]
    fn manifest_nodes_entry_carries_the_full_model_context() {
        let model = context_rich_model("model.shop.dim_payers");
        let manifest = manifest_for(
            vec![
                model,
                column_test_node(
                    "test.shop.unique_dim_payers_id",
                    "model.shop.dim_payers",
                    "id",
                    TestMetadata::new("unique", None, Value::Null),
                ),
                // Known built-in at MODEL level → §2.2 prose name + detail.
                model_test_node(
                    "test.shop.relationships_dim_payers",
                    "model.shop.dim_payers",
                    TestMetadata::new(
                        "relationships",
                        None,
                        json!({ "to": "ref('stg_payers')", "field": "payer_id" }),
                    ),
                ),
                // Unknown package test at MODEL level → package-qualified
                // raw name, no detail (open-ended kwargs uninterpreted).
                model_test_node(
                    "test.shop.unique_combo_dim_payers",
                    "model.shop.dim_payers",
                    TestMetadata::new(
                        "unique_combination_of_columns",
                        Some("dbt_utils".to_owned()),
                        json!({ "combination_of_columns": ["id", "status"] }),
                    ),
                ),
            ],
            vec![],
        );
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_payers")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let entry = payload
            .manifest_nodes
            .get("dim_payers")
            .expect("in-scope model keyed by BARE name");
        assert_eq!(entry.description.as_deref(), Some("One row per payer."));
        assert_eq!(entry.materialized.as_deref(), Some("incremental"));
        assert_eq!(entry.tags, ["marts".to_owned(), "finance".to_owned()]);
        assert_eq!(
            entry.columns,
            vec![
                ManifestColumnPayload {
                    name: "id".to_owned(),
                    description: Some("Primary key".to_owned()),
                    column_type: Some("bigint".to_owned()),
                    tests: vec![bare_test("unique")],
                },
                ManifestColumnPayload {
                    name: "status".to_owned(),
                    description: None,
                    column_type: None,
                    tests: vec![],
                },
            ],
            "declared columns in name order, decorated with #165 meta"
        );
        assert_eq!(
            entry.model_tests,
            vec![
                ModelTestPayload {
                    name: "dbt_utils.unique_combination_of_columns".to_owned(),
                    detail: None,
                },
                ModelTestPayload {
                    name: "relationships".to_owned(),
                    detail: Some("stg_payers.payer_id".to_owned()),
                },
            ],
            "model-level tests via the §2.2 mapping, sorted by name"
        );
    }

    #[test]
    fn manifest_nodes_include_ref_ed_upstreams_and_exclude_unrelated_models() {
        let target = model_node("model.shop.dim_x", "x", Some("select 1"));
        let upstream = model_node("model.shop.stg_src", "s", Some("select 1"))
            .with_model_metadata(Some("Staged source.".to_owned()), Vec::new());
        // Described but neither in scope nor ref()-ed → must NOT appear
        // (never the whole project graph).
        let unrelated = model_node("model.shop.dim_unrelated", "u", Some("select 1"))
            .with_model_metadata(Some("Unrelated.".to_owned()), Vec::new());
        let ut = UnitTest::new(
            "t",
            NodeId::new("dim_x"),
            vec![
                UnitTestGiven::new("ref('stg_src')", json!([]), None, None),
                // `this` resolves to the (already-present) target model.
                UnitTestGiven::new("this", json!([]), None, None),
                // source(...) inputs contribute nothing (not model nodes).
                UnitTestGiven::new("source('raw', 'orders')", json!([]), None, None),
                // An unresolvable ref contributes nothing (graceful).
                UnitTestGiven::new("ref('not_a_model')", json!([]), None, None),
            ],
            UnitTestExpect::new(json!([]), None, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(
            vec![target, upstream, unrelated],
            vec![("unit_test.shop.t", ut)],
        );
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.dim_x")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert_eq!(
            payload.manifest_nodes.keys().collect::<Vec<_>>(),
            ["stg_src"],
            "the ref()-ed upstream appears; the unrelated model does not \
             (dim_x itself is all-empty context → skipped)"
        );
        assert_eq!(
            payload.manifest_nodes["stg_src"].description.as_deref(),
            Some("Staged source.")
        );
    }

    #[test]
    fn manifest_nodes_key_is_omitted_when_every_entry_is_empty() {
        // Bare synthetic models (no description/tags/materialized/columns/
        // attached tests) must keep the manifest_nodes key OFF the wire —
        // the pre-#200 byte-stability contract.
        let node = model_node("model.shop.m", "body", Some("select 1"));
        let ut = simple_unit_test("m", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert!(
            payload.manifest_nodes.is_empty(),
            "all-empty entries are skipped"
        );
        let json = serde_json::to_string(&payload).unwrap();
        assert!(
            !json.contains("manifest_nodes"),
            "empty lookup omits the key entirely: {json}"
        );
    }

    #[test]
    fn test_payload_overrides_round_trip_groups_and_native_scalar_types() {
        // The cute-dbt#197 founder decision, asserted at the WIRE level:
        // the serialized payload carries JSON bool/number/string scalars —
        // never their stringified forms.
        let node = model_node("model.shop.m", "body", Some("select 1"));
        let ut = simple_unit_test("m", "test_one").with_overrides(Some(grouped_overrides()));
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert_eq!(test.overrides.as_ref(), Some(&grouped_overrides()));
        let json = serde_json::to_string(test).unwrap();
        assert!(
            json.contains(r#""overrides":{"macros":{"is_incremental":true},"vars":{"dq_quarantine_threshold":0.05,"encounter_lookback_days":7}}"#),
            "grouped map with native scalars (bool/float/int), deterministic order: {json}"
        );
        assert!(
            !json.contains(r#""is_incremental":"true""#),
            "never stringified: {json}"
        );
    }

    #[test]
    fn test_payload_omits_the_overrides_key_when_none() {
        let node = model_node("model.shop.m", "body", Some("select 1"));
        let ut = simple_unit_test("m", "test_one");
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let json = serde_json::to_string(&payload.models[0].tests[0]).unwrap();
        assert!(
            !json.contains("overrides"),
            "no-override tests stay byte-stable: {json}"
        );
    }

    #[test]
    fn model_payload_threads_the_model_description() {
        let node = model_node("model.shop.m", "body", Some("select 1"))
            .with_model_metadata(Some("One row per order.".to_owned()), Vec::new());
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        assert_eq!(
            payload.models[0].description.as_deref(),
            Some("One row per order.")
        );
    }

    #[test]
    fn model_payload_omits_the_description_key_when_none() {
        let node = model_node("model.shop.m", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.m")]);
        let payload = build_payload(
            &manifest,
            &InScopeSet::new(),
            &models,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            "b",
        );
        let json = serde_json::to_string(&payload.models[0]).unwrap();
        assert!(
            !json.contains("description"),
            "undescribed models stay byte-stable: {json}"
        );
    }

    // ===== PrDagPayload::from_graph (cute-dbt#404) =====

    /// Build a [`PrDagNode`] with the given id/state/connector flag and a
    /// fixed `±` line delta — the minimum a `from_graph` descriptor-count
    /// test needs. `name` is derived from the id's leaf for legibility.
    fn pr_dag_node(
        id: &str,
        state: crate::domain::PrDagState,
        is_connector: bool,
        lines_added: usize,
        lines_removed: usize,
    ) -> crate::domain::PrDagNode {
        crate::domain::PrDagNode {
            id: id.to_owned(),
            name: id.rsplit('.').next().unwrap_or(id).to_owned(),
            state,
            is_connector,
            is_halo: false,
            lines_added,
            lines_removed,
        }
    }

    #[test]
    fn from_graph_classifies_each_node_state_into_its_descriptor_tier() {
        use crate::domain::PrDagState;
        // One of every classifiable shape: New + Modified both fold into the
        // "modified" tier; Deleted into deleted; a connector (regardless of
        // its placeholder state) into connectors.
        let graph = PrDagGraph {
            nodes: vec![
                pr_dag_node("model.shop.a_new", PrDagState::New, false, 9, 0),
                pr_dag_node("model.shop.b_mod", PrDagState::Modified, false, 3, 2),
                pr_dag_node("model.shop.c_conn", PrDagState::Modified, true, 0, 0),
                pr_dag_node("model.shop.d_del", PrDagState::Deleted, false, 0, 7),
            ],
            edges: vec![
                crate::domain::PrDagEdge {
                    from: "model.shop.a_new".to_owned(),
                    to: "model.shop.c_conn".to_owned(),
                },
                crate::domain::PrDagEdge {
                    from: "model.shop.c_conn".to_owned(),
                    to: "model.shop.b_mod".to_owned(),
                },
            ],
        };

        let payload = PrDagPayload::from_graph(graph.clone(), 48);

        // New + Modified collapse to the modified tier; connector and deleted
        // each go to their own.
        assert_eq!(payload.modified_count, 2, "New + Modified ⇒ modified tier");
        assert_eq!(payload.connector_count, 1, "is_connector wins over state");
        assert_eq!(payload.halo_count, 0, "no halo node in this fixture");
        assert_eq!(payload.deleted_count, 1, "Deleted ⇒ deleted tier");
        // The descriptor tiers partition the node set exactly.
        assert_eq!(
            payload.modified_count
                + payload.connector_count
                + payload.halo_count
                + payload.deleted_count,
            graph.nodes.len(),
            "tiers partition the node set"
        );
        // The graph rides through verbatim — nodes, edges, and per-node
        // line deltas are not mutated by the wrap.
        assert_eq!(payload.graph.nodes, graph.nodes, "nodes carried verbatim");
        assert_eq!(payload.graph.edges, graph.edges, "edges carried verbatim");
        assert_eq!(payload.graph.nodes[0].lines_added, 9);
        assert_eq!(payload.graph.nodes[3].lines_removed, 7);
        // 4 nodes ≤ cap 48 ⇒ not collapsed.
        assert!(!payload.collapsed, "node count under cap ⇒ inline render");
    }

    #[test]
    fn from_graph_connector_flag_overrides_a_deleted_state() {
        use crate::domain::PrDagState;
        // A connector is counted as a connector even if its placeholder state
        // is Deleted — `is_connector` is the first branch, so it wins. This
        // pins the branch order (the else-if on Deleted is unreachable for a
        // connector node).
        let graph = PrDagGraph {
            nodes: vec![pr_dag_node(
                "model.shop.weird",
                PrDagState::Deleted,
                true,
                0,
                0,
            )],
            ..PrDagGraph::default()
        };
        let payload = PrDagPayload::from_graph(graph, 48);
        assert_eq!(payload.connector_count, 1);
        assert_eq!(payload.deleted_count, 0, "connector flag wins over Deleted");
        assert_eq!(payload.modified_count, 0);
    }

    #[test]
    fn from_graph_counts_halo_nodes_in_their_own_dimmed_context_tier() {
        use crate::domain::PrDagState;
        // A disconnected modified model (cute-dbt#428) with two 1-hop halo
        // neighbors: the halo nodes carry `state=Modified` + `is_halo` (their
        // structural placeholder), but they must NOT inflate `modified_count`
        // — they land in `halo_count`, the dimmed-context tier.
        let halo = |id: &str| crate::domain::PrDagNode {
            id: id.to_owned(),
            name: id.rsplit('.').next().unwrap_or(id).to_owned(),
            state: PrDagState::Modified,
            is_connector: false,
            is_halo: true,
            lines_added: 0,
            lines_removed: 0,
        };
        let graph = PrDagGraph {
            nodes: vec![
                pr_dag_node("model.shop.m", PrDagState::Modified, false, 4, 1),
                halo("model.shop.up"),
                halo("model.shop.down"),
            ],
            ..PrDagGraph::default()
        };
        let payload = PrDagPayload::from_graph(graph.clone(), 48);
        assert_eq!(payload.modified_count, 1, "only the genuine modified model");
        assert_eq!(payload.halo_count, 2, "the two 1-hop neighbors ⇒ halo tier");
        assert_eq!(payload.connector_count, 0);
        assert_eq!(payload.deleted_count, 0);
        // The four tiers still partition the node set exactly.
        assert_eq!(
            payload.modified_count
                + payload.connector_count
                + payload.halo_count
                + payload.deleted_count,
            graph.nodes.len(),
        );
    }

    #[test]
    fn from_graph_collapses_only_when_node_count_strictly_exceeds_cap() {
        use crate::domain::PrDagState;
        let nodes: Vec<crate::domain::PrDagNode> = (0..3)
            .map(|i| {
                pr_dag_node(
                    &format!("model.shop.m{i}"),
                    PrDagState::Modified,
                    false,
                    1,
                    0,
                )
            })
            .collect();

        // count == cap ⇒ NOT collapsed (the bound is `> cap`, not `>= cap`).
        let at_cap = PrDagPayload::from_graph(
            PrDagGraph {
                nodes: nodes.clone(),
                ..PrDagGraph::default()
            },
            3,
        );
        assert!(!at_cap.collapsed, "node_count == cap ⇒ inline render");

        // count > cap ⇒ collapsed (Mermaid suppressed; data still rides JSON).
        let over_cap = PrDagPayload::from_graph(
            PrDagGraph {
                nodes,
                ..PrDagGraph::default()
            },
            2,
        );
        assert!(over_cap.collapsed, "node_count > cap ⇒ summary line");
        assert_eq!(
            over_cap.modified_count, 3,
            "counts computed even when collapsed"
        );
    }

    #[test]
    fn from_graph_on_empty_graph_yields_all_zero_counts_uncollapsed() {
        let payload = PrDagPayload::from_graph(PrDagGraph::default(), 48);
        assert_eq!(payload.modified_count, 0);
        assert_eq!(payload.connector_count, 0);
        assert_eq!(payload.deleted_count, 0);
        assert!(!payload.collapsed, "0 nodes ≤ any cap ⇒ not collapsed");
        assert!(payload.graph.nodes.is_empty());
        assert!(payload.graph.edges.is_empty());
    }

    #[test]
    fn from_graph_leaves_by_axis_empty_so_baseline_goldens_stay_byte_identical() {
        // The baseline-arm / no-axis constructor: `by_axis` is empty, so the
        // serde-skip keeps every pre-#430 + baseline golden byte-identical.
        use crate::domain::PrDagState;
        let graph = PrDagGraph {
            nodes: vec![pr_dag_node(
                "model.shop.a",
                PrDagState::Modified,
                false,
                1,
                0,
            )],
            ..PrDagGraph::default()
        };
        let payload = PrDagPayload::from_graph(graph, 48);
        assert!(payload.by_axis.is_empty(), "no per-axis views on this arm");
        // serde-skip: the key never appears in JSON for an empty map.
        let json = serde_json::to_string(&payload).expect("serialize");
        assert!(
            !json.contains("by_axis"),
            "empty by_axis is omitted from JSON (byte-identity)"
        );
    }

    #[test]
    fn from_graph_with_axes_wraps_each_axis_subgraph_with_its_own_counts() {
        use crate::domain::{PrDagEdge, PrDagState};
        // The "all" view: stg_orders + fct_orders modified, int_order_items the
        // connector between them.
        let all = PrDagGraph {
            nodes: vec![
                pr_dag_node("model.shop.stg", PrDagState::Modified, false, 2, 1),
                pr_dag_node("model.shop.int", PrDagState::Modified, true, 0, 0),
                pr_dag_node("model.shop.fct", PrDagState::Modified, false, 3, 0),
            ],
            edges: vec![
                PrDagEdge {
                    from: "model.shop.stg".to_owned(),
                    to: "model.shop.int".to_owned(),
                },
                PrDagEdge {
                    from: "model.shop.int".to_owned(),
                    to: "model.shop.fct".to_owned(),
                },
            ],
        };
        // The "config" subset: only fct_orders fired config — a single
        // disconnected modified model, no connectors.
        let config = PrDagGraph {
            nodes: vec![pr_dag_node(
                "model.shop.fct",
                PrDagState::Modified,
                false,
                0,
                0,
            )],
            ..PrDagGraph::default()
        };
        let mut by_axis = BTreeMap::new();
        by_axis.insert("config".to_owned(), config);

        let payload = PrDagPayload::from_graph_with_axes(all, by_axis, 48);
        // The top-level "all" view: 2 modified + 1 connector.
        assert_eq!(payload.modified_count, 2);
        assert_eq!(payload.connector_count, 1);
        // The per-axis "config" view: just the one modified model, no
        // connector recomputed over the subset.
        let cfg = payload.by_axis.get("config").expect("config view present");
        assert_eq!(cfg.modified_count, 1, "config subset is one modified model");
        assert_eq!(
            cfg.connector_count, 0,
            "no connector in a single-seed subset"
        );
        assert_eq!(cfg.graph.nodes.len(), 1);
        assert!(!cfg.collapsed);
        // The map is serialized under `by_axis` when non-empty.
        let json = serde_json::to_string(&payload).expect("serialize");
        assert!(json.contains("by_axis"), "non-empty by_axis is serialized");
    }

    #[test]
    fn from_graph_with_axes_view_counts_halo_separately() {
        use crate::domain::{PrDagNode, PrDagState};
        // A per-axis subset where the one modified model is disconnected and
        // pulls in a halo neighbor — the halo lands in its own tier, never
        // inflating the subset's modified_count.
        let halo = PrDagNode {
            id: "model.shop.up".to_owned(),
            name: "up".to_owned(),
            state: PrDagState::Modified,
            is_connector: false,
            is_halo: true,
            lines_added: 0,
            lines_removed: 0,
        };
        let body = PrDagGraph {
            nodes: vec![
                pr_dag_node("model.shop.m", PrDagState::Modified, false, 4, 0),
                halo,
            ],
            ..PrDagGraph::default()
        };
        let mut by_axis = BTreeMap::new();
        by_axis.insert("body".to_owned(), body);
        let payload = PrDagPayload::from_graph_with_axes(PrDagGraph::default(), by_axis, 48);
        let v = payload.by_axis.get("body").expect("body view");
        assert_eq!(v.modified_count, 1);
        assert_eq!(v.halo_count, 1, "the 1-hop neighbor is dimmed context");
        assert_eq!(v.connector_count, 0);
    }
}
