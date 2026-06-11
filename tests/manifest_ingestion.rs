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
fn real_fixture_carries_column_descriptions_and_column_test_attribution() {
    // cute-dbt#165 verified against the REAL fusion-compiled jaffle-shop
    // fixture (not just synthetic JSON — fusion null-fills unset Options
    // and serializes an unset column description as `""`, shapes
    // synthetic tests miss).
    let manifest = FileManifestSource
        .load(&fixture("jaffle-shop-current.json"))
        .expect("the current fixture is a valid compiled v12 manifest");

    // Authored column description on the customers model.
    let customers = manifest
        .node(&NodeId::new("model.jaffle_shop.customers"))
        .expect("customers model present");
    assert_eq!(
        customers
            .column_descriptions()
            .get("customer_id")
            .map(String::as_str),
        Some("This is a unique identifier for a customer"),
    );

    // fusion serializes stg_customers.customer_id's UNSET description as
    // `""` — the adapter must drop it (no empty-bubble noise downstream).
    let stg_customers = manifest
        .node(&NodeId::new("model.jaffle_shop.stg_customers"))
        .expect("stg_customers model present");
    assert!(stg_customers.columns().contains_key("customer_id"));
    assert!(
        !stg_customers
            .column_descriptions()
            .contains_key("customer_id"),
        "an empty wire description must not survive ingestion",
    );

    // Column-scoped generic tests: unique + not_null on
    // stg_customers.customer_id, attributed via attached_node +
    // column_name + test_metadata.
    let mut names: Vec<&str> = manifest
        .nodes()
        .values()
        .filter(|n| {
            n.resource_type() == "test"
                && n.attached_node() == Some(&NodeId::new("model.jaffle_shop.stg_customers"))
                && n.column_name() == Some("customer_id")
        })
        .filter_map(|n| n.test_metadata().map(cute_dbt::domain::TestMetadata::name))
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["not_null", "unique"],
        "stg_customers.customer_id carries its two column-scoped tests",
    );

    // accepted_values kwargs pass through untyped (the render layer
    // summarizes `values`).
    let accepted = manifest
        .nodes()
        .values()
        .find(|n| {
            n.attached_node() == Some(&NodeId::new("model.jaffle_shop.orders"))
                && n.column_name() == Some("status")
                && n.test_metadata()
                    .is_some_and(|tm| tm.name() == "accepted_values")
        })
        .expect("the fixture carries an accepted_values test on orders.status");
    let tm = accepted.test_metadata().expect("metadata present");
    assert!(
        tm.kwargs()["values"].is_array(),
        "accepted_values kwargs carry the authored values list",
    );
}

#[test]
fn real_fixture_carries_model_description_tags_and_full_overrides() {
    // cute-dbt#200 verified against the REAL committed fixtures (the
    // fusion-first rule's second leg — synthetic JSON misses the engines'
    // unset shapes). The wire types are pinned to dbt-fusion
    // `dbt-schemas` @ 9977b6cbb1b761065536300037560d8e3c037011
    // (`ManifestMaterializableCommonAttributes.{description,tags}`,
    // `UnitTestOverrides.{env_vars,macros,vars}`).

    // --- jaffle-shop (dbt 1.11 wire): "" description + null overrides ---
    let jaffle = FileManifestSource
        .load(&fixture("jaffle-shop-current.json"))
        .expect("the current fixture is a valid compiled v12 manifest");
    let customers = jaffle
        .node(&NodeId::new("model.jaffle_shop.customers"))
        .expect("customers model present");
    assert_eq!(
        customers.description(),
        Some(
            "This table has basic information about a customer, as well as \
             some derived facts based on a customer's orders"
        ),
        "authored top-level model description ingests verbatim",
    );
    let stg_customers = jaffle
        .node(&NodeId::new("model.jaffle_shop.stg_customers"))
        .expect("stg_customers model present");
    assert!(
        stg_customers.description().is_none(),
        "the wire's empty-string unset description must be dropped",
    );
    assert!(stg_customers.tags().is_empty(), "untagged model: []");
    let jaffle_ut = jaffle
        .unit_test("unit_test.jaffle_shop.stg_customers.test_stg_customers_renames_columns")
        .expect("unit test present");
    assert!(
        jaffle_ut.overrides().is_none(),
        "the wire's explicit `\"overrides\": null` collapses to None",
    );

    // --- playground: real tags (top-level, deduplicated) + the #125
    //     overrides splice with empty sibling channels ---
    let playground = FileManifestSource
        .load(&fixture("playground-current.json"))
        .expect("the playground fixture is a valid compiled v12 manifest");
    let mart = playground
        .node(&NodeId::new("model.healthcare_analytics.mart_dq_summary"))
        .expect("mart_dq_summary present");
    assert_eq!(
        mart.tags(),
        [
            "analytics".to_owned(),
            "data_quality".to_owned(),
            "marts".to_owned()
        ],
        "the TOP-LEVEL deduplicated tags — this fixture's config.tags \
         carries the project+model merge duplicates and must not be read",
    );
    assert!(
        mart.description()
            .is_some_and(|d| d.starts_with("Data quality summary metrics")),
        "authored model description ingests",
    );
    let incremental_ut = playground
        .unit_test(
            "unit_test.healthcare_analytics.fct_encounters_incremental.\
             test_fct_encounters_incremental_appends_new_encounters",
        )
        .expect("the #125 overrides splice is present");
    let overrides = incremental_ut
        .overrides()
        .expect("populated macros group retained");
    assert_eq!(
        overrides.keys().collect::<Vec<_>>(),
        ["macros"],
        "the wire's empty `\"vars\": {{}}` / `\"env_vars\": {{}}` channels are dropped",
    );
    assert_eq!(
        overrides["macros"]["is_incremental"],
        serde_json::json!(true),
        "a native JSON bool survives ingestion untouched (never stringified)",
    );
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
