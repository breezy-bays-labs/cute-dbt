//! Step definitions for `features/macro_perspective.feature`
//! (cute-dbt#265 — the macro perspective lens, Slice B).
//!
//! Subprocess wire round-trip (the BDD house style, the
//! `project_definition.rs` precedent): each scenario builds a synthetic
//! current manifest carrying a root-project (or vendor) macro called by
//! models, injects the macro in its real wire shape
//! (`{ macro_sql, depends_on, original_file_path, name, package_name }` —
//! the domain stores macros as bare body strings the reader does not
//! accept), writes a real working-tree macro source file byte-aligned to
//! that body (the same-revision contract), synthesizes a `--unified=0`
//! patch editing exactly one line of it, runs the actual `cute-dbt` binary
//! with `--pr-diff @macro.patch --project-root .`, and asserts the rendered
//! macro-lens markup. The experimental switch is opted in exactly like a
//! consumer (the `CUTE_DBT_EXPERIMENTAL` env surface, default-OFF).

use std::collections::{BTreeMap, HashMap};

use cucumber::{given, then, when};
use serde_json::{Value, json};

use super::super::common;
use super::World;
use cute_dbt::domain::{Checksum, DependsOn, Manifest, ManifestMetadata, Node, NodeConfig, NodeId};

/// The project the macro-lens scenarios pin as the root project (the
/// blast-radius `package_name == project_name` filter keys on this).
const PROJECT: &str = "shop";

/// The changed macro's declaring file, project-relative — the path the
/// synthesized patch addresses and the working-tree source file lives at.
const MACRO_PATH: &str = "macros/dq.sql";

/// The macro body (3 lines). Line 2 is the line the patch edits; the `+`
/// side byte-matches it so the N7b drift guard aligns. The working-tree
/// source file is this body + one trailing newline (the on-disk shape the
/// reconstruction normalizes).
const MACRO_BODY: &str =
    "{% macro add_dq_flags(col) %}\n  case when {{ col }} then 1 end\n{% endmacro %}";

/// The macro's `unique_id` (root-project arm).
fn root_macro_id() -> String {
    format!("macro.{PROJECT}.add_dq_flags")
}

/// A root-project `model` node calling `macro_id` directly, declaring
/// `original_file_path` and a passing compiled body (so Stage-2 preflight
/// is a no-op — these models carry no unit tests).
fn macro_model(bare: &str, ofp: &str, macro_id: &str) -> Node {
    macro_model_with_raw(bare, ofp, macro_id, None)
}

