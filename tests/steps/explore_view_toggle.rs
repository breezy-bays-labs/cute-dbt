//! Step definitions for `features/explore_view_toggle.feature` —
//! cute-dbt#102, the explore view toggle (CTE ⇄ model) on dag.html and
//! the tests.html unit-test viewer.
//!
//! Reuses the `explore_full_manifest.rs` Givens/When (the synthetic
//! `ExplorePlan` accumulator + the real `cute-dbt explore` subprocess);
//! the Thens here assert the cute-dbt#102 render-time surface: the
//! two-arm view toggle's DOM (CTE arm disabled — selection is a runtime
//! act), the carrier's per-model `cte_dags` map (present for a
//! CTE-bearing compiled model, ABSENT for an uncompiled one — the
//! fail-open contract renders a labeled degraded view client-side,
//! never an error), the shared test-card partial on tests.html, the
//! index-row viewer wiring, and the no-graph-engine + zero-egress
//! posture of the tests page. The RUNTIME toggle behavior (enable on
//! highlight, view flip, lineage state persistence) is the headless
//! Chromium suite's job (`tests/headless_zero_egress.rs`).

use cucumber::{given, then};
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

/// The rendered tests.html, panicking with stderr context when absent.
fn tests_html(world: &World) -> String {
    world
        .explore_tests_html
        .clone()
        .unwrap_or_else(|| panic!("tests.html was not written; stderr={}", world.last_stderr))
}

// --- Given ----------------------------------------------------------------

#[given(regex = r#"^the explore model "([^"]+)" compiles to SQL with the CTE "([^"]+)"$"#)]
fn model_compiles_with_cte(world: &mut World, bare: String, cte: String) {
    let decl = world
        .explore_plan
        .models
        .iter_mut()
        .find(|m| m.bare == bare)
        .unwrap_or_else(|| panic!("model {bare:?} must be declared before it is configured"));
    decl.compiled_sql = Some(format!(
        "with {cte} as (select * from db.sch.orders) select * from {cte}"
    ));
}

// --- dag.html: the view toggle ----------------------------------------------

#[then("dag.html renders the lineage and CTE view toggle arms")]
fn dag_renders_view_toggle(world: &mut World) {
    let html = dag_html(world);
    let dom = tl::parse(&html, tl::ParserOptions::default()).expect("dag.html must parse");
    let parser = dom.parser();
    let arm = |view: &str| {
        dom.query_selector(&format!("button[data-view=\"{view}\"]"))
            .and_then(|mut it| it.next())
            .and_then(|h| h.get(parser))
            .and_then(|n| n.as_tag())
            .unwrap_or_else(|| panic!("dag.html must render the {view} toggle arm"))
            .attributes()
            .clone()
    };
    let lineage = arm("lineage");
    assert!(
        lineage.get("disabled").is_none(),
        "the lineage arm (the boot view) is never disabled",
    );
    let cte = arm("cte");
    assert!(
        cte.get("disabled").is_some(),
        "the CTE arm is GATED on a highlight — disabled at render time \
         (selection is a runtime act)",
    );
}

#[then("dag.html renders the CTE view host hidden at render time")]
fn dag_renders_cte_host_hidden(world: &mut World) {
    let html = dag_html(world);
    let dom = tl::parse(&html, tl::ParserOptions::default()).expect("dag.html must parse");
    let parser = dom.parser();
    let host = dom
        .query_selector("div.cte-view")
        .and_then(|mut it| it.next())
        .and_then(|h| h.get(parser))
        .and_then(|n| n.as_tag())
        .expect("dag.html must render <div class=\"cte-view\">");
    assert!(
        host.attributes().get("hidden").is_some(),
        "the CTE view host starts hidden — lineage is the boot view",
    );
}

#[then("dag.html embeds the explore CTE engine")]
fn dag_embeds_cte_engine(world: &mut World) {
    let html = dag_html(world);
    assert!(
        html.contains("cute-dbt explore CTE engine v1"),
        "the first-party CTE engine banner must be inlined",
    );
}

// --- dag.html: the cte_dags carrier ------------------------------------------

#[then(regex = r#"^the dag carrier embeds a CTE DAG for "([^"]+)" containing the node "([^"]+)"$"#)]
fn carrier_embeds_cte_dag(world: &mut World, bare: String, cte_node: String) {
    let payload = lineage_payload(world);
    let model_id = payload_node(&payload, &bare)["id"]
        .as_str()
        .expect("model id is a string")
        .to_owned();
    let dag = &payload["cte_dags"][&model_id];
    assert!(
        dag.is_object(),
        "cte_dags must carry an entry for {bare:?} ({model_id}): {payload}",
    );
    let nodes = dag["nodes"].as_array().expect("cte dag nodes array");
    assert!(
        nodes
            .iter()
            .any(|n| n["id"] == Value::String(cte_node.clone())),
        "the {bare:?} CTE DAG must contain the {cte_node:?} node: {dag}",
    );
}

#[then(regex = r#"^the dag carrier embeds no CTE DAG for "([^"]+)"$"#)]
fn carrier_embeds_no_cte_dag(world: &mut World, bare: String) {
    let payload = lineage_payload(world);
    let model_id = payload_node(&payload, &bare)["id"]
        .as_str()
        .expect("model id is a string")
        .to_owned();
    assert!(
        payload["cte_dags"].get(&model_id).is_none(),
        "an uncompiled model ships NO cte_dags entry — the client \
         renders the labeled fail-open degraded view off not_compiled, \
         never an error: {payload}",
    );
}

// --- tests.html: the shared unit-test card -----------------------------------

#[then("tests.html renders the shared unit-test card")]
fn tests_html_renders_shared_card(world: &mut World) {
    let html = tests_html(world);
    let dom = tl::parse(&html, tl::ParserOptions::default()).expect("tests.html must parse");
    let parser = dom.parser();
    for selector in ["section.test-section", "div.panel-row"] {
        assert!(
            dom.query_selector(selector)
                .and_then(|mut it| it.next())
                .and_then(|h| h.get(parser))
                .is_some(),
            "tests.html must render the shared partial's {selector} \
             (templates/partials/test-card.html — the markup report.html renders)",
        );
    }
    assert!(
        dom.get_element_by_id("test-select").is_some(),
        "the partial's unit-test selector must be present",
    );
}

#[then("tests.html embeds the explore tests viewer")]
fn tests_html_embeds_viewer(world: &mut World) {
    let html = tests_html(world);
    assert!(
        html.contains("cute-dbt explore tests viewer v1"),
        "the first-party tests-viewer banner must be inlined",
    );
}

#[then(regex = r#"^tests\.html wires the unit test "([^"]+)" to the viewer$"#)]
fn tests_html_wires_test(world: &mut World, test: String) {
    let html = tests_html(world);
    let handle = format!("data-test-id=\"unit_test.jaffle_shop.{test}\"");
    assert!(
        html.contains(&handle),
        "the index row must carry the {handle} viewer handle",
    );
}

#[then("tests.html embeds no Cytoscape engine")]
fn tests_html_no_cytoscape(world: &mut World) {
    let html = tests_html(world);
    assert!(
        !html.contains("The Cytoscape Consortium"),
        "tests.html must not embed the Cytoscape core (its license banner leaked)",
    );
    assert!(
        !html.contains("cytoscapeDagre"),
        "tests.html must not embed the cytoscape-dagre layout extension",
    );
}

#[then("tests.html carries no external resource references")]
fn tests_html_zero_egress(world: &mut World) {
    let html = tests_html(world);
    super::super::common::assert_no_external_refs(&html);
}
