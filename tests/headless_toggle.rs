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
//! Shares the same CI job as `headless_zero_egress` and is `#[ignore]` by
//! default. One Chrome cold-start covers all the headless tests. Locally:
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

use cute_dbt::adapters::manifest::FileManifestSource;
use cute_dbt::adapters::render::{
    ExternalFixtures, LoadedFixture, ScopeSource, render_report, render_report_with_externals,
};
use cute_dbt::domain::{
    BlockDiff, Checksum, DEFAULT_REPORT_TITLE, DependsOn, DiffLine, DiffLineKind, FileHunks, Hunk,
    InScopeSet, Manifest, ManifestMetadata, ModelInScopeSet, Node, NodeConfig, NodeId,
    NormalizedDiffIndex, PrDiff, TestMetadata, UnitTest, UnitTestDataDiff, UnitTestExpect,
    UnitTestGiven, UnitTestYamlBlock, external_fixture_table, reconstruct_table_diffs,
};
use cute_dbt::ports::ManifestSource;

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

/// A `model` node carrying `raw_code` (so the Model SQL section renders)
/// — cute-dbt#111.
fn model_node_with_raw(full_id: &str, raw_code: &str) -> Node {
    Node::new(
        NodeId::new(full_id),
        "model",
        Checksum::new("sha256", "ck"),
        Some("select 1".to_owned()),
        Some(raw_code.to_owned()),
        DependsOn::default(),
        None,
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
}

/// A `model` node carrying `raw_code` AND `original_file_path` — the
/// cute-dbt#179 Model-SQL code-card full-path header rides the latter.
fn model_node_with_raw_and_path(full_id: &str, raw_code: &str, path: &str) -> Node {
    Node::new(
        NodeId::new(full_id),
        "model",
        Checksum::new("sha256", "ck"),
        Some("select 1".to_owned()),
        Some(raw_code.to_owned()),
        DependsOn::default(),
        Some(path.to_owned()),
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
        UnitTestExpect::new(serde_json::Value::Null, None, None),
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

/// Render a report for `(nodes, tests, models_in_scope, changed)` under
/// `scope` to a temp file and return its `file://` URL.
fn render_with_scope(
    filename: &str,
    nodes: Vec<Node>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    changed_ids: &[&str],
    scope: ScopeSource,
    baseline_label: &str,
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
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        baseline_label,
        scope,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let p = out.to_str().expect("report path is valid UTF-8");
    format!("file://{p}")
}

/// Baseline-mode render (the #91 toggle tests).
fn render_to_file(
    filename: &str,
    nodes: Vec<Node>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    changed_ids: &[&str],
) -> String {
    render_with_scope(
        filename,
        nodes,
        tests,
        model_ids,
        changed_ids,
        ScopeSource::Baseline,
        "baseline.json",
    )
}

/// PR-diff-mode render (cute-dbt#96 — the block-precision affirmation lives
/// on this path only). No baseline manifest, so the label is empty.
fn render_pr_diff_to_file(
    filename: &str,
    nodes: Vec<Node>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    changed_ids: &[&str],
) -> String {
    render_with_scope(
        filename,
        nodes,
        tests,
        model_ids,
        changed_ids,
        ScopeSource::PrDiff,
        "",
    )
}

// --- headless evaluation helpers ------------------------------------

/// Evaluate `expr` in the page and return the (return-by-value) result.
///
/// FAILS LOUDLY on a thrown JS exception (cute-dbt#109): `silent: true`
/// only suppresses the `Runtime.exceptionThrown` event/debugger pause —
/// the `Runtime.evaluate` response still carries `exception_details`,
/// which we surface as a panic. A missing DOM node must never coerce to
/// a benign `Null` that lets a negative assertion pass vacuously.
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
    if let Some(exc) = r.exception_details {
        let detail = exc
            .exception
            .as_ref()
            .and_then(|e| e.description.clone())
            .unwrap_or(exc.text);
        panic!("JS evaluation threw for `{expr}`: {detail}");
    }
    r.result.value.unwrap_or(serde_json::Value::Null)
}

fn eval_string(tab: &Tab, expr: &str) -> String {
    match eval(tab, expr) {
        serde_json::Value::String(s) => s,
        v => panic!("JS evaluation of `{expr}` returned non-string {v:?} (cute-dbt#109)"),
    }
}

fn eval_bool(tab: &Tab, expr: &str) -> bool {
    let v = eval(tab, expr);
    v.as_bool().unwrap_or_else(|| {
        panic!(
            "JS evaluation of `{expr}` returned non-boolean {v:?} — a missing/renamed \
             DOM hook must fail loudly, never read as a benign false (cute-dbt#109)"
        )
    })
}

/// Condition-based document-readiness wait after a reload / re-navigation
/// (cute-dbt#208). Call it after every `tab.reload(..)` or same-tab
/// `tab.navigate_to(..)` + `wait_until_navigated()` pair, BEFORE the next
/// eval.
///
/// `wait_until_navigated` resolves on the CDP navigation event, which can
/// fire while the new document is still being swapped in —
/// `document.documentElement` is briefly null mid-swap, so any eval touching
/// it throws (`TypeError: Cannot read properties of null`; lost on PR #205's
/// and #207's CI under load). Poll `document.readyState` on a 50ms interval
/// until it reports `complete`; a protocol error or a thrown eval mid-swap
/// counts as "not ready yet — keep polling", never as a failure. The 10s cap
/// is a guardrail against a wedged tab, not the wait mechanism (no bare
/// sleeps as the wait).
fn wait_for_document_ready(tab: &Tab) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        // Raw Runtime::Evaluate, deliberately NOT the fail-loud `eval`
        // helper: mid-swap the evaluation may error or throw, and readiness
        // polling must treat both as "not yet" — while `eval` (correctly,
        // cute-dbt#109) panics on a thrown exception.
        let ready = tab
            .call_method(Runtime::Evaluate {
                expression: "document.readyState".to_string(),
                object_group: None,
                include_command_line_api: None,
                silent: Some(true),
                context_id: None,
                return_by_value: Some(true),
                generate_preview: None,
                user_gesture: None,
                await_promise: Some(false),
                throw_on_side_effect: None,
                timeout: None,
                disable_breaks: None,
                repl_mode: None,
                allow_unsafe_eval_blocked_by_csp: None,
                unique_context_id: None,
                serialization_options: None,
            })
            .ok()
            .filter(|r| r.exception_details.is_none())
            .and_then(|r| r.result.value)
            .is_some_and(|v| v.as_str() == Some("complete"));
        if ready {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "document never reached readyState 'complete' within 10s of the \
             reload/navigation (cute-dbt#208)"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
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

/// `true` when the PR-diff block-precision affirmation element is in the DOM
/// at all (it is server-rendered only on the PR-diff path; cute-dbt#96).
fn affirm_present(tab: &Tab) -> bool {
    eval_bool(
        tab,
        "document.querySelector('[data-testid=\"zero-updated-affirm\"]') !== null",
    )
}

fn affirm_hidden(tab: &Tab) -> bool {
    eval_bool(
        tab,
        "document.querySelector('[data-testid=\"zero-updated-affirm\"]').hidden",
    )
}

fn affirm_text(tab: &Tab) -> String {
    eval_string(
        tab,
        "document.querySelector('[data-testid=\"zero-updated-affirm\"]').textContent.trim()",
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
    launch_browser_sized(None)
}

/// Like `launch_browser`, but with an optional launch window size
/// (`--window-size=W,H`). In headless Chrome `window.innerWidth` tracks the
/// launch window size, so this is the lever that drives a narrow (mobile)
/// layout for the cute-dbt#157 viewport regression — `None` preserves the
/// historic default-width behaviour every other headless test relies on.
fn launch_browser_sized(window_size: Option<(u32, u32)>) -> Browser {
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
    if let Some(size) = window_size {
        builder.window_size(Some(size));
    }
    if let Some(p) = chrome_path.as_ref() {
        builder.path(Some(p.clone()));
    }
    let opts = builder.build().expect("LaunchOptions must build");
    Browser::new(opts).expect("Chromium must launch")
}

// --- harness self-checks (cute-dbt#109) -------------------------------
//
// The eval helpers must FAIL LOUDLY on a thrown JS exception. Before
// cute-dbt#109 the helper coerced a throw to `Null` → `false`/`""`/`-1`,
// so a missing DOM node (`document.querySelector(...)` returning `null`
// and the `.hidden`/`.classList` read then throwing) read as a benign
// `false` — letting negative assertions like `assert!(!hint_hidden(..))`
// pass even when the queried node was absent entirely (e.g. a renderer
// regression dropping a `data-testid` hook). These self-checks pin the
// fail-loud contract; they evaluate against the fresh tab's blank page
// (no report needed — the contract under test is the harness itself).

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
#[should_panic(expected = "JS evaluation threw")]
fn harness_eval_fails_loudly_when_js_throws() {
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    // A missing DOM node turns the property read into a TypeError — the
    // exact masking class from cute-dbt#109. Must panic, never read Null.
    eval(
        &tab,
        "document.querySelector('[data-testid=\"not-in-the-dom\"]').hidden",
    );
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
#[should_panic(expected = "non-boolean")]
fn harness_eval_bool_fails_loudly_on_non_boolean_result() {
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    // A non-boolean result must never coerce to a benign `false`.
    eval_bool(&tab, "'truthy-but-not-a-bool'");
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
    wait_for_document_ready(&tab);

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

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn pr_diff_zero_updated_affirms_block_precision() {
    // cute-dbt#96 (CPO Finding A): when block-precision narrows every test to
    // context (0 updated — the common SQL-only / outside-the-block PR), the
    // PR-diff report must AFFIRM that block-precision ran with the LOCKED copy
    // "no unit-test definitions changed in this diff" — never a scoping-failure
    // phrasing. The copy is PR-diff-specific; baseline mode must not show it.

    // PR-diff, 0 updated → the affirmation is visible.
    let pr_zero = render_pr_diff_to_file(
        "headless_affirm_pr_zero.html",
        vec![model_node("model.shop.dim_z")],
        vec![("unit_test.shop.dim_z.only", unit_test("only", "dim_z"))],
        &["model.shop.dim_z"],
        &[],
    );
    // PR-diff, 1 updated → the affirmation is present but hidden.
    let pr_updated = render_pr_diff_to_file(
        "headless_affirm_pr_updated.html",
        vec![model_node("model.shop.dim_y")],
        vec![("unit_test.shop.dim_y.upd", unit_test("upd", "dim_y"))],
        &["model.shop.dim_y"],
        &["unit_test.shop.dim_y.upd"],
    );
    // Baseline mode, 0 updated → the affirmation element is absent entirely.
    let baseline_zero = render_to_file(
        "headless_affirm_baseline_zero.html",
        vec![model_node("model.shop.dim_z")],
        vec![("unit_test.shop.dim_z.only", unit_test("only", "dim_z"))],
        &["model.shop.dim_z"],
        &[],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");

    // ===== PR-diff, 0 updated =====
    tab.navigate_to(&pr_zero).expect("navigate pr_zero");
    tab.wait_until_navigated()
        .expect("await pr_zero navigation");
    assert!(
        all_mode_active(&tab),
        "a 0-updated PR-diff auto-opens in All-tests mode",
    );
    assert!(
        affirm_present(&tab),
        "the PR-diff path server-renders the affirmation element",
    );
    assert!(
        !affirm_hidden(&tab),
        "the affirmation is visible when the diff updated zero tests",
    );
    assert_eq!(
        affirm_text(&tab),
        "no unit-test definitions changed in this diff",
        "the LOCKED block-precision affirmation copy (CPO Finding A)",
    );

    // ===== PR-diff, 1 updated =====
    tab.navigate_to(&pr_updated).expect("navigate pr_updated");
    tab.wait_until_navigated()
        .expect("await pr_updated navigation");
    wait_for_document_ready(&tab);
    assert!(
        affirm_present(&tab),
        "the element is server-rendered on the PR-diff path regardless of count",
    );
    assert!(
        affirm_hidden(&tab),
        "the affirmation is hidden when at least one test is updated",
    );

    // ===== Baseline, 0 updated =====
    tab.navigate_to(&baseline_zero)
        .expect("navigate baseline_zero");
    tab.wait_until_navigated()
        .expect("await baseline_zero navigation");
    wait_for_document_ready(&tab);
    assert!(
        !affirm_present(&tab),
        "baseline mode never renders the PR-diff-specific affirmation",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#96 concern 2: inline YAML diff drawer -----------------

/// PR-diff render that injects an authoring-YAML map and an inline-diff map
/// so the Authored↔Diff drawer renders. `authoring` is `(test_id, raw)`;
/// `diffs` is `(test_id, BlockDiff)`. The block span is irrelevant to
/// the JS drawer (only `raw` + the diff lines are surfaced), so it is pinned
/// to `[1, line_count]`.
fn render_pr_diff_with_diffs(
    filename: &str,
    nodes: Vec<Node>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    changed_ids: &[&str],
    authoring: Vec<(&str, &str)>,
    diffs: Vec<(&str, BlockDiff)>,
) -> String {
    let all_ids: Vec<String> = tests.iter().map(|(id, _)| (*id).to_owned()).collect();
    let m = manifest(nodes, tests);
    let in_scope: InScopeSet = all_ids.into_iter().collect();
    let models: ModelInScopeSet = model_ids.iter().map(|id| NodeId::new(*id)).collect();
    let changed: InScopeSet = changed_ids.iter().map(|s| (*s).to_owned()).collect();
    let authoring_yaml: HashMap<String, UnitTestYamlBlock> = authoring
        .into_iter()
        .map(|(id, raw)| {
            let n = raw.split('\n').count();
            (
                id.to_owned(),
                UnitTestYamlBlock::new(raw.to_owned(), 1, 1, n),
            )
        })
        .collect();
    let yaml_diffs: HashMap<String, BlockDiff> = diffs
        .into_iter()
        .map(|(id, d)| (id.to_owned(), d))
        .collect();
    let out = tmp(filename);
    let _ = std::fs::remove_file(&out);
    render_report(
        &out,
        &m,
        &in_scope,
        &models,
        &changed,
        &authoring_yaml,
        &yaml_diffs,
        &HashMap::new(),
        &HashMap::new(),
        "",
        ScopeSource::PrDiff,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let p = out.to_str().expect("report path is valid UTF-8");
    format!("file://{p}")
}

fn dl(kind: DiffLineKind, text: &str, emphasis: Option<(usize, usize)>) -> DiffLine {
    DiffLine {
        kind,
        text: text.to_owned(),
        emphasis,
    }
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn yaml_diff_drawer_defaults_to_diff_and_toggles_to_authored() {
    // dim_a's `upd` test was edited (model: payments → orders); the run loop
    // attaches an inline diff. PR-diff mode auto-selects the updated test, so
    // its drawer is built on load.
    let diff = BlockDiff {
        lines: vec![
            dl(DiffLineKind::Context, "  - name: upd", None),
            dl(DiffLineKind::Removed, "    model: payments", Some((11, 18))),
            dl(DiffLineKind::Added, "    model: orders", Some((11, 16))),
            dl(DiffLineKind::Context, "    given: []", None),
        ],
    };
    let url = render_pr_diff_with_diffs(
        "headless_yaml_diff.html",
        vec![model_node("model.shop.dim_a")],
        vec![("unit_test.shop.dim_a.upd", unit_test("upd", "dim_a"))],
        &["model.shop.dim_a"],
        &["unit_test.shop.dim_a.upd"],
        vec![(
            "unit_test.shop.dim_a.upd",
            "  - name: upd\n    model: orders\n    given: []",
        )],
        vec![("unit_test.shop.dim_a.upd", diff)],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // The edited test's drawer summary names the diff, and the Diff view is
    // the default (Authored hidden).
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.authoring-yaml > summary').textContent.trim()"
        ),
        "Authoring YAML — diff",
        "an edited test's drawer summary is 'Authoring YAML — diff'",
    );
    assert!(
        eval_bool(&tab, "document.querySelector('.yaml-diff-toggle') !== null"),
        "the Authored↔Diff toggle is present",
    );
    assert!(
        !eval_bool(&tab, "document.querySelector('.yaml-diff-view').hidden"),
        "the Diff view is the default (visible)",
    );
    assert!(
        eval_bool(&tab, "document.querySelector('.yaml-authored-view').hidden"),
        "the Authored view starts hidden",
    );
    // Intra-line emphasis renders as <strong>; both change lines present.
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.yaml-diff-view .diff-removed strong') !== null"
        ),
        "the removed line carries an intra-line emphasis <strong>",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.yaml-diff-view .diff-added') !== null"
        ),
        "the diff renders the added line",
    );

    // Clicking "Authored" flips the two views.
    let _ = eval(
        &tab,
        "document.querySelector('.yaml-view-btn[data-view=\"authored\"]').click()",
    );
    assert!(
        eval_bool(&tab, "document.querySelector('.yaml-diff-view').hidden"),
        "the Diff view hides after switching to Authored",
    );
    assert!(
        !eval_bool(&tab, "document.querySelector('.yaml-authored-view').hidden"),
        "the Authored view shows after the toggle",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn override_only_edit_shows_text_diff_and_no_spurious_cell_diff() {
    // cute-dbt#125 — the rendered-visibility proof (the domain test
    // `override_only_edit_surfaces_in_text_diff_but_not_cell_diff_end_to_end`
    // pins the payload computation; this pins what the reviewer SEES). A
    // unit test with non-empty given/expect rows whose ONLY edit is inside
    // the `overrides:` block (all 3 dbt override kinds present): the change
    // must surface in the Authoring-YAML Diff view, while the given/expect
    // cell grids carry NO cell-diff (data_diff is None → Current-only),
    // so the drawer is the sole place the override change shows.
    let id = "unit_test.shop.dim_a.override_only";
    let raw = [
        "  - name: override_only",
        "    model: dim_a",
        "    overrides:",
        "      macros:",
        "        is_incremental: false",
        "      vars:",
        "        cutoff_days: 30",
        "      env_vars:",
        "        DBT_REGION: us-east-1",
        "    given:",
        "      - input: ref('orders')",
        "        format: dict",
        "        rows:",
        "          - id: 1",
        "            amount: 100",
        "    expect:",
        "      format: dict",
        "      rows:",
        "        - id: 1",
        "          total: 100",
    ]
    .join("\n");
    // The reconstructed block diff: the whole entry as context, with ONLY the
    // `vars.cutoff_days` line as a removed/added change pair. Sibling override
    // keys + the given/expect data ride along as context (the slice spans the
    // whole `- name:` entry — the #125 invariant).
    let diff = BlockDiff {
        lines: vec![
            dl(DiffLineKind::Context, "  - name: override_only", None),
            dl(DiffLineKind::Context, "    model: dim_a", None),
            dl(DiffLineKind::Context, "    overrides:", None),
            dl(DiffLineKind::Context, "      macros:", None),
            dl(DiffLineKind::Context, "        is_incremental: false", None),
            dl(DiffLineKind::Context, "      vars:", None),
            // emphasis = the changed VALUE codepoints. `cutoff_days: ` is 13
            // chars after the 8-space indent (indices 8..=20), so the value
            // starts at codepoint 21 (Gemini PR#144): `7` → [21,22), `30` →
            // [21,23). The assertions below key on the change LINES, not the
            // emphasis, but the mock should still describe a faithful diff.
            dl(
                DiffLineKind::Removed,
                "        cutoff_days: 7",
                Some((21, 22)),
            ),
            dl(
                DiffLineKind::Added,
                "        cutoff_days: 30",
                Some((21, 23)),
            ),
            dl(DiffLineKind::Context, "      env_vars:", None),
            dl(DiffLineKind::Context, "        DBT_REGION: us-east-1", None),
        ],
    };

    let ut = UnitTest::new(
        "override_only",
        NodeId::new("dim_a"),
        vec![UnitTestGiven::new(
            "ref('orders')",
            serde_json::json!([{"id": 1, "amount": 100}]),
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(
            serde_json::json!([{"id": 1, "total": 100}]),
            Some("dict".to_owned()),
            None,
        ),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    );

    let url = render_pr_diff_with_diffs(
        "headless_override_only.html",
        vec![model_node("model.shop.dim_a")],
        vec![(id, ut)],
        &["model.shop.dim_a"],
        &[id],
        vec![(id, &raw)],
        vec![(id, diff)],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // (1) The override change is SHOWN — the Diff view (default) carries the
    // new override value as an added line.
    assert!(
        !eval_bool(&tab, "document.querySelector('.yaml-diff-view').hidden"),
        "the Authoring-YAML Diff view is the default (visible)",
    );
    assert!(
        eval_bool(
            &tab,
            "Array.from(document.querySelectorAll('.yaml-diff-view .diff-added'))\
             .some(function(e){return e.textContent.indexOf('cutoff_days: 30') !== -1;})"
        ),
        "the new override value shows as an added line in the drawer Diff view",
    );
    assert!(
        eval_bool(
            &tab,
            "Array.from(document.querySelectorAll('.yaml-diff-view .diff-removed'))\
             .some(function(e){return e.textContent.indexOf('cutoff_days: 7') !== -1;})"
        ),
        "the old override value shows as a removed line in the drawer Diff view",
    );

    // (2) NO spurious cell diff on the expect side — `expect` is unchanged,
    // so its grid renders Current-only (no cell-diff grid or toggle). The
    // expect panel renders on test selection (no tab switch needed).
    assert!(
        eval_bool(&tab, "document.querySelector('.expected-table') !== null"),
        "the expect fixture's Current grid renders",
    );
    assert!(
        !eval_bool(&tab, "document.querySelector('.cell-diff-toggle') !== null"),
        "an override-only edit must NOT produce a cell-diff Current↔Diff toggle",
    );
    assert!(
        !eval_bool(&tab, "document.querySelector('.cell-diff-table') !== null"),
        "an override-only edit must NOT render a cell-diff grid",
    );

    // (3) Same on the given side — the All-inputs view builds the given grid
    // lazily; switch to it and confirm it renders Current-only (no cell diff).
    let _ = eval(
        &tab,
        "document.querySelector('.panel-toggle [data-mode=\"inputs\"]').click()",
    );
    assert!(
        eval_bool(&tab, "document.querySelector('.given-table') !== null"),
        "the given fixture's Current grid renders in the All-inputs view",
    );
    assert!(
        !eval_bool(&tab, "document.querySelector('.cell-diff-table') !== null"),
        "the given side carries no cell-diff grid for an override-only edit",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn render_yaml_diff_js_emits_classes_sigils_and_codepoint_emphasis() {
    // Exercises the `__cuteRenderYamlDiff` JS seam directly (parallels
    // `__cuteHighlightYaml`). Any rendered report carries the function; the
    // emphasis offsets are codepoint indices, so a multibyte line must slice
    // on `Array.from`, not UTF-16 — `café x` → emphasis (5,6) marks `x`.
    let url = render_pr_diff_to_file(
        "headless_yaml_diff_js.html",
        vec![model_node("model.shop.dim_a")],
        vec![("unit_test.shop.dim_a.t", unit_test("t", "dim_a"))],
        &["model.shop.dim_a"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    let html = eval_string(
        &tab,
        "window.__cuteRenderYamlDiff({lines:[\
         {kind:'context',text:'  ctx',emphasis:null},\
         {kind:'removed',text:'café x',emphasis:[5,6]},\
         {kind:'added',text:'café y',emphasis:[5,6]}\
         ]})",
    );
    // Line classes + sigils.
    assert!(
        html.contains("diff-context")
            && html.contains("diff-removed")
            && html.contains("diff-added"),
        "all three line kinds map to their classes: {html}",
    );
    assert!(
        html.contains("diff-sigil\">+"),
        "the added line carries a '+' sigil: {html}",
    );
    // Codepoint-correct emphasis: the multibyte prefix `café ` (5 codepoints)
    // is preserved and only `x` / `y` is wrapped in <strong>.
    assert!(
        html.contains("café <strong>x</strong>"),
        "removed emphasis wraps exactly the changed codepoint after the café prefix: {html}",
    );
    assert!(
        html.contains("café <strong>y</strong>"),
        "added emphasis wraps exactly the changed codepoint: {html}",
    );

    let _ = tab.close(true);
}

/// Read a numeric expression from the page as i64 (DOM element counts).
fn eval_i64(tab: &Tab, expr: &str) -> i64 {
    let v = eval(tab, expr);
    v.as_i64().unwrap_or_else(|| {
        panic!("JS evaluation of `{expr}` returned non-i64 {v:?} (cute-dbt#109)")
    })
}

/// A JS array-literal of `n` context DiffLine objects (text `c0..c{n-1}`).
fn ctx_lines_js(n: usize) -> String {
    (0..n)
        .map(|i| format!("{{kind:'context',text:'c{i}',emphasis:null}}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn block_diff_folds_long_context_runs_and_reveals_on_activate() {
    // cute-dbt#132 — hunk contraction. Drives the `__cuteRenderBlockDiff` JS
    // seam with synthetic diffs (no manifest content needed) to verify:
    //  (1) a long middle context run folds: a `.diff-fold` control with the
    //      correct hidden count + `.diff-folded[hidden]` lines;
    //  (2) a SHORT block (change + 2 context) renders NO fold (small YAML
    //      test blocks must never fold);
    //  (3) activating the control (click AND keyboard) reveals the folded
    //      lines (they lose `hidden`), and reveal is PARENT-SCOPED: a second
    //      block with the same local `fold-0` id stays hidden.
    let url = render_pr_diff_to_file(
        "headless_fold_js.html",
        vec![model_node("model.shop.dim_a")],
        vec![("unit_test.shop.dim_a.t", unit_test("t", "dim_a"))],
        &["model.shop.dim_a"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // (1) LONG middle run: 1 removed + 1 added, then 10 context, then a change.
    // head=3, tail=3, hidden=4.
    let long_diff = format!(
        "{{lines:[{{kind:'removed',text:'a',emphasis:null}},\
         {{kind:'added',text:'b',emphasis:null}},{ctx},\
         {{kind:'removed',text:'c',emphasis:null}},\
         {{kind:'added',text:'d',emphasis:null}}]}}",
        ctx = ctx_lines_js(10),
    );
    let folded_html = eval_string(
        &tab,
        &format!("window.__cuteRenderBlockDiff({long_diff}, window.__cuteTokenizeSql)"),
    );
    assert!(
        folded_html.contains("Show 4 unchanged lines"),
        "the long run folds with the correct hidden count: {folded_html}",
    );
    assert!(
        folded_html.contains("diff-fold") && folded_html.contains("data-fold=\"0\""),
        "a fold control with a local id is emitted: {folded_html}",
    );
    assert!(
        folded_html.contains("diff-folded fold-0")
            && folded_html.contains("diff-folded fold-0\" hidden>"),
        "the folded middle lines carry `diff-folded fold-0` and the hidden attribute: {folded_html}",
    );

    // (2) SHORT block: a change + 2 context → NO fold.
    let short_html = eval_string(
        &tab,
        "window.__cuteRenderBlockDiff({lines:[\
         {kind:'removed',text:'x',emphasis:null},\
         {kind:'added',text:'y',emphasis:null},\
         {kind:'context',text:'c1',emphasis:null},\
         {kind:'context',text:'c2',emphasis:null}\
         ]}, window.__cuteTokenizeSql)",
    );
    assert!(
        !short_html.contains("diff-fold"),
        "a short block (change + 2 context) does NOT fold: {short_html}",
    );

    // (3) Reveal on activate, parent-scoped. Inject the SAME folded HTML into
    // TWO separate <code> blocks (each carries its own local fold-0) so the
    // parent-scoping invariant is exercised. The delegated document handler
    // (bound once by bindGlobalHandlers) must fire on the injected nodes.
    let _ = eval(
        &tab,
        &format!(
            "(function(){{\
               var h = window.__cuteRenderBlockDiff({long_diff}, window.__cuteTokenizeSql);\
               var a = document.createElement('code'); a.id='fold-block-a'; a.innerHTML = h;\
               var b = document.createElement('code'); b.id='fold-block-b'; b.innerHTML = h;\
               var s = document.createElement('code'); s.id='fold-block-s'; s.innerHTML = h;\
               document.body.appendChild(a); document.body.appendChild(b); document.body.appendChild(s);\
             }})()"
        ),
    );
    // Both blocks start with 4 hidden folded lines each.
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#fold-block-a .diff-folded[hidden]').length"
        ),
        4,
        "block A starts with 4 hidden folded lines",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#fold-block-b .diff-folded[hidden]').length"
        ),
        4,
        "block B starts with 4 hidden folded lines",
    );

    // CLICK the control in block A: A reveals (0 hidden); the control STAYS
    // visible and relabels to a "Hide N" collapse affordance (#136
    // bidirectional). B is untouched (still 4 hidden) — toggle is parent-scoped.
    let _ = eval(
        &tab,
        "document.querySelector('#fold-block-a .diff-fold').click()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#fold-block-a .diff-folded[hidden]').length"
        ),
        0,
        "clicking block A's control reveals its folded lines",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('#fold-block-a .diff-fold').hidden"
        ),
        "the activated control STAYS visible (bidirectional toggle, #136)",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#fold-block-a .diff-fold').getAttribute('aria-expanded')"
        ),
        "true",
        "the expanded control reports aria-expanded=true",
    );
    assert!(
        eval_string(
            &tab,
            "document.querySelector('#fold-block-a .diff-fold-label').textContent"
        )
        .contains("Hide 4 unchanged lines"),
        "the expanded control relabels to 'Hide N unchanged lines'",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#fold-block-b .diff-folded[hidden]').length"
        ),
        4,
        "block B stays folded — toggle is PARENT-SCOPED despite the shared fold-0 id",
    );

    // CLICK A's control AGAIN: it re-collapses (4 hidden) and relabels to Show —
    // the round-trip the old one-way reveal could not do (#136).
    let _ = eval(
        &tab,
        "document.querySelector('#fold-block-a .diff-fold').click()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#fold-block-a .diff-folded[hidden]').length"
        ),
        4,
        "clicking A's control again re-collapses its folded lines",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#fold-block-a .diff-fold').getAttribute('aria-expanded')"
        ),
        "false",
        "the re-collapsed control reports aria-expanded=false",
    );
    assert!(
        eval_string(
            &tab,
            "document.querySelector('#fold-block-a .diff-fold-label').textContent"
        )
        .contains("Show 4 unchanged lines"),
        "the re-collapsed control relabels back to 'Show N unchanged lines'",
    );

    // KEYBOARD activate block B's control (Enter): B reveals too.
    let _ = eval(
        &tab,
        "(function(){\
           var c = document.querySelector('#fold-block-b .diff-fold');\
           c.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));\
         })()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#fold-block-b .diff-folded[hidden]').length"
        ),
        0,
        "pressing Enter on block B's control reveals its folded lines (keyboard activation)",
    );

    // KEYBOARD activate block S's control with SPACE: S reveals too (pins the
    // spec's literal Enter/Space activation, distinct from the Enter path).
    let _ = eval(
        &tab,
        "(function(){\
           var c = document.querySelector('#fold-block-s .diff-fold');\
           c.dispatchEvent(new KeyboardEvent('keydown',{key:' ',bubbles:true}));\
         })()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#fold-block-s .diff-folded[hidden]').length"
        ),
        0,
        "pressing Space on block S's control reveals its folded lines (Space activation)",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#111: inline model SQL diff (Raw↔Diff toggle) ----------

/// PR-diff render that injects a model-SQL-diff map so the Model SQL
/// section's Raw↔Diff toggle renders. `sql_diffs` is `(model_id,
/// BlockDiff)`. Every model carries `raw_code` so the section is shown.
fn render_pr_diff_with_sql_diffs(
    filename: &str,
    nodes: Vec<Node>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    sql_diffs: Vec<(&str, BlockDiff)>,
) -> String {
    let all_ids: Vec<String> = tests.iter().map(|(id, _)| (*id).to_owned()).collect();
    let m = manifest(nodes, tests);
    let in_scope: InScopeSet = all_ids.into_iter().collect();
    let models: ModelInScopeSet = model_ids.iter().map(|id| NodeId::new(*id)).collect();
    let sql_diff_map: HashMap<String, BlockDiff> = sql_diffs
        .into_iter()
        .map(|(id, d)| (id.to_owned(), d))
        .collect();
    let out = tmp(filename);
    let _ = std::fs::remove_file(&out);
    render_report(
        &out,
        &m,
        &in_scope,
        &models,
        &InScopeSet::new(),
        &HashMap::new(),
        &HashMap::new(),
        &sql_diff_map,
        &HashMap::new(),
        "",
        ScopeSource::PrDiff,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let p = out.to_str().expect("report path is valid UTF-8");
    format!("file://{p}")
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn model_sql_section_defaults_to_diff_and_toggles_to_raw() {
    // dim_a's .sql changed (from t → from u); the run loop attaches an
    // inline SQL diff. The Model SQL section defaults to the Diff view and
    // flips to Raw on the toggle.
    let diff = BlockDiff {
        lines: vec![
            dl(DiffLineKind::Context, "select id", None),
            dl(DiffLineKind::Removed, "from t", Some((5, 6))),
            dl(DiffLineKind::Added, "from u", Some((5, 6))),
        ],
    };
    let url = render_pr_diff_with_sql_diffs(
        "headless_sql_diff.html",
        vec![model_node_with_raw("model.shop.dim_a", "select id\nfrom u")],
        vec![("unit_test.shop.dim_a.t", unit_test("t", "dim_a"))],
        &["model.shop.dim_a"],
        vec![("model.shop.dim_a", diff)],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // The Model SQL section is shown with the Raw↔Diff toggle and the Diff
    // view default; the summary hint reads "diff".
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-sql').style.display !== 'none'"
        ),
        "the Model SQL section is shown for a model with raw_code",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql-summary-hint').textContent.trim()"
        ),
        "diff",
        "the summary hint names the diff view",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-sql .model-sql-toggle') !== null"
        ),
        "the Raw↔Diff toggle is present in the Model SQL section",
    );
    // cute-dbt#178 — the code-card header names the model file and hosts
    // the toggle. This node carries NO original_file_path, so this is the
    // cute-dbt#179 FALLBACK arm: the synthesized `<name>.sql` (the DAG
    // terminal label); the full-path arm is asserted by
    // `model_sql_header_shows_the_full_model_path`.
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql .code-header .code-filename').textContent.trim()"
        ),
        "dim_a.sql",
        "absent original_file_path ⇒ the header falls back to the model's file name",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('.model-sql .sql-diff-view').hidden"
        ),
        "the Diff view is the default (visible)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-sql .sql-raw-view').hidden"
        ),
        "the Raw view starts hidden",
    );
    // The diff renders the change pair with intra-line emphasis.
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-sql .sql-diff-view .diff-removed strong') !== null"
        ),
        "the removed SQL line carries intra-line emphasis <strong>",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-sql .sql-diff-view .diff-added') !== null"
        ),
        "the diff renders the added SQL line",
    );

    // Clicking "Raw" flips to the plain SQL view.
    let _ = eval(
        &tab,
        "document.querySelector('.model-sql .yaml-view-btn[data-view=\"raw\"]').click()",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-sql .sql-diff-view').hidden"
        ),
        "the Diff view hides after switching to Raw",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('.model-sql .sql-raw-view').hidden"
        ),
        "the Raw view shows after the toggle",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn model_sql_section_shows_plain_sql_when_no_diff() {
    // No sql_diff for the model (in scope via a changed test, or baseline):
    // the Model SQL section shows the plain raw SQL with no toggle.
    let url = render_pr_diff_with_sql_diffs(
        "headless_sql_no_diff.html",
        vec![model_node_with_raw("model.shop.dim_b", "select id\nfrom t")],
        vec![("unit_test.shop.dim_b.t", unit_test("t", "dim_b"))],
        &["model.shop.dim_b"],
        vec![], // no SQL diff
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-sql').style.display !== 'none'"
        ),
        "the Model SQL section is shown",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-sql .model-sql-toggle') === null"
        ),
        "no Raw↔Diff toggle when the model has no SQL diff",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql-summary-hint').textContent.trim()"
        ),
        "raw, with Jinja",
        "the summary hint reads the plain-raw label",
    );
    // The raw SQL is highlighted in the (only) <pre>.
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-sql .model-sql-code') !== null"
        ),
        "the plain Model SQL code block renders",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn model_sql_header_shows_the_full_model_path() {
    // cute-dbt#179 — founder call: when the manifest carries the model's
    // original_file_path, the Model-SQL code-card header names the FULL
    // project-relative path (`models/…/x.sql`), not the bare `<name>.sql`
    // synthesis (that stays the fallback — see
    // `model_sql_section_defaults_to_diff_and_toggles_to_raw`).
    let url = render_pr_diff_with_sql_diffs(
        "headless_sql_full_path.html",
        vec![model_node_with_raw_and_path(
            "model.shop.dim_c",
            "select id\nfrom t",
            "models/marts/core/dim_c.sql",
        )],
        vec![("unit_test.shop.dim_c.t", unit_test("t", "dim_c"))],
        &["model.shop.dim_c"],
        vec![], // no SQL diff — the plain-raw card still carries the header
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql .code-header .code-filename').textContent.trim()"
        ),
        "models/marts/core/dim_c.sql",
        "the Model-SQL code-card header shows the model's full project-relative path",
    );
    // The title attribute mirrors the path so a CSS-truncated header is
    // still fully readable on hover.
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql .code-header .code-filename').getAttribute('title')"
        ),
        "models/marts/core/dim_c.sql",
        "the header's title attribute carries the full path (truncation affordance)",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#121: each diff drawer renders exactly ONE <pre> ---------

