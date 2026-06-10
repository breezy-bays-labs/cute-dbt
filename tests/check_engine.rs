//! Real-fixture verification of the check engine (cute-dbt#169).
//!
//! The unit tests in `src/domain/checks.rs` drive the detector over
//! synthetic nodes; this suite runs the SAME pipeline over the committed
//! fusion-compiled `playground-current.json` fixture, through the real
//! Stage-1 manifest adapter — the working-rules requirement that every
//! dbt wire shape is verified against a real fixture, never just
//! synthetic JSON (fusion null-fills unset `Option` fields and emits
//! shapes synthetic tests miss).
//!
//! The fixture carries every shape the detector consumes:
//! a string `unique_key` (`fct_encounters_incremental`), a composite
//! list key (`fct_encounters_monthly`), `unique` test nodes, and a
//! `dbt_utils.unique_combination_of_columns` node (`fct_patient_summary`)
//! — plus models with NO unique_key and an explicit-`null` config arm.

use std::path::{Path, PathBuf};

use cute_dbt::domain::{Finding, HeuristicId, Manifest, NodeId, Verdict, model_findings};
use cute_dbt::ports::ManifestSource;

/// Absolute path to a committed fixture under `tests/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Load a committed fixture through the real Stage-1 adapter.
fn load(name: &str) -> Manifest {
    cute_dbt::adapters::manifest::FileManifestSource
        .load(&fixture(name))
        .expect("committed fixture passes Stage-1 preflight")
}

/// Run the engine pipeline on one model of `manifest`.
fn findings_for(manifest: &Manifest, model_id: &str) -> Vec<Finding<HeuristicId>> {
    let model = manifest
        .node(&NodeId::new(model_id))
        .expect("model exists in fixture");
    model_findings(manifest, model)
}

#[test]
fn playground_composite_key_with_no_uniqueness_test_is_uncovered() {
    // fct_encounters_monthly declares unique_key = ["year_month",
    // "encounter_class"] and the fixture carries NO uniqueness test on
    // it — the real-data UNCOVERED case.
    let manifest = load("playground-current.json");
    let findings = findings_for(
        &manifest,
        "model.healthcare_analytics.fct_encounters_monthly",
    );
    assert_eq!(findings.len(), 1);
    let finding = &findings[0];
    assert_eq!(finding.check, HeuristicId::GrainUniqueKeyUnbacked);
    assert_eq!(finding.verdict, Verdict::Uncovered);
    assert_eq!(finding.evidence[0].value, "year_month, encounter_class");
    assert!(finding.recommendation.is_some());
}

#[test]
fn playground_string_key_with_unique_test_is_covered_with_attribution() {
    // fct_encounters_incremental: unique_key = "encounter_id" (string
    // wire form) + an enabled `unique` test on encounter_id.
    let manifest = load("playground-current.json");
    let findings = findings_for(
        &manifest,
        "model.healthcare_analytics.fct_encounters_incremental",
    );
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].verdict,
        Verdict::Covered {
            by: vec![
                "test.healthcare_analytics.unique_fct_encounters_incremental_encounter_id.a165c01d01"
                    .to_owned(),
            ],
        },
    );
}

#[test]
fn playground_combination_test_stays_composite_on_real_data() {
    // fct_patient_summary: unique_key = "patient_summary_key". The
    // fixture ALSO carries a dbt_utils.unique_combination_of_columns
    // over {patient_id, year_actual} — a column set NOT ⊆ the key.
    // fusion's PK inference flattens that combo per column; cute-dbt
    // must not: coverage is attributed ONLY to the `unique` test on the
    // key column, never to the wider combo.
    let manifest = load("playground-current.json");
    let findings = findings_for(&manifest, "model.healthcare_analytics.fct_patient_summary");
    assert_eq!(findings.len(), 1);
    let Verdict::Covered { by } = &findings[0].verdict else {
        panic!("expected Covered, got {:?}", findings[0].verdict);
    };
    assert_eq!(
        by,
        &vec![
            "test.healthcare_analytics.unique_fct_patient_summary_patient_summary_key.e532e1ff45"
                .to_owned(),
        ],
        "the wider combination test must NOT attribute coverage at this grain",
    );
}

#[test]
fn jaffle_shop_models_without_unique_key_carry_no_findings() {
    // The jaffle-shop fixture declares no unique_key anywhere (its
    // configs carry the explicit-null fusion arm) — zero findings on
    // every model.
    let manifest = load("jaffle-shop-current.json");
    for (id, node) in manifest.nodes() {
        if node.resource_type() == "model" {
            assert!(
                model_findings(&manifest, node).is_empty(),
                "unexpected finding on {id}"
            );
        }
    }
}

#[test]
fn playground_findings_payload_snapshot() {
    // Pin the exact serialized findings JSON for the three interesting
    // real-fixture models — the payload contract cute-dbt#170's render
    // surface will consume.
    let manifest = load("playground-current.json");
    let mut snapshot = serde_json::Map::new();
    for model_id in [
        "model.healthcare_analytics.fct_encounters_monthly",
        "model.healthcare_analytics.fct_encounters_incremental",
        "model.healthcare_analytics.fct_patient_summary",
    ] {
        let findings = findings_for(&manifest, model_id);
        snapshot.insert(
            model_id.to_owned(),
            serde_json::to_value(findings).expect("findings serialize"),
        );
    }
    let rendered = serde_json::to_string_pretty(&serde_json::Value::Object(snapshot))
        .expect("snapshot value serializes");
    insta::assert_snapshot!("playground_unique_key_findings", rendered);
}
