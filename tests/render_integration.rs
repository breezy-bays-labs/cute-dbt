//! Integration coverage for the askama renderer, exercised against a
//! real compiled jaffle-shop manifest.
//!
//! The test loads the fixture through the manifest adapter, computes
//! scope via [`StateComparator`], and asserts the rendered HTML carries
//! the inlined asset bundle, the expected DOM contract, and emits no
//! external resource-loading constructs (the secondary zero-egress
//! guard alongside the headless-browser network-block test tracked
//! separately).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cute_dbt::adapters::asset_embed::{
    DATATABLES_CSS, DATATABLES_JS, JQUERY_JS, MERMAID_JS, SAKURA_CSS,
};
use cute_dbt::adapters::manifest::FileManifestSource;
use cute_dbt::adapters::render::{ScopeSource, column_meta_for_model, render_report};
use cute_dbt::domain::{DEFAULT_REPORT_TITLE, Manifest, NodeId, StateComparator};
use cute_dbt::ports::ManifestSource;

/// Absolute path to a committed fixture under `tests/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A path inside the cargo-provided integration-test temp directory.
fn tmp(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn load(name: &str) -> Manifest {
    FileManifestSource
        .load(&fixture(name))
        .unwrap_or_else(|err| panic!("fixture {name} is a valid v12 manifest: {err:?}"))
}

/// Render the jaffle-shop current-vs-baseline fixture pair to `out`.
fn render_jaffle_shop(out: &Path) {
    let current = load("jaffle-shop-current.json");
    let baseline = load("jaffle-shop-baseline.json");
    let comparator = StateComparator::body_only();
    let in_scope = comparator.in_scope_unit_tests(&current, &baseline);
    let models_in_scope = comparator.models_in_scope(&current, &baseline);
    let changed = StateComparator::changed_unit_tests(&current, &baseline);
    render_report(
        out,
        &current,
        &in_scope,
        &models_in_scope,
        &changed,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "jaffle-shop-baseline.json",
        ScopeSource::Baseline,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
}

/// The HTML cute-dbt itself emits, with the five inlined asset bodies
/// stripped out. Scanning *this* for egress constructs avoids the false
/// positives the minified bundles' inert URL literals would otherwise
/// produce (`ARCHITECTURE.md` §5).
fn chrome_only(html: &str) -> String {
    let mut chrome = html.to_owned();
    for asset in [
        SAKURA_CSS,
        DATATABLES_CSS,
        JQUERY_JS,
        DATATABLES_JS,
        MERMAID_JS,
    ] {
        chrome = chrome.replace(asset, "<<inlined-asset>>");
    }
    chrome
}

#[test]
fn the_real_renderer_bundles_every_asset_for_a_real_fixture() {
    let out = tmp("integration_inlining.html");
    let _ = std::fs::remove_file(&out);
    render_jaffle_shop(&out);
    let html = std::fs::read_to_string(&out).expect("report exists");
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
fn the_real_renderer_emits_the_design_dom_contract() {
    let out = tmp("integration_dom.html");
    let _ = std::fs::remove_file(&out);
    render_jaffle_shop(&out);
    let html = std::fs::read_to_string(&out).expect("report exists");
    // The skeleton sections the design's interaction.js populates at boot.
    // Match `class="<...>foo<...>"` substring forms — sections may carry
    // multi-class lists (e.g. `class="panel expected-panel"`).
    assert!(html.contains("\"report-header\""), "report-header class");
    assert!(
        html.contains("\"diff-scope-banner\""),
        "diff-scope-banner class"
    );
    assert!(html.contains("\"test-selection\""), "test-selection class");
    assert!(html.contains("\"cte-dag\""), "cte-dag class");
    assert!(html.contains("\"panel-row\""), "panel-row class");
    assert!(html.contains("expected-panel"), "expected-panel class");
    assert!(html.contains("id=\"model-select\""), "model selector");
    assert!(html.contains("id=\"test-select\""), "test selector");
    assert!(
        html.contains("id=\"cute-dbt-data\""),
        "JSON payload carrier"
    );
    assert!(html.contains("href=\"data:,\""), "favicon is data: URI");
    assert!(
        html.contains("jaffle-shop-baseline.json"),
        "baseline label rendered"
    );
}

#[test]
fn the_real_renderer_payload_carries_an_in_scope_model_with_its_tests() {
    let out = tmp("integration_payload.html");
    let _ = std::fs::remove_file(&out);
    render_jaffle_shop(&out);
    let html = std::fs::read_to_string(&out).expect("report exists");
    // Extract the JSON between the carrier script's opening and closing
    // tags; if either is missing the slice is empty and the assertions
    // below fail with a helpful message.
    let start_tag = "<script type=\"application/json\" id=\"cute-dbt-data\">";
    let end_tag = "</script>";
    let start = html.find(start_tag).expect("payload carrier opens") + start_tag.len();
    let after_start = &html[start..];
    let end = after_start.find(end_tag).expect("payload carrier closes");
    let json = &after_start[..end];
    let value: serde_json::Value =
        serde_json::from_str(json).unwrap_or_else(|err| panic!("payload parses: {err}\n{json}"));
    assert!(
        value.get("baseline").is_some(),
        "payload carries baseline label",
    );
    let models = value
        .get("models")
        .and_then(|m| m.as_array())
        .expect("models array");
    assert!(!models.is_empty(), "at least one model in scope");
    // The jaffle-shop fixture pair modifies one model and ships an
    // in-scope unit test for it; that model carries a populated DAG and
    // a non-empty `tests` list. Don't pin the model name — the fixture
    // is a maintained-elsewhere artifact and renaming it should not
    // break this contract.
    let with_tests = models
        .iter()
        .find(|m| {
            m.get("tests")
                .and_then(|t| t.as_array())
                .is_some_and(|t| !t.is_empty())
        })
        .expect("at least one in-scope model carries its tests");
    assert!(
        with_tests.get("dag").is_some(),
        "in-scope model carries its DAG",
    );
    let model_name = with_tests
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("(unnamed)");
    let test_count = with_tests
        .get("tests")
        .and_then(|t| t.as_array())
        .map_or(0, Vec::len);
    assert!(
        test_count >= 1,
        "model {model_name} carries {test_count} in-scope unit test(s) — expected ≥1",
    );
    // cute-dbt#91 — every rendered test carries the additive boolean
    // `changed` (updated-vs-context) flag. Fixture-agnostic: assert the
    // field's presence + type, not a specific value (the manifest pair is
    // a maintained-elsewhere artifact).
    let tests_arr = with_tests
        .get("tests")
        .and_then(|t| t.as_array())
        .expect("tests array");
    assert!(
        tests_arr.iter().all(|t| t
            .get("changed")
            .and_then(serde_json::Value::as_bool)
            .is_some()),
        "every rendered test carries a boolean `changed` flag (cute-dbt#91)",
    );
}

#[test]
fn the_rendered_chrome_is_stable_for_a_known_fixture() {
    // Insta golden snapshot of the rendered HTML's CHROME — the askama-
    // emitted DOM + JSON payload + interaction.js + selectors — with the
    // five inlined asset bodies stripped before snapshotting. The
    // snapshot's job is to lock the template + payload shape so an
    // accidental DOM/class/JS regression is visible; asset edits churn
    // `assets/MANIFEST.toml`, not this file.
    let out = tmp("integration_snapshot.html");
    let _ = std::fs::remove_file(&out);
    render_jaffle_shop(&out);
    let html = std::fs::read_to_string(&out).expect("report exists");
    let chrome = chrome_only(&html);
    // `cargo insta` writes the .snap; reviewers approve the diff.
    insta::assert_snapshot!("rendered_chrome_jaffle_shop", chrome);
}

#[test]
fn the_real_renderer_emits_no_external_resource_constructs() {
    // Local belt-and-braces guard for the zero-egress invariant. The
    // canonical proof is the structured resource-ref lint plus the
    // headless-browser network-block test tracked at
    // `breezy-bays-labs/cute-dbt#12`; this test is the fast fixture-
    // backed signal that runs on every `cargo test`.
    let out = tmp("integration_egress.html");
    let _ = std::fs::remove_file(&out);
    render_jaffle_shop(&out);
    let html = std::fs::read_to_string(&out).expect("report exists");
    let chrome = chrome_only(&html);
    assert!(!chrome.contains("<script src"), "no <script src> in chrome");
    assert!(!chrome.contains("<link href"), "no <link href> in chrome");
    assert!(
        !chrome.contains("<img"),
        "no <img> in chrome (we emit no images)",
    );
    assert!(!chrome.contains(" src=\""), "no src= attribute in chrome");
    assert!(!chrome.contains("@import"), "no CSS @import in chrome");
    assert!(!chrome.contains("url("), "no CSS url() in chrome");
    assert!(!chrome.contains("http://"), "no http URL in chrome");
    assert!(!chrome.contains("https://"), "no https URL in chrome");
    assert!(!chrome.contains("\"//"), "no protocol-relative reference");
    // The only href in the chrome is the empty data: favicon.
    assert_eq!(
        chrome.matches("href=").count(),
        1,
        "exactly one href: {chrome}"
    );
    assert!(chrome.contains("href=\"data:,\""), "favicon is a data: URI");
}

#[test]
fn column_meta_matches_the_handoff_mapping_against_the_real_fusion_fixture() {
    // cute-dbt#179 AC2 — verify the handoff README §2.2 column-test
    // display mapping against a REAL committed fixture (the
    // fusion-compiled playground manifest), not just synthetic
    // TestMetadata literals. Fusion's real generic-test kwargs carry
    // EXTRA keys the synthetic tests omit (`column_name`, `model:
    // "{{ get_where_subquery(ref('…')) }}"`); this pins that the mapping
    // reads only the keys it summarizes and tolerates the rest.
    let current = load("playground-current.json");
    let patients = current
        .nodes()
        .get(&NodeId::new(
            "model.healthcare_analytics.stg_synthea__patients",
        ))
        .expect("the playground fixture carries stg_synthea__patients");
    let meta = column_meta_for_model(&current, patients);

    // gender — described + three column tests, covering three §2.2 arms
    // against real fusion payloads: bare built-in, accepted_values
    // pills, and a package test left as its package-qualified raw
    // identifier (dbt_expectations args stay uninterpreted).
    let gender = meta.get("gender").expect("gender carries column meta");
    assert_eq!(
        gender.description.as_deref(),
        Some("Patient gender (M or F)")
    );
    let gender_tests: Vec<(&str, &[String], Option<&str>)> = gender
        .tests
        .iter()
        .map(|t| (t.name.as_str(), t.values.as_slice(), t.detail.as_deref()))
        .collect();
    assert!(
        gender_tests.contains(&("not null", &[][..], None)),
        "gender lists the bare not_null built-in; got {gender_tests:?}",
    );
    let m = "M".to_owned();
    let f = "F".to_owned();
    assert!(
        gender_tests.contains(&("accepted values", &[m, f][..], None)),
        "gender's real accepted_values kwargs render as the two pills; got {gender_tests:?}",
    );
    assert!(
        gender_tests.contains(&(
            "dbt_expectations.expect_column_values_to_match_regex",
            &[][..],
            None
        )),
        "a real package test keeps its package-qualified raw name, no detail; got {gender_tests:?}",
    );

    // patient_id — unique + not_null, prose display names.
    let patient_id = meta
        .get("patient_id")
        .expect("patient_id carries column meta");
    let patient_names: Vec<&str> = patient_id.tests.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        patient_names,
        vec!["not null", "unique"],
        "patient_id lists its two built-ins under their §2.2 prose names",
    );

    // birth_date — the real project expresses range checks via
    // dbt_expectations.expect_column_values_to_be_between, the §2.2
    // near-miss of accepted_range: it must NOT be summarized into a
    // range detail (open-ended arg vocabulary), only name-qualified.
    let birth_date = meta
        .get("birth_date")
        .expect("birth_date carries column meta");
    let between = birth_date
        .tests
        .iter()
        .find(|t| t.name == "dbt_expectations.expect_column_values_to_be_between")
        .expect("birth_date carries the real package range test");
    assert!(
        between.values.is_empty() && between.detail.is_none(),
        "expect_column_values_to_be_between stays uninterpreted (no pills, no detail)",
    );

    // The model-level row-count test (column_name: null) appears in NO
    // column entry — column-scoped tests only (the v1 scope line).
    assert!(
        meta.values()
            .flat_map(|m| &m.tests)
            .all(|t| { t.name != "dbt_expectations.expect_table_row_count_to_be_between" }),
        "a model-level test (column_name null) never lands in column meta",
    );

    // relationships — real fusion `to: "ref('stg_synthea__organizations')"`
    // unwraps to the bare model name and joins the field (§2.2:
    // `"model.field"`).
    let providers = current
        .nodes()
        .get(&NodeId::new(
            "model.healthcare_analytics.stg_synthea__providers",
        ))
        .expect("the playground fixture carries stg_synthea__providers");
    let providers_meta = column_meta_for_model(&current, providers);
    let org = providers_meta
        .get("organization_id")
        .expect("organization_id carries column meta");
    let rel = org
        .tests
        .iter()
        .find(|t| t.name == "relationships")
        .expect("organization_id carries the relationships test");
    assert_eq!(
        rel.detail.as_deref(),
        Some("stg_synthea__organizations.organization_id"),
        "relationships detail is \"model.field\" with the real ref('…') unwrapped",
    );
    assert!(rel.values.is_empty(), "relationships carries no pills");
}
