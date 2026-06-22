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
    CYTOSCAPE_JS, DATATABLES_CSS, DATATABLES_JS, JQUERY_JS, MERMAID_JS, SAKURA_CSS,
};
use cute_dbt::adapters::manifest::FileManifestSource;
use cute_dbt::adapters::render::{
    ScopeSource, build_payload, column_meta_for_model, render_report,
};
use cute_dbt::domain::{
    DEFAULT_REPORT_TITLE, InScopeSet, Manifest, ModelInScopeSet, NodeId, StateComparator,
};
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
        &HashMap::new(),
        "jaffle-shop-baseline.json",
        ScopeSource::Baseline,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
}

/// The HTML cute-dbt itself emits, with the six inlined VENDORED asset
/// bodies stripped out (the first-party report.css / interaction.js /
/// theme.js / cyto-dag.js stay — they are ours and belong in the
/// snapshot). Scanning *this* for egress constructs avoids the false
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
        CYTOSCAPE_JS,
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
        ("cytoscape", CYTOSCAPE_JS),
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

#[test]
fn source_given_binds_end_to_end_from_a_committed_fixture() {
    // cute-dbt#57 vertical, file → Stage-1 preflight → sources block →
    // payload binding: the committed synthetic fixture carries a model
    // with BOTH a ref()-based import CTE and a source()-based import
    // CTE, plus a two-dialect `sources` block (core-style explicit
    // nulls AND fusion-style absent keys — the #145 rule).
    let current = load("ref-and-source-import-cte.json");

    // The adapter parsed both dialects of the sources block.
    assert_eq!(current.sources().len(), 2);
    let patients = current
        .source_by_name("demo_raw", "patients")
        .expect("the core-style (explicit-null) source entry resolves");
    assert_eq!(
        patients.relation_name(),
        Some("\"mixed_shop\".\"raw_layer\".\"patients\"")
    );
    let encounters = current
        .source_by_name("demo_raw", "encounters")
        .expect("the fusion-style (absent-key) source entry resolves");
    assert!(encounters.identifier().is_none());
    assert!(encounters.relation_name().is_none());

    // The render payload binds each given to its own import CTE.
    let test_id = "unit_test.mixed_shop.test_stg_mixed_joins_ref_and_source";
    let model_id = NodeId::new("model.mixed_shop.stg_mixed");
    let in_scope = InScopeSet::from_iter([test_id.to_owned()]);
    let models = ModelInScopeSet::from_iter([model_id]);
    let payload = build_payload(
        &current,
        &in_scope,
        &models,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "baseline",
    );
    let test = &payload.models[0].tests[0];
    assert_eq!(
        test.given[0].bound_to_node.as_deref(),
        Some("orders"),
        "ref('raw_orders') binds to the ref-based import CTE",
    );
    assert_eq!(
        test.given[1].bound_to_node.as_deref(),
        Some("patients"),
        "source('demo_raw','patients') binds to the source-based import CTE \
         via the manifest sources block",
    );
}

#[test]
fn source_given_binds_against_the_real_playground_fixture() {
    // cute-dbt#57 REAL-fixture proof (the fusion-rule's "verify vs a real
    // committed fixture, not just synthetic JSON"): the playground's
    // dbt-core-1.11-compiled manifest carries
    // `test_stg_synthea__patients_renames_and_hashes_source_columns` with
    // `given: input: source('synthea_raw', 'patients')` on a staging
    // model whose compiled body is the canonical unwrapper shape
    // `with source as (select * from "memory"."main"."patients")`. The
    // given must bind to the `source` import CTE through the manifest
    // sources block — real engine wire, no hand-rolled shapes.
    let current = load("playground-current.json");

    // The real sources block parsed (16 synthea_raw entries) and the
    // bound entry resolves by its authored (source_name, name) pair.
    let patients = current
        .source_by_name("synthea_raw", "patients")
        .expect("the real playground sources block carries synthea_raw.patients");
    assert_eq!(patients.identifier(), Some("patients"));
    assert_eq!(
        patients.relation_name(),
        Some("\"memory\".\"main\".\"patients\""),
        "dbt-core emits the fully-quoted three-part relation",
    );

    let test_id = "unit_test.healthcare_analytics.stg_synthea__patients.\
                   test_stg_synthea__patients_renames_and_hashes_source_columns";
    let model_id = NodeId::new("model.healthcare_analytics.stg_synthea__patients");
    let unit_test = current
        .unit_test(test_id)
        .expect("the real playground fixture carries the source-given unit test");
    let source_ordinal = unit_test
        .given()
        .iter()
        .position(|g| g.input() == "source('synthea_raw', 'patients')")
        .expect("the unit test declares the source('synthea_raw', 'patients') given");

    let in_scope = InScopeSet::from_iter([test_id.to_owned()]);
    let models = ModelInScopeSet::from_iter([model_id]);
    let payload = build_payload(
        &current,
        &in_scope,
        &models,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "baseline",
    );
    let test = payload.models[0]
        .tests
        .iter()
        .find(|t| t.id == test_id)
        .expect("the payload carries the source-given unit test");
    assert_eq!(
        test.given[source_ordinal].bound_to_node.as_deref(),
        Some("source"),
        "the real source('synthea_raw','patients') given binds to the \
         `source` import CTE via the sources-block resolution",
    );
}

