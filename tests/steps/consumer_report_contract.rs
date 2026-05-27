//! Step definitions for `features/consumer_report_contract.feature` —
//! the formal articulation of cute-dbt's expected report properties for
//! the CI sticky-comment use case (cute-dbt#71). The recipe at
//! `book/src/recipes/ci-sticky-comment.md` documents the workflow
//! shape; these scenarios pin the structural properties of the rendered
//! report that make that workflow useful.
//!
//! Doc-feature framing: the Given + When steps are reused from
//! `unit_test_format_coverage.rs` (the rendered playground report is
//! the same artifact regardless of which contract we're asserting);
//! the Then steps below delegate to existing test infrastructure
//! (`common::assert_no_external_refs`, embedded-payload parsing,
//! `std::fs::metadata`). The .feature file is the formal statement of
//! the contract; these step defs are thin glue over the existing
//! proof.

use cucumber::then;
use serde_json::Value;

use super::super::common;
use super::World;

/// Soft size budget for the rendered report's single-file HTML. Not a
/// GitHub Actions artifact upload limit (those are generously sized);
/// the budget is for the reviewer's local download + browser-open
/// experience. Today's committed examples are ~3.5 MB each; 10 MB
/// gives ~3x headroom before this gate fires.
const REPORT_SIZE_BUDGET_BYTES: u64 = 10 * 1024 * 1024;

#[then("the resulting HTML contains zero external resource references")]
fn then_zero_external_refs(world: &mut World) {
    assert_eq!(
        world.last_exit_code,
        Some(0),
        "cute-dbt failed; stderr={}",
        world.last_stderr,
    );
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    common::assert_no_external_refs(html);
}

#[then(regex = r#"^the resulting HTML embeds the "([^"]+)" payload with at least one model$"#)]
fn then_embeds_payload_with_models(world: &mut World, payload_id: String) {
    let html = world
        .report_html
        .as_ref()
        .expect("report.html was written by the subprocess");
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("report HTML must parse");
    let parser = dom.parser();
    let node = dom
        .get_element_by_id(payload_id.as_str())
        .unwrap_or_else(|| panic!("report must include <script id={payload_id:?}>"))
        .get(parser)
        .expect("payload node resolves");
    let raw = node.inner_text(parser);
    let payload: Value = serde_json::from_str(&raw).expect("embedded payload must be valid JSON");
    let models = payload["models"]
        .as_array()
        .expect("payload.models is an array");
    assert!(
        !models.is_empty(),
        "expected at least one model in the rendered payload; got 0 — the modified-set \
         selection is not surfacing any models, which would defeat the sticky-comment \
         affordance for the reviewer",
    );
}

#[then("the resulting HTML file size is under 10 megabytes")]
fn then_under_artifact_size_budget(world: &mut World) {
    let path = world
        .out_path
        .as_ref()
        .expect("subprocess wrote --out path");
    let size = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .len();
    assert!(
        size < REPORT_SIZE_BUDGET_BYTES,
        "report HTML at {} is {} bytes (>= {} byte budget) — too large for a comfortable \
         artifact download in CI sticky-comment review",
        path.display(),
        size,
        REPORT_SIZE_BUDGET_BYTES,
    );
}
