//! Step definitions for `features/pr_comments.feature`
//! (cute-dbt#419 / #420 / #421 / #422, epic #353 — the report's inline PR
//! review-comments surface).
//!
//! Subprocess wire round-trip (the BDD house style, the
//! `macro_perspective.rs` precedent): each scenario runs the **real**
//! `cute-dbt` binary against the committed comments-showcase fixtures (the
//! shared `prdiff-minidag` manifest + patch + source — so the synthetic
//! review threads anchor onto real rendered diff lines) plus the synthetic
//! `--pr-comments @<file>` review-threads payload (the deterministic
//! injection seam standing in for the live `gh` fetch). The experimental
//! switch is opted in exactly like a consumer (the `CUTE_DBT_EXPERIMENTAL`
//! env surface, default-OFF). The assertions read the rendered HTML: the
//! embedded `DATA.pr_comments` payload (the per-model buckets + counts) and
//! the always-present static containers the JS fills.
//!
//! The shared `#[then("the exit code is 0")]` lives in
//! `report_generation.rs` (cucumber steps are global) — this module owns
//! only the comments-specific Given/When/Then.

use cucumber::{given, then, when};
use serde_json::Value;

use super::super::common;
use super::World;

// --- fixtures --------------------------------------------------------

const CURRENT: &str = "prdiff-minidag-current.json";
const PATCH: &str = "prdiff-minidag-pr-diff.patch";
const PROJECT_ROOT: &str = "prdiff-minidag-source";
const PR_COMMENTS: &str = "comments-showcase-pr-comments.json";

// --- Given -----------------------------------------------------------

#[given("the comments-showcase manifest and PR diff")]
fn comments_showcase_inputs(_world: &mut World) {
    // The committed fixtures are the subject; nothing to stage. (The
    // When step references them directly via `common::fixture`.) This
    // Given documents the scenario's subject in the spec prose.
}

#[given("the experimental switch enables pr-comments")]
fn experimental_switch_enables_pr_comments(world: &mut World) {
    world.experimental_env = Some("pr-comments".to_owned());
}

#[given("the PR carries synthetic review comments")]
fn pr_carries_review_comments(_world: &mut World) {
    // Documents the scenario subject: the synthetic review-threads fixture
    // exists. Whether the run actually passes `--pr-comments @<fixture>` is
    // controlled by the When step ("with the PR comments" vs "without PR
    // comments"), so this Given stages nothing — it keeps the spec prose
    // readable without a redundant flag on `World`.
}

// --- When ------------------------------------------------------------

#[when("I run cute-dbt report with the PR comments")]
fn run_with_pr_comments(world: &mut World) {
    run(world, true);
}

#[when("I run cute-dbt report without PR comments")]
fn run_without_pr_comments(world: &mut World) {
    run(world, false);
}

/// Run the real `cute-dbt report` against the committed comments-showcase
/// fixtures. `with_comments` controls whether `--pr-comments @<fixture>` is
/// passed (the "without PR comments" degrade scenario sets it false);
/// `world.experimental_env` controls the gate (a scenario that never set it
/// runs experiment-OFF).
fn run(world: &mut World, with_comments: bool) {
    let manifest = common::fixture(CURRENT);
    let patch = common::fixture(PATCH);
    let project_root = common::fixture(PROJECT_ROOT);
    let out = common::tmp("pr_comments_report.html");
    common::clear(&out);

    let scope_arg = format!("@{}", common::s(&patch));
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cute-dbt"));
    cmd.args([
        "report",
        "--manifest",
        common::s(&manifest),
        "--pr-diff",
        &scope_arg,
        "--project-root",
        common::s(&project_root),
        "--out",
        common::s(&out),
    ]);
    if with_comments {
        let comments = common::fixture(PR_COMMENTS);
        cmd.args(["--pr-comments", &format!("@{}", common::s(&comments))]);
    }
    cmd.env_remove("CUTE_DBT_EXPERIMENTAL");
    if let Some(value) = &world.experimental_env {
        cmd.env("CUTE_DBT_EXPERIMENTAL", value);
    }

    let output = cmd.output().expect("the cute-dbt binary spawns");
    world.last_exit_code = output.status.code();
    world.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    world.report_html = std::fs::read_to_string(&out).ok();
    if let Some(html) = &world.report_html {
        // The inlined comments must never introduce an outbound request.
        common::assert_no_external_refs(html);
    }
    world.out_path = Some(out);
}

