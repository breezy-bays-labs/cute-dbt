//! Integration coverage for the PR 8a asset-embedding infrastructure,
//! exercised against a real `CteGraph` parsed from the jaffle-shop
//! fixture's compiled SQL.
//!
//! This is the PR 8a → (PR 4b + PR 7) *fixture-readiness* edge: the smoke
//! renderer's **test** loads the real fixture through the manifest adapter
//! and the CTE engine; the renderer's production code imports only
//! `domain` types.

use std::path::{Path, PathBuf};

use cute4dbt::adapters::asset_embed::{
    DATATABLES_CSS, DATATABLES_JS, JQUERY_JS, MERMAID_JS, SAKURA_CSS, smoke_report_html,
};
use cute4dbt::adapters::cte_engine::{TERMINAL_NODE_NAME, parse_cte_graph};
use cute4dbt::adapters::manifest::FileManifestSource;
use cute4dbt::domain::{CteGraph, Manifest};
use cute4dbt::ports::ManifestSource;

/// Absolute path to a committed fixture under `tests/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn load(name: &str) -> Manifest {
    FileManifestSource
        .load(&fixture(name))
        .unwrap_or_else(|err| panic!("fixture {name} is a valid v12 manifest: {err:?}"))
}

/// The CTE graph of the jaffle-shop `customers` model — six CTEs, all
/// joined with `LEFT` joins, plus the synthetic terminal node.
fn customers_graph() -> CteGraph {
    let manifest = load("jaffle-shop-current.json");
    let sql = manifest
        .nodes()
        .values()
        .find(|node| node.id().as_str() == "model.jaffle_shop.customers")
        .expect("fixture has the customers model")
        .compiled_code()
        .expect("the customers model was compiled (dbt compile/run)")
        .to_owned();
    parse_cte_graph(&sql).expect("the customers model's compiled SQL parses")
}

#[test]
fn the_smoke_report_bundles_every_asset_for_a_real_graph() {
    let html = smoke_report_html(&customers_graph());
    for (label, asset) in [
        ("sakura", SAKURA_CSS),
        ("datatables-css", DATATABLES_CSS),
        ("jquery", JQUERY_JS),
        ("datatables-js", DATATABLES_JS),
        ("mermaid", MERMAID_JS),
    ] {
        assert!(html.contains(asset), "{label} is inlined into the report");
    }
}

#[test]
fn the_smoke_report_renders_the_real_cte_graph() {
    let html = smoke_report_html(&customers_graph());
    assert!(html.contains("graph LR"), "a Mermaid graph is emitted");
    // The jaffle-shop customers model declares a `customers` CTE; the
    // engine appends the synthetic terminal node after it.
    assert!(html.contains("[\"customers\"]"), "a real CTE is a node");
    assert!(
        html.contains(&format!("[\"{TERMINAL_NODE_NAME}\"]")),
        "the terminal node is rendered",
    );
    // The customers model joins exclusively with LEFT joins.
    assert!(html.contains("-->|LEFT|"), "join-typed edges are rendered");
}

#[test]
fn the_smoke_report_is_well_formed_for_a_real_graph() {
    let html = smoke_report_html(&customers_graph());
    assert!(html.starts_with("<!doctype html>"), "doctype first");
    assert!(html.trim_end().ends_with("</html>"), "html closed");
    assert!(
        html.contains("<pre class=\"mermaid\">"),
        "mermaid container present",
    );
    assert!(
        html.contains("href=\"data:,\""),
        "favicon is an empty data: URI",
    );
}