/// PR-diff render that injects BOTH a model-SQL diff AND a unit-test
/// authoring-YAML diff into the same report — so the Model SQL drawer's
/// Diff/File toggle AND the Authoring YAML drawer's Diff/File toggle both
/// render against a single auto-selected (changed) test.
///
/// `sql_diffs` is `(model_id, BlockDiff)` keyed by the model's FULL node
/// id; `authoring` is `(test_id, raw)`; `yaml_diffs` is `(test_id,
/// BlockDiff)`. Every model carries `raw_code` so the Model SQL section
/// shows. The changed test is foregrounded + auto-selected by the PR-diff
/// JS, so its YAML drawer is built on load.
#[allow(clippy::too_many_arguments)]
fn render_pr_diff_with_both_diffs(
    filename: &str,
    nodes: Vec<Node>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    changed_ids: &[&str],
    authoring: Vec<(&str, &str)>,
    yaml_diffs: Vec<(&str, BlockDiff)>,
    sql_diffs: Vec<(&str, BlockDiff)>,
) -> String {
    let in_scope: InScopeSet = tests.iter().map(|(id, _)| (*id).to_owned()).collect();
    let m = manifest(nodes, tests);
    let models: ModelInScopeSet = model_ids.iter().map(|id| NodeId::new(*id)).collect();
    let changed: InScopeSet = changed_ids.iter().map(|s| (*s).to_owned()).collect();
    let authoring_yaml: HashMap<String, UnitTestYamlBlock> = authoring
        .into_iter()
        .map(|(id, raw)| {
            let n = raw.lines().count();
            (
                id.to_owned(),
                UnitTestYamlBlock::new(raw.to_owned(), 1, 1, n),
            )
        })
        .collect();
    let yaml_diff_map: HashMap<String, BlockDiff> = yaml_diffs
        .into_iter()
        .map(|(id, d)| (id.to_owned(), d))
        .collect();
    let sql_diff_map: HashMap<String, BlockDiff> = sql_diffs
        .into_iter()
        .map(|(id, d)| (id.to_owned(), d))
        .collect();
    let out = tmp(filename);
    let _ = std::fs::remove_file(&out);
    render_report(
        &out,
        &m,
        &in_scope,
        &models,
        &changed,
        &authoring_yaml,
        &yaml_diff_map,
        &sql_diff_map,
        &HashMap::new(),
        "",
        ScopeSource::PrDiff,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let p = out.to_str().expect("report path is valid UTF-8");
    format!("file://{p}")
}

/// `true` iff the element matched by `selector` is visually rendered
/// (`offsetHeight > 0`). This is the load-bearing assertion for
/// cute-dbt#121: the inactive `<pre>` carries the `hidden` attribute
/// (so `.hidden === true`) yet still renders, because Sakura's
/// `pre { display: block }` (author origin) beats the UA
/// `[hidden] { display: none }`. `.hidden` cannot catch the bug;
/// `offsetHeight` can.
fn visible(tab: &Tab, selector: &str) -> bool {
    eval_bool(
        tab,
        &format!("(document.querySelector('{selector}') || {{}}).offsetHeight > 0"),
    )
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn diff_drawers_render_exactly_one_view_each() {
    // cute-dbt#121 — a PR-diff report carrying BOTH a model `sql_diff`
    // (Model SQL Diff/File toggle) AND a unit-test `yaml_diff` (Authoring
    // YAML Diff/File toggle). The toggle JS sets `hidden` on the inactive
    // <pre>, but Sakura's bare `pre { display: block }` (author origin)
    // overrides the UA `[hidden] { display: none }`, so before the fix
    // BOTH <pre> blocks render at once. Asserted via `offsetHeight > 0`
    // (the `hidden` property is `true` either way — useless here).
    let sql_diff = BlockDiff {
        lines: vec![
            dl(DiffLineKind::Context, "select id", None),
            dl(DiffLineKind::Removed, "from t", Some((5, 6))),
            dl(DiffLineKind::Added, "from u", Some((5, 6))),
        ],
    };
    let yaml_diff = BlockDiff {
        lines: vec![
            dl(DiffLineKind::Context, "  - name: upd", None),
            dl(DiffLineKind::Removed, "    model: payments", Some((11, 18))),
            dl(DiffLineKind::Added, "    model: orders", Some((11, 16))),
            dl(DiffLineKind::Context, "    given: []", None),
        ],
    };
    let url = render_pr_diff_with_both_diffs(
        "headless_both_diffs.html",
        vec![model_node_with_raw("model.shop.dim_a", "select id\nfrom u")],
        vec![("unit_test.shop.dim_a.upd", unit_test("upd", "dim_a"))],
        &["model.shop.dim_a"],
        &["unit_test.shop.dim_a.upd"],
        vec![(
            "unit_test.shop.dim_a.upd",
            "  - name: upd\n    model: orders\n    given: []",
        )],
        vec![("unit_test.shop.dim_a.upd", yaml_diff)],
        vec![("model.shop.dim_a", sql_diff)],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // The Model SQL section's <details> is collapsed by default (cute-dbt#47);
    // force it open so the active Diff <pre> can report a real offsetHeight.
    // The Authoring YAML <details> is created open by the JS, so it needs no
    // nudge. This isolates the test to display:none vs display:block — not
    // the collapsed-details ancestor.
    let _ = eval(
        &tab,
        "var d = document.querySelector('.model-sql details'); if (d) d.open = true;",
    );

    // The non-diff button reads exactly "File" in BOTH drawers (was "Raw"
    // for SQL, "Authored" for YAML).
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql-toggle .yaml-view-btn[data-view=\"raw\"]')\
             .textContent.trim()"
        ),
        "File",
        "the Model SQL non-diff button reads 'File'",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.authoring-yaml .yaml-diff-toggle .yaml-view-btn[data-view=\"authored\"]')\
             .textContent.trim()"
        ),
        "File",
        "the Authoring YAML non-diff button reads 'File'",
    );

    // ===== Default (Diff view): exactly one <pre> visible per drawer =====
    assert!(
        visible(&tab, ".model-sql .sql-diff-view"),
        "Model SQL: the Diff view is visible by default",
    );
    assert!(
        !visible(&tab, ".model-sql .sql-raw-view"),
        "Model SQL: the File (raw) view is NOT visible by default \
         (cute-dbt#121: Sakura pre{{display:block}} must not defeat [hidden])",
    );
    assert!(
        visible(&tab, ".authoring-yaml .yaml-diff-view"),
        "Authoring YAML: the Diff view is visible by default",
    );
    assert!(
        !visible(&tab, ".authoring-yaml .yaml-authored-view"),
        "Authoring YAML: the File (authored) view is NOT visible by default \
         (cute-dbt#121)",
    );

    // ===== After clicking the non-diff (File) toggle in each drawer =====
    let _ = eval(
        &tab,
        "document.querySelector('.model-sql-toggle .yaml-view-btn[data-view=\"raw\"]').click()",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.authoring-yaml .yaml-diff-toggle .yaml-view-btn[data-view=\"authored\"]').click()",
    );

    assert!(
        visible(&tab, ".model-sql .sql-raw-view"),
        "Model SQL: the File view is visible after clicking File",
    );
    assert!(
        !visible(&tab, ".model-sql .sql-diff-view"),
        "Model SQL: the Diff view is hidden after clicking File (cute-dbt#121)",
    );
    assert!(
        visible(&tab, ".authoring-yaml .yaml-authored-view"),
        "Authoring YAML: the File view is visible after clicking File",
    );
    assert!(
        !visible(&tab, ".authoring-yaml .yaml-diff-view"),
        "Authoring YAML: the Diff view is hidden after clicking File (cute-dbt#121)",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#98: cell-level data-table diff (Current↔Diff toggle) ----
//
// The structured sibling of the cute-dbt#96 Authoring-YAML drawer toggle.
// Each given/expect grid offers a per-table Current↔Diff toggle when the
// test carries a `data_diff` for that table; the Diff view tints changed
// cells inline (old → new) and badges added/removed rows + columns. The
// toggle MUST show exactly one of the two views at a time — the same
// `[hidden]{display:none!important}` (cute-dbt#121) mechanism the YAML/SQL
// drawers use. `offsetHeight` is the load-bearing oracle (the `hidden`
// property is `true` on the inactive view either way; `<table>` has no
// competing author `display`, but the rule is the same).

/// A `model` node carrying the import CTE `src` so a `ref('src')` given
/// binds to a node (the cell grid renders inside the Node-detail panel).
fn model_node_with_src(full_id: &str) -> Node {
    Node::new(
        NodeId::new(full_id),
        "model",
        Checksum::new("sha256", "ck"),
        Some("with src as (select 1 as id) select id from src".to_owned()),
        None,
        DependsOn::default(),
        None,
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
}

/// A unit test with one `given` (`ref('src')`) carrying inline dict rows and
/// an empty `expect` — the Current grid the cell-diff toggle decorates.
fn unit_test_with_given(name: &str, model_bare: &str, rows: serde_json::Value) -> UnitTest {
    UnitTest::new(
        name.to_owned(),
        NodeId::new(model_bare),
        vec![UnitTestGiven::new(
            "ref('src')".to_owned(),
            rows,
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(serde_json::Value::Null, None, None),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
}

/// PR-diff render that injects a `data_diffs` map so a test's given grid
/// renders the cell-diff Current↔Diff toggle. `data_diffs` is `(test_id,
/// UnitTestDataDiff)`. The test is built (by the caller) with given rows so
/// the Current grid has data, and is the sole in-scope/changed test so the
/// PR-diff JS auto-selects it and builds its Node-detail panel on load.
fn render_pr_diff_with_data_diffs(
    filename: &str,
    nodes: Vec<Node>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    changed_ids: &[&str],
    data_diffs: Vec<(&str, UnitTestDataDiff)>,
) -> String {
    let in_scope: InScopeSet = tests.iter().map(|(id, _)| (*id).to_owned()).collect();
    let m = manifest(nodes, tests);
    let models: ModelInScopeSet = model_ids.iter().map(|id| NodeId::new(*id)).collect();
    let changed: InScopeSet = changed_ids.iter().map(|s| (*s).to_owned()).collect();
    let data_diff_map: HashMap<String, UnitTestDataDiff> = data_diffs
        .into_iter()
        .map(|(id, d)| (id.to_owned(), d))
        .collect();
    let out = tmp(filename);
    let _ = std::fs::remove_file(&out);
    render_report(
        &out,
        &m,
        &in_scope,
        &models,
        &changed,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &data_diff_map,
        "",
        ScopeSource::PrDiff,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let p = out.to_str().expect("report path is valid UTF-8");
    format!("file://{p}")
}

/// Switch the left panel to "All inputs" mode so the given grid (and its
/// cell-diff toggle) is on-screen regardless of DAG-node selection — the
/// Node-detail panel only shows a given once its import-CTE node is clicked,
/// but the All-inputs panel renders every given unconditionally.
fn show_all_inputs(tab: &Tab) {
    let _ = eval(
        tab,
        "document.querySelector('.panel-toggle [data-mode=\"inputs\"]').click()",
    );
}

/// `data_diff` for a single given input: one row, one Modified cell whose
/// `id` value goes 1 → 2 (the synthetic toggle fixture).
fn one_cell_modified_data_diff(input: &str) -> UnitTestDataDiff {
    use cute_dbt::domain::{
        Cell, CellChange, CellValue, ColumnStatus, DiffColumn, FixtureTableDiff, NamedTableDiff,
        RowChange, RowChangeKind,
    };
    UnitTestDataDiff {
        given: vec![NamedTableDiff {
            ordinal: 0,
            input: input.to_owned(),
            diff: FixtureTableDiff {
                columns: vec![DiffColumn {
                    name: "id".into(),
                    status: ColumnStatus::Present,
                }],
                rows: vec![RowChange {
                    kind: RowChangeKind::Modified,
                    cells: vec![CellChange {
                        old: Cell::new(CellValue::Number("1".into())),
                        new: Cell::new(CellValue::Number("2".into())),
                        changed: true,
                    }],
                }],
            },
        }],
        expect: None,
    }
}

/// `data_diff` for one given input with a single Modified row carrying TWO
/// changed cells: one whose NEW value is a real null, one whose NEW value is
/// the string literal `"null"` — the cute-dbt#132 null-vs-string distinction.
fn null_vs_string_null_data_diff(input: &str) -> UnitTestDataDiff {
    use cute_dbt::domain::{
        Cell, CellChange, CellValue, ColumnStatus, DiffColumn, FixtureTableDiff, NamedTableDiff,
        RowChange, RowChangeKind,
    };
    UnitTestDataDiff {
        given: vec![NamedTableDiff {
            ordinal: 0,
            input: input.to_owned(),
            diff: FixtureTableDiff {
                columns: vec![
                    DiffColumn {
                        name: "cnt".into(),
                        status: ColumnStatus::Present,
                    },
                    DiffColumn {
                        name: "status".into(),
                        status: ColumnStatus::Present,
                    },
                ],
                rows: vec![RowChange {
                    kind: RowChangeKind::Modified,
                    cells: vec![
                        // new side is a REAL null -> italic muted-gray .cell-null.
                        CellChange {
                            old: Cell::new(CellValue::Number("1".into())),
                            new: Cell::new(CellValue::Null),
                            changed: true,
                        },
                        // new side is the STRING "null" -> a normal value
                        // (.cell-new), NOT .cell-null.
                        CellChange {
                            old: Cell::new(CellValue::Str("completed".into())),
                            new: Cell::new(CellValue::Str("null".into())),
                            changed: true,
                        },
                    ],
                }],
            },
        }],
        expect: None,
    }
}

/// cute-dbt#131 — two `given:` blocks against the SAME `ref('a')`, each with a
/// DISTINCT one-cell change, tagged by SOURCE ORDINAL (0 and 1). The renderer
/// must bind each given-section to its OWN diff by ordinal; keying by `input`
/// text (the pre-#131 behavior) would collapse both onto the first match.
fn two_same_ref_givens_distinct_diffs() -> UnitTestDataDiff {
    use cute_dbt::domain::{
        Cell, CellChange, CellValue, ColumnStatus, DiffColumn, FixtureTableDiff, NamedTableDiff,
        RowChange, RowChangeKind,
    };
    fn one(ordinal: usize, old: &str, new: &str) -> NamedTableDiff {
        NamedTableDiff {
            ordinal,
            // BOTH givens share this `input` on purpose — the ordinal, not the
            // text, is the identity.
            input: "ref('a')".to_owned(),
            diff: FixtureTableDiff {
                columns: vec![DiffColumn {
                    name: "amt".into(),
                    status: ColumnStatus::Present,
                }],
                rows: vec![RowChange {
                    kind: RowChangeKind::Modified,
                    cells: vec![CellChange {
                        old: Cell::new(CellValue::Number(old.into())),
                        new: Cell::new(CellValue::Number(new.into())),
                        changed: true,
                    }],
                }],
            },
        }
    }
    UnitTestDataDiff {
        given: vec![one(0, "100", "111"), one(1, "200", "222")],
        expect: None,
    }
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn same_ref_givens_each_render_their_own_cell_diff() {
    // cute-dbt#131 — the end-to-end (template-binding) guard for per-given
    // identity. The domain test `data_diff_ordinal_is_source_position_not_push_index`
    // pins the ordinal *computation*; THIS test pins the JS binding that the bug
    // actually lived in. A test with two `given:` blocks against the SAME
    // `ref('a')`, both carrying a real (distinct) cell change, renders one
    // given-section per given IN SOURCE ORDER. Each section must show ITS OWN
    // cell diff: section 0 -> 111, section 1 -> 222. Pre-#131 `givenDataDiff`
    // matched by `input` text + first hit, so BOTH sections rendered ordinal 0's
    // diff ("111"); binding by source ordinal fixes section 1 to show "222".
    let id = "unit_test.shop.dim_a.dup";
    let ut = UnitTest::new(
        "dup",
        NodeId::new("dim_a"),
        vec![
            UnitTestGiven::new(
                "ref('a')".to_owned(),
                serde_json::json!([{"amt": 111}]),
                Some("dict".to_owned()),
                None,
            ),
            UnitTestGiven::new(
                "ref('a')".to_owned(),
                serde_json::json!([{"amt": 222}]),
                Some("dict".to_owned()),
                None,
            ),
        ],
        UnitTestExpect::new(serde_json::Value::Null, None, None),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    );
    let url = render_pr_diff_with_data_diffs(
        "headless_same_ref_givens.html",
        vec![model_node_with_src("model.shop.dim_a")],
        vec![(id, ut)],
        &["model.shop.dim_a"],
        &[id],
        vec![(id, two_same_ref_givens_distinct_diffs())],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    show_all_inputs(&tab);

    // Two given-sections (both for `ref('a')`), in source order.
    assert_eq!(
        eval_i64(&tab, "document.querySelectorAll('.given-section').length"),
        2,
        "two same-ref givens render two given-sections",
    );

    // Each section's cell diff shows ITS OWN new value — the per-section new
    // value is the `.cell-new` inside the changed cell.
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelectorAll('.given-section')[0]\
             .querySelector('.cell-diff-table .cell-changed .cell-new').textContent.trim()"
        ),
        "111",
        "the first given-section (ordinal 0) shows its own cell diff",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelectorAll('.given-section')[1]\
             .querySelector('.cell-diff-table .cell-changed .cell-new').textContent.trim()"
        ),
        "222",
        "the second given-section (ordinal 1) shows ITS OWN cell diff, not ordinal 0's",
    );
    // Regression guard: section 1 must NOT carry ordinal 0's value (the pre-#131
    // mis-bind, where both sections resolved to the first `ref('a')` diff).
    assert!(
        !eval_bool(
            &tab,
            "document.querySelectorAll('.given-section')[1]\
             .querySelector('.cell-diff-table').textContent.indexOf('111') !== -1"
        ),
        "section 1's diff must not leak ordinal 0's value",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn changed_cell_distinguishes_a_real_null_from_a_string_null() {
    // cute-dbt#132 — inside an inline old -> new changed cell, a REAL null reads
    // as italic muted-gray `.cell-null` ("NULL") while a string literal 'null'
    // reads as a normal value (`.cell-new`, "null") — visually distinct even
    // mid-diff. Also pins the no-fill emphasis: the changed value is bold
    // colored TEXT with a transparent background, not a github-style fill.
    let id = "unit_test.shop.dim_a.upd";
    let url = render_pr_diff_with_data_diffs(
        "headless_cell_null_distinction.html",
        vec![model_node_with_src("model.shop.dim_a")],
        vec![(
            id,
            unit_test_with_given(
                "upd",
                "dim_a",
                serde_json::json!([{"cnt": null, "status": "null"}]),
            ),
        )],
        &["model.shop.dim_a"],
        &[id],
        vec![(id, null_vs_string_null_data_diff("ref('src')"))],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    show_all_inputs(&tab);

    // A real null on the new side -> `.cell-null` "NULL", italic.
    assert!(
        visible(&tab, ".cell-diff-table .cell-changed .cell-null"),
        "a real null on the changed cell's new side renders as .cell-null",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.cell-diff-table .cell-changed .cell-null').textContent.trim()"
        ),
        "NULL",
        "the real null reads as NULL",
    );
    assert_eq!(
        eval_string(
            &tab,
            "getComputedStyle(document.querySelector('.cell-diff-table .cell-changed .cell-null')).fontStyle"
        ),
        "italic",
        "a real null is italic (muted-gray), distinct from a normal value",
    );

    // The string literal 'null' -> a normal `.cell-new` value (NOT .cell-null).
    assert!(
        eval_bool(
            &tab,
            "Array.from(document.querySelectorAll('.cell-diff-table .cell-changed .cell-new'))\
             .some(function(e){return e.textContent.trim()==='null';})"
        ),
        "a string literal 'null' renders as a normal .cell-new value, not .cell-null",
    );

    // No-fill emphasis: the changed value is bold colored TEXT, transparent bg.
    assert_eq!(
        eval_string(
            &tab,
            "getComputedStyle(document.querySelector('.cell-diff-table .cell-changed .cell-new')).backgroundColor"
        ),
        "rgba(0, 0, 0, 0)",
        "the changed value has no background fill (bold colored text, not a github-style highlight)",
    );
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn cell_diff_toggle_defaults_to_diff_and_shows_exactly_one_view() {
    // cute-dbt#98 — a changed test carrying a `data_diff` for its given input
    // renders a per-table Current↔Diff toggle. The Diff view is the default;
    // EXACTLY one of {Diff, Current} is visible at a time (offsetHeight, the
    // cute-dbt#121 oracle). Clicking "Current" flips the body: the inline
    // old → new cell disappears and the plain Current grid shows.
    let id = "unit_test.shop.dim_a.upd";
    let url = render_pr_diff_with_data_diffs(
        "headless_cell_diff_toggle.html",
        vec![model_node_with_src("model.shop.dim_a")],
        vec![(
            id,
            unit_test_with_given("upd", "dim_a", serde_json::json!([{"id": 2}])),
        )],
        &["model.shop.dim_a"],
        &[id],
        vec![(id, one_cell_modified_data_diff("ref('src')"))],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // The All-inputs panel renders the given grid unconditionally (no DAG
    // node click needed). The cell-diff toggle is then in the DOM.
    show_all_inputs(&tab);

    assert!(
        eval_bool(&tab, "document.querySelector('.cell-diff-toggle') !== null"),
        "a test with a data_diff renders the Current↔Diff cell-diff toggle",
    );
    // The toggle's active (default) button reads "Diff".
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.cell-diff-toggle .yaml-view-btn.active').textContent.trim()"
        ),
        "Diff",
        "the cell-diff toggle defaults to the Diff view",
    );

    // ===== Default (Diff view): exactly ONE grid visible =====
    assert!(
        visible(&tab, ".fixture-view .cell-diff-table"),
        "Diff view: the cell-diff grid is visible by default",
    );
    // The non-diff (Current) grid is a plain given-table that is NOT the
    // cell-diff-table; assert exactly one given-table is visible.
    assert_eq!(
        eval(
            &tab,
            "Array.from(document.querySelectorAll('.fixture-view table.given-table'))\
             .filter(function(t){return t.offsetHeight > 0;}).length"
        ),
        serde_json::json!(1),
        "exactly ONE given grid is visible in the Diff view (cute-dbt#121 oracle)",
    );
    // The default Diff view carries the inline old → new changed cell.
    assert!(
        visible(&tab, ".cell-diff-table .cell-changed .cell-old"),
        "the Diff view renders the inline old value of the changed cell",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.cell-diff-table .cell-changed .cell-old').textContent.trim()"
        ),
        "1",
        "the old value is 1",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.cell-diff-table .cell-changed .cell-new').textContent.trim()"
        ),
        "2",
        "the new value is 2 (old → new)",
    );

    // ===== Flip to Current: the body changes, exactly one grid visible =====
    let _ = eval(
        &tab,
        "document.querySelector('.cell-diff-toggle .yaml-view-btn[data-view=\"current\"]').click()",
    );
    assert!(
        !visible(&tab, ".fixture-view .cell-diff-table"),
        "Current view: the Diff grid is hidden after clicking Current (cute-dbt#121)",
    );
    assert_eq!(
        eval(
            &tab,
            "Array.from(document.querySelectorAll('.fixture-view table.given-table'))\
             .filter(function(t){return t.offsetHeight > 0;}).length"
        ),
        serde_json::json!(1),
        "exactly ONE given grid is visible in the Current view too",
    );
    // The plain Current grid carries NO inline old → new cell (the body
    // changed — a format-only style plain grid).
    assert!(
        !visible(&tab, ".cell-old"),
        "the Current view shows no inline old → new diff cell",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#98 CENTERPIECE: fusion csv format-only=no-diff ----------
//
// The must-prove behavior, end-to-end over the COMMITTED
// `tests/fixtures/fusion-csv-raw-string.json` (dbt-fusion 2.0-preview's
// csv-as-RAW-STRING encoding). The NEW given table is value-inferred from
// the fixture's raw-csv body; the OLD table is reconstructed from a
// synthesized `--unified=0` hunk. Two reconstructions, ONE manifest:
//
//   * a **format-only reformat** of the OLD data row (`1` → `1.00`, both
//     infer `Number(1)`) → `has_real_change() == false` → NO entry → the
//     given grid renders the plain Current grid (NO `.cell-diff-toggle`,
//     NO `.cell-old`). This is the cute-dbt#127 convergence, proven in the
//     real DOM.
//   * a **genuine value change** (`9` → `1`) → an entry → the Diff view
//     tints the changed cell inline `9 → 1` (`.cell-old` / `.cell-new`).
//
// Driving through `reconstruct_table_diffs` (not a hand-built POD) ties the
// DOM assertion to the real value-inference + diff pipeline.

/// The committed fusion-csv fixture's unit test id.
const FUSION_TEST_ID: &str = "unit_test.fusion_demo.test_dq_rollup_csv_raw_string";

/// Load the committed fusion-csv-raw-string manifest through the REAL adapter.
fn load_fusion_manifest() -> Manifest {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fusion-csv-raw-string.json");
    FileManifestSource
        .load(&path)
        .expect("fusion-csv-raw-string fixture loads as a v12 manifest")
}

/// Reconstruct the fusion test's `data_diff` from a working-tree YAML block
/// whose given `rows: |` csv body's first data row's `quarantined_count`
/// field is `new_count`, edited from `old_count` by a synthesized
/// `--unified=0` hunk. `old_count == "1.00"` is the format-only reformat
/// (converges); `old_count == "9"` is a genuine value change.
fn reconstruct_fusion_data_diffs(
    new_count: &str,
    old_count: &str,
) -> HashMap<String, UnitTestDataDiff> {
    let manifest = load_fusion_manifest();
    // Working-tree (NEW) block. It carries BOTH the given AND the expect
    // sub-blocks so each side reconstructs its OLD table from the SAME block:
    // the given `rows: |` csv mirrors the manifest's NEW given (first data row
    // carries `new_count`); the expect `rows: |` csv mirrors the manifest's
    // NEW expect verbatim. The hunk touches ONLY the given data row, so the
    // expect side reconstructs OLD == NEW (Context-only) → no expect change;
    // the given side is the sole variable. `- name:` is source line 1.
    let given_data_row = format!("          encounters,{new_count},false");
    let raw = format!(
        "  - name: test_dq_rollup_csv_raw_string\n    given:\n      - input: ref('src_checks')\n        format: csv\n        rows: |\n          entity_type,quarantined_count,is_dq_valid\n{given_data_row}\n          medications,2,true\n    expect:\n      format: csv\n      rows: |\n        entity_type,quarantined_count,is_dq_valid\n        encounters,1.00,false\n        medications,2,TRUE"
    );
    let n = raw.split('\n').count();
    let block = UnitTestYamlBlock::new(raw.clone(), 1, 1, n);
    // The given csv data row is at block line 7 (1: -name, 2: given, 3: -input
    // ref, 4: format, 5: rows |, 6: header, 7: first data row). The hunk edits
    // ONLY that row: removed `…,old_count,…`, added `…,new_count,…` (the
    // working tree), so the block stays aligned and the header + the entire
    // expect sub-block survive as Context.
    let hunk = Hunk {
        new_start: 7,
        new_len: 1,
        removed_lines: vec![format!("          encounters,{old_count},false")],
        added_lines: vec![format!("          encounters,{new_count},false")],
    };
    let diff = PrDiff {
        renames: Vec::new(),
        files: vec![FileHunks {
            path: "models/marts/_unit_tests.yml".to_owned(),
            hunks: vec![hunk],
        }],
    };
    let index = NormalizedDiffIndex::new(&diff, None);
    let changed: InScopeSet = [FUSION_TEST_ID.to_owned()].into_iter().collect();
    let mut blocks = HashMap::new();
    blocks.insert(FUSION_TEST_ID.to_owned(), block);
    reconstruct_table_diffs(&manifest, &changed, &blocks, &index)
}

/// Render the fusion manifest as a PR-diff report with the reconstructed
/// `data_diffs` threaded in. The fixture's model + unit test are real; only
/// the in-scope/changed sets + the data_diffs map are supplied here.
fn render_fusion_report(filename: &str, data_diffs: HashMap<String, UnitTestDataDiff>) -> String {
    let manifest = load_fusion_manifest();
    let in_scope: InScopeSet = [FUSION_TEST_ID.to_owned()].into_iter().collect();
    let models: ModelInScopeSet = [NodeId::new("model.fusion_demo.dq_rollup")]
        .into_iter()
        .collect();
    let out = tmp(filename);
    let _ = std::fs::remove_file(&out);
    render_report(
        &out,
        &manifest,
        &in_scope,
        &models,
        &in_scope,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &data_diffs,
        "",
        ScopeSource::PrDiff,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let p = out.to_str().expect("report path is valid UTF-8");
    format!("file://{p}")
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn fusion_csv_format_only_shows_no_diff_cell_but_value_change_shows_old_to_new() {
    // ===== Format-only reformat: 1 → 1.00 converges → NO diff cell =====
    // The Rust pipeline emits NO entry (has_real_change()==false), so the
    // map is empty: the given grid is the plain Current grid, no toggle.
    let format_only = reconstruct_fusion_data_diffs("1", "1.00");
    assert!(
        format_only.is_empty(),
        "a format-only reformat (1 vs 1.00) must NOT reconstruct a data_diff \
         (cute-dbt#127 value-inference convergence); got {format_only:?}",
    );
    let url_format_only = render_fusion_report("headless_fusion_format_only.html", format_only);

    // ===== Genuine value change: 9 → 1 → an entry with old → new =====
    let value_change = reconstruct_fusion_data_diffs("1", "9");
    assert!(
        value_change.contains_key(FUSION_TEST_ID),
        "a genuine value change (9 → 1) MUST reconstruct a data_diff entry; \
         got {value_change:?}",
    );
    let url_value_change = render_fusion_report("headless_fusion_value_change.html", value_change);

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");

    // ===== Format-only report: plain Current grid, NO cell-diff =====
    tab.navigate_to(&url_format_only)
        .expect("navigate format-only");
    tab.wait_until_navigated()
        .expect("await format-only navigation");
    show_all_inputs(&tab);
    // The given grid renders (the fusion csv parses to a real table)…
    assert!(
        visible(&tab, ".given-section table.given-table"),
        "the fusion given csv grid renders",
    );
    // …but a format-only reformat shows NO cell-diff toggle and NO old→new.
    assert!(
        eval_bool(&tab, "document.querySelector('.cell-diff-toggle') === null"),
        "format-only reformat (1 vs 1.00) shows NO Current↔Diff cell-diff toggle",
    );
    assert!(
        eval_bool(&tab, "document.querySelector('.cell-old') === null"),
        "format-only reformat shows NO inline old → new diff cell",
    );
    // cute-dbt#138 — the FIDELITY fix, asserted on the EXPECT grid. The
    // fixture's NEW `expect` csv authors `1.00` for `quarantined_count` and
    // `TRUE` for `is_dq_valid`; those authored tokens must survive to the cell
    // text even though their canonical keys are Number("1") / Bool(true).
    // Pre-#138 the Current grid normalized them to `1` / `true`.
    assert!(
        eval_bool(
            &tab,
            "Array.prototype.some.call(\
               document.querySelectorAll('.expected-table td.cell-num'),\
               function (td) { return td.textContent.trim() === '1.00'; })"
        ),
        "the EXPECT Current grid shows the AUTHORED `1.00`, not the normalized `1` (cute-dbt#138)",
    );
    assert!(
        eval_bool(
            &tab,
            "Array.prototype.some.call(\
               document.querySelectorAll('.expected-table td'),\
               function (td) { return td.textContent.trim() === 'TRUE'; })"
        ),
        "the EXPECT Current grid shows the AUTHORED `TRUE`, not the normalized `true` (cute-dbt#138)",
    );
    // The `1.00` cell is still styled numeric (the class comes from the
    // canonical key, not the display token), so the column sorts numerically.
    assert!(
        eval_bool(
            &tab,
            "Array.prototype.every.call(\
               document.querySelectorAll('.expected-table td'),\
               function (td) {\
                 return td.textContent.trim() !== '1.00' || td.classList.contains('cell-num');\
               })"
        ),
        "the authored `1.00` cell keeps numeric styling from its key (cute-dbt#138)",
    );

    // ===== Value-change report: Diff view tints the changed cell 9 → 1 =====
    tab.navigate_to(&url_value_change)
        .expect("navigate value-change");
    tab.wait_until_navigated()
        .expect("await value-change navigation");
    wait_for_document_ready(&tab);
    show_all_inputs(&tab);
    assert!(
        eval_bool(&tab, "document.querySelector('.cell-diff-toggle') !== null"),
        "a genuine value change renders the Current↔Diff cell-diff toggle",
    );
    assert!(
        visible(&tab, ".cell-diff-table .cell-changed .cell-old"),
        "the value-change Diff view renders the inline old value",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.cell-diff-table .cell-changed .cell-old').textContent.trim()"
        ),
        "9",
        "the old quarantined_count was 9 (from the Removed csv line)",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.cell-diff-table .cell-changed .cell-new').textContent.trim()"
        ),
        "1",
        "the new quarantined_count is 1 (the working-tree value), old → new",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#137: literal-row sql given renders as a TABLE -----------
//
// End-to-end over the COMMITTED `tests/fixtures/sql-literal-given.json`,
// loaded through the REAL manifest adapter. The unit test carries TWO
// `format: sql` givens:
//
//   * `ref('literal_checks')` — a literal-row `SELECT … UNION ALL …` that
//     cute-dbt#137 tabulates: the Current view must render a `given-table`
//     data grid (identical affordance to dict/csv), NOT the sql code block.
//   * `ref('opaque_checks')` — a non-literal sql (a real `FROM … WHERE`)
//     that is conservatively rejected: the Current view must fall back to
//     the `.fixture-sql-block-wrap` syntax-highlighted code block.
//
// Driving through the real adapter + renderer ties the DOM assertion to the
// actual `table_from_manifest_rows` literal-sql producer.

/// The committed sql-literal fixture's unit test id.
const SQL_LITERAL_TEST_ID: &str = "unit_test.sql_literal_demo.test_dq_flags_sql_literal_givens";

/// Load the committed sql-literal-given manifest through the REAL adapter.
fn load_sql_literal_manifest() -> Manifest {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sql-literal-given.json");
    FileManifestSource
        .load(&path)
        .expect("sql-literal-given fixture loads as a v12 manifest")
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn literal_sql_given_renders_as_table_non_literal_falls_back_to_code_block() {
    let manifest = load_sql_literal_manifest();
    let in_scope: InScopeSet = [SQL_LITERAL_TEST_ID.to_owned()].into_iter().collect();
    let models: ModelInScopeSet = [NodeId::new("model.sql_literal_demo.dq_flags")]
        .into_iter()
        .collect();
    let out = tmp("headless_sql_literal_given.html");
    let _ = std::fs::remove_file(&out);
    render_report(
        &out,
        &manifest,
        &in_scope,
        &models,
        &in_scope,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "",
        ScopeSource::PrDiff,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let p = out.to_str().expect("report path is valid UTF-8");
    let url = format!("file://{p}");

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    show_all_inputs(&tab);

    // Exactly two given sections render (the two sql givens). The `'` in the
    // `ref('…')` input names can't go through `querySelector('…')`'s
    // single-quoted JS string, so these assertions select by class +
    // attribute-substring (no literal `'` in the selector) instead of the
    // full `data-input-name` value.

    // The LITERAL-row sql given tabulates → a Current data grid renders. Its
    // section is identifiable by the `literal_checks` substring of its
    // `data-input-name`.
    assert!(
        eval_bool(
            &tab,
            "(document.querySelector('.given-section[data-input-name*=literal_checks] table.given-table') || {}).offsetHeight > 0"
        ),
        "the literal-row sql given (cute-dbt#137) renders as a data table in the Current view",
    );
    // Its tabulated cells carry the literal values: the string `encounters`.
    assert!(
        eval_bool(
            &tab,
            "Array.prototype.some.call(\
               document.querySelectorAll('.given-section[data-input-name*=literal_checks] table.given-table td'),\
               function (td) { return td.textContent.trim() === 'encounters'; })"
        ),
        "the literal-sql table shows the authored string literal encounters",
    );

    // The NON-literal sql given is rejected → the sql code-block fallback
    // renders, and NO data table is present in that section.
    assert!(
        eval_bool(
            &tab,
            "(document.querySelector('.given-section[data-input-name*=opaque_checks] .fixture-sql-block-wrap') || {}).offsetHeight > 0"
        ),
        "the non-literal sql given falls back to the syntax-highlighted code block",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.given-section[data-input-name*=opaque_checks] table.given-table') === null"
        ),
        "the non-literal sql given renders NO data table (conservative reject)",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#132: SQL token-stream highlighter contract -------------

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn sql_tokenizer_pins_keyword_jinja_and_string_classification() {
    // CHARACTERIZATION test for the token-stream highlighter that shipped in
    // THIS PR (commit 6f4a756). The JS highlighter sits OUTSIDE every Rust gate
    // (mutation/CRAP only see domain Rust), and had zero committed assertions on
    // its documented invariants — this pins the classification contract,
    // INCLUDING the deliberate keyword-as-column limitation (see assertion 4).
    let url = render_pr_diff_to_file(
        "headless_sql_tokenizer.html",
        vec![model_node("model.shop.dim_a")],
        vec![("unit_test.shop.dim_a.t", unit_test("t", "dim_a"))],
        &["model.shop.dim_a"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // (1) GAP-FREE invariant: tokenizeSql(s).join("") === s over a mixed string
    //     (keywords, a keyword-substring identifier, jinja, a jinja string).
    let gapfree = eval_string(
        &tab,
        "(function(){var s=\"select a.from_date, count(*) from t where x = {{ ref('y') }}\";\
          return window.__cuteTokenizeSql(s).map(function(t){return t.text;}).join('')===s ? 'OK':'BAD';})()",
    );
    assert_eq!(gapfree, "OK", "tokenizeSql is gap-free over its raw input");

    // (2) Keyword-as-SUBSTRING is SAFE: `from_date` scans as ONE identifier and
    //     is looked up WHOLE, so it does NOT highlight the `from` keyword.
    let from_date = eval_string(
        &tab,
        "(function(){var ts=window.__cuteTokenizeSql('from_date');\
          return ts.length===1 ? ('['+ts[0].cls+']') : 'MULTI';})()",
    );
    assert_eq!(
        from_date, "[]",
        "from_date is a single PLAIN identifier token, not a keyword"
    );

    // (3) A standalone `from` DOES classify as a keyword.
    let from_kw = eval_string(
        &tab,
        "(function(){return window.__cuteTokenizeSql('from')[0].cls;})()",
    );
    assert_eq!(from_kw, "sql-keyword", "a bare `from` is a keyword");

    // (4) KNOWN LIMITATION, pinned as a DELIBERATE decision (not a bug): a
    //     column/alias/CTE whose ENTIRE name is a keyword false-highlights,
    //     because this is lexer-only highlighting with NO render-time SQL parse
    //     (a parse would fight cute-dbt's zero-egress / zero-compute property).
    //     GitHub, Prism, and highlight.js all behave the same way. The keyword
    //     set is deliberately CONSERVATIVE — `name`, `date`, `value`, `status`,
    //     `id`, `amount` are intentionally excluded — so the practical surface
    //     is narrow. Changing this assertion is a conscious scope decision.
    let count_as_col = eval_string(
        &tab,
        "(function(){var ts=window.__cuteTokenizeSql('select count from t');\
          for(var i=0;i<ts.length;i++){if(ts[i].text==='count')return ts[i].cls;}return 'NONE';})()",
    );
    assert_eq!(
        count_as_col, "sql-keyword",
        "PINNED limitation: `count` used as a column still reads as a keyword (lexer-only)",
    );

    // (5) NESTED jinja (the headline #132 fix): a string INSIDE `{{ }}` reads as
    //     sql-string (`'stg_orders'`), and the dbt function name reads sql-jinja.
    let nested = eval_string(
        &tab,
        "(function(){var ts=window.__cuteTokenizeSql(\"{{ ref('stg_orders') }}\");\
          var s='',j='';for(var i=0;i<ts.length;i++){\
            if(ts[i].cls==='sql-string')s=ts[i].text;\
            if(ts[i].text==='ref')j=ts[i].cls;}\
          return s+'|'+j;})()",
    );
    assert_eq!(
        nested, "'stg_orders'|sql-jinja",
        "a string inside jinja is sql-string; the dbt function name is sql-jinja",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#132: configurable fold context + global expand/collapse ---

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn render_block_diff_honors_a_configurable_fold_pad() {
    // The "Context lines" control flows into renderBlockDiff as an optional 3rd
    // arg (the .diff-context-input handler sets the module default; the tests
    // pin specific pads directly without touching shared state). 10 context
    // lines between two changes:
    //   pad 3 (default) -> head 3 + tail 3, hidden 4
    //   pad 1           -> head 1 + tail 1, hidden 8
    //   pad 5           -> head 5 + tail 5, hidden 0 (< FOLD_MIN_HIDDEN) -> NO fold
    let url = render_pr_diff_to_file(
        "headless_fold_pad.html",
        vec![model_node("model.shop.dim_a")],
        vec![("unit_test.shop.dim_a.t", unit_test("t", "dim_a"))],
        &["model.shop.dim_a"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    let long_diff = format!(
        "{{lines:[{{kind:'removed',text:'a',emphasis:null}},\
         {{kind:'added',text:'b',emphasis:null}},{ctx},\
         {{kind:'removed',text:'c',emphasis:null}},\
         {{kind:'added',text:'d',emphasis:null}}]}}",
        ctx = ctx_lines_js(10),
    );

    let default_html = eval_string(
        &tab,
        &format!("window.__cuteRenderBlockDiff({long_diff}, window.__cuteTokenizeSql)"),
    );
    assert!(
        default_html.contains("Show 4 unchanged lines"),
        "default pad 3 hides 4 of the 10 context lines: {default_html}",
    );

    let pad1_html = eval_string(
        &tab,
        &format!("window.__cuteRenderBlockDiff({long_diff}, window.__cuteTokenizeSql, 1)"),
    );
    assert!(
        pad1_html.contains("Show 8 unchanged lines"),
        "pad 1 keeps 1 context line each side, hiding 8: {pad1_html}",
    );

    let pad5_html = eval_string(
        &tab,
        &format!("window.__cuteRenderBlockDiff({long_diff}, window.__cuteTokenizeSql, 5)"),
    );
    assert!(
        !pad5_html.contains("diff-fold"),
        "pad 5 keeps 5 context each side (hidden 0 < FOLD_MIN_HIDDEN), so NO fold: {pad5_html}",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn global_expand_collapse_mirrors_every_fold() {
    // The global expand-all/collapse-all is a SYMMETRIC DOM MIRROR (setAllFolds),
    // NOT a re-render: expand reveals every folded middle line and relabels the
    // (still-visible) per-hunk controls Show->Hide; collapse restores both
    // EXACTLY (#136 bidirectional). A re-render would have reset the SQL
    // File<->Diff view and re-flashed mermaid. Verified through the __cute* seams
    // on a mounted folded block (the report's own diff is not guaranteed long
    // enough to fold) plus the controls strip.
    let url = render_pr_diff_to_file(
        "headless_global_fold.html",
        vec![model_node("model.shop.dim_a")],
        vec![("unit_test.shop.dim_a.t", unit_test("t", "dim_a"))],
        &["model.shop.dim_a"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // The controls strip renders in PR-diff mode (now Expand-all only; the
    // configurable context-lines input moved into the #139 settings cog panel,
    // where it still defaults to 3).
    assert_eq!(
        eval_string(
            &tab,
            "String(document.querySelectorAll('.diff-view-controls').length)"
        ),
        "1",
        "the diff-view controls strip renders in PR-diff mode",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#settings-context-input').value"
        ),
        "3",
        "the settings-panel context-lines input defaults to 3",
    );

    // Mount a folded block: 1+1 change, 10 context, 1+1 change -> 4 folded.
    let long_diff = format!(
        "{{lines:[{{kind:'removed',text:'a',emphasis:null}},\
         {{kind:'added',text:'b',emphasis:null}},{ctx},\
         {{kind:'removed',text:'c',emphasis:null}},\
         {{kind:'added',text:'d',emphasis:null}}]}}",
        ctx = ctx_lines_js(10),
    );
    let _ = eval(
        &tab,
        &format!(
            "(function(){{var g=document.createElement('code');g.id='gf';\
               g.innerHTML=window.__cuteRenderBlockDiff({long_diff}, window.__cuteTokenizeSql);\
               document.body.appendChild(g);}})()"
        ),
    );

    // Default state: 4 folded+hidden, 1 visible per-hunk control.
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#gf .diff-folded[hidden]').length"
        ),
        4,
        "starts with 4 hidden folded lines",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#gf .diff-fold:not([hidden])').length"
        ),
        1,
        "the per-hunk control is visible by default",
    );

    // Expand all -> 0 folded hidden, control hidden.
    let _ = eval(
        &tab,
        "window.__cuteExpandAllFolds(document.getElementById('gf'))",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#gf .diff-folded[hidden]').length"
        ),
        0,
        "expand-all reveals every folded line",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#gf .diff-fold:not([hidden])').length"
        ),
        1,
        "expand-all keeps the per-hunk control visible (#136 bidirectional)",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#gf .diff-fold').getAttribute('aria-expanded')"
        ),
        "true",
        "expand-all sets the per-hunk control to aria-expanded=true",
    );

    // Collapse all -> back to 4 folded hidden + visible control (exact restore).
    let _ = eval(
        &tab,
        "window.__cuteCollapseAllFolds(document.getElementById('gf'))",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#gf .diff-folded[hidden]').length"
        ),
        4,
        "collapse-all re-hides every folded line",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#gf .diff-fold:not([hidden])').length"
        ),
        1,
        "collapse-all keeps the per-hunk control visible",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#gf .diff-fold').getAttribute('aria-expanded')"
        ),
        "false",
        "collapse-all sets the per-hunk control back to aria-expanded=false",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#139: report settings menu (cog) -----------------------------

/// `data_diff` for one given input with a single Modified row carrying TWO
/// cells: a REAL value change (`qty` 100 -> 200, keys differ) AND a FORMAT-ONLY
/// change (`amt` displays `1.00` -> `1`, both keying to Number("1")). The
/// format-only cell is `changed: false` (the Rust normalized verdict), the real
/// cell `changed: true`. This is the cute-dbt#139 toggle anchor: the strict lens
/// must flip the format-only cell to changed; the normalized lens must keep it
/// unchanged. The format-only cell lives in a Modified row (the only place both
/// authored displays survive on the wire — an Unchanged row discards the OLD
/// display).
fn format_only_plus_real_data_diff(input: &str) -> UnitTestDataDiff {
    use cute_dbt::domain::{
        Cell, CellChange, CellValue, ColumnStatus, DiffColumn, FixtureTableDiff, NamedTableDiff,
        RowChange, RowChangeKind,
    };
    UnitTestDataDiff {
        given: vec![NamedTableDiff {
            ordinal: 0,
            input: input.to_owned(),
            diff: FixtureTableDiff {
                columns: vec![
                    DiffColumn {
                        name: "qty".into(),
                        status: ColumnStatus::Present,
                    },
                    DiffColumn {
                        name: "amt".into(),
                        status: ColumnStatus::Present,
                    },
                ],
                rows: vec![RowChange {
                    kind: RowChangeKind::Modified,
                    cells: vec![
                        // Real value change: 100 -> 200 (keys differ).
                        CellChange {
                            old: Cell::new(CellValue::Number("100".into())),
                            new: Cell::new(CellValue::Number("200".into())),
                            changed: true,
                        },
                        // Format-only change: display 1.00 -> 1, BOTH key
                        // Number("1") so the Rust verdict is `changed: false`.
                        // The strict lens (compare display) flips this to true.
                        CellChange {
                            old: Cell::with_display("1.00".into(), CellValue::Number("1".into())),
                            new: Cell::with_display("1".into(), CellValue::Number("1".into())),
                            changed: false,
                        },
                    ],
                }],
            },
        }],
        expect: None,
    }
}

/// Render the standard single-test PR-diff report whose sole given carries the
/// format-only-plus-real cell diff, then switch to All-inputs so the grid (and
/// its cell-diff Diff view) is on screen. Returns the open `Tab`.
fn settings_fixture_tab(browser: &Browser, filename: &str) -> std::sync::Arc<Tab> {
    let id = "unit_test.shop.dim_a.upd";
    let url = render_pr_diff_with_data_diffs(
        filename,
        vec![model_node_with_src("model.shop.dim_a")],
        vec![(
            id,
            unit_test_with_given("upd", "dim_a", serde_json::json!([{"qty": 200, "amt": 1}])),
        )],
        &["model.shop.dim_a"],
        &[id],
        vec![(id, format_only_plus_real_data_diff("ref('src')"))],
    );
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    show_all_inputs(&tab);
    tab
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn settings_cog_opens_and_closes_by_click_keyboard_and_outside() {
    // The cog (top-right, aria-labelled, aria-haspopup) toggles a non-blocking
    // panel: click opens (aria-expanded=true, panel visible), click closes,
    // Escape closes + returns focus to the cog, an outside click closes.
    let browser = launch_browser();
    let tab = settings_fixture_tab(&browser, "headless_settings_cog.html");

    // The cog is present, aria-labelled, collapsed at boot; panel hidden.
    assert!(
        eval_bool(&tab, "document.querySelector('.settings-cog') !== null"),
        "the settings cog renders in PR-diff mode",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.settings-cog').getAttribute('aria-label')"
        ),
        "Report settings",
        "the cog is aria-labelled",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.settings-cog').getAttribute('aria-expanded')"
        ),
        "false",
        "the cog starts collapsed",
    );
    assert!(
        !visible(&tab, "#settings-panel"),
        "the panel is hidden at boot",
    );

    // Click opens.
    let _ = eval(&tab, "document.querySelector('.settings-cog').click()");
    assert!(
        visible(&tab, "#settings-panel"),
        "clicking the cog opens the panel",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.settings-cog').getAttribute('aria-expanded')"
        ),
        "true",
        "the open cog reports aria-expanded=true",
    );

    // Escape closes and returns focus to the cog.
    let _ = eval(
        &tab,
        "document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}))",
    );
    assert!(!visible(&tab, "#settings-panel"), "Escape closes the panel",);
    assert!(
        eval_bool(
            &tab,
            "document.activeElement === document.querySelector('.settings-cog')"
        ),
        "Escape returns focus to the cog",
    );

    // Re-open, then an outside click (on the body) closes it.
    let _ = eval(&tab, "document.querySelector('.settings-cog').click()");
    assert!(visible(&tab, "#settings-panel"), "re-opened");
    let _ = eval(&tab, "document.body.click()");
    assert!(
        !visible(&tab, "#settings-panel"),
        "an outside click closes the panel",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn settings_context_lines_refolds_block_diffs_live() {
    // Changing the panel's context-lines input re-folds the live block diffs
    // (the same renderForSelectedModel re-render path) and updates diffFoldPad.
    // Verified through the __cuteRenderBlockDiff seam (the report's own diff is
    // not guaranteed long enough to fold): set the input to 1 and assert a
    // freshly-rendered 10-context block now hides 8 (head 1 + tail 1) lines.
    let browser = launch_browser();
    let tab = settings_fixture_tab(&browser, "headless_settings_context.html");

    let long_diff = format!(
        "{{lines:[{{kind:'removed',text:'a',emphasis:null}},\
         {{kind:'added',text:'b',emphasis:null}},{ctx},\
         {{kind:'removed',text:'c',emphasis:null}},\
         {{kind:'added',text:'d',emphasis:null}}]}}",
        ctx = ctx_lines_js(10),
    );

    // Default pad 3: a fresh block hides 4.
    let default_html = eval_string(
        &tab,
        &format!("window.__cuteRenderBlockDiff({long_diff}, window.__cuteTokenizeSql)"),
    );
    assert!(
        default_html.contains("Show 4 unchanged lines"),
        "default pad 3 hides 4: {default_html}",
    );

    // Set the panel input to 1 and fire change -> diffFoldPad becomes 1.
    let _ = eval(
        &tab,
        "(function(){var i=document.querySelector('#settings-context-input');\
           i.value='1';i.dispatchEvent(new Event('change'));})()",
    );
    let pad1_html = eval_string(
        &tab,
        &format!("window.__cuteRenderBlockDiff({long_diff}, window.__cuteTokenizeSql)"),
    );
    assert!(
        pad1_html.contains("Show 8 unchanged lines"),
        "after setting context lines to 1, a fresh block hides 8: {pad1_html}",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn settings_normalize_toggle_flips_the_format_only_cell_lens() {
    // The normalize-equality lens, driven from BOTH the settings switch AND the
    // __cuteCellChanged seam: ON (default) hides a format-only cell change
    // (key-equal); OFF flags it (display differs). The real value change stays
    // flagged either way. Row alignment is never re-paired — only the per-cell
    // flag + the row-modified rollup move.
    let browser = launch_browser();
    let tab = settings_fixture_tab(&browser, "headless_settings_normalize.html");

    // Pin the lens directly on the two cells via the seam (no DOM dependency).
    let real = "{old:{display:'100',key:{t:'number',v:'100'}},\
                 new:{display:'200',key:{t:'number',v:'200'}},changed:true}";
    let fmt = "{old:{display:'1.00',key:{t:'number',v:'1'}},\
               new:{display:'1',key:{t:'number',v:'1'}},changed:false}";

    // Normalized (default ON): the real cell flags, the format-only does not.
    assert!(
        eval_bool(&tab, &format!("window.__cuteCellChanged({real})")),
        "normalized lens flags a real value change",
    );
    assert!(
        !eval_bool(&tab, &format!("window.__cuteCellChanged({fmt})")),
        "normalized lens HIDES a format-only change",
    );

    // The Diff grid: exactly one changed cell (the real one), so ONE
    // .cell-changed and the row keeps its row-modified tint.
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.cell-diff-table tr.row-modified .cell-changed').length"
        ),
        1,
        "normalized: only the real value change renders as a changed cell",
    );

    // Flip the switch OFF (strict) via the panel checkbox.
    let _ = eval(&tab, "document.querySelector('.settings-cog').click()");
    let _ = eval(
        &tab,
        "(function(){var c=document.querySelector('#settings-normalize-input');\
           c.checked=false;c.dispatchEvent(new Event('change'));})()",
    );

    // Strict lens: the format-only cell now flags (displays differ).
    assert!(
        eval_bool(&tab, &format!("window.__cuteCellChanged({fmt})")),
        "strict lens FLAGS a format-only change (displays differ)",
    );
    // And the re-render now shows BOTH cells changed in the Diff grid.
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.cell-diff-table tr.row-modified .cell-changed').length"
        ),
        2,
        "strict: both the real and the format-only change render as changed cells",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn settings_persist_across_reload_where_supported() {
    // Settings persist across a reload via localStorage where available; under
    // a file:// origin where localStorage throws, the load is a no-op (defaults
    // hold) and nothing crashes. Either way the page renders. We assert the
    // happy path when storage is available, else the graceful-default path.
    let browser = launch_browser();
    let tab = settings_fixture_tab(&browser, "headless_settings_persist.html");

    // Is localStorage usable at this origin? (file:// may throw or be null.)
    let storage_ok = eval_bool(
        &tab,
        "(function(){try{if(!window.localStorage)return false;\
           window.localStorage.setItem('__probe','1');\
           window.localStorage.removeItem('__probe');return true;}\
           catch(e){return false;}})()",
    );

    // Change both settings through the panel.
    let _ = eval(&tab, "document.querySelector('.settings-cog').click()");
    let _ = eval(
        &tab,
        "(function(){var i=document.querySelector('#settings-context-input');\
           i.value='7';i.dispatchEvent(new Event('change'));\
           var c=document.querySelector('#settings-normalize-input');\
           c.checked=false;c.dispatchEvent(new Event('change'));})()",
    );

    if storage_ok {
        // Persisted blob carries the new values.
        let raw = eval_string(
            &tab,
            "window.localStorage.getItem('cute-dbt.settings.v1') || ''",
        );
        assert!(
            raw.contains("\"contextLines\":7") && raw.contains("\"normalizeEquality\":false"),
            "settings persisted to localStorage: {raw}",
        );

        // Reload and assert the controls hydrate to the persisted values. The
        // boot `$(function(){...})` (bindSettingsMenu hydrates the controls)
        // runs after jQuery-ready, which `wait_until_navigated` alone races —
        // poll the hydrated input value with a bounded retry.
        tab.reload(false, None).expect("reload");
        tab.wait_until_navigated().expect("await reload");
        wait_for_document_ready(&tab);
        let mut ctx_val = String::new();
        for _ in 0..50 {
            ctx_val = eval_string(
                &tab,
                "(document.querySelector('#settings-context-input')||{}).value||''",
            );
            if ctx_val == "7" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(
            ctx_val, "7",
            "context-lines hydrates from localStorage after reload",
        );
        assert!(
            !eval_bool(
                &tab,
                "document.querySelector('#settings-normalize-input').checked"
            ),
            "normalize switch hydrates to OFF from localStorage after reload",
        );
    } else {
        // file:// no-storage path: the in-memory settings still drive the live
        // UI (the switch we just flipped is OFF) and the page did not crash.
        assert!(
            !eval_bool(
                &tab,
                "document.querySelector('#settings-normalize-input').checked"
            ),
            "without storage, the in-memory toggle still reflects the live OFF state",
        );
    }

    let _ = tab.close(true);
}

// --- cute-dbt#178: appearance settings + unified/split diff layouts ---

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn appearance_settings_flip_theme_density_diff_layout_and_persist() {
    // The cute-dbt#178 appearance engine (theme.js): the settings panel's
    // static controls flip the html-level appearance attributes the chassis
    // CSS keys on, DataTables dark mode syncs via html.dark, the diff-layout
    // seg-radio switches the rendered unified <-> split views, and the whole
    // appearance state persists under cute-dbt.appearance.v1.
    let diff = BlockDiff {
        lines: vec![
            dl(DiffLineKind::Context, "select id", None),
            dl(DiffLineKind::Removed, "from t", Some((5, 6))),
            dl(DiffLineKind::Added, "from u", Some((5, 6))),
        ],
    };
    let url = render_pr_diff_with_sql_diffs(
        "headless_appearance.html",
        vec![model_node_with_raw("model.shop.dim_a", "select id\nfrom u")],
        vec![("unit_test.shop.dim_a.t", unit_test("t", "dim_a"))],
        &["model.shop.dim_a"],
        vec![("model.shop.dim_a", diff)],
    );
    // Wide launch window: the explicit-Split media floor is 760px, so the
    // layout flip below must not be at the responsive fallback's mercy.
    let browser = launch_browser_sized(Some((1280, 900)));
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    const ROOT: &str = "document.documentElement";

    // ===== boot defaults: the template attrs + theme.js boot =====
    assert_eq!(
        eval_string(&tab, &format!("{ROOT}.getAttribute('data-style')")),
        "soft",
        "the default style pack is soft (template attr, re-applied at boot)",
    );
    // Boot follows the HOST's prefers-color-scheme (headless Chrome inherits
    // the OS preference, so the exact theme is platform-dependent — the #158
    // lesson: never pin a platform value). Assert the scheme-FOLLOWING
    // contract instead, then pin a known state via the Light chip.
    assert!(
        eval_bool(
            &tab,
            &format!(
                "{ROOT}.getAttribute('data-theme') === \
                 ((window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) ? 'dark' : 'light')"
            )
        ),
        "boot applies the prefers-color-scheme default (saved -> scheme -> light)",
    );

    // ===== theme chip: [data-theme] flips + DataTables dark sync =====
    let _ = eval(&tab, "document.querySelector('.settings-cog').click()");
    let _ = eval(
        &tab,
        "document.querySelector('.theme-chip[data-theme-id=\"light\"]').click()",
    );
    assert_eq!(
        eval_string(&tab, &format!("{ROOT}.getAttribute('data-theme')")),
        "light",
        "clicking the Light chip sets html[data-theme=light]",
    );
    assert!(
        !eval_bool(&tab, &format!("{ROOT}.classList.contains('dark')")),
        "no html.dark class on the light theme",
    );
    // ===== cute-dbt#198 — the pass-2 themes ride the same contract =====
    // One NEW light-family id (latte) and one NEW dark-family id (dracula):
    // the chip flips [data-theme] and the html.dark DataTables sync follows
    // the theme's family.
    let _ = eval(
        &tab,
        "document.querySelector('.theme-chip[data-theme-id=\"latte\"]').click()",
    );
    assert_eq!(
        eval_string(&tab, &format!("{ROOT}.getAttribute('data-theme')")),
        "latte",
        "clicking the Catppuccin Latte chip sets html[data-theme=latte]",
    );
    assert!(
        !eval_bool(&tab, &format!("{ROOT}.classList.contains('dark')")),
        "latte is light-family — no html.dark class",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.theme-chip[data-theme-id=\"dracula\"]').click()",
    );
    assert_eq!(
        eval_string(&tab, &format!("{ROOT}.getAttribute('data-theme')")),
        "dracula",
        "clicking the Dracula chip sets html[data-theme=dracula]",
    );
    assert!(
        eval_bool(&tab, &format!("{ROOT}.classList.contains('dark')")),
        "dracula is dark-family — html.dark toggles on (DataTables sync)",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.theme-grid .theme-chip').length"
        ),
        8,
        "the settings theme grid lists all 8 themes",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.theme-chip[data-theme-id=\"dark\"]').click()",
    );
    assert_eq!(
        eval_string(&tab, &format!("{ROOT}.getAttribute('data-theme')")),
        "dark",
        "clicking the Dark chip sets html[data-theme=dark]",
    );
    assert!(
        eval_bool(&tab, &format!("{ROOT}.classList.contains('dark')")),
        "the dark theme toggles html.dark — the DataTables dark-mode sync lever",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.theme-chip[data-theme-id=\"dark\"]').getAttribute('aria-pressed')"
        ),
        "true",
        "the active theme chip reports aria-pressed=true",
    );

    // ===== density seg-radio: [data-density] =====
    let _ = eval(
        &tab,
        "document.querySelector('.density-seg button[data-density=\"compact\"]').click()",
    );
    assert_eq!(
        eval_string(&tab, &format!("{ROOT}.getAttribute('data-density')")),
        "compact",
        "clicking Compact sets html[data-density=compact]",
    );

    // ===== diff layout: unified <-> split over the SAME rendered diff =====
    // Both views are emitted into the Model-SQL diff <pre>; CSS shows one.
    const UNIFIED_DISPLAY: &str = "getComputedStyle(document.querySelector('.model-sql .sql-diff-view .diff-unified')).display";
    const SPLIT_DISPLAY: &str =
        "getComputedStyle(document.querySelector('.model-sql .sql-diff-view .diff-split')).display";
    let _ = eval(
        &tab,
        "document.querySelector('.difflayout-seg button[data-difflayout=\"unified\"]').click()",
    );
    assert_eq!(
        eval_string(&tab, &format!("{ROOT}.getAttribute('data-difflayout')")),
        "unified",
        "clicking Unified sets html[data-difflayout=unified]",
    );
    assert_ne!(
        eval_string(&tab, UNIFIED_DISPLAY),
        "none",
        "Unified layout shows the unified view",
    );
    assert_eq!(
        eval_string(&tab, SPLIT_DISPLAY),
        "none",
        "Unified layout hides the split table",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.difflayout-seg button[data-difflayout=\"split\"]').click()",
    );
    assert_eq!(
        eval_string(&tab, &format!("{ROOT}.getAttribute('data-difflayout')")),
        "split",
        "clicking Split sets html[data-difflayout=split]",
    );
    assert_eq!(
        eval_string(&tab, UNIFIED_DISPLAY),
        "none",
        "Split layout (wide viewport) hides the unified view",
    );
    assert_eq!(
        eval_string(&tab, SPLIT_DISPLAY),
        "table",
        "Split layout (wide viewport) shows the split table",
    );

    // ===== cute-dbt#198 — the #188 diff-cells colour/marks control is =====
    // retired (design pass-2): no control markup, no [data-diffstyle] hook.
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.diff-seg, [data-diffstyle]').length"
        ),
        0,
        "the diff-cells colour/marks control is retired — no .diff-seg \
         markup and no data-diffstyle attribute anywhere",
    );

    // ===== persistence: the appearance blob (where storage is usable) =====
    let storage_ok = eval_bool(
        &tab,
        "(function(){try{if(!window.localStorage)return false;\
           window.localStorage.setItem('__probe','1');\
           window.localStorage.removeItem('__probe');return true;}\
           catch(e){return false;}})()",
    );
    if storage_ok {
        let raw = eval_string(
            &tab,
            "window.localStorage.getItem('cute-dbt.appearance.v1') || ''",
        );
        assert!(
            raw.contains("\"theme\":\"dark\"")
                && raw.contains("\"density\":\"compact\"")
                && raw.contains("\"difflayout\":\"split\""),
            "the appearance state persisted under cute-dbt.appearance.v1: {raw}",
        );

        // cute-dbt#198 — a LEGACY persisted `diffstyle` key (written by the
        // retired #188 control) is ignored gracefully: boot copies only the
        // live keys, sets no data-diffstyle attribute, and the surviving
        // appearance keys still hydrate. Poll the hydrated theme — boot runs
        // after DOMContentLoaded, which wait_until_navigated alone races.
        // The poll expression is NULL-SAFE on documentElement: immediately
        // after a reload the document can be transiently empty
        // (documentElement === null), and the fail-loud eval() contract
        // (cute-dbt#109) would otherwise panic on the TypeError instead of
        // polling through it (seen twice on CI runners, 2026-06-10).
        let _ = eval(
            &tab,
            "window.localStorage.setItem('cute-dbt.appearance.v1', \
             JSON.stringify({theme:'dark',density:'compact',\
             difflayout:'split',diffstyle:'marks'}))",
        );
        tab.reload(false, None).expect("reload");
        tab.wait_until_navigated().expect("await reload");
        wait_for_document_ready(&tab);
        let mut theme = String::new();
        for _ in 0..50 {
            theme = eval_string(
                &tab,
                "(document.documentElement \
                 && document.documentElement.getAttribute('data-theme')) || ''",
            );
            if theme == "dark" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(
            theme, "dark",
            "the live appearance keys still hydrate alongside a legacy diffstyle key",
        );
        assert!(
            !eval_bool(&tab, &format!("{ROOT}.hasAttribute('data-diffstyle')")),
            "a legacy persisted diffstyle key leaves no data-diffstyle attribute",
        );
    }

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn split_diff_renders_the_same_block_diff_as_unified() {
    // cute-dbt#178 — the split renderer consumes the SAME BlockDiff lines
    // (kind/text/emphasis) verbatim: removed pairs left, added right,
    // context on both sides, word-level <strong> emphasis preserved, and the
    // unified renderer's two-column gutter numbers the same way.
    let url = render_pr_diff_to_file(
        "headless_split_diff_js.html",
        vec![model_node("model.shop.dim_a")],
        vec![("unit_test.shop.dim_a.t", unit_test("t", "dim_a"))],
        &["model.shop.dim_a"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    const DIFF_JS: &str = "{lines:[\
        {kind:'context',text:'select id',emphasis:null},\
        {kind:'removed',text:'from t',emphasis:[5,6]},\
        {kind:'added',text:'from u',emphasis:[5,6]}\
        ]}";

    // Unified: the gutter carries old|new pairs — the removed line has no
    // new number, the added line no old number.
    let unified = eval_string(
        &tab,
        &format!("window.__cuteRenderBlockDiff({DIFF_JS}, window.__cuteTokenizeSql)"),
    );
    assert!(
        unified.contains("dln dln-o") && unified.contains("dln dln-n"),
        "the unified renderer emits the two-column line-number gutter: {unified}",
    );
    assert!(
        unified.contains("<strong>u</strong>"),
        "the unified renderer keeps word-level emphasis on the added line: {unified}",
    );

    // Split: one row pairs the removed (left) and added (right) sides;
    // context appears on both sides; emphasis is preserved on both sides.
    let split = eval_string(
        &tab,
        &format!("window.__cuteRenderSplitDiff({DIFF_JS}, window.__cuteTokenizeSql)"),
    );
    assert!(
        split.contains("ds-removed") && split.contains("ds-added"),
        "the split renderer tints the removed/added sides: {split}",
    );
    assert!(
        split.contains("<strong>t</strong>") && split.contains("<strong>u</strong>"),
        "the split renderer keeps word-level emphasis on BOTH sides: {split}",
    );
    assert!(
        split.matches("ds-context").count() >= 4,
        "the context line renders on both sides (2 num + 2 code cells): {split}",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#145: incremental-model unit-test semantics ------------
//
// The rendered-DOM proof of the incremental affordances. The payload facts
// (`is_incremental`, `is_incremental_mode`, `is_this`) are pinned by
// `tests/steps/incremental_models.rs`; the badges + the expect-semantics
// tooltip are JS-generated over the inlined payload and so are absent from the
// static HTML — only a real runtime renders them. The headless harness renders
// domain objects in-process via `render_report` (NO wire/subprocess path), so
// the mode is set on the domain `UnitTest` directly via the cute-dbt#145
// builder and the model's `config.materialized` via `NodeConfig` — the two
// wire-shape divergences the BDD path injects do not apply on this path.

/// A `model` node whose `config.materialized` is set, so the payload's
/// `is_incremental` reflects `materialized == "incremental"`.
fn model_node_materialized(full_id: &str, materialized: &str) -> Node {
    Node::new(
        NodeId::new(full_id),
        "model",
        Checksum::new("sha256", "ck"),
        Some("select 1".to_owned()),
        // raw_code so the Model-SQL section renders — since cute-dbt#178 the
        // incremental badge lives in its code-card header (no static span).
        Some("select 1".to_owned()),
        DependsOn::default(),
        None,
        NodeConfig::new(
            BTreeMap::from([(
                "materialized".to_owned(),
                serde_json::Value::String(materialized.to_owned()),
            )]),
            false,
        ),
        None,
        BTreeMap::new(),
    )
}

/// A `UnitTest` carrying an explicit incremental `mode` (the
/// `overrides.macros.is_incremental` flag, set on the domain object via the
/// cute-dbt#145 builder) plus `given` inputs. Rows are empty — the headless
/// assertions key on the mode badge / tooltip / this-badge, not fixture data.
fn incremental_unit_test(
    name: &str,
    model_bare: &str,
    mode: Option<bool>,
    given_inputs: &[&str],
) -> UnitTest {
    let givens = given_inputs
        .iter()
        .map(|inp| {
            UnitTestGiven::new(
                (*inp).to_owned(),
                serde_json::Value::Array(Vec::new()),
                None,
                None,
            )
        })
        .collect();
    UnitTest::new(
        name.to_owned(),
        NodeId::new(model_bare),
        givens,
        UnitTestExpect::new(serde_json::Value::Null, None, None),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
    .with_incremental_mode(mode)
}

/// Select a unit test by its FULL node id — `#test-select` option values are
/// `t.id` (unlike `#model-select`, which keys on the bare model name).
fn select_test(tab: &Tab, test_id: &str) {
    let _ = eval(
        tab,
        &format!(
            "(function(){{var s=document.querySelector('#test-select');\
             s.value='{test_id}';s.dispatchEvent(new Event('change'));}})()"
        ),
    );
}

/// Trimmed text of the `.this-badge` on the `given: - input: this` section,
/// or `""` when absent. Iterates `.given-section` by `data-input-name` so the
/// awkward `ref('orders')` attribute-selector quoting is avoided.
fn this_given_badge_text(tab: &Tab) -> String {
    eval_string(
        tab,
        "(function(){\
           var s=Array.prototype.slice.call(document.querySelectorAll('.given-section'))\
             .filter(function(x){return x.getAttribute('data-input-name')==='this';})[0];\
           var b=s?s.querySelector('.this-badge'):null;return b?b.textContent.trim():'';})()",
    )
}

/// `true` when NO non-`this` given section carries a `.this-badge` (e.g. the
/// `ref('orders')` given must never be badged prior-model-state).
fn non_this_givens_unbadged(tab: &Tab) -> bool {
    eval_bool(
        tab,
        "Array.prototype.slice.call(document.querySelectorAll('.given-section'))\
           .filter(function(x){return x.getAttribute('data-input-name')!=='this';})\
           .every(function(x){return x.querySelector('.this-badge')===null;})",
    )
}

const MODE_BADGE: &str = "document.querySelector('.expected-panel .panel-header .mode-badge')";
const TOOLTIP: &str = "document.querySelector('.expected-panel .panel-header .expect-tooltip')";

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn incremental_badges_modes_tooltip_and_this_given() {
    // One report, two models:
    //   - dim_inc (incremental): mode_on  → Some(true)  + givens [this, ref]
    //                            mode_off → Some(false) + given  [this]
    //   - dim_tbl (table):       plain    → None        + given  [ref]
    //
    // The LOCKED D4 invariant: the expect-semantics tooltip rides the
    // AUTHORITATIVE bool (is_incremental_mode === true), NEVER the `this`-given
    // proxy, and NEVER on the full-refresh branch (there `expect` IS the final
    // table → the tooltip would be wrong-help). mode_off CARRIES a `this` given
    // precisely so its tooltip-absent assertion proves the gate keys off the
    // bool, not the proxy — and the mode_on → mode_off transition (no reload)
    // exercises the idempotent `.mode-badge, .expect-tooltip` clear on the
    // persistent `.panel-header`.
    // dim_empty is a modified-but-untested model (no unit tests) — selecting it
    // drives currentTest() to null, the leak path for a stale mode badge /
    // tooltip (renderExpectedPanel, which clears them, runs ONLY in the `if (t)`
    // arm of renderForSelectedModel).
    let url = render_to_file(
        "headless_incremental.html",
        vec![
            model_node_materialized("model.shop.dim_inc", "incremental"),
            model_node_materialized("model.shop.dim_tbl", "table"),
            model_node_materialized("model.shop.dim_empty", "table"),
        ],
        vec![
            (
                "unit_test.shop.dim_inc.mode_on",
                incremental_unit_test("mode_on", "dim_inc", Some(true), &["this", "ref('orders')"]),
            ),
            (
                "unit_test.shop.dim_inc.mode_off",
                incremental_unit_test("mode_off", "dim_inc", Some(false), &["this"]),
            ),
            (
                "unit_test.shop.dim_tbl.plain",
                incremental_unit_test("plain", "dim_tbl", None, &["ref('orders')"]),
            ),
        ],
        &[
            "model.shop.dim_inc",
            "model.shop.dim_tbl",
            "model.shop.dim_empty",
        ],
        &[], // 0 changed → auto-All mode → every in-scope test is selectable
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // ===== incremental model → the U1 model badge is visible =====
    // cute-dbt#178 — the badge is JS-created inside the Model-SQL code-card
    // header (the static template span is gone), so presence === visible.
    select_model(&tab, "dim_inc");
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-sql .code-header .incremental-badge') !== null"
        ),
        "the incremental-badge renders in the Model-SQL code-card header for an incremental model",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql .code-header .incremental-badge').textContent.trim()"
        ),
        "incremental",
        "the relocated badge keeps its label",
    );

    // ===== incremental-mode test (mode_on): badge + tooltip =====
    select_test(&tab, "unit_test.shop.dim_inc.mode_on");
    assert_eq!(
        eval_string(&tab, &format!("{MODE_BADGE}.textContent.trim()")),
        "incremental branch",
        "an incremental-mode test labels the Expected panel 'incremental branch'",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{MODE_BADGE}.classList.contains('mode-incremental')")
        ),
        "the incremental-branch badge carries the .mode-incremental class",
    );
    assert!(
        eval_bool(&tab, &format!("{TOOLTIP} !== null")),
        "an incremental-mode test shows the expect-semantics tooltip",
    );
    // cute-dbt#146 review — the tooltip is a FOCUSABLE button (keyboard + touch
    // reachable), not a hover-only `<span title>`.
    assert_eq!(
        eval_string(&tab, &format!("{TOOLTIP}.tagName")),
        "BUTTON",
        "the expect-tooltip is a focusable <button>, not a hover-only span",
    );
    // The dbt gotcha wording lives in the VISIBLE CSS bubble AND the aria-label.
    // Assert an ASCII substring — the tip contains em-dashes (U+2014), so a
    // full-string compare would mismatch. cute-dbt#159: the copy is now
    // strategy-invariant ("the rows the configured incremental strategy will
    // apply to the table") — true for all 5 strategies, where the old
    // "merged or inserted" was wrong for insert_overwrite / microbatch.
    let bubble_text = eval_string(
        &tab,
        &format!("{TOOLTIP}.querySelector('.expect-tooltip-bubble').textContent"),
    );
    assert!(
        bubble_text.contains("incremental strategy will apply to the table"),
        "the visible tooltip bubble explains Expected is the rows the strategy applies, got {bubble_text:?}",
    );
    assert!(
        !bubble_text.contains("merged or inserted"),
        "the bubble must NOT carry the old merge/append-centric wording (cute-dbt#159), got {bubble_text:?}",
    );
    let aria = eval_string(&tab, &format!("{TOOLTIP}.getAttribute('aria-label')"));
    assert!(
        aria.contains("incremental strategy will apply to the table"),
        "the tooltip aria-label carries the same strategy-invariant wording (a11y parity), got {aria:?}",
    );
    assert!(
        !aria.contains("merged or inserted"),
        "the aria-label must NOT carry the old merge/append-centric wording (cute-dbt#159), got {aria:?}",
    );
    // cute-dbt#146 review — the regression guard for "hover shows nothing": the
    // bubble is hidden until hover/focus, and FOCUS reveals it (the keyboard
    // path; `:hover` shares the same CSS rule, so a visible-on-focus bubble
    // proves the hover path paints too).
    const BUBBLE_VIS: &str = "getComputedStyle(document.querySelector('.expect-tooltip .expect-tooltip-bubble')).visibility";
    assert_eq!(
        eval_string(&tab, BUBBLE_VIS),
        "hidden",
        "the tooltip bubble is hidden until hover/focus",
    );
    let _ = eval(&tab, &format!("{TOOLTIP}.focus()"));
    assert_eq!(
        eval_string(&tab, BUBBLE_VIS),
        "visible",
        "focusing the tooltip reveals the bubble (the keyboard path a native title never had)",
    );

    // ===== the `this` given is prior-model-state; `ref(...)` is not =====
    show_all_inputs(&tab);
    assert_eq!(
        this_given_badge_text(&tab),
        "prior model state",
        "a `given: - input: this` is badged 'prior model state'",
    );
    assert!(
        non_this_givens_unbadged(&tab),
        "a ref(...) given carries no prior-model-state badge",
    );

    // ===== full-refresh-mode test (mode_off): the LOCKED proxy proof =====
    // mode_off carries a `this` given but is_incremental_mode === false → the
    // badge reads 'full-refresh branch', there is NO tooltip (the gate keys off
    // the bool, NOT the `this` proxy), and the mode_on tooltip was cleared.
    select_test(&tab, "unit_test.shop.dim_inc.mode_off");
    assert_eq!(
        eval_string(&tab, &format!("{MODE_BADGE}.textContent.trim()")),
        "full-refresh branch",
        "a full-refresh-mode test labels the Expected panel 'full-refresh branch'",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{MODE_BADGE}.classList.contains('mode-full-refresh')")
        ),
        "the full-refresh badge carries the .mode-full-refresh class",
    );
    assert!(
        eval_bool(&tab, &format!("{TOOLTIP} === null")),
        "LOCKED: no expect-semantics tooltip on a full-refresh test — even one \
         carrying a `this` given (the gate keys off is_incremental_mode, not the proxy); \
         this also proves the mode_on tooltip was cleared idempotently",
    );
    // The this-badge still shows on mode_off's `this` given — it is gated on
    // is_this alone, independent of the test's incremental branch.
    show_all_inputs(&tab);
    assert_eq!(
        this_given_badge_text(&tab),
        "prior model state",
        "the this-badge shows on a `this` given regardless of the test's branch",
    );

    // ===== table model → no incremental affordances (cross-model clear) =====
    // cute-dbt#178 — for a non-incremental model the badge is simply never
    // created (renderModelSql rebuilds the code-card header per model).
    select_model(&tab, "dim_tbl");
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.incremental-badge') === null"
        ),
        "no incremental-badge exists anywhere for a table model",
    );
    select_test(&tab, "unit_test.shop.dim_tbl.plain");
    assert!(
        eval_bool(&tab, &format!("{MODE_BADGE} === null")),
        "a test on a non-incremental model carries no mode badge (cross-model clear)",
    );
    assert!(
        eval_bool(&tab, &format!("{TOOLTIP} === null")),
        "a test on a non-incremental model shows no expect-semantics tooltip",
    );

    // ===== modified-but-untested model clears a LEAKED incremental tooltip =====
    // Re-establish an incremental-mode tooltip, then select a model with no
    // tests (currentTest() === null → renderExpectedPanel, which clears the
    // badge/tooltip, is NOT called). renderForSelectedModel must clear them
    // unconditionally, else the prior test's badge + tooltip leak onto the
    // persistent `.panel-header` (gemini review, PR #146).
    select_model(&tab, "dim_inc");
    select_test(&tab, "unit_test.shop.dim_inc.mode_on");
    assert!(
        eval_bool(&tab, &format!("{TOOLTIP} !== null")),
        "precondition: the incremental-mode tooltip is present before the switch",
    );
    select_model(&tab, "dim_empty");
    assert!(
        eval_bool(&tab, &format!("{MODE_BADGE} === null")),
        "selecting a modified-but-untested model clears the leaked mode badge",
    );
    assert!(
        eval_bool(&tab, &format!("{TOOLTIP} === null")),
        "selecting a modified-but-untested model clears the leaked expect-semantics tooltip",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#126 external fixture file rendering --------------------

/// Render a report whose tests carry external fixtures, threading the loaded
/// `ExternalFixtures` (what the cli `gather_external_fixtures` step produces)
/// through `render_report_with_externals`.
fn render_with_external_fixtures(
    filename: &str,
    nodes: Vec<Node>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    externals: HashMap<String, ExternalFixtures>,
) -> String {
    let all_ids: Vec<String> = tests.iter().map(|(id, _)| (*id).to_owned()).collect();
    let m = manifest(nodes, tests);
    let in_scope: InScopeSet = all_ids.iter().cloned().collect();
    let models: ModelInScopeSet = model_ids.iter().map(|id| NodeId::new(*id)).collect();
    let changed: InScopeSet = all_ids.into_iter().collect();
    let out = tmp(filename);
    let _ = std::fs::remove_file(&out);
    render_report_with_externals(
        &out,
        &m,
        &in_scope,
        &models,
        &changed,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &externals,
        "",
        ScopeSource::PrDiff,
        DEFAULT_REPORT_TITLE,
        None,
        &cute_dbt::domain::CheckPolicy::default(),
    )
    .expect("render writes the report");
    let p = out.to_str().expect("report path is valid UTF-8");
    format!("file://{p}")
}

/// A unit test with one external-fixture given (`rows: null` + a `fixture`
/// path — the confirmed fusion shape).
fn ut_external_given(name: &str, model_bare: &str, fixture: &str, format: &str) -> UnitTest {
    UnitTest::new(
        name.to_owned(),
        NodeId::new(model_bare),
        vec![UnitTestGiven::new(
            "ref('a')",
            serde_json::Value::Null,
            Some(format.to_owned()),
            Some(fixture.to_owned()),
        )],
        UnitTestExpect::new(serde_json::Value::Null, None, None),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
}

/// The `ExternalFixtures` the cli would build for a LOADED csv/sql given 0.
fn loaded_given_0(text: &str, format: &str) -> ExternalFixtures {
    let mut ext = ExternalFixtures::default();
    ext.given.insert(
        0,
        LoadedFixture {
            text: text.to_owned(),
            format: Some(format.to_owned()),
            table: external_fixture_table(text, Some(format)),
        },
    );
    ext
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn external_csv_fixture_renders_grid_with_provenance_and_no_affordance() {
    // The #126 happy path: a `fixture: tests/fixtures/a.csv` given whose file
    // the reader loaded renders a REAL cell grid (not the #98 silently-empty
    // affordance) + a "from <path>" provenance chip.
    let test_id = "unit_test.shop.m.t";
    let mut externals = HashMap::new();
    externals.insert(
        test_id.to_owned(),
        loaded_given_0("id,amount\n1,10\n2,20\n", "csv"),
    );
    let url = render_with_external_fixtures(
        "headless_ext_csv.html",
        vec![model_node("model.shop.m")],
        vec![(
            test_id,
            ut_external_given("t", "m", "tests/fixtures/a.csv", "csv"),
        )],
        &["model.shop.m"],
        externals,
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    show_all_inputs(&tab);

    assert!(
        visible(&tab, ".given-section table.given-table"),
        "an external csv fixture that loaded renders a real grid",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.given-section table.given-table tbody tr').length",
        ),
        2,
        "the two loaded csv rows render",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.given-section .external-fixture-note') === null",
        ),
        "the silently-empty-grid affordance must NOT appear when the fixture loaded",
    );
    assert!(
        eval_string(
            &tab,
            "(document.querySelector('.given-section .fixture-provenance') || {}).textContent || ''",
        )
        .contains("tests/fixtures/a.csv"),
        "the provenance chip names the external fixture path",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn unreadable_external_fixture_shows_affordance_not_grid() {
    // The fixture file could not be read (no project root / missing file): the
    // cli leaves it out of the externals map, so `rows` stays null and the
    // template falls back to the #98 affordance — never a grid, never a chip.
    let test_id = "unit_test.shop.m.t";
    let url = render_with_external_fixtures(
        "headless_ext_unreadable.html",
        vec![model_node("model.shop.m")],
        vec![(
            test_id,
            ut_external_given("t", "m", "tests/fixtures/a.csv", "csv"),
        )],
        &["model.shop.m"],
        HashMap::new(),
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    show_all_inputs(&tab);

    assert!(
        visible(&tab, ".given-section .external-fixture-note"),
        "an unreadable external fixture falls back to the #98 affordance",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.given-section table.given-table') === null",
        ),
        "no grid renders when the fixture is unreadable",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.given-section .fixture-provenance') === null",
        ),
        "no provenance chip on the unreadable affordance path",
    );

    let _ = tab.close(true);
}

/// A `model` node whose compiled SQL is exactly `compiled` — so the CTE
/// engine builds a real DAG from it (cute-dbt#155).
fn model_node_with_compiled(full_id: &str, compiled: &str) -> Node {
    Node::new(
        NodeId::new(full_id),
        "model",
        Checksum::new("sha256", "ck"),
        Some(compiled.to_owned()),
        None,
        DependsOn::default(),
        None,
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn self_named_import_cte_renders_distinct_dag_nodes_not_a_cycle() {
    // cute-dbt#155 end-to-end: a model named `orders` whose import CTE is
    // also named `orders` (`with orders as (...)`). Pre-fix the terminal's
    // id was the model name, so it collapsed with the import CTE into ONE
    // Mermaid node — a spurious `orders ↔ final` cycle, and the import
    // node's compiled-SQL panel was clobbered with the terminal's
    // `select * from final`. The fix keys node identity by the stable engine
    // name and labels the terminal `orders.sql`.
    let url = render_to_file(
        "headless_cte_name_collision.html",
        vec![model_node_with_compiled(
            "model.shop.orders",
            "with orders as (select * from raw_orders), \
                  final as (select * from orders) \
             select * from final",
        )],
        vec![("unit_test.shop.orders.t", unit_test("t", "orders"))],
        &["model.shop.orders"],
        &[],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate collision report");
    tab.wait_until_navigated().expect("await navigation");

    // The single in-scope model auto-selects on load; wait for Mermaid to
    // render its DAG SVG (startOnLoad: false → renders on demand).
    tab.wait_for_element_with_custom_timeout(
        ".cte-dag-mermaid svg",
        std::time::Duration::from_secs(15),
    )
    .expect("Mermaid DAG SVG renders");

    // Three DISTINCT nodes — orders (import), final (transform), and the
    // terminal labelled `orders.sql`. Pre-fix the collapse yielded two
    // nodes and the string `orders.sql` appeared nowhere.
    let node_count = eval(
        &tab,
        "document.querySelectorAll('.cte-dag-mermaid svg g.node').length",
    )
    .as_u64()
    .unwrap_or(0);
    assert_eq!(
        node_count, 3,
        "three distinct DAG nodes render (no name-collision collapse)",
    );
    let svg_text = eval_string(
        &tab,
        "document.querySelector('.cte-dag-mermaid svg').textContent",
    );
    assert!(
        svg_text.contains("orders.sql"),
        "the terminal node is labelled `orders.sql`, distinct from the import CTE: {svg_text}",
    );

    // a11y: the terminal node's aria-label announces its VISIBLE label
    // (`orders.sql`), not the internal id `(final select)` — they diverge
    // for the terminal (cute-dbt#155).
    let aria_matches_label = eval_bool(
        &tab,
        "Array.from(document.querySelectorAll('.cte-dag-mermaid svg g.node'))\
         .some(function(g){return (g.getAttribute('aria-label')||'').indexOf('orders.sql')>=0;})",
    );
    assert!(
        aria_matches_label,
        "the terminal node's aria-label announces its visible `orders.sql` label",
    );

    // The literal reported bug: click the import CTE node and assert its
    // compiled-SQL panel shows ITS OWN body (`raw_orders`), not the
    // terminal's `from final`.
    let clicked = eval_bool(
        &tab,
        "(function(){var g=document.querySelector('.cte-dag-mermaid svg g.node[data-node-id=\"orders\"]');\
          if(!g){return false;}g.dispatchEvent(new MouseEvent('click',{bubbles:true}));return true;})()",
    );
    assert!(
        clicked,
        "the import CTE node is present + clickable by its own id `orders`"
    );
    let detail_sql = eval_string(
        &tab,
        "(document.querySelector('.node-detail .sql-block')||{}).textContent||''",
    );
    assert!(
        detail_sql.contains("raw_orders"),
        "the import node shows its OWN compiled SQL: {detail_sql}",
    );
    assert!(
        !detail_sql.contains("from final"),
        "the import node SQL is not overwritten by the terminal's: {detail_sql}",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#157: mobile viewport blowout on the stacked panel -------

/// Build `n_rows` JSON dict rows each with `n_cols` columns of long,
/// unbreakable string values. The given/expected table cells are
/// `white-space: nowrap` (templates/report.html:172) and the table is
/// `width: max-content` (:170), so a row this wide overflows a half-viewport
/// column and trips the JS `reflowPanelsForOverflow` stack toggle at 375px —
/// the precise path that exposed the `.is-stacked { grid-template-columns:
/// 1fr }` blowout (the bare `1fr` = `minmax(auto,1fr)` whose `auto` floor
/// sized the single stacked track to the table's min-content).
fn wide_cols_rows(n_cols: usize, n_rows: usize) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = (0..n_rows)
        .map(|r| {
            let mut obj = serde_json::Map::new();
            for c in 0..n_cols {
                obj.insert(
                    format!("dimension_column_{c:02}"),
                    serde_json::Value::String(format!("value_r{r:02}_c{c:02}_unbreakable_token")),
                );
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::Value::Array(rows)
}

/// A unit test with a WIDE given table AND a WIDE expect table (both inline
/// dict fixtures), so both panels are populated with content wide enough to
/// trip the stack toggle.
fn wide_unit_test(name: &str, model_bare: &str) -> UnitTest {
    UnitTest::new(
        name.to_owned(),
        NodeId::new(model_bare),
        vec![UnitTestGiven::new(
            format!("ref('{model_bare}')"),
            wide_cols_rows(12, 3),
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(wide_cols_rows(12, 3), Some("dict".to_owned()), None),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn stacked_panel_does_not_blow_out_viewport_at_375px() {
    // cute-dbt#157 — on a narrow (phone) viewport the report's JS responsive
    // helper collapses the two-column Inspect/Expected `.panel-row` to one
    // column by adding `.is-stacked`. The bug: `.is-stacked` set the track to
    // a bare `1fr` (= `minmax(auto,1fr)`), whose `auto` floor sized the single
    // stacked track to the wide given/expected DataTable's min-content,
    // ballooning `<body>` past the viewport. The fix clamps the track to
    // `minmax(0,1fr)` (matching the desktop rule) so the table scrolls inside
    // its `.table-fit` wrapper as designed. This test drives a real Chromium at
    // a 375px (iPhone-class) viewport, populates BOTH panels with a wide table,
    // forces the reflow, and asserts no page-level horizontal overflow. On the
    // unfixed CSS it FAILS (documentElement.scrollWidth > innerWidth).

    let url = render_to_file(
        "headless_responsive_375.html",
        vec![model_node("model.shop.dim_wide")],
        vec![("unit_test.shop.dim_wide.t", wide_unit_test("t", "dim_wide"))],
        &["model.shop.dim_wide"],
        &["unit_test.shop.dim_wide.t"],
    );

    // Launch a DEDICATED 375x812 (iPhone-class) headless window. In headless
    // Chrome `window.innerWidth` tracks the launch window size (`--window-size`),
    // and the report's `width=device-width` viewport meta makes the layout
    // viewport follow it — the lever that actually drives a narrow layout
    // (CDP `Emulation.setDeviceMetricsOverride` silently no-ops on
    // `window.innerWidth` in headless_chrome 1.0.21). A dedicated browser keeps
    // the shared `launch_browser()` (and the desktop-width DOM the sibling
    // headless tests assert) untouched.
    let browser = launch_browser_sized(Some((375, 812)));
    let tab = browser.new_tab().expect("new tab");

    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // The expected panel populates on the auto-selected test; ALSO switch the
    // left panel to "All inputs" so the wide given table renders there too —
    // both panels populated exercises the stacked path robustly (an
    // empty-panel fixture would not trip the toggle).
    let _ = eval(
        &tab,
        "document.querySelector('.panel-toggle [data-mode=\"inputs\"]').click()",
    );

    // The reflow helper runs inside requestAnimationFrame after each
    // DataTables init and on resize, and is closure-scoped (not a `window.__*`
    // seam), so nudge the bound `resize` listener and poll for `.is-stacked`.
    let _ = eval(&tab, "window.dispatchEvent(new Event('resize'))");
    let mut stacked = false;
    for _ in 0..40 {
        if eval_bool(
            &tab,
            "document.querySelector('.panel-row.is-stacked') !== null",
        ) {
            stacked = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Path guard: the stacked branch MUST have been exercised, else a future
    // CSS regression on `.is-stacked` would silently pass here (this holds on
    // BOTH the broken and fixed CSS — it proves the path, not the fix).
    assert!(
        stacked,
        "the responsive helper collapsed `.panel-row` to `.is-stacked` at 375px \
         (the stacked path must be exercised for this regression to be meaningful)",
    );

    // The effective `window.innerWidth` of the narrow launch window. Headless
    // Chrome does not honour an EXACT CSS width (the `--window-size=375,812`
    // launch lands near 500 in this harness), so we do NOT assert a specific
    // width — `.is-stacked` above already proves the stacked layout path is
    // exercised, which is the real precondition the bug needs, and the
    // assertion below is a relative `scrollWidth <= innerWidth` check that
    // holds at whatever width the harness produces (cross-platform robust).
    let inner_width = eval_i64(&tab, "window.innerWidth");

    // The bug: the stacked track sized to the wide table's min-content and
    // dragged `<body>` past the viewport → a horizontal page scroll. The fix
    // clamps the track so the table scrolls inside its own `.table-fit`
    // wrapper instead. Assert no page-level horizontal overflow (1px
    // tolerance for sub-pixel rounding).
    let scroll_width = eval_i64(&tab, "document.documentElement.scrollWidth");
    assert!(
        scroll_width <= inner_width + 1,
        "no horizontal page overflow at 375px: \
         documentElement.scrollWidth={scroll_width} must be <= innerWidth={inner_width} + 1 \
         (cute-dbt#157 — the stacked `.panel-row` track must not blow out the viewport)",
    );

    // Stronger proof the table is CONTAINED rather than the overflow merely
    // vanishing: the wide given/expected table scrolls INSIDE its `.table-fit`
    // wrapper (`scrollWidth > clientWidth`), which is the intended behaviour
    // the zero-floored track restores.
    assert!(
        eval_bool(
            &tab,
            "Array.from(document.querySelectorAll('.panel-row .table-fit'))\
             .some(function(w){return w.scrollWidth > w.clientWidth + 1;})",
        ),
        "the wide fixture table scrolls inside its `.table-fit` wrapper (containment, not erasure)",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#165 — column-header tooltips ---------------------------

/// A column-scoped generic-test node attached to `model_id` — the
/// manifest shape `column_meta_for_model` resolves (column_name +
/// attached_node + test_metadata). Takes the full [`TestMetadata`] so
/// arg-carrying tests (accepted_values / relationships) can be staged.
fn column_test_node(id: &str, model_id: &str, column: &str, tm: TestMetadata) -> Node {
    Node::new(
        NodeId::new(id),
        "test",
        Checksum::new("sha256", "ck"),
        None,
        None,
        DependsOn::default(),
        None,
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
    .with_test_attachment(
        Some(column.to_owned()),
        Some(NodeId::new(model_id)),
        Some(tm),
    )
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn column_header_tooltips_th_trigger_hover_focus_and_skip_bare_columns() {
    // cute-dbt#165 → cute-dbt#178 (the handoff spec): the WHOLE header cell
    // is the tooltip trigger — NO per-column icon/button in the DOM. dim_x's
    // `id` column is described AND carries unique / not_null /
    // accepted_values / relationships column tests; its `status` column has
    // neither (the no-empty-bubbles negative path). The stg_src given
    // verifies a given table resolves ITS OWN input model's metadata.
    //
    // A11y (#166 carried onto the th): tabindex makes the th keyboard-
    // focusable; aria-label is the AT surface; the singleton #col-tooltip
    // bubble is aria-hidden; hover AND focus both reveal; decorated headers
    // shed the native title (it would double-show over the bubble).
    let mut dim_desc = BTreeMap::new();
    dim_desc.insert("id".to_owned(), "Primary key for dim_x".to_owned());
    let dim = model_node("model.shop.dim_x").with_column_descriptions(dim_desc);
    let mut src_desc = BTreeMap::new();
    src_desc.insert("src_id".to_owned(), "Source key for stg_src".to_owned());
    let src = model_node("model.shop.stg_src").with_column_descriptions(src_desc);

    let ut = UnitTest::new(
        "cols".to_owned(),
        NodeId::new("dim_x"),
        vec![UnitTestGiven::new(
            "ref('stg_src')".to_owned(),
            serde_json::json!([{ "src_id": 1, "bare_col": 2 }]),
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1, "status": "ok" }]),
            Some("dict".to_owned()),
            None,
        ),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    );

    let url = render_to_file(
        "headless_col_tooltips.html",
        vec![
            dim,
            src,
            column_test_node(
                "test.shop.unique_dim_x_id",
                "model.shop.dim_x",
                "id",
                TestMetadata::new("unique", None, serde_json::Value::Null),
            ),
            column_test_node(
                "test.shop.not_null_dim_x_id",
                "model.shop.dim_x",
                "id",
                TestMetadata::new("not_null", None, serde_json::Value::Null),
            ),
            column_test_node(
                "test.shop.accepted_values_dim_x_id",
                "model.shop.dim_x",
                "id",
                TestMetadata::new(
                    "accepted_values",
                    None,
                    serde_json::json!({ "values": ["alpha", "beta"] }),
                ),
            ),
            column_test_node(
                "test.shop.relationships_dim_x_id",
                "model.shop.dim_x",
                "id",
                TestMetadata::new(
                    "relationships",
                    None,
                    serde_json::json!({ "to": "ref('stg_src')", "field": "src_id" }),
                ),
            ),
        ],
        vec![("unit_test.shop.dim_x.cols", ut)],
        &["model.shop.dim_x"],
        &[], // 0 changed → auto-All mode → the test is selected with content
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    select_model(&tab, "dim_x");

    // ===== expected table: exactly one decorated th, on `id` =====
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.expected-panel th.has-col-meta').length"
        )
        .as_u64(),
        Some(1),
        "exactly one decorated header in the expect thead (id yes, status no)",
    );
    // The spec's no-icon contract: NO tooltip trigger element inside any th
    // (the th itself is the trigger), and no legacy info button anywhere.
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('th .col-tooltip, th button, .col-info-btn') === null"
        ),
        "no per-column icon/button — the header cell itself is the trigger",
    );
    const TH: &str = "document.querySelector('.expected-panel th.has-col-meta')";
    assert_eq!(
        eval_string(&tab, &format!("{TH}.getAttribute('data-col-name')")),
        "id",
        "the decorated th is the described+tested column",
    );
    // Keyboard reachability: a bare th is not focusable — tabindex makes it.
    assert_eq!(
        eval_string(&tab, &format!("{TH}.getAttribute('tabindex')")),
        "0",
        "the decorated th carries tabindex=0 (keyboard users can reach the tip)",
    );
    // The native title is REMOVED from decorated headers (it would
    // double-show over the bubble); undecorated headers keep theirs.
    assert!(
        eval_bool(&tab, &format!("{TH}.getAttribute('title') === null")),
        "a decorated th sheds the native title attribute",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.expected-panel th[title=\"status\"]') !== null"
        ),
        "the metadata-less column keeps its plain th (title intact, no decoration)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.expected-panel th[title=\"status\"].has-col-meta') === null"
        ),
        "a column with no description and no tests gets no tooltip affordance",
    );

    // ===== aria parity on the trigger =====
    let aria = eval_string(&tab, &format!("{TH}.getAttribute('aria-label')"));
    assert!(
        aria.contains("Primary key for dim_x")
            && aria.contains("unique")
            && aria.contains("not null"),
        "the th aria-label carries the description + display test names, got {aria:?}",
    );

    // ===== FOCUS reveals the singleton bubble (keyboard path) =====
    assert!(
        eval_bool(
            &tab,
            "(function(){var el=document.getElementById('col-tooltip');\
             return el === null || el.hidden;})()"
        ),
        "the bubble is absent/hidden before any hover/focus",
    );
    let _ = eval(&tab, &format!("{TH}.focus()"));
    const BUBBLE: &str = "document.getElementById('col-tooltip')";
    assert!(
        !eval_bool(&tab, &format!("{BUBBLE}.hidden")),
        "focusing the th reveals the bubble (the keyboard path a native title never had)",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{BUBBLE}.getAttribute('aria-hidden') === 'true'")
        ),
        "the bubble is aria-hidden — the th aria-label is the AT surface",
    );
    // Spec content: .ct-desc description, accent .ct-key test names,
    // .ct-val chips for accepted_values args, .ct-detail for relationships.
    assert_eq!(
        eval_string(
            &tab,
            &format!("{BUBBLE}.querySelector('.ct-desc').textContent")
        ),
        "Primary key for dim_x",
        "the bubble leads with the authored description",
    );
    let keys = eval_string(
        &tab,
        &format!(
            "Array.from({BUBBLE}.querySelectorAll('.ct-key'))\
             .map(function(k){{return k.textContent;}}).join('|')"
        ),
    );
    assert_eq!(
        keys, "accepted values|not null|relationships|unique",
        "every column test renders a .ct-key name (sorted by display name)",
    );
    let chips = eval_string(
        &tab,
        &format!(
            "Array.from({BUBBLE}.querySelectorAll('.ct-vals .ct-val'))\
             .map(function(v){{return v.textContent;}}).join('|')"
        ),
    );
    assert_eq!(
        chips, "alpha|beta",
        "accepted_values args render as distinct .ct-val chips",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{BUBBLE}.querySelector('.ct-detail').textContent")
        ),
        "stg_src.src_id",
        "relationships renders its model.field detail in muted mono",
    );

    // Blur hides it again (focusout path).
    let _ = eval(&tab, &format!("{TH}.blur()"));
    assert!(
        eval_bool(&tab, &format!("{BUBBLE}.hidden")),
        "blurring the th hides the bubble",
    );

    // ===== HOVER reveals too (mouse path; delegated mouseenter) =====
    let _ = eval(
        &tab,
        &format!("{TH}.dispatchEvent(new MouseEvent('mouseover', {{bubbles: true}}))"),
    );
    assert!(
        !eval_bool(&tab, &format!("{BUBBLE}.hidden")),
        "hovering the th reveals the bubble (mouse-only users reach it)",
    );
    let _ = eval(
        &tab,
        &format!("{TH}.dispatchEvent(new MouseEvent('mouseout', {{bubbles: true}}))"),
    );
    assert!(
        eval_bool(&tab, &format!("{BUBBLE}.hidden")),
        "leaving the th hides the bubble",
    );

    // ===== given table: the INPUT model's metadata, filtered the same =====
    show_all_inputs(&tab);
    const GIVEN_TH: &str = "document.querySelector('.given-section th.has-col-meta')";
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.given-section th.has-col-meta').length"
        )
        .as_u64(),
        Some(1),
        "exactly one decorated header in the given thead (src_id yes, bare_col no)",
    );
    assert_eq!(
        eval_string(&tab, &format!("{GIVEN_TH}.getAttribute('data-col-name')")),
        "src_id",
        "the given trigger rides the input model's described column",
    );
    let _ = eval(&tab, &format!("{GIVEN_TH}.focus()"));
    assert_eq!(
        eval_string(
            &tab,
            &format!("{BUBBLE}.querySelector('.ct-desc').textContent")
        ),
        "Source key for stg_src",
        "the given bubble carries the INPUT model's description",
    );

    let _ = tab.close(true);
}

// ===== cute-dbt#180 — Mermaid <-> Cytoscape engine picker ============
//
// These fixtures reuse `model_node_with_compiled` (the #155 helper above):
// the compiled body below carries two parallel branches, so tapping one
// branch's import must dim the OTHER branch (the lineage-complement
// assertion needs nodes outside the tapped lineage).

/// Two independent branches joining into `final_join`: tapping `src_a`
/// leaves `src_b` + `branch_b` outside the lineage.
const TWO_BRANCH_SQL: &str = "with src_a as (select 1 as id), \
     src_b as (select 1 as id), \
     branch_a as (select id from src_a), \
     branch_b as (select id from src_b), \
     final_join as (select branch_a.id from branch_a inner join branch_b on branch_a.id = branch_b.id) \
     select id from final_join";

/// Render a baseline report whose single model carries the two-branch DAG
/// and navigate a fresh tab to it.
fn dag_engine_fixture_tab(browser: &Browser, filename: &str) -> std::sync::Arc<Tab> {
    let id = "unit_test.shop.dim_a.t";
    let url = render_to_file(
        filename,
        vec![model_node_with_compiled("model.shop.dim_a", TWO_BRANCH_SQL)],
        vec![(id, unit_test("t", "dim_a"))],
        &["model.shop.dim_a"],
        &[id],
    );
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    tab
}

/// Open the settings cog and click the engine-picker segment button for
/// `engine` ("mermaid" | "cytoscape").
fn click_engine(tab: &Tab, engine: &str) {
    let _ = eval(tab, "document.querySelector('.settings-cog').click()");
    let _ = eval(
        tab,
        &format!("document.querySelector('.engine-seg button[data-engine=\"{engine}\"]').click()"),
    );
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn dag_engine_picker_switches_engines_in_place_and_persists() {
    // The cute-dbt#180 picker contract: Mermaid renders by default (the
    // static default engine), the Cytoscape segment flips the DAG to a live
    // cy canvas IN PLACE (no page reload — a window sentinel survives), the
    // choice persists with the appearance state under
    // cute-dbt.appearance.v1, and flipping back restores the Mermaid SVG
    // and destroys the cy instance.
    let browser = launch_browser();
    let tab = dag_engine_fixture_tab(&browser, "headless_engine_picker.html");

    // Boot: Mermaid is the default-rendered engine; the Cytoscape host is
    // hidden and empty.
    tab.wait_for_element_with_custom_timeout(
        ".cte-dag-mermaid svg",
        std::time::Duration::from_secs(15),
    )
    .expect("Mermaid renders the DAG by default");
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.cte-dag-cyto').classList.contains('is-hidden')"
        ),
        "the Cytoscape host starts hidden (Mermaid is the static default)",
    );
    assert!(
        eval_bool(&tab, "window.CuteCyto.cyInstance() === null"),
        "no cy instance exists while Mermaid is active",
    );

    // The in-place proof: a sentinel set before the flip must survive it
    // (a reload would wipe window state).
    let _ = eval(&tab, "window.__cuteEngineFlipSentinel = 42");
    click_engine(&tab, "cytoscape");
    tab.wait_for_element_with_custom_timeout(
        ".cte-dag-cyto canvas",
        std::time::Duration::from_secs(15),
    )
    .expect("the Cytoscape engine renders a live canvas after the flip");
    assert_eq!(
        eval(&tab, "window.__cuteEngineFlipSentinel"),
        serde_json::json!(42),
        "the engine swap happens in place — no page reload",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.cte-dag-mermaid').classList.contains('is-hidden') \
             && document.querySelector('.cte-dag-mermaid').childElementCount === 0"
        ),
        "the Mermaid host is hidden and emptied while Cytoscape is active",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('.cte-dag-cyto').classList.contains('is-hidden')"
        ),
        "the Cytoscape host is revealed",
    );
    assert_eq!(
        eval_i64(&tab, "window.CuteCyto.cyInstance().nodes().length"),
        6,
        "the cy graph carries the five CTE nodes + the terminal",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.engine-seg button[data-engine=\"cytoscape\"]').getAttribute('aria-pressed')"
        ),
        "true",
        "the active engine segment reports aria-pressed=true",
    );

    // Persistence rides the appearance blob (where storage is usable).
    let storage_ok = eval_bool(
        &tab,
        "(function(){try{if(!window.localStorage)return false;\
           window.localStorage.setItem('__probe','1');\
           window.localStorage.removeItem('__probe');return true;}\
           catch(e){return false;}})()",
    );
    if storage_ok {
        let raw = eval_string(
            &tab,
            "window.localStorage.getItem('cute-dbt.appearance.v1') || ''",
        );
        assert!(
            raw.contains("\"engine\":\"cytoscape\""),
            "the engine choice persisted under cute-dbt.appearance.v1: {raw}",
        );
    }

    // Flip back: Mermaid SVG returns, the cy instance is destroyed.
    click_engine(&tab, "mermaid");
    tab.wait_for_element_with_custom_timeout(
        ".cte-dag-mermaid svg",
        std::time::Duration::from_secs(15),
    )
    .expect("flipping back re-renders the Mermaid SVG");
    assert!(
        eval_bool(&tab, "window.CuteCyto.cyInstance() === null"),
        "flipping back destroys the cy instance",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.cte-dag-cyto').classList.contains('is-hidden')"
        ),
        "the Cytoscape host hides again",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn cytoscape_hover_card_appears_and_tap_highlights_lineage_in_place() {
    // The cute-dbt#180 interaction set, exercised through the handlers
    // cyto-dag.js binds (emit() fires the real bound listeners):
    //  - hover a node -> the .cyto-node-card context card appears, filled
    //    via textContent;
    //  - tap a node -> its full lineage highlights and the complement dims
    //    IN PLACE (the same cy instance survives — the spike's
    //    no-renderDag-per-click rule), and the Inspect panel selects the
    //    node through __cuteSelectNode;
    //  - background tap clears the highlight.
    let browser = launch_browser();
    let tab = dag_engine_fixture_tab(&browser, "headless_cyto_interactions.html");
    click_engine(&tab, "cytoscape");
    tab.wait_for_element_with_custom_timeout(
        ".cte-dag-cyto canvas",
        std::time::Duration::from_secs(15),
    )
    .expect("the Cytoscape engine renders");

    // ===== hover -> context card =====
    let _ = eval(
        &tab,
        "void window.CuteCyto.cyInstance().getElementById('src_a').emit('mouseover')",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.cyto-node-card').classList.contains('is-visible')"
        ),
        "hovering a node reveals the context card",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.cyto-node-card .nc-name').textContent"
        ),
        "src_a",
        "the card names the hovered node (textContent-filled)",
    );
    let _ = eval(
        &tab,
        "void window.CuteCyto.cyInstance().getElementById('src_a').emit('mouseout')",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('.cyto-node-card').classList.contains('is-visible')"
        ),
        "leaving the node hides the context card",
    );

    // ===== tap -> lineage highlight + dim-complement, in place =====
    let _ = eval(
        &tab,
        "void (window.__cuteCyRef = window.CuteCyto.cyInstance())",
    );
    let _ = eval(
        &tab,
        "void window.CuteCyto.cyInstance().getElementById('src_a').emit('tap')",
    );
    assert!(
        eval_bool(
            &tab,
            "window.CuteCyto.cyInstance().getElementById('src_a').hasClass('sel')"
        ),
        "the tapped node carries the selected ring",
    );
    assert_eq!(
        eval_i64(&tab, "window.CuteCyto.cyInstance().nodes('.dim').length"),
        2,
        "exactly the other branch (src_b + branch_b) dims — the lineage \
         (src_a -> branch_a -> final_join -> terminal) stays lit",
    );
    assert!(
        eval_bool(
            &tab,
            "window.CuteCyto.cyInstance().getElementById('branch_b').hasClass('dim') \
             && window.CuteCyto.cyInstance().getElementById('src_b').hasClass('dim')"
        ),
        "both non-lineage nodes are the dimmed ones",
    );
    assert!(
        !eval_bool(
            &tab,
            "window.CuteCyto.cyInstance().getElementById('branch_a').hasClass('dim')"
        ),
        "downstream lineage stays undimmed",
    );
    assert!(
        eval_bool(&tab, "window.CuteCyto.cyInstance() === window.__cuteCyRef"),
        "the tap mutates classes on the SAME cy instance — no rebuild, \
         pan/zoom state survives (the no-renderDag-per-click rule)",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.left-panel-body .node-detail').getAttribute('data-node-id')"
        ),
        "src_a",
        "the tap drives the Inspect panel through __cuteSelectNode",
    );

    // ===== background tap clears =====
    let _ = eval(&tab, "void window.CuteCyto.cyInstance().emit('tap')");
    assert_eq!(
        eval_i64(
            &tab,
            "window.CuteCyto.cyInstance().elements('.dim, .sel, .trace').length"
        ),
        0,
        "a background tap clears the highlight classes",
    );

    let _ = tab.close(true);
}

// ===== cute-dbt#170 — the per-model coverage-checks panel ==============
//
// The findings surface is client-side JS over the payload's
// `m.findings` (FindingPayloads) + `check_specs` catalog, so a real
// browser is the only honest verification: panel presence in both
// scope-toggle views, the three-valued checklist, tier-chip
// distinctness + the hover/focus tooltip contract, the rationale
// drawer (offline) + click-only book link, the copyable YAML sketch,
// evidence pinning, and the visible-but-quiet suppressed reveal.

/// A model tripping BOTH registered checks: `config.unique_key` with no
/// backing uniqueness test (grain, TOTAL, UNCOVERED) and a UNION whose
/// arms no unit-test given feeds (union, HIGH, UNCOVERED + sketches).
fn findings_model(full_id: &str) -> Node {
    let mut config = BTreeMap::new();
    config.insert("unique_key".to_owned(), serde_json::json!("k"));
    let compiled = "with arm_a as (select * from src_a), \
                    arm_b as (select * from src_b), \
                    unioned as (select * from arm_a union all select * from arm_b) \
                    select * from unioned";
    Node::new(
        NodeId::new(full_id),
        "model",
        Checksum::new("sha256", "ck"),
        Some(compiled.to_owned()),
        None,
        DependsOn::default(),
        None,
        NodeConfig::new(config, false),
        None,
        BTreeMap::new(),
    )
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn findings_panel_renders_checklist_tiers_sketch_rationale_and_pin() {
    // dim_both trips grain (TOTAL, uncovered) + union (HIGH, uncovered
    // with sketches); dim_quiet trips nothing (the deliberate quiet
    // empty state) and has zero tests (the panel must render without a
    // selected test).
    let url = render_to_file(
        "headless_findings_panel.html",
        vec![
            findings_model("model.shop.dim_both"),
            model_node("model.shop.dim_quiet"),
        ],
        vec![("unit_test.shop.dim_both.t1", unit_test("t1", "dim_both"))],
        &["model.shop.dim_both", "model.shop.dim_quiet"],
        &["unit_test.shop.dim_both.t1"],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // --- panel + three-valued checklist ------------------------------
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('[data-testid=\"model-findings\"]') !== null",
        ),
        "the coverage-checks panel section renders",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.findings-checklist > .finding-row').length",
        ),
        2,
        "one checklist row per (construct, check) finding",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelectorAll('.finding-row.verdict-uncovered .f-verdict').length === 2",
        ),
        "each row carries its verdict WORD (mark is never colour-only)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-findings').textContent.indexOf('%') === -1",
        ),
        "coverage is a checklist — no percentage anywhere in the panel",
    );

    // --- tier chips: labeled + visually distinct ----------------------
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.tier-chip.tier-total') !== null \
             && document.querySelector('.tier-chip.tier-high') !== null",
        ),
        "both tier chips render, labeled by tier",
    );
    assert!(
        eval_bool(
            &tab,
            "(function(){\
               var a = getComputedStyle(document.querySelector('.tier-chip.tier-total'));\
               var b = getComputedStyle(document.querySelector('.tier-chip.tier-high'));\
               return a.backgroundColor !== b.backgroundColor \
                   || a.borderTopColor !== b.borderTopColor;\
             })()",
        ),
        "TOTAL and HIGH chips are visually distinct (never blended)",
    );
    // The #146/#188 tooltip contract: focusable trigger, aria-label for
    // AT, NO native title; keyboard focus reveals the bubble.
    assert!(
        eval_bool(
            &tab,
            "(function(){\
               var c = document.querySelector('.tier-chip.tier-total');\
               return c.getAttribute('tabindex') === '0' \
                   && !c.hasAttribute('title') \
                   && (c.getAttribute('aria-label') || '').indexOf('TOTAL tier') === 0;\
             })()",
        ),
        "tier chip is a focusable, aria-labelled trigger with no native title",
    );
    assert!(
        eval_bool(
            &tab,
            "(function(){\
               document.querySelector('.tier-chip.tier-total').focus();\
               var t = document.getElementById('col-tooltip');\
               return !!t && !t.hidden && t.textContent.indexOf('TOTAL tier') === 0;\
             })()",
        ),
        "keyboard focus reveals the tier tooltip bubble",
    );

    // --- recommendation + copyable YAML sketch ------------------------
    assert!(
        eval_bool(
            &tab,
            "document.querySelectorAll('.finding-sketch .sql-copy').length >= 1",
        ),
        "the union sketch renders with the #188 copy button",
    );
    assert!(
        eval_bool(
            &tab,
            // textContent includes the numbered gutter (the #178 code-line
            // DOM), so `contains`, not a position-0 anchor.
            "document.querySelector('.finding-sketch .sql-block').textContent\
             .indexOf(\"- input: ref('\") >= 0",
        ),
        "the sketch code block carries the copy-pasteable given-row YAML",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelectorAll('.finding-recommendation').length === 2",
        ),
        "each uncovered finding carries its recommendation copy",
    );

    // --- rationale drawer (offline) + click-only book link ------------
    assert!(
        eval_bool(
            &tab,
            "(function(){\
               var d = document.querySelector('.finding-rationale');\
               if (!d) return false;\
               d.open = true;\
               var body = d.querySelector('.finding-rationale-body');\
               return !!body && body.textContent.length > 40;\
             })()",
        ),
        "the rationale drawer opens with embedded (offline) check prose",
    );
    assert!(
        eval_bool(
            &tab,
            "(function(){\
               var a = document.querySelector('.finding-rationale-body a');\
               return !!a \
                   && a.getAttribute('href').indexOf('checks/') > 0 \
                   && a.getAttribute('target') === '_blank' \
                   && a.getAttribute('rel') === 'noopener noreferrer';\
             })()",
        ),
        "the book reference is a plain click-only anchor (no fetch-on-load)",
    );

    // --- evidence pinning ---------------------------------------------
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.finding-pin[data-pin=\"unioned\"]') !== null",
        ),
        "the union finding pins the consumer CTE node",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.finding-pin[data-pin=\"(final select)\"]') !== null",
        ),
        "the model-level grain finding pins the terminal node",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.finding-pin[data-pin=\"unioned\"]').click()",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.left-panel-body .node-detail').getAttribute('data-node-id')",
        ),
        "unioned",
        "the pin selects the cited construct (Inspect follows)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.cte-dag').classList.contains('pin-flash')",
        ),
        "the pin flashes the DAG section (the scroll target exists)",
    );

    // --- visible in BOTH scope-toggle views (the #91 contract) ---------
    click_mode(&tab, "all");
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.findings-checklist > .finding-row').length",
        ),
        2,
        "the panel persists across the Updated-only ↔ All-tests flip",
    );
    click_mode(&tab, "updated");

    // --- pin under the Cytoscape engine: in-place, never a rebuild -----
    // (the cute-dbt#180 per-click contract; the pin emits a tap on the
    // SAME cy instance instead of re-calling renderDag).
    click_engine(&tab, "cytoscape");
    tab.wait_for_element_with_custom_timeout(
        ".cte-dag-cyto canvas",
        std::time::Duration::from_secs(15),
    )
    .expect("the Cytoscape engine renders after the flip");
    let _ = eval(
        &tab,
        "void (window.__cutePinCyRef = window.CuteCyto.cyInstance())",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.finding-pin[data-pin=\"(final select)\"]').click()",
    );
    assert!(
        eval_bool(
            &tab,
            "window.CuteCyto.cyInstance() === window.__cutePinCyRef",
        ),
        "a pin click in Cytoscape mode mutates the SAME cy instance (no rebuild)",
    );
    assert!(
        eval_bool(
            &tab,
            "window.CuteCyto.cyInstance().getElementById('(final select)').hasClass('sel')",
        ),
        "the pin selects the cited node in place",
    );
    click_engine(&tab, "mermaid");

    // --- quiet empty state on a findings-free, test-free model ---------
    select_model(&tab, "dim_quiet");
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('[data-testid=\"findings-empty\"]') !== null",
        ),
        "a model with no findings renders the quiet empty line (never a hidden panel)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.findings-checklist') === null",
        ),
        "no checklist renders on a findings-free model",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn incremental_branch_gap_renders_in_the_findings_panel() {
    // cute-dbt#164 — the incremental.branch-coverage check surfaces
    // through the generic findings panel (cute-dbt#194 renders ALL
    // registered checks; no new affordance). dim_inc_gap is materialized
    // incremental with ONE no-override unit test — the false-only
    // rollup: the incremental branch is the gap, and the row must carry
    // the verdict word, the HIGH tier chip, the recommendation cue, and
    // the copyable sketch with the true override.
    let url = render_to_file(
        "headless_incremental_branch_finding.html",
        vec![model_node_materialized(
            "model.shop.dim_inc_gap",
            "incremental",
        )],
        vec![(
            "unit_test.shop.dim_inc_gap.t_full",
            incremental_unit_test("t_full", "dim_inc_gap", None, &[]),
        )],
        &["model.shop.dim_inc_gap"],
        &["unit_test.shop.dim_inc_gap.t_full"],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    const ROW: &str =
        "document.querySelector('.finding-row[data-check=\"incremental.branch-coverage\"]')";
    assert!(
        eval_bool(&tab, &format!("{ROW} !== null")),
        "the incremental.branch-coverage finding renders a checklist row",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{ROW}.classList.contains('verdict-uncovered')"),
        ),
        "the false-only gap renders as UNCOVERED",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{ROW}.querySelector('.f-verdict').textContent")
        ),
        "uncovered",
        "the row carries its verdict WORD (mark is never colour-only)",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{ROW}.querySelector('.tier-chip.tier-high') !== null"),
        ),
        "the check is labeled HIGH tier (a cue, never an assertion)",
    );
    assert!(
        eval_bool(
            &tab,
            &format!(
                "{ROW}.querySelector('.finding-recommendation').textContent\
                 .indexOf('is_incremental') >= 0",
            ),
        ),
        "the recommendation cue names the is_incremental override",
    );
    assert!(
        eval_bool(
            &tab,
            &format!(
                "{ROW}.querySelector('.finding-sketch .sql-block').textContent\
                 .indexOf('is_incremental: true') >= 0",
            ),
        ),
        "the copyable sketch carries the missing-branch true override",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{ROW}.textContent.indexOf('false-only') >= 0",),
        ),
        "the branch-coverage rollup evidence renders in the row",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn suppressed_findings_render_as_a_collapsed_count_with_reasons() {
    // The cute-dbt#171 policy marks (never removes) suppressed findings;
    // the surface renders them visible-but-quiet: a collapsed count that
    // reveals the acknowledged rows WITH their reasons.
    use cute_dbt::domain::{CheckPolicy, HeuristicId, SuppressRule, SuppressionSource};

    let node = findings_model("model.shop.dim_both");
    let m = manifest(
        vec![node],
        vec![("unit_test.shop.dim_both.t1", unit_test("t1", "dim_both"))],
    );
    let in_scope: InScopeSet = ["unit_test.shop.dim_both.t1".to_owned()]
        .into_iter()
        .collect();
    let models: ModelInScopeSet = [NodeId::new("model.shop.dim_both")].into_iter().collect();
    let changed: InScopeSet = ["unit_test.shop.dim_both.t1".to_owned()]
        .into_iter()
        .collect();
    let policy = CheckPolicy::<HeuristicId> {
        suppressions: vec![SuppressRule {
            check: HeuristicId::GrainUniqueKeyUnbacked,
            model: "dim_both".to_owned(),
            reason: Some("duplicate grain accepted during backfill".to_owned()),
            source: SuppressionSource::Config,
        }],
        ..Default::default()
    };
    let out = tmp("headless_findings_suppressed.html");
    let _ = std::fs::remove_file(&out);
    render_report_with_externals(
        &out,
        &m,
        &in_scope,
        &models,
        &changed,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "baseline.json",
        ScopeSource::Baseline,
        DEFAULT_REPORT_TITLE,
        None,
        &policy,
    )
    .expect("render writes the report");
    let url = format!("file://{}", out.to_str().expect("UTF-8 path"));

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    assert_eq!(
        eval_i64(&tab, "document.querySelectorAll('.finding-row').length",),
        2,
        "suppression never removes a finding from the surface",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.findings-checklist > .finding-row:not(.is-suppressed)').length",
        ),
        1,
        "only the unsuppressed finding sits in the main checklist",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('[data-testid=\"findings-suppressed\"] > summary')\
             .textContent.trim()",
        ),
        "1 suppressed finding",
        "the suppressed count renders collapsed (visible-but-quiet)",
    );
    assert!(
        eval_bool(
            &tab,
            "!document.querySelector('[data-testid=\"findings-suppressed\"]').open",
        ),
        "the suppressed reveal starts collapsed",
    );
    assert!(
        eval_bool(
            &tab,
            "(function(){\
               var d = document.querySelector('[data-testid=\"findings-suppressed\"]');\
               d.open = true;\
               var row = d.querySelector('.finding-row.is-suppressed');\
               if (!row) return false;\
               row.querySelector('.finding-details').open = true;\
               var reason = row.querySelector('.finding-suppress-reason');\
               return !!reason \
                   && reason.textContent.indexOf('duplicate grain accepted during backfill') > 0 \
                   && row.querySelector('.suppress-chip').textContent === 'suppressed · config';\
             })()",
        ),
        "opening the reveal shows the acknowledged row with its reason + source",
    );

    let _ = tab.close(true);
}