/// As [`macro_model`], but with an optional `raw_code` body — the
/// call-site + inline-SQL source the Slice C model-selector reveals.
fn macro_model_with_raw(bare: &str, ofp: &str, macro_id: &str, raw: Option<&str>) -> Node {
    Node::new(
        NodeId::new(format!("model.{PROJECT}.{bare}")),
        "model",
        Checksum::new("sha256", bare),
        Some("select 1".to_owned()),
        raw.map(str::to_owned),
        DependsOn::new(vec![macro_id.to_owned()], Vec::new()),
        Some(format!("models/{ofp}")),
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
    .with_identity(None, Some(PROJECT.to_owned()))
}

/// Build the root-project current manifest: two models in different
/// directory subtrees, both calling the root-project macro.
fn manifest_two_callers() -> Manifest {
    let nodes = [
        macro_model("stg_orders", "staging/stg_orders.sql", &root_macro_id()),
        macro_model("fct_orders", "marts/core/fct_orders.sql", &root_macro_id()),
    ];
    Manifest::new(
        ManifestMetadata::new("https://schemas.getdbt.com/dbt/manifest/v12.json")
            .with_project_name(Some(PROJECT.to_owned())),
        nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
        HashMap::new(),
        HashMap::new(),
    )
}

/// Build the Slice C current manifest: two root-project models both
/// calling the macro INLINE in their `raw_code` (the call-site source the
/// model-selector reveals). `fct_orders` calls it twice (the over-cap copy
/// only triggers past the cap, so two is shown in full — the cap is
/// exercised in the render-lane unit/headless layer, not the slow BDD
/// subprocess). The bodies name `add_dq_flags(` so the first-order
/// call-site scan resolves them.
fn manifest_two_callers_with_call_sites() -> Manifest {
    let stg_raw = "select *\nfrom raw_orders\n  {{ add_dq_flags() }}";
    let fct_raw =
        "select *\nfrom stg_orders\n  {{ add_dq_flags(amount) }}\n  {{ add_dq_flags(qty) }}";
    let nodes = [
        macro_model_with_raw(
            "stg_orders",
            "staging/stg_orders.sql",
            &root_macro_id(),
            Some(stg_raw),
        ),
        macro_model_with_raw(
            "fct_orders",
            "marts/core/fct_orders.sql",
            &root_macro_id(),
            Some(fct_raw),
        ),
    ];
    Manifest::new(
        ManifestMetadata::new("https://schemas.getdbt.com/dbt/manifest/v12.json")
            .with_project_name(Some(PROJECT.to_owned())),
        nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
        HashMap::new(),
        HashMap::new(),
    )
}

/// Build the vendor-arm manifest: a root-project model calls a
/// VENDOR-package macro (`macro.dbt_utils.add_dq_flags`). Editing the
/// vendor macro must NOT surface the lens (the root-project filter on both
/// the changed-macro detection name-key and the blast radius).
fn manifest_vendor_macro() -> Manifest {
    let vendor_id = "macro.dbt_utils.add_dq_flags".to_owned();
    let node = macro_model("stg_orders", "staging/stg_orders.sql", &vendor_id);
    Manifest::new(
        ManifestMetadata::new("https://schemas.getdbt.com/dbt/manifest/v12.json")
            .with_project_name(Some(PROJECT.to_owned())),
        std::iter::once((node.id().clone(), node)).collect(),
        HashMap::new(),
        HashMap::new(),
    )
}

/// Serialize the manifest, then inject the macro in its real wire shape:
/// `{ macro_sql, depends_on: { macros: [] }, original_file_path, name,
/// package_name }`. The domain `Manifest` cannot express the macro
/// identity triple through a bare-body serialization, so the wire shape is
/// spliced in at the JSON layer (the `serialize_with_wire_macros_to_tmp`
/// precedent, extended with the #265 identity fields).
fn serialize_with_wire_macro(manifest: &Manifest, name: &str, macro_id: &str, package: &str) {
    let mut value: Value = serde_json::to_value(manifest).expect("manifest serializes to Value");
    let macro_name = macro_id.rsplit('.').next().unwrap_or(macro_id);
    if !value["macros"].is_object() {
        value["macros"] = json!({});
    }
    value["macros"][macro_id] = json!({
        "macro_sql": MACRO_BODY,
        "depends_on": { "macros": [] },
        "original_file_path": MACRO_PATH,
        "name": macro_name,
        "package_name": package,
    });
    let path = common::tmp(name);
    std::fs::write(
        &path,
        serde_json::to_string(&value).expect("injected manifest serializes"),
    )
    .expect("write synthetic manifest");
}

/// The workdir the subprocess runs in (`--project-root .`); the working-tree
/// macro source file is written under it.
fn macro_workdir() -> std::path::PathBuf {
    let workdir = common::tmp("macro_lens_workdir");
    std::fs::create_dir_all(workdir.join("macros")).expect("create macro workdir");
    workdir
}

// ---------------------------------------------------------------------
// Given
// ---------------------------------------------------------------------

#[given("a current manifest with a root-project macro called by two models")]
fn current_manifest_two_callers(world: &mut World) {
    world.current_manifest = Some(manifest_two_callers());
    // Mark the root arm so the When serializes the root macro wire shape.
    world.fixture_choice = None;
}

#[given("a current manifest with a root-project macro called inline by two models")]
fn current_manifest_two_callers_inline(world: &mut World) {
    world.current_manifest = Some(manifest_two_callers_with_call_sites());
    world.fixture_choice = None;
}

#[given("a current manifest with a vendor-package macro called by a root-project model")]
fn current_manifest_vendor_macro(world: &mut World) {
    world.current_manifest = Some(manifest_vendor_macro());
    // Stash the vendor id so the When injects the vendor wire shape.
    world.last_named_model = Some("vendor".to_owned());
}

#[given("the working tree carries that macro's source file")]
fn working_tree_carries_macro_source(_world: &mut World) {
    let workdir = macro_workdir();
    // Source file = the macro body + one trailing newline (the on-disk
    // shape; the reconstruction strips exactly one terminator).
    std::fs::write(workdir.join(MACRO_PATH), format!("{MACRO_BODY}\n"))
        .expect("write macro source file");
}

#[given("the PR diff edits the macro's body")]
fn diff_edits_macro_body(world: &mut World) {
    // `--unified=0` single-line edit on macro line 2. The `+` side
    // byte-matches the working-tree body (N7b aligns); the `-` side is the
    // invented pre-edit line.
    let patch = format!(
        concat!(
            "diff --git a/{path} b/{path}\n",
            "--- a/{path}\n",
            "+++ b/{path}\n",
            "@@ -2 +2 @@\n",
            "-  case when {{{{ col }}}} then 0 end\n",
            "+  case when {{{{ col }}}} then 1 end\n",
        ),
        path = MACRO_PATH,
    );
    write_macro_patch(world, &patch);
}

#[given("the PR diff edits only a model's SQL, not the macro")]
fn diff_edits_model_only(world: &mut World) {
    // A diff touching a NON-macro file: the macro section must stay absent.
    let patch = concat!(
        "diff --git a/models/staging/stg_orders.sql b/models/staging/stg_orders.sql\n",
        "--- a/models/staging/stg_orders.sql\n",
        "+++ b/models/staging/stg_orders.sql\n",
        "@@ -1 +1 @@\n",
        "-select 0\n",
        "+select 1\n",
    );
    write_macro_patch(world, patch);
}

#[given("the experimental switch enables macro-lens")]
fn experimental_switch_enables_macro_lens(world: &mut World) {
    world.experimental_env = Some("macro-lens".to_owned());
}

fn write_macro_patch(world: &mut World, patch: &str) {
    let path = common::tmp("macro_lens.patch");
    std::fs::write(&path, patch).expect("write macro.patch");
    world.explicit_patch = Some(path);
}

// ---------------------------------------------------------------------
// When
// ---------------------------------------------------------------------

#[when("I run cute-dbt report in pr-diff mode against the macro patch")]
fn run_macro_pr_diff(world: &mut World) {
    let manifest = world
        .current_manifest
        .take()
        .expect("a Given built a manifest");
    let is_vendor = world.last_named_model.as_deref() == Some("vendor");
    if is_vendor {
        serialize_with_wire_macro(
            &manifest,
            "macro_lens_current.json",
            "macro.dbt_utils.add_dq_flags",
            "dbt_utils",
        );
    } else {
        serialize_with_wire_macro(
            &manifest,
            "macro_lens_current.json",
            &root_macro_id(),
            PROJECT,
        );
    }
    world.current_manifest = Some(manifest);
    let manifest_path = common::tmp("macro_lens_current.json");

    let workdir = macro_workdir();
    let patch_path = world
        .explicit_patch
        .clone()
        .expect("a Given wrote the patch");
    let out = common::tmp("macro_lens_report.html");
    common::clear(&out);

    let scope_arg = format!("@{}", common::s(&patch_path));
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args([
        "report",
        "--manifest",
        common::s(&manifest_path),
        "--pr-diff",
        &scope_arg,
        "--project-root",
        ".",
        "--out",
        common::s(&out),
    ])
    .current_dir(&workdir);
    cmd.env_remove("CUTE_DBT_EXPERIMENTAL");
    if let Some(value) = &world.experimental_env {
        cmd.env("CUTE_DBT_EXPERIMENTAL", value);
    }
    let output = cmd.output().expect("the cute-dbt binary spawns");
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    if let Some(html) = &world.report_html {
        common::assert_no_external_refs(html);
    }
    world.out_path = Some(out);
}

// ---------------------------------------------------------------------
// Then
// ---------------------------------------------------------------------

fn html(world: &World) -> &str {
    world
        .report_html
        .as_deref()
        .unwrap_or_else(|| panic!("report.html was not written; stderr={}", world.last_stderr))
}

#[then("the report carries the macro-lens section")]
fn report_carries_macro_section(world: &mut World) {
    assert!(
        html(world).contains(r#"data-testid="macro-lens-panel""#),
        "the macro-lens section must render; stderr={}",
        world.last_stderr,
    );
}

#[then("the report carries no macro-lens section")]
fn report_carries_no_macro_section(world: &mut World) {
    assert!(
        !html(world).contains(r#"data-testid="macro-lens-panel""#),
        "no macro-lens section may render in this case; stderr={}",
        world.last_stderr,
    );
}

#[then("the macro-lens section names the changed macro")]
fn section_names_macro(world: &mut World) {
    assert!(
        html(world).contains("add_dq_flags"),
        "the section must name the changed macro",
    );
}

#[then("the macro-lens section carries the macro body diff")]
fn section_carries_body_diff(world: &mut World) {
    let html = html(world);
    assert!(
        html.contains(r#"data-testid="macro-lens-diff""#),
        "the section must carry the body diff",
    );
    assert!(
        html.contains("then 0 end"),
        "the diff must splice in the pre-edit macro line",
    );
}

#[then(regex = r#"^the macro-lens section reports the impacted-model count as (\d+)$"#)]
fn section_reports_count(world: &mut World, count: String) {
    let html = html(world);
    assert!(
        html.contains(&format!(r#"data-count="{count}""#)),
        "the impacted-model count must be {count}",
    );
}

#[then("the macro-lens section lists both impacted models in the directory tree")]
fn section_lists_both_models(world: &mut World) {
    let html = html(world);
    assert!(
        html.contains(r#"data-testid="macro-lens-tree""#),
        "tree present"
    );
    assert!(
        html.contains(r#"data-model="model.shop.stg_orders""#),
        "stg_orders leaf present",
    );
    assert!(
        html.contains(r#"data-model="model.shop.fct_orders""#),
        "fct_orders leaf present",
    );
}

#[then(regex = r#"^the macro-lens fidelity chip reads "([^"]+)"$"#)]
fn fidelity_chip_reads(world: &mut World, fidelity: String) {
    assert!(
        html(world).contains(&format!(r#"data-fidelity="{fidelity}""#)),
        "the fidelity chip must read {fidelity:?}",
    );
}

#[then(regex = r#"^the macro-lens section never names a "([^"]+)" selector$"#)]
fn section_never_names_selector(world: &mut World, selector: String) {
    assert!(
        !html(world).contains(&selector),
        "the section must never name the {selector:?} selector (critique S2)",
    );
}

// ---------------------------------------------------------------------
// Slice C — model-selector + first-order call sites
// ---------------------------------------------------------------------

#[then("the macro-lens section carries an impacted-model selector")]
fn section_carries_model_selector(world: &mut World) {
    assert!(
        html(world).contains(r#"data-testid="macro-lens-model-select""#),
        "the section must carry the impacted-model selector (founder D4)",
    );
}

#[then("the impacted-model selector offers both models")]
fn selector_offers_both_models(world: &mut World) {
    let html = html(world);
    assert!(
        html.contains(r#"<option value="model.shop.stg_orders""#),
        "stg_orders option present",
    );
    assert!(
        html.contains(r#"<option value="model.shop.fct_orders""#),
        "fct_orders option present",
    );
}

#[then("each impacted model carries a server-rendered SQL panel")]
fn each_model_has_sql_panel(world: &mut World) {
    let html = html(world);
    assert!(
        html.contains(r#"data-testid="macro-lens-model-panel""#),
        "a per-model panel renders",
    );
    assert!(
        html.contains(r#"data-testid="macro-lens-model-sql""#),
        "the inline model SQL renders server-side",
    );
    // Both models' panels (the selector toggles which is visible; both are
    // in the DOM — the server-render-everything zero-egress posture).
    assert!(html.contains(r#"data-model="model.shop.stg_orders""#));
    assert!(html.contains(r#"data-model="model.shop.fct_orders""#));
}

#[then("the macro-lens section shows the macro's first-order call sites")]
fn section_shows_call_sites(world: &mut World) {
    let html = html(world);
    assert!(
        html.contains(r#"data-testid="macro-lens-callsites""#),
        "the call-site list renders",
    );
    assert!(
        html.contains(r#"data-testid="macro-lens-callsite""#),
        "at least one call-site row renders",
    );
    assert!(
        html.contains("{{ add_dq_flags() }}") || html.contains("add_dq_flags(amount)"),
        "a call-site source line renders verbatim",
    );
}
