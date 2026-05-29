//! Integration coverage for the PR 4b manifest ingestion adapter,
//! exercised through the real-file [`FileManifestSource`] port impl
//! against the PR 4a jaffle-shop fixtures.
//!
//! The golden `insta` snapshot is a **deterministic projection** of the
//! deserialized `Manifest`, not the whole 580 KB structure: a full
//! snapshot would lock the *fixture* (485 macro bodies, every compiled
//! SQL string) rather than the *wire→domain translation*, churn on
//! every fixture re-sanitization, and be non-deterministic (`HashMap`
//! iteration order). The projection sorts nodes by id and unit tests by
//! key into `BTreeMap`s, and records macro provenance as a scalar count
//! (the macro-body→string reduction is locked precisely by a unit test
//! in `adapters::manifest`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cute_dbt::adapters::manifest::{FileManifestSource, load_baseline};
use cute_dbt::domain::{Manifest, NodeId, PreflightError, UnitTest};
use cute_dbt::ports::ManifestSource;

/// Absolute path to a committed fixture under `tests/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// One node, reduced to the fields the wire→domain translation owns.
#[derive(serde::Serialize)]
struct NodeDigest {
    id: String,
    resource_type: String,
    checksum_name: String,
    checksum: String,
    has_compiled_code: bool,
    depends_on_nodes: Vec<String>,
}

/// A deterministic projection of a `Manifest` — the golden-snapshot
/// shape. `nodes` is sorted by id; `unit_tests` is a key-sorted
/// `BTreeMap`; macros are a scalar count.
#[derive(serde::Serialize)]
struct ManifestDigest<'a> {
    schema_version: String,
    node_count: usize,
    macro_count: usize,
    unit_test_count: usize,
    nodes: Vec<NodeDigest>,
    unit_tests: BTreeMap<String, &'a UnitTest>,
}

impl<'a> ManifestDigest<'a> {
    fn of(manifest: &'a Manifest) -> Self {
        let mut nodes: Vec<NodeDigest> = manifest
            .nodes()
            .values()
            .map(|node| {
                let mut deps: Vec<String> = node
                    .depends_on()
                    .nodes()
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .collect();
                deps.sort();
                NodeDigest {
                    id: node.id().as_str().to_owned(),
                    resource_type: node.resource_type().to_owned(),
                    checksum_name: node.checksum().name().to_owned(),
                    checksum: node.checksum().checksum().to_owned(),
                    has_compiled_code: node.compiled_code().is_some(),
                    depends_on_nodes: deps,
                }
            })
            .collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));

        let unit_tests: BTreeMap<String, &UnitTest> = manifest
            .unit_tests()
            .iter()
            .map(|(key, ut)| (key.clone(), ut))
            .collect();

        Self {
            schema_version: manifest.metadata().dbt_schema_version().to_owned(),
            node_count: manifest.nodes().len(),
            macro_count: manifest.macros().len(),
            unit_test_count: manifest.unit_tests().len(),
            nodes,
            unit_tests,
        }
    }
}

#[test]
fn golden_baseline_manifest_digest() {
    let manifest = FileManifestSource
        .load(&fixture("jaffle-shop-baseline.json"))
        .expect("the baseline fixture is a valid compiled v12 manifest");

    // Explicit, human-readable witnesses alongside the snapshot.
    assert_eq!(manifest.unit_tests().len(), 1, "one authored unit test");
    assert!(
        !manifest.macros().is_empty(),
        "dbt ships hundreds of built-in macros — the macro map must populate",
    );

    let digest = ManifestDigest::of(&manifest);
    let rendered = serde_json::to_string_pretty(&digest).expect("digest serializes");
    insta::assert_snapshot!(rendered);
}

#[test]
fn baseline_and_current_form_the_modified_stg_customers_diff_pair() {
    // PR 5's StateComparator diffs node body checksums; PR 4b must carry
    // that signal through translation intact. The fixtures are a pair:
    // the same project with stg_customers' body modified.
    let stg_customers = NodeId::new("model.jaffle_shop.stg_customers");

    let baseline = FileManifestSource
        .load(&fixture("jaffle-shop-baseline.json"))
        .expect("baseline loads");
    let current = FileManifestSource
        .load(&fixture("jaffle-shop-current.json"))
        .expect("current loads");

    let baseline_checksum = baseline
        .node(&stg_customers)
        .expect("baseline has stg_customers")
        .checksum()
        .checksum()
        .to_owned();
    let current_checksum = current
        .node(&stg_customers)
        .expect("current has stg_customers")
        .checksum()
        .checksum()
        .to_owned();

    assert_ne!(
        baseline_checksum, current_checksum,
        "the diff-pair signal: stg_customers' body checksum must differ",
    );
}

#[test]
fn parse_only_fixture_loads_with_every_model_uncompiled() {
    // A `dbt parse` manifest is valid Stage-1 input — it deserializes
    // and passes the schema floor. The compiled-SQL-presence check is
    // Stage-2 (PR 6), run only after scoping; Stage-1 must not reject it.
    let manifest = FileManifestSource
        .load(&fixture("jaffle-shop-parse-only.json"))
        .expect("a parse-only manifest still passes Stage-1");

    let models: Vec<&NodeId> = manifest
        .nodes()
        .iter()
        .filter(|(_, node)| node.resource_type() == "model")
        .map(|(id, _)| id)
        .collect();
    assert!(!models.is_empty(), "the fixture has model nodes");

    for id in models {
        let node = manifest.node(id).expect("listed node resolves");
        assert!(
            node.compiled_code().is_none(),
            "parse-only: model {id} must have compiled_code: null",
        );
    }
}

#[test]
fn file_manifest_source_reports_a_missing_file_as_unreadable() {
    let err = FileManifestSource
        .load(Path::new("/no/such/manifest.json"))
        .expect_err("a missing file cannot be loaded");
    match err {
        PreflightError::Unreadable { detail } => {
            assert!(
                detail.contains("manifest.json"),
                "the detail names the offending path: {detail}",
            );
        }
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

#[test]
fn load_baseline_with_the_file_source_remaps_a_missing_file() {
    let err = load_baseline(&FileManifestSource, Path::new("/no/such/baseline.json"))
        .expect_err("a missing baseline cannot be used");
    assert!(
        matches!(err, PreflightError::BaselineUnusable { .. }),
        "a missing baseline file is BaselineUnusable, not Unreadable: {err:?}",
    );
}