// --- Then ------------------------------------------------------------

fn html(world: &World) -> &str {
    world
        .report_html
        .as_deref()
        .unwrap_or_else(|| panic!("report.html was not written; stderr={}", world.last_stderr))
}

/// Extract and parse the embedded `<script type="application/json">` payload.
fn payload(world: &World) -> Value {
    let html = html(world);
    let start = html
        .find("type=\"application/json\"")
        .expect("the report embeds a JSON payload script");
    let after = &html[start..];
    let open = after.find('>').expect("script open tag closes") + 1;
    let body = &after[open..];
    let end = body.find("</script>").expect("the JSON script closes");
    // The Rust-side escaper turns tag-opening `<` into `<`; serde_json
    // round-trips the original characters, so a plain replace recovers them.
    let json = body[..end].replace("\\u003c", "<");
    serde_json::from_str(&json).expect("the embedded payload is valid JSON")
}

#[then("the report carries the PR review-comments payload")]
fn report_carries_payload(world: &mut World) {
    let p = payload(world);
    assert!(
        p.get("pr_comments").is_some(),
        "DATA.pr_comments must be present when comments are surfaced; stderr={}",
        world.last_stderr,
    );
}

#[then("the report carries no PR review-comments payload")]
fn report_carries_no_payload(world: &mut World) {
    let p = payload(world);
    assert!(
        p.get("pr_comments").is_none(),
        "DATA.pr_comments must be ABSENT when the surface is off / no comments \
         (byte-stable default goldens); stderr={}",
        world.last_stderr,
    );
}

#[then(regex = r"^the comment payload reports a total of (\d+)$")]
fn payload_reports_total(world: &mut World, total: u64) {
    let p = payload(world);
    let got = p["pr_comments"]["total"]
        .as_u64()
        .expect("total is a number");
    assert_eq!(got, total, "the report-wide comment total");
}

#[then(regex = r"^the model (\S+) carries a comment count of (\d+)$")]
fn model_carries_count(world: &mut World, model: String, count: u64) {
    let p = payload(world);
    let buckets = p["pr_comments"]["by_model"]
        .as_array()
        .expect("by_model is an array");
    let bucket = buckets
        .iter()
        .find(|b| {
            b["model"]
                .as_str()
                .is_some_and(|m| m.ends_with(&format!(".{model}")))
        })
        .unwrap_or_else(|| panic!("no comment bucket for model {model}"));
    assert_eq!(
        bucket["count"].as_u64().expect("count is a number"),
        count,
        "comment count for {model}",
    );
}

#[then("the comment payload carries an outdated thread with no live line")]
fn payload_has_outdated_thread(world: &mut World) {
    let p = payload(world);
    let buckets = p["pr_comments"]["by_model"]
        .as_array()
        .expect("by_model is an array");
    let has_outdated = buckets.iter().any(|b| {
        b["threads"].as_array().is_some_and(|threads| {
            threads
                .iter()
                .any(|t| t["outdated"].as_bool() == Some(true) && t.get("line").is_none())
        })
    });
    assert!(
        has_outdated,
        "an outdated thread (outdated=true, no live `line`) must be carried honestly",
    );
}

#[then("the report carries the top comment-count navigation container")]
fn report_carries_top_count_container(world: &mut World) {
    assert!(
        html(world).contains(r#"data-testid="pr-comments-count""#),
        "the always-present top-of-report count container must be in the DOM",
    );
}

#[then("the report carries the per-model comment-count container")]
fn report_carries_per_model_container(world: &mut World) {
    assert!(
        html(world).contains(r#"data-testid="model-comments""#),
        "the always-present per-model count container must be in the DOM",
    );
}
