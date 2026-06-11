//! Step definitions for `features/explore_lineage_dag.feature` —
//! cute-dbt#101, the interactive Cytoscape model-lineage DAG.
//!
//! Reuses the `explore_full_manifest.rs` Givens/When (the synthetic
//! `ExplorePlan` accumulator + the real `cute-dbt explore` subprocess);
//! the Thens here assert the cute-dbt#101 surface: the
//! [`LineagePayload`](cute_dbt::adapters::explore::LineagePayload) JSON
//! carrier's forward-only edge contract, the vendored
//! Cytoscape + cytoscape-dagre embedding, the search/canvas affordance
//! DOM structure, and the render-time half of the highlight-vs-focus
//! contract (no `data-selected-model` is ever WRITTEN at render time —
//! only the runtime Space commit writes it, which the headless suite
//! drives with a real keyboard).

use cucumber::then;
use serde_json::Value;

use super::World;
use super::explore_full_manifest::{lineage_payload, payload_node};

/// The rendered dag.html, panicking with stderr context when absent.
fn dag_html(world: &World) -> String {
    world
        .explore_dag_html
        .clone()
        .unwrap_or_else(|| panic!("dag.html was not written; stderr={}", world.last_stderr))
}

// --- payload shape -----------------------------------------------------

#[then(regex = r"^the lineage payload carries exactly (\d+) nodes and (\d+) edges?$")]
fn payload_counts(world: &mut World, nodes: usize, edges: usize) {
    let payload = lineage_payload(world);
    assert_eq!(
        payload["nodes"].as_array().map_or(0, Vec::len),
        nodes,
        "lineage payload node count: {payload}",
    );
    assert_eq!(
        payload["edges"].as_array().map_or(0, Vec::len),
        edges,
        "lineage payload edge count: {payload}",
    );
}

#[then(regex = r#"^the lineage payload carries no reverse edge from "([^"]+)" to "([^"]+)"$"#)]
fn payload_no_reverse_edge(world: &mut World, from: String, to: String) {
    let payload = lineage_payload(world);
    let from_id = payload_node(&payload, &from)["id"].clone();
    let to_id = payload_node(&payload, &to)["id"].clone();
    let edges = payload["edges"].as_array().expect("edges array");
    assert!(
        !edges
            .iter()
            .any(|e| e["from"] == from_id && e["to"] == to_id),
        "edges are FORWARD only (upstream -> downstream); the client \
         traverses both directions itself — found a reverse \
         {from:?} -> {to:?} edge: {payload}",
    );
}

// --- engine embedding ---------------------------------------------------

#[then("dag.html embeds the Cytoscape core and the dagre layout extension")]
fn dag_embeds_cytoscape_and_dagre(world: &mut World) {
    let html = dag_html(world);
    assert!(
        html.contains("The Cytoscape Consortium"),
        "the vendored Cytoscape core (its license banner) must be inlined",
    );
    assert!(
        html.contains("cytoscapeDagre"),
        "the vendored cytoscape-dagre UMD extension (its global) must be inlined",
    );
    assert!(
        !html.contains("sourceMappingURL"),
        "no vendored bundle may carry a sourceMappingURL trailer \
         (stripped at vendoring — a sibling-.map ref breaks the \
         self-contained-page posture)",
    );
}

#[then("dag.html embeds the explore lineage engine")]
fn dag_embeds_lineage_engine(world: &mut World) {
    let html = dag_html(world);
    assert!(
        html.contains("cute-dbt explore lineage engine v1"),
        "the first-party lineage engine banner must be inlined",
    );
}

#[then("dag.html no longer embeds a Mermaid lineage")]
fn dag_has_no_mermaid(world: &mut World) {
    let html = dag_html(world);
    assert!(
        !html.contains("mermaid.initialize") && !html.contains("graph LR"),
        "the V1 static Mermaid lineage was replaced by the interactive \
         Cytoscape engine (cute-dbt#101) — no Mermaid boot may remain",
    );
}

// --- affordance DOM structure --------------------------------------------

#[then("dag.html renders the model search combobox")]
fn dag_renders_search(world: &mut World) {
    let html = dag_html(world);
    let dom = tl::parse(&html, tl::ParserOptions::default()).expect("dag.html must parse");
    let parser = dom.parser();
    let input = dom
        .query_selector("input.lineage-search-input")
        .and_then(|mut it| it.next())
        .and_then(|h| h.get(parser))
        .and_then(|n| n.as_tag())
        .expect("dag.html must render <input class=\"lineage-search-input\">");
    let attr = |name: &str| -> Option<String> {
        input
            .attributes()
            .get(name)
            .flatten()
            .map(|v| v.as_utf8_str().into_owned())
    };
    assert_eq!(attr("role").as_deref(), Some("combobox"), "combobox role");
    assert_eq!(
        attr("aria-expanded").as_deref(),
        Some("false"),
        "the results listbox starts closed",
    );
    assert!(
        dom.get_element_by_id("lineage-search-results").is_some(),
        "the results listbox host must exist",
    );
}

#[then("dag.html renders the focusable lineage canvas host")]
fn dag_renders_focusable_canvas(world: &mut World) {
    let html = dag_html(world);
    let dom = tl::parse(&html, tl::ParserOptions::default()).expect("dag.html must parse");
    let parser = dom.parser();
    let canvas = dom
        .query_selector("div.lineage-canvas")
        .and_then(|mut it| it.next())
        .and_then(|h| h.get(parser))
        .and_then(|n| n.as_tag())
        .expect("dag.html must render <div class=\"lineage-canvas\">");
    let tabindex = canvas
        .attributes()
        .get("tabindex")
        .flatten()
        .map(|v| v.as_utf8_str().into_owned());
    assert_eq!(
        tabindex.as_deref(),
        Some("0"),
        "the canvas host must be focusable (tabindex=0) so a \
         search-select can hand it focus — the cute-dbt#101 hard AC",
    );
}

#[then("dag.html writes no data-selected-model at render time")]
fn dag_no_selected_model_at_render(world: &mut World) {
    let html = dag_html(world);
    let dom = tl::parse(&html, tl::ParserOptions::default()).expect("dag.html must parse");
    let parser = dom.parser();
    let body = dom
        .query_selector("body")
        .and_then(|mut it| it.next())
        .and_then(|h| h.get(parser))
        .and_then(|n| n.as_tag())
        .expect("dag.html has a <body>");
    assert!(
        body.attributes().get("data-selected-model").is_none(),
        "data-selected-model is the RUNTIME focus-commit signal (Space \
         only) — the renderer must never bake it into the page",
    );
}

// --- hostile names --------------------------------------------------------

#[then(regex = r#"^the lineage payload round-trips the name "(.+)"$"#)]
fn payload_round_trips_hostile_name(world: &mut World, name: String) {
    let payload = lineage_payload(world);
    let nodes = payload["nodes"].as_array().expect("nodes array");
    assert!(
        nodes
            .iter()
            .any(|n| n["name"] == Value::String(name.clone())),
        "the hostile name must survive the wire round-trip as DATA: {payload}",
    );
}

#[then("dag.html carries no unescaped script-closing markup in the payload carrier")]
fn dag_escapes_script_closers(world: &mut World) {
    let html = dag_html(world);
    assert!(
        !html.contains("</script><img"),
        "a hostile model name must never materialize tag-opening markup \
         (the json_for_html_script escape contract)",
    );
}
