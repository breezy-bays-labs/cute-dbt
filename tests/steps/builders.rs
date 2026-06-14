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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use cute_dbt::domain::{
    Checksum, DependsOn, Manifest, ManifestMetadata, Node, NodeConfig, NodeId, UnitTest,
    UnitTestExpect, UnitTestGiven,
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

/// A manifest carrying one hook `operation.*` node in the dbt shape
/// (cute-dbt#269; dbt-fusion `resolve_operations.rs:106-145` @
/// `9977b6cb…`): id `operation.{project}.{project}-on-run-{kind}-{i}`,
/// `raw_code` = the hook SQL verbatim, declaring path
/// `./dbt_project.yml` VERBATIM (the `./` prefix is part of the wire
/// shape the panel must resolve through the normalization authority).
#[must_use]
pub fn manifest_with_operation_node(
    project: &str,
    kind: &str,
    index: usize,
    sql: &str,
) -> Manifest {
    let name = format!("{project}-on-run-{kind}-{index}");
    let id = NodeId::new(format!("operation.{project}.{name}"));
    let node = Node::new(
        id.clone(),
        "operation",
        Checksum::new("sha256", "hook"),
        None,
        Some(sql.to_owned()),
        DependsOn::default(),
        Some("./dbt_project.yml".to_owned()),
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
    .with_identity(Some(name), Some(project.to_owned()));
    Manifest::new(
        ManifestMetadata::new("https://schemas.getdbt.com/dbt/manifest/v12.json"),
        HashMap::from([(id, node)]),
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
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
}

/// Construct a model `Node` like [`model_node`] but carrying explicit
/// `depends_on.nodes` edges to other models, by bare name (cute-dbt#100
/// — the explore lineage scenarios).
#[must_use]
pub fn model_node_with_deps(
    bare: &str,
    checksum: &str,
    compiled: Option<&str>,
    dep_bares: &[&str],
) -> Node {
    Node::new(
        model_id(bare),
        "model",
        Checksum::new("sha256", checksum),
        compiled.map(str::to_owned),
        None,
        DependsOn::new(Vec::new(), dep_bares.iter().map(|d| model_id(d)).collect()),
        None,
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
}

/// Construct a model `Node` like [`model_node`] but carrying an explicit
/// manifest `fqn` (cute-dbt#267 — the config-tree attribution
/// scenarios; the fqn's first segment is the PROJECT name from the
/// scenario's dbt_project.yml, independent of the node-id package
/// label).
#[must_use]
pub fn model_node_with_fqn(
    bare: &str,
    checksum: &str,
    compiled: Option<&str>,
    fqn: &[&str],
) -> Node {
    model_node(bare, checksum, compiled).with_fqn(fqn.iter().map(|s| (*s).to_owned()).collect())
}

/// Construct a model `Node` like [`model_node`] but carrying an explicit
/// resolved config dict (cute-dbt#160 — the config-only-change
/// scenarios). Contract stays unenforced; the other facets mirror
/// [`model_node`].
#[must_use]
pub fn model_node_with_config(
    bare: &str,
    checksum: &str,
    compiled: Option<&str>,
    config: &[(&str, &str)],
) -> Node {
    let map: BTreeMap<String, Value> = config
        .iter()
        .map(|(k, v)| ((*k).to_owned(), Value::from(*v)))
        .collect();
    Node::new(
        model_id(bare),
        "model",
        Checksum::new("sha256", checksum),
        compiled.map(str::to_owned),
        None,
        DependsOn::default(),
        None,
        NodeConfig::new(map, false),
        None,
        BTreeMap::new(),
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
        UnitTestExpect::new(Value::Array(Vec::new()), None, None),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
}

/// Construct a `UnitTest` targeting the model `target` whose givens
/// carry literal dict rows — the cute-dbt#196 subquery-satisfaction
/// scenarios assert key-match coverage over these rows.
#[must_use]
pub fn unit_test_with_givens(
    name: &str,
    target_bare: &str,
    givens: &[(String, Value)],
) -> UnitTest {
    let givens = givens
        .iter()
        .map(|(input, rows)| {
            UnitTestGiven::new(input.clone(), rows.clone(), Some("dict".to_owned()), None)
        })
        .collect();
    UnitTest::new(
        name,
        NodeId::new(target_bare),
        givens,
        UnitTestExpect::new(Value::Array(Vec::new()), None, None),
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
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
}

/// Like [`model_node_with_original_file_path`] but also carries a
/// `patch_path` (the model's scheme-stripped `schema.yml`) — the
/// cute-dbt#413 `config` change-axis matches the diff against this. The
/// builder mirrors the dbt-emitted package-relative, scheme-stripped form.
#[must_use]
pub fn model_node_with_ofp_and_patch_path(
    bare: &str,
    checksum: &str,
    compiled: Option<&str>,
    original_file_path: &str,
    patch_path: &str,
) -> Node {
    model_node_with_original_file_path(bare, checksum, compiled, original_file_path)
        .with_patch_path(Some(patch_path.to_owned()))
}

/// A deterministic multi-line `raw_code` (raw Jinja, dbt-core shape:
/// trailing newline already stripped) for the model-SQL-diff scenarios
/// (cute-dbt#111). Line 2 is the value a `ModelSqlTargetKind::Edit` /
/// `Whitespace` hunk rewrites; the synthesizer matches its `+` line to
/// this content so N7b aligns.
pub const MODEL_SQL_RAW_CODE: &str =
    "with src as (\n    select * from {{ ref('upstream') }}\n)\nselect * from src";

/// Construct a model `Node` carrying both `raw_code` (so the Model SQL
/// section + the cute-dbt#111 SQL diff can render) and an explicit
/// `original_file_path`. `compiled` keeps the model past Stage-2 preflight.
#[must_use]
pub fn model_node_with_raw_code(
    bare: &str,
    checksum: &str,
    compiled: Option<&str>,
    original_file_path: &str,
    raw_code: &str,
) -> Node {
    Node::new(
        model_id(bare),
        "model",
        Checksum::new("sha256", checksum),
        compiled.map(str::to_owned),
        Some(raw_code.to_owned()),
        DependsOn::default(),
        Some(original_file_path.to_owned()),
        NodeConfig::default(),
        None,
        BTreeMap::new(),
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
        UnitTestExpect::new(Value::Array(Vec::new()), None, None),
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

/// Serialize a synthetic `Manifest` like [`serialize_to_tmp`], but
/// injecting wire-shaped `macros` entries (cute-dbt#268).
///
/// The domain `Manifest` stores macros as `id -> body string`, while the
/// wire shape is `id -> { macro_sql, depends_on }` — a plain
/// domain→JSON round-trip would hand the adapter a string where it
/// expects an object (the cute-dbt#160 wire-config divergence,
/// macros edition). Each `(id, macro_sql)` entry is written in the real
/// wire shape with an empty `depends_on.macros` (the leaf-macro shape).
#[must_use]
pub fn serialize_with_wire_macros_to_tmp(
    manifest: &Manifest,
    name: &str,
    macros: &[(String, String)],
) -> PathBuf {
    let mut value: Value = serde_json::to_value(manifest).expect("manifest serializes to Value");
    for (id, sql) in macros {
        value["macros"][id] = serde_json::json!({
            "macro_sql": sql,
            "depends_on": { "macros": [] },
        });
    }
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.json"));
    std::fs::write(
        &path,
        serde_json::to_string(&value).expect("injected manifest serializes"),
    )
    .expect("write synthetic manifest to temp file");
    path
}

/// Serialize a synthetic `Manifest` like [`serialize_to_tmp`], but
/// rewriting each node's `config` to dbt's FLAT wire dict (cute-dbt#160).
///
/// The cute-dbt#145 wire-shape divergence applies: the domain serializes
/// [`NodeConfig`] as a nested `{ config: {...}, contract_enforced }`
/// struct while the wire reader **flattens** `config`, so a plain
/// domain→JSON→wire round-trip would mangle the config dict — and with
/// it the config-only divergence the `--modified-selectors configs`
/// scenarios assert. Each node's domain config map is injected verbatim
/// as the wire `config` object instead.
#[must_use]
pub fn serialize_with_wire_config_to_tmp(manifest: &Manifest, name: &str) -> PathBuf {
    let mut value: Value = serde_json::to_value(manifest).expect("manifest serializes to Value");
    for (id, node) in manifest.nodes() {
        let config: serde_json::Map<String, Value> = node
            .config()
            .config()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        value["nodes"][id.as_str()]["config"] = Value::Object(config);
    }
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.json"));
    std::fs::write(
        &path,
        serde_json::to_string(&value).expect("injected manifest serializes"),
    )
    .expect("write synthetic manifest to temp file");
    path
}

// ---------------------------------------------------------------------
// Coverage-check builders (coverage_checks.feature — cute-dbt#169).
// ---------------------------------------------------------------------

/// Serialize a synthetic CURRENT manifest for the coverage-check
/// scenarios, injecting the wire shapes the flat-domain serialization
/// cannot express (the [`serialize_incremental_to_tmp`] precedent):
///
/// 1. **flat model `config`** — each `(bare, config)` entry rewrites the
///    node's `config` to dbt's flat dict (carrying `materialized` and
///    `unique_key` exactly as fusion serializes them: a string OR an
///    array of strings — `DbtUniqueKey`, dbt-fusion `9977b6cb…`);
/// 2. **generic-test nodes** — each `(node id, node json)` entry is
///    spliced into `nodes` verbatim, in the real wire shape
///    (`resource_type: "test"`, `attached_node`, `column_name`,
///    `test_metadata`, flat `config.enabled`) that the domain types do
///    not round-trip (domain `NodeConfig` serializes nested);
/// 3. **unit-test `overrides.macros.is_incremental`** — each
///    `(unit-test key, bool)` entry injects the nested wire `overrides`
///    block (the cute-dbt#145 divergence: the domain stores the mode
///    flat) for the cute-dbt#164 branch-rollup scenarios;
/// 4. **the top-level `disabled` map** (cute-dbt#259) — each
///    `(unique_id, entry json)` pair injects one per-id ARRAY entry in
///    the real wire shape (whole node payloads keyed by unique_id —
///    where both engines put disabled tests).
#[must_use]
pub fn serialize_coverage_to_tmp(
    manifest: &Manifest,
    name: &str,
    model_configs: &[(&str, Value)],
    test_nodes: &[(String, Value)],
    unit_test_overrides: &[(String, bool)],
    disabled: &[(String, Value)],
) -> PathBuf {
    let mut value: Value = serde_json::to_value(manifest).expect("manifest serializes to Value");
    for (bare, config) in model_configs {
        let id = model_id(bare);
        value["nodes"][id.as_str()]["config"] = config.clone();
    }
    for (id, node) in test_nodes {
        value["nodes"][id.as_str()] = node.clone();
    }
    for (key, is_incremental) in unit_test_overrides {
        value["unit_tests"][key.as_str()]["overrides"] =
            serde_json::json!({ "macros": { "is_incremental": is_incremental } });
    }
    for (id, entry) in disabled {
        value["disabled"][id.as_str()] = Value::Array(vec![entry.clone()]);
    }
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.json"));
    std::fs::write(
        &path,
        serde_json::to_string(&value).expect("injected manifest serializes"),
    )
    .expect("write coverage manifest to temp file");
    path
}

/// Serialize a synthetic CURRENT manifest for the explore scenarios
/// (cute-dbt#104), injecting the wire shapes the flat-domain
/// serialization cannot express (the [`serialize_coverage_to_tmp`]
/// precedent, generalized to arbitrary top-level node keys):
///
/// 1. **node patches** — each `(bare, key, value)` triple rewrites one
///    top-level key on the model's wire node: the flat `config` dict
///    (domain `NodeConfig` serializes nested; the wire reader flattens)
///    and the object-shaped `columns` map (the domain serializes a
///    name→type map the wire reader cannot ingest);
/// 2. **raw nodes** — each `(node id, node json)` entry is spliced
///    into `nodes` verbatim in the real wire shape: test nodes
///    (`resource_type: "test"`, `attached_node`, `test_metadata`, flat
///    `config.enabled`) and, since cute-dbt#253, snapshot/seed nodes;
/// 3. **top-level map entries** (cute-dbt#253) — each
///    `(map, entry key, entry json)` triple is spliced into a top-level
///    manifest map (`sources` / `exposures`) verbatim, in the real wire
///    shape the domain types do not round-trip (the domain serializes
///    its own field names; the wire reader expects dbt's).
#[must_use]
pub fn serialize_explore_to_tmp(
    manifest: &Manifest,
    name: &str,
    node_patches: &[(String, String, Value)],
    raw_nodes: &[(String, Value)],
    top_map_entries: &[(String, String, Value)],
) -> PathBuf {
    let mut value: Value = serde_json::to_value(manifest).expect("manifest serializes to Value");
    for (bare, key, patch) in node_patches {
        let id = model_id(bare);
        value["nodes"][id.as_str()][key.as_str()] = patch.clone();
    }
    for (id, node) in raw_nodes {
        value["nodes"][id.as_str()] = node.clone();
    }
    for (map, key, entry) in top_map_entries {
        value[map.as_str()][key.as_str()] = entry.clone();
    }
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.json"));
    std::fs::write(
        &path,
        serde_json::to_string(&value).expect("injected manifest serializes"),
    )
    .expect("write explore manifest to temp file");
    path
}

// ---------------------------------------------------------------------
// Incremental-model builders (incremental_models.feature — cute-dbt#145).
// ---------------------------------------------------------------------

/// Construct a `UnitTest` for the incremental scenarios — a bare target
/// plus `given` inputs (empty rows; these scenarios assert badges, not
/// fixture content). The `overrides.macros.is_incremental` mode is NOT set
/// on the domain object: it lives only in the wire shape and is injected by
/// [`serialize_incremental_to_tmp`] (the cute-dbt#145 wire-shape divergence
/// — the domain stores the mode flat, the wire nests it under `overrides`).
#[must_use]
pub fn incremental_unit_test(name: &str, target_bare: &str, given_inputs: &[String]) -> UnitTest {
    let givens = given_inputs
        .iter()
        .map(|input| UnitTestGiven::new(input.clone(), Value::Array(Vec::new()), None, None))
        .collect();
    UnitTest::new(
        name,
        NodeId::new(target_bare),
        givens,
        UnitTestExpect::new(Value::Array(Vec::new()), None, None),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
}

/// Serialize a synthetic CURRENT manifest, injecting the two dbt WIRE
/// shapes the flat-domain serialization cannot express (cute-dbt#145):
///
/// 1. **`config.materialized`** — the domain serializes `NodeConfig` as a
///    nested `{ config: {...}, contract_enforced }` struct, but the wire
///    reader **flattens** `config`, so a domain→JSON→wire round-trip loses
///    `materialized`. Each `(bare, materialized)` entry rewrites the node's
///    `config` to dbt's flat `{ "materialized": <m> }`.
/// 2. **`overrides.macros.is_incremental`** — the domain stores the mode
///    flat (`is_incremental_mode`); the wire reads it nested under
///    `overrides`. Each `(bare test, is_incremental)` entry injects
///    `overrides.macros.is_incremental`.
///
/// Everything else (checksums, compiled_code, `given` inputs, expect)
/// round-trips natively, so the rest of the manifest comes straight from
/// the domain serialization.
#[must_use]
pub fn serialize_incremental_to_tmp(
    manifest: &Manifest,
    name: &str,
    materialized: &[(&str, &str)],
    overrides: &[(&str, bool)],
) -> PathBuf {
    let mut value: Value = serde_json::to_value(manifest).expect("manifest serializes to Value");
    for &(bare, mat) in materialized {
        let id = model_id(bare);
        value["nodes"][id.as_str()]["config"] = serde_json::json!({ "materialized": mat });
    }
    for &(test, is_incremental) in overrides {
        let key = unit_test_key(test);
        value["unit_tests"][key.as_str()]["overrides"] =
            serde_json::json!({ "macros": { "is_incremental": is_incremental } });
    }
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.json"));
    std::fs::write(
        &path,
        serde_json::to_string(&value).expect("injected manifest serializes"),
    )
    .expect("write incremental manifest to temp file");
    path
}
