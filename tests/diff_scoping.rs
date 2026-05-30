//! Integration coverage for the PR 5 `StateComparator`, exercised against
//! the real jaffle-shop diff pair (baseline vs current, `stg_customers`
//! body modified) loaded through the PR 4b manifest adapter.
//!
//! This is the PR 4b → PR 5 *fixture-readiness* edge: PR 5's **tests**
//! consume PR 4b's loader to deserialize the real fixture; PR 5's
//! production code imports only `domain` types — `domain` never reaches
//! into `adapters`.

use std::path::{Path, PathBuf};

use cute_dbt::adapters::manifest::FileManifestSource;
use cute_dbt::domain::{Manifest, NodeId, StateComparator, resolve_target_model};
use cute_dbt::ports::ManifestSource;

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

#[test]
fn with_sub_selectors_has_no_false_positives_on_an_identical_real_manifest() {
    // cute-dbt#17 — the full v0.1 + v0.2 comparator over a real manifest
    // compared against itself: the `.configs` / `.relation` / `.macros` /
    // `.contract` modifiers parse the real config / relation_name /
    // depends_on.macros / columns blocks and flag NOTHING when nothing
    // changed. Guards against a modifier that spuriously reports identity
    // as a change (e.g. an unstable map ordering leaking through).
    let baseline = load("jaffle-shop-baseline.json");
    let comparator = StateComparator::with_sub_selectors();
    assert!(
        comparator.modified_set(&baseline, &baseline).is_empty(),
        "no sub-selector fires on a manifest compared against itself",
    );
}

#[test]
fn with_sub_selectors_is_a_superset_of_body_only_on_the_real_diff() {
    // Union semantics over the real diff pair: registering the four v0.2
    // sub-selectors alongside the body modifier can only ADD to (never
    // remove from) the modified set body_only() produces. Every node
    // body_only flags is still flagged with sub-selectors registered.
    let baseline = load("jaffle-shop-baseline.json");
    let current = load("jaffle-shop-current.json");

    let body = StateComparator::body_only().modified_set(&current, &baseline);
    let wide = StateComparator::with_sub_selectors().modified_set(&current, &baseline);

    for id in body.iter() {
        assert!(
            wide.contains(id),
            "with_sub_selectors must be a superset of body_only ({id} dropped)",
        );
    }
    // The body-modified stg_customers stays in scope under the wider set.
    assert!(wide.contains(&NodeId::new("model.jaffle_shop.stg_customers")));
}