// ===== cute-dbt#253 — typed lineage nodes, proven on the REAL fixtures =====
//
// The explorer lineage must render snapshots/seeds/sources/exposures as
// typed DAG nodes: the pre-#253 model-only filter severed
// `stg → snapshot → downstream` into two components and faked the
// downstream model (and every source-fed staging model) as a root.
// These tests are the fixture-proven halves of the cute-dbt#253 ACs:
// the committed playground manifest carries a real mid-chain snapshot +
// 16 sources + 1 exposure; the committed jaffle-shop manifest carries 3
// seeds.

/// Every manifest entry the typed lineage renders, with its expected
/// type — models/snapshots/seeds from the `nodes` map plus the
/// `sources`/`exposures` maps.
fn renderable_ids(
    manifest: &Manifest,
) -> std::collections::HashMap<String, cute_dbt::adapters::explore::LineageNodeType> {
    use cute_dbt::adapters::explore::LineageNodeType;
    let mut ids = std::collections::HashMap::new();
    for (id, node) in manifest.nodes() {
        let node_type = match node.resource_type() {
            "model" => LineageNodeType::Model,
            "snapshot" => LineageNodeType::Snapshot,
            "seed" => LineageNodeType::Seed,
            _ => continue,
        };
        ids.insert(id.as_str().to_owned(), node_type);
    }
    for id in manifest.sources().keys() {
        ids.insert(
            id.as_str().to_owned(),
            cute_dbt::adapters::explore::LineageNodeType::Source,
        );
    }
    for id in manifest.exposures().keys() {
        ids.insert(
            id.as_str().to_owned(),
            cute_dbt::adapters::explore::LineageNodeType::Exposure,
        );
    }
    ids
}

