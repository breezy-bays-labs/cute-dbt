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

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::Path;

use askama::Template;
use serde::Serialize;
use serde_json::Value;

use crate::adapters::asset_embed::{
    DATATABLES_CSS, DATATABLES_JS, FAVICON_DATA_URI, JQUERY_JS, MERMAID_JS, SAKURA_CSS,
};
use crate::adapters::cte_engine::{TERMINAL_NODE_NAME, parse_cte_graph};
use crate::domain::{
    BANNER_EMPTY_SCOPE, BlockDiff, CteGraph, EdgeType, FixtureTable, InScopeSet, Manifest,
    ModelInScopeSet, Node, NodeId, UnitTest, UnitTestDataDiff, UnitTestGiven, UnitTestYamlBlock,
    resolve_target_model, table_from_manifest_rows,
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
/// form `ref('NAME')` (case-insensitive `ref`, single quotes only —
/// matches dbt's serialized form in the manifest).
///
/// The keyword check is case-insensitive across any byte casing
/// (`ref` / `REF` / `Ref` / `rEf` / …) and tolerates whitespace between
/// the keyword and the opening parenthesis (`ref ('x')`, `REF\t('y')`,
/// etc. — Jinja's `{{ ref(...) }}` macro accepts this).
///
/// Returns `None` when the input does not match the `ref('…')` shape,
/// when the inner name is empty, or when the parentheses / quotes are
/// unbalanced. The caller (`bind_import_to_given`) treats `None` as "no
/// import-CTE match" and surfaces the design's empty-state copy.
#[must_use]
pub fn parse_ref_name(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    let prefix = trimmed.get(..3)?;
    if !prefix.eq_ignore_ascii_case("ref") {
        return None;
    }
    let after_ref = trimmed[3..].trim_start();
    let inside = after_ref.strip_prefix('(')?.strip_suffix(')')?;
    let inner = inside.trim();
    let name = inner.strip_prefix('\'')?.strip_suffix('\'')?;
    if name.is_empty() { None } else { Some(name) }
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

/// Per-model entry in the JSON payload — mirrors the design's
/// `window.CUTE_DBT_SAMPLE.models[i]` shape so the inlined interaction
/// script consumes it without remapping.
#[derive(Debug, Clone, Serialize)]
pub struct ModelPayload {
    /// Bare model name (e.g. `customer_rollup`) — the model selector
    /// label and the terminal-node id in the DAG.
    pub name: String,
    /// DAG nodes + edges, keyed for the design's JS.
    pub dag: DagPayload,
    /// Per-node compiled SQL, keyed by node id (CTE name or model name
    /// for the terminal). Empty when the CTE engine could not parse
    /// (the model card still renders the metadata + tests + an empty DAG).
    pub compiled_sql: BTreeMap<String, String>,
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
    /// Unit tests targeting this model that are in scope. Empty
    /// `[]` triggers the "0 unit tests wired" empty state.
    pub tests: Vec<TestPayload>,
    /// `true` when the model's compiled SQL was a `WITH RECURSIVE`
    /// query — the template surfaces a banner; the recursive arm has
    /// already been omitted from the graph by the CTE engine.
    pub is_recursive: bool,
}

/// One DAG node — id and role.
#[derive(Debug, Clone, Serialize)]
pub struct NodePayload {
    /// Stable node id used by both the DAG (Mermaid `g.node[id]`) and
    /// the `compiled_sql` map. For non-terminal nodes this is the CTE
    /// name; for the terminal node this is the model's bare name.
    pub id: String,
    /// Render-layer classification (see [`NodeRole`]).
    pub role: NodeRole,
}

/// One DAG edge — `from` and `to` are the [`NodePayload::id`] strings.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct DagPayload {
    /// CTE nodes plus the terminal model node, in stable rendering
    /// order.
    pub nodes: Vec<NodePayload>,
    /// Directed edges between [`Self::nodes`] entries, classified by
    /// [`EdgePayload::edge_type`].
    pub edges: Vec<EdgePayload>,
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
    jquery_js: &'a str,
    datatables_js: &'a str,
    mermaid_js: &'a str,
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
    /// JSON payload, pre-escaped for safe interpolation inside
    /// `<script type="application/json">` via [`payload_json_for_html_script`].
    /// The template emits this with `|safe`; the safety property is the
    /// Rust-side escape, not askama's HTML filter.
    payload_json: &'a str,
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
/// `/` or `!` — the only sequences that matter under HTML5's
/// script-data state machine. The `\uXXXX` form is a documented JSON
/// escape (RFC 8259 §7) so the output remains a valid JSON document
/// that `JSON.parse(...)` decodes back to the original characters.
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
        if c == '<' && matches!(chars.peek(), Some('/' | '!')) {
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
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    baseline_label: &str,
) -> ReportPayload {
    let model_tests = index_tests_for_models(current, models_in_scope);
    let empty: Vec<(&str, &UnitTest)> = Vec::new();
    let mut models = Vec::new();
    for model_id in models_in_scope.iter() {
        let Some(model) = current.node(model_id) else {
            continue;
        };
        let tests = model_tests.get(model_id).unwrap_or(&empty).as_slice();
        models.push(build_model_payload(
            model,
            tests,
            changed,
            authoring_yaml,
            yaml_diffs,
            sql_diffs,
            data_diffs,
        ));
    }
    ReportPayload {
        baseline: baseline_label.to_owned(),
        models,
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
    data_diffs: &HashMap<String, UnitTestDataDiff>,
    baseline_label: &str,
    scope_source: ScopeSource,
    report_title: &str,
    report_subtitle: Option<&str>,
) -> io::Result<()> {
    let payload = build_payload(
        current,
        changed,
        models_in_scope,
        authoring_yaml,
        yaml_diffs,
        sql_diffs,
        data_diffs,
        baseline_label,
    );
    // The empty-scope banner contract reads the TRUE in-scope set, not the
    // widened render set or the changed subset (cute-dbt#91).
    let banner_text = compose_banner_text(in_scope);
    let payload_json = payload_json_for_html_script(&payload)
        .map_err(|err| io::Error::other(format!("payload serialization: {err}")))?;
    let template = ReportTemplate {
        sakura_css: SAKURA_CSS,
        datatables_css: DATATABLES_CSS,
        jquery_js: JQUERY_JS,
        datatables_js: DATATABLES_JS,
        mermaid_js: MERMAID_JS,
        favicon_data_uri: FAVICON_DATA_URI,
        report_title,
        report_subtitle,
        banner_text: &banner_text,
        baseline_label,
        is_pr_diff: scope_source == ScopeSource::PrDiff,
        payload_json: &payload_json,
    };
    let html = template
        .render()
        .map_err(|err| io::Error::other(format!("render: {err}")))?;
    fs::write(out, html)
}

/// Build a [`ModelPayload`] for one in-scope model.
fn build_model_payload(
    model: &Node,
    tests: &[(&str, &UnitTest)],
    changed: &InScopeSet,
    authoring_yaml: &HashMap<String, UnitTestYamlBlock>,
    yaml_diffs: &HashMap<String, BlockDiff>,
    sql_diffs: &HashMap<String, BlockDiff>,
    data_diffs: &HashMap<String, UnitTestDataDiff>,
) -> ModelPayload {
    let bare_name = leaf_segment(model.id().as_str()).to_owned();
    let compiled_code = model.compiled_code().unwrap_or_default();
    let graph = parse_cte_graph(compiled_code).unwrap_or_default();
    let is_recursive = graph.is_recursive();
    let nodes = build_node_payloads(&graph, &bare_name);
    let edges = build_edge_payloads(&graph, &bare_name);
    let compiled_sql = build_compiled_sql(&graph, &bare_name, compiled_code);
    let raw_sql = model
        .raw_code()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    // SQL diff is keyed by the model's FULL node id (the
    // `reconstruct_model_sql_diffs` key), not the bare name.
    let sql_diff = sql_diffs.get(model.id().as_str()).cloned();
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
            )
        })
        .collect();
    ModelPayload {
        name: bare_name,
        dag: DagPayload { nodes, edges },
        compiled_sql,
        raw_sql,
        sql_diff,
        tests: test_payloads,
        is_recursive,
    }
}

