//! The `cute-dbt explore` two-page renderer (cute-dbt#100, cute-dbt#101).
//!
//! Emits the full-manifest explorer into `--out-dir`:
//!
//! - **`dag.html`** — the **interactive** model-lineage DAG
//!   (cute-dbt#101): every `model` node, edges from `depends_on.nodes`,
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
    CYTOSCAPE_DAGRE_JS, CYTOSCAPE_JS, EXPLORE_CTE_JS, EXPLORE_LINEAGE_JS, EXPLORE_TESTS_JS,
    FAVICON_DATA_URI, SAKURA_CSS,
};
use crate::adapters::render::{DagPayload, ReportPayload};
use crate::domain::{Manifest, ModelInScopeSet, NodeId, resolve_target_model};

/// One model node in the lineage graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageNode {
    /// Full manifest node id (`model.<package>.<name>`).
    pub id: String,
    /// Bare model name (the last dotted segment) — the rendered label.
    pub name: String,
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
    /// `unit_tests` entries whose bare `model:` reference resolves here
    /// ([`resolve_target_model`], the same bridge the report uses).
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
/// `unit_tests` entry stores the BARE model name, bridged to its node
/// by [`resolve_target_model`] (the report renderer's exact resolution
/// — the two surfaces cannot disagree on a test's target). An
/// unresolvable `model:` reference contributes nothing (skipped, not
/// failed — the explore fail-open posture).
fn unit_test_counts(current: &Manifest) -> HashMap<NodeId, usize> {
    let mut counts: HashMap<NodeId, usize> = HashMap::new();
    for unit_test in current.unit_tests().values() {
        if let Some(model) = resolve_target_model(current, unit_test.model()) {
            *counts.entry(model.id().clone()).or_insert(0) += 1;
        }
    }
    counts
}

/// The full-manifest model lineage: nodes in deterministic node-id
/// order, edges as `(from_index, to_index)` pairs pointing **upstream →
/// downstream** (a model depends on its `from`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lineage {
    /// Every `model` node, ordered by node id.
    pub nodes: Vec<LineageNode>,
    /// Dependency edges between models in `nodes` (indices), ordered.
    pub edges: Vec<(usize, usize)>,
}

/// Build the model-lineage graph for the explore scope.
///
/// Nodes are exactly the `models` set (the
/// [`all_models`](crate::domain::all_models) seam), in its deterministic
/// `BTreeSet` order. Edges come from each model's `depends_on.nodes`,
/// filtered to ids inside the same set — sources, seeds, macros and
/// cross-project refs outside the model set are silently skipped (they
/// are not model-lineage edges in v0.x). Self-edges are skipped
/// defensively (a manifest should never carry one).
#[must_use]
pub fn build_lineage(current: &Manifest, models: &ModelInScopeSet) -> Lineage {
    let index_of: HashMap<&NodeId, usize> =
        models.iter().enumerate().map(|(i, id)| (id, i)).collect();
    let data_tests = data_test_counts(current);
    let unit_tests = unit_test_counts(current);
    let nodes: Vec<LineageNode> = models
        .iter()
        .map(|id| {
            let node = current.node(id);
            LineageNode {
                id: id.as_str().to_owned(),
                name: leaf_segment(id.as_str()).to_owned(),
                not_compiled: node.is_none_or(|n| n.compiled_code().is_none()),
                data_tests: data_tests.get(id).copied().unwrap_or(0),
                unit_tests: unit_tests.get(id).copied().unwrap_or(0),
            }
        })
        .collect();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (to_idx, id) in models.iter().enumerate() {
        let Some(node) = current.node(id) else {
            continue;
        };
        for dep in node.depends_on().nodes() {
            if let Some(&from_idx) = index_of.get(dep) {
                if from_idx != to_idx {
                    edges.push((from_idx, to_idx));
                }
            }
        }
    }
    edges.sort_unstable();
    edges.dedup();
    Lineage { nodes, edges }
}

