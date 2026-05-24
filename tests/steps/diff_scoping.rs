//! Step definitions for `features/diff_scoping.feature` — six scenarios
//! that exercise `StateComparator::modified_set`,
//! `StateComparator::in_scope_unit_tests`, and
//! `StateComparator::models_in_scope` against synthetic in-memory
//! manifests (no fixture file I/O).

use cucumber::{given, then, when};
use cute4dbt::domain::{
    DependsOn, NodeId, StateComparator, UnitTest, UnitTestExpect, UnitTestGiven,
};

use super::World;
use super::builders::{
    empty_pair, model_id, model_node, unit_test_for, unit_test_key, with_node, with_unit_test,
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
        UnitTestExpect::new(serde_json::Value::Array(Vec::new()), None),
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
    // v0.1 body-checksum scoping ignores config; the bodies are equal.
    model_body_same(world, bare);
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
