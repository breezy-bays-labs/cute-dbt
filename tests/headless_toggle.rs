//! Updated-only ↔ All-tests toggle behavior — exercised through the
//! rendered report via a real headless Chromium runtime (cute-dbt#91).
//!
//! Why this test exists:
//!
//! The toggle, the toggle-dependent per-model count, the 0-updated inline
//! hint, the foreground-updated default, and the auto-All-when-zero-updated
//! landing are all client-side JS over the inlined `cute-dbt-data` payload.
//! `render_integration.rs` runs NO browser (its `chrome_only` helper strips
//! asset bytes from static HTML — string + snapshot only), so without this
//! test the entire user-facing payoff would ship unverified. It drives the
//! real DOM: clicking the toggle, switching models, reading the selectors.
//!
//! Two synthetic reports are rendered to temp files:
//!
//! - **P1** — two in-scope models: `dim_a` (1 updated + 1 context test) and
//!   `dim_b` (1 test, 0 updated). Exercises the default Updated mode, the
//!   toggle-dependent counts, the 0-updated hint, the flip to All-tests
//!   (hint disappears), and `state.showAll` persistence across a model
//!   switch.
//! - **P2** — one model whose only test is context (`totalUpdated === 0`,
//!   the common SQL-only PR). Exercises the auto-All landing: the report
//!   must open in All-tests mode AND land on a real selected test with
//!   content (not the empty "No unit test selected" panel).
//!
//! ## Runtime cost
//!
//! Shares the same CI job as `headless_zero_egress` / `headless_csv_parser`
//! and is `#[ignore]` by default. One Chrome cold-start covers all the
//! headless tests. Locally:
//!
//! ```bash
//! cargo test --test headless_toggle -- --ignored
//! ```
//!
//! Tracked: breezy-bays-labs/cute-dbt#91.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use headless_chrome::protocol::cdp::Runtime;
use headless_chrome::{Browser, LaunchOptionsBuilder, Tab};

use cute_dbt::adapters::render::{ScopeSource, render_report};
use cute_dbt::domain::{
    Checksum, DEFAULT_REPORT_TITLE, DependsOn, InScopeSet, Manifest, ManifestMetadata,
    ModelInScopeSet, Node, NodeConfig, NodeId, UnitTest, UnitTestExpect,
};

// --- synthetic manifest builders ------------------------------------

