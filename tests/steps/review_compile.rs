//! Step definitions for `features/review_compile.feature` — the
//! `review` compile step (cute-dbt#301, epic #294 V2): engine
//! detection from `dbt --version` output shape, exit-code-disciplined
//! `dbt compile`, and the `--no-compile` staleness warning.
//!
//! The dbt on PATH is always a shim (the `TestRepo` PATH is fully
//! controlled): the Givens here swap shim behaviors or remove the shim
//! entirely. Repo construction, the Whens, and the shared Thens live in
//! `one_command_review.rs` (same `World` fields).

use cucumber::{given, then};

use super::super::common::ShimSpec;
use super::World;

/// The repo the shared Given built (one_command_review.rs).
fn repo(world: &World) -> &super::super::common::TestRepo {
    world
        .review_repo
        .as_ref()
        .expect("a Given built the review repo")
}

// --- Given ----------------------------------------------------------

#[given("dbt is not installed on PATH")]
fn given_no_dbt(world: &mut World) {
    // Remove the default dbt stand-in (binary copy + sibling spec) so dbt
    // is genuinely NotFound on the controlled PATH.
    repo(world).remove_shim("dbt");
}

#[given("the dbt on PATH fails its compile with a compilation error")]
fn given_failing_compile(world: &mut World) {
    repo(world).install_dbt_shim(&ShimSpec::new(0).version("dbt 2.0.0-preview.186\n").rule(
        "compile",
        "",
        "Compilation Error in model stg_customers\n",
        1,
    ));
}

#[given("the dbt on PATH answers with the python core version block")]
fn given_core_shim(world: &mut World) {
    repo(world).install_dbt_shim(
        &ShimSpec::new(0)
            .version("Core:\n  - installed: 1.10.2\n  - latest:    1.10.2 - Up to date!\n")
            .compile_ok(),
    );
}

#[given("the manifest is older than the model sources")]
fn given_stale_manifest(world: &mut World) {
    let manifest = repo(world).root.join("target/manifest.json");
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&manifest)
        .expect("open the manifest")
        .set_modified(past)
        .expect("age the manifest");
}

// --- Then -----------------------------------------------------------

#[then("stderr says dbt was not found and names --no-compile")]
fn stderr_dbt_missing(world: &mut World) {
    assert!(
        world.last_stderr.contains("`dbt` was not found on PATH"),
        "the install remediation fires: {}",
        world.last_stderr,
    );
    assert!(
        world.last_stderr.contains("--no-compile"),
        "the no-dbt escape hatch is named: {}",
        world.last_stderr,
    );
}

#[then("stderr relays the dbt compilation error verbatim")]
fn stderr_relays_compile_error(world: &mut World) {
    assert!(
        world
            .last_stderr
            .contains("Compilation Error in model stg_customers"),
        "dbt's own stderr streams through: {}",
        world.last_stderr,
    );
    assert!(
        world.last_stderr.contains("dbt compile failed"),
        "the review-stage failure is named: {}",
        world.last_stderr,
    );
}

#[then("a manifest file still exists in the project target directory")]
fn manifest_still_exists(world: &mut World) {
    assert!(
        repo(world).root.join("target/manifest.json").exists(),
        "the fusion-trap precondition holds: the manifest file is present",
    );
}

#[then("stderr warns the manifest is stale")]
fn stderr_warns_stale(world: &mut World) {
    assert!(
        world.last_stderr.contains("older than"),
        "the staleness warning fires: {}",
        world.last_stderr,
    );
}

#[then(regex = r#"^stderr announces the dbt engine as "([^"]+)"$"#)]
fn stderr_announces_engine(world: &mut World, engine: String) {
    assert!(
        world.last_stderr.contains(&format!("dbt engine: {engine}")),
        "the detected engine is announced: {}",
        world.last_stderr,
    );
}
