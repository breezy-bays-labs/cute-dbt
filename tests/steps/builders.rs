//! Synthetic-Manifest builders.
//!
//! Every scenario in `diff_scoping.feature` and `cte_rendering.feature`
//! describes a small named situation ("the model `stg_orders` has a
//! different body checksum than the baseline"). The builders here turn
//! that prose into in-memory `Manifest` / `Node` / `UnitTest` values
//! without going through fixture files — the .feature files are the
//! spec, the fixture pairs under `tests/fixtures/` are for the
//! subprocess-level scenarios.
//!
//! Bare model names from the scenarios (`stg_orders`, `stg_customers`,
//! `stg_returns`, `stg_payments`) are mapped to fully-qualified node
//! ids via [`model_id`]: `model.jaffle_shop.<bare>`. The renderer's
//! `resolve_target_model` step is what maps the bare `model:` field on
//! a unit test back to the fully-qualified node, so this builders
//! module is consistent with how the production code routes the same
//! relationships.

use std::collections::HashMap;

use cute4dbt::domain::{
    Checksum, DependsOn, Manifest, ManifestMetadata, Node, NodeId, UnitTest, UnitTestExpect,
    UnitTestGiven,
};
use serde_json::Value;

/// Map a bare model name (`stg_orders`) to its fully-qualified node id
/// (`model.jaffle_shop.stg_orders`). The package label matches the
/// committed `tests/fixtures/jaffle-shop-*.json` set so the synthetic
/// scenarios talk in the same vocabulary as the real fixtures.
#[must_use]
pub fn model_id(bare: &str) -> NodeId {
    NodeId::new(format!("model.jaffle_shop.{bare}"))
}

/// Map a bare unit-test name (`test_stg_orders_dedup`) to the
/// fully-qualified manifest key (`unit_test.jaffle_shop.<bare>`).
#[must_use]
pub fn unit_test_key(bare: &str) -> String {
    format!("unit_test.jaffle_shop.{bare}")
}

/// A baseline + current pair seeded with a single model whose body
/// checksum the scenario can advance to make the model "modified".
/// The starting state of both manifests is identical (so `modified_set`
/// returns empty when no further mutation happens).
#[must_use]
pub fn empty_pair() -> (Manifest, Manifest) {
    (empty_manifest(), empty_manifest())
}

/// A `Manifest` with metadata pinned to v12 (the minimum supported
/// schema floor) and no nodes / no unit tests.
#[must_use]
pub fn empty_manifest() -> Manifest {
    Manifest::new(
        ManifestMetadata::new("https://schemas.getdbt.com/dbt/manifest/v12.json"),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    )
}

/// Construct a model `Node` with explicit checksum and (optional)
/// compiled SQL. Resource type is fixed to `"model"`; the synthetic
/// scenarios never need other resource types.
#[must_use]
pub fn model_node(bare: &str, checksum: &str, compiled: Option<&str>) -> Node {
    Node::new(
        model_id(bare),
        "model",
        Checksum::new("sha256", checksum),
        compiled.map(str::to_owned),
        None,
        DependsOn::default(),
    )
}

/// Construct a minimal `UnitTest` targeting the model `target` with
/// empty given/expect blocks. The scenarios only assert scoping
/// membership and identity — the fixture rows are noise here.
#[must_use]
pub fn unit_test_for(name: &str, target_bare: &str) -> UnitTest {
    UnitTest::new(
        name,
        NodeId::new(target_bare),
        Vec::<UnitTestGiven>::new(),
        UnitTestExpect::new(Value::Array(Vec::new()), None),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
}

/// Insert a `Node` into a `Manifest`, returning the modified manifest.
#[must_use]
pub fn with_node(mut manifest: Manifest, node: Node) -> Manifest {
    // The HashMap is reached via a borrowing accessor (`manifest.nodes()`).
    // To mutate, clone the inner maps (POD-only domain has no builder
    // mutation method by ADR-1). Cloning is cheap on the small
    // synthetic manifests these scenarios build.
    let mut nodes: HashMap<NodeId, Node> = manifest.nodes().clone();
    nodes.insert(node.id().clone(), node);
    let unit_tests = manifest.unit_tests().clone();
    let macros = manifest.macros().clone();
    manifest = Manifest::new(manifest.metadata().clone(), nodes, unit_tests, macros);
    manifest
}

/// Insert a `UnitTest` into a `Manifest`, keyed by `unit_test.jaffle_shop.<name>`.
#[must_use]
pub fn with_unit_test(mut manifest: Manifest, unit_test: UnitTest) -> Manifest {
    let key = unit_test_key(unit_test.name());
    let nodes = manifest.nodes().clone();
    let mut unit_tests = manifest.unit_tests().clone();
    unit_tests.insert(key, unit_test);
    let macros = manifest.macros().clone();
    manifest = Manifest::new(manifest.metadata().clone(), nodes, unit_tests, macros);
    manifest
}