fn model_node(full_id: &str) -> Node {
    Node::new(
        NodeId::new(full_id),
        "model",
        Checksum::new("sha256", "ck"),
        Some("select 1".to_owned()),
        None,
        DependsOn::default(),
        None,
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
}

fn unit_test(name: &str, model_bare: &str) -> UnitTest {
    UnitTest::new(
        name.to_owned(),
        NodeId::new(model_bare),
        Vec::new(),
        UnitTestExpect::new(serde_json::Value::Null, None),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
}

fn manifest(nodes: Vec<Node>, tests: Vec<(&str, UnitTest)>) -> Manifest {
    Manifest::new(
        ManifestMetadata::new("v12"),
        nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
        tests.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        HashMap::new(),
    )
}

fn tmp(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

/// Render a report for `(nodes, tests, models_in_scope, changed)` to a
/// temp file and return its `file://` URL.
fn render_to_file(
    filename: &str,
    nodes: Vec<Node>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    changed_ids: &[&str],
) -> String {
    let all_ids: Vec<String> = tests.iter().map(|(id, _)| (*id).to_owned()).collect();
    let m = manifest(nodes, tests);
    let in_scope: InScopeSet = all_ids.into_iter().collect();
    let models: ModelInScopeSet = model_ids.iter().map(|id| NodeId::new(*id)).collect();
    let changed: InScopeSet = changed_ids.iter().map(|s| (*s).to_owned()).collect();
    let out = tmp(filename);
    let _ = std::fs::remove_file(&out);
    render_report(
        &out,
        &m,
        &in_scope,
        &models,
        &changed,
        &HashMap::new(),
        "baseline.json",
        ScopeSource::Baseline,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let p = out.to_str().expect("report path is valid UTF-8");
    format!("file://{p}")
}

// --- headless evaluation helpers ------------------------------------

/// Evaluate `expr` in the page and return the (return-by-value) result.
fn eval(tab: &Tab, expr: &str) -> serde_json::Value {
    let r = tab
        .call_method(Runtime::Evaluate {
            expression: expr.to_string(),
            object_group: None,
            include_command_line_api: None,
            silent: Some(true),
            context_id: None,
            return_by_value: Some(true),
            generate_preview: None,
            user_gesture: Some(true),
            await_promise: Some(false),
            throw_on_side_effect: None,
            timeout: None,
            disable_breaks: None,
            repl_mode: None,
            allow_unsafe_eval_blocked_by_csp: None,
            unique_context_id: None,
            serialization_options: None,
        })
        .expect("evaluate expression");
    r.result.value.unwrap_or(serde_json::Value::Null)
}

fn eval_string(tab: &Tab, expr: &str) -> String {
    eval(tab, expr)
        .as_str()
        .map(str::to_owned)
        .unwrap_or_default()
}

fn eval_bool(tab: &Tab, expr: &str) -> bool {
    eval(tab, expr).as_bool().unwrap_or(false)
}

/// `|`-joined option labels of a `<select>`, trimmed per option.
fn options_of(tab: &Tab, select_id: &str) -> String {
    eval_string(
        tab,
        &format!(
            "Array.from(document.querySelectorAll('#{select_id} option'))\
             .map(function(o){{return o.textContent.trim();}}).join('|')"
        ),
    )
}

fn hint_hidden(tab: &Tab) -> bool {
    eval_bool(
        tab,
        "document.querySelector('[data-testid=\"zero-updated-hint\"]').hidden",
    )
}

fn all_mode_active(tab: &Tab) -> bool {
    eval_bool(
        tab,
        "document.querySelector('[data-testid=\"updated-toggle\"] [data-test-mode=\"all\"]')\
         .classList.contains('is-active')",
    )
}

fn click_mode(tab: &Tab, mode: &str) {
    let _ = eval(
        tab,
        &format!(
            "document.querySelector('[data-testid=\"updated-toggle\"] [data-test-mode=\"{mode}\"]').click()"
        ),
    );
}

fn select_model(tab: &Tab, model: &str) {
    let _ = eval(
        tab,
        &format!(
            "(function(){{var s=document.querySelector('#model-select');\
             s.value='{model}';s.dispatchEvent(new Event('change'));}})()"
        ),
    );
}

fn launch_browser() -> Browser {
    let chrome_path = std::env::var_os("CHROME").map(PathBuf::from);
    let host_resolver = OsStr::new("--host-resolver-rules=MAP * ~NOTFOUND");
    let no_first_run = OsStr::new("--no-first-run");
    let no_default_check = OsStr::new("--no-default-browser-check");
    let disable_breakpad = OsStr::new("--disable-breakpad");

    let mut builder = LaunchOptionsBuilder::default();
    builder.headless(true).sandbox(false).args(vec![
        host_resolver,
        no_first_run,
        no_default_check,
        disable_breakpad,
    ]);
    if let Some(p) = chrome_path.as_ref() {
        builder.path(Some(p.clone()));
    }
    let opts = builder.build().expect("LaunchOptions must build");
    Browser::new(opts).expect("Chromium must launch")
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn updated_toggle_drives_visibility_counts_hint_and_auto_all() {
    // P1: dim_a has one updated (`upd`) + one context (`ctx`) test;
    // dim_b has one context test (`bee`) → 0 updated. Sorted-by-id order
    // within dim_a is [ctx, upd] (the id `…dim_a.ctx` < `…dim_a.upd`).
    let p1 = render_to_file(
        "headless_toggle_p1.html",
        vec![
            model_node("model.shop.dim_a"),
            model_node("model.shop.dim_b"),
        ],
        vec![
            ("unit_test.shop.dim_a.ctx", unit_test("ctx", "dim_a")),
            ("unit_test.shop.dim_a.upd", unit_test("upd", "dim_a")),
            ("unit_test.shop.dim_b.bee", unit_test("bee", "dim_b")),
        ],
        &["model.shop.dim_a", "model.shop.dim_b"],
        &["unit_test.shop.dim_a.upd"],
    );

    // P2: one model whose only test is context → totalUpdated === 0.
    let p2 = render_to_file(
        "headless_toggle_p2.html",
        vec![model_node("model.shop.dim_c")],
        vec![("unit_test.shop.dim_c.only", unit_test("only", "dim_c"))],
        &["model.shop.dim_c"],
        &[],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");

    // ===== P1 =====
    tab.navigate_to(&p1).expect("navigate P1");
    tab.wait_until_navigated().expect("await P1 navigation");

    // Default: Updated-only mode (totalUpdated >= 1), default model is the
    // first with an updated test (dim_a). Per-model counts are the UPDATED
    // counts; dim_a's test list shows only the updated test.
    assert!(!all_mode_active(&tab), "default opens in Updated-only mode");
    assert_eq!(
        options_of(&tab, "model-select"),
        "dim_a  (1)|dim_b  (0)",
        "Updated-mode model counts are the updated counts",
    );
    assert_eq!(
        options_of(&tab, "test-select"),
        "upd",
        "Updated mode lists only the updated test for dim_a",
    );
    assert!(
        hint_hidden(&tab),
        "no 0-updated hint for a model with updates"
    );

    // Select the 0-updated model (dim_b) in Updated mode: empty test list
    // + the inline hint appears.
    select_model(&tab, "dim_b");
    assert_eq!(
        options_of(&tab, "test-select"),
        "",
        "a 0-updated model has an empty test list in Updated mode",
    );
    assert!(
        !hint_hidden(&tab),
        "the 0-updated hint is shown for dim_b in Updated mode",
    );

    // Flip to All-tests (still on dim_b): the hint disappears, all tests
    // show, and the per-model counts become totals.
    click_mode(&tab, "all");
    assert!(all_mode_active(&tab), "All-tests mode is now active");
    assert!(
        hint_hidden(&tab),
        "the 0-updated hint disappears once flipped to All tests",
    );
    assert_eq!(
        options_of(&tab, "test-select"),
        "bee",
        "All mode shows dim_b's (sole, context) test",
    );
    assert_eq!(
        options_of(&tab, "model-select"),
        "dim_a  (2)|dim_b  (1)",
        "All-mode model counts are the totals",
    );

    // state.showAll persists across a model switch: pick dim_a again — it
    // stays in All-tests mode (both tests visible, total count shown).
    select_model(&tab, "dim_a");
    assert!(
        all_mode_active(&tab),
        "showAll persists across a model switch",
    );
    assert_eq!(
        options_of(&tab, "test-select"),
        "ctx|upd",
        "All mode shows both of dim_a's tests after the switch",
    );

    // ===== P2 (auto-All landing) =====
    tab.navigate_to(&p2).expect("navigate P2");
    tab.wait_until_navigated().expect("await P2 navigation");

    // totalUpdated === 0 → the report auto-opens in All-tests mode and
    // lands on a REAL selected test (not the empty 0-updated view).
    assert!(
        all_mode_active(&tab),
        "a 0-updated diff auto-opens in All-tests mode",
    );
    assert_eq!(
        options_of(&tab, "test-select"),
        "only",
        "the single context test is visible under auto-All",
    );
    assert_eq!(
        eval_string(&tab, "document.querySelector('#test-select').value"),
        "unit_test.shop.dim_c.only",
        "auto-All lands on a real selected test, not the empty panel",
    );
    assert!(
        hint_hidden(&tab),
        "no 0-updated hint under auto-All (All-tests mode)",
    );

    let _ = tab.close(true);
}
