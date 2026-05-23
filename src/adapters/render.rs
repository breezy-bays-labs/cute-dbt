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
//! - **Clean-import-CTE binding.** Parses `ref('NAME')` out of each
//!   unit test's `given[].input`, then locates the matching import-CTE
//!   node in two passes (case-insensitive): first by CTE name (the
//!   convention where the unwrapper CTE inherits the upstream model's
//!   name), then by the leaf table reference inside the CTE's body
//!   (dbt's compiled-SQL idiom: `with source as (select * from
//!   "db"."schema"."MODEL")`). Unmatched givens surface "no fixture
//!   provided — dbt treats unspecified inputs as empty" in the template.
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
    BANNER_EMPTY_SCOPE, CteGraph, EdgeType, InScopeSet, Manifest, ModelInScopeSet, Node, NodeId,
    UnitTest, UnitTestGiven, resolve_target_model,
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
/// Returns `None` when the input does not match the `ref('…')` shape,
/// when the inner name is empty, or when the parentheses / quotes are
/// unbalanced. The caller (`bind_import_to_given`) treats `None` as "no
/// import-CTE match" and surfaces the design's empty-state copy.
#[must_use]
pub fn parse_ref_name(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix("ref(").or_else(|| {
        trimmed
            .strip_prefix("REF(")
            .or_else(|| trimmed.strip_prefix("Ref("))
    })?;
    let rest = rest.strip_suffix(')')?;
    let inner = rest.trim();
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
/// `Final`. A CTE with zero incoming edges and a body that consists of a
/// plain `SELECT … FROM <single relation>` is `Import`. Everything else
/// is `Transform`.
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
    if node.raw_sql().is_some_and(is_simple_from_select) {
        NodeRole::Import
    } else {
        NodeRole::Transform
    }
}

/// `true` when `sql` is a single `SELECT … FROM <relation>` with no joins
/// and no further FROM clauses — the import-CTE shape.
///
/// Whitespace-and-comment-tolerant heuristic: lower-cases the body,
/// requires a `select` keyword, exactly one `from` keyword, and no `join`
/// keyword. Stricter classification would require a full AST walk; the
/// CTE engine already parsed once and the renderer would re-parse to
/// re-classify — overkill for the visual taxonomy this drives.
fn is_simple_from_select(sql: &str) -> bool {
    let lower = sql.to_ascii_lowercase();
    if !lower.contains("select") {
        return false;
    }
    if lower.contains(" join ") || lower.contains("\njoin ") {
        return false;
    }
    let from_count = lower.matches(" from ").count() + lower.matches("\nfrom ").count();
    from_count == 1
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
    /// Unit tests targeting this model that are in scope. Empty
    /// `[]` triggers the "0 unit tests wired" empty state.
    pub tests: Vec<TestPayload>,
    /// `true` when the model's compiled SQL was a `WITH RECURSIVE`
    /// query — the template surfaces a banner; the recursive arm has
    /// already been omitted from the graph by the CTE engine.
    pub is_recursive: bool,
}

/// One DAG node — id, role, and (for import nodes) the source ref name.
#[derive(Debug, Clone, Serialize)]
pub struct NodePayload {
    /// Stable node id used by both the DAG (Mermaid `g.node[id]`) and
    /// the `compiled_sql` map. For non-terminal nodes this is the CTE
    /// name; for the terminal node this is the model's bare name.
    pub id: String,
    /// Render-layer classification (see [`NodeRole`]).
    pub role: NodeRole,
    /// For import nodes, the upstream `ref('…')` name (carried so the
    /// node-detail panel can label the Given table). `None` for
    /// transform and final nodes.
    #[serde(skip_serializing_if = "Option::is_none", rename = "ref")]
    pub ref_name: Option<String>,
}

/// One DAG edge — `from` and `to` are the [`NodePayload::id`] strings.
#[derive(Debug, Clone, Serialize)]
pub struct EdgePayload {
    pub from: String,
    pub to: String,
    pub edge_type: EdgeType,
}

/// The full DAG carried in a [`ModelPayload`].
#[derive(Debug, Clone, Serialize)]
pub struct DagPayload {
    pub nodes: Vec<NodePayload>,
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
    pub input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_to_node: Option<String>,
    pub rows: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defined_in: Option<String>,
    pub given: Vec<GivenPayload>,
    pub expected: ExpectedPayload,
}

