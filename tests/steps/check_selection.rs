//! Step definitions for `features/check_selection.feature` — check
//! selection + suppression via the `[checks]` config section and the
//! inline SQL pragma (cute-dbt#171).
//!
//! Self-contained (the `coverage_checks.rs` pattern): the Givens build a
//! [`SelectionPlan`](super::world::SelectionPlan) into the `World`; the
//! `When` serializes a synthetic current/baseline pair (flat model
//! `config` carrying the unbacked `unique_key` — the wire shape the
//! flat-domain serialization cannot express), writes the scenario's
//! `[checks]` TOML to a temp `--config` file when present, and runs the
//! real `cute-dbt` subprocess. The Thens assert the embedded
//! `cute-dbt-data` payload's per-model `findings` facts: removal under
//! selection, the `suppressed` mark + carried reason under suppression,
//! and the fail-closed usage-error path (exit ≠ 0, no report, stderr
//! remediation) for an illegal/unknown `[checks]` entry.
//!
//! Step wording is deliberately selection-specific (`selection model`,
//! `I render the check-selection report`) so it cannot collide with the
//! scaffolding bound by other feature modules; the generic payload
//! Thens (`the payload carries … finding`) are REUSED from
//! `coverage_checks.rs` (cucumber step regexes are binary-global).

use cucumber::{given, then, when};
use serde_json::{Value, json};

use super::super::common;
use super::World;
use super::builders::{empty_manifest, model_id, serialize_to_tmp};
use super::world::SelectionModel;

// --- Background -----------------------------------------------------

#[given("a check-selection report scenario")]
fn given_scenario(world: &mut World) {
    world.selection_plan = Default::default();
}

#[given(
    regex = r#"^the modified selection model "([^"]+)" declares unique_key "([^"]+)" with no uniqueness test$"#
)]
fn model_with_unbacked_key(world: &mut World, bare: String, key: String) {
    world.selection_plan.models.push(SelectionModel {
        bare,
        unique_key: key,
        raw_sql: None,
    });
}

// --- Given: config --------------------------------------------------

#[given(regex = r#"^a checks config that disables "([^"]+)"$"#)]
fn config_disables(world: &mut World, entry: String) {
    world.selection_plan.config_toml = Some(format!("[checks]\ndisable = [\"{entry}\"]\n"));
}

#[given(regex = r#"^an opt-in checks config that enables "([^"]+)"$"#)]
fn config_opt_in_enables(world: &mut World, entry: String) {
    world.selection_plan.config_toml = Some(format!(
        "[checks]\nmode = \"opt-in\"\nenable = [\"{entry}\"]\n"
    ));
}

#[given("an opt-in checks config with an empty enable list")]
fn config_opt_in_empty_enable(world: &mut World) {
    world.selection_plan.config_toml =
        Some("[checks]\nmode = \"opt-in\"\nenable = []\n".to_owned());
}

#[given(regex = r#"^a checks config that suppresses "([^"]+)" on "([^"]+)" because "([^"]+)"$"#)]
fn config_suppresses(world: &mut World, check: String, model: String, reason: String) {
    world.selection_plan.config_toml = Some(format!(
        "[[checks.suppress]]\ncheck = \"{check}\"\nmodel = \"{model}\"\nreason = \"{reason}\"\n"
    ));
}

#[given(regex = r#"^a checks config that enables "([^"]+)" without opt-in mode$"#)]
fn config_enable_without_opt_in(world: &mut World, entry: String) {
    // mode stays the opt-out default — `enable` is illegal there.
    world.selection_plan.config_toml = Some(format!("[checks]\nenable = [\"{entry}\"]\n"));
}

// --- Given: inline pragma -------------------------------------------

#[given(regex = r#"^the model "([^"]+)" raw SQL carries the pragma (.+)$"#)]
fn model_raw_sql_with_pragma(world: &mut World, bare: String, pragma: String) {
    let model = world
        .selection_plan
        .models
        .iter_mut()
        .find(|m| m.bare == bare)
        .unwrap_or_else(|| panic!("model {bare:?} must be declared before its pragma"));
    model.raw_sql = Some(format!("{pragma}\nselect 1 as order_id"));
}

// --- When -----------------------------------------------------------

