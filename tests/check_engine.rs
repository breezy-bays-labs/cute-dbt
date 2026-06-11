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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use cute_dbt::adapters::cte_engine::parse_cte_graph;
use cute_dbt::adapters::render::build_payload_with_externals;
use cute_dbt::domain::{
    CheckId, CheckPolicy, ChecksConfig, Checksum, DependsOn, Finding, HeuristicId, InScopeSet,
    Manifest, ManifestMetadata, ModelInScopeSet, Node, NodeConfig, NodeId, SuppressRule,
    SuppressionSource, Verdict, model_findings, resolve_check_policy,
};
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

/// Run the engine pipeline on one model of `manifest`, with the model's
/// real CTE graph parsed from its fixture `compiled_code` — the same
/// single-parse pass the renderer threads into the engine
/// (cute-dbt#172).
fn findings_for(manifest: &Manifest, model_id: &str) -> Vec<Finding<HeuristicId>> {
    let model = manifest
        .node(&NodeId::new(model_id))
        .expect("model exists in fixture");
    let graph = parse_cte_graph(model.compiled_code().unwrap_or_default()).unwrap_or_default();
    model_findings(manifest, model, Some(&graph))
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
fn jaffle_shop_models_carry_no_grain_or_union_findings() {
    // The jaffle-shop fixture declares no unique_key anywhere (its
    // configs carry the explicit-null fusion arm) AND no UNION-bearing
    // model — zero grain/union findings on every model, even with the
    // real graph threaded through. (The cute-dbt#173 join pair DOES
    // fire on its LEFT JOINs — pinned separately below.)
    let manifest = load("jaffle-shop-current.json");
    for (id, node) in manifest.nodes() {
        if node.resource_type() == "model" {
            let non_join: Vec<Finding<HeuristicId>> = findings_for(&manifest, id.as_str())
                .into_iter()
                .filter(|f| f.check.spec().group != "join")
                .collect();
            assert!(non_join.is_empty(), "unexpected finding on {id}");
        }
    }
}

#[test]
fn jaffle_shop_customers_left_joins_fire_the_left_null_check() {
    // Real-fixture verification of join.left-null-propagation
    // (cute-dbt#173): the customers model LEFT-JOINs CTE chains whose
    // right-side columns reach the projection directly. Two constructs
    // bind statically (UNCOVERED — the fixture has no unit test
    // exercising a no-match row); the customer_payments side of the
    // final join is a non-simple aggregate chain — honest UNKNOWN.
    let manifest = load("jaffle-shop-current.json");
    let findings = findings_for(&manifest, "model.jaffle_shop.customers");
    let by_construct: Vec<(&str, &Verdict)> = findings
        .iter()
        .filter(|f| f.check == HeuristicId::JoinLeftNullPropagation)
        .map(|f| (f.construct.as_str(), &f.verdict))
        .collect();
    assert_eq!(
        by_construct,
        vec![
            ("left_join[customer_payments:orders]", &Verdict::Uncovered),
            ("left_join[final:customer_orders]", &Verdict::Uncovered),
            ("left_join[final:customer_payments]", &Verdict::Unknown),
        ],
    );
}

#[test]
fn playground_left_join_fires_uncovered_with_a_closure_bound_sketch() {
    // int_patients__with_conditions: `final` LEFT-JOINs the
    // condition_stats aggregate chain and projects its columns
    // directly (cs.first_condition_date). The model has NO unit tests,
    // so the construct is UNCOVERED on real fusion-compiled data, and
    // the no-match sketch binds through the simple-FROM closure to the
    // external relations the givens would mock.
    let manifest = load("playground-current.json");
    let findings = findings_for(
        &manifest,
        "model.healthcare_analytics.int_patients__with_conditions",
    );
    let join_findings: Vec<&Finding<HeuristicId>> = findings
        .iter()
        .filter(|f| f.check == HeuristicId::JoinLeftNullPropagation)
        .collect();
    assert_eq!(
        join_findings.len(),
        1,
        "the chronic_flags/chronic_count joins project only through \
         COALESCE — the declared expression exclusion keeps them silent",
    );
    let finding = join_findings[0];
    assert_eq!(finding.construct, "left_join[final:condition_stats]");
    assert_eq!(finding.verdict, Verdict::Uncovered);
    let sketch = finding
        .evidence
        .iter()
        .find(|e| e.label == "suggested given")
        .expect("no-match sketch present");
    assert!(
        sketch.value.contains("- input: ref('dim_patients')"),
        "left side binds through the patients CTE: {}",
        sketch.value,
    );
    assert!(
        sketch
            .value
            .contains("- input: ref('stg_synthea__conditions')"),
        "right side binds through the condition_stats -> conditions closure: {}",
        sketch.value,
    );
}

/// Build the real render payload for one playground model under a
/// display policy (cute-dbt#171) and return its serialized JSON.
fn payload_json_for(model_id: &str, policy: &CheckPolicy<HeuristicId>) -> serde_json::Value {
    let manifest = load("playground-current.json");
    let models: ModelInScopeSet = [NodeId::new(model_id)].into_iter().collect();
    let changed: InScopeSet = std::iter::empty::<String>().collect();
    let payload = build_payload_with_externals(
        &manifest,
        &changed,
        &models,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "baseline.json",
        policy,
    );
    serde_json::to_value(&payload.models[0]).expect("model payload serializes")
}

const MONTHLY: &str = "model.healthcare_analytics.fct_encounters_monthly";

#[test]
fn disabling_the_grain_group_removes_the_finding_from_the_real_payload() {
    // The cute-dbt#171 selection path over real data: `disable =
    // ["grain.*"]` resolved against the production registry empties the
    // model's findings (and the key is serde-skipped entirely).
    let config = ChecksConfig {
        disable: Some(vec!["grain.*".to_owned()]),
        ..Default::default()
    };
    let policy = resolve_check_policy::<HeuristicId>(&config).expect("policy resolves");
    let model = payload_json_for(MONTHLY, &policy);
    assert!(
        model.get("findings").is_none(),
        "disabled findings must be removed (and serde-skipped): {model}"
    );
}

#[test]
fn a_suppressed_finding_stays_in_the_real_payload_with_its_reason() {
    // The cute-dbt#171 suppression path over real data: the finding is
    // KEPT (verdict intact) and marked with source + reason — the
    // payload contract the findings surface renders.
    let policy = CheckPolicy {
        suppressions: vec![SuppressRule {
            check: HeuristicId::GrainUniqueKeyUnbacked,
            model: "fct_encounters_monthly".to_owned(), // bare-name match
            reason: Some("monthly grain duplicates accepted during backfill".to_owned()),
            source: SuppressionSource::Config,
        }],
        ..Default::default()
    };
    let model = payload_json_for(MONTHLY, &policy);
    let finding = &model["findings"][0];
    assert_eq!(finding["check"], "grain.unique-key-unbacked");
    assert_eq!(finding["verdict"]["status"], "uncovered");
    assert_eq!(finding["suppressed"]["source"], "config");
    assert_eq!(
        finding["suppressed"]["reason"],
        "monthly grain duplicates accepted during backfill"
    );
}

#[test]
fn union_arm_coverage_obeys_the_display_layer_invariant_on_real_data() {
    // cute-dbt#171 invariant extended to the cute-dbt#172 check:
    // disabling or suppressing union.arm-coverage is display-layer ONLY.
    // fct_clinical_events trips the union check UNCOVERED on the real
    // fixture; the suppressed finding must be byte-identical to the
    // default-policy finding apart from the `suppressed` mark (proof
    // that the policy altered neither evaluation nor supersedes
    // resolution), and disabling `union.*` must remove exactly it.
    const EVENTS: &str = "model.healthcare_analytics.fct_clinical_events";

    let baseline = payload_json_for(EVENTS, &CheckPolicy::default());
    let union_default = baseline["findings"]
        .as_array()
        .expect("findings present under the default policy")
        .iter()
        .find(|f| f["check"] == "union.arm-coverage")
        .expect("union.arm-coverage fires on fct_clinical_events")
        .clone();
    assert_eq!(union_default["verdict"]["status"], "uncovered");

    // Disable arm: `union.*` removes the union finding, nothing else.
    let config = ChecksConfig {
        disable: Some(vec!["union.*".to_owned()]),
        ..Default::default()
    };
    let policy = resolve_check_policy::<HeuristicId>(&config).expect("policy resolves");
    let disabled = payload_json_for(EVENTS, &policy);
    let remaining: Vec<&serde_json::Value> = disabled["findings"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    assert!(
        remaining.iter().all(|f| f["check"] != "union.arm-coverage"),
        "disabling union.* removes the union finding: {remaining:?}"
    );

    // Suppress arm: the finding is kept and marked; stripping the mark
    // recovers the default-policy finding exactly.
    let policy = CheckPolicy {
        suppressions: vec![SuppressRule {
            check: HeuristicId::UnionArmCoverage,
            model: "fct_clinical_events".to_owned(),
            reason: Some("event arms exercised downstream".to_owned()),
            source: SuppressionSource::Config,
        }],
        ..Default::default()
    };
    let suppressed = payload_json_for(EVENTS, &policy);
    let mut union_suppressed = suppressed["findings"]
        .as_array()
        .expect("suppression never removes findings")
        .iter()
        .find(|f| f["check"] == "union.arm-coverage")
        .expect("union finding stays present when suppressed")
        .clone();
    assert_eq!(
        union_suppressed["suppressed"]["reason"],
        "event arms exercised downstream"
    );
    union_suppressed
        .as_object_mut()
        .expect("finding is an object")
        .remove("suppressed");
    assert_eq!(
        union_suppressed, union_default,
        "suppression marks the finding and changes NOTHING else"
    );
}

#[test]
fn playground_union_with_both_arms_fed_is_covered_with_attribution() {
    // mart_dq_summary: `combined_metrics` UNION ALLs the
    // `encounter_metrics` and `medication_metrics` CTE arms; the
    // "combines" unit test feeds BOTH arms non-empty csv givens
    // (stg_synthea__encounters / stg_synthea__medications) — the
    // real-data COVERED case for union.arm-coverage (cute-dbt#172).
    let manifest = load("playground-current.json");
    let findings = findings_for(&manifest, "model.healthcare_analytics.mart_dq_summary");
    let union_finding = findings
        .iter()
        .find(|f| f.check == HeuristicId::UnionArmCoverage)
        .expect("union finding fires on mart_dq_summary");
    assert_eq!(union_finding.construct, "union[combined_metrics]");
    let Verdict::Covered { by } = &union_finding.verdict else {
        panic!("expected Covered, got {:?}", union_finding.verdict);
    };
    assert!(
        by.contains(
            &"unit_test.healthcare_analytics.mart_dq_summary.test_mart_dq_summary_combines_encounter_and_medication_metrics"
                .to_owned()
        ),
        "attribution names the feeding test: {by:?}",
    );
    assert!(union_finding.recommendation.is_none());
}

#[test]
fn playground_sentinel_union_arm_is_unknown_never_uncovered() {
    // dim_payers: `final` UNION ALLs `unknown_member` (a constant
    // SELECT with no upstream relation — the dimensional sentinel
    // idiom) and `sequenced` (fed via stg_synthea__payers). The
    // sentinel arm has no resolvable input, so the construct is honest
    // UNKNOWN — never a nagged gap (the cute-dbt#172 exclusion).
    let manifest = load("playground-current.json");
    let findings = findings_for(&manifest, "model.healthcare_analytics.dim_payers");
    let union_finding = findings
        .iter()
        .find(|f| f.check == HeuristicId::UnionArmCoverage)
        .expect("union finding fires on dim_payers");
    assert_eq!(union_finding.construct, "union[final]");
    assert_eq!(union_finding.verdict, Verdict::Unknown);
    assert!(union_finding.recommendation.is_none());
    assert!(
        union_finding
            .evidence
            .iter()
            .any(|e| e.label == "unattributable arm" && e.value.contains("unknown_member")),
        "evidence names the sentinel arm: {:?}",
        union_finding.evidence,
    );
}

#[test]
fn playground_union_model_with_no_unit_tests_is_uncovered_with_a_sketch() {
    // fct_clinical_events UNION ALLs five event-type arms and the
    // fixture carries NO unit test targeting it — every arm is provably
    // unexercised, and the recommendation payload carries a concrete
    // given-row sketch per arm (the catalog C3 worked example, on real
    // fusion-compiled data).
    let manifest = load("playground-current.json");
    let findings = findings_for(&manifest, "model.healthcare_analytics.fct_clinical_events");
    let union_finding = findings
        .iter()
        .find(|f| f.check == HeuristicId::UnionArmCoverage)
        .expect("union finding fires on fct_clinical_events");
    assert_eq!(union_finding.verdict, Verdict::Uncovered);
    assert!(union_finding.recommendation.is_some());
    assert!(
        union_finding
            .evidence
            .iter()
            .any(|e| e.label == "suggested given" && e.value.starts_with("- input: ref('")),
        "a copy-pasteable given sketch rides in the evidence: {:?}",
        union_finding.evidence,
    );
}

/// A minimal synthetic model node whose `compiled_code` is `sql` —
/// for end-to-end runs through the REAL sqlparser engine.
fn model_with_sql(id: &str, sql: &str) -> Node {
    Node::new(
        NodeId::new(id),
        "model",
        Checksum::new("sha256", "x"),
        Some(sql.to_owned()),
        None,
        DependsOn::default(),
        None,
        NodeConfig::new(BTreeMap::new(), false),
        None,
        BTreeMap::new(),
    )
}

/// The cute-dbt#173 supersedes showcase SQL: the dbt-style anti-join —
/// import CTEs, bare `*` projection (so left-null-propagation's own
/// conditions match), and the `WHERE <right key> IS NULL` filter.
const ANTI_JOIN_SQL: &str = "with customers as (\n    select * from \"db\".\"main\".\"stg_customers\"\n),\norders as (\n    select * from \"db\".\"main\".\"stg_orders\"\n),\nfinal as (\n    select *\n    from customers\n    left join orders on customers.customer_id = orders.customer_id\n    where orders.customer_id is null\n)\nselect * from final";

#[test]
fn anti_join_supersedes_left_null_through_the_real_engine() {
    // End-to-end (cute-dbt#173 AC): real sqlparser parse → graph facts
    // → production registry pipeline. Only join.anti-join survives on
    // the construct, with the INVERTED recommendation.
    let node = model_with_sql("model.shop.customers_with_no_orders", ANTI_JOIN_SQL);
    let manifest = Manifest::new(
        ManifestMetadata::new("v12"),
        [(node.id().clone(), node)].into_iter().collect(),
        HashMap::new(),
        HashMap::new(),
    );
    let model = manifest
        .node(&NodeId::new("model.shop.customers_with_no_orders"))
        .expect("model exists");
    let graph = parse_cte_graph(model.compiled_code().unwrap()).expect("anti-join SQL parses");
    let findings = model_findings(&manifest, model, Some(&graph));
    let join_checks: Vec<HeuristicId> = findings
        .iter()
        .filter(|f| f.check.spec().group == "join")
        .map(|f| f.check)
        .collect();
    assert_eq!(
        join_checks,
        vec![HeuristicId::JoinAntiJoin],
        "left-null-propagation is silenced by supersedes resolution: {findings:?}",
    );
    let anti = findings
        .iter()
        .find(|f| f.check == HeuristicId::JoinAntiJoin)
        .expect("anti-join finding present");
    assert_eq!(anti.construct, "left_join[final:orders]");
    assert_eq!(anti.verdict, Verdict::Uncovered);
    let sketch = anti
        .evidence
        .iter()
        .find(|e| e.label == "suggested given")
        .expect("inverted sketch present");
    assert!(
        sketch.value.contains("# matches the right row below"),
        "the recommendation is the INVERTED (matching) given: {}",
        sketch.value,
    );
    assert!(
        sketch.value.contains("ref('stg_orders')") && sketch.value.contains("ref('stg_customers')"),
        "the sketch binds through the import-CTE closures: {}",
        sketch.value,
    );
}

#[test]
fn not_exists_anti_join_is_silent_through_the_real_engine() {
    // Paired negative test for the declared NOT EXISTS exclusion
    // (cute-dbt#173 Discovery call): v1 detects the LEFT JOIN + IS NULL
    // form ONLY — the NOT EXISTS equivalent emits no join finding at
    // all (silent, never misclassified).
    let sql = "select * from \"db\".\"main\".\"stg_customers\" c \
               where not exists (select 1 from \"db\".\"main\".\"stg_orders\" o \
               where o.customer_id = c.customer_id)";
    let node = model_with_sql("model.shop.customers_with_no_orders_ne", sql);
    let manifest = Manifest::new(
        ManifestMetadata::new("v12"),
        [(node.id().clone(), node)].into_iter().collect(),
        HashMap::new(),
        HashMap::new(),
    );
    let model = manifest
        .node(&NodeId::new("model.shop.customers_with_no_orders_ne"))
        .expect("model exists");
    let graph = parse_cte_graph(model.compiled_code().unwrap()).expect("NOT EXISTS SQL parses");
    let findings = model_findings(&manifest, model, Some(&graph));
    assert!(
        findings.iter().all(|f| f.check.spec().group != "join"),
        "the NOT EXISTS form is invisible to the v1 join pair: {findings:?}",
    );
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

#[test]
fn playground_union_findings_payload_snapshot() {
    // Pin the exact serialized union.arm-coverage findings for the
    // three real-fixture verdict shapes — COVERED (mart_dq_summary),
    // UNKNOWN (dim_payers' sentinel arm), UNCOVERED + given-row sketch
    // (fct_clinical_events) — the payload contract cute-dbt#170's render
    // surface will consume (cute-dbt#172).
    let manifest = load("playground-current.json");
    let mut snapshot = serde_json::Map::new();
    for model_id in [
        "model.healthcare_analytics.mart_dq_summary",
        "model.healthcare_analytics.dim_payers",
        "model.healthcare_analytics.fct_clinical_events",
    ] {
        let findings: Vec<Finding<HeuristicId>> = findings_for(&manifest, model_id)
            .into_iter()
            .filter(|f| f.check == HeuristicId::UnionArmCoverage)
            .collect();
        snapshot.insert(
            model_id.to_owned(),
            serde_json::to_value(findings).expect("findings serialize"),
        );
    }
    let rendered = serde_json::to_string_pretty(&serde_json::Value::Object(snapshot))
        .expect("snapshot value serializes");
    insta::assert_snapshot!("playground_union_arm_findings", rendered);
}