/// One node in the serialized lineage payload (the `explore-dag-data`
/// JSON carrier consumed by `templates/explore-lineage.js`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineageNodePayload {
    /// Full manifest node id (`model.<package>.<name>`) — the Cytoscape
    /// element id and the value the Space focus commit writes to
    /// `document.body.dataset.selectedModel`.
    pub id: String,
    /// Bare model name — the canvas-text label and the fuzzy-search
    /// candidate.
    pub name: String,
    /// The fail-open "not compiled" flag (cute-dbt#100) — rendered as a
    /// dashed node, never raised.
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
/// nodes = models, edges = forward dependency edges. An empty `nodes`
/// array selects the page's empty-state message instead of a Cytoscape
/// render.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LineagePayload {
    /// Every model node, in deterministic node-id order.
    pub nodes: Vec<LineageNodePayload>,
    /// Forward dependency edges between models in `nodes`, ordered.
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
/// Composes [`build_lineage`] (nodes = the model set, edges =
/// `depends_on.nodes` filtered to models, **forward only**) into the
/// id-keyed POD the Cytoscape engine consumes. Pure assembly over owned
/// manifest data — no I/O.
#[must_use]
pub fn build_lineage_payload(current: &Manifest, models: &ModelInScopeSet) -> LineagePayload {
    let lineage = build_lineage(current, models);
    let edges = lineage
        .edges
        .iter()
        .map(|&(from, to)| LineageEdgePayload {
            from: lineage.nodes[from].id.clone(),
            to: lineage.nodes[to].id.clone(),
        })
        .collect();
    let nodes = lineage
        .nodes
        .into_iter()
        .map(|n| LineageNodePayload {
            id: n.id,
            name: n.name,
            not_compiled: n.not_compiled,
            data_tests: n.data_tests,
            unit_tests: n.unit_tests,
            badge: test_badge(n.data_tests, n.unit_tests),
        })
        .collect();
    LineagePayload {
        nodes,
        edges,
        cte_dags: BTreeMap::new(),
    }
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

/// askama binding for `templates/explore-dag.html`.
#[derive(Template)]
#[template(path = "explore-dag.html", escape = "html")]
struct ExploreDagTemplate<'a> {
    sakura_css: &'a str,
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
    model_count: usize,
    edge_count: usize,
    not_compiled_count: usize,
}

/// askama binding for `templates/explore-tests.html`.
#[derive(Template)]
#[template(path = "explore-tests.html", escape = "html")]
struct ExploreTestsTemplate<'a> {
    sakura_css: &'a str,
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

/// Render the two explore pages into `out_dir` (created if absent).
///
/// Writes `dag.html` then `tests.html`; a failure on either write (or
/// on directory creation) surfaces the underlying [`io::Error`] —
/// the cli layer names `--out-dir` in the operator message. Template
/// rendering itself is compile-time-checked askama (the same
/// infallible-at-runtime posture as the report renderer).
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when `out_dir` cannot be
/// created or either page cannot be written.
pub fn render_explore(
    out_dir: &Path,
    current: &Manifest,
    models: &ModelInScopeSet,
    payload: &ReportPayload,
) -> io::Result<()> {
    fs::create_dir_all(out_dir)?;

    let mut lineage = build_lineage_payload(current, models);
    // cute-dbt#102 — the CTE ⇄ model toggle's per-model CTE DAGs ride
    // the same carrier (the payload's graphs, parsed once upstream).
    lineage.cte_dags = cte_dags_by_model(models, payload);
    let not_compiled_count = lineage.nodes.iter().filter(|n| n.not_compiled).count();
    let dag_json = json_for_html_script(&lineage)
        .map_err(|err| io::Error::other(format!("dag payload serialization: {err}")))?;
    let dag_html = ExploreDagTemplate {
        sakura_css: SAKURA_CSS,
        cytoscape_js: CYTOSCAPE_JS,
        cytoscape_dagre_js: CYTOSCAPE_DAGRE_JS,
        explore_lineage_js: EXPLORE_LINEAGE_JS,
        explore_cte_js: EXPLORE_CTE_JS,
        favicon_data_uri: FAVICON_DATA_URI,
        dag_json: &dag_json,
        model_count: lineage.nodes.len(),
        edge_count: lineage.edges.len(),
        not_compiled_count,
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
        favicon_data_uri: FAVICON_DATA_URI,
        models: &models_pod,
        model_count: models_pod.len(),
        test_count,
        explore_tests_js: EXPLORE_TESTS_JS,
        payload_json: &payload_json,
    }
    .render()
    .map_err(|err| io::Error::other(format!("render tests.html: {err}")))?;
    fs::write(out_dir.join("tests.html"), tests_html)?;

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
        let payload = build_lineage_payload(&current, &all_models(&current));
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
        let payload = build_lineage_payload(&current, &all_models(&current));
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
        let payload = build_lineage_payload(&current, &all_models(&current));
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
        let payload = build_lineage_payload(&current, &all_models(&current));
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

        render_explore(&dir, &current, &models, &payload).expect("explore renders");

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
        // tests.html is the page-aware static page: no Mermaid bundle.
        assert!(!tests.contains("mermaid"), "tests.html carries no Mermaid");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_explore_is_deterministic() {
        let current = three_model_manifest();
        let models = all_models(&current);
        let payload = payload_for(&current, &models);
        let dir_a = tmp_dir("det-a");
        let dir_b = tmp_dir("det-b");
        render_explore(&dir_a, &current, &models, &payload).expect("first render");
        render_explore(&dir_b, &current, &models, &payload).expect("second render");
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
        render_explore(&dir, &current, &models, &payload).expect("creates out-dir");
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
        render_explore(&dir, &current, &models, &payload).expect("empty manifest renders");
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
        render_explore(&dir, &current, &models, &payload).expect("explore renders");
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
        render_explore(&dir, &current, &models, &payload).expect("explore renders");
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
    /// the BARE model name — resolution is `resolve_target_model`).
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
        let payload = build_lineage_payload(&current, &all_models(&current));
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
}