/// Build [`NodePayload`]s for every graph node, mapping the terminal
/// node's name to the model's bare name.
fn build_node_payloads(graph: &CteGraph, model_name: &str) -> Vec<NodePayload> {
    graph
        .nodes()
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            let role = classify_node_role(graph, idx);
            let id = if role == NodeRole::Final {
                model_name.to_owned()
            } else {
                node.name().to_owned()
            };
            NodePayload { id, role }
        })
        .collect()
}

/// Build [`EdgePayload`]s, swapping the terminal node's index id for the
/// model's bare name on both endpoints.
fn build_edge_payloads(graph: &CteGraph, model_name: &str) -> Vec<EdgePayload> {
    graph
        .edges()
        .iter()
        .map(|edge| {
            let from = endpoint_id(graph, edge.from(), model_name);
            let to = endpoint_id(graph, edge.to(), model_name);
            EdgePayload {
                from,
                to,
                edge_type: edge.edge_type(),
            }
        })
        .collect()
}

/// Resolve a graph-node index to its rendered id (CTE name, or model
/// name for the terminal node).
fn endpoint_id(graph: &CteGraph, index: usize, model_name: &str) -> String {
    let Some(node) = graph.nodes().get(index) else {
        return String::new();
    };
    if node.name() == TERMINAL_NODE_NAME {
        model_name.to_owned()
    } else {
        node.name().to_owned()
    }
}