/// `expect` block lifted into payload shape.
#[derive(Debug, Clone, Serialize)]
pub struct ExpectedPayload {
    pub rows: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
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

/// askama template binding for the v0.1 report.
///
/// Every field except `payload_json` is an inlined asset constant; the
/// template interpolates them with `| safe` and the payload with
/// `| json | safe`. Asset values are pinned `&'static str` constants
/// from [`asset_embed`](crate::adapters::asset_embed) so the renderer
/// cannot drift from the vendored bundle.
#[derive(Template)]
#[template(path = "report.html", escape = "html")]
struct ReportTemplate<'a> {
    sakura_css: &'a str,
    datatables_css: &'a str,
    jquery_js: &'a str,
    datatables_js: &'a str,
    mermaid_js: &'a str,
    favicon_data_uri: &'a str,
    /// Server-rendered banner text — a single contiguous string the
    /// `report_generation.feature` and `tests/run_loop.rs` assertions
    /// can grep against without the static HTML's span boundaries
    /// breaking the substring. JS may rewrite the `.diff-scope-text`
    /// element at boot, but the static fallback is the contract.
    banner_text: &'a str,
    payload: &'a ReportPayload,
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
/// `BTreeSet` traversal). A model not present in `current.nodes()` is
/// silently skipped — the comparator should not have surfaced it, but
/// belt-and-braces.
#[must_use]
pub fn build_payload(
    current: &Manifest,
    in_scope: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    baseline_label: &str,
) -> ReportPayload {
    let model_tests = index_in_scope_tests_by_model(current, in_scope);
    let empty: Vec<(&str, &UnitTest)> = Vec::new();
    let mut models = Vec::new();
    for model_id in models_in_scope.iter() {
        let Some(model) = current.node(model_id) else {
            continue;
        };
        let tests = model_tests.get(model_id).unwrap_or(&empty).as_slice();
        models.push(build_model_payload(model, tests));
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
pub fn render_report(
    out: &Path,
    current: &Manifest,
    in_scope: &InScopeSet,
    models_in_scope: &ModelInScopeSet,
    baseline_label: &str,
) -> io::Result<()> {
    let payload = build_payload(current, in_scope, models_in_scope, baseline_label);
    let banner_text = compose_banner_text(in_scope);
    let template = ReportTemplate {
        sakura_css: SAKURA_CSS,
        datatables_css: DATATABLES_CSS,
        jquery_js: JQUERY_JS,
        datatables_js: DATATABLES_JS,
        mermaid_js: MERMAID_JS,
        favicon_data_uri: FAVICON_DATA_URI,
        banner_text: &banner_text,
        payload: &payload,
    };
    let html = template
        .render()
        .map_err(|err| io::Error::other(format!("render: {err}")))?;
    fs::write(out, html)
}

/// Build a [`ModelPayload`] for one in-scope model.
fn build_model_payload(model: &Node, tests: &[(&str, &UnitTest)]) -> ModelPayload {
    let bare_name = leaf_segment(model.id().as_str()).to_owned();
    let compiled_code = model.compiled_code().unwrap_or_default();
    let graph = parse_cte_graph(compiled_code).unwrap_or_default();
    let is_recursive = graph.is_recursive();
    let nodes = build_node_payloads(&graph, &bare_name);
    let edges = build_edge_payloads(&graph, &bare_name);
    let compiled_sql = build_compiled_sql(&graph, &bare_name, compiled_code);
    let test_payloads = tests
        .iter()
        .map(|(id, ut)| build_test_payload(id, ut, &graph, &bare_name))
        .collect();
    ModelPayload {
        name: bare_name,
        dag: DagPayload { nodes, edges },
        compiled_sql,
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
            let ref_name = if role == NodeRole::Import {
                Some(node.name().to_owned())
            } else {
                None
            };
            NodePayload { id, role, ref_name }
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
/// given.
fn build_test_payload(
    id: &str,
    unit_test: &UnitTest,
    graph: &CteGraph,
    model_name: &str,
) -> TestPayload {
    let given = unit_test
        .given()
        .iter()
        .map(|g| {
            let bound_to_node = parse_ref_name(g.input())
                .and_then(|ref_name| find_import_node_id(graph, ref_name, model_name));
            GivenPayload {
                input: g.input().to_owned(),
                bound_to_node,
                rows: g.rows().clone(),
                format: g.format().map(str::to_owned),
            }
        })
        .collect();
    TestPayload {
        id: id.to_owned(),
        name: unit_test.name().to_owned(),
        target_model: unit_test.model().as_str().to_owned(),
        description: unit_test.description().map(str::to_owned),
        tags: unit_test.tags().map(<[String]>::to_vec),
        meta: unit_test.meta().cloned(),
        defined_in: unit_test.original_file_path().map(str::to_owned),
        given,
        expected: ExpectedPayload {
            rows: unit_test.expect().rows().clone(),
            format: unit_test.expect().format().map(str::to_owned),
        },
    }
}

/// Locate the import-CTE node that binds to `ref_name`.
///
/// Two-pass match — both case-insensitive:
///
/// 1. **Name match** (the design's sample-data convention): an
///    import-CTE whose own name equals `ref_name`.
/// 2. **Body match** (dbt's idiomatic compiled-SQL shape): an
///    import-CTE whose body unwraps an external table whose leaf
///    identifier equals `ref_name`. dbt-compiled SQL commonly carries
///    `with source as (select * from "db"."schema"."MODEL")`, where the
///    CTE name is the unwrapper convention (`source`, `src_*`, etc.)
///    and the model name lives only inside the body. Pass 1 misses
///    that shape; pass 2 catches it via [`extract_table_leaf_refs`].
///
/// Returns the import-CTE's name (the payload's stable node id), or
/// `None` when neither pass matches.
fn find_import_node_id(graph: &CteGraph, ref_name: &str, _model_name: &str) -> Option<String> {
    let target = ref_name.to_ascii_lowercase();
    // Pass 1: name match (design's convention).
    if let Some((_, node)) = graph.nodes().iter().enumerate().find(|(idx, node)| {
        node.name().eq_ignore_ascii_case(&target)
            && classify_node_role(graph, *idx) == NodeRole::Import
    }) {
        return Some(node.name().to_owned());
    }
    // Pass 2: body match (dbt's compiled-SQL shape).
    for (idx, node) in graph.nodes().iter().enumerate() {
        if classify_node_role(graph, idx) != NodeRole::Import {
            continue;
        }
        let Some(sql) = node.raw_sql() else {
            continue;
        };
        if extract_table_leaf_refs(sql).iter().any(|t| t == &target) {
            return Some(node.name().to_owned());
        }
    }
    None
}

/// Extract leaf table identifiers from any `FROM` or `JOIN` clause in
/// `sql`, lowercased.
///
/// Whitespace-tokenizing heuristic: walks the body, looks for a `from`
/// or `join` keyword, and takes the next token's trailing identifier
/// (stripping schema/database prefixes and surrounding quotes). Used by
/// [`find_import_node_id`]'s pass-2 body match. Not a SQL parser —
/// false positives are constrained by the surrounding [`NodeRole`]
/// filter (only import-CTE nodes are searched), and the typical dbt
/// `source as (select * from "db"."schema"."MODEL")` shape resolves to
/// the right leaf cleanly.
///
/// Returned identifiers are lowercase so the caller can compare
/// case-insensitively without re-folding.
fn extract_table_leaf_refs(sql: &str) -> Vec<String> {
    let tokens: Vec<&str> = sql.split_whitespace().collect();
    let mut out = Vec::new();
    for (i, tok) in tokens.iter().enumerate() {
        let lower = tok.to_ascii_lowercase();
        if lower != "from" && lower != "join" {
            continue;
        }
        let Some(next) = tokens.get(i + 1) else {
            continue;
        };
        // Strip surrounding `(` and trailing `,`/`)`/`;`/`(`; take the
        // last `.`-delimited segment; strip `"` quotes.
        let cleaned = next
            .trim_start_matches('(')
            .trim_end_matches([',', ')', ';']);
        let leaf = cleaned.rsplit('.').next().unwrap_or("");
        let leaf = leaf.trim_matches('"');
        if leaf.is_empty() {
            continue;
        }
        if !leaf.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        out.push(leaf.to_ascii_lowercase());
    }
    out
}

/// Build a map from in-scope model id to the unit tests targeting it.
///
/// Resolved via [`resolve_target_model`] (the bare `model:` name →
/// full node id mapping). Order within each list is `InScopeSet`
/// iteration order, which is `BTreeSet` over the unit-test id —
/// deterministic for the golden snapshot.
fn index_in_scope_tests_by_model<'m>(
    current: &'m Manifest,
    in_scope: &InScopeSet,
) -> HashMap<NodeId, Vec<(&'m str, &'m UnitTest)>> {
    let mut map: HashMap<NodeId, Vec<(&'m str, &'m UnitTest)>> = HashMap::new();
    for test_id in in_scope.iter() {
        let Some((id_owned, unit_test)) = current.unit_tests().get_key_value(test_id) else {
            continue;
        };
        let Some(model) = resolve_target_model(current, unit_test.model()) else {
            continue;
        };
        map.entry(model.id().clone())
            .or_default()
            .push((id_owned.as_str(), unit_test));
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
        Checksum, CteEdge, CteNode, DependsOn, EdgeType, Manifest, ManifestMetadata, NodeId,
        UnitTest, UnitTestExpect, UnitTestGiven,
    };
    use serde_json::json;
    use std::collections::HashMap;

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
        UnitTestGiven::new(input, json!([]), None)
    }

    fn unit_test_with_givens(givens: Vec<UnitTestGiven>) -> UnitTest {
        UnitTest::new(
            "t",
            NodeId::new("model.shop.x"),
            givens,
            UnitTestExpect::new(json!([]), None),
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

    fn cte_node(name: &str, raw_sql: Option<&str>) -> CteNode {
        CteNode::new(name, None, raw_sql.map(str::to_owned), None)
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
            vec![cte_node("stg_orders", Some("select id from raw.orders"))],
            vec![],
        );
        assert_eq!(classify_node_role(&graph, 0), NodeRole::Import);
    }

    #[test]
    fn classify_node_with_incoming_edges_as_transform() {
        let graph = CteGraph::new(
            vec![
                cte_node("a", Some("select 1 from x")),
                cte_node("b", Some("select * from a")),
            ],
            vec![CteEdge::new(0, 1, EdgeType::From)],
        );
        assert_eq!(classify_node_role(&graph, 1), NodeRole::Transform);
    }

    #[test]
    fn classify_node_with_join_as_transform() {
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
    fn classify_no_raw_sql_with_no_incoming_is_transform() {
        // A node with no raw_sql cannot be classified as `Import` (the
        // import shape needs to inspect the body). Fall back to `Transform`.
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
        Node::new(
            NodeId::new(id),
            "model",
            checksum(body),
            compiled.map(str::to_owned),
            DependsOn::default(),
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
            )],
            UnitTestExpect::new(json!([]), None),
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
        let payload = build_payload(&manifest, &in_scope, &models, "baseline.json");
        assert_eq!(payload.models.len(), 1);
        let model = &payload.models[0];
        assert_eq!(model.name, "stg_orders");
        assert_eq!(model.tests.len(), 1);
        assert_eq!(model.tests[0].name, "test_one");
    }

    #[test]
    fn build_payload_emits_empty_tests_for_modified_model_with_no_unit_tests() {
        let compiled = "select 1";
        let node = model_node("model.shop.no_test", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let in_scope = InScopeSet::new();
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.no_test")]);
        let payload = build_payload(&manifest, &in_scope, &models, "baseline.json");
        assert_eq!(payload.models.len(), 1);
        assert!(payload.models[0].tests.is_empty());
    }

    #[test]
    fn build_payload_skips_a_model_missing_from_manifest_nodes() {
        let manifest = manifest_for(vec![], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.ghost")]);
        let payload = build_payload(&manifest, &InScopeSet::new(), &models, "b");
        assert!(payload.models.is_empty());
    }

    #[test]
    fn build_payload_terminal_node_renders_with_model_bare_name() {
        let compiled = "with src as (select * from raw_x) select * from src";
        let node = model_node("model.shop.final_one", "body", Some(compiled));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.final_one")]);
        let payload = build_payload(&manifest, &InScopeSet::new(), &models, "b");
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
        let payload = build_payload(&manifest, &InScopeSet::new(), &models, "b");
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
        let payload = build_payload(&manifest, &InScopeSet::new(), &models, "b");
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
        let payload = build_payload(&manifest, &InScopeSet::new(), &models, "b");
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
        let payload = build_payload(&manifest, &in_scope, &models, "b");
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
            vec![UnitTestGiven::new("ref('stg_orders_src')", json!([]), None)],
            UnitTestExpect::new(json!([]), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_orders")]);
        let payload = build_payload(&manifest, &in_scope, &models, "b");
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
            vec![UnitTestGiven::new("ref('raw_customers')", json!([]), None)],
            UnitTestExpect::new(json!([]), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.stg_customers")]);
        let payload = build_payload(&manifest, &in_scope, &models, "b");
        let test = &payload.models[0].tests[0];
        assert_eq!(
            test.given[0].bound_to_node.as_deref(),
            Some("source"),
            "ref('raw_customers') binds to the import-CTE `source` via its body table reference",
        );
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
            vec![UnitTestGiven::new("ref('target')", json!([]), None)],
            UnitTestExpect::new(json!([]), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let payload = build_payload(&manifest, &in_scope, &models, "b");
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
            vec![UnitTestGiven::new("ref('nonexistent')", json!([]), None)],
            UnitTestExpect::new(json!([]), None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        );
        let manifest = manifest_for(vec![node], vec![("unit_test.shop.test_one", ut)]);
        let in_scope = InScopeSet::from_iter(["unit_test.shop.test_one".to_owned()]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.flat")]);
        let payload = build_payload(&manifest, &in_scope, &models, "b");
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
        let payload = build_payload(&manifest, &InScopeSet::new(), &models, "b");
        assert!(payload.models[0].is_recursive);
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
            "baseline.json",
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
        render_report(&tmp, &manifest, &InScopeSet::new(), &models, "lab1@aaaaaaa")
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
        // Parallel to tests/asset_embed.rs::the_smoke_report_emits_no_external_resource_constructs.
        // Strip the inlined asset bodies before scanning so we measure the
        // CHROME, not the bundled URL literals inside the assets.
        let node = model_node("model.shop.x", "body", Some("select 1"));
        let manifest = manifest_for(vec![node], vec![]);
        let models = ModelInScopeSet::from_iter([NodeId::new("model.shop.x")]);
        let tmp = std::env::temp_dir().join("cute_dbt_render_egress_test.html");
        let _ = std::fs::remove_file(&tmp);
        render_report(&tmp, &manifest, &InScopeSet::new(), &models, "b").expect("render writes");
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
        assert!(!chrome.contains(" src=\""), "no src= attributes in chrome");
        assert!(!chrome.contains("@import"), "no CSS @import");
        assert!(!chrome.contains("http://"), "no http URL");
        assert!(!chrome.contains("https://"), "no https URL");
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
            UnitTestExpect::new(json!([]), None),
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
        render_report(&tmp, &manifest, &in_scope, &models, "b").expect("render writes the report");
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

    // ===== extract_table_leaf_refs =====

    #[test]
    fn extract_table_leaf_refs_strips_schema_and_quote_qualifiers() {
        let sql = "select * from \"jaffle_shop\".\"main\".\"raw_customers\"";
        assert_eq!(extract_table_leaf_refs(sql), vec!["raw_customers"]);
    }

    #[test]
    fn extract_table_leaf_refs_lowercases_and_handles_unquoted_idents() {
        let sql = "SELECT * FROM RAW_CUSTOMERS";
        assert_eq!(extract_table_leaf_refs(sql), vec!["raw_customers"]);
    }

    #[test]
    fn extract_table_leaf_refs_picks_up_join_clauses() {
        let sql = "select * from \"a\".\"b\".\"orders\" join customers on c.id = o.cid";
        let refs = extract_table_leaf_refs(sql);
        assert!(refs.iter().any(|r| r == "orders"));
        assert!(refs.iter().any(|r| r == "customers"));
    }

    #[test]
    fn extract_table_leaf_refs_ignores_non_from_tokens() {
        let sql = "from x where from_col = 1";
        assert_eq!(extract_table_leaf_refs(sql), vec!["x"]);
    }

    #[test]
    fn extract_table_leaf_refs_drops_punctuation_around_idents() {
        let sql = "select * from (raw_customers)";
        let refs = extract_table_leaf_refs(sql);
        assert!(refs.iter().any(|r| r == "raw_customers"), "{refs:?}");
    }

    // ===== is_simple_from_select =====

    #[test]
    fn is_simple_from_select_accepts_a_plain_from() {
        assert!(is_simple_from_select("select id, name\nfrom raw.customers"));
    }

    #[test]
    fn is_simple_from_select_rejects_a_join() {
        assert!(!is_simple_from_select(
            "select * from a join b on a.k = b.k",
        ));
    }

    #[test]
    fn is_simple_from_select_rejects_multiple_from_clauses() {
        // A subquery introduces a second `from` keyword — not a "simple"
        // import-style body.
        assert!(!is_simple_from_select(
            "select * from (select id from raw.x) from raw.y",
        ));
    }

    #[test]
    fn is_simple_from_select_rejects_an_empty_or_select_only_body() {
        assert!(!is_simple_from_select(""));
        assert!(!is_simple_from_select("select 1"));
    }

    // ===== leaf_segment =====

    #[test]
    fn leaf_segment_strips_qualifying_prefix() {
        assert_eq!(leaf_segment("model.shop.x"), "x");
        assert_eq!(leaf_segment("x"), "x");
        assert_eq!(leaf_segment(""), "");
    }
}
