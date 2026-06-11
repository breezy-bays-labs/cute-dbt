//! Step definitions for `features/diff_scoping.feature` — the in-process
//! scenarios exercise `StateComparator::modified_set`,
//! `StateComparator::in_scope_unit_tests`, and
//! `StateComparator::models_in_scope` against synthetic in-memory
//! manifests (no fixture file I/O); the `--modified-selectors` pair
//! (cute-dbt#160) serializes the same synthetic manifests and runs the
//! real `cute-dbt` subprocess so the CLI flag wiring is asserted end to
//! end.

use cucumber::{given, then, when};
use cute_dbt::domain::{
    DependsOn, NodeId, StateComparator, UnitTest, UnitTestExpect, UnitTestGiven,
};

use super::super::common;
use super::World;
use super::builders::{
    empty_pair, model_id, model_node, model_node_with_config, serialize_with_wire_config_to_tmp,
    unit_test_for, unit_test_key, with_node, with_unit_test,
};

const SAME: &str = "sha256:same";
const CHANGED: &str = "sha256:changed";

fn ensure_pair(world: &mut World) {
    if world.current_manifest.is_none() {
        let (current, baseline) = empty_pair();
        world.current_manifest = Some(current);
        world.baseline_manifest = Some(baseline);
    }
}

// --- Background -----------------------------------------------------

#[given("a current manifest and a baseline manifest")]
fn given_pair(world: &mut World) {
    ensure_pair(world);
}

// --- Per-scenario Givens --------------------------------------------

#[given(regex = r#"^the model "([^"]+)" has a different body checksum than the baseline$"#)]
fn model_body_changed(world: &mut World, bare: String) {
    ensure_pair(world);
    let current = world.current_manifest.take().unwrap();
    let baseline = world.baseline_manifest.take().unwrap();
    world.current_manifest = Some(with_node(
        current,
        model_node(&bare, CHANGED, Some("SELECT 1")),
    ));
    world.baseline_manifest = Some(with_node(
        baseline,
        model_node(&bare, SAME, Some("SELECT 1")),
    ));
    world.last_named_model = Some(bare);
}

#[given(regex = r#"^"([^"]+)" has a unit test "([^"]+)"$"#)]
fn model_has_unit_test(world: &mut World, bare: String, test_name: String) {
    ensure_pair(world);
    let current = world.current_manifest.take().unwrap();
    let baseline = world.baseline_manifest.take().unwrap();
    let ut = unit_test_for(&test_name, &bare);
    world.current_manifest = Some(with_unit_test(current, ut.clone()));
    world.baseline_manifest = Some(with_unit_test(baseline, ut));
    world
        .model_to_tests
        .entry(bare)
        .or_default()
        .push(test_name);
}

#[given(regex = r#"^the model "([^"]+)" has the same body checksum as the baseline$"#)]
fn model_body_same(world: &mut World, bare: String) {
    ensure_pair(world);
    let current = world.current_manifest.take().unwrap();
    let baseline = world.baseline_manifest.take().unwrap();
    world.current_manifest = Some(with_node(
        current,
        model_node(&bare, SAME, Some("SELECT 1")),
    ));
    world.baseline_manifest = Some(with_node(
        baseline,
        model_node(&bare, SAME, Some("SELECT 1")),
    ));
    world.last_named_model = Some(bare);
}

#[given(regex = r#"^the model "([^"]+)" does not exist in the baseline$"#)]
fn model_new_in_current(world: &mut World, bare: String) {
    ensure_pair(world);
    let current = world.current_manifest.take().unwrap();
    // Current carries the model; baseline does not.
    world.current_manifest = Some(with_node(
        current,
        model_node(&bare, CHANGED, Some("SELECT 1")),
    ));
    world.last_named_model = Some(bare);
}

#[given(regex = r#"^the model "([^"]+)" is unchanged in body$"#)]
fn model_unchanged_body(world: &mut World, bare: String) {
    model_body_same(world, bare);
}

#[given(regex = r#"^its unit test "([^"]+)" was itself modified$"#)]
fn unit_test_modified(world: &mut World, test_name: String) {
    ensure_pair(world);
    let current = world.current_manifest.take().unwrap();
    let baseline = world.baseline_manifest.take().unwrap();
    // Identify the bare model from the most recent named-model Given.
    let target = world
        .last_named_model
        .clone()
        .expect("a model was named in a prior Given");
    // Current carries the modified test (description differs); baseline carries the original.
    let ut_current = UnitTest::new(
        test_name.clone(),
        NodeId::new(&target),
        Vec::<UnitTestGiven>::new(),
        UnitTestExpect::new(serde_json::Value::Array(Vec::new()), None, None),
        Some("v2".to_owned()),
        DependsOn::default(),
        None,
        None,
        None,
    );
    let ut_baseline = unit_test_for(&test_name, &target);
    world.current_manifest = Some(with_unit_test(current, ut_current));
    world.baseline_manifest = Some(with_unit_test(baseline, ut_baseline));
}

#[given(regex = r#"^the model "([^"]+)" changed only in its config block$"#)]
fn model_config_only_change(world: &mut World, bare: String) {
    // A REAL config divergence (`materialized: table` vs `view`) under
    // an identical body checksum — invisible to the body-only default,
    // visible to the opt-in `.configs` sub-selector (cute-dbt#160). The
    // genuine divergence keeps the documented-limit scenario honest: it
    // passes because body-checksum scoping cannot SEE the change, not
    // because nothing changed.
    ensure_pair(world);
    let current = world.current_manifest.take().unwrap();
    let baseline = world.baseline_manifest.take().unwrap();
    world.current_manifest = Some(with_node(
        current,
        model_node_with_config(&bare, SAME, Some("SELECT 1"), &[("materialized", "table")]),
    ));
    world.baseline_manifest = Some(with_node(
        baseline,
        model_node_with_config(&bare, SAME, Some("SELECT 1"), &[("materialized", "view")]),
    ));
    world.last_named_model = Some(bare);
}