/// Build the `compiled_sql` map: per-CTE `raw_sql` keyed by node id,
/// plus the terminal node keyed by the model's bare name.
///
/// Falls back to the model's full compiled code on the terminal node
/// when the engine emitted no per-CTE body (empty graph from a model
/// with no `WITH` clause).
fn build_compiled_sql(
    graph: &CteGraph,
    model_name: &str,
    full_compiled_code: &str,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if graph.is_empty() {
        map.insert(model_name.to_owned(), full_compiled_code.to_owned());
        return map;
    }
    for node in graph.nodes() {
        let id = if node.name() == TERMINAL_NODE_NAME {
            model_name.to_owned()
        } else {
            node.name().to_owned()
        };
        if let Some(sql) = node.raw_sql() {
            map.insert(id, sql.to_owned());
        }
    }
    map
}

/// Build a single test's payload, including import-CTE binding for each
/// given. `changed` is the set of updated test ids — `id`'s membership
/// sets [`TestPayload::changed`] (cute-dbt#91).
fn build_test_payload(
    id: &str,
    unit_test: &UnitTest,
    graph: &CteGraph,
    changed: &InScopeSet,
    authoring_yaml: Option<&UnitTestYamlBlock>,
    yaml_diff: Option<&BlockDiff>,
    data_diff: Option<&UnitTestDataDiff>,
) -> TestPayload {
    let given = unit_test
        .given()
        .iter()
        .map(|g| {
            let bound_to_node =
                parse_ref_name(g.input()).and_then(|ref_name| find_import_node_id(graph, ref_name));
            GivenPayload {
                input: g.input().to_owned(),
                bound_to_node,
                table: current_view_table(g.rows(), g.format(), g.fixture()),
                rows: g.rows().clone(),
                format: g.format().map(str::to_owned),
                fixture: g.fixture().map(str::to_owned),
            }
        })
        .collect();
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
        given,
        expected: ExpectedPayload {
            table: current_view_table(
                unit_test.expect().rows(),
                unit_test.expect().format(),
                unit_test.expect().fixture(),
            ),
            rows: unit_test.expect().rows().clone(),
            format: unit_test.expect().format().map(str::to_owned),
            fixture: unit_test.expect().fixture().map(str::to_owned),
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
/// 2. **sql / opaque** — [`table_from_manifest_rows`] returns `None` (a raw
///    `SELECT` string has no cells); the JS renders the sql code block.
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
/// Resolved via [`resolve_target_model`] (the bare `model:` name → full
/// node id mapping). Unlike the prior in-scope-only indexer, this
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
        let Some(model) = resolve_target_model(current, unit_test.model()) else {
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
        Checksum, CteEdge, CteNode, DEFAULT_REPORT_TITLE, DependsOn, DiffLine, DiffLineKind,
        EdgeType, Manifest, ManifestMetadata, NodeConfig, NodeId, UnitTest, UnitTestExpect,
        UnitTestGiven,
    };
    use serde_json::json;
    use std::collections::{BTreeMap, HashMap};

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
    }

    #[test]
    fn parse_ref_name_returns_none_on_double_quoted_name() {
        // dbt manifests serialize ref(...) with single quotes; the renderer
        // is deliberately strict here so a stray "ref" mention in user text
        // does not silently produce a binding.
        assert_eq!(parse_ref_name("ref(\"x\")"), None);
    }

    #[test]
    fn parse_ref_name_returns_none_on_unmatched_quotes() {
        assert_eq!(parse_ref_name("ref('x"), None);
        assert_eq!(parse_ref_name("ref(x')"), None);
    }

    #[test]
    fn parse_ref_name_returns_none_on_non_ref_input() {
        assert_eq!(parse_ref_name("source('a', 'b')"), None);
        assert_eq!(parse_ref_name(""), None);
        assert_eq!(parse_ref_name("plain_table"), None);
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
            "b",
        );
        // No models in scope → no payload entries; the indexer's None
        // branch simply doesn't contribute.
        assert!(payload.models.is_empty());
    }

    #[test]
    fn build_payload_terminal_node_renders_with_model_bare_name() {
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
            "b",
        );
        let model = &payload.models[0];
        let terminal = model
            .dag
            .nodes
            .iter()
            .find(|n| n.role == NodeRole::Final)
            .expect("terminal node present");
        assert_eq!(terminal.id, "final_one");
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
            "b",
        );
        let model = &payload.models[0];
        assert!(
            model.compiled_sql.contains_key("stg_x_src"),
            "import node compiled SQL keyed by CTE name: {:?}",
            model.compiled_sql.keys().collect::<Vec<_>>(),
        );
        assert!(
            model.compiled_sql.contains_key("x"),
            "terminal node compiled SQL keyed by model bare name",
        );
    }

    #[test]
    fn build_payload_empty_graph_falls_back_to_full_compiled_code() {
        // A `select 1` body has no WITH clause → empty CteGraph. The
        // compiled_sql map still carries the model's body keyed by the
        // bare name so the renderer surfaces SOMETHING.
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
            "b",
        );
        assert_eq!(
            payload.models[0].compiled_sql.get("flat").unwrap(),
            compiled
        );
    }

    #[test]
    fn build_payload_handles_unparseable_compiled_code_gracefully() {
        // The engine returns CteError::Parse for garbage SQL. The renderer
        // treats that as an empty graph, NOT a hard failure — the report
        // still ships, the model card just has no DAG.
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
            "b",
        );
        assert_eq!(payload.models.len(), 1);
        // Empty graph → empty nodes/edges, but compiled_sql carries the
        // original body keyed by bare name.
        assert!(payload.models[0].dag.nodes.is_empty());
        assert_eq!(
            payload.models[0].compiled_sql.get("broken").unwrap(),
            "not valid sql {",
        );
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
            "b",
        );
        let test = &payload.models[0].tests[0];
        assert!(test.given[0].bound_to_node.is_none());
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
            "b",
        );
        assert!(payload.models[0].is_recursive);
    }

    // ===== payload_json_for_html_script =====

    #[test]
    fn payload_json_escapes_closing_script_tag_via_unicode() {
        let payload = ReportPayload {
            baseline: "</script><script>alert(1)</script>".to_owned(),
            models: vec![],
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
        };
        let serialized = payload_json_for_html_script(&payload).unwrap();
        assert!(serialized.contains("a < b"), "bare `<` is preserved");
    }

    #[test]
    fn payload_json_output_is_round_trippable_through_json_parse() {
        // The Unicode escape must remain valid JSON; serde_json round-trips
        // it back to the original string.
        let original = ReportPayload {
            baseline: "</script><!--end".to_owned(),
            models: vec![],
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
        // stripping the five inlined asset bodies so we don't
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
        let payload =
            build_test_payload("unit_test.shop.t", &ut, &graph, &changed, None, None, None);
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
        );
        assert_eq!(payload.data_diff.as_ref(), Some(&data_diff));
        let json = serde_json::to_string(&payload).expect("payload serializes");
        assert!(
            json.contains("data_diff"),
            "present data_diff is on the wire"
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
}
