//! Integration coverage for the PR 7 CTE engine, exercised against the
//! real jaffle-shop fixture's compiled SQL loaded through the PR 4b
//! manifest adapter.
//!
//! This is the PR 4b → PR 7 *fixture-readiness* edge: PR 7's **tests**
//! consume PR 4b's loader to deserialize the real fixture and feed its
//! `compiled_code` to the engine; PR 7's production code imports only
//! `domain` types — `adapters::cte_engine` never reaches into the
//! manifest adapter.

use std::path::{Path, PathBuf};

use cute4dbt::adapters::cte_engine::{TERMINAL_NODE_NAME, parse_cte_graph};
use cute4dbt::adapters::manifest::FileManifestSource;
use cute4dbt::domain::{EdgeType, Manifest};
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

/// The compiled SQL of one model node, by full node id.
fn compiled_code(manifest: &Manifest, node_id: &str) -> String {
    manifest
        .nodes()
        .values()
        .find(|node| node.id().as_str() == node_id)
        .unwrap_or_else(|| panic!("fixture has a node {node_id}"))
        .compiled_code()
        .unwrap_or_else(|| panic!("{node_id} was compiled (dbt compile/run)"))
        .to_owned()
}

#[test]
fn the_customers_model_yields_its_six_ctes_and_a_terminal_node() {
    let manifest = load("jaffle-shop-current.json");
    let sql = compiled_code(&manifest, "model.jaffle_shop.customers");
    let graph = parse_cte_graph(&sql).expect("the customers model's compiled SQL parses");

    let names: Vec<&str> = graph.nodes().iter().map(|n| n.name()).collect();
    assert_eq!(
        names,
        [
            "customers",
            "orders",
            "payments",
            "customer_orders",
            "customer_payments",
            "final",
            TERMINAL_NODE_NAME,
        ],
        "six CTEs in declaration order, then the terminal node",
    );
}

#[test]
fn the_customers_model_classifies_every_join_as_left_and_bases_as_from() {
    // The jaffle-shop `customers` model joins exclusively with LEFT joins.
    // With the widened vocabulary, base relations get From edges and joined
    // relations get Left edges. The plain FROM-only CTEs also emit From.
    let manifest = load("jaffle-shop-current.json");
    let sql = compiled_code(&manifest, "model.jaffle_shop.customers");
    let graph = parse_cte_graph(&sql).expect("parses");

    // Every edge must be acyclic.
    for edge in graph.edges() {
        assert!(edge.from() < edge.to(), "the graph is acyclic");
    }
    // No UnionAll/UnionDistinct edges in the customers model.
    for edge in graph.edges() {
        assert!(
            !matches!(
                edge.edge_type(),
                EdgeType::UnionAll | EdgeType::UnionDistinct
            ),
            "customers model has no UNION edges",
        );
    }
    // All non-From edges must be Left.
    for edge in graph.edges() {
        if edge.edge_type() != EdgeType::From {
            assert_eq!(
                edge.edge_type(),
                EdgeType::Left,
                "all join edges in customers are LEFT",
            );
        }
    }
}

#[test]
fn a_join_free_cte_has_a_from_incoming_edge() {
    // `customer_orders` (declaration index 3) depends on `orders` via a
    // plain `FROM orders` — now emits a From edge (widened from v0.1
    // "non-join FROM emits no edge" rule).
    let manifest = load("jaffle-shop-current.json");
    let sql = compiled_code(&manifest, "model.jaffle_shop.customers");
    let graph = parse_cte_graph(&sql).expect("parses");

    // `orders` is index 1, `customer_orders` is index 3.
    let from_edges_to_customer_orders: Vec<_> =
        graph.edges().iter().filter(|e| e.to() == 3).collect();
    assert!(
        !from_edges_to_customer_orders.is_empty(),
        "customer_orders now has an incoming From edge",
    );
    assert!(
        from_edges_to_customer_orders
            .iter()
            .all(|e| e.edge_type() == EdgeType::From),
        "all edges into customer_orders are From (no join)",
    );
}

#[test]
fn a_model_whose_ctes_never_join_produces_from_edges() {
    // `stg_customers` has two CTEs (`source`, `renamed`) wired purely by
    // pass-through `FROM` — now emits From edges (not an empty edge set).
    let manifest = load("jaffle-shop-current.json");
    let sql = compiled_code(&manifest, "model.jaffle_shop.stg_customers");
    let graph = parse_cte_graph(&sql).expect("parses");

    assert_eq!(graph.nodes().len(), 3, "two CTEs plus the terminal node");
    assert!(
        !graph.edges().is_empty(),
        "pass-through FROM now emits From edges",
    );
    for edge in graph.edges() {
        assert_eq!(
            edge.edge_type(),
            EdgeType::From,
            "every edge in a join-free model is From",
        );
    }
}

#[test]
fn every_compiled_model_in_the_fixture_parses() {
    // Dialect-coverage guard: the generic dialect must parse every
    // compiled model in the fixture. A real model that fails here is the
    // signal to revisit the dialect choice.
    let manifest = load("jaffle-shop-current.json");
    for node in manifest.nodes().values() {
        if node.resource_type() != "model" {
            continue;
        }
        let Some(sql) = node.compiled_code() else {
            continue;
        };
        assert!(
            parse_cte_graph(sql).is_ok(),
            "compiled SQL for {} parses under the generic dialect",
            node.id(),
        );
    }
}