#[given("its body checksum is identical to the baseline")]
fn model_body_identical_to_baseline(_world: &mut World) {
    // No-op — the previous step already pinned both sides to SAME.
}

#[given(regex = r#"^"([^"]+)" has no unit tests in the current manifest$"#)]
fn model_has_no_unit_tests(world: &mut World, bare: String) {
    // The "model has a different body checksum" step already populated
    // nodes. Recording the empty test list here lets later assertions
    // verify the scenario's intent explicitly.
    world.model_to_tests.entry(bare).or_default();
}

// --- When -----------------------------------------------------------

#[when("the in-scope set is computed")]
fn compute_in_scope(world: &mut World) {
    let current = world
        .current_manifest
        .as_ref()
        .expect("a current manifest exists");
    let baseline = world
        .baseline_manifest
        .as_ref()
        .expect("a baseline manifest exists");
    let comparator = StateComparator::body_only();
    world.last_in_scope = Some(comparator.in_scope_unit_tests(current, baseline));
}

#[when("the models-in-scope set is computed")]
fn compute_models_in_scope(world: &mut World) {
    let current = world
        .current_manifest
        .as_ref()
        .expect("a current manifest exists");
    let baseline = world
        .baseline_manifest
        .as_ref()
        .expect("a baseline manifest exists");
    let comparator = StateComparator::body_only();
    world.last_models_in_scope = Some(comparator.models_in_scope(current, baseline));
}

// --- When: the --modified-selectors subprocess runs (cute-dbt#160) ---
//
// Unlike the in-process Whens above (which call `StateComparator`
// directly), these serialize the scenario's synthetic pair and run the
// REAL `cute-dbt` subprocess so the clap flag → run loop →
// `from_selectors` wiring is exercised end to end. The flat wire
// `config` injection (`serialize_with_wire_config_to_tmp`) carries the
// config divergence across the domain→JSON→wire round-trip.

#[when(regex = r#"^I run cute-dbt on the synthetic pair with --modified-selectors "([^"]+)"$"#)]
fn run_synthetic_pair_with_selectors(world: &mut World, selectors: String) {
    run_synthetic_pair(world, Some(&selectors));
}

#[when("I run cute-dbt on the synthetic pair without --modified-selectors")]
fn run_synthetic_pair_without_selectors(world: &mut World) {
    run_synthetic_pair(world, None);
}

fn run_synthetic_pair(world: &mut World, selectors: Option<&str>) {
    let current = world
        .current_manifest
        .as_ref()
        .expect("a current manifest exists");
    let baseline = world
        .baseline_manifest
        .as_ref()
        .expect("a baseline manifest exists");
    let current_path = serialize_with_wire_config_to_tmp(current, "bdd_selectors_current");
    let baseline_path = serialize_with_wire_config_to_tmp(baseline, "bdd_selectors_baseline");
    let out = common::tmp(if selectors.is_some() {
        "bdd_selectors_with.html"
    } else {
        "bdd_selectors_without.html"
    });
    common::clear(&out);

    let mut args: Vec<&str> = vec![
        "report",
        "--manifest",
        common::s(&current_path),
        "--baseline-manifest",
        common::s(&baseline_path),
        "--out",
        common::s(&out),
    ];
    if let Some(tokens) = selectors {
        args.push("--modified-selectors");
        args.push(tokens);
    }
    let output = common::run_cli(&args);
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    world.out_path = Some(out);
}

// --- Then -----------------------------------------------------------

#[then(regex = r#"^"([^"]+)" is in scope$"#)]
fn name_is_in_scope(world: &mut World, name: String) {
    let set = world.last_in_scope.as_ref().expect("in-scope set computed");
    let key = unit_test_key(&name);
    assert!(
        set.iter().any(|id| id == key.as_str()),
        "expected {key} in scope; got {:?}",
        set.iter().collect::<Vec<_>>()
    );
}

#[then(regex = r#"^"([^"]+)" is not in scope$"#)]
fn name_is_not_in_scope(world: &mut World, name: String) {
    // "Not in scope" applies to either a test name or a model name —
    // both are absent from the InScopeSet (which carries only unit
    // test ids). The unified assertion is: no id mentions `name`.
    let set = world.last_in_scope.as_ref().expect("in-scope set computed");
    let key = unit_test_key(&name);
    assert!(
        !set.iter()
            .any(|id| id == key.as_str() || id.ends_with(&name)),
        "did not expect any id matching {name} in scope; got {:?}",
        set.iter().collect::<Vec<_>>()
    );
}

#[then(regex = r#"^the model "([^"]+)" is in models_in_scope$"#)]
fn model_is_in_models_in_scope(world: &mut World, bare: String) {
    let set = world
        .last_models_in_scope
        .as_ref()
        .expect("models_in_scope computed");
    let id = model_id(&bare);
    assert!(
        set.iter().any(|node_id| node_id == &id),
        "expected {id} in models_in_scope; got {:?}",
        set.iter().collect::<Vec<_>>()
    );
}