/// Assert the lineage payload over `manifest` renders every renderable
/// manifest entry, typed, with every dependency edge between renderable
/// ids present (no severing ⇒ no false roots), and return the payload.
fn assert_lineage_complete(manifest: &Manifest) -> cute_dbt::adapters::explore::LineagePayload {
    use cute_dbt::adapters::explore::build_lineage_payload;
    use cute_dbt::domain::all_models;
    let expected = renderable_ids(manifest);
    let payload = build_lineage_payload(manifest, &all_models(manifest), None);
    // (1) node-set completeness + typing.
    let rendered: std::collections::HashMap<&str, _> = payload
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.node_type))
        .collect();
    for (id, node_type) in &expected {
        assert_eq!(
            rendered.get(id.as_str()),
            Some(node_type),
            "{id} must render as a typed lineage node",
        );
    }
    assert_eq!(
        rendered.len(),
        expected.len(),
        "the lineage renders exactly the renderable manifest entries",
    );
    // (2) edge completeness: every manifest dependency between two
    // renderable ids is a payload edge — the no-severing invariant
    // (a missing edge is what manufactured the false roots).
    let edges: std::collections::HashSet<(&str, &str)> = payload
        .edges
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect();
    let mut checked = 0usize;
    let require_edge = |from: &str, to: &str| {
        assert!(
            edges.contains(&(from, to)),
            "manifest dependency {from} -> {to} must be a lineage edge",
        );
    };
    for (id, node) in manifest.nodes() {
        if !expected.contains_key(id.as_str()) {
            continue;
        }
        for dep in node.depends_on().nodes() {
            if expected.contains_key(dep.as_str()) {
                require_edge(dep.as_str(), id.as_str());
                checked += 1;
            }
        }
    }
    for (id, exposure) in manifest.exposures() {
        for dep in exposure.depends_on().nodes() {
            if expected.contains_key(dep.as_str()) {
                require_edge(dep.as_str(), id.as_str());
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "the fixture must exercise at least one edge");
    payload
}

#[test]
fn playground_lineage_renders_the_snapshot_mid_chain_with_no_false_roots() {
    let current = load("playground-current.json");
    let payload = assert_lineage_complete(&current);
    // The cute-dbt#253 discovery chain, by id: the snapshot sits
    // MID-CHAIN and both its edges survive.
    let edges: std::collections::HashSet<(&str, &str)> = payload
        .edges
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect();
    assert!(edges.contains(&(
        "model.healthcare_analytics.stg_synthea__patients",
        "snapshot.healthcare_analytics.snp_patients",
    )));
    assert!(edges.contains(&(
        "snapshot.healthcare_analytics.snp_patients",
        "model.healthcare_analytics.dim_patients",
    )));
    // dim_patients is fed by the snapshot — NOT a false root.
    assert!(
        payload
            .edges
            .iter()
            .any(|e| e.to == "model.healthcare_analytics.dim_patients"),
        "dim_patients must have an incoming edge (the severed-chain defect)",
    );
    // The 16 sources render as roots; the exposure as a sink.
    let sources = payload
        .nodes
        .iter()
        .filter(|n| n.node_type == cute_dbt::adapters::explore::LineageNodeType::Source)
        .count();
    assert_eq!(sources, 16, "every playground source renders");
    let exposure_id = "exposure.healthcare_analytics.provider_quality_dashboard";
    assert!(
        payload.edges.iter().any(|e| e.to == exposure_id),
        "the exposure terminates a lineage chain",
    );
    assert!(
        !payload.edges.iter().any(|e| e.from == exposure_id),
        "an exposure is a sink",
    );
}

#[test]
fn jaffle_shop_lineage_renders_seeds_as_typed_roots() {
    use cute_dbt::adapters::explore::LineageNodeType;
    let current = load("jaffle-shop-current.json");
    let payload = assert_lineage_complete(&current);
    let seeds: Vec<&str> = payload
        .nodes
        .iter()
        .filter(|n| n.node_type == LineageNodeType::Seed)
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(seeds.len(), 3, "the jaffle-shop fixture carries 3 seeds");
    for seed in seeds {
        assert!(
            payload.edges.iter().any(|e| e.from == seed),
            "{seed} feeds at least one staging model",
        );
        assert!(
            !payload.edges.iter().any(|e| e.to == seed),
            "{seed} is a root (no incoming edges)",
        );
        assert!(
            !payload
                .nodes
                .iter()
                .find(|n| n.id == seed)
                .expect("seed node present")
                .not_compiled,
            "{seed} never renders dbt-parse-dashed (fusion null-fills \
             seed compiled_code unconditionally)",
        );
    }
}

// ── cute-dbt#464 (Z3): the dogfood RAW-ZONE VISIBILITY contract ──
//
// Z2 (#448) landed the scanner + projection with synthetic UNIT tests. Z3's
// dogfood-alongside-every-feature obligation is that the zone map is VISIBLE in
// a COMMITTED artifact: the playground manifest's `fct_encounters_incremental`
// carries BOTH a Shape-A `{% for %}` loop INSIDE one CTE body AND a
// `{% if is_incremental() %}` guard, so the regenerated playground golden
// surfaces a real `code_map.raw_zones[]` with both zone kinds. This test pins
// that the renderer projects them through the SAME public `ReportPayload` the
// golden inlines — a mechanically-present-but-invisible feature would pass the
// #448 unit tests yet fail HERE (L9: the claim is scoped to Shape A + the
// incremental guard; Shape-B N-CTE is out of scope).
#[test]
fn playground_dogfood_model_surfaces_both_raw_zone_kinds() {
    use cute_dbt::adapters::render::RawZonePayload;
    use cute_dbt::domain::ZoneKind;

    let current = load("playground-current.json");
    let baseline = load("playground-baseline.json");
    let comparator = StateComparator::body_only();
    let in_scope = comparator.in_scope_unit_tests(&current, &baseline);
    let models_in_scope = comparator.models_in_scope(&current, &baseline);
    let payload = build_payload(
        &current,
        &in_scope,
        &models_in_scope,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "playground-baseline.json",
    );

    // The dogfood model is a NEW (baseline-absent) incremental model, so it is
    // in scope and renders its `code_map`.
    let model = payload
        .models
        .iter()
        .find(|m| m.name == "fct_encounters_incremental")
        .expect("the dogfood incremental model renders in the playground report");
    let code_map = model
        .code_map
        .as_ref()
        .expect("a compiled model carries a code_map");
    let zones: &[RawZonePayload] = &code_map.raw_zones;
    assert_eq!(
        zones.len(),
        2,
        "the dogfood model surfaces exactly two zones (Shape-A for-loop + \
         incremental guard) — got {:?}",
        zones
            .iter()
            .map(|z| (z.kind, z.presence))
            .collect::<Vec<_>>(),
    );

    // The Shape-A `{% for %}` loop expands columns INSIDE the `source` CTE: its
    // compiled tokens land STRICTLY nested in one CteBody span → Structural,
    // bound to the containing node (never compiled_out, never a fabricated
    // 1→1 edge to a sibling CTE).
    let for_loop = zones
        .iter()
        .find(|z| z.kind == ZoneKind::ForLoop)
        .expect("the Shape-A for-loop zone is visible in the golden");
    assert_eq!(
        for_loop.presence, "structural",
        "a loop inside one CTE body is Structural (L9 Shape A)",
    );
    assert_eq!(
        for_loop.node_id.as_deref(),
        Some("source"),
        "the Structural for-loop binds to its containing CTE node",
    );
    assert!(
        code_map.node_spans.contains_key("source"),
        "the bound node has a real CteBody span (the Structural contains_range \
         anchor)",
    );

    // The `{% if is_incremental() %}` guard is the second visible zone: in this
    // manifest its tokens compiled IN as a terminal WHERE, so it is located and
    // bound (never a false claim either direction).
    let guard = zones
        .iter()
        .find(|z| z.kind == ZoneKind::IncrementalGuard)
        .expect("the incremental-guard zone is visible in the golden");
    assert!(
        guard.presence == "structural" || guard.presence == "compiled_in",
        "the located guard is bound to a node, never a false compiled_out \
         (got {})",
        guard.presence,
    );
    assert!(
        guard.node_id.is_some(),
        "a located guard carries its owning node (compiled: Some ⇒ an edge)",
    );
}
