//! Integration coverage for the PR 5 `StateComparator`, exercised against
//! the real jaffle-shop diff pair (baseline vs current, `stg_customers`
//! body modified) loaded through the PR 4b manifest adapter.
//!
//! This is the PR 4b → PR 5 *fixture-readiness* edge: PR 5's **tests**
//! consume PR 4b's loader to deserialize the real fixture; PR 5's
//! production code imports only `domain` types — `domain` never reaches
//! into `adapters`.

use std::path::{Path, PathBuf};

use cute4dbt::adapters::manifest::FileManifestSource;
use cute4dbt::domain::{Manifest, NodeId, StateComparator, resolve_target_model};
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

#[test]
fn body_checksum_scoping_against_the_real_jaffle_shop_diff_pair() {
    let baseline = load("jaffle-shop-baseline.json");
    let current = load("jaffle-shop-current.json");
    let comparator = StateComparator::body_only();

    let modified = comparator.modified_set(&current, &baseline);
    assert!(
        modified.contains(&NodeId::new("model.jaffle_shop.stg_customers")),
        "stg_customers' body was modified in the current fixture",
    );
    assert!(
        !modified.contains(&NodeId::new("model.jaffle_shop.customers")),
        "customers depends on stg_customers but its own body is unchanged",
    );

    let in_scope = comparator.in_scope_unit_tests(&current, &baseline);
    assert_eq!(
        in_scope.len(),
        1,
        "the jaffle-shop fixture authors exactly one unit test",
    );
    assert!(
        in_scope
            .contains("unit_test.jaffle_shop.stg_customers.test_stg_customers_renames_columns",),
        "the stg_customers unit test is in scope: its target model body changed",
    );
}

#[test]
fn resolve_target_model_maps_the_bare_unit_test_model_name() {
    // The PR 4b finding routed to PR 5: dbt records `unit_tests.<id>.model`
    // as the bare model name (`stg_customers`), not the fully-qualified
    // `model.<package>.<name>` node id.
    let current = load("jaffle-shop-current.json");
    let resolved = resolve_target_model(&current, &NodeId::new("stg_customers"))
        .expect("the bare name `stg_customers` resolves to its model node");
    assert_eq!(
        resolved.id().as_str(),
        "model.jaffle_shop.stg_customers",
        "resolution maps the bare name to the full manifest node id",
    );
}

#[test]
fn a_manifest_compared_against_itself_scopes_nothing() {
    // Empty-but-valid: identical manifests yield zero modified nodes and
    // zero in-scope unit tests. No false positives — the run loop's
    // "0 unit tests in scope" path.
    let baseline = load("jaffle-shop-baseline.json");
    let comparator = StateComparator::body_only();
    assert!(comparator.modified_set(&baseline, &baseline).is_empty());
    assert!(
        comparator
            .in_scope_unit_tests(&baseline, &baseline)
            .is_empty(),
    );
}