#[when("I render the check-selection report")]
fn render_selection_report(world: &mut World) {
    let plan = world.selection_plan.clone();

    // Every declared model is modified vs the baseline (differing body
    // checksums) so the comparator puts it in scope; compiled_code
    // clears the Stage-2 preflight. raw_code rides on the wire node
    // (it round-trips natively) so the pragma scanner sees it.
    let current = empty_manifest();
    let baseline = empty_manifest();
    let mut current_value: Value = serde_json::to_value(&current).expect("manifest serializes");
    for model in &plan.models {
        let id = model_id(&model.bare);
        current_value["nodes"][id.as_str()] = json!({
            "resource_type": "model",
            "checksum": { "name": "sha256", "checksum": "current" },
            "compiled_code": "select 1 as order_id",
            "raw_code": model.raw_sql.clone(),
            // `table`, not `incremental`: the grain check is
            // materialization-agnostic, and an incremental config would
            // also trip incremental.branch-coverage (cute-dbt#164),
            // breaking these scenarios' single-concern "no findings"
            // assertions.
            "config": {
                "materialized": "table",
                "unique_key": model.unique_key,
            },
        });
    }
    // The spliced wire JSON is written verbatim — the subprocess's
    // Stage-1 adapter is the real parser under test.
    let current_path = common::tmp("selection_current.json");
    std::fs::write(
        &current_path,
        serde_json::to_string(&current_value).expect("spliced manifest serializes"),
    )
    .expect("write selection manifest");
    let baseline_path = serialize_to_tmp(&baseline, "selection_baseline");

    let out = common::tmp("selection_report.html");
    common::clear(&out);
    let mut args: Vec<String> = vec![
        "report".into(),
        "--manifest".into(),
        current_path.display().to_string(),
        "--baseline-manifest".into(),
        baseline_path.display().to_string(),
        "--out".into(),
        out.display().to_string(),
    ];
    if let Some(toml) = &plan.config_toml {
        let config_path = common::tmp("selection_config.toml");
        std::fs::write(&config_path, toml).expect("write selection config TOML");
        args.push("--config".into());
        args.push(config_path.display().to_string());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = common::run_cli(&arg_refs);
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    if let Some(html) = &world.report_html {
        common::assert_no_external_refs(html);
    }
    world.out_path = Some(out);
}

// --- Then -----------------------------------------------------------

#[then(
    regex = r#"^the "([^"]+)" finding for "([^"]+)" is suppressed by "([^"]+)" with reason "([^"]+)"$"#
)]
fn finding_is_suppressed_with_reason(
    world: &mut World,
    check: String,
    model: String,
    source: String,
    reason: String,
) {
    let finding = selection_finding(world, &check, &model);
    assert_eq!(
        finding["suppressed"]["source"].as_str(),
        Some(source.as_str()),
        "suppression source: {finding}"
    );
    assert_eq!(
        finding["suppressed"]["reason"].as_str(),
        Some(reason.as_str()),
        "suppression reason rides into the payload: {finding}"
    );
}

#[then(
    regex = r#"^the "([^"]+)" finding for "([^"]+)" is suppressed by "pragma" without a reason$"#
)]
fn finding_is_suppressed_without_reason(world: &mut World, check: String, model: String) {
    let finding = selection_finding(world, &check, &model);
    assert_eq!(finding["suppressed"]["source"].as_str(), Some("pragma"));
    assert!(
        finding["suppressed"].get("reason").is_none(),
        "an absent pragma reason must be serde-skipped: {finding}"
    );
}

#[then("stderr explains that enable requires opt-in mode")]
fn stderr_explains_enable_mode(world: &mut World) {
    assert!(
        world
            .last_stderr
            .contains("only legal with mode = \"opt-in\""),
        "expected the enable/opt-in remediation; got: {}",
        world.last_stderr
    );
}

#[then(regex = r#"^stderr names the unknown entry "([^"]+)" and lists the known check ids$"#)]
fn stderr_names_unknown_entry(world: &mut World, entry: String) {
    assert!(
        world.last_stderr.contains(&entry),
        "stderr names the offending entry {entry:?}; got: {}",
        world.last_stderr
    );
    assert!(
        world.last_stderr.contains("grain.unique-key-unbacked"),
        "remediation lists the known check ids; got: {}",
        world.last_stderr
    );
}

// --- Payload helpers (the coverage_checks.rs shape) -------------------

/// Parse the embedded `cute-dbt-data` payload and find the finding with
/// `check` on the model named `model`.
fn selection_finding(world: &World, check: &str, model: &str) -> Value {
    let html = world
        .report_html
        .as_ref()
        .unwrap_or_else(|| panic!("report.html was not written; stderr={}", world.last_stderr));
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("report HTML must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id("cute-dbt-data")
        .expect("report must include <script id=\"cute-dbt-data\">")
        .get(parser)
        .expect("payload node resolves");
    let payload: Value = serde_json::from_str(&node.inner_text(parser))
        .expect("embedded payload must be valid JSON");
    let model_obj = payload["models"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["name"].as_str() == Some(model))
        .unwrap_or_else(|| panic!("model {model:?} not in payload: {payload}"))
        .clone();
    model_obj["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["check"].as_str() == Some(check))
        .cloned()
        .unwrap_or_else(|| panic!("no {check:?} finding on model {model:?}: {model_obj}"))
}
