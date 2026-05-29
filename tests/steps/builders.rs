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
use std::path::{Path, PathBuf};

use cute_dbt::domain::{
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
        None,
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

// ---------------------------------------------------------------------
// PR-diff scoping builders (pr_diff_scoping.feature — cute-dbt#84).
//
// The PR-diff path matches a changed-file list against each node's
// `original_file_path`, so these builders carry an explicit
// `original_file_path` (unlike `model_node` / `unit_test_for`, which
// leave it `None` for the baseline-comparison scenarios). The synthetic
// manifest is serialized to a temp file and re-read by the `cute-dbt`
// subprocess, exercising the real `resolve_scope_input → select_in_scope
// → render` run-loop end to end.
// ---------------------------------------------------------------------

/// Construct a model `Node` with an explicit `original_file_path` (the
/// dbt-emitted on-disk location the PR-diff path matches against). Bare
/// name + checksum + optional compiled SQL otherwise mirror
/// [`model_node`].
#[must_use]
pub fn model_node_with_original_file_path(
    bare: &str,
    checksum: &str,
    compiled: Option<&str>,
    original_file_path: &str,
) -> Node {
    Node::new(
        model_id(bare),
        "model",
        Checksum::new("sha256", checksum),
        compiled.map(str::to_owned),
        None,
        DependsOn::default(),
        Some(original_file_path.to_owned()),
    )
}

/// Construct a `UnitTest` targeting `target_bare` with an explicit
/// `original_file_path` (the declaring `.yml` file the PR-diff path
/// matches against). Given/expect blocks are empty — the PR-diff
/// scenarios assert scoping membership, not fixture content.
#[must_use]
pub fn unit_test_with_path(name: &str, target_bare: &str, original_file_path: &str) -> UnitTest {
    UnitTest::new(
        name,
        NodeId::new(target_bare),
        Vec::<UnitTestGiven>::new(),
        UnitTestExpect::new(Value::Array(Vec::new()), None),
        None,
        DependsOn::default(),
        None,
        None,
        Some(original_file_path.to_owned()),
    )
}

/// Serialize a synthetic `Manifest` to `CARGO_TARGET_TMPDIR/{name}.json`
/// and return the path. The `cute-dbt` subprocess re-reads it through the
/// Stage-1 manifest adapter, so the domain→JSON→wire round-trip is
/// exercised for real (empty macros + no `config.tags`/`meta` keep the
/// two wire-shape divergences out of play). `bdd.rs` runs
/// `max_concurrent_scenarios(1)`, so the shared temp dir is collision-free.
#[must_use]
pub fn serialize_to_tmp(manifest: &Manifest, name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.json"));
    let json = serde_json::to_string(manifest).expect("synthetic manifest serializes");
    std::fs::write(&path, json).expect("write synthetic manifest to temp file");
    path
}
