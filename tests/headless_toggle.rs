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
    BlockDiff, Checksum, DEFAULT_REPORT_TITLE, DependsOn, DiffLine, DiffLineKind, FileHunks,
    HookChangeFacts, HookManifestPresence, Hunk, InScopeSet, Manifest, ManifestMetadata,
    ModelInScopeSet, Node, NodeConfig, NodeId, NormalizedDiffIndex, PrDiff, ProjectChange,
    ProjectChangeCategory, ProjectChangePanel, ProjectFacts, ProjectFallbackReason, SourceNode,
    TestMetadata, UnitTest, UnitTestDataDiff, UnitTestExpect, UnitTestGiven, UnitTestYamlBlock,
    external_fixture_table, raw_hunk_lines, reconstruct_table_diffs,
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

    // cute-dbt#247 — the drawer shows the SELECTED UNIT TEST's authored
    // YAML, so it is labeled "Unit test YAML" (supersedes the #233 D7
    // "Model YAML" label, which was a spec naming error — that name now
    // belongs to the model-level schema-entry section). Still no
    // diff-variant suffix: the Diff/File toggle in the code header
    // carries the diff affordance. The Diff view stays the default
    // (Authored hidden).
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.authoring-yaml > summary').textContent.trim()"
        ),
        "Unit test YAML",
        "the drawer summary is the truthful 'Unit test YAML' label (cute-dbt#247)",
    );
    assert!(
        eval_bool(&tab, "document.querySelector('.yaml-diff-toggle') !== null"),
        "the Authored↔Diff toggle is present",
    );
    // cute-dbt#199 — the drawer's code header gains the per-diff fold toggle
    // and the inline-SVG copy-icon button (the Model-YAML copy affordance).
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.authoring-yaml .code-header .diff-fold-toggle') !== null"
        ),
        "the Model-YAML diff header carries the per-diff fold toggle",
    );
    assert!(
        eval_bool(
            &tab,
            "(function(){var b=document.querySelector('.authoring-yaml .code-header .code-copy-btn');\
               return !!b && b.tagName==='BUTTON' && b.getAttribute('aria-label')==='Copy'\
                 && !!b.querySelector('svg.icon');})()"
        ),
        "the Model-YAML header carries the inline-SVG copy-icon button (aria-label Copy)",
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

    // (3) Same on the given side — the Given panel always renders every
    // input (cute-dbt#201 retired the Inspect/All-inputs toggle); confirm
    // it renders Current-only (no cell diff).
    assert!(
        eval_bool(&tab, "document.querySelector('.given-table') !== null"),
        "the given fixture's Current grid renders in the Given panel",
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
    // cute-dbt#132 / cute-dbt#199 — hunk contraction under the directional
    // step-expansion model (supersedes the #136 click-toggles-all + "Hide N"
    // relabel). Drives the `__cuteRenderBlockDiff` JS seam with synthetic
    // diffs (no manifest content needed) to verify:
    //  (1) a long middle context run folds: a `.diff-fold` control with the
    //      correct hidden count, a `data-fold-dir`, the gutter +/− steppers,
    //      a (hidden-until-revealed) collapse-all + `.diff-folded[hidden]`
    //      lines;
    //  (2) a SHORT block (change + 2 context) renders NO fold (small YAML
    //      test blocks must never fold);
    //  (3) activating the band (click AND keyboard) expands BY STEP — with
    //      the default step (20) ≥ the hidden count (4) one activation still
    //      reveals all, the control stays visible relabeled "All N lines
    //      shown" (the "Hide N" relabel assertion is consciously superseded),
    //      and its collapse-all restores the folded state. Reveal stays
    //      PARENT-SCOPED: a second block with the same local `fold-0` id is
    //      untouched.
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
    // cute-dbt#199 — the new control anatomy: a reveal direction (this middle
    // run has a hunk above, so it expands DOWN), the gutter steppers, and the
    // initially-hidden per-hunk collapse-all.
    assert!(
        folded_html.contains("data-fold-dir=\"down\""),
        "a middle run (hunk above) carries data-fold-dir=down: {folded_html}",
    );
    assert!(
        folded_html.contains("fold-steppers")
            && folded_html.contains("class=\"fold-expand\"")
            && folded_html.contains("class=\"fold-contract\""),
        "the gutter carries the +/− fold steppers: {folded_html}",
    );
    assert!(
        folded_html.contains("fold-collapse-all"),
        "the per-hunk collapse-all affordance is emitted: {folded_html}",
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

    // CLICK the band in block A: activate = expand-by-step (#199); the
    // default step (20) ≥ the 4 hidden lines, so ONE activation reveals all.
    // The control STAYS visible (the #136 invariant), relabeled "All N lines
    // shown" (supersedes the old "Hide N" relabel), its + stepper disables,
    // and the collapse-all affordance appears. B is untouched (still 4
    // hidden) — the reveal is parent-scoped.
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
        "clicking block A's band reveals its folded lines (step 20 >= hidden 4)",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('#fold-block-a .diff-fold').hidden"
        ),
        "the activated control STAYS visible (#136 invariant under #199)",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#fold-block-a .diff-fold').getAttribute('aria-expanded')"
        ),
        "true",
        "the fully-expanded control reports aria-expanded=true",
    );
    assert!(
        eval_string(
            &tab,
            "document.querySelector('#fold-block-a .diff-fold-label').textContent"
        )
        .contains("All 4 lines shown"),
        "the fully-expanded control relabels to 'All N lines shown' (#199)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('#fold-block-a .fold-expand').disabled"
        ),
        "the + stepper disables once everything is revealed",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('#fold-block-a .fold-collapse-all').hidden"
        ),
        "the per-hunk collapse-all appears once anything is revealed",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#fold-block-b .diff-folded[hidden]').length"
        ),
        4,
        "block B stays folded — the reveal is PARENT-SCOPED despite the shared fold-0 id",
    );

    // CLICK A's collapse-all: the hunk re-collapses (4 hidden) and the
    // control relabels to Show — the #199 restore path (the band itself only
    // expands now; re-collapse moved to the explicit affordance).
    let _ = eval(
        &tab,
        "document.querySelector('#fold-block-a .fold-collapse-all').click()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#fold-block-a .diff-folded[hidden]').length"
        ),
        4,
        "the collapse-all re-collapses A's folded lines",
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
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('#fold-block-a .fold-collapse-all').hidden"
        ),
        "the collapse-all hides again once nothing is revealed",
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

    // cute-dbt#199 — the diff code header carries the per-diff fold toggle
    // (replacing the top-of-report strip) and the copy-icon button
    // (replacing the absolutely-positioned text Copy). Both are REAL
    // focusable <button>s with an accessible name (the #146 rule).
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql .code-header .diff-fold-toggle').textContent.trim()\
             + '|' + document.querySelector('.model-sql .code-header .diff-fold-toggle').getAttribute('aria-pressed')"
        ),
        "Expand all|false",
        "the model-SQL diff header carries the per-diff fold toggle, unpressed",
    );
    assert!(
        eval_bool(
            &tab,
            "(function(){var b=document.querySelector('.model-sql .code-header .code-copy-btn');\
               return !!b && b.tagName==='BUTTON' && b.getAttribute('aria-label')==='Copy'\
                 && !!b.querySelector('svg.icon');})()"
        ),
        "the model-SQL header carries the inline-SVG copy-icon button (aria-label Copy)",
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
fn per_diff_fold_toggle_drives_its_own_diffs_folds() {
    // cute-dbt#199 — the per-diff Expand/Collapse-all button in a diff's code
    // header drives THAT diff's folds through the same setAllFolds mirror the
    // __cute hooks use: click => every fold in this diff reveals, the button
    // relabels Collapse-all + aria-pressed=true; click again => exact
    // restore. A long-context model-SQL diff guarantees a real fold exists.
    let mut lines = vec![
        dl(DiffLineKind::Removed, "select a", Some((7, 8))),
        dl(DiffLineKind::Added, "select b", Some((7, 8))),
    ];
    for i in 0..10 {
        lines.push(dl(DiffLineKind::Context, &format!("ctx{i}"), None));
    }
    lines.push(dl(DiffLineKind::Removed, "from t", Some((5, 6))));
    lines.push(dl(DiffLineKind::Added, "from u", Some((5, 6))));
    let url = render_pr_diff_with_sql_diffs(
        "headless_per_diff_fold_toggle.html",
        vec![model_node_with_raw("model.shop.dim_a", "select b\nfrom u")],
        vec![("unit_test.shop.dim_a.t", unit_test("t", "dim_a"))],
        &["model.shop.dim_a"],
        vec![("model.shop.dim_a", BlockDiff { lines })],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // The 10-line context run folds (pad 3 => 4 hidden) inside the unified
    // view of the model-SQL diff.
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.model-sql .sql-diff-view .diff-unified .diff-folded[hidden]').length"
        ),
        4,
        "the model-SQL diff renders default-folded (4 hidden)",
    );

    // Click the header's fold toggle: every fold in THIS diff (both the
    // unified <code> and the split <tbody>) reveals; the button flips.
    let _ = eval(
        &tab,
        "document.querySelector('.model-sql .code-header .diff-fold-toggle').click()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.model-sql .sql-diff-view .diff-folded[hidden]').length"
        ),
        0,
        "the per-diff toggle expands every fold in its diff (unified AND split)",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql .code-header .diff-fold-toggle').textContent.trim()\
             + '|' + document.querySelector('.model-sql .code-header .diff-fold-toggle').getAttribute('aria-pressed')"
        ),
        "Collapse all|true",
        "the pressed toggle relabels to Collapse all",
    );

    // Click again: exact restore (the symmetric DOM mirror).
    let _ = eval(
        &tab,
        "document.querySelector('.model-sql .code-header .diff-fold-toggle').click()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.model-sql .sql-diff-view .diff-unified .diff-folded[hidden]').length"
        ),
        4,
        "the toggle restores the default-folded state exactly",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql .code-header .diff-fold-toggle').getAttribute('aria-pressed')"
        ),
        "false",
        "the released toggle reports aria-pressed=false",
    );

    // --- Gemini PR #213 — the toggle's state is DERIVED, never cached -----
    // (a) a global fold op through the __cute hooks keeps the per-diff
    // toggle truthful (label + aria-pressed flip without the button being
    // clicked).
    let _ = eval(&tab, "window.__cuteExpandAllFolds(document)");
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql .code-header .diff-fold-toggle').textContent.trim()\
             + '|' + document.querySelector('.model-sql .code-header .diff-fold-toggle').getAttribute('aria-pressed')"
        ),
        "Collapse all|true",
        "global expand-all flips the per-diff toggle's label + aria-pressed",
    );
    // ...and a click NOW acts on the DOM truth: nothing is hidden, so the
    // click COLLAPSES (a cached boolean would have 'expanded' a no-op and
    // relabeled into a lie).
    let _ = eval(
        &tab,
        "document.querySelector('.model-sql .code-header .diff-fold-toggle').click()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.model-sql .sql-diff-view .diff-unified .diff-folded[hidden]').length"
        ),
        4,
        "a toggle click after a global expand acts on DOM truth and collapses",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql .code-header .diff-fold-toggle').textContent.trim()\
             + '|' + document.querySelector('.model-sql .code-header .diff-fold-toggle').getAttribute('aria-pressed')"
        ),
        "Expand all|false",
        "the toggle relabels from the resulting DOM state after the collapse",
    );

    // (b) stepping a fold to fully-revealed keeps the toggle truthful: the
    // unified band-click (step 20) reveals the unified fold, but the diff as
    // a whole still holds hidden rows (the split twin), so the toggle
    // truthfully stays unpressed; clicking it then expands the remainder.
    let _ = eval(
        &tab,
        "document.querySelector('.model-sql .sql-diff-view .diff-unified .diff-fold').click()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.model-sql .sql-diff-view .diff-unified .diff-folded[hidden]').length"
        ),
        0,
        "the unified band-click fully reveals the unified fold",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql .code-header .diff-fold-toggle').textContent.trim()\
             + '|' + document.querySelector('.model-sql .code-header .diff-fold-toggle').getAttribute('aria-pressed')"
        ),
        "Expand all|false",
        "the toggle stays truthful while the split twin still holds hidden rows",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.model-sql .code-header .diff-fold-toggle').click()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.model-sql .sql-diff-view .diff-folded[hidden]').length"
        ),
        0,
        "the toggle click expands the remaining (split) folds from DOM-derived state",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-sql .code-header .diff-fold-toggle').textContent.trim()\
             + '|' + document.querySelector('.model-sql .code-header .diff-fold-toggle').getAttribute('aria-pressed')"
        ),
        "Collapse all|true",
        "the fully-revealed diff presses the toggle",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn copy_icon_button_signals_failure_truthfully() {
    // Gemini PR #213 — a failed copy must NEVER flash "Copied". Both write
    // paths are stubbed to fail in-page (writeText rejects; execCommand
    // returns false), which deterministically drives the copy-failed branch:
    // the button gains `.copy-failed` with title/aria-label "Copy failed"
    // (asserted inside the 1.2s flash window via a bounded poll), never
    // gains `.copied`, and resets to the rest "Copy" state afterwards.
    let url = render_pr_diff_with_sql_diffs(
        "headless_copy_failed.html",
        vec![model_node_with_raw("model.shop.dim_a", "select id\nfrom t")],
        vec![("unit_test.shop.dim_a.t", unit_test("t", "dim_a"))],
        &["model.shop.dim_a"],
        vec![],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    let _ = eval(
        &tab,
        "(function(){\
           try{if(navigator.clipboard){\
             navigator.clipboard.writeText=function(){return Promise.reject(new Error('denied'));};\
           }}catch(e){}\
           document.execCommand=function(){return false;};\
         })()",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.model-sql .code-header .code-copy-btn').click()",
    );
    // The writeText rejection lands on a microtask; poll within the flash
    // window for the failure state.
    let mut state = String::new();
    for _ in 0..20 {
        state = eval_string(
            &tab,
            "(function(){var b=document.querySelector('.model-sql .code-header .code-copy-btn');\
               return b.classList.contains('copy-failed')\
                 ? b.getAttribute('aria-label')+'|'+b.getAttribute('title') : '';})()",
        );
        if !state.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        state, "Copy failed|Copy failed",
        "a failed copy flashes copy-failed with a truthful title + aria-label",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('.model-sql .code-header .code-copy-btn').classList.contains('copied')"
        ),
        "a failed copy never claims Copied",
    );
    // After the flash window the button returns to its rest state.
    let mut rest = String::new();
    for _ in 0..30 {
        rest = eval_string(
            &tab,
            "(function(){var b=document.querySelector('.model-sql .code-header .code-copy-btn');\
               return (!b.classList.contains('copy-failed') && !b.classList.contains('copied'))\
                 ? b.getAttribute('aria-label')+'|'+b.getAttribute('title') : '';})()",
        );
        if !rest.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(
        rest, "Copy|Copy",
        "the button resets to the rest Copy state after the flash",
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

// (The old `show_all_inputs` helper is retired with the Inspect/All-inputs
// panel toggle — cute-dbt#201: the Given panel always renders every given
// input unconditionally, so the given grids are on-screen at load.)

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
    // NOT a re-render: expand reveals every folded middle line and recomputes
    // the (still-visible) per-hunk controls' full anatomy — label,
    // aria-expanded, stepper disabled-states, collapse-all visibility (#199);
    // collapse restores everything EXACTLY (#136 bidirectional, carried onto
    // the new control anatomy). A re-render would have reset the SQL
    // File<->Diff view and re-flashed mermaid. Verified through the
    // __cuteExpandAllFolds/__cuteCollapseAllFolds seams on a mounted folded
    // block (the report's own diff is not guaranteed long enough to fold).
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

    // cute-dbt#199 — the #132 top-of-report controls strip is RETIRED; the
    // per-diff fold toggle (buildFoldToggleBtn, asserted in the code-header
    // guards) replaced it. The configurable context-lines input stays in the
    // #139 settings cog panel, defaulting to 3.
    assert_eq!(
        eval_string(
            &tab,
            "String(document.querySelectorAll('.diff-view-controls').length)"
        ),
        "0",
        "the top-of-report diff-view controls strip is retired (#199)",
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
    // cute-dbt#199 — the mirror covers the FULL control anatomy, not just
    // aria-expanded: label, stepper disabled-state, collapse-all visibility.
    assert!(
        eval_string(
            &tab,
            "document.querySelector('#gf .diff-fold-label').textContent"
        )
        .contains("All 4 lines shown"),
        "expand-all relabels the per-hunk control to 'All N lines shown'",
    );
    assert!(
        eval_bool(&tab, "document.querySelector('#gf .fold-expand').disabled"),
        "expand-all disables the + stepper (nothing left to show)",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('#gf .fold-collapse-all').hidden"
        ),
        "expand-all reveals the per-hunk collapse-all affordance",
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
    assert!(
        eval_string(
            &tab,
            "document.querySelector('#gf .diff-fold-label').textContent"
        )
        .contains("Show 4 unchanged lines"),
        "collapse-all relabels the per-hunk control back to 'Show N unchanged lines'",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('#gf .fold-contract').disabled"
        ),
        "collapse-all disables the − stepper (nothing revealed yet)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('#gf .fold-collapse-all').hidden"
        ),
        "collapse-all hides the per-hunk collapse-all affordance again",
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
    // cute-dbt#199 — the expand-step setting, by contrast, needs NO re-render:
    // expandFold reads settings.expandStep live on each activation, so a block
    // mounted BEFORE the setting changed still steps by the new value.
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

    // cute-dbt#199 — expand-step needs NO re-render. Mount a folded block
    // NOW (8 hidden under pad 1), THEN change the expand-step setting; the
    // already-mounted DOM is untouched by the change, and the next band
    // activation reads the new step live (exactly 3 lines reveal).
    let _ = eval(
        &tab,
        &format!(
            "(function(){{var g=document.createElement('code');g.id='es-live';\
               g.innerHTML=window.__cuteRenderBlockDiff({long_diff}, window.__cuteTokenizeSql);\
               document.body.appendChild(g);}})()"
        ),
    );
    let _ = eval(
        &tab,
        "(function(){var i=document.querySelector('#settings-expand-step');\
           i.value='3';i.dispatchEvent(new Event('change'));})()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#es-live .diff-folded[hidden]').length"
        ),
        8,
        "changing expand-step does NOT touch an already-mounted block (no re-render)",
    );
    let _ = eval(
        &tab,
        "document.querySelector('#es-live .diff-fold').click()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#es-live .diff-folded[hidden]').length"
        ),
        5,
        "the next activation reads the new expand-step live (8 hidden − step 3 = 5)",
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

// --- cute-dbt#199: gutter fold steppers + the expand-step setting ---------

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn expand_step_steppers_reveal_contract_directionally_and_persist() {
    // The cute-dbt#199 step-expansion contract, end to end:
    //  (1) the static #settings-expand-step row defaults to 20 and clamps;
    //  (2) the + stepper reveals exactly `step` lines toward the hunk
    //      ("down" fold: from the TOP of the hidden run);
    //  (3) the − stepper re-hides exactly `step` lines mirroring direction
    //      (the most-recently-revealed lines, farthest from the hunk);
    //  (4) the per-hunk collapse-all restores the fully-folded state;
    //  (5) a LEADING fold (hunk below) carries data-fold-dir=up and reveals
    //      from the BOTTOM of the hidden run (adjacent to the hunk);
    //  (6) the setting persists in cute-dbt.settings.v1 across reload where
    //      storage is available.
    let browser = launch_browser();
    let tab = settings_fixture_tab(&browser, "headless_expand_step.html");

    // (1) static markup: the row is present, default 20, range 0..=500.
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#settings-expand-step').value"
        ),
        "20",
        "the expand-step input defaults to 20",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#settings-expand-step').min + '-' + \
             document.querySelector('#settings-expand-step').max"
        ),
        "0-500",
        "the expand-step input advertises the 0..=500 range",
    );

    // Set the step to 2 through the panel.
    let _ = eval(
        &tab,
        "(function(){var i=document.querySelector('#settings-expand-step');\
           i.value='2';i.dispatchEvent(new Event('change'));})()",
    );

    // Mount a "down" fold: 1+1 change, 10 context (c0..c9), 1+1 change with
    // the default pad 3 => head c0..c2, hidden c3..c6 (4), tail c7..c9.
    let down_diff = format!(
        "{{lines:[{{kind:'removed',text:'a',emphasis:null}},\
         {{kind:'added',text:'b',emphasis:null}},{ctx},\
         {{kind:'removed',text:'c',emphasis:null}},\
         {{kind:'added',text:'d',emphasis:null}}]}}",
        ctx = ctx_lines_js(10),
    );
    let _ = eval(
        &tab,
        &format!(
            "(function(){{var g=document.createElement('code');g.id='st';\
               g.innerHTML=window.__cuteRenderBlockDiff({down_diff}, window.__cuteTokenizeSql);\
               document.body.appendChild(g);}})()"
        ),
    );

    // (2) + stepper: exactly 2 reveal, from the TOP of the hidden run (c3,
    // c4) — the side adjacent to the preceding hunk. The control re-parks
    // just above the remaining hidden run and relabels to the remainder.
    let _ = eval(&tab, "document.querySelector('#st .fold-expand').click()");
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#st .diff-folded[hidden]').length"
        ),
        2,
        "the + stepper reveals exactly step=2 of the 4 hidden lines",
    );
    assert_eq!(
        eval_string(
            &tab,
            "Array.from(document.querySelectorAll('#st .fold-0:not([hidden]) .diff-code'))\
               .map(function(n){return n.textContent;}).join(',')"
        ),
        "c3,c4",
        "a down fold reveals from the TOP of the hidden run (adjacent to the hunk above)",
    );
    assert!(
        eval_string(
            &tab,
            "document.querySelector('#st .diff-fold-label').textContent"
        )
        .contains("Show 2 unchanged lines"),
        "the label tracks the remaining hidden count",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#st .diff-fold').getAttribute('aria-expanded')"
        ),
        "false",
        "a partially-revealed fold still reports aria-expanded=false",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('#st .fold-contract').disabled"
        ),
        "the − stepper enables once something is revealed",
    );
    // The control parked just above the remaining hidden run: the next
    // element after it is the first still-hidden folded line (c5).
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#st .diff-fold').nextElementSibling\
               .querySelector('.diff-code').textContent"
        ),
        "c5",
        "the control re-parks adjacent to the remaining hidden run",
    );

    // (3) − stepper: re-hides exactly 2, mirroring direction (the same c3,
    // c4 that were revealed go back under the fold).
    let _ = eval(&tab, "document.querySelector('#st .fold-contract').click()");
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#st .diff-folded[hidden]').length"
        ),
        4,
        "the − stepper re-hides exactly step=2 lines (back to fully folded)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('#st .fold-contract').disabled"
        ),
        "the − stepper disables again at the fully-folded bound",
    );

    // (4) collapse-all restores the folded state after stepping out twice
    // (2 + 2 = fully revealed => "All 4 lines shown", + disabled, parked
    // below the run).
    let _ = eval(&tab, "document.querySelector('#st .fold-expand').click()");
    let _ = eval(&tab, "document.querySelector('#st .fold-expand').click()");
    assert!(
        eval_string(
            &tab,
            "document.querySelector('#st .diff-fold-label').textContent"
        )
        .contains("All 4 lines shown"),
        "two step-2 expansions fully reveal the 4-line run",
    );
    assert!(
        eval_bool(&tab, "document.querySelector('#st .fold-expand').disabled"),
        "the + stepper disables at the fully-revealed bound",
    );
    let _ = eval(
        &tab,
        "document.querySelector('#st .fold-collapse-all').click()",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#st .diff-folded[hidden]').length"
        ),
        4,
        "collapse-all restores the fully-folded hunk",
    );

    // (5) a LEADING fold (context run first, hunk below): dir=up, reveal
    // from the BOTTOM of the hidden run (the lines adjacent to the hunk).
    // 10 context then a change, pad 3 => head 0, hidden c0..c6 (7), tail
    // c7..c9.
    let up_diff = format!(
        "{{lines:[{ctx},\
         {{kind:'removed',text:'c',emphasis:null}},\
         {{kind:'added',text:'d',emphasis:null}}]}}",
        ctx = ctx_lines_js(10),
    );
    let _ = eval(
        &tab,
        &format!(
            "(function(){{var g=document.createElement('code');g.id='su';\
               g.innerHTML=window.__cuteRenderBlockDiff({up_diff}, window.__cuteTokenizeSql);\
               document.body.appendChild(g);}})()"
        ),
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#su .diff-fold').getAttribute('data-fold-dir')"
        ),
        "up",
        "a leading fold (hunk below) carries data-fold-dir=up",
    );
    let _ = eval(&tab, "document.querySelector('#su .fold-expand').click()");
    assert_eq!(
        eval_string(
            &tab,
            "Array.from(document.querySelectorAll('#su .fold-0:not([hidden]) .diff-code'))\
               .map(function(n){return n.textContent;}).join(',')"
        ),
        "c5,c6",
        "an up fold reveals from the BOTTOM of the hidden run (adjacent to the hunk below)",
    );

    // (6) persistence: the changed step rides cute-dbt.settings.v1 and
    // hydrates after a reload (where this origin supports storage).
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
            "window.localStorage.getItem('cute-dbt.settings.v1') || ''",
        );
        assert!(
            raw.contains("\"expandStep\":2"),
            "the expand-step setting persisted to localStorage: {raw}",
        );
        tab.reload(false, None).expect("reload");
        tab.wait_until_navigated().expect("await reload");
        let mut step_val = String::new();
        for _ in 0..50 {
            step_val = eval_string(
                &tab,
                "(document.querySelector('#settings-expand-step')||{}).value||''",
            );
            if step_val == "2" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(
            step_val, "2",
            "expand-step hydrates from localStorage after reload",
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
    // cute-dbt#178 / cute-dbt#199 — the split renderer consumes the SAME
    // BlockDiff lines (kind/text/emphasis) verbatim: removed pairs left,
    // added right, context on both sides, word-level <strong> emphasis
    // preserved, and the unified renderer's two-column gutter numbers the
    // same way. Since #199 the parity extends to FOLDS: long context runs
    // fold in split mode too, with the same hunk model + control anatomy
    // (steppers + label + collapse-all) so one control set drives both
    // layouts.
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
    // cute-dbt#199 — the split table pins its 50/50 geometry via a colgroup
    // (3.8em number cols / auto code cols), fold rows must not skew it.
    assert!(
        split.contains("<colgroup>") && split.contains("ds-c-num") && split.contains("ds-c-code"),
        "the split table carries the ds-c-num/ds-c-code colgroup: {split}",
    );

    // cute-dbt#199 — split folds: a long context run folds in split mode with
    // the SAME control anatomy as unified (fold row + gutter steppers +
    // colspan label cell + collapse-all), the same hidden count, and the
    // same parent-scoped `.fold-<id>` class on the hidden rows.
    let long_diff = format!(
        "{{lines:[{{kind:'removed',text:'a',emphasis:null}},\
         {{kind:'added',text:'b',emphasis:null}},{ctx},\
         {{kind:'removed',text:'c',emphasis:null}},\
         {{kind:'added',text:'d',emphasis:null}}]}}",
        ctx = ctx_lines_js(10),
    );
    let split_folded = eval_string(
        &tab,
        &format!("window.__cuteRenderSplitDiff({long_diff}, window.__cuteTokenizeSql)"),
    );
    assert!(
        split_folded.contains("diff-fold")
            && split_folded.contains("data-fold=\"0\"")
            && split_folded.contains("data-fold-dir=\"down\""),
        "the split renderer emits the same directional fold control: {split_folded}",
    );
    assert!(
        split_folded.contains("Show 4 unchanged lines"),
        "the split fold hides the same 4-line middle as unified (pad 3): {split_folded}",
    );
    assert!(
        split_folded.contains("ds-fold-gutter")
            && split_folded.contains("fold-steppers")
            && split_folded.contains("colspan=\"3\"")
            && split_folded.contains("fold-collapse-all"),
        "the split fold row carries the stepper gutter + colspan label cell: {split_folded}",
    );
    assert!(
        split_folded.contains("diff-folded fold-0\" hidden>"),
        "the split folded rows carry `diff-folded fold-0` and the hidden attribute: {split_folded}",
    );

    // Mounted, the split fold answers the same control set: the band click
    // expands by step (default 20 => all 4), setAllFolds mirrors it back.
    let _ = eval(
        &tab,
        &format!(
            "(function(){{var d=document.createElement('div');d.id='sf';\
               d.innerHTML=window.__cuteRenderSplitDiff({long_diff}, window.__cuteTokenizeSql);\
               document.body.appendChild(d);}})()"
        ),
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#sf .diff-folded[hidden]').length"
        ),
        4,
        "the mounted split block starts with 4 hidden folded rows",
    );
    let _ = eval(&tab, "document.querySelector('#sf .diff-fold').click()");
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#sf .diff-folded[hidden]').length"
        ),
        0,
        "activating the split fold band reveals its rows (step 20 >= hidden 4)",
    );
    let _ = eval(
        &tab,
        "window.__cuteCollapseAllFolds(document.getElementById('sf'))",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('#sf .diff-folded[hidden]').length"
        ),
        4,
        "collapse-all drives the split layout through the same setAllFolds mirror",
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

/// Trimmed label text of the `.this-badge` on the `given: - input: this`
/// section, or `""` when absent. Iterates `.given-section` by
/// `data-input-name` so the awkward `ref('orders')` attribute-selector
/// quoting is avoided. Reads the badge's FIRST TEXT NODE only — since
/// cute-dbt#202 the badge nests its CSS tooltip bubble, so a bare
/// `textContent` would concatenate the tip copy onto the label.
fn this_given_badge_text(tab: &Tab) -> String {
    eval_string(
        tab,
        "(function(){\
           var s=Array.prototype.slice.call(document.querySelectorAll('.given-section'))\
             .filter(function(x){return x.getAttribute('data-input-name')==='this';})[0];\
           var b=s?s.querySelector('.this-badge'):null;\
           return b&&b.firstChild?b.firstChild.nodeValue.trim():'';})()",
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

// cute-dbt#232 (audit D2) — table renders ALWAYS relocate the mode badge
// onto the .fixture-view-bar meta row below the title (mirroring Given);
// only the sql-format / external-fixture paths keep the header placement.
// These tests' expect fixtures render the table path, so the bar is the
// spec-true location.
const MODE_BADGE: &str = "document.querySelector('.expected-panel .fixture-view-bar .mode-badge')";
// cute-dbt#202 (founder decision, epic #197) — the expect-semantics tooltip
// is BADGE-BORNE: the mode badge itself is the focusable trigger and the
// bubble nests inside it (the #146 CSS-bubble mechanism, pass-2 styling).
// The separate ⓘ `.expect-tooltip` button is retired from the expected
// panel (the static DAG hint keeps the class elsewhere).
const MODE_TIP_TRIGGER: &str =
    "document.querySelector('.expected-panel .fixture-view-bar .mode-badge.has-mode-tip')";

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
    // exercises the idempotent `.mode-badge, .expected-model-badge` clear on
    // the persistent `.panel-header` (the #202 clear-list).
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
    // The badge's LABEL is its first text node — since cute-dbt#202 the
    // badge nests its CSS bubble, so bare textContent would concatenate
    // the tip copy onto the label.
    assert_eq!(
        eval_string(&tab, &format!("{MODE_BADGE}.firstChild.nodeValue.trim()")),
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
    // cute-dbt#202 — the badge ITSELF carries the expect-semantics tip (the
    // founder-decided #146 CSS-bubble mechanism, badge-borne): it is a
    // FOCUSABLE trigger (keyboard + touch reachable), never a hover-only
    // native `title`.
    assert!(
        eval_bool(&tab, &format!("{MODE_TIP_TRIGGER} !== null")),
        "an incremental-mode test's badge carries the expect-semantics tip",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{MODE_TIP_TRIGGER}.getAttribute('tabindex')")
        ),
        "0",
        "the tip-bearing badge is keyboard-focusable (tabindex=0)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.expected-panel .expect-tooltip') === null"
        ),
        "the separate ⓘ expect-tooltip button is retired from the expected panel (cute-dbt#202)",
    );
    // The dbt gotcha wording lives in the VISIBLE CSS bubble AND the aria-label.
    // Assert an ASCII substring — the tip contains em-dashes (U+2014), so a
    // full-string compare would mismatch. cute-dbt#159: the copy is now
    // strategy-invariant ("the rows the configured incremental strategy will
    // apply to the table") — true for all 5 strategies, where the old
    // "merged or inserted" was wrong for insert_overwrite / microbatch.
    let bubble_text = eval_string(
        &tab,
        &format!("{MODE_TIP_TRIGGER}.querySelector('.expect-tooltip-bubble').textContent"),
    );
    assert!(
        bubble_text.contains("incremental strategy will apply to the table"),
        "the visible tooltip bubble explains Expected is the rows the strategy applies, got {bubble_text:?}",
    );
    assert!(
        !bubble_text.contains("merged or inserted"),
        "the bubble must NOT carry the old merge/append-centric wording (cute-dbt#159), got {bubble_text:?}",
    );
    let aria = eval_string(
        &tab,
        &format!("{MODE_TIP_TRIGGER}.getAttribute('aria-label')"),
    );
    assert!(
        aria.contains("incremental strategy will apply to the table"),
        "the badge aria-label carries the same strategy-invariant wording (a11y parity), got {aria:?}",
    );
    assert!(
        !aria.contains("merged or inserted"),
        "the aria-label must NOT carry the old merge/append-centric wording (cute-dbt#159), got {aria:?}",
    );
    // cute-dbt#146 review — the regression guard for "hover shows nothing": the
    // bubble is hidden until hover/focus, and FOCUS reveals it (the keyboard
    // path; `:hover` shares the same CSS rule, so a visible-on-focus bubble
    // proves the hover path paints too).
    const BUBBLE_VIS: &str = "getComputedStyle(document.querySelector('.expected-panel .mode-badge .expect-tooltip-bubble')).visibility";
    assert_eq!(
        eval_string(&tab, BUBBLE_VIS),
        "hidden",
        "the tooltip bubble is hidden until hover/focus",
    );
    let _ = eval(&tab, &format!("{MODE_TIP_TRIGGER}.focus()"));
    assert_eq!(
        eval_string(&tab, BUBBLE_VIS),
        "visible",
        "focusing the badge reveals the bubble (the keyboard path a native title never had)",
    );

    // ===== the `this` given is prior-model-state; `ref(...)` is not =====
    assert_eq!(
        this_given_badge_text(&tab),
        "prior model state",
        "a `given: - input: this` is badged 'prior model state'",
    );
    assert!(
        non_this_givens_unbadged(&tab),
        "a ref(...) given carries no prior-model-state badge",
    );
    // cute-dbt#202 — the this-badge carries its explanatory tip via the same
    // badge-borne #146 CSS-bubble mechanism: focusable trigger, aria-label
    // parity, bubble revealed on keyboard focus, hidden again on blur.
    const THIS_TIP_TRIGGER: &str = "document.querySelector('.given-section[data-input-name=\"this\"] .this-badge.has-mode-tip')";
    assert!(
        eval_bool(&tab, &format!("{THIS_TIP_TRIGGER} !== null")),
        "the this-badge carries the prior-model-state info tip",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{THIS_TIP_TRIGGER}.getAttribute('tabindex')")
        ),
        "0",
        "the this-badge tip trigger is keyboard-focusable",
    );
    let this_aria = eval_string(
        &tab,
        &format!("{THIS_TIP_TRIGGER}.getAttribute('aria-label')"),
    );
    assert!(
        this_aria.contains("BEFORE this run"),
        "the this-badge aria-label explains the prior-model-state semantics, got {this_aria:?}",
    );
    assert!(
        !this_aria.contains("merge/insert"),
        "the this-badge copy stays strategy-invariant (the #159/#161 lesson), got {this_aria:?}",
    );
    const THIS_BUBBLE_VIS: &str = "getComputedStyle(document.querySelector('.given-section[data-input-name=\"this\"] .this-badge .expect-tooltip-bubble')).visibility";
    assert_eq!(
        eval_string(&tab, THIS_BUBBLE_VIS),
        "hidden",
        "the this-badge bubble is hidden until hover/focus",
    );
    let _ = eval(&tab, &format!("{THIS_TIP_TRIGGER}.focus()"));
    assert_eq!(
        eval_string(&tab, THIS_BUBBLE_VIS),
        "visible",
        "focusing the this-badge reveals its bubble",
    );
    let _ = eval(&tab, &format!("{THIS_TIP_TRIGGER}.blur()"));
    assert_eq!(
        eval_string(&tab, THIS_BUBBLE_VIS),
        "hidden",
        "blurring the this-badge hides its bubble again",
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
        eval_bool(&tab, &format!("{MODE_TIP_TRIGGER} === null")),
        "LOCKED: no expect-semantics tip on a full-refresh test — even one \
         carrying a `this` given (the gate keys off is_incremental_mode, not the proxy); \
         this also proves the mode_on badge+tip was cleared idempotently",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{MODE_BADGE}.querySelector('.expect-tooltip-bubble') === null")
        ),
        "LOCKED: the full-refresh badge nests NO bubble (there `expect` IS the final table)",
    );
    // The this-badge still shows on mode_off's `this` given — it is gated on
    // is_this alone, independent of the test's incremental branch.
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
        eval_bool(&tab, &format!("{MODE_TIP_TRIGGER} === null")),
        "a test on a non-incremental model shows no expect-semantics tip",
    );

    // ===== modified-but-untested model clears a LEAKED incremental tooltip =====
    // Re-establish an incremental-mode badge+tip, then select a model with no
    // tests (currentTest() === null → renderExpectedPanel, which clears the
    // badge/pill, is NOT called). renderForSelectedModel must clear them
    // unconditionally, else the prior test's badge + tip leak onto the
    // persistent `.panel-header` (gemini review, PR #146).
    select_model(&tab, "dim_inc");
    select_test(&tab, "unit_test.shop.dim_inc.mode_on");
    assert!(
        eval_bool(&tab, &format!("{MODE_TIP_TRIGGER} !== null")),
        "precondition: the incremental-mode badge+tip is present before the switch",
    );
    select_model(&tab, "dim_empty");
    assert!(
        eval_bool(&tab, &format!("{MODE_BADGE} === null")),
        "selecting a modified-but-untested model clears the leaked mode badge (its \
         nested bubble goes with it)",
    );
    // cute-dbt#202 — the #161 clear-list extends to .expected-model-badge:
    // the model pill from the prior test must not leak either (location-
    // agnostic since cute-dbt#232: the pill rides the meta bar, which the
    // no-test body wipe destroys; the header must stay clear too).
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.expected-panel .expected-model-badge') === null"
        ),
        "selecting a modified-but-untested model clears the leaked expected model pill",
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
        &HashMap::new(),
        &externals,
        "",
        ScopeSource::PrDiff,
        DEFAULT_REPORT_TITLE,
        None,
        &cute_dbt::domain::CheckPolicy::default(),
        &cute_dbt::domain::ProjectFacts::default(),
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

// --- cute-dbt#57: source() given binding in the Node-detail panel -----

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn source_given_fixture_card_renders_in_the_given_panel_not_the_shelf() {
    // cute-dbt#57 end-to-end DOM proof, re-homed by cute-dbt#201: a
    // `given: source('synthea_raw', 'patients')` resolves through the
    // manifest sources block to the physical identifier and renders its
    // fixture card in the Given panel (which always shows every input).
    // Clicking the bound `source` import CTE opens the node-detail SHELF
    // with the node's compiled SQL — and deliberately NO fixture card
    // there (the shelf's no-fixtures rule: givens live in the Given panel,
    // never duplicated in node detail).
    let model = model_node_with_compiled(
        "model.shop.stg_patients",
        "with source as (select * from \"memory\".\"main\".\"patients\") \
         select id, name from source",
    );
    let ut = UnitTest::new(
        "t".to_owned(),
        NodeId::new("stg_patients"),
        vec![UnitTestGiven::new(
            "source('synthea_raw', 'patients')".to_owned(),
            serde_json::json!([{"id": 1, "name": "Synthetic Sam"}]),
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(serde_json::Value::Null, None, None),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    );
    let source = SourceNode::new(
        NodeId::new("source.shop.synthea_raw.patients"),
        "synthea_raw",
        "patients",
        Some("patients".to_owned()),
        "main",
        Some("memory".to_owned()),
        Some("\"memory\".\"main\".\"patients\"".to_owned()),
    );
    let m = manifest(vec![model], vec![("unit_test.shop.stg_patients.t", ut)])
        .with_sources(std::iter::once((source.id().clone(), source)).collect());
    let in_scope: InScopeSet =
        std::iter::once("unit_test.shop.stg_patients.t".to_owned()).collect();
    let models: ModelInScopeSet = std::iter::once(NodeId::new("model.shop.stg_patients")).collect();
    let out = tmp("headless_source_given_binding.html");
    let _ = std::fs::remove_file(&out);
    render_report(
        &out,
        &m,
        &in_scope,
        &models,
        &InScopeSet::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "baseline.json",
        ScopeSource::Baseline,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let url = format!(
        "file://{}",
        out.to_str().expect("report path is valid UTF-8")
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url)
        .expect("navigate source-binding report");
    tab.wait_until_navigated().expect("await navigation");
    tab.wait_for_element_with_custom_timeout(
        ".cte-dag-mermaid svg",
        std::time::Duration::from_secs(15),
    )
    .expect("Mermaid DAG SVG renders");

    // ===== the Given panel renders the source() given unconditionally ====
    let card_input = eval_string(
        &tab,
        "(document.querySelector('.left-panel-body .given-section')||{getAttribute:function(){return ''}})\
         .getAttribute('data-input-name')||''",
    );
    assert_eq!(
        card_input, "source('synthea_raw', 'patients')",
        "the source() given renders its fixture card in the Given panel",
    );
    // The fixture rows actually render in the card's grid.
    let card_text = eval_string(
        &tab,
        "(document.querySelector('.left-panel-body .given-section')||{}).textContent||''",
    );
    assert!(
        card_text.contains("Synthetic Sam"),
        "the given's mocked rows render inside the fixture card: {card_text}",
    );

    // ===== clicking the bound import CTE opens the shelf, fixture-free ====
    let clicked = eval_bool(
        &tab,
        "(function(){var g=document.querySelector('.cte-dag-mermaid svg g.node[data-node-id=\"source\"]');\
          if(!g){return false;}g.dispatchEvent(new MouseEvent('click',{bubbles:true}));return true;})()",
    );
    assert!(
        clicked,
        "the `source` import CTE node is present + clickable"
    );
    assert_eq!(
        eval_string(
            &tab,
            "(document.querySelector('.dag-shelf-body .node-detail')||{getAttribute:function(){return ''}})\
             .getAttribute('data-node-id')||''",
        ),
        "source",
        "clicking the import CTE opens the node-detail shelf for that node",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.dag-shelf-body .node-detail .sql-block') !== null",
        ),
        "the shelf shows the node's compiled SQL",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('.dag-shelf-body .given-section') !== null",
        ),
        "the shelf renders NO fixture card — givens live in the Given panel only \
         (the cute-dbt#201 no-fixtures rule)",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#201: the DAG node-detail shelf ---------------------------

/// Poll until `expr` evaluates true (the Mermaid re-render after a node
/// click is async — the fresh SVG arrives on the render promise).
fn wait_until_true(tab: &Tab, expr: &str, what: &str) {
    for _ in 0..60 {
        if eval_bool(tab, expr) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("timed out waiting for: {what}");
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn dag_node_click_opens_shelf_with_model_card_and_close_clears_selection() {
    // cute-dbt#201 — the node-detail shelf contract:
    //  - a DAG-node click opens the shelf with that node's compiled SQL;
    //  - the final-select node maps to THIS model, so the model-detail
    //    card renders from the #200 manifest_nodes entry (description +
    //    tags here);
    //  - a node with NO manifest mapping omits the card but still shows
    //    its compiled SQL;
    //  - ✕ closes the shelf, clears the body, and clears the baked
    //    Mermaid selection (the DAG re-centers);
    //  - a fresh test selection closes the shelf.
    let model = model_node_with_compiled(
        "model.shop.dim_shelf",
        "with src_ext as (select * from \"memory\".\"main\".\"widgets\") \
         select * from src_ext",
    )
    .with_model_metadata(
        Some("Shelf detail model (synthetic).".to_owned()),
        vec!["mart".to_owned()],
    );
    let url = render_to_file(
        "headless_node_shelf.html",
        vec![model],
        vec![
            ("unit_test.shop.dim_shelf.t1", unit_test("t1", "dim_shelf")),
            ("unit_test.shop.dim_shelf.t2", unit_test("t2", "dim_shelf")),
        ],
        &["model.shop.dim_shelf"],
        &["unit_test.shop.dim_shelf.t1", "unit_test.shop.dim_shelf.t2"],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    tab.wait_for_element_with_custom_timeout(
        ".cte-dag-mermaid svg",
        std::time::Duration::from_secs(15),
    )
    .expect("Mermaid DAG SVG renders");

    // ===== closed at load =====
    assert!(
        eval_bool(&tab, "document.querySelector('.dag-shelf').hidden"),
        "the shelf starts hidden",
    );

    // ===== the DAG hint is the #146-contract ⓘ bubble button ============
    // A focusable <button> whose bubble paints on keyboard focus (`:hover`
    // shares the same CSS rule, so visible-on-focus proves hover too).
    assert_eq!(
        eval_string(&tab, "document.querySelector('.cte-dag-hint').tagName"),
        "BUTTON",
        "the DAG hint is a focusable <button>, not a hover-only subtitle",
    );
    const HINT_BUBBLE_VIS: &str = "getComputedStyle(document.querySelector('.cte-dag-hint .expect-tooltip-bubble')).visibility";
    assert_eq!(
        eval_string(&tab, HINT_BUBBLE_VIS),
        "hidden",
        "the DAG-hint bubble is hidden until hover/focus",
    );
    let _ = eval(&tab, "document.querySelector('.cte-dag-hint').focus()");
    assert_eq!(
        eval_string(&tab, HINT_BUBBLE_VIS),
        "visible",
        "focusing the DAG hint reveals its bubble (the #146 keyboard path)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.cte-dag-hint').getAttribute('aria-label')\
             .indexOf('Click a node') === 0",
        ),
        "the DAG hint carries the hint text as its aria-label (AT parity)",
    );

    // ===== final-select click -> shelf with model card + compiled SQL ====
    let clicked = eval_bool(
        &tab,
        "(function(){var g=document.querySelector('.cte-dag-mermaid svg g.node[data-node-id=\"(final select)\"]');\
          if(!g){return false;}g.dispatchEvent(new MouseEvent('click',{bubbles:true}));return true;})()",
    );
    assert!(clicked, "the final-select node is present + clickable");
    assert!(
        !eval_bool(&tab, "document.querySelector('.dag-shelf').hidden"),
        "clicking a DAG node opens the shelf",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.dag-shelf-body .node-detail').getAttribute('data-node-id')",
        ),
        "(final select)",
        "the shelf renders the clicked node's detail",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.dag-shelf-body .node-detail .compiled-sql .sql-block') !== null",
        ),
        "the shelf shows the node's compiled SQL",
    );
    // role badge rides the Compiled SQL summary row.
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.dag-shelf-body .compiled-sql > summary .node-role-badge.role-final') !== null",
        ),
        "the role badge rides the Compiled SQL summary row",
    );
    // the final node maps to THIS model -> the manifest_nodes card.
    assert_eq!(
        eval_string(
            &tab,
            "(document.querySelector('.dag-shelf-body .model-detail-card .mdc-name')||{textContent:''}).textContent",
        ),
        "dim_shelf",
        "the final-select node's model card names this model",
    );
    assert!(
        eval_bool(
            &tab,
            "(document.querySelector('.dag-shelf-body .mdc-desc')||{textContent:''})\
             .textContent.indexOf('Shelf detail model') === 0",
        ),
        "the model card carries the manifest_nodes description",
    );
    assert_eq!(
        eval_string(
            &tab,
            "(document.querySelector('.dag-shelf-body .mdc-tag')||{textContent:''}).textContent",
        ),
        "mart",
        "the model card carries the manifest_nodes tags",
    );

    // ===== unmapped node: card omitted, compiled SQL still shows =========
    // (the click re-rendered the SVG async — wait for the fresh nodes).
    wait_until_true(
        &tab,
        "document.querySelector('.cte-dag-mermaid svg g.node[data-node-id=\"src_ext\"]') !== null",
        "the re-rendered DAG exposes the src_ext node",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.cte-dag-mermaid svg g.node[data-node-id=\"src_ext\"]')\
         .dispatchEvent(new MouseEvent('click',{bubbles:true}))",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.dag-shelf-body .node-detail').getAttribute('data-node-id')",
        ),
        "src_ext",
        "re-clicking another node re-renders the shelf in place",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.dag-shelf-body .model-detail-card') === null",
        ),
        "a node with no manifest mapping omits the model card",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.dag-shelf-body .compiled-sql .sql-block') !== null",
        ),
        "…but its compiled SQL still shows",
    );

    // ===== ✕ closes, clears the body, and clears the selection ===========
    let _ = eval(&tab, "document.querySelector('.dag-shelf-close').click()");
    assert!(
        eval_bool(&tab, "document.querySelector('.dag-shelf').hidden"),
        "✕ hides the shelf",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.dag-shelf-body').children.length === 0",
        ),
        "✕ clears the shelf body",
    );
    // the close re-renders the DAG without the baked `selected` class.
    wait_until_true(
        &tab,
        "document.querySelector('.cte-dag-mermaid svg') !== null \
         && document.querySelector('.cte-dag-mermaid g.selected') === null",
        "the re-rendered DAG carries no selected node after ✕",
    );

    // ===== a fresh test selection closes the shelf =======================
    let _ = eval(
        &tab,
        "document.querySelector('.cte-dag-mermaid svg g.node[data-node-id=\"(final select)\"]')\
         .dispatchEvent(new MouseEvent('click',{bubbles:true}))",
    );
    assert!(
        !eval_bool(&tab, "document.querySelector('.dag-shelf').hidden"),
        "the shelf re-opens on a fresh node click",
    );
    select_test(&tab, "unit_test.shop.dim_shelf.t2");
    assert!(
        eval_bool(&tab, "document.querySelector('.dag-shelf').hidden"),
        "a fresh test selection closes the shelf (the DAG re-centers)",
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

    // Both panels populate on the auto-selected test: the expected panel
    // renders the expect fixture and the Given panel always renders every
    // given input (cute-dbt#201 retired the Inspect/All-inputs toggle) —
    // both panels populated exercises the stacked path robustly (an
    // empty-panel fixture would not trip the toggle). Re-verified on the
    // #201 layout: the .dag-stage flex row (DAG canvas + shelf) wraps below
    // on narrow viewports and must not widen the page either.

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
fn column_header_tooltips_th_trigger_hover_focus_and_bare_column_fallback() {
    // cute-dbt#165 → cute-dbt#178 (the handoff spec): the WHOLE header cell
    // is the tooltip trigger — NO per-column icon/button in the DOM. dim_x's
    // `id` column is described AND carries unique / not_null /
    // accepted_values / relationships column tests; its `status` column has
    // neither. cute-dbt#240 (defect B, third report) REVERSES the old
    // "no metadata ⇒ no affordance" arm: a bare column is decorated too and
    // reveals the truthful no-metadata fallback naming the owning node —
    // hover always answers, and the no-EMPTY-bubble honesty invariant is
    // preserved (the fallback bubble always carries content). The stg_src
    // given verifies a given table resolves ITS OWN input model's metadata.
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

    // ===== expected table: EVERY th decorated (cute-dbt#240) =====
    // `id` carries the rich metadata bubble; `status` carries the truthful
    // no-metadata fallback — no header is hover-dead.
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.expected-panel th.has-col-meta').length"
        )
        .as_u64(),
        Some(2),
        "every expect header is a tooltip trigger (id rich, status fallback)",
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.expected-panel th.has-col-meta.col-meta-empty').length"
        )
        .as_u64(),
        Some(1),
        "exactly the metadata-less `status` header carries the fallback marker",
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
    // The native title is REMOVED from every header (it would double-show
    // over the bubble — cute-dbt#240 decorates ALL of them now).
    assert!(
        eval_bool(&tab, &format!("{TH}.getAttribute('title') === null")),
        "a decorated th sheds the native title attribute",
    );
    const BARE_TH: &str = "document.querySelector('.expected-panel th[data-col-name=\"status\"]')";
    assert!(
        eval_bool(&tab, &format!("{BARE_TH}.getAttribute('title') === null")),
        "the fallback-decorated th sheds the native title too (cute-dbt#240)",
    );
    // cute-dbt#240 — the metadata-less header reveals the truthful fallback
    // bubble naming the owning node (the target model for the expect table).
    let _ = eval(&tab, &format!("{BARE_TH}.focus()"));
    assert!(
        !eval_bool(&tab, "document.getElementById('col-tooltip').hidden"),
        "focusing the metadata-less header reveals the fallback bubble",
    );
    let empty_line = eval_string(
        &tab,
        "document.querySelector('#col-tooltip .ct-empty').textContent",
    );
    assert!(
        empty_line.contains("No description or data tests declared on dim_x"),
        "the fallback bubble names the owning node truthfully, got {empty_line:?}",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('#col-tooltip .ct-desc').textContent"
        ),
        "status",
        "the fallback bubble leads with the column name",
    );
    let _ = eval(&tab, &format!("{BARE_TH}.blur()"));

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
    // cute-dbt#202 — detail strings render as ct-vals/ct-val chips too (the
    // ct-detail run is retired): the shipped single-value forms (the
    // relationships `m.f` here) stay ONE chip, so the chip list is the
    // accepted_values args followed by the relationships detail (the tests
    // render in sorted display-name order: accepted values … relationships).
    let chips = eval_string(
        &tab,
        &format!(
            "Array.from({BUBBLE}.querySelectorAll('.ct-vals .ct-val'))\
             .map(function(v){{return v.textContent;}}).join('|')"
        ),
    );
    assert_eq!(
        chips, "alpha|beta|stg_src.src_id",
        "accepted_values args render as distinct chips; the single-value \
         relationships detail stays one chip",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{BUBBLE}.querySelector('.ct-detail') === null")
        ),
        "the ct-detail run is retired — detail renders as ct-val chips (cute-dbt#202)",
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
    // cute-dbt#240 — both given headers are triggers now: src_id rich (the
    // input model's description), bare_col the no-metadata fallback naming
    // the input model.
    const GIVEN_TH: &str = "document.querySelector('.given-section th[data-col-name=\"src_id\"]')";
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.given-section th.has-col-meta').length"
        )
        .as_u64(),
        Some(2),
        "every given header is a tooltip trigger (src_id rich, bare_col fallback)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.given-section th[data-col-name=\"bare_col\"]')\
             .classList.contains('col-meta-empty')"
        ),
        "the metadata-less given header carries the fallback marker",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.given-section th[data-col-name=\"bare_col\"]').focus()",
    );
    let given_empty = eval_string(
        &tab,
        "document.querySelector('#col-tooltip .ct-empty').textContent",
    );
    assert!(
        given_empty.contains("No description or data tests declared on stg_src"),
        "the given fallback bubble names the INPUT model, got {given_empty:?}",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.given-section th[data-col-name=\"bare_col\"]').blur()",
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

// ===== cute-dbt#202 — rich hover cards ===============================
//
// Model pills (the Given header `ref(` pill `)` + the Expected model
// badge), the format badge's fixture reconstruction, and the
// `overrides · N` badge — all served by the three body-appended singleton
// tips (#model-tooltip / #fmt-tooltip / #ov-tooltip) on mouseenter AND
// focusin (the #146 keyboard/touch-parity rule). The this-badge and the
// incremental mode badge ride the badge-borne CSS bubble instead (founder
// decision, epic #197) — guarded in
// `incremental_badges_modes_tooltip_and_this_given` above.

/// A model-LEVEL test node (`attached_node` set, `column_name` = None) —
/// feeds `ManifestNodePayload::model_tests` (cute-dbt#200).
fn model_test_node(id: &str, model_id: &str, tm: TestMetadata) -> Node {
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
    .with_test_attachment(None, Some(NodeId::new(model_id)), Some(tm))
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn rich_hover_cards_model_pill_format_badge_and_overrides() {
    // One model (dim_x) with one test carrying:
    //   given 0 — ref('stg_src'), dict rows. stg_src HAS a manifest_nodes
    //             entry (description + tags + materialized + a model-level
    //             test) → the pill is a #model-tooltip trigger.
    //   given 1 — ref('ghost'), csv raw-string rows. `ghost` is NOT a
    //             manifest model → the pill renders WITHOUT a trigger
    //             (graceful absence).
    //   overrides — three groups, native scalars (bool / float / string).
    let mut ov = cute_dbt::domain::UnitTestOverrides::new();
    ov.insert(
        "macros".to_owned(),
        BTreeMap::from([("is_incremental".to_owned(), serde_json::json!(true))]),
    );
    ov.insert(
        "vars".to_owned(),
        BTreeMap::from([("threshold".to_owned(), serde_json::json!(0.05))]),
    );
    ov.insert(
        "env_vars".to_owned(),
        BTreeMap::from([("DBT_ENV".to_owned(), serde_json::json!("ci"))]),
    );
    let ut = UnitTest::new(
        "rich".to_owned(),
        NodeId::new("dim_x"),
        vec![
            UnitTestGiven::new(
                "ref('stg_src')".to_owned(),
                serde_json::json!([{ "id": 1, "amount": 2.5 }, { "id": 2, "amount": 7 }]),
                Some("dict".to_owned()),
                None,
            ),
            UnitTestGiven::new(
                "ref('ghost')".to_owned(),
                serde_json::json!("id,amount\n1,10\n"),
                Some("csv".to_owned()),
                None,
            ),
        ],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
            Some("dict".to_owned()),
            None,
        ),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
    .with_overrides(Some(ov));

    let url = render_to_file(
        "headless_rich_hover_cards.html",
        vec![
            model_node("model.shop.dim_x")
                .with_model_metadata(Some("Dimension of x".to_owned()), vec![]),
            model_node_materialized("model.shop.stg_src", "view").with_model_metadata(
                Some("Staged source rows".to_owned()),
                vec!["staging".to_owned()],
            ),
            model_test_node(
                "test.shop.unique_stg_src",
                "model.shop.stg_src",
                TestMetadata::new("unique", None, serde_json::Value::Null),
            ),
        ],
        vec![("unit_test.shop.dim_x.rich", ut)],
        &["model.shop.dim_x"],
        &[], // 0 changed → auto-All mode → the test is selected with content
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    select_model(&tab, "dim_x");

    // ===== given 0: the ref() pill renders + is a model-tip trigger =====
    const PILL: &str =
        "document.querySelectorAll('.given-section')[0].querySelector('.table-title .gt-model')";
    assert_eq!(
        eval_string(&tab, &format!("{PILL}.textContent")),
        "stg_src",
        "the given title's pill carries the bare ref() target",
    );
    assert_eq!(
        eval_string(
            &tab,
            "Array.from(document.querySelectorAll('.given-section')[0]\
             .querySelectorAll('.table-title .gt-prefix'))\
             .map(function(p){return p.textContent;}).join('')"
        ),
        "ref()",
        "the title renders `ref(` + pill + `)` — the `given ·` prefix is dropped",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelectorAll('.given-section')[0].getAttribute('data-input-name')"
        ),
        "ref('stg_src')",
        "data-input-name is retained on the section (the test seam)",
    );
    assert!(
        eval_bool(&tab, &format!("{PILL}.classList.contains('has-model-tip')")),
        "a manifest-known ref target makes the pill a model-tip trigger",
    );
    assert_eq!(
        eval_string(&tab, &format!("{PILL}.getAttribute('tabindex')")),
        "0",
        "the pill trigger is keyboard-focusable",
    );

    // ===== keyboard focus reveals the model card =====
    const MODEL_TIP: &str = "document.getElementById('model-tooltip')";
    let _ = eval(&tab, &format!("{PILL}.focus()"));
    assert!(
        !eval_bool(&tab, &format!("{MODEL_TIP}.hidden")),
        "focusing the pill reveals the model tip (keyboard path)",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{MODEL_TIP}.querySelector('.mt-mat').textContent")
        ),
        "view",
        "the model tip leads with the materialization chip",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{MODEL_TIP}.querySelector('.mt-tag').textContent")
        ),
        "staging",
        "the model tip shows the model's tags",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{MODEL_TIP}.querySelector('.ct-desc').textContent")
        ),
        "Staged source rows",
        "the model tip carries the authored description",
    );
    // cute-dbt#235 — the model tip's test rows ride the COLUMN tooltip's
    // .ct-tests/.ct-test/.ct-key anatomy (the shared ctTestHtml builder;
    // the .mt-mtests/.dt-* dress is retired in the tip).
    assert_eq!(
        eval_string(
            &tab,
            &format!("{MODEL_TIP}.querySelector('.ct-tests .ct-key').textContent")
        ),
        "unique",
        "the model tip lists the model-level data tests in ct-key anatomy",
    );
    // ===== focusout hides; hover reveals again (mouse path) =====
    let _ = eval(&tab, &format!("{PILL}.blur()"));
    assert!(
        eval_bool(&tab, &format!("{MODEL_TIP}.hidden")),
        "blurring the pill hides the model tip",
    );
    let _ = eval(
        &tab,
        &format!("{PILL}.dispatchEvent(new MouseEvent('mouseover', {{bubbles: true}}))"),
    );
    assert!(
        !eval_bool(&tab, &format!("{MODEL_TIP}.hidden")),
        "hovering the pill reveals the model tip (mouse path)",
    );
    let _ = eval(
        &tab,
        &format!("{PILL}.dispatchEvent(new MouseEvent('mouseout', {{bubbles: true}}))"),
    );
    assert!(
        eval_bool(&tab, &format!("{MODEL_TIP}.hidden")),
        "leaving the pill hides the model tip",
    );

    // ===== given 1: an absent manifest entry ⇒ NO trigger class =====
    const GHOST_PILL: &str =
        "document.querySelectorAll('.given-section')[1].querySelector('.table-title .gt-model')";
    assert_eq!(
        eval_string(&tab, &format!("{GHOST_PILL}.textContent")),
        "ghost",
        "the unknown ref target still renders as a pill",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("!{GHOST_PILL}.classList.contains('has-model-tip')")
        ),
        "a ref target absent from manifest_nodes carries NO tip trigger (graceful)",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{GHOST_PILL}.getAttribute('tabindex') === null")
        ),
        "the trigger-less pill is not focusable",
    );

    // ===== format badge: dict reconstructs as YAML =====
    const DICT_BADGE: &str =
        "document.querySelectorAll('.given-section')[0].querySelector('.format-badge')";
    assert_eq!(
        eval_string(&tab, &format!("{DICT_BADGE}.textContent")),
        "format: dict",
        "the dict format badge now shows (the pre-#202 suppression is retired)",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{DICT_BADGE}.classList.contains('has-fmt-tip')")
        ),
        "the dict badge is a fmt-tip trigger",
    );
    assert_eq!(
        eval_string(&tab, &format!("{DICT_BADGE}.getAttribute('tabindex')")),
        "0",
        "the fmt-tip trigger is keyboard-focusable",
    );
    const FMT_TIP: &str = "document.getElementById('fmt-tooltip')";
    let _ = eval(&tab, &format!("{DICT_BADGE}.focus()"));
    assert!(
        !eval_bool(&tab, &format!("{FMT_TIP}.hidden")),
        "focusing the dict badge reveals the reconstruction tip",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{FMT_TIP}.querySelector('.code-filename').textContent")
        ),
        "format: dict",
        "the tip frame is labeled with the format",
    );
    // Column order follows the FixtureTable POD (dict keys arrive through
    // serde_json's sorted map, so `amount` precedes `id` — the same order
    // the Current grid shows).
    assert_eq!(
        eval_string(
            &tab,
            &format!(
                "Array.from({FMT_TIP}.querySelectorAll('.diff-code'))\
                 .map(function(l){{return l.textContent;}}).join('\\n')"
            ),
        ),
        "- amount: 2.5\n  id: 1\n- amount: 7\n  id: 2",
        "the dict fixture reconstructs faithfully as a YAML row list",
    );
    let _ = eval(&tab, &format!("{DICT_BADGE}.blur()"));
    assert!(
        eval_bool(&tab, &format!("{FMT_TIP}.hidden")),
        "blurring the dict badge hides the fmt tip",
    );

    // ===== format badge: csv reconstructs header + comma rows =====
    const CSV_BADGE: &str =
        "document.querySelectorAll('.given-section')[1].querySelector('.format-badge')";
    assert_eq!(
        eval_string(&tab, &format!("{CSV_BADGE}.textContent")),
        "format: csv",
        "the csv format badge renders",
    );
    let _ = eval(&tab, &format!("{CSV_BADGE}.focus()"));
    assert_eq!(
        eval_string(
            &tab,
            &format!(
                "Array.from({FMT_TIP}.querySelectorAll('.diff-code'))\
                 .map(function(l){{return l.textContent;}}).join('\\n')"
            ),
        ),
        "id,amount\n1,10",
        "the csv fixture reconstructs faithfully as header + comma rows",
    );
    let _ = eval(&tab, &format!("{CSV_BADGE}.blur()"));
    assert!(
        eval_bool(&tab, &format!("{FMT_TIP}.hidden")),
        "blurring the csv badge hides the fmt tip",
    );

    // ===== overrides badge: count + grouped key = value rows =====
    const OV_BADGE: &str = "document.querySelector('.test-badges .tb-overrides')";
    assert_eq!(
        eval_string(&tab, &format!("{OV_BADGE}.textContent")),
        "overrides · 3",
        "the badge counts TOTAL keys across groups",
    );
    assert_eq!(
        eval_string(&tab, &format!("{OV_BADGE}.getAttribute('tabindex')")),
        "0",
        "the overrides badge is keyboard-focusable",
    );
    const OV_TIP: &str = "document.getElementById('ov-tooltip')";
    let _ = eval(&tab, &format!("{OV_BADGE}.focus()"));
    assert!(
        !eval_bool(&tab, &format!("{OV_TIP}.hidden")),
        "focusing the overrides badge reveals the grouped tip",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!(
                "Array.from({OV_TIP}.querySelectorAll('.ov-grp-name'))\
                 .map(function(g){{return g.textContent;}}).join('|')"
            ),
        ),
        "env_vars|macros|vars",
        "the tip groups by macros/vars/env_vars (deterministic BTreeMap order)",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!(
                "Array.from({OV_TIP}.querySelectorAll('.ov-row'))\
                 .map(function(r){{return r.querySelector('.ov-key').textContent\
                 + '=' + r.querySelector('.ov-val').textContent;}}).join('|')"
            ),
        ),
        "DBT_ENV=ci|is_incremental=true|threshold=0.05",
        "key = value rows stringify the native scalars (bool/float/string)",
    );
    let _ = eval(&tab, &format!("{OV_BADGE}.blur()"));
    assert!(
        eval_bool(&tab, &format!("{OV_TIP}.hidden")),
        "blurring the overrides badge hides the tip",
    );

    // ===== Expected: the model pill rides the meta bar (cute-dbt#232 —
    // the bar exists for EVERY table render now, diff or not; the old
    // right-aligned header fallback is retired) =====
    const EX_PILL: &str =
        "document.querySelector('.expected-panel .fixture-view-bar .expected-model-badge')";
    assert_eq!(
        eval_string(&tab, &format!("{EX_PILL}.textContent")),
        "dim_x",
        "the Expected panel carries the target model's pill",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{EX_PILL}.classList.contains('has-model-tip')")
        ),
        "the expected pill is a model-tip trigger when manifest_nodes knows the model",
    );
    let _ = eval(&tab, &format!("{EX_PILL}.focus()"));
    assert_eq!(
        eval_string(
            &tab,
            &format!("{MODEL_TIP}.querySelector('.ct-desc').textContent")
        ),
        "Dimension of x",
        "the expected pill's tip shows the TARGET model's description",
    );
    let _ = eval(&tab, &format!("{EX_PILL}.blur()"));

    // ===== idempotent re-render: exactly one expected pill =====
    select_test(&tab, "unit_test.shop.dim_x.rich");
    select_test(&tab, "unit_test.shop.dim_x.rich");
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.expected-panel .expected-model-badge').length"
        ),
        1,
        "re-renders never duplicate the expected model pill (the #161 clear-list)",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn expected_bar_orders_model_pill_mode_badge_rowcount_toggle_far_right() {
    // cute-dbt#202 — when the expect side carries a cell diff, the
    // fixture-view bar hosts the relocated meta in reading order
    // [model pill] [mode badge] [row count] with the Diff/File toggle
    // pushed to the FAR RIGHT (the #188/#161 ordering, ported to the
    // pass-2 layout). The badge-borne expect-semantics bubble must still
    // reveal from its relocated position (no clipping ancestor).
    use cute_dbt::domain::{
        Cell, CellChange, CellValue, ColumnStatus, DiffColumn, FixtureTableDiff, RowChange,
        RowChangeKind,
    };
    let id = "unit_test.shop.dim_inc.t";
    let ut = UnitTest::new(
        "t".to_owned(),
        NodeId::new("dim_inc"),
        Vec::new(),
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
            Some("dict".to_owned()),
            None,
        ),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
    .with_incremental_mode(Some(true));
    let expect_diff = UnitTestDataDiff {
        given: Vec::new(),
        expect: Some(FixtureTableDiff {
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
        }),
    };
    let url = render_pr_diff_with_data_diffs(
        "headless_expected_bar_order.html",
        vec![model_node_materialized("model.shop.dim_inc", "incremental")],
        vec![(id, ut)],
        &["model.shop.dim_inc"],
        &[id],
        vec![(id, expect_diff)],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    const BAR: &str = "document.querySelector('.expected-panel .fixture-view-bar')";
    assert!(
        eval_bool(&tab, &format!("{BAR} !== null")),
        "the expect cell diff renders the fixture-view bar",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!(
                "Array.from({BAR}.children).map(function(c){{\
                 return c.classList.contains('expected-model-badge') ? 'model'\
                 : c.classList.contains('mode-badge') ? 'mode'\
                 : c.classList.contains('expected-rowcount') ? 'rows'\
                 : c.classList.contains('cell-diff-toggle') ? 'toggle' : '?';}}).join('|')"
            ),
        ),
        "model|mode|rows|toggle",
        "bar reading order is [model pill][mode badge][row count] … [Diff/File toggle]",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{BAR}.querySelector('.expected-rowcount').textContent")
        ),
        "1 row",
        "the relocated row count rides the bar",
    );
    // The toggle sits visually to the RIGHT of the meta (margin-left:auto).
    assert!(
        eval_bool(
            &tab,
            &format!(
                "{BAR}.querySelector('.cell-diff-toggle').getBoundingClientRect().left \
                 > {BAR}.querySelector('.expected-rowcount').getBoundingClientRect().right"
            ),
        ),
        "the Diff/File toggle is pushed to the far right of the bar",
    );
    // The badge-borne tip still reveals from the relocated bar position.
    const BAR_BADGE: &str =
        "document.querySelector('.expected-panel .fixture-view-bar .mode-badge.has-mode-tip')";
    const BAR_BUBBLE_VIS: &str = "getComputedStyle(document.querySelector('.expected-panel .fixture-view-bar .mode-badge .expect-tooltip-bubble')).visibility";
    assert!(
        eval_bool(&tab, &format!("{BAR_BADGE} !== null")),
        "the relocated incremental badge keeps its tip trigger",
    );
    let _ = eval(&tab, &format!("{BAR_BADGE}.focus()"));
    assert_eq!(
        eval_string(&tab, BAR_BUBBLE_VIS),
        "visible",
        "the relocated badge's bubble reveals on keyboard focus (no clipping ancestor)",
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
    // cute-dbt#201 — DELIBERATE re-target: __cuteSelectNode now opens the
    // DAG node-detail shelf (.dag-shelf-body), not the retired left-panel
    // Node-detail mode. The seam (one global fn) survives — only the
    // destination moved.
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.dag-shelf-body .node-detail').getAttribute('data-node-id')"
        ),
        "src_a",
        "the tap drives the node-detail shelf through __cuteSelectNode",
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
    // cute-dbt#201 — DELIBERATE re-target: the pin routes through
    // __cuteSelectNode, which now opens the DAG node-detail shelf
    // (.dag-shelf-body) instead of the retired left-panel Node-detail mode.
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.dag-shelf-body .node-detail').getAttribute('data-node-id')",
        ),
        "unioned",
        "the pin selects the cited construct (the node-detail shelf follows)",
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

/// A `model` node declaring `config.unique_key = "k"` (no union SQL —
/// the grain check is the only finding) — cute-dbt#259.
fn unique_key_model(full_id: &str) -> Node {
    let mut config = BTreeMap::new();
    config.insert("unique_key".to_owned(), serde_json::json!("k"));
    Node::new(
        NodeId::new(full_id),
        "model",
        Checksum::new("sha256", "ck"),
        Some("select 1".to_owned()),
        None,
        DependsOn::default(),
        None,
        NodeConfig::new(config, false),
        None,
        BTreeMap::new(),
    )
}

/// A generic `unique` test node on column `k` of `attached`, carrying
/// the given flat config entries — cute-dbt#259.
fn unique_k_test(full_id: &str, attached: &str, config: &[(&str, serde_json::Value)]) -> Node {
    let map: BTreeMap<String, serde_json::Value> = config
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect();
    Node::new(
        NodeId::new(full_id),
        "test",
        Checksum::new("none", ""),
        None,
        None,
        DependsOn::default(),
        None,
        NodeConfig::new(map, false),
        None,
        BTreeMap::new(),
    )
    .with_test_attachment(
        Some("k".to_owned()),
        Some(NodeId::new(attached)),
        Some(TestMetadata::new(
            "unique",
            None,
            serde_json::json!({ "column_name": "k" }),
        )),
    )
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn degraded_disabled_and_singular_truthfulness_render_in_the_findings_panel() {
    // cute-dbt#259 — the coverage-truthfulness surface: a fully degraded
    // attribution renders the summary chip (the #146/#188 tooltip
    // contract) + the per-test cause list; a partially degraded one
    // keeps the summary quiet but still enumerates causes; the
    // exists-but-disabled evidence renders distinct from absent; a
    // singular-only backing renders honest UNKNOWN, never a nag.
    let singular = Node::new(
        NodeId::new("test.shop.assert_dim_singular_ok"),
        "test",
        Checksum::new("sha256", "s"),
        Some("select 1".to_owned()),
        None,
        DependsOn::new(Vec::new(), vec![NodeId::new("model.shop.dim_singular")]),
        None,
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    );
    let url = render_to_file(
        "headless_truthfulness_panel.html",
        vec![
            unique_key_model("model.shop.dim_degraded"),
            unique_key_model("model.shop.dim_partial"),
            unique_key_model("model.shop.dim_singular"),
            // dim_degraded: ONE covering test, warn-severity + filtered.
            unique_k_test(
                "test.shop.unique_dim_degraded_k",
                "model.shop.dim_degraded",
                &[
                    ("severity", serde_json::json!("warn")),
                    ("where", serde_json::json!("k > 0")),
                ],
            ),
            // … plus a disabled twin (exists-but-disabled, in-row).
            unique_k_test(
                "test.shop.unique_dim_degraded_k_off",
                "model.shop.dim_degraded",
                &[("enabled", serde_json::json!(false))],
            ),
            // dim_partial: one clean + one degraded covering test.
            unique_k_test(
                "test.shop.a_unique_dim_partial_k",
                "model.shop.dim_partial",
                &[],
            ),
            unique_k_test(
                "test.shop.b_unique_dim_partial_k",
                "model.shop.dim_partial",
                &[("severity", serde_json::json!("warn"))],
            ),
            singular,
        ],
        vec![(
            "unit_test.shop.dim_degraded.t1",
            unit_test("t1", "dim_degraded"),
        )],
        &[
            "model.shop.dim_degraded",
            "model.shop.dim_partial",
            "model.shop.dim_singular",
        ],
        &["unit_test.shop.dim_degraded.t1"],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    const ROW: &str =
        "document.querySelector('.finding-row[data-check=\"grain.unique-key-unbacked\"]')";

    // --- dim_degraded: covered, fully degraded, disabled twin surfaced --
    select_model(&tab, "dim_degraded");
    assert!(
        eval_bool(
            &tab,
            &format!("{ROW}.classList.contains('verdict-covered')")
        ),
        "a degraded backing still attributes — covered, never dropped",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{ROW}.querySelector('.degraded-chip') !== null")
        ),
        "every attributing test is weakened — the summary chip shows",
    );
    // The #146/#188 tooltip contract on the new chip: focusable trigger,
    // aria-label for AT, no native title, focus reveals the bubble.
    assert!(
        eval_bool(
            &tab,
            &format!(
                "(function(){{\
                   var c = {ROW}.querySelector('.degraded-chip');\
                   return c.getAttribute('tabindex') === '0' \
                       && !c.hasAttribute('title') \
                       && (c.getAttribute('aria-label') || '').indexOf('Degraded backing') === 0;\
                 }})()"
            ),
        ),
        "the degraded chip is a focusable, aria-labelled trigger with no native title",
    );
    assert!(
        eval_bool(
            &tab,
            &format!(
                "(function(){{\
                   {ROW}.querySelector('.degraded-chip').focus();\
                   var t = document.getElementById('col-tooltip');\
                   return !!t && !t.hidden && t.textContent.indexOf('Degraded backing') === 0;\
                 }})()"
            ),
        ),
        "keyboard focus reveals the degraded-backing bubble",
    );
    assert!(
        eval_bool(
            &tab,
            &format!(
                "(function(){{\
                   var deg = {ROW}.querySelector('.finding-degraded');\
                   return !!deg \
                       && deg.textContent.indexOf('test.shop.unique_dim_degraded_k') >= 0 \
                       && deg.textContent.indexOf('severity: warn') >= 0 \
                       && deg.textContent.indexOf('where-filtered') >= 0;\
                 }})()"
            ),
        ),
        "the per-test causes are enumerated in-row beside the attribution",
    );
    assert!(
        eval_bool(
            &tab,
            &format!(
                "{ROW}.querySelector('.finding-evidence').textContent\
                 .indexOf('exists but disabled') >= 0"
            ),
        ),
        "the disabled twin surfaces as exists-but-disabled, distinct from absent",
    );

    // --- dim_partial: clean backing exists — quiet summary, causes stay --
    select_model(&tab, "dim_partial");
    assert!(
        eval_bool(
            &tab,
            &format!("{ROW}.querySelector('.degraded-chip') === null")
        ),
        "a clean covering test keeps the summary chip quiet (no false alarm)",
    );
    assert!(
        eval_bool(
            &tab,
            &format!(
                "(function(){{\
                   var deg = {ROW}.querySelector('.finding-degraded');\
                   return !!deg \
                       && deg.textContent.indexOf('test.shop.b_unique_dim_partial_k') >= 0 \
                       && deg.textContent.indexOf('test.shop.a_unique_dim_partial_k') === -1;\
                 }})()"
            ),
        ),
        "only the weakened test carries causes — the clean one stays unmarked",
    );

    // --- dim_singular: honest UNKNOWN, never a nag ----------------------
    select_model(&tab, "dim_singular");
    assert!(
        eval_bool(
            &tab,
            &format!("{ROW}.classList.contains('verdict-unknown')")
        ),
        "singular-only backing is UNKNOWN, never a false Uncovered nag",
    );
    assert!(
        eval_bool(
            &tab,
            &format!(
                "{ROW}.textContent.indexOf('assert_dim_singular_ok') >= 0 \
                 && {ROW}.textContent.indexOf('singular') >= 0"
            ),
        ),
        "the singular tests are enumerated in the row's evidence",
    );
    assert!(
        eval_bool(
            &tab,
            &format!("{ROW}.querySelector('.finding-recommendation') === null"),
        ),
        "an UNKNOWN verdict carries no recommendation nag",
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
        &HashMap::new(),
        "baseline.json",
        ScopeSource::Baseline,
        DEFAULT_REPORT_TITLE,
        None,
        &policy,
        &cute_dbt::domain::ProjectFacts::default(),
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

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn coverage_intelligence_toggle_hides_check_surfaces_and_persists() {
    // cute-dbt#219 — the viewer-side coverage-intelligence display toggle.
    // A settings-panel switch (default ON, persisted as the `coverage` field
    // of cute-dbt.appearance.v1 — the SAME mechanism as theme/density/the
    // engine picker) hides every check-engine-derived surface via
    // html[data-coverage=off] + one CSS rule. PURE display: the findings
    // panel's rendered content stays in the DOM while hidden (the payload is
    // untouched), so flipping back ON restores it with zero re-render.
    let url = render_to_file(
        "headless_coverage_toggle.html",
        vec![findings_model("model.shop.dim_both")],
        vec![("unit_test.shop.dim_both.t1", unit_test("t1", "dim_both"))],
        &["model.shop.dim_both"],
        &["unit_test.shop.dim_both.t1"],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);

    const ROOT: &str = "document.documentElement";
    const INPUT: &str = "document.querySelector('#settings-coverage-input')";

    // ===== boot defaults: toggle present, ON, panel visible =====
    assert!(
        eval_bool(&tab, &format!("{INPUT} !== null")),
        "the settings panel carries the coverage-intelligence switch",
    );
    assert!(
        eval_bool(&tab, &format!("{INPUT}.checked")),
        "the coverage-intelligence toggle defaults to ON",
    );
    assert!(
        !eval_bool(&tab, &format!("{ROOT}.hasAttribute('data-coverage')")),
        "ON leaves no data-coverage attribute on <html> (the default state)",
    );
    assert!(
        visible(&tab, ".model-findings"),
        "the findings panel renders at boot (toggle ON)",
    );
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.findings-checklist > .finding-row').length",
        ),
        2,
        "the fixture model trips both checks — two checklist rows render",
    );

    // ===== a11y: focusable + aria-labelled (the #146 discipline) =====
    assert!(
        eval_bool(
            &tab,
            &format!("({INPUT}.getAttribute('aria-label') || '').length > 0"),
        ),
        "the switch input carries an aria-label for AT",
    );
    let _ = eval(&tab, "document.querySelector('.settings-cog').click()");
    let _ = eval(&tab, &format!("{INPUT}.focus()"));
    assert!(
        eval_bool(&tab, &format!("document.activeElement === {INPUT}")),
        "the switch input is keyboard-focusable",
    );

    // ===== keyboard-operable: a REAL Space keypress toggles OFF =====
    // (tab.press_key sends CDP input, so the checkbox's native default
    // action fires — a synthetic KeyboardEvent would not.)
    tab.press_key(" ")
        .expect("press Space on the focused switch");
    assert!(
        !eval_bool(&tab, &format!("{INPUT}.checked")),
        "Space on the focused switch turns coverage intelligence OFF",
    );
    assert_eq!(
        eval_string(&tab, &format!("{ROOT}.getAttribute('data-coverage')")),
        "off",
        "OFF sets html[data-coverage=off] — the one CSS hook",
    );
    assert!(
        !visible(&tab, ".model-findings"),
        "OFF hides the findings panel (every check-derived surface)",
    );
    // Display-only: the rendered checklist stays in the DOM while hidden —
    // the payload was never touched and nothing was unrendered.
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelectorAll('.findings-checklist > .finding-row').length",
        ),
        2,
        "hiding is class/visibility mutation only — the checklist rows stay in the DOM",
    );

    // ===== toggling back ON restores in place (no re-render) =====
    let _ = eval(&tab, &format!("{INPUT}.click()"));
    assert!(
        eval_bool(&tab, &format!("{INPUT}.checked")),
        "clicking the switch turns coverage intelligence back ON",
    );
    assert!(
        !eval_bool(&tab, &format!("{ROOT}.hasAttribute('data-coverage')")),
        "ON removes the data-coverage attribute",
    );
    assert!(
        visible(&tab, ".model-findings"),
        "ON restores the findings panel without a re-render",
    );

    // ===== persistence: the appearance blob + reload hydration =====
    let storage_ok = eval_bool(
        &tab,
        "(function(){try{if(!window.localStorage)return false;\
           window.localStorage.setItem('__probe','1');\
           window.localStorage.removeItem('__probe');return true;}\
           catch(e){return false;}})()",
    );
    if storage_ok {
        let _ = eval(&tab, &format!("{INPUT}.click()")); // OFF again
        let raw = eval_string(
            &tab,
            "window.localStorage.getItem('cute-dbt.appearance.v1') || ''",
        );
        assert!(
            raw.contains("\"coverage\":\"off\""),
            "the coverage choice persists alongside the appearance state \
             under cute-dbt.appearance.v1: {raw}",
        );
        tab.reload(false, None).expect("reload");
        tab.wait_until_navigated().expect("await reload");
        wait_for_document_ready(&tab);
        // Poll the hydrated attribute — theme.js boots after DOMContentLoaded,
        // which wait_for_document_ready alone can race (the cute-dbt#178
        // pattern). NULL-SAFE on documentElement mid-swap (cute-dbt#208).
        let mut coverage = String::new();
        for _ in 0..50 {
            coverage = eval_string(
                &tab,
                "(document.documentElement \
                 && document.documentElement.getAttribute('data-coverage')) || ''",
            );
            if coverage == "off" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(
            coverage, "off",
            "the persisted OFF state hydrates html[data-coverage=off] across a reload",
        );
        assert!(
            !eval_bool(&tab, &format!("{INPUT}.checked")),
            "the switch hydrates to OFF from localStorage after reload",
        );
        assert!(
            !visible(&tab, ".model-findings"),
            "the findings panel stays hidden across the reload",
        );
    }

    let _ = tab.close(true);
}

// ===== cute-dbt#206 — tier-chip WCAG AA contrast across every theme ====
//
// The Solarized tier-high chip shipped at 3.41:1 — under the 4.5:1 AA
// floor — because the chip's outlined-accent text renders the theme's
// verbatim accent on the finding row's `--surface`, and nothing gated
// that pairing per theme. This guard is the mechanical encoding of the
// sweep: for EVERY [data-theme] pack the chassis ships, every tier chip
// must reach AA on its true backdrop (a 9th theme cannot silently
// regress). The contrast math runs in evaluated JS over RESOLVED
// computed styles, so token overrides (the #198 latte / #206 solarized
// chip-scoped overrides) are measured exactly as the browser paints
// them.
//
// Backdrop resolution mirrors how the chips actually composite: chips
// append into `.finding-summary` inside `.finding-row { background:
// var(--surface) }`, so the effective text backdrop is the nearest
// opaque ancestor background (NOT `--bg`; body bg only shows through
// the gaps between rows). The self-backed TOTAL chip is judged on its
// own `--control-active-bg` fill instead.

/// The full WCAG sweep, evaluated in-page. Returns a JSON array of
/// `{theme, tier, ratio, fg, bg}` — one entry per (theme, tier chip).
/// No live check carries the Advisory tier yet (test registries only),
/// so the sweep injects one advisory chip into a real
/// `.finding-summary` — a pure class-based DOM addition that exercises
/// the exact shipped `.tier-chip.tier-advisory` rules.
const TIER_CHIP_CONTRAST_SWEEP_JS: &str = r#"(function () {
  var THEMES = ["light", "solarized", "latte", "rosepine",
                "dark", "tokyo", "gruvbox", "dracula"];
  var DARK = { dark: true, tokyo: true, gruvbox: true, dracula: true };
  function parseRgb(s) {
    var m = /rgba?\(([^)]+)\)/.exec(s || "");
    if (!m) return null;
    var p = m[1].split(",");
    return { r: parseFloat(p[0]), g: parseFloat(p[1]), b: parseFloat(p[2]),
             a: p.length > 3 ? parseFloat(p[3]) : 1 };
  }
  function chan(v) {
    v = v / 255;
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  }
  function lum(c) {
    return 0.2126 * chan(c.r) + 0.7152 * chan(c.g) + 0.0722 * chan(c.b);
  }
  function ratio(f, b) {
    var lf = lum(f), lb = lum(b);
    var hi = Math.max(lf, lb), lo = Math.min(lf, lb);
    return (hi + 0.05) / (lo + 0.05);
  }
  function backdropOf(el) {
    for (var n = el; n; n = n.parentElement) {
      var c = parseRgb(getComputedStyle(n).backgroundColor);
      if (c && c.a === 1) return c;
    }
    return null; /* no opaque ancestor — surfaced as ratio -1 */
  }
  if (!document.querySelector(".tier-chip.tier-advisory")) {
    var sp = document.createElement("span");
    sp.className = "tier-chip tier-advisory";
    sp.textContent = "advisory";
    document.querySelector(".finding-summary").appendChild(sp);
  }
  /* cute-dbt#269 — the project panel's dispatch banner ships the fourth
     tier class (.tier-chip.tier-unknown). This fixture renders no
     project panel, so inject one the same way as advisory — the exact
     shipped rules are exercised on the same --surface backdrop family
     the real banner sits on. */
  if (!document.querySelector(".tier-chip.tier-unknown")) {
    var su = document.createElement("span");
    su.className = "tier-chip tier-unknown";
    su.textContent = "UNKNOWN";
    document.querySelector(".finding-summary").appendChild(su);
  }
  var root = document.documentElement;
  var out = [];
  for (var i = 0; i < THEMES.length; i++) {
    /* exactly theme.js applyTheme: set data-theme + sync html.dark */
    root.setAttribute("data-theme", THEMES[i]);
    root.classList.toggle("dark", !!DARK[THEMES[i]]);
    var chips = document.querySelectorAll(".tier-chip");
    for (var j = 0; j < chips.length; j++) {
      var cs = getComputedStyle(chips[j]);
      var fg = parseRgb(cs.color);
      var own = parseRgb(cs.backgroundColor);
      var bg = own && own.a === 1 ? own : backdropOf(chips[j].parentElement);
      var tier =
        (/tier-(total|high|advisory|unknown)/.exec(chips[j].className) || [])[1]
        || "unclassed";
      out.push({
        theme: THEMES[i], tier: tier,
        ratio: fg && bg ? ratio(fg, bg) : -1,
        fg: cs.color,
        bg: bg ? "rgb(" + bg.r + ", " + bg.g + ", " + bg.b + ")" : "none"
      });
    }
  }
  return JSON.stringify(out);
})()"#;

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn tier_chips_meet_aa_contrast_on_every_theme() {
    // dim_both trips grain (TOTAL chip) + union (HIGH chip); the sweep
    // JS injects the advisory chip — full tier vocabulary, all 8 themes.
    let url = render_to_file(
        "headless_tier_chip_contrast.html",
        vec![findings_model("model.shop.dim_both")],
        vec![("unit_test.shop.dim_both.t1", unit_test("t1", "dim_both"))],
        &["model.shop.dim_both"],
        &["unit_test.shop.dim_both.t1"],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);

    let raw = eval_string(&tab, TIER_CHIP_CONTRAST_SWEEP_JS);
    let measured: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the contrast sweep returns valid JSON");
    assert_eq!(
        measured.len(),
        32,
        "8 themes x 4 tier chips (total/high/advisory/unknown) measured, got: {raw}",
    );

    let mut failures = Vec::new();
    for m in &measured {
        let theme = m["theme"].as_str().expect("theme is a string");
        let tier = m["tier"].as_str().expect("tier is a string");
        let ratio = m["ratio"].as_f64().expect("ratio is a number");
        let fg = m["fg"].as_str().unwrap_or("?");
        let bg = m["bg"].as_str().unwrap_or("?");
        assert_ne!(tier, "unclassed", "every chip carries a known tier class");
        assert!(
            ratio > 0.0,
            "the {theme}/{tier} chip resolved no opaque backdrop — the \
             backdrop walk must end on a painted surface",
        );
        eprintln!("tier-chip contrast {theme:>9} / {tier:<8} = {ratio:.2}  ({fg} on {bg})");
        if ratio < 4.5 {
            failures.push(format!("{theme}/{tier} = {ratio:.2} ({fg} on {bg})"));
        }
    }
    assert!(
        failures.is_empty(),
        "tier chips below the WCAG AA 4.5:1 floor (cute-dbt#206): {failures:#?}",
    );

    let _ = tab.close(true);
}

// ===== cute-dbt#227 — suppressed-row text AA contrast across every theme ====
//
// The #206 guard above deliberately skipped suppress-chips: every one of
// them sat inside `.finding-row.is-suppressed { opacity: 0.72 }`, which
// composited ALL descendant text toward the page background — effective
// contrast was both sub-AA (2.7–3.9 across themes) and uncomputable from
// resolved tokens. The recorded #227 decision moves "visible-but-quiet"
// from opacity to token-level dimming (muted-but-AA colors, normal
// weight, subtle border), so token math equals effective contrast again.
// This guard pins both halves:
//   · CONTRAST — every informational text surface of the suppressed
//     reveal (suppress-chip, verdict word, check name, construct chip,
//     collapsed-count summary) reaches AA 4.5:1 on its true backdrop in
//     every theme;
//   · MECHANISM — no ancestor of any of those surfaces carries computed
//     opacity < 1, the structural guarantee that the token-derived ratio
//     IS the painted one (a reintroduced row opacity fails loudly here).
// The decorative `.f-mark` stays aria-hidden + `--text-faint` (WCAG
// non-text exemption) and is deliberately NOT measured.

/// The suppressed-surface sweep, evaluated in-page. Returns a JSON array
/// of `{theme, el, ratio, fg, bg, dimmed}` — one entry per
/// (theme, suppressed text surface); `dimmed` lists any self-or-ancestor
/// node whose computed opacity is below 1.
const SUPPRESSED_TEXT_CONTRAST_SWEEP_JS: &str = r#"(function () {
  var THEMES = ["light", "solarized", "latte", "rosepine",
                "dark", "tokyo", "gruvbox", "dracula"];
  var DARK = { dark: true, tokyo: true, gruvbox: true, dracula: true };
  function parseRgb(s) {
    var m = /rgba?\(([^)]+)\)/.exec(s || "");
    if (!m) return null;
    var p = m[1].split(",");
    return { r: parseFloat(p[0]), g: parseFloat(p[1]), b: parseFloat(p[2]),
             a: p.length > 3 ? parseFloat(p[3]) : 1 };
  }
  function chan(v) {
    v = v / 255;
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  }
  function lum(c) {
    return 0.2126 * chan(c.r) + 0.7152 * chan(c.g) + 0.0722 * chan(c.b);
  }
  function ratio(f, b) {
    var lf = lum(f), lb = lum(b);
    var hi = Math.max(lf, lb), lo = Math.min(lf, lb);
    return (hi + 0.05) / (lo + 0.05);
  }
  function backdropOf(el) {
    for (var n = el; n; n = n.parentElement) {
      var c = parseRgb(getComputedStyle(n).backgroundColor);
      if (c && c.a === 1) return c;
    }
    return null; /* no opaque ancestor — surfaced as ratio -1 */
  }
  function dimmedChain(el) {
    var out = [];
    for (var n = el; n; n = n.parentElement) {
      if (parseFloat(getComputedStyle(n).opacity) < 1) {
        out.push(n.tagName.toLowerCase()
          + (n.className ? "." + String(n.className).trim().split(/\s+/).join(".") : ""));
      }
    }
    return out;
  }
  /* measurement hygiene: body backgrounds transition over 120ms, so a
     just-switched theme would otherwise be read mid-interpolation */
  var kill = document.createElement("style");
  kill.textContent = "* { transition: none !important; animation: none !important; }";
  document.head.appendChild(kill);
  /* fail legibly on missing markup: a null target would otherwise die
     inside getComputedStyle as an opaque TypeError — name the absentee
     instead (the eval harness surfaces thrown messages verbatim) */
  var reveal = document.querySelector('[data-testid="findings-suppressed"]');
  if (!reveal) {
    throw new Error("suppressed-text sweep: no [data-testid=findings-suppressed] reveal in the DOM");
  }
  reveal.open = true;
  var row = document.querySelector(".finding-row.is-suppressed");
  if (!row) {
    throw new Error("suppressed-text sweep: no .finding-row.is-suppressed row in the DOM");
  }
  var TARGETS = [
    ["suppress-chip", row.querySelector(".suppress-chip")],
    ["f-verdict", row.querySelector(".f-verdict")],
    ["f-name", row.querySelector(".f-name")],
    ["f-construct", row.querySelector(".f-construct")],
    ["suppressed-summary", reveal.querySelector(":scope > summary")]
  ];
  var missing = TARGETS.filter(function (t) { return !t[1]; })
    .map(function (t) { return t[0]; });
  if (missing.length) {
    throw new Error("suppressed-text sweep: target element(s) not found: "
      + missing.join(", "));
  }
  var root = document.documentElement;
  var out = [];
  for (var i = 0; i < THEMES.length; i++) {
    /* exactly theme.js applyTheme: set data-theme + sync html.dark */
    root.setAttribute("data-theme", THEMES[i]);
    root.classList.toggle("dark", !!DARK[THEMES[i]]);
    for (var j = 0; j < TARGETS.length; j++) {
      var el = TARGETS[j][1];
      var cs = getComputedStyle(el);
      var fg = parseRgb(cs.color);
      var own = parseRgb(cs.backgroundColor);
      var bg = own && own.a === 1 ? own : backdropOf(el.parentElement);
      out.push({
        theme: THEMES[i], el: TARGETS[j][0],
        ratio: fg && bg ? ratio(fg, bg) : -1,
        fg: cs.color,
        bg: bg ? "rgb(" + bg.r + ", " + bg.g + ", " + bg.b + ")" : "none",
        dimmed: dimmedChain(el)
      });
    }
  }
  return JSON.stringify(out);
})()"#;

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn suppressed_row_text_meets_aa_contrast_on_every_theme() {
    // The same suppressed fixture as the visible-but-quiet reveal test:
    // dim_both trips grain + union; the grain finding is suppressed via
    // config, so the reveal carries one `.is-suppressed` row with its
    // suppress-chip next to one normal row (the quietness comparator).
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
    let out = tmp("headless_suppressed_contrast.html");
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
        &HashMap::new(),
        "baseline.json",
        ScopeSource::Baseline,
        DEFAULT_REPORT_TITLE,
        None,
        &policy,
        &cute_dbt::domain::ProjectFacts::default(),
    )
    .expect("render writes the report");
    let url = format!("file://{}", out.to_str().expect("UTF-8 path"));

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);

    let raw = eval_string(&tab, SUPPRESSED_TEXT_CONTRAST_SWEEP_JS);
    let measured: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the suppressed-text sweep returns valid JSON");
    assert_eq!(
        measured.len(),
        40,
        "8 themes x 5 suppressed text surfaces measured, got: {raw}",
    );

    let mut dim_failures = Vec::new();
    let mut aa_failures = Vec::new();
    for m in &measured {
        let theme = m["theme"].as_str().expect("theme is a string");
        let el = m["el"].as_str().expect("el is a string");
        let ratio = m["ratio"].as_f64().expect("ratio is a number");
        let fg = m["fg"].as_str().unwrap_or("?");
        let bg = m["bg"].as_str().unwrap_or("?");
        let dimmed = m["dimmed"].as_array().expect("dimmed is an array");
        assert!(
            ratio > 0.0,
            "the {theme}/{el} text resolved no opaque backdrop — the \
             backdrop walk must end on a painted surface",
        );
        eprintln!("suppressed-text contrast {theme:>9} / {el:<18} = {ratio:.2}  ({fg} on {bg})");
        if !dimmed.is_empty() {
            dim_failures.push(format!("{theme}/{el} dimmed by {dimmed:?}"));
        }
        if ratio < 4.5 {
            aa_failures.push(format!("{theme}/{el} = {ratio:.2} ({fg} on {bg})"));
        }
    }
    assert!(
        dim_failures.is_empty(),
        "suppressed-row text sits under an opacity-dimmed ancestor — token \
         math no longer equals effective contrast (the cute-dbt#227 \
         mechanism pin): {dim_failures:#?}",
    );
    assert!(
        aa_failures.is_empty(),
        "suppressed-row text below the WCAG AA 4.5:1 floor (cute-dbt#227): {aa_failures:#?}",
    );

    let _ = tab.close(true);
}

// ===== cute-dbt#232 — pass-2 conformance: edge-aware tooltip bubbles ·
// expected meta row left · 13.44px tip text ================================
//
// Fixes the three confirmed deviations from the 2026-06-11 design-
// conformance audit (D1 edge clipping, D2 expected badges right+reversed,
// D3 7.8px bubble text). NOTE (merge contention): sibling PR cute-dbt#230
// also appends tests to this file — the #232 block sits at the end so a
// textual conflict resolves by keeping both blocks.

/// An incremental-mode unit test whose expect is a real dict table with NO
/// expect-side cell diff — the COMMON Expected-panel presentation (every
/// baseline-mode render + every pr-diff test without an expect change),
/// where `buildFixtureView` returns a bare grid with no Diff/File toggle.
/// The #232 assertions exercise exactly this path, not the diff-bar
/// special case the existing `expected_bar_orders_…` test covers.
fn incremental_dict_expect_test(name: &str, model_bare: &str) -> UnitTest {
    UnitTest::new(
        name.to_owned(),
        NodeId::new(model_bare),
        vec![UnitTestGiven::new(
            "ref('stg_src')".to_owned(),
            serde_json::json!([{ "id": 1 }]),
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
            Some("dict".to_owned()),
            None,
        ),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
    .with_incremental_mode(Some(true))
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn badge_tip_bubble_stays_inside_a_narrow_viewport() {
    // cute-dbt#232 (audit D1) — the CSS-bubble tip family had zero edge
    // awareness: on a phone-class viewport the incremental badge's bubble
    // painted past the right viewport edge (audit measurement on the
    // playground report: bubble right edge 422.7px vs a 375px viewport =
    // 47.7px of text clipped mid-word). Two fixes cooperate here: the meta
    // row moves LEFT under the Expected title mirroring Given (R2), and the
    // geometry-only edge tagger + CSS anchor rules keep the bubble inside
    // the viewport wherever the trigger sits (R1). Visibility stays pure
    // CSS — the #146 contract — so `focus()` below both reveals the bubble
    // (the CSS :focus rule) and lets the focusin tagger annotate geometry.
    // The fixture stages the audit's phone-viewport worst case: a WIDE
    // given table (the #157 helper) trips the content-overflow stack
    // toggle, so the Expected panel spans the full narrow viewport and the
    // unfixed right-aligned header pushes the badge cluster against the
    // right edge — exactly where its left-anchored bubble clips. The
    // 1-char model name keeps the pill (the badge's right neighbour in the
    // unfixed header) from pulling the badge back from the edge.
    let ut = UnitTest::new(
        "t".to_owned(),
        NodeId::new("m"),
        vec![UnitTestGiven::new(
            "ref('stg_src')".to_owned(),
            wide_cols_rows(12, 3),
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
            Some("dict".to_owned()),
            None,
        ),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
    .with_incremental_mode(Some(true));
    let url = render_to_file(
        "headless_232_bubble_edge.html",
        vec![model_node_materialized("model.shop.m", "incremental")],
        vec![("unit_test.shop.m.t", ut)],
        &["model.shop.m"],
        &[], // 0 changed → auto-All mode → the test is selectable
    );
    // Dedicated narrow (iPhone-class) launch window — the same lever the
    // cute-dbt#157 viewport regression uses (CDP device-metrics overrides
    // silently no-op on `window.innerWidth` in headless_chrome 1.0.21).
    let browser = launch_browser_sized(Some((375, 812)));
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);
    select_model(&tab, "m");
    select_test(&tab, "unit_test.shop.m.t");

    // Nudge the closure-scoped responsive helper and wait for the stacked
    // (single-column) phone layout — the #157 pattern.
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
    assert!(
        stacked,
        "precondition: the phone-class viewport stacks the panel row \
         (the badge-at-the-edge geometry under test needs the stacked layout)",
    );

    // Location-agnostic trigger selector: header on the pre-#232 layout,
    // the always-present fixture-view bar after — the assertion is about
    // viewport containment, not placement.
    const TRIGGER: &str = "document.querySelector('.expected-panel .mode-badge.has-mode-tip')";
    assert!(
        eval_bool(&tab, &format!("{TRIGGER} !== null")),
        "precondition: the incremental-mode badge tip trigger renders",
    );
    let _ = eval(&tab, &format!("{TRIGGER}.focus()"));
    let m = eval(
        &tab,
        &format!(
            "(function(){{var b={TRIGGER}.querySelector('.expect-tooltip-bubble');\
             var r=b.getBoundingClientRect();\
             return {{left:r.left,right:r.right,vw:window.innerWidth}};}})()"
        ),
    );
    let left = m["left"].as_f64().expect("bubble left is a number");
    let right = m["right"].as_f64().expect("bubble right is a number");
    let vw = m["vw"].as_f64().expect("innerWidth is a number");
    eprintln!("badge tip bubble geometry: left={left:.1} right={right:.1} innerWidth={vw:.1}");
    // 1px tolerance for sub-pixel rounding (the #157 precedent —
    // getBoundingClientRect returns fractional px in headless Chrome). The
    // regression this guards is 23–47px of clipping, so the tolerance
    // costs no teeth.
    assert!(
        right <= vw + 1.0,
        "the badge bubble must not paint past the right viewport edge \
         (cute-dbt#232 audit D1 — 47.7px clipped at 375px on the unfixed CSS): \
         bubble.right={right} > innerWidth={vw} + 1",
    );
    assert!(
        left >= -1.0,
        "the badge bubble must not paint past the LEFT viewport edge either: bubble.left={left}",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn badge_tip_text_matches_column_tooltip_description_size() {
    // cute-dbt#232 (audit D3) — the shared `.expect-tooltip-bubble` rule
    // sized its text `0.78rem` = 7.8px at Sakura's 62.5% root
    // (html{font-size:62.5%} → 1rem = 10px) — 58% of spec. Pass-2 sizes
    // badge-tip text exactly like the column-tooltip description: the
    // 12px `.col-tooltip` shell base × 1.12em `.ct-desc` = 13.44px, in
    // ABSOLUTE px precisely because of the Sakura root. The equality is
    // asserted against the real computed `.ct-desc`, plus the absolute
    // value, so the two surfaces can never drift to a shared wrong size.
    let mut col_desc = BTreeMap::new();
    col_desc.insert("id".to_owned(), "Primary key for dim_inc".to_owned());
    let url = render_to_file(
        "headless_232_tip_size.html",
        vec![
            model_node_materialized("model.shop.dim_inc", "incremental")
                .with_column_descriptions(col_desc),
        ],
        vec![(
            "unit_test.shop.dim_inc.t",
            incremental_dict_expect_test("t", "dim_inc"),
        )],
        &["model.shop.dim_inc"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);
    select_model(&tab, "dim_inc");
    select_test(&tab, "unit_test.shop.dim_inc.t");

    // Materialize the singleton #col-tooltip (and its .ct-desc) through the
    // real path: focusing the described expected-table header cell.
    let _ = eval(
        &tab,
        "document.querySelector('.expected-panel th.has-col-meta').focus()",
    );
    let ct_desc_px = eval_string(
        &tab,
        "getComputedStyle(document.querySelector('#col-tooltip .ct-desc')).fontSize",
    );
    assert_eq!(
        ct_desc_px, "13.44px",
        "sanity: the column-tooltip description is the spec's 12px x 1.12em = 13.44px",
    );
    let bubble_px = eval_string(
        &tab,
        "getComputedStyle(document.querySelector(\
         '.expected-panel .mode-badge.has-mode-tip .expect-tooltip-bubble')).fontSize",
    );
    assert_eq!(
        bubble_px, ct_desc_px,
        "the badge-tip bubble text matches the column-tooltip description size \
         (cute-dbt#232 audit D3: 7.8px vs 13.44px on the unfixed CSS)",
    );

    let _ = tab.close(true);
}

// ===== cute-dbt#233 — pass-2 residual deltas (audit D4/D5/D6) ===============
//
// The 2026-06-11 design-conformance audit found the implementation kept
// stale pass-1 values that pass-2 revised. D4 is the load-bearing one:
// `.col-tooltip .ct-key` shipped raw `var(--accent)`, which resolves to
// ≈3:1 against the always-dark tooltip fill on the 4 light themes — an
// AA failure for the 12px bold mono test names. Pass-2 (engine/base.css,
// the `.ct-key` rule) brightens it to
// `color-mix(in oklab, var(--accent) 60%, white)`.
//
// The guard extends the #206/#227 AA family with the same methodology:
// contrast is computed from RESOLVED computed styles against the
// element's EFFECTIVE backdrop — for tooltip text that is the tooltip
// bubble's own opaque `--tooltip-bg` fill (every theme paints the tip
// dark), NEVER `--bg` and NEVER `--surface`. The sweep records the
// bubble's own resolved fill alongside the backdrop the ancestor walk
// found, so the methodology itself is pinned: if the walk ever escapes
// the bubble to a page surface, the test fails loudly.

/// The `.col-tooltip .ct-key` WCAG sweep, evaluated in-page. Returns a
/// JSON array of `{theme, ratio, fg, bg, tipbg}` — one entry per theme;
/// `tipbg` is the bubble's own resolved background (the methodology pin).
const COL_TOOLTIP_CT_KEY_CONTRAST_SWEEP_JS: &str = r#"(function () {
  var THEMES = ["light", "solarized", "latte", "rosepine",
                "dark", "tokyo", "gruvbox", "dracula"];
  var DARK = { dark: true, tokyo: true, gruvbox: true, dracula: true };
  function parseRgb(s) {
    var m = /rgba?\(([^)]+)\)/.exec(s || "");
    if (!m) return null;
    var p = m[1].split(",");
    return { r: parseFloat(p[0]), g: parseFloat(p[1]), b: parseFloat(p[2]),
             a: p.length > 3 ? parseFloat(p[3]) : 1 };
  }
  function chan(v) {
    v = v / 255;
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  }
  function lum(c) {
    return 0.2126 * chan(c.r) + 0.7152 * chan(c.g) + 0.0722 * chan(c.b);
  }
  function ratio(f, b) {
    var lf = lum(f), lb = lum(b);
    var hi = Math.max(lf, lb), lo = Math.min(lf, lb);
    return (hi + 0.05) / (lo + 0.05);
  }
  function backdropOf(el) {
    for (var n = el; n; n = n.parentElement) {
      var c = parseRgb(getComputedStyle(n).backgroundColor);
      if (c && c.a === 1) return c;
    }
    return null; /* no opaque ancestor — surfaced as ratio -1 */
  }
  /* normalize ANY css color serialization through a 1x1 canvas: the
     color-mix(in oklab, ...) fg computes to a non-rgb() string in
     Chrome (oklab()/color()), which the #206 regex parse can't read.
     An unparseable string falls back to canvas-black, which lands a
     ~1:1 ratio on the dark tooltip fill — a loud failure, never a
     silent pass. */
  function cssToRgb(s) {
    var cv = document.createElement("canvas");
    cv.width = cv.height = 1;
    var cx = cv.getContext("2d");
    cx.fillStyle = s;
    cx.fillRect(0, 0, 1, 1);
    var d = cx.getImageData(0, 0, 1, 1).data;
    return { r: d[0], g: d[1], b: d[2], a: d[3] / 255 };
  }
  var tip = document.getElementById("col-tooltip");
  var key = tip.querySelector(".ct-key");
  var root = document.documentElement;
  var out = [];
  for (var i = 0; i < THEMES.length; i++) {
    /* exactly theme.js applyTheme: set data-theme + sync html.dark */
    root.setAttribute("data-theme", THEMES[i]);
    root.classList.toggle("dark", !!DARK[THEMES[i]]);
    var cs = getComputedStyle(key);
    var fg = cssToRgb(cs.color);
    var own = parseRgb(cs.backgroundColor);
    var bg = own && own.a === 1 ? own : backdropOf(key.parentElement);
    out.push({
      theme: THEMES[i],
      ratio: fg && bg ? ratio(fg, bg) : -1,
      fg: cs.color,
      bg: bg ? "rgb(" + bg.r + ", " + bg.g + ", " + bg.b + ")" : "none",
      tipbg: getComputedStyle(tip).backgroundColor
    });
  }
  return JSON.stringify(out);
})()"#;

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn col_tooltip_ct_key_meets_aa_contrast_on_every_theme() {
    // A described + column-tested `id` column materializes the singleton
    // #col-tooltip with a real `.ct-key` row through the shipped path
    // (focusing the decorated expected-table header).
    let mut col_desc = BTreeMap::new();
    col_desc.insert("id".to_owned(), "Primary key for dim_aa".to_owned());
    let ut = UnitTest::new(
        "aa".to_owned(),
        NodeId::new("dim_aa"),
        Vec::new(),
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
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
        "headless_233_ct_key_contrast.html",
        vec![
            model_node("model.shop.dim_aa").with_column_descriptions(col_desc),
            column_test_node(
                "test.shop.unique_dim_aa_id",
                "model.shop.dim_aa",
                "id",
                TestMetadata::new("unique", None, serde_json::Value::Null),
            ),
        ],
        vec![("unit_test.shop.dim_aa.aa", ut)],
        &["model.shop.dim_aa"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);
    select_model(&tab, "dim_aa");
    select_test(&tab, "unit_test.shop.dim_aa.aa");

    let _ = eval(
        &tab,
        "document.querySelector('.expected-panel th.has-col-meta').focus()",
    );
    assert!(
        !eval_bool(&tab, "document.getElementById('col-tooltip').hidden"),
        "precondition: focusing the decorated th reveals the column tooltip",
    );

    // cute-dbt#233 (audit D4) — the full 8-theme AA sweep.
    let raw = eval_string(&tab, COL_TOOLTIP_CT_KEY_CONTRAST_SWEEP_JS);
    let measured: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the contrast sweep returns valid JSON");
    assert_eq!(measured.len(), 8, "all 8 themes measured, got: {raw}",);

    let mut failures = Vec::new();
    for m in &measured {
        let theme = m["theme"].as_str().expect("theme is a string");
        let ratio = m["ratio"].as_f64().expect("ratio is a number");
        let fg = m["fg"].as_str().unwrap_or("?");
        let bg = m["bg"].as_str().unwrap_or("?");
        let tipbg = m["tipbg"].as_str().unwrap_or("?");
        assert!(
            ratio > 0.0,
            "the {theme} .ct-key resolved no opaque backdrop — the backdrop \
             walk must end on a painted surface",
        );
        // The methodology pin: the effective backdrop IS the tooltip
        // bubble's own fill — never a page surface shining through.
        assert_eq!(
            bg, tipbg,
            "the {theme} backdrop walk must land on the tooltip's own \
             --tooltip-bg fill (got {bg}, the bubble paints {tipbg})",
        );
        eprintln!("col-tooltip .ct-key contrast {theme:>9} = {ratio:.2}  ({fg} on {bg})");
        if ratio < 4.5 {
            failures.push(format!("{theme} = {ratio:.2} ({fg} on {bg})"));
        }
    }
    assert!(
        failures.is_empty(),
        ".col-tooltip .ct-key below the WCAG AA 4.5:1 floor (cute-dbt#233 \
         audit D4 — raw var(--accent) lands ≈3:1 on the 4 light themes): \
         {failures:#?}",
    );

    // cute-dbt#233 (audit D5) — the pass-2 chip-row gap is
    // `0.25rem 0.7rem`: rowGap 2.5px / columnGap 7px at Sakura's 10px
    // root (html{font-size:62.5%}); pass-1 shipped 0.45rem = 4.5px.
    assert_eq!(
        eval_string(
            &tab,
            "getComputedStyle(document.querySelector('#col-tooltip .ct-test')).rowGap"
        ),
        "2.5px",
        "the .ct-test row gap stays the shared 0.25rem = 2.5px",
    );
    assert_eq!(
        eval_string(
            &tab,
            "getComputedStyle(document.querySelector('#col-tooltip .ct-test')).columnGap"
        ),
        "7px",
        "the .ct-test column gap is the pass-2 0.7rem = 7px at the 10px \
         Sakura root (cute-dbt#233 audit D5 — pass-1 shipped 0.45rem = 4.5px)",
    );

    let _ = tab.close(true);
}

// ===== cute-dbt#238 — the ov-tooltip key: the ACCENT family's dark-fill
// surface the #273 token matrix never measured ===============================
//
// `.ov-tooltip .ov-key` ships `color-mix(in oklab, var(--accent) 50%,
// white)` for ALL themes (report.css — the #202 overrides tip), inside the
// always-dark `--tooltip-bg` fill. Latte's fill (#4c4f69) is the lightest
// of the 8 and its accent the darkest of the light themes' (deepened
// further by #273 for light-PAGE surfaces — correct there, marginally
// worse here), so the 50% mix lands 3.69:1 on latte — an AA failure for
// the 12px bold mono override keys. The repair is the symmetric twin of
// #233 D4's `.ct-key` latte stand-in: a latte-scoped 35% mix (measured
// 4.76 PASS; 40% still fails at 4.38), the other 7 themes' spec-literal
// 50% mix stays pinned.
//
// Methodology: the #233 sweep's canvas-normalized color-mix fg + the #273
// matrix's alpha-composited effective backdrop, transition kill, and
// checkVisibility hygiene — through the REAL showOvTip reveal path
// (focusing the `overrides · N` badge, the #146 keyboard-parity rule).

/// The `.ov-tooltip .ov-key` WCAG sweep, evaluated in-page. Returns a
/// JSON array of `{theme, ratio, fg, bg, tipbg}` — one entry per theme;
/// `tipbg` is the bubble's own resolved background (the methodology pin).
const OV_TOOLTIP_OV_KEY_CONTRAST_SWEEP_JS: &str = r##"(function () {
  var THEMES = ["light", "solarized", "latte", "rosepine",
                "dark", "tokyo", "gruvbox", "dracula"];
  var DARK = { dark: true, tokyo: true, gruvbox: true, dracula: true };
  function parseRgb(s) {
    var m = /rgba?\(([^)]+)\)/.exec(s || "");
    if (!m) return null;
    var p = m[1].split(",");
    return { r: parseFloat(p[0]), g: parseFloat(p[1]), b: parseFloat(p[2]),
             a: p.length > 3 ? parseFloat(p[3]) : 1 };
  }
  function chan(v) {
    v = v / 255;
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  }
  function lum(c) {
    return 0.2126 * chan(c.r) + 0.7152 * chan(c.g) + 0.0722 * chan(c.b);
  }
  function ratio(f, b) {
    var lf = lum(f), lb = lum(b);
    var hi = Math.max(lf, lb), lo = Math.min(lf, lb);
    return (hi + 0.05) / (lo + 0.05);
  }
  /* the #273 matrix's EFFECTIVE composited backdrop: collect every painted
     fill from the element itself up to the first OPAQUE one, then
     alpha-composite top-down — an opaque-only walk would skip rgba tints. */
  function effectiveBackdrop(el) {
    var layers = [];
    for (var n = el; n; n = n.parentElement) {
      var c = parseRgb(getComputedStyle(n).backgroundColor);
      if (c && c.a > 0) {
        layers.push(c);
        if (c.a === 1) {
          var acc = layers.pop();
          while (layers.length) {
            var top = layers.pop();
            acc = { r: top.a * top.r + (1 - top.a) * acc.r,
                    g: top.a * top.g + (1 - top.a) * acc.g,
                    b: top.a * top.b + (1 - top.a) * acc.b, a: 1 };
          }
          return acc;
        }
      }
    }
    return null;
  }
  /* the #233 canvas normalization: the color-mix(in oklab, ...) fg
     computes to a non-rgb() string in Chrome (oklab()/color()), which the
     regex parse can't read. An unparseable string falls back to
     canvas-black, which lands a ~1:1 ratio on the dark tooltip fill — a
     loud failure, never a silent pass. */
  function cssToRgb(s) {
    var cv = document.createElement("canvas");
    cv.width = cv.height = 1;
    var cx = cv.getContext("2d");
    cx.fillStyle = s;
    cx.fillRect(0, 0, 1, 1);
    var d = cx.getImageData(0, 0, 1, 1).data;
    return { r: d[0], g: d[1], b: d[2], a: d[3] / 255 };
  }
  /* measurement hygiene: kill transitions before any read */
  var kill = document.createElement("style");
  kill.textContent = "* { transition: none !important; animation: none !important; }";
  document.head.appendChild(kill);
  var tip = document.getElementById("ov-tooltip");
  if (!tip || tip.hidden) { throw new Error("ov-key sweep: #ov-tooltip is not revealed"); }
  var key = tip.querySelector(".ov-key");
  if (!key) { throw new Error("ov-key sweep: no .ov-key row in the revealed tip"); }
  /* checkVisibility, never rect>0 — a hidden tip would measure vacuously */
  if (!key.checkVisibility()) {
    throw new Error("ov-key sweep: .ov-key is not visible");
  }
  var root = document.documentElement;
  var out = [];
  for (var i = 0; i < THEMES.length; i++) {
    /* exactly theme.js applyTheme: set data-theme + sync html.dark */
    root.setAttribute("data-theme", THEMES[i]);
    root.classList.toggle("dark", !!DARK[THEMES[i]]);
    var cs = getComputedStyle(key);
    var fg = cssToRgb(cs.color);
    var bg = effectiveBackdrop(key);
    out.push({
      theme: THEMES[i],
      ratio: fg && bg ? ratio(fg, bg) : -1,
      fg: cs.color,
      bg: bg ? "rgb(" + bg.r + ", " + bg.g + ", " + bg.b + ")" : "none",
      tipbg: getComputedStyle(tip).backgroundColor
    });
  }
  return JSON.stringify(out);
})()"##;

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn ov_tooltip_ov_key_meets_aa_contrast_on_every_theme() {
    // The rich-fixture overrides shape: three groups, native scalars —
    // materializes the `overrides · 3` badge whose focus runs the REAL
    // showOvTip path into the #ov-tooltip singleton.
    let mut ov = cute_dbt::domain::UnitTestOverrides::new();
    ov.insert(
        "macros".to_owned(),
        BTreeMap::from([("is_incremental".to_owned(), serde_json::json!(true))]),
    );
    ov.insert(
        "vars".to_owned(),
        BTreeMap::from([("threshold".to_owned(), serde_json::json!(0.05))]),
    );
    ov.insert(
        "env_vars".to_owned(),
        BTreeMap::from([("DBT_ENV".to_owned(), serde_json::json!("ci"))]),
    );
    let ut = UnitTest::new(
        "aa".to_owned(),
        NodeId::new("dim_ov"),
        Vec::new(),
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
            Some("dict".to_owned()),
            None,
        ),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
    .with_overrides(Some(ov));
    let url = render_to_file(
        "headless_238_ov_key_contrast.html",
        vec![model_node("model.shop.dim_ov")],
        vec![("unit_test.shop.dim_ov.aa", ut)],
        &["model.shop.dim_ov"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);
    select_model(&tab, "dim_ov");
    select_test(&tab, "unit_test.shop.dim_ov.aa");

    const OV_BADGE: &str = "document.querySelector('.test-badges .tb-overrides')";
    let _ = eval(&tab, &format!("{OV_BADGE}.focus()"));
    assert!(
        !eval_bool(&tab, "document.getElementById('ov-tooltip').hidden"),
        "precondition: focusing the overrides badge reveals the grouped tip \
         (the real showOvTip path)",
    );

    // cute-dbt#238 — the full 8-theme AA sweep.
    let raw = eval_string(&tab, OV_TOOLTIP_OV_KEY_CONTRAST_SWEEP_JS);
    let measured: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the ov-key contrast sweep returns valid JSON");
    assert_eq!(measured.len(), 8, "all 8 themes measured, got: {raw}");

    let mut failures = Vec::new();
    for m in &measured {
        let theme = m["theme"].as_str().expect("theme is a string");
        let ratio = m["ratio"].as_f64().expect("ratio is a number");
        let fg = m["fg"].as_str().unwrap_or("?");
        let bg = m["bg"].as_str().unwrap_or("?");
        let tipbg = m["tipbg"].as_str().unwrap_or("?");
        assert!(
            ratio > 0.0,
            "the {theme} .ov-key resolved no opaque backdrop — the composite \
             walk must end on a painted surface",
        );
        // The methodology pin: the effective backdrop IS the tooltip
        // bubble's own --tooltip-bg fill (no .ov-grp/.ov-row layer paints,
        // so a diverging composite means the walk escaped the bubble).
        assert_eq!(
            bg, tipbg,
            "the {theme} backdrop walk must land on the tooltip's own \
             --tooltip-bg fill (got {bg}, the bubble paints {tipbg})",
        );
        eprintln!("ov-tooltip .ov-key contrast {theme:>9} = {ratio:.2}  ({fg} on {bg})");
        if ratio < 4.5 {
            failures.push(format!("{theme} = {ratio:.2} ({fg} on {bg})"));
        }
    }
    assert!(
        failures.is_empty(),
        ".ov-tooltip .ov-key below the WCAG AA 4.5:1 floor (cute-dbt#238 — \
         the all-theme 50% accent mix lands 3.69:1 on latte's #4c4f69 \
         fill): {failures:#?}",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn badge_tip_bubble_rounds_at_small_radius() {
    // cute-dbt#233 (audit D6) — pass-2 (engine/base.css:548-556) rounds
    // the badge-borne bubble at `--radius-sm`; pass-1 let it inherit the
    // shared `.expect-tooltip-bubble` `--radius-pan`. Asserted against
    // in-page reference elements so no px values are hardcoded (the
    // tokens vary per style pack); the sm≠pan precondition keeps the
    // equality's teeth honest.
    let url = render_to_file(
        "headless_233_badge_radius.html",
        vec![model_node_materialized("model.shop.dim_inc", "incremental")],
        vec![(
            "unit_test.shop.dim_inc.t",
            incremental_dict_expect_test("t", "dim_inc"),
        )],
        &["model.shop.dim_inc"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);
    select_model(&tab, "dim_inc");
    select_test(&tab, "unit_test.shop.dim_inc.t");

    let m = eval(
        &tab,
        "(function(){\
         var ref = document.createElement('div');\
         document.body.appendChild(ref);\
         ref.style.borderRadius = 'var(--radius-sm)';\
         var sm = getComputedStyle(ref).borderRadius;\
         ref.style.borderRadius = 'var(--radius-pan)';\
         var pan = getComputedStyle(ref).borderRadius;\
         ref.remove();\
         var bubble = document.querySelector(\
           '.expected-panel .mode-badge.has-mode-tip .expect-tooltip-bubble');\
         var got = bubble ? getComputedStyle(bubble).borderRadius : '';\
         return {got: got, sm: sm, pan: pan};})()",
    );
    // A missing bubble surfaces as got == "" so the assert below names the
    // mismatch cleanly instead of a cryptic in-page TypeError.
    let got = m["got"].as_str().expect("bubble radius resolves");
    assert!(
        !got.is_empty(),
        "precondition: the badge-borne bubble renders \
         ('.expected-panel .mode-badge.has-mode-tip .expect-tooltip-bubble' not found)",
    );
    let sm = m["sm"].as_str().expect("--radius-sm resolves");
    let pan = m["pan"].as_str().expect("--radius-pan resolves");
    assert_ne!(
        sm, pan,
        "precondition: the default pack's --radius-sm and --radius-pan \
         differ, so the equality below has teeth",
    );
    assert_eq!(
        got, sm,
        "the badge-borne bubble rounds at --radius-sm (cute-dbt#233 audit \
         D6 — the unfixed CSS inherited --radius-pan = {pan})",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn expected_meta_row_reads_left_without_an_expect_diff() {
    // cute-dbt#232 (audit D2) — spec: the Expected meta reads LEFT under
    // the panel title in [model pill][mode badge][N rows] order, mirroring
    // Given, in EVERY table render. On the unfixed JS the meta bar only
    // existed when an expect-side cell diff built the Diff/File toggle; in
    // every other render (the COMMON presentation) the badges fell back
    // into the .panel-header pushed RIGHT in REVERSED order
    // [rows][branch][model].
    let url = render_to_file(
        "headless_232_meta_row.html",
        vec![model_node_materialized("model.shop.dim_inc", "incremental")],
        vec![(
            "unit_test.shop.dim_inc.t",
            incremental_dict_expect_test("t", "dim_inc"),
        )],
        &["model.shop.dim_inc"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);
    select_model(&tab, "dim_inc");
    select_test(&tab, "unit_test.shop.dim_inc.t");

    const BAR: &str = "document.querySelector('.expected-panel .fixture-view-bar')";
    assert!(
        eval_bool(&tab, &format!("{BAR} !== null")),
        "the Expected meta row exists even with NO expect diff \
         (cute-dbt#232 audit D2 — the bar was diff-only on the unfixed JS)",
    );
    // Reading order inside the bar: [model pill][mode badge][row count] —
    // no Diff/File toggle on this path (no expect diff to toggle to).
    assert_eq!(
        eval_string(
            &tab,
            &format!(
                "Array.from({BAR}.children).map(function(c){{\
                 return c.classList.contains('expected-model-badge') ? 'model'\
                 : c.classList.contains('mode-badge') ? 'mode'\
                 : c.classList.contains('expected-rowcount') ? 'rows'\
                 : c.classList.contains('cell-diff-toggle') ? 'toggle' : '?';}}).join('|')"
            ),
        ),
        "model|mode|rows",
        "the no-diff meta row reads [model pill][mode badge][row count], no toggle",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{BAR}.querySelector('.expected-rowcount').textContent")
        ),
        "1 row",
        "the relocated row count rides the bar",
    );
    // Visual reading order matches DOM order: the pill paints LEFT of the
    // row count (left-aligned cluster — no margin-left:auto push here).
    assert!(
        eval_bool(
            &tab,
            &format!(
                "{BAR}.querySelector('.expected-model-badge').getBoundingClientRect().left \
                 < {BAR}.querySelector('.expected-rowcount').getBoundingClientRect().left"
            ),
        ),
        "the model pill paints left of the row count (left-aligned meta)",
    );
    // The header carries only the <h2> — the badges/rowcount all moved to
    // the meta row (the header fallback is retired for table renders).
    assert_eq!(
        eval_i64(
            &tab,
            "document.querySelector('.expected-panel .panel-header').children.length"
        ),
        1,
        "the Expected header carries only the title once the meta row owns the badges",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.expected-panel .panel-header').children[0].tagName"
        ),
        "H2",
        "the header's only child is the panel title",
    );

    let _ = tab.close(true);
}

// ===== cute-dbt#235 — given column-header tooltips for seed/source =====
//
// The #165 given-side column tooltips only ever resolved `ref(...)`-to-
// MODEL and `this` inputs; a seed-ref given (the committed jaffle-shop
// fixture's `ref('raw_customers')` shape) and a `source('a','b')` given
// carried NO column metadata, so their headers offered no tooltip while
// the expected table's did. These guards pin the widened payload
// (models | seeds | snapshots via the refable resolver; sources via the
// manifest `sources` map) + the honest degrade (a metadata-less column
// renders NO trigger — never an empty bubble).

/// Baseline-mode render whose manifest also carries `sources` entries —
/// the `render_to_file` twin for `source(...)`-given tests.
fn render_with_sources_to_file(
    filename: &str,
    nodes: Vec<Node>,
    sources: Vec<SourceNode>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    changed_ids: &[&str],
) -> String {
    let all_ids: Vec<String> = tests.iter().map(|(id, _)| (*id).to_owned()).collect();
    let m = manifest(nodes, tests)
        .with_sources(sources.into_iter().map(|s| (s.id().clone(), s)).collect());
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

/// A `seed` node (resource_type "seed") — dbt's ref() resolves over the
/// refable set (models, seeds, snapshots), so a given may ref it.
fn seed_node(full_id: &str) -> Node {
    Node::new(
        NodeId::new(full_id),
        "seed",
        Checksum::new("sha256", "ck"),
        None,
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
fn given_header_tooltips_resolve_seed_and_source_metadata() {
    // RED on pre-#235 main: both givens rendered 0 decorated headers
    // (the seed/source arms contributed no column_meta) while the
    // expected table's headers tooltipped — the regression Christopher
    // hit reviewing PR #230. Hover AND keyboard focus must both reveal
    // (the #146/#161 contract the th trigger carries since #166).
    let mut seed_desc = BTreeMap::new();
    seed_desc.insert("customer_id".to_owned(), "Seed primary key".to_owned());
    let seed = seed_node("seed.shop.raw_customers").with_column_descriptions(seed_desc);
    let mut src_desc = BTreeMap::new();
    src_desc.insert("Id".to_owned(), "Unique patient identifier".to_owned());
    let source = SourceNode::new(
        NodeId::new("source.shop.raw.patients"),
        "raw",
        "patients",
        None,
        "main",
        None,
        None,
    )
    .with_column_descriptions(src_desc);

    let ut = UnitTest::new(
        "seed_source_cols".to_owned(),
        NodeId::new("dim_x"),
        vec![
            UnitTestGiven::new(
                "ref('raw_customers')".to_owned(),
                serde_json::json!([{ "customer_id": 1, "undocumented": 2 }]),
                Some("dict".to_owned()),
                None,
            ),
            UnitTestGiven::new(
                "source('raw', 'patients')".to_owned(),
                serde_json::json!([{ "Id": "a-1", "FIRST": "Ada" }]),
                Some("dict".to_owned()),
                None,
            ),
        ],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
            Some("dict".to_owned()),
            None,
        ),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    );

    let url = render_with_sources_to_file(
        "headless_seed_source_col_tooltips.html",
        vec![
            model_node("model.shop.dim_x"),
            seed,
            column_test_node(
                "test.shop.unique_raw_customers_customer_id",
                "seed.shop.raw_customers",
                "customer_id",
                TestMetadata::new("unique", None, serde_json::Value::Null),
            ),
            column_test_node(
                "test.shop.not_null_raw_patients_Id",
                "source.shop.raw.patients",
                "Id",
                TestMetadata::new("not_null", None, serde_json::Value::Null),
            ),
        ],
        vec![source],
        vec![("unit_test.shop.dim_x.seed_source_cols", ut)],
        &["model.shop.dim_x"],
        &[], // 0 changed → auto-All mode → the test is selected with content
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    select_model(&tab, "dim_x");

    // ===== given 0 (seed ref): the described+tested column decorates =====
    // cute-dbt#240 — BOTH headers are triggers (customer_id rich,
    // undocumented the truthful fallback); exactly one is rich.
    const SEED_SEC: &str = "document.querySelectorAll('.given-section')[0]";
    assert_eq!(
        eval(
            &tab,
            &format!("{SEED_SEC}.querySelectorAll('th.has-col-meta').length")
        )
        .as_u64(),
        Some(2),
        "every seed-given header is a trigger (customer_id rich, undocumented fallback)",
    );
    assert_eq!(
        eval(
            &tab,
            &format!("{SEED_SEC}.querySelectorAll('th.has-col-meta:not(.col-meta-empty)').length")
        )
        .as_u64(),
        Some(1),
        "exactly the described seed column is rich (customer_id)",
    );
    const SEED_TH: &str = "document.querySelectorAll('.given-section')[0]\
         .querySelector('th[data-col-name=\"customer_id\"]')";
    assert_eq!(
        eval_string(&tab, &format!("{SEED_TH}.getAttribute('data-col-name')")),
        "customer_id",
        "the decorated seed-given header is the seed's described column",
    );
    // Keyboard path (the #146/#161 contract): focus reveals the bubble.
    let _ = eval(&tab, &format!("{SEED_TH}.focus()"));
    const BUBBLE: &str = "document.getElementById('col-tooltip')";
    assert!(
        !eval_bool(&tab, &format!("{BUBBLE}.hidden")),
        "focusing the seed-given header reveals the bubble",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{BUBBLE}.querySelector('.ct-desc').textContent")
        ),
        "Seed primary key",
        "the bubble carries the SEED's authored column description",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!(
                "Array.from({BUBBLE}.querySelectorAll('.ct-key'))\
                 .map(function(k){{return k.textContent;}}).join('|')"
            ),
        ),
        "unique",
        "the column-scoped test attached to the seed rides the bubble",
    );
    let _ = eval(&tab, &format!("{SEED_TH}.blur()"));
    // Mouse path: hover reveals too.
    let _ = eval(
        &tab,
        &format!("{SEED_TH}.dispatchEvent(new MouseEvent('mouseover', {{bubbles: true}}))"),
    );
    assert!(
        !eval_bool(&tab, &format!("{BUBBLE}.hidden")),
        "hovering the seed-given header reveals the bubble",
    );
    let _ = eval(
        &tab,
        &format!("{SEED_TH}.dispatchEvent(new MouseEvent('mouseout', {{bubbles: true}}))"),
    );

    // ===== given 1 (source): the described source column decorates =====
    // cute-dbt#240 — both headers are triggers; exactly `Id` is rich.
    const SRC_SEC: &str = "document.querySelectorAll('.given-section')[1]";
    assert_eq!(
        eval(
            &tab,
            &format!("{SRC_SEC}.querySelectorAll('th.has-col-meta').length")
        )
        .as_u64(),
        Some(2),
        "every source-given header is a trigger (Id rich, FIRST fallback)",
    );
    assert_eq!(
        eval(
            &tab,
            &format!("{SRC_SEC}.querySelectorAll('th.has-col-meta:not(.col-meta-empty)').length")
        )
        .as_u64(),
        Some(1),
        "exactly the described source column is rich (Id)",
    );
    const SRC_TH: &str = "document.querySelectorAll('.given-section')[1]\
         .querySelector('th[data-col-name=\"Id\"]')";
    assert_eq!(
        eval_string(&tab, &format!("{SRC_TH}.getAttribute('data-col-name')")),
        "Id",
        "the decorated source-given header is the source's described column",
    );
    let _ = eval(&tab, &format!("{SRC_TH}.focus()"));
    assert_eq!(
        eval_string(
            &tab,
            &format!("{BUBBLE}.querySelector('.ct-desc').textContent")
        ),
        "Unique patient identifier",
        "the bubble carries the SOURCE's authored column description",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!(
                "Array.from({BUBBLE}.querySelectorAll('.ct-key'))\
                 .map(function(k){{return k.textContent;}}).join('|')"
            ),
        ),
        "not null",
        "the column-scoped test attached to the source rides the bubble",
    );
    let _ = eval(&tab, &format!("{SRC_TH}.blur()"));

    // ===== cute-dbt#240 honest fallback: metadata-less columns ARE =====
    // ===== triggers and reveal a truthful, NEVER-empty bubble.      =====
    // The old contract ("no metadata ⇒ no affordance") made these headers
    // silently hover-dead — the founder's thrice-reported defect B. The
    // honesty invariant survives strengthened: hover always answers, and
    // the bubble always carries content (never empty).
    assert!(
        eval_bool(
            &tab,
            &format!(
                "{SEED_SEC}.querySelector('th[data-col-name=\"undocumented\"]')\
                 .classList.contains('col-meta-empty')"
            ),
        ),
        "a given column without metadata is a fallback trigger (cute-dbt#240)",
    );
    let _ = eval(
        &tab,
        &format!(
            "{SEED_SEC}.querySelector('th[data-col-name=\"undocumented\"]')\
             .dispatchEvent(new MouseEvent('mouseover', {{bubbles: true}}))"
        ),
    );
    assert!(
        !eval_bool(&tab, &format!("{BUBBLE}.hidden")),
        "hovering a metadata-less given header reveals the fallback bubble",
    );
    assert!(
        eval_bool(&tab, &format!("{BUBBLE}.textContent.trim().length > 0"),),
        "the fallback bubble is never empty (the preserved honesty invariant)",
    );
    let seed_fallback = eval_string(
        &tab,
        &format!("{BUBBLE}.querySelector('.ct-empty').textContent"),
    );
    assert!(
        seed_fallback.contains("No description or data tests declared on raw_customers"),
        "the fallback names the owning seed, got {seed_fallback:?}",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn given_header_metadataless_columns_reveal_truthful_fallback() {
    // cute-dbt#240 (defect B, third report) — DELIBERATE contract reversal
    // of the #235-era "metadata-less given has no triggers" pin. A given
    // whose source genuinely lacks per-column metadata (here a seed with NO
    // declared columns, the committed jaffle-shop `raw_customers` shape)
    // was the founder's exact hover-dead surface: every header showed
    // nothing while the expected table answered. Every header is a trigger
    // now; metadata-less ones reveal the truthful fallback naming the
    // owning node. The honesty invariant survives strengthened: never an
    // empty bubble — and no dead headers either.
    let ut = UnitTest::new(
        "bare".to_owned(),
        NodeId::new("dim_x"),
        vec![UnitTestGiven::new(
            "ref('bare_seed')".to_owned(),
            serde_json::json!([{ "a": 1, "b": 2 }]),
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
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
        "headless_bare_given_no_triggers.html",
        vec![
            model_node("model.shop.dim_x"),
            seed_node("seed.shop.bare_seed"),
        ],
        vec![("unit_test.shop.dim_x.bare", ut)],
        &["model.shop.dim_x"],
        &[],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    select_model(&tab, "dim_x");

    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.given-section th.has-col-meta.col-meta-empty').length"
        )
        .as_u64(),
        Some(2),
        "a metadata-less given decorates EVERY header as a fallback trigger \
         (cute-dbt#240 — no hover-dead headers)",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.given-section th')\
         .dispatchEvent(new MouseEvent('mouseover', {bubbles: true}))",
    );
    assert!(
        !eval_bool(&tab, "document.getElementById('col-tooltip').hidden"),
        "hovering a metadata-less given header reveals the fallback bubble",
    );
    assert!(
        eval_bool(
            &tab,
            "document.getElementById('col-tooltip').textContent.trim().length > 0"
        ),
        "the bubble is never empty (the preserved honesty invariant)",
    );
    let fallback = eval_string(
        &tab,
        "document.querySelector('#col-tooltip .ct-empty').textContent",
    );
    assert!(
        fallback.contains("No description or data tests declared on bare_seed"),
        "the fallback names the owning seed, got {fallback:?}",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn model_badge_tooltip_rides_column_tooltip_chip_anatomy() {
    // cute-dbt#235 concern 2 — the model-ref tip's test rows must use
    // the COLUMN tooltip's own anatomy: .ct-tests container, .ct-test
    // rows, accent .ct-key names (the readable-on-dark color-mix form),
    // .ct-vals/.ct-val chips for key→value arguments. RED on pre-#235
    // main: the rows rendered the shelf card's .dt-row/.dt-key/.dt-val
    // dress instead (flat divergent styling).
    let ut = UnitTest::new(
        "anatomy".to_owned(),
        NodeId::new("dim_x"),
        vec![UnitTestGiven::new(
            "ref('stg_src')".to_owned(),
            serde_json::json!([{ "x": 1 }]),
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
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
        "headless_model_tip_anatomy.html",
        vec![
            model_node("model.shop.dim_x"),
            model_node("model.shop.stg_src")
                .with_model_metadata(Some("Staged source rows".to_owned()), vec![]),
            // A detail-bearing model-level test (accepted_range → the
            // "0–100" chip) + a bare one (name-only row, no chips).
            model_test_node(
                "test.shop.range_stg_src",
                "model.shop.stg_src",
                TestMetadata::new(
                    "accepted_range",
                    None,
                    serde_json::json!({ "min_value": 0, "max_value": 100 }),
                ),
            ),
            model_test_node(
                "test.shop.unique_stg_src",
                "model.shop.stg_src",
                TestMetadata::new("unique", None, serde_json::Value::Null),
            ),
        ],
        vec![("unit_test.shop.dim_x.anatomy", ut)],
        &["model.shop.dim_x"],
        &[],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    select_model(&tab, "dim_x");

    const PILL: &str =
        "document.querySelectorAll('.given-section')[0].querySelector('.table-title .gt-model')";
    let _ = eval(&tab, &format!("{PILL}.focus()"));
    const MODEL_TIP: &str = "document.getElementById('model-tooltip')";
    assert!(
        !eval_bool(&tab, &format!("{MODEL_TIP}.hidden")),
        "focusing the pill reveals the model tip",
    );

    // The shared anatomy: a .ct-tests container with one .ct-test row
    // per model test, sorted by display name.
    assert_eq!(
        eval(
            &tab,
            &format!("{MODEL_TIP}.querySelectorAll('.ct-tests .ct-test').length")
        )
        .as_u64(),
        Some(2),
        "each model test renders one .ct-test row inside .ct-tests",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!(
                "Array.from({MODEL_TIP}.querySelectorAll('.ct-key'))\
                 .map(function(k){{return k.textContent;}}).join('|')"
            ),
        ),
        "accepted range|unique",
        "test names render as .ct-key elements (sorted display names)",
    );
    // Key→value pairs: the accepted_range arguments render as the
    // column tooltip's .ct-val chips (never a parallel chip system).
    assert_eq!(
        eval_string(
            &tab,
            &format!(
                "Array.from({MODEL_TIP}.querySelectorAll('.ct-vals .ct-val'))\
                 .map(function(v){{return v.textContent;}}).join('|')"
            ),
        ),
        "0\u{2013}100",
        "the detail argument renders as a .ct-val chip",
    );
    // Accent treatment: the model tip's .ct-key computes to the
    // readable-on-dark color-mix form (compared against an in-page
    // reference element so no oklab numbers are hardcoded).
    assert!(
        eval_bool(
            &tab,
            &format!(
                "(function(){{\
                 var ref = document.createElement('span');\
                 ref.style.color = 'color-mix(in oklab, var(--accent) 60%, white)';\
                 document.body.appendChild(ref);\
                 var got = getComputedStyle({MODEL_TIP}.querySelector('.ct-key')).color;\
                 var want = getComputedStyle(ref).color;\
                 ref.remove();\
                 return got === want;}})()"
            ),
        ),
        "the model tip's test names carry the color-mix accent treatment",
    );
    // The retired dress must be gone from the tip (the shelf card keeps
    // its light-surface .dt-* chips — out of this tip's scope).
    assert_eq!(
        eval(
            &tab,
            &format!(
                "{MODEL_TIP}.querySelectorAll('.dt-row, .dt-key, .dt-val, .mt-mtests').length"
            )
        )
        .as_u64(),
        Some(0),
        "no .dt-*/.mt-mtests remnants inside the model tip",
    );

    let _ = tab.close(true);
}

// ===== cute-dbt#231 — normal-row construct chip AA contrast across every
// theme =====================================================================
//
// Pre-existing, found during #227's empirical 8-theme sweep: the construct
// code chip on NON-suppressed finding rows renders `--text-muted` on its
// own `--bg-alt` fill, and nothing gated that pairing per theme. Three
// light themes shipped under the 4.5:1 AA floor on that exact pairing —
// latte 4.06:1, rosepine 4.37:1, and solarized 4.497:1 (the same
// hair-under-floor pairing #227 deepened on suppressed rows; the issue
// named only latte+rosepine, but the verbatim solarized tokens
// #5e6e73-on-#f3ecd6 sit at 4.497 on normal rows too — hex resolves to
// exact integer RGB, so the browser paints the same ratio the token math
// gives). #227 fixed only the suppressed-row surfaces; this guard pins
// the NORMAL-row chip the same way.
//
// Same shape as the #206 tier-chip guard: for EVERY [data-theme] pack the
// chassis ships, every normal-row `.f-construct` chip must reach AA on its
// true backdrop (a 9th theme cannot silently regress). The chip's own
// `--bg-alt` fill is opaque, so the effective backdrop is the chip fill
// itself — the sweep still composites via the own-background-then-ancestor
// walk rather than assuming it (the #206/#227 lesson: assumed backdrops
// produce wrong reference numbers). The #227 mechanism pin rides along:
// no self-or-ancestor computed opacity < 1, so token math IS the painted
// contrast.

/// The normal-row construct-chip sweep, evaluated in-page. Returns a JSON
/// array of `{theme, idx, ratio, fg, bg, dimmed}` — one entry per
/// (theme, non-suppressed-row `.f-construct` chip); `dimmed` lists any
/// self-or-ancestor node whose computed opacity is below 1.
const NORMAL_ROW_CONSTRUCT_CONTRAST_SWEEP_JS: &str = r#"(function () {
  var THEMES = ["light", "solarized", "latte", "rosepine",
                "dark", "tokyo", "gruvbox", "dracula"];
  var DARK = { dark: true, tokyo: true, gruvbox: true, dracula: true };
  function parseRgb(s) {
    var m = /rgba?\(([^)]+)\)/.exec(s || "");
    if (!m) return null;
    var p = m[1].split(",");
    return { r: parseFloat(p[0]), g: parseFloat(p[1]), b: parseFloat(p[2]),
             a: p.length > 3 ? parseFloat(p[3]) : 1 };
  }
  function chan(v) {
    v = v / 255;
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  }
  function lum(c) {
    return 0.2126 * chan(c.r) + 0.7152 * chan(c.g) + 0.0722 * chan(c.b);
  }
  function ratio(f, b) {
    var lf = lum(f), lb = lum(b);
    var hi = Math.max(lf, lb), lo = Math.min(lf, lb);
    return (hi + 0.05) / (lo + 0.05);
  }
  function backdropOf(el) {
    for (var n = el; n; n = n.parentElement) {
      var c = parseRgb(getComputedStyle(n).backgroundColor);
      if (c && c.a === 1) return c;
    }
    return null; /* no opaque ancestor — surfaced as ratio -1 */
  }
  function dimmedChain(el) {
    var out = [];
    for (var n = el; n; n = n.parentElement) {
      if (parseFloat(getComputedStyle(n).opacity) < 1) {
        out.push(n.tagName.toLowerCase()
          + (n.className ? "." + String(n.className).trim().split(/\s+/).join(".") : ""));
      }
    }
    return out;
  }
  /* measurement hygiene: body backgrounds transition over 120ms, so a
     just-switched theme would otherwise be read mid-interpolation */
  var kill = document.createElement("style");
  kill.textContent = "* { transition: none !important; animation: none !important; }";
  document.head.appendChild(kill);
  var chips = document.querySelectorAll(
    ".finding-row:not(.is-suppressed) .f-construct");
  if (!chips.length) {
    throw new Error("normal-row construct sweep: no .f-construct chip on a "
      + "non-suppressed .finding-row in the DOM");
  }
  var root = document.documentElement;
  var out = [];
  for (var i = 0; i < THEMES.length; i++) {
    /* exactly theme.js applyTheme: set data-theme + sync html.dark */
    root.setAttribute("data-theme", THEMES[i]);
    root.classList.toggle("dark", !!DARK[THEMES[i]]);
    for (var j = 0; j < chips.length; j++) {
      var cs = getComputedStyle(chips[j]);
      var fg = parseRgb(cs.color);
      var own = parseRgb(cs.backgroundColor);
      var bg = own && own.a === 1 ? own : backdropOf(chips[j].parentElement);
      out.push({
        theme: THEMES[i], idx: j,
        ratio: fg && bg ? ratio(fg, bg) : -1,
        fg: cs.color,
        bg: bg ? "rgb(" + bg.r + ", " + bg.g + ", " + bg.b + ")" : "none",
        dimmed: dimmedChain(chips[j])
      });
    }
  }
  return JSON.stringify(out);
})()"#;

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn normal_row_construct_chip_meets_aa_contrast_on_every_theme() {
    // The same findings fixture as the #206 tier-chip guard: dim_both
    // trips grain + union — two NORMAL (non-suppressed) finding rows,
    // each carrying its construct code chip. No suppression policy, so
    // every row stays on the normal-row token path.
    let url = render_to_file(
        "headless_231_construct_chip_contrast.html",
        vec![findings_model("model.shop.dim_both")],
        vec![("unit_test.shop.dim_both.t1", unit_test("t1", "dim_both"))],
        &["model.shop.dim_both"],
        &["unit_test.shop.dim_both.t1"],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);

    let raw = eval_string(&tab, NORMAL_ROW_CONSTRUCT_CONTRAST_SWEEP_JS);
    let measured: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the construct-chip sweep returns valid JSON");
    assert_eq!(
        measured.len(),
        16,
        "8 themes x 2 normal-row construct chips (grain + union findings) \
         measured, got: {raw}",
    );

    let mut dim_failures = Vec::new();
    let mut aa_failures = Vec::new();
    for m in &measured {
        let theme = m["theme"].as_str().expect("theme is a string");
        let idx = m["idx"].as_u64().expect("idx is a number");
        let ratio = m["ratio"].as_f64().expect("ratio is a number");
        let fg = m["fg"].as_str().unwrap_or("?");
        let bg = m["bg"].as_str().unwrap_or("?");
        let dimmed = m["dimmed"].as_array().expect("dimmed is an array");
        assert!(
            ratio > 0.0,
            "the {theme}/chip[{idx}] text resolved no opaque backdrop — the \
             backdrop walk must end on a painted surface",
        );
        eprintln!(
            "normal-row construct contrast {theme:>9} / chip[{idx}] = {ratio:.2}  ({fg} on {bg})"
        );
        if !dimmed.is_empty() {
            dim_failures.push(format!("{theme}/chip[{idx}] dimmed by {dimmed:?}"));
        }
        if ratio < 4.5 {
            aa_failures.push(format!("{theme}/chip[{idx}] = {ratio:.2} ({fg} on {bg})"));
        }
    }
    assert!(
        dim_failures.is_empty(),
        "normal-row construct chip sits under an opacity-dimmed ancestor — \
         token math no longer equals effective contrast: {dim_failures:#?}",
    );
    assert!(
        aa_failures.is_empty(),
        "normal-row construct chips below the WCAG AA 4.5:1 floor \
         (cute-dbt#231): {aa_failures:#?}",
    );

    let _ = tab.close(true);
}

// ===== cute-dbt#240 — founder review-2 render defects =================
//
// Five defects found reviewing the PR #239 sticky-comment reports. The
// guards below encode the REAL interaction sequences the prior (#166 /
// #236) validations never exercised — that meta-gap is why those fixes
// passed while the founder's experience stayed broken:
//
//  - defect B guards quantified EXISTENTIALLY ("this column known to
//    carry metadata reveals a bubble") — true before and after every
//    prior fix — while the founder hovered columns the manifest never
//    declared (hover-dead by design). The #240 guard quantifies
//    UNIVERSALLY: across EVERY given/expected header, in the Diff AND
//    File views, after a real view toggle and a real column sort, a
//    hover must reveal a non-empty bubble. The dead-header state no
//    longer exists, so the regression class cannot recur silently.
//  - defects A/D pin geometric CONTAINMENT (content inside the painted
//    bubble, bubble inside the viewport) rather than mere visibility.
//  - defects C/E extend the #206/#227/#231/#233 AA family (effective-
//    backdrop methodology) to the covered-by ids and the baseline path.

/// The universal hover sweep, evaluated in-page: for every VISIBLE
/// given/expected header, dispatch a real mouseover, assert the singleton
/// bubble reveals with non-empty text, then mouseout. Returns a JSON
/// array of failure strings (empty = pass).
const UNIVERSAL_HEADER_HOVER_SWEEP_JS: &str = r#"(function () {
  var fails = [];
  var ths = document.querySelectorAll('.given-section th, .expected-panel th');
  if (!ths.length) return JSON.stringify(["no fixture headers found"]);
  for (var i = 0; i < ths.length; i++) {
    var th = ths[i];
    if (th.offsetParent === null) continue; /* hidden view's twin table */
    var name = th.getAttribute('data-col-name') || th.textContent;
    if (!th.classList.contains('has-col-meta')) {
      fails.push('no-trigger: ' + name);
      continue;
    }
    th.dispatchEvent(new MouseEvent('mouseover', {bubbles: true}));
    var b = document.getElementById('col-tooltip');
    if (!b || b.hidden) {
      fails.push('hover-dead: ' + name);
    } else if (!b.textContent.trim()) {
      fails.push('empty-bubble: ' + name);
    }
    th.dispatchEvent(new MouseEvent('mouseout', {bubbles: true}));
  }
  return JSON.stringify(fails);
})()"#;

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn fixture_header_tooltips_universal_after_view_toggle_and_sort() {
    // cute-dbt#240 (defect B) — the guard-methodology fix. The fixture
    // mirrors the founder's real surface: an input model that declares
    // only ONE of its two given columns (real projects under-declare
    // staging models) and a target model that declares only one expected
    // column. The given carries a cell diff so the Diff↔File toggle is
    // live — the prior probes hovered only the initial render.
    use cute_dbt::domain::{
        Cell, CellChange, CellValue, ColumnStatus, DiffColumn, FixtureTableDiff, NamedTableDiff,
        RowChange, RowChangeKind,
    };
    let mut src_desc = BTreeMap::new();
    src_desc.insert("described_col".to_owned(), "Input described".to_owned());
    let mut target_desc = BTreeMap::new();
    target_desc.insert("id".to_owned(), "Target id".to_owned());
    let ut = UnitTest::new(
        "tog".to_owned(),
        NodeId::new("dim_t"),
        vec![UnitTestGiven::new(
            "ref('src')".to_owned(),
            serde_json::json!([{ "described_col": 1, "undescribed_col": 2 }]),
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1, "extra": 2 }]),
            Some("dict".to_owned()),
            None,
        ),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    );
    let data_diff = UnitTestDataDiff {
        given: vec![NamedTableDiff {
            ordinal: 0,
            input: "ref('src')".to_owned(),
            diff: FixtureTableDiff {
                columns: vec![
                    DiffColumn {
                        name: "described_col".into(),
                        status: ColumnStatus::Present,
                    },
                    DiffColumn {
                        name: "undescribed_col".into(),
                        status: ColumnStatus::Present,
                    },
                ],
                rows: vec![RowChange {
                    kind: RowChangeKind::Modified,
                    cells: vec![
                        CellChange {
                            old: Cell::new(CellValue::Number("1".into())),
                            new: Cell::new(CellValue::Number("9".into())),
                            changed: true,
                        },
                        CellChange {
                            old: Cell::new(CellValue::Number("2".into())),
                            new: Cell::new(CellValue::Number("2".into())),
                            changed: false,
                        },
                    ],
                }],
            },
        }],
        expect: None,
    };
    let url = render_pr_diff_with_data_diffs(
        "headless_240_universal_header_hover.html",
        vec![
            model_node("model.shop.dim_t").with_column_descriptions(target_desc),
            model_node("model.shop.src").with_column_descriptions(src_desc),
        ],
        vec![("unit_test.shop.dim_t.tog", ut)],
        &["model.shop.dim_t"],
        &["unit_test.shop.dim_t.tog"],
        vec![("unit_test.shop.dim_t.tog", data_diff)],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    select_model(&tab, "dim_t");

    // ===== 1. initial render (the given defaults to the Diff view) =====
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.given-section .cell-diff-toggle') !== null",
        ),
        "precondition: the given carries a live Diff↔File toggle",
    );
    let initial = eval_string(&tab, UNIVERSAL_HEADER_HOVER_SWEEP_JS);
    assert_eq!(
        initial, "[]",
        "EVERY visible fixture header reveals a non-empty bubble on hover \
         (initial render): {initial}",
    );

    // ===== 2. after a REAL view toggle (Diff → File) =====
    let _ = eval(
        &tab,
        "document.querySelector('.given-section .cell-diff-toggle \
         [data-view=\"current\"]').click()",
    );
    let after_toggle = eval_string(&tab, UNIVERSAL_HEADER_HOVER_SWEEP_JS);
    assert_eq!(
        after_toggle, "[]",
        "EVERY visible fixture header reveals a non-empty bubble on hover \
         AFTER the Diff→File toggle (the #145/#146 wipe class): {after_toggle}",
    );

    // ===== 3. after a REAL column sort (DataTables redraw) =====
    let _ = eval(
        &tab,
        "(function(){var ths=document.querySelectorAll('.given-section th');\
          for (var i=0;i<ths.length;i++){\
            if (ths[i].offsetParent !== null){ ths[i].click(); return; }}})()",
    );
    let after_sort = eval_string(&tab, UNIVERSAL_HEADER_HOVER_SWEEP_JS);
    assert_eq!(
        after_sort, "[]",
        "EVERY visible fixture header reveals a non-empty bubble on hover \
         AFTER a column sort: {after_sort}",
    );

    // ===== 4. and back to the Diff view again =====
    let _ = eval(
        &tab,
        "document.querySelector('.given-section .cell-diff-toggle \
         [data-view=\"diff\"]').click()",
    );
    let round_trip = eval_string(&tab, UNIVERSAL_HEADER_HOVER_SWEEP_JS);
    assert_eq!(
        round_trip, "[]",
        "EVERY visible fixture header reveals a non-empty bubble on hover \
         after the File→Diff round-trip: {round_trip}",
    );

    // ===== fallback truthfulness on the undeclared given column =====
    let _ = eval(
        &tab,
        "(function(){var ths=document.querySelectorAll(\
          '.given-section th[data-col-name=\"undescribed_col\"]');\
          for (var i=0;i<ths.length;i++){\
            if (ths[i].offsetParent !== null){\
              ths[i].dispatchEvent(new MouseEvent('mouseover',{bubbles:true}));\
              return; }}})()",
    );
    let fallback = eval_string(
        &tab,
        "document.querySelector('#col-tooltip .ct-empty').textContent",
    );
    assert!(
        fallback.contains("No description or data tests declared on src"),
        "the undeclared given column's bubble names the input model, got {fallback:?}",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn model_tip_long_test_name_wraps_inside_bubble() {
    // cute-dbt#240 (defect A) — the founder's exact overflow: the model
    // badge tip's MODEL TESTS row carried the unbreakable mono token
    // `dbt_expectations.expect_table_row_count_to_be_between` (383px)
    // inside the 32rem-capped bubble — it painted 55px past the painted
    // background. RED pre-fix; the wrap rules contain it.
    let ut = UnitTest::new(
        "wrap".to_owned(),
        NodeId::new("dim_x"),
        vec![UnitTestGiven::new(
            "ref('stg_src')".to_owned(),
            serde_json::json!([{ "id": 1 }]),
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
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
        "headless_240_model_tip_wrap.html",
        vec![
            model_node("model.shop.dim_x"),
            model_node("model.shop.stg_src").with_model_metadata(
                Some("Staging model for prescribed medications.".to_owned()),
                vec![],
            ),
            model_test_node(
                "test.shop.dbt_expectations_row_count_stg_src",
                "model.shop.stg_src",
                TestMetadata::new(
                    "dbt_expectations.expect_table_row_count_to_be_between",
                    None,
                    serde_json::Value::Null,
                ),
            ),
        ],
        vec![("unit_test.shop.dim_x.wrap", ut)],
        &["model.shop.dim_x"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    select_model(&tab, "dim_x");

    const PILL: &str = "document.querySelector('.given-section .gt-model.has-model-tip')";
    let _ = eval(
        &tab,
        &format!("{PILL}.dispatchEvent(new MouseEvent('mouseover', {{bubbles: true}}))"),
    );
    const TIP: &str = "document.getElementById('model-tooltip')";
    assert!(
        !eval_bool(&tab, &format!("{TIP}.hidden")),
        "hovering the model pill reveals the model tip",
    );
    let key = eval_string(&tab, &format!("{TIP}.querySelector('.ct-key').textContent"));
    assert!(
        key.contains("expect_table_row_count_to_be_between"),
        "precondition: the long mono test name rides the tip, got {key:?}",
    );
    // The containment pin (1px tolerance, the repo convention): the
    // bubble's content never paints past its own painted background.
    assert!(
        eval_bool(&tab, &format!("{TIP}.scrollWidth <= {TIP}.clientWidth + 1"),),
        "the model tip CONTAINS its content (no horizontal overflow past \
         the painted background)",
    );
    assert!(
        eval_bool(
            &tab,
            &format!(
                "(function(){{var t={TIP};var tr=t.getBoundingClientRect();\
                 var els=t.querySelectorAll('*');\
                 for (var i=0;i<els.length;i++){{\
                   if (els[i].getBoundingClientRect().right > tr.right + 1) return false;}}\
                 return true;}})()"
            ),
        ),
        "no tip descendant paints past the bubble's right edge",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn ov_tip_long_value_contained_in_bubble_and_viewport_at_right_edge() {
    // cute-dbt#240 (defect D) — the `overrides · N` badge tip near the
    // right viewport edge. Two pins:
    //  1. content containment: a long override value (the nowrap rows
    //     painted it past the bubble background and past the screen edge)
    //     must wrap inside the painted bubble — RED pre-fix;
    //  2. viewport containment at a REAL right-edge trigger geometry:
    //     the bubble box stays fully inside the visible viewport (the
    //     positionTipNear clamp, now scrollbar-safe via clientWidth).
    let mut ov = cute_dbt::domain::UnitTestOverrides::new();
    ov.insert(
        "env_vars".to_owned(),
        BTreeMap::from([(
            "DBT_DEPLOY_TARGET_URL".to_owned(),
            serde_json::json!(
                "https://very-long-subdomain.example-warehouse-host.internal/projects/analytics/deployments/segment"
            ),
        )]),
    );
    let ut = UnitTest::new(
        "ovclip".to_owned(),
        NodeId::new("dim_x"),
        vec![UnitTestGiven::new(
            "ref('src')".to_owned(),
            serde_json::json!([{ "id": 1 }]),
            Some("dict".to_owned()),
            None,
        )],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
            Some("dict".to_owned()),
            None,
        ),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    )
    .with_overrides(Some(ov));
    let url = render_to_file(
        "headless_240_ov_tip_clip.html",
        vec![model_node("model.shop.dim_x"), model_node("model.shop.src")],
        vec![("unit_test.shop.dim_x.ovclip", ut)],
        &["model.shop.dim_x"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    select_model(&tab, "dim_x");

    const BADGE: &str = "document.querySelector('.tb-overrides.has-ov-tip')";
    assert!(
        eval_bool(&tab, &format!("{BADGE} !== null")),
        "precondition: the overrides badge renders",
    );
    // Park the trigger at a REAL right-edge geometry (the tier-chip
    // sweep's in-page-mutation precedent): the singleton positions off the
    // trigger's live rect, so this exercises the exact founder geometry.
    let _ = eval(
        &tab,
        &format!(
            "(function(){{var b={BADGE};b.style.position='fixed';\
             b.style.right='2px';b.style.left='auto';b.style.top='60px';}})()"
        ),
    );
    let _ = eval(
        &tab,
        &format!("{BADGE}.dispatchEvent(new MouseEvent('mouseover', {{bubbles: true}}))"),
    );
    const TIP: &str = "document.getElementById('ov-tooltip')";
    assert!(
        !eval_bool(&tab, &format!("{TIP}.hidden")),
        "hovering the right-edge overrides badge reveals the tip",
    );
    // Pin 1 — content containment (RED pre-fix: nowrap rows overflowed).
    assert!(
        eval_bool(&tab, &format!("{TIP}.scrollWidth <= {TIP}.clientWidth + 1"),),
        "the overrides tip CONTAINS its content (the long value wraps \
         inside the painted background instead of painting past it)",
    );
    // Pin 2 — viewport containment at the right-edge trigger. The top
    // bound also pins the PR #244-review post-flip top clamp (the full
    // pathological-viewport-height pin rides cute-dbt#246).
    assert!(
        eval_bool(
            &tab,
            &format!(
                "(function(){{var r={TIP}.getBoundingClientRect();\
                 var vw=document.documentElement.clientWidth;\
                 return r.top >= 0 && r.left >= 0 && r.right <= vw + 1;}})()"
            ),
        ),
        "the overrides bubble's box lies fully within the visible viewport \
         at a right-edge trigger",
    );
    assert!(
        eval_bool(
            &tab,
            &format!(
                "(function(){{var t={TIP};var tr=t.getBoundingClientRect();\
                 var vw=document.documentElement.clientWidth;\
                 var els=t.querySelectorAll('*');\
                 for (var i=0;i<els.length;i++){{\
                   var r=els[i].getBoundingClientRect();\
                   if (r.right > tr.right + 1 || r.right > vw + 1) return false;}}\
                 return true;}})()"
            ),
        ),
        "no overrides-tip descendant paints past the bubble or the viewport",
    );

    let _ = tab.close(true);
}

/// cute-dbt#240 (defect C) — `.finding-covered-by code` AA sweep across
/// all 8 themes. Sakura's `code {{ background:#f1f1f1 }}` paired with the
/// near-white dark-theme `--text` at ≈1.2:1 (the founder's "near-white
/// pills"). Effective-backdrop methodology: measure against the code
/// chip's OWN resolved fill composited over its ancestors — never an
/// assumed token (the #206/#227 lesson).
const COVERED_BY_CONTRAST_SWEEP_JS: &str = r#"(function () {
  var THEMES = ["light", "solarized", "latte", "rosepine",
                "dark", "tokyo", "gruvbox", "dracula"];
  var DARK = { dark: true, tokyo: true, gruvbox: true, dracula: true };
  function parseRgb(s) {
    var m = /rgba?\(([^)]+)\)/.exec(s || "");
    if (!m) return null;
    var p = m[1].split(",");
    return { r: parseFloat(p[0]), g: parseFloat(p[1]), b: parseFloat(p[2]),
             a: p.length > 3 ? parseFloat(p[3]) : 1 };
  }
  function chan(v) {
    v = v / 255;
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  }
  function lum(c) {
    return 0.2126 * chan(c.r) + 0.7152 * chan(c.g) + 0.0722 * chan(c.b);
  }
  function ratio(f, b) {
    var lf = lum(f), lb = lum(b);
    var hi = Math.max(lf, lb), lo = Math.min(lf, lb);
    return (hi + 0.05) / (lo + 0.05);
  }
  function backdropOf(el) {
    for (var n = el; n; n = n.parentElement) {
      var c = parseRgb(getComputedStyle(n).backgroundColor);
      if (c && c.a === 1) return c;
    }
    return null;
  }
  /* PR #244 verification — the body carries `transition: background
     120ms ease, color 120ms ease`, so an instant setAttribute +
     getComputedStyle sweep can read MID-TRANSITION values (false
     readings observed by the independent verifier). Kill every
     transition for the duration of the sweep so each theme flip
     resolves to its final painted colors synchronously. */
  var kill = document.createElement("style");
  kill.textContent = "* { transition: none !important; }";
  document.head.appendChild(kill);
  /* EVERY text run in the quoted line (the existential-vs-universal
     lesson applies to AA sweeps too): the code pills AND the
     "Covered by " prefix label. */
  var els = document.querySelectorAll(
    ".finding-covered-by code, .finding-covered-by .f-label");
  if (!els.length) { kill.remove(); return JSON.stringify([]); }
  var root = document.documentElement;
  var out = [];
  for (var i = 0; i < THEMES.length; i++) {
    root.setAttribute("data-theme", THEMES[i]);
    root.classList.toggle("dark", !!DARK[THEMES[i]]);
    for (var j = 0; j < els.length; j++) {
      var cs = getComputedStyle(els[j]);
      var fg = parseRgb(cs.color);
      var own = parseRgb(cs.backgroundColor);
      var bg = own && own.a === 1 ? own : backdropOf(els[j].parentElement);
      out.push({
        theme: THEMES[i], idx: j,
        el: els[j].tagName === "CODE" ? "code" : "f-label",
        ratio: fg && bg ? ratio(fg, bg) : -1,
        fg: cs.color,
        bg: bg ? "rgb(" + bg.r + ", " + bg.g + ", " + bg.b + ")" : "none"
      });
    }
  }
  kill.remove();
  return JSON.stringify(out);
})()"#;

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn covered_by_test_ids_meet_aa_contrast_on_every_theme() {
    // A COVERED union finding through the real check path: dim_both's two
    // union arms (src_a, src_b) are each fed rows by the unit test's
    // givens, so the union check lands Verdict::Covered and the panel
    // renders the "Covered by <ids>" row whose code chips defect C reads.
    let ut = UnitTest::new(
        "feeds".to_owned(),
        NodeId::new("dim_both"),
        vec![
            UnitTestGiven::new(
                "ref('src_a')".to_owned(),
                serde_json::json!([{ "id": 1 }]),
                Some("dict".to_owned()),
                None,
            ),
            UnitTestGiven::new(
                "ref('src_b')".to_owned(),
                serde_json::json!([{ "id": 2 }]),
                Some("dict".to_owned()),
                None,
            ),
        ],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
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
        "headless_240_covered_by_contrast.html",
        vec![
            findings_model("model.shop.dim_both"),
            model_node("model.shop.src_a"),
            model_node("model.shop.src_b"),
        ],
        vec![("unit_test.shop.dim_both.feeds", ut)],
        &["model.shop.dim_both"],
        &["unit_test.shop.dim_both.feeds"],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);
    select_model(&tab, "dim_both");

    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.finding-covered-by code') !== null",
        ),
        "precondition: a COVERED finding renders its covered-by test ids \
         (both union arms are fed by the test's givens)",
    );
    let raw = eval_string(&tab, COVERED_BY_CONTRAST_SWEEP_JS);
    let measured: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the contrast sweep returns valid JSON");
    // 8 themes × (≥2 code pills + 1 "Covered by " f-label) — EVERY text
    // run in the quoted line is measured (PR #244 verification residual:
    // the latte f-label sat at 4.37:1 while the pills passed).
    assert!(
        measured.len() >= 16,
        "every covered-by text run (code pills + the f-label prefix) \
         measured on each of the 8 themes, got: {raw}",
    );
    assert!(
        measured.iter().any(|m| m["el"].as_str() == Some("f-label")),
        "the sweep covers the 'Covered by ' prefix label run, got: {raw}",
    );
    let mut failures = Vec::new();
    for m in &measured {
        let theme = m["theme"].as_str().expect("theme is a string");
        let idx = m["idx"].as_u64().unwrap_or(0);
        let el = m["el"].as_str().unwrap_or("?");
        let ratio = m["ratio"].as_f64().expect("ratio is a number");
        let fg = m["fg"].as_str().unwrap_or("?");
        let bg = m["bg"].as_str().unwrap_or("?");
        assert!(
            ratio > 0.0,
            "the {theme}/{el}[{idx}] run resolved no opaque backdrop — the \
             backdrop walk must end on a painted surface",
        );
        eprintln!("covered-by contrast {theme:>9} / {el}[{idx}] = {ratio:.2}  ({fg} on {bg})");
        if ratio < 4.5 {
            failures.push(format!("{theme}/{el}[{idx}] = {ratio:.2} ({fg} on {bg})"));
        }
    }
    assert!(
        failures.is_empty(),
        "covered-by line text below the WCAG AA 4.5:1 floor (cute-dbt#240 \
         defect C): {failures:#?}",
    );

    let _ = tab.close(true);
}

/// cute-dbt#240 (defect E) — the baseline-manifest path in the scope
/// banner (`code.diff-scope-baseline`), same Sakura-light-code-bg root
/// cause as defect C, same effective-backdrop sweep, all 8 themes.
const BASELINE_PATH_CONTRAST_SWEEP_JS: &str = r#"(function () {
  var THEMES = ["light", "solarized", "latte", "rosepine",
                "dark", "tokyo", "gruvbox", "dracula"];
  var DARK = { dark: true, tokyo: true, gruvbox: true, dracula: true };
  function parseRgb(s) {
    var m = /rgba?\(([^)]+)\)/.exec(s || "");
    if (!m) return null;
    var p = m[1].split(",");
    return { r: parseFloat(p[0]), g: parseFloat(p[1]), b: parseFloat(p[2]),
             a: p.length > 3 ? parseFloat(p[3]) : 1 };
  }
  function chan(v) {
    v = v / 255;
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  }
  function lum(c) {
    return 0.2126 * chan(c.r) + 0.7152 * chan(c.g) + 0.0722 * chan(c.b);
  }
  function ratio(f, b) {
    var lf = lum(f), lb = lum(b);
    var hi = Math.max(lf, lb), lo = Math.min(lf, lb);
    return (hi + 0.05) / (lo + 0.05);
  }
  function backdropOf(el) {
    for (var n = el; n; n = n.parentElement) {
      var c = parseRgb(getComputedStyle(n).backgroundColor);
      if (c && c.a === 1) return c;
    }
    return null;
  }
  /* PR #244 verification — kill transitions for the sweep's duration:
     body transitions background/color over 120ms, so an instant
     setAttribute + getComputedStyle read can land mid-transition. */
  var kill = document.createElement("style");
  kill.textContent = "* { transition: none !important; }";
  document.head.appendChild(kill);
  var code = document.querySelector("code.diff-scope-baseline");
  if (!code) { kill.remove(); return JSON.stringify([]); }
  var root = document.documentElement;
  var out = [];
  for (var i = 0; i < THEMES.length; i++) {
    root.setAttribute("data-theme", THEMES[i]);
    root.classList.toggle("dark", !!DARK[THEMES[i]]);
    var cs = getComputedStyle(code);
    var fg = parseRgb(cs.color);
    var own = parseRgb(cs.backgroundColor);
    var bg = own && own.a === 1 ? own : backdropOf(code.parentElement);
    out.push({
      theme: THEMES[i],
      ratio: fg && bg ? ratio(fg, bg) : -1,
      fg: cs.color,
      bg: bg ? "rgb(" + bg.r + ", " + bg.g + ", " + bg.b + ")" : "none"
    });
  }
  kill.remove();
  return JSON.stringify(out);
})()"#;

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn baseline_banner_path_meets_aa_contrast_on_every_theme() {
    let url = render_to_file(
        "headless_240_baseline_banner_contrast.html",
        vec![model_node("model.shop.dim_x")],
        vec![("unit_test.shop.dim_x.t", unit_test("t", "dim_x"))],
        &["model.shop.dim_x"],
        &["unit_test.shop.dim_x.t"],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);

    assert!(
        eval_bool(
            &tab,
            "document.querySelector('code.diff-scope-baseline') !== null",
        ),
        "precondition: the baseline-mode banner names the baseline manifest",
    );
    let raw = eval_string(&tab, BASELINE_PATH_CONTRAST_SWEEP_JS);
    let measured: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the contrast sweep returns valid JSON");
    assert_eq!(
        measured.len(),
        8,
        "the baseline path chip measured on each of the 8 themes, got: {raw}",
    );
    let mut failures = Vec::new();
    for m in &measured {
        let theme = m["theme"].as_str().expect("theme is a string");
        let ratio = m["ratio"].as_f64().expect("ratio is a number");
        let fg = m["fg"].as_str().unwrap_or("?");
        let bg = m["bg"].as_str().unwrap_or("?");
        assert!(
            ratio > 0.0,
            "the {theme} baseline path resolved no opaque backdrop — the \
             backdrop walk must end on a painted surface",
        );
        eprintln!("baseline-path contrast {theme:>9} = {ratio:.2}  ({fg} on {bg})");
        if ratio < 4.5 {
            failures.push(format!("{theme} = {ratio:.2} ({fg} on {bg})"));
        }
    }
    assert!(
        failures.is_empty(),
        "the baseline-manifest path below the WCAG AA 4.5:1 floor \
         (cute-dbt#240 defect E): {failures:#?}",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn given_owner_label_accepts_double_quoted_ref_and_source() {
    // cute-dbt#240 (PR #244 review) — dbt accepts BOTH quote styles in a
    // given's `input:`, and dbt-fusion ships the authored string verbatim
    // (a `ref("stg_payments")` given compiles onto the manifest wire
    // double-quoted, unnormalized — verified against a real fusion
    // 2.0-preview.177 compile). The owner-label parser behind the
    // metadata-less fallback bubble must therefore name the node for
    // double-quoted ref(...) and source(...) inputs too, not degrade to
    // echoing the raw input string.
    let source = SourceNode::new(
        NodeId::new("source.shop.raw.patients"),
        "raw",
        "patients",
        None,
        "main",
        None,
        None,
    );
    let ut = UnitTest::new(
        "dq".to_owned(),
        NodeId::new("dim_x"),
        vec![
            UnitTestGiven::new(
                "ref(\"bare_seed\")".to_owned(),
                serde_json::json!([{ "a": 1 }]),
                Some("dict".to_owned()),
                None,
            ),
            UnitTestGiven::new(
                "source(\"raw\", \"patients\")".to_owned(),
                serde_json::json!([{ "Id": "x-1" }]),
                Some("dict".to_owned()),
                None,
            ),
        ],
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
            Some("dict".to_owned()),
            None,
        ),
        None,
        DependsOn::default(),
        None,
        None,
        None,
    );
    let url = render_with_sources_to_file(
        "headless_240_double_quoted_owner.html",
        vec![
            model_node("model.shop.dim_x"),
            seed_node("seed.shop.bare_seed"),
        ],
        vec![source],
        vec![("unit_test.shop.dim_x.dq", ut)],
        &["model.shop.dim_x"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    select_model(&tab, "dim_x");

    // Given 0 — ref("bare_seed"): the fallback names the bare node, not
    // the raw `ref("bare_seed")` string.
    let _ = eval(
        &tab,
        "document.querySelectorAll('.given-section')[0].querySelector('th')\
         .dispatchEvent(new MouseEvent('mouseover', {bubbles: true}))",
    );
    let ref_fallback = eval_string(
        &tab,
        "document.querySelector('#col-tooltip .ct-empty').textContent",
    );
    assert!(
        ref_fallback.contains("No description or data tests declared on bare_seed"),
        "a double-quoted ref given's fallback names the bare node, got {ref_fallback:?}",
    );
    let _ = eval(
        &tab,
        "document.querySelectorAll('.given-section')[0].querySelector('th')\
         .dispatchEvent(new MouseEvent('mouseout', {bubbles: true}))",
    );

    // Given 1 — source("raw", "patients"): the fallback names source.table.
    let _ = eval(
        &tab,
        "document.querySelectorAll('.given-section')[1].querySelector('th')\
         .dispatchEvent(new MouseEvent('mouseover', {bubbles: true}))",
    );
    let src_fallback = eval_string(
        &tab,
        "document.querySelector('#col-tooltip .ct-empty').textContent",
    );
    assert!(
        src_fallback.contains("No description or data tests declared on raw.patients"),
        "a double-quoted source given's fallback names source.table, got {src_fallback:?}",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn whitespace_only_description_degrades_to_truthful_fallback() {
    // cute-dbt#240 (PR #244 review) — a whitespace-only authored
    // description ("   ") must NOT count as column metadata: untrimmed it
    // passed the hasMeta gate, suppressed the truthful fallback, and the
    // bubble led with an escaped-whitespace .ct-desc — effectively an
    // empty bubble on a description-only column, the exact state the
    // never-empty-bubble contract forbids. The trim happens once in
    // decorateColHeader (the single writer of data-col-desc).
    let mut ws_desc = BTreeMap::new();
    ws_desc.insert("id".to_owned(), "   ".to_owned());
    let ut = UnitTest::new(
        "ws".to_owned(),
        NodeId::new("dim_ws"),
        Vec::new(),
        UnitTestExpect::new(
            serde_json::json!([{ "id": 1 }]),
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
        "headless_240_ws_desc_fallback.html",
        vec![model_node("model.shop.dim_ws").with_column_descriptions(ws_desc)],
        vec![("unit_test.shop.dim_ws.ws", ut)],
        &["model.shop.dim_ws"],
        &[],
    );
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    select_model(&tab, "dim_ws");

    const TH: &str = "document.querySelector('.expected-panel th[data-col-name=\"id\"]')";
    assert!(
        eval_bool(&tab, &format!("{TH}.classList.contains('col-meta-empty')"),),
        "a whitespace-only description does not count as metadata — the \
         header carries the fallback marker",
    );
    let _ = eval(
        &tab,
        &format!("{TH}.dispatchEvent(new MouseEvent('mouseover', {{bubbles: true}}))"),
    );
    const BUBBLE: &str = "document.getElementById('col-tooltip')";
    assert!(
        !eval_bool(&tab, &format!("{BUBBLE}.hidden")),
        "hovering the whitespace-described header reveals the bubble",
    );
    assert_eq!(
        eval_string(
            &tab,
            &format!("{BUBBLE}.querySelector('.ct-desc').textContent")
        ),
        "id",
        "the bubble leads with the column NAME, never the whitespace run",
    );
    let fallback = eval_string(
        &tab,
        &format!("{BUBBLE}.querySelector('.ct-empty').textContent"),
    );
    assert!(
        fallback.contains("No description or data tests declared on dim_ws"),
        "the truthful fallback renders for the whitespace-described column, \
         got {fallback:?}",
    );

    let _ = tab.close(true);
}

// ---------------------------------------------------------------------
// cute-dbt#247 — the Model YAML section (peer of Model SQL): the model's
// authored schema-file `models:` entry with File/Diff views, and the
// truthful degrade placeholder when the block could not be surfaced.
// ---------------------------------------------------------------------

use cute_dbt::domain::ModelYamlOutcome;

/// Render with a `model_yaml` gather map (cute-dbt#247) under `scope`.
fn render_with_model_yaml(
    filename: &str,
    nodes: Vec<Node>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    changed_ids: &[&str],
    model_yaml: Vec<(&str, ModelYamlOutcome)>,
    scope: ScopeSource,
) -> String {
    let all_ids: Vec<String> = tests.iter().map(|(id, _)| (*id).to_owned()).collect();
    let m = manifest(nodes, tests);
    let in_scope: InScopeSet = all_ids.into_iter().collect();
    let models: ModelInScopeSet = model_ids.iter().map(|id| NodeId::new(*id)).collect();
    let changed: InScopeSet = changed_ids.iter().map(|s| (*s).to_owned()).collect();
    let model_yaml: HashMap<String, ModelYamlOutcome> = model_yaml
        .into_iter()
        .map(|(id, o)| (id.to_owned(), o))
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
        &model_yaml,
        &HashMap::new(),
        "baseline.json",
        scope,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let p = out.to_str().expect("report path is valid UTF-8");
    format!("file://{p}")
}

/// A Found outcome over a one-entry schema block for `name`.
fn found_model_yaml(name: &str, path: &str) -> ModelYamlOutcome {
    let raw = format!("  - name: {name}\n    description: demo model {name}");
    ModelYamlOutcome::Found {
        path: path.to_owned(),
        block: UnitTestYamlBlock::new(raw, 2, 2, 3),
        diff: None,
    }
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn model_yaml_section_shows_block_or_truthful_placeholder_for_every_model() {
    // Three models spanning the outcome arms: a sliced block, a missing
    // schema file, and a model with no schema entry at all. The #240
    // lesson — assert over EVERY model in the picker (universal, not the
    // one known-good case): for each selection the section must show
    // either real code or a non-empty truthful placeholder, never an
    // empty or hidden card.
    let url = render_with_model_yaml(
        "headless_model_yaml_universal.html",
        vec![
            model_node("model.shop.dim_a"),
            model_node("model.shop.dim_b"),
            model_node("model.shop.dim_c"),
        ],
        vec![
            ("unit_test.shop.dim_a.t1", unit_test("t1", "dim_a")),
            ("unit_test.shop.dim_b.t2", unit_test("t2", "dim_b")),
            ("unit_test.shop.dim_c.t3", unit_test("t3", "dim_c")),
        ],
        &["model.shop.dim_a", "model.shop.dim_b", "model.shop.dim_c"],
        &[],
        vec![
            (
                "model.shop.dim_a",
                found_model_yaml("dim_a", "models/schema.yml"),
            ),
            (
                "model.shop.dim_b",
                ModelYamlOutcome::FileMissing {
                    path: "models/missing.yml".to_owned(),
                },
            ),
            ("model.shop.dim_c", ModelYamlOutcome::NoPatchPath),
        ],
        ScopeSource::Baseline,
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // Universal sweep: every model in the picker renders a visible
    // section carrying either code or a non-empty placeholder.
    for model in ["dim_a", "dim_b", "dim_c"] {
        select_model(&tab, model);
        assert!(
            !eval_bool(
                &tab,
                "getComputedStyle(document.querySelector('.model-yaml')).display === 'none'"
            ),
            "the Model YAML section is visible for {model}",
        );
        assert!(
            eval_bool(
                &tab,
                "(function(){var c=document.querySelector('.model-yaml .model-yaml-code');\
                   var p=document.querySelector('.model-yaml .model-yaml-missing');\
                   return !!(c && c.textContent.trim()) || !!(p && p.textContent.trim());})()"
            ),
            "the section carries code or a truthful placeholder for {model} — never empty",
        );
        assert!(
            eval_string(
                &tab,
                "document.querySelector('.model-yaml > details > summary').textContent"
            )
            .contains("Model YAML"),
            "the section summary is labeled 'Model YAML' for {model}",
        );
    }

    // dim_a: the sliced block renders in the File view with the schema
    // file path in the code-card header.
    select_model(&tab, "dim_a");
    let code = eval_string(
        &tab,
        "document.querySelector('.model-yaml .model-yaml-code').textContent",
    );
    assert!(
        code.contains("- name: dim_a") && code.contains("description: demo model dim_a"),
        "the authored block renders verbatim; got {code:?}",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-yaml .code-header .code-filename').textContent"
        ),
        "models/schema.yml",
        "the code-card header names the schema file",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-yaml-summary-hint').textContent"
        ),
        "authored schema entry",
        "the summary hint reads 'authored schema entry' on the plain File view",
    );

    // dim_b: missing schema file → the placeholder names the file and the
    // failure, and no code block renders.
    select_model(&tab, "dim_b");
    let placeholder = eval_string(
        &tab,
        "document.querySelector('.model-yaml .model-yaml-missing').textContent",
    );
    assert!(
        placeholder.contains("models/missing.yml") && placeholder.contains("not found"),
        "the placeholder names the missing file; got {placeholder:?}",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-yaml .model-yaml-code') === null"
        ),
        "no code block renders for a missing schema file",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-yaml-summary-hint').textContent"
        ),
        "unavailable",
        "the summary hint reads 'unavailable' on the degrade view",
    );

    // dim_c: no schema entry in the manifest → the placeholder says so.
    select_model(&tab, "dim_c");
    let placeholder = eval_string(
        &tab,
        "document.querySelector('.model-yaml .model-yaml-missing').textContent",
    );
    assert!(
        placeholder.contains("No schema file declares this model"),
        "the placeholder names the absent schema entry; got {placeholder:?}",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn model_yaml_diff_defaults_to_diff_and_file_toggle_switches_views() {
    // PR-diff mode with an attached Model-YAML block diff: the section
    // defaults to the Diff view (hint "diff"), the header carries the
    // Diff/File toggle + fold toggle + copy button (the Model SQL
    // idiom), and REAL clicks switch the views both ways.
    let diff = BlockDiff {
        lines: vec![
            dl(DiffLineKind::Context, "  - name: dim_a", None),
            dl(
                DiffLineKind::Removed,
                "    description: old words",
                Some((17, 26)),
            ),
            dl(
                DiffLineKind::Added,
                "    description: demo model dim_a",
                Some((17, 33)),
            ),
        ],
    };
    let model_yaml = ModelYamlOutcome::Found {
        path: "models/schema.yml".to_owned(),
        block: UnitTestYamlBlock::new(
            "  - name: dim_a\n    description: demo model dim_a".to_owned(),
            2,
            2,
            3,
        ),
        diff: Some(diff),
    };
    let url = render_with_model_yaml(
        "headless_model_yaml_diff_toggle.html",
        vec![model_node("model.shop.dim_a")],
        vec![("unit_test.shop.dim_a.t1", unit_test("t1", "dim_a"))],
        &["model.shop.dim_a"],
        &[],
        vec![("model.shop.dim_a", model_yaml)],
        ScopeSource::PrDiff,
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // Open the collapsed drawer with a REAL click on its summary.
    let _ = eval(
        &tab,
        "document.querySelector('.model-yaml > details > summary').click()",
    );
    assert!(
        eval_bool(&tab, "document.querySelector('.model-yaml > details').open"),
        "clicking the summary opens the Model YAML drawer",
    );
    assert_eq!(
        eval_string(
            &tab,
            "document.querySelector('.model-yaml-summary-hint').textContent"
        ),
        "diff",
        "the summary hint reads 'diff' when an inline diff is attached",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('.model-yaml .yaml-diff-view').hidden"
        ),
        "the Diff view is the default (visible)",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-yaml .yaml-authored-view').hidden"
        ),
        "the File view starts hidden",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-yaml .yaml-diff-view .diff-removed strong') !== null"
        ),
        "the removed line carries an intra-line emphasis <strong>",
    );
    // The code-card header carries the #199 affordances.
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-yaml .code-header .diff-fold-toggle') !== null"
        ),
        "the header carries the per-diff fold toggle",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-yaml .code-header .code-copy-btn') !== null"
        ),
        "the header carries the copy-icon button",
    );

    // REAL click: File → the views flip.
    let _ = eval(
        &tab,
        "document.querySelector('.model-yaml .yaml-view-btn[data-view=\"file\"]').click()",
    );
    assert!(
        eval_bool(
            &tab,
            "document.querySelector('.model-yaml .yaml-diff-view').hidden"
        ),
        "the Diff view hides after switching to File",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('.model-yaml .yaml-authored-view').hidden"
        ),
        "the File view shows after the toggle",
    );
    let file_text = eval_string(
        &tab,
        "document.querySelector('.model-yaml .yaml-authored-view').textContent",
    );
    assert!(
        file_text.contains("description: demo model dim_a"),
        "the File view renders the working-tree block; got {file_text:?}",
    );

    // REAL click back: Diff.
    let _ = eval(
        &tab,
        "document.querySelector('.model-yaml .yaml-view-btn[data-view=\"diff\"]').click()",
    );
    assert!(
        !eval_bool(
            &tab,
            "document.querySelector('.model-yaml .yaml-diff-view').hidden"
        ),
        "the Diff view returns after toggling back",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn model_yaml_section_hides_when_the_payload_has_no_gather_outcome() {
    // A render path that never ran the gather (every existing helper in
    // this file passes an empty map) must HIDE the section — hiding is
    // honest there because the payload carries no outcome to degrade
    // from; the placeholder arms cover every gather-ran case.
    let url = render_to_file(
        "headless_model_yaml_hidden.html",
        vec![model_node("model.shop.dim_a")],
        vec![("unit_test.shop.dim_a.t1", unit_test("t1", "dim_a"))],
        &["model.shop.dim_a"],
        &[],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    assert!(
        eval_bool(
            &tab,
            "getComputedStyle(document.querySelector('.model-yaml')).display === 'none'"
        ),
        "the Model YAML section hides when the payload carries no model_yaml",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn fold_toggle_hides_in_file_view_and_folds_only_the_visible_diff_universal() {
    // PR #250 review (gemini, templates/interaction.js): with a non-diff
    // view active, the per-diff fold toggle used to stay visible and
    // mutate the HIDDEN diff pre (its only meaningful target — File/raw/
    // authored views carry zero .diff-folded structure), flipping its own
    // label with no visible effect. The truthful fix: the fold toggle is
    // a Diff-view affordance — hidden while the non-diff view is active.
    //
    // UNIVERSAL quantification (the #240 lesson): swept over EVERY
    // dual-view drawer kind in one report — Model SQL (#111), Model YAML
    // (#247), and the unit-test YAML drawer (#96) — not one known-good
    // case. Real clicks throughout.
    let long_ctx = |name: &str| {
        let mut lines = vec![
            dl(DiffLineKind::Context, &format!("  - name: {name}"), None),
            dl(
                DiffLineKind::Removed,
                "    description: old",
                Some((17, 20)),
            ),
            dl(DiffLineKind::Added, "    description: new", Some((17, 20))),
        ];
        for i in 0..14 {
            lines.push(dl(DiffLineKind::Context, &format!("    # ctx {i}"), None));
        }
        BlockDiff { lines }
    };

    let test_id = "unit_test.shop.dim_a.upd";
    let raw_block = "  - name: upd\n    model: dim_a\n    given: []";
    let m = manifest(
        vec![model_node_with_raw_and_path(
            "model.shop.dim_a",
            "select 1 from x",
            "models/dim_a.sql",
        )],
        vec![(test_id, unit_test("upd", "dim_a"))],
    );
    let in_scope: InScopeSet = [test_id.to_owned()].into_iter().collect();
    let models: ModelInScopeSet = [NodeId::new("model.shop.dim_a")].into_iter().collect();
    let changed: InScopeSet = [test_id.to_owned()].into_iter().collect();
    let mut authoring_yaml: HashMap<String, UnitTestYamlBlock> = HashMap::new();
    authoring_yaml.insert(
        test_id.to_owned(),
        UnitTestYamlBlock::new(raw_block.to_owned(), 1, 1, 3),
    );
    let mut yaml_diffs: HashMap<String, BlockDiff> = HashMap::new();
    yaml_diffs.insert(test_id.to_owned(), long_ctx("upd"));
    let mut sql_diffs: HashMap<String, BlockDiff> = HashMap::new();
    sql_diffs.insert("model.shop.dim_a".to_owned(), long_ctx("dim_a_sql"));
    let mut model_yaml: HashMap<String, ModelYamlOutcome> = HashMap::new();
    model_yaml.insert(
        "model.shop.dim_a".to_owned(),
        ModelYamlOutcome::Found {
            path: "models/schema.yml".to_owned(),
            block: UnitTestYamlBlock::new("  - name: dim_a".to_owned(), 2, 2, 2),
            diff: Some(long_ctx("dim_a")),
        },
    );
    let out = tmp("headless_fold_toggle_dual_view_universal.html");
    let _ = std::fs::remove_file(&out);
    render_report(
        &out,
        &m,
        &in_scope,
        &models,
        &changed,
        &authoring_yaml,
        &yaml_diffs,
        &sql_diffs,
        &model_yaml,
        &HashMap::new(),
        "",
        ScopeSource::PrDiff,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let url = format!("file://{}", out.to_str().expect("UTF-8 path"));

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");

    // (drawer label, section selector, diff-pre selector, file-side
    // data-view value, file-pre selector, summary selector to open)
    let drawers = [
        (
            "Model SQL",
            ".model-sql",
            ".model-sql .sql-diff-view",
            "raw",
            ".model-sql .sql-raw-view",
            Some(".model-sql > details > summary"),
        ),
        (
            "Model YAML",
            ".model-yaml",
            ".model-yaml .yaml-diff-view",
            "file",
            ".model-yaml .yaml-authored-view",
            Some(".model-yaml > details > summary"),
        ),
        // The unit-test YAML drawer renders open by default.
        (
            "Unit test YAML",
            ".authoring-yaml",
            ".authoring-yaml .yaml-diff-view",
            "authored",
            ".authoring-yaml .yaml-authored-view",
            None,
        ),
    ];

    for (label, section, diff_pre, file_view, file_pre, summary) in drawers {
        if let Some(summary) = summary {
            let _ = eval(
                &tab,
                &format!("document.querySelector('{summary}').click()"),
            );
        }
        let folded_hidden = |tab: &Tab| {
            eval(
                tab,
                &format!(
                    "Array.from(document.querySelector('{diff_pre}')\
                     .querySelectorAll('.diff-folded')).filter(function(e){{return e.hidden;}}).length"
                ),
            )
            .as_u64()
            .expect("folded count")
        };
        let fold_btn_hidden = |tab: &Tab| {
            eval_bool(
                tab,
                &format!(
                    "(function(){{var b=document.querySelector('{section} .code-header .diff-fold-toggle');\
                     return !b || b.hidden || getComputedStyle(b).display === 'none';}})()"
                ),
            )
        };

        // Diff view (default): the toggle is visible and folds exist.
        assert!(
            !fold_btn_hidden(&tab),
            "[{label}] the fold toggle is visible in the Diff view",
        );
        let initial = folded_hidden(&tab);
        assert!(
            initial > 0,
            "[{label}] the diff renders with folded context (got {initial})",
        );
        // The file-side pre carries NO fold structure (nothing to fold).
        assert_eq!(
            eval(
                &tab,
                &format!(
                    "document.querySelector('{file_pre}').querySelectorAll('.diff-folded').length"
                ),
            )
            .as_u64(),
            Some(0),
            "[{label}] the file-side view has no fold structure",
        );
        // REAL click in Diff view: expands the VISIBLE diff pre.
        let _ = eval(
            &tab,
            &format!("document.querySelector('{section} .code-header .diff-fold-toggle').click()"),
        );
        assert_eq!(
            folded_hidden(&tab),
            0,
            "[{label}] clicking the fold toggle in the Diff view expands the visible diff",
        );

        // Switch to the file-side view with a REAL click: the toggle hides
        // and the diff pre's fold state is untouched by the switch.
        let _ = eval(
            &tab,
            &format!(
                "document.querySelector('{section} .yaml-view-btn[data-view=\"{file_view}\"]').click()"
            ),
        );
        assert!(
            fold_btn_hidden(&tab),
            "[{label}] the fold toggle hides while the file-side view is active \
             (it would otherwise mutate the hidden diff)",
        );
        assert_eq!(
            folded_hidden(&tab),
            0,
            "[{label}] switching views does not mutate the diff's fold state",
        );

        // Back to Diff: the toggle returns and still works.
        let _ = eval(
            &tab,
            &format!(
                "document.querySelector('{section} .yaml-view-btn[data-view=\"diff\"]').click()"
            ),
        );
        assert!(
            !fold_btn_hidden(&tab),
            "[{label}] the fold toggle returns when the Diff view is re-selected",
        );
        let _ = eval(
            &tab,
            &format!("document.querySelector('{section} .code-header .diff-fold-toggle').click()"),
        );
        assert!(
            folded_hidden(&tab) > 0,
            "[{label}] the returned fold toggle still collapses the visible diff",
        );
    }

    let _ = tab.close(true);
}

// ===== cute-dbt#242 — explore pages adopt the design system ==============
//
// The explore pages historically embedded Sakura plus hardcoded
// light-mode inline styles and ignored the saved appearance entirely.
// The #242 extraction re-layers report.css into shared askama partials
// (templates/partials/tokens.css + templates/partials/base.css) and
// ships a minimal shared appearance engine (templates/appearance.js),
// so both explore pages honor cute-dbt.appearance.v1 and theme
// correctly on ALL 8 themes. The guards below quantify UNIVERSALLY:
// every theme x both pages x every measured surface.
//
// Measurement hygiene (hard-won on prior verification runs):
//   - transitions are killed via `* { transition: none !important }`
//     before measuring — the body carries a 120ms background/color
//     transition that corrupts mid-flip reads;
//   - visibility is asserted via checkVisibility(), never rect>0
//     (closed-<details> children return non-zero rects in headless
//     Chromium) and never first-match sampling over duplicated classes.

use cute_dbt::adapters::explore::render_explore;
use cute_dbt::adapters::render::build_payload;
use cute_dbt::domain::all_models;

/// A compiled explore model with `raw_code` and a file path (so the
/// tests page renders a `.model-path code` surface).
fn explore_theme_model(id: &str, path: &str) -> Node {
    Node::new(
        NodeId::new(id),
        "model",
        Checksum::new("sha256", "ck"),
        Some("select 1".to_owned()),
        Some("select 1".to_owned()),
        DependsOn::default(),
        Some(path.to_owned()),
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
}

/// Render the explore pages in-process (domain objects ->
/// `render_explore`, no subprocess) and return the out directory.
fn render_explore_theme_pages(stem: &str) -> PathBuf {
    let m = manifest(
        vec![
            explore_theme_model("model.shop.stg_orders", "models/staging/stg_orders.sql"),
            explore_theme_model("model.shop.dim_orders", "models/marts/dim_orders.sql"),
        ],
        vec![(
            "unit_test.shop.dim_orders.t1",
            unit_test("t1", "dim_orders"),
        )],
    );
    let models = all_models(&m);
    let payload = build_payload(
        &m,
        &InScopeSet::new(),
        &models,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "",
    );
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(stem);
    let _ = std::fs::remove_dir_all(&dir);
    render_explore(&dir, &m, &models, None, &payload).expect("explore renders");
    dir
}

fn explore_page_url(dir: &Path, page: &str) -> String {
    let p = dir.join(page);
    format!("file://{}", p.to_str().expect("page path is valid UTF-8"))
}

/// The per-page theme sweep, evaluated in-page. For each of the 8
/// themes it sets `html[data-theme]` + the `html.dark` family class
/// (exactly what the shared appearance engine does), then measures:
///   - `pageBg`: the body's computed background vs the theme's resolved
///     `--bg` token (the page actually consumes the token layer);
///   - one entry per (theme, surface): WCAG contrast of the surface's
///     text against its EFFECTIVE composited backdrop (own opaque fill,
///     else the nearest opaque ancestor fill).
///
/// `targets` is a JS array literal of \[label, selector\] pairs; every
/// target must exist and be visible (checkVisibility) or the sweep
/// throws with the absentee's name.
fn explore_theme_sweep_js(targets: &str) -> String {
    format!(
        r#"(function () {{
  var THEMES = ["light", "solarized", "latte", "rosepine",
                "dark", "tokyo", "gruvbox", "dracula"];
  var DARK = {{ dark: true, tokyo: true, gruvbox: true, dracula: true }};
  function parseRgb(s) {{
    var m = /rgba?\(([^)]+)\)/.exec(s || "");
    if (!m) return null;
    var p = m[1].split(",");
    return {{ r: parseFloat(p[0]), g: parseFloat(p[1]), b: parseFloat(p[2]),
             a: p.length > 3 ? parseFloat(p[3]) : 1 }};
  }}
  function hexRgb(s) {{
    var m = /^#([0-9a-f]{{6}})$/i.exec((s || "").trim());
    if (!m) return null;
    return {{ r: parseInt(m[1].slice(0, 2), 16), g: parseInt(m[1].slice(2, 4), 16),
             b: parseInt(m[1].slice(4, 6), 16), a: 1 }};
  }}
  function chan(v) {{
    v = v / 255;
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  }}
  function lum(c) {{
    return 0.2126 * chan(c.r) + 0.7152 * chan(c.g) + 0.0722 * chan(c.b);
  }}
  function ratio(f, b) {{
    var lf = lum(f), lb = lum(b);
    var hi = Math.max(lf, lb), lo = Math.min(lf, lb);
    return (hi + 0.05) / (lo + 0.05);
  }}
  function backdropOf(el) {{
    for (var n = el; n; n = n.parentElement) {{
      var c = parseRgb(getComputedStyle(n).backgroundColor);
      if (c && c.a === 1) return c;
    }}
    return null;
  }}
  /* measurement hygiene: the shared base chassis transitions body
     background/color over 120ms — kill every transition first */
  var kill = document.createElement("style");
  kill.textContent = "* {{ transition: none !important; animation: none !important; }}";
  document.head.appendChild(kill);
  var TARGETS = {targets};
  var resolved = [];
  for (var t = 0; t < TARGETS.length; t++) {{
    var el = document.querySelector(TARGETS[t][1]);
    if (!el) {{
      throw new Error("explore theme sweep: no element for " + TARGETS[t][0]
        + " (" + TARGETS[t][1] + ")");
    }}
    /* checkVisibility, never rect>0: closed-details children report
       non-zero rects in headless Chromium */
    if (!el.checkVisibility()) {{
      throw new Error("explore theme sweep: " + TARGETS[t][0]
        + " (" + TARGETS[t][1] + ") is not visible — measuring a hidden "
        + "twin would be vacuous");
    }}
    resolved.push([TARGETS[t][0], el]);
  }}
  var root = document.documentElement;
  var out = [];
  for (var i = 0; i < THEMES.length; i++) {{
    /* exactly appearance.js applyTheme: data-theme + the html.dark sync */
    root.setAttribute("data-theme", THEMES[i]);
    root.classList.toggle("dark", !!DARK[THEMES[i]]);
    var tokenBg = hexRgb(getComputedStyle(root).getPropertyValue("--bg"));
    var bodyBg = parseRgb(getComputedStyle(document.body).backgroundColor);
    out.push({{
      theme: THEMES[i], el: "pageBg",
      ratio: -1,
      fg: getComputedStyle(root).getPropertyValue("--bg").trim(),
      bg: bodyBg ? "rgb(" + bodyBg.r + ", " + bodyBg.g + ", " + bodyBg.b + ")" : "none",
      tokenApplied: !!(tokenBg && bodyBg && bodyBg.a === 1
        && tokenBg.r === bodyBg.r && tokenBg.g === bodyBg.g && tokenBg.b === bodyBg.b)
    }});
    for (var j = 0; j < resolved.length; j++) {{
      var el2 = resolved[j][1];
      var cs = getComputedStyle(el2);
      var fg = parseRgb(cs.color);
      var own = parseRgb(cs.backgroundColor);
      var bg = own && own.a === 1 ? own : backdropOf(el2.parentElement);
      out.push({{
        theme: THEMES[i], el: resolved[j][0],
        ratio: fg && bg ? ratio(fg, bg) : -1,
        fg: cs.color,
        bg: bg ? "rgb(" + bg.r + ", " + bg.g + ", " + bg.b + ")" : "none",
        tokenApplied: true
      }});
    }}
  }}
  return JSON.stringify(out);
}})()"#
    )
}

/// Drive the per-page sweep and assert: the page consumes the token
/// layer on every theme (body bg == resolved --bg) and every measured
/// text surface clears WCAG AA 4.5:1 on its effective backdrop.
fn assert_explore_page_themes(tab: &Tab, page: &str, targets: &str, surfaces: usize) {
    let raw = eval_string(tab, &explore_theme_sweep_js(targets));
    let measured: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the explore theme sweep returns valid JSON");
    assert_eq!(
        measured.len(),
        8 * (surfaces + 1),
        "[{page}] 8 themes x ({surfaces} surfaces + pageBg) measured, got: {raw}",
    );
    let mut failures = Vec::new();
    for m in &measured {
        let theme = m["theme"].as_str().expect("theme is a string");
        let el = m["el"].as_str().expect("el is a string");
        let fg = m["fg"].as_str().unwrap_or("?");
        let bg = m["bg"].as_str().unwrap_or("?");
        if el == "pageBg" {
            if m["tokenApplied"] != serde_json::Value::Bool(true) {
                failures.push(format!(
                    "[{page}] {theme}: body background ({bg}) does not paint the \
                     theme's resolved --bg token ({fg}) — the page is not \
                     consuming the shared token layer"
                ));
            }
            continue;
        }
        let ratio = m["ratio"].as_f64().expect("ratio is a number");
        assert!(
            ratio > 0.0,
            "[{page}] the {theme}/{el} surface resolved no opaque backdrop — \
             the backdrop walk must end on a painted surface",
        );
        eprintln!("explore contrast [{page}] {theme:>9} / {el:<16} = {ratio:.2}  ({fg} on {bg})");
        if ratio < 4.5 {
            failures.push(format!("[{page}] {theme}/{el} = {ratio:.2} ({fg} on {bg})"));
        }
    }
    assert!(
        failures.is_empty(),
        "explore surfaces below the WCAG AA 4.5:1 floor or not token-themed \
         (cute-dbt#242): {failures:#?}",
    );
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn explore_pages_boot_applies_the_appearance_attributes() {
    // The shared appearance engine boots on BOTH explore pages: with no
    // saved appearance, html[data-theme] follows the host's
    // prefers-color-scheme (saved -> scheme -> light, the same contract
    // the report's persist test pins — never a platform-pinned value),
    // and the html.dark family class tracks the theme family.
    let dir = render_explore_theme_pages("explore-theme-boot");
    let browser = launch_browser();
    for page in ["dag.html", "tests.html"] {
        let tab = browser.new_tab().expect("new tab");
        tab.navigate_to(&explore_page_url(&dir, page))
            .expect("navigate");
        tab.wait_until_navigated().expect("await navigation");
        wait_for_document_ready(&tab);
        assert!(
            eval_bool(
                &tab,
                "document.documentElement.getAttribute('data-theme') === \
                 ((window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) ? 'dark' : 'light')"
            ),
            "[{page}] boot applies the prefers-color-scheme default theme \
             (saved -> scheme -> light)",
        );
        assert!(
            eval_bool(
                &tab,
                "document.documentElement.classList.contains('dark') === \
                 (document.documentElement.getAttribute('data-theme') === 'dark')"
            ),
            "[{page}] the html.dark family class tracks the booted theme",
        );
        let _ = tab.close(true);
    }
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn explore_pages_honor_the_saved_appearance_key() {
    // The cross-page contract: a theme saved on ANY cute-dbt page
    // (cute-dbt.appearance.v1) hydrates on the explore pages at boot.
    // Storage-gated exactly like the report's persist guard: where the
    // headless file:// origin denies localStorage the hydration legs are
    // skipped (the boot + sweep guards still run storage-free).
    let dir = render_explore_theme_pages("explore-theme-persist");
    let browser = launch_browser();
    for page in ["dag.html", "tests.html"] {
        let tab = browser.new_tab().expect("new tab");
        tab.navigate_to(&explore_page_url(&dir, page))
            .expect("navigate");
        tab.wait_until_navigated().expect("await navigation");
        wait_for_document_ready(&tab);
        let storage_ok = eval_bool(
            &tab,
            "(function(){try{if(!window.localStorage)return false;\
               window.localStorage.setItem('__probe','1');\
               window.localStorage.removeItem('__probe');return true;}\
               catch(e){return false;}})()",
        );
        if !storage_ok {
            eprintln!(
                "[{page}] localStorage unusable on this file:// origin — hydration leg skipped"
            );
            let _ = tab.close(true);
            continue;
        }
        let _ = eval(
            &tab,
            "window.localStorage.setItem('cute-dbt.appearance.v1', \
             JSON.stringify({theme:'dracula',density:'compact'}))",
        );
        tab.reload(false, None).expect("reload");
        tab.wait_until_navigated().expect("await reload");
        wait_for_document_ready(&tab);
        // Poll the hydrated theme — boot timing must never race the read
        // (the report persist guard's #208 lesson), null-safe mid-swap.
        let mut theme = String::new();
        for _ in 0..50 {
            theme = eval_string(
                &tab,
                "(document.documentElement \
                 && document.documentElement.getAttribute('data-theme')) || ''",
            );
            if theme == "dracula" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(
            theme, "dracula",
            "[{page}] the saved cute-dbt.appearance.v1 theme hydrates at boot",
        );
        assert!(
            eval_bool(&tab, "document.documentElement.classList.contains('dark')"),
            "[{page}] dracula is dark-family — html.dark follows the saved theme",
        );
        assert_eq!(
            eval_string(
                &tab,
                "document.documentElement.getAttribute('data-density') || ''",
            ),
            "compact",
            "[{page}] the saved density attribute applies (inert without \
             density rules, but the attribute contract is one across pages)",
        );
        // Clean up so the sibling page's leg starts from no-saved-state.
        let _ = eval(
            &tab,
            "window.localStorage.removeItem('cute-dbt.appearance.v1')",
        );
        let _ = tab.close(true);
    }
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn explore_dag_page_themes_correctly_on_every_theme() {
    // dag.html: the page consumes the shared token layer on all 8 themes
    // and the key text surfaces stay AA against their EFFECTIVE
    // composited backdrops. Since cute-dbt#251 the sweep also pins the
    // token-level AA matrix this page consumes: `--text-muted` on `--bg`
    // (the counts caption + hint), the `--accent` link on `--bg` (the
    // cross-page nav), and `--text-muted` on `--surface-2` (the hint's
    // kbd chip) — the combos the #251 per-theme token repair brought to
    // >= 4.5:1 (solarized/latte/rosepine accent; latte/rosepine/tokyo/
    // gruvbox muted).
    let dir = render_explore_theme_pages("explore-theme-dag");
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&explore_page_url(&dir, "dag.html"))
        .expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);
    assert_explore_page_themes(
        &tab,
        "dag.html",
        r#"[
          ["page-title", ".explore-header h1"],
          ["legend-chip", ".lineage-legend .legend-chip"],
          ["legend-code", ".lineage-legend code"],
          ["counts-muted-on-bg", ".explore-counts"],
          ["nav-link-on-bg", ".explore-nav a"],
          ["hint-muted-on-bg", ".lineage-hint"],
          ["kbd-muted-on-surface2", ".lineage-hint kbd"]
        ]"#,
        7,
    );
    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn explore_tests_page_themes_correctly_on_every_theme() {
    // tests.html: same universal sweep over the unit-test index + the
    // shared test-card viewer (visible — the fixture carries a unit
    // test, so the viewer section is not hidden). Since cute-dbt#251 the
    // sweep also pins this page's token-level AA matrix: `--text-muted`
    // on `--bg` (counts caption + the index's test-shape suffix), the
    // `--accent` nav link on `--bg`, and `--text-muted` on `--surface-2`
    // (the row-count badge — the C3 matrix's worst offender, 3.77:1 on
    // rosepine pre-repair).
    let dir = render_explore_theme_pages("explore-theme-tests");
    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&explore_page_url(&dir, "tests.html"))
        .expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);
    assert_explore_page_themes(
        &tab,
        "tests.html",
        r#"[
          ["page-title", ".explore-header h1"],
          ["model-heading", ".explore-model h2"],
          ["model-path-code", ".model-path code"],
          ["test-card-label", ".explore-viewer .test-card .test-select-field label"],
          ["counts-muted-on-bg", ".explore-counts"],
          ["nav-link-on-bg", ".explore-nav a"],
          ["test-shape-muted-on-bg", ".explore-test .test-shape"],
          ["row-count-badge-muted-on-surface2", ".explore-viewer .row-count-badge"]
        ]"#,
        8,
    );
    let _ = tab.close(true);
}

// ===== cute-dbt#251 — token-level AA matrix: the shared palette ==========
//
// The #264 adversarial verification measured the SHARED palette tokens
// sub-AA on 5 of 8 themes: `--accent` nav links on `--bg` (solarized
// 3.41), `--text-muted` on `--bg` (rosepine 3.53) and on `--surface-2`
// (the row-count badge — rosepine/latte/tokyo/gruvbox 3.77–4.41), plus
// the issue's original block-diff surfaces (solarized add-sigil 2.81,
// removed-line word-emphasis 3.53–4.08 across the light themes, the
// `.code-filename` header path and the `--text-muted` degrade copy).
// The repair is TOKEN-LEVEL (the #206 solarized-tier-chip pattern):
// per-theme value deepening in templates/partials/tokens.css, repairing
// the report and explore pages simultaneously. This guard is the
// mechanical encoding for the REPORT page (the explore twins live on
// the #242 sweeps above, extended with the same combos): for EVERY
// theme, every measured surface reaches AA 4.5:1 on its EFFECTIVE
// composited backdrop — semi-transparent layers (the dark themes' rgba
// row/word tints) are alpha-composited down to the first opaque fill,
// never skipped.
//
// Measurement hygiene (the #242 lessons, plus #227's mechanism pin):
//   - transitions/animations killed via injected style before measuring;
//   - visibility asserted via checkVisibility() on every target (the
//     Model SQL / Model YAML <details> are opened first; closed-details
//     rects lie in headless Chromium);
//   - the unified diff layout is pinned via html[data-difflayout] so the
//     measured sigil/word instances are the PAINTED ones (both layouts
//     are emitted; the hidden twin would measure vacuously);
//   - `dimmed` records any self-or-ancestor computed opacity < 1 on a
//     measured surface — composited opacity is not used for text (the
//     #227 decision; `.diff-sigil { opacity: 0.9 }` was removed at #251
//     under that rule, and this pin fails loudly if it returns);
//   - the link probe is injected (the #206 advisory-chip precedent): a
//     bare <a> on <body> exercises the shipped `a { color:
//     var(--accent) }` chassis rule on the page background, the exact
//     combo the C3 matrix measured on the explore nav.
//
// The split-view `.ds-num` line numbers are NOT contrast-measured:
// they are classified decorative (positional metadata duplicated from
// document order — the GitHub gutter idiom) and instead pinned to
// aria-hidden parity with the unified `.diff-gutter`, which has carried
// aria-hidden since #178 ("screen readers read the code, not the
// numbering"). The #251 AC sanctions exactly this either/or.

const REPORT_AA_TOKEN_MATRIX_SWEEP_JS: &str = r##"(function () {
  var THEMES = ["light", "solarized", "latte", "rosepine",
                "dark", "tokyo", "gruvbox", "dracula"];
  var DARK = { dark: true, tokyo: true, gruvbox: true, dracula: true };
  function parseRgb(s) {
    var m = /rgba?\(([^)]+)\)/.exec(s || "");
    if (!m) return null;
    var p = m[1].split(",");
    return { r: parseFloat(p[0]), g: parseFloat(p[1]), b: parseFloat(p[2]),
             a: p.length > 3 ? parseFloat(p[3]) : 1 };
  }
  function chan(v) {
    v = v / 255;
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  }
  function lum(c) {
    return 0.2126 * chan(c.r) + 0.7152 * chan(c.g) + 0.0722 * chan(c.b);
  }
  function ratio(f, b) {
    var lf = lum(f), lb = lum(b);
    var hi = Math.max(lf, lb), lo = Math.min(lf, lb);
    return (hi + 0.05) / (lo + 0.05);
  }
  /* The EFFECTIVE composited backdrop: collect every painted fill from
     the element itself up to the first OPAQUE one, then alpha-composite
     top-down. An opaque-only walk would skip the dark themes' rgba
     row/word tints entirely and measure the wrong backdrop. */
  function effectiveBackdrop(el) {
    var layers = [];
    for (var n = el; n; n = n.parentElement) {
      var c = parseRgb(getComputedStyle(n).backgroundColor);
      if (c && c.a > 0) {
        layers.push(c);
        if (c.a === 1) {
          var acc = layers.pop();
          while (layers.length) {
            var top = layers.pop();
            acc = { r: top.a * top.r + (1 - top.a) * acc.r,
                    g: top.a * top.g + (1 - top.a) * acc.g,
                    b: top.a * top.b + (1 - top.a) * acc.b, a: 1 };
          }
          return acc;
        }
      }
    }
    return null;
  }
  function dimmedChain(el) {
    var out = [];
    for (var n = el; n; n = n.parentElement) {
      if (parseFloat(getComputedStyle(n).opacity) < 1) {
        out.push(n.tagName.toLowerCase()
          + (n.className ? "." + String(n.className).trim().split(/\s+/).join(".") : ""));
      }
    }
    return out;
  }
  /* measurement hygiene: kill transitions before any read */
  var kill = document.createElement("style");
  kill.textContent = "* { transition: none !important; animation: none !important; }";
  document.head.appendChild(kill);
  /* open the drawers the measured surfaces live in */
  var sqlDetails = document.querySelector(".model-sql-details");
  if (!sqlDetails) { throw new Error("aa-matrix sweep: no .model-sql-details in the DOM"); }
  sqlDetails.open = true;
  var yamlDetails = document.querySelector(".model-yaml-details");
  if (!yamlDetails) { throw new Error("aa-matrix sweep: no .model-yaml-details in the DOM"); }
  yamlDetails.open = true;
  /* pin the unified layout so the measured diff instances are painted */
  document.documentElement.setAttribute("data-difflayout", "unified");
  /* the injected link probe (the #206 advisory-chip precedent) */
  if (!document.getElementById("aa251-link-probe")) {
    var a = document.createElement("a");
    a.id = "aa251-link-probe";
    a.href = "#aa251";
    a.textContent = "link probe";
    document.body.appendChild(a);
  }
  var TARGETS = [
    ["muted-hint-on-bg", ".model-sql .model-sql-summary-hint"],
    ["accent-link-on-bg", "#aa251-link-probe"],
    ["row-count-badge-muted", ".row-count-badge"],
    ["code-filename-muted", ".model-sql .code-header .code-filename"],
    ["yaml-degrade-muted", ".model-yaml .model-yaml-missing"],
    ["diff-add-sigil", ".model-sql .sql-diff-view .diff-unified .diff-added .diff-sigil"],
    ["diff-rem-sigil", ".model-sql .sql-diff-view .diff-unified .diff-removed .diff-sigil"],
    ["diff-add-word", ".model-sql .sql-diff-view .diff-unified .diff-added strong"],
    ["diff-rem-word", ".model-sql .sql-diff-view .diff-unified .diff-removed strong"]
  ];
  var resolved = [];
  for (var t = 0; t < TARGETS.length; t++) {
    var el = document.querySelector(TARGETS[t][1]);
    if (!el) {
      throw new Error("aa-matrix sweep: no element for " + TARGETS[t][0]
        + " (" + TARGETS[t][1] + ")");
    }
    /* checkVisibility, never rect>0 (closed-details rects lie) */
    if (!el.checkVisibility()) {
      throw new Error("aa-matrix sweep: " + TARGETS[t][0]
        + " (" + TARGETS[t][1] + ") is not visible — a hidden twin would "
        + "measure vacuously");
    }
    resolved.push([TARGETS[t][0], el]);
  }
  var root = document.documentElement;
  var out = [];
  for (var i = 0; i < THEMES.length; i++) {
    /* exactly theme.js applyTheme: data-theme + the html.dark sync */
    root.setAttribute("data-theme", THEMES[i]);
    root.classList.toggle("dark", !!DARK[THEMES[i]]);
    for (var j = 0; j < resolved.length; j++) {
      var el2 = resolved[j][1];
      var cs = getComputedStyle(el2);
      var fg = parseRgb(cs.color);
      var bg = effectiveBackdrop(el2);
      out.push({
        theme: THEMES[i], el: resolved[j][0],
        ratio: fg && bg ? ratio(fg, bg) : -1,
        fg: cs.color,
        bg: bg ? "rgb(" + bg.r + ", " + bg.g + ", " + bg.b + ")" : "none",
        dimmed: dimmedChain(el2)
      });
    }
  }
  return JSON.stringify(out);
})()"##;

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn report_aa_token_palette_matrix_on_every_theme() {
    // One PR-diff report carrying every #251 surface: a model SQL diff
    // (sigils + word-emphasis on both sides), the code-card file header,
    // the Model YAML degrade copy, the row-count badge, a muted summary
    // hint, and the injected link probe.
    let diff = BlockDiff {
        lines: vec![
            dl(DiffLineKind::Context, "select id", None),
            dl(DiffLineKind::Removed, "from t", Some((5, 6))),
            dl(DiffLineKind::Added, "from u", Some((5, 6))),
        ],
    };
    let m = manifest(
        vec![model_node_with_raw_and_path(
            "model.shop.dim_orders",
            "select id\nfrom u",
            "models/marts/dim_orders.sql",
        )],
        vec![(
            "unit_test.shop.dim_orders.t1",
            unit_test("t1", "dim_orders"),
        )],
    );
    let in_scope: InScopeSet = ["unit_test.shop.dim_orders.t1".to_owned()]
        .into_iter()
        .collect();
    let models: ModelInScopeSet = [NodeId::new("model.shop.dim_orders")].into_iter().collect();
    let sql_diffs: HashMap<String, BlockDiff> = [("model.shop.dim_orders".to_owned(), diff)]
        .into_iter()
        .collect();
    let model_yaml: HashMap<String, ModelYamlOutcome> = [(
        "model.shop.dim_orders".to_owned(),
        ModelYamlOutcome::FileMissing {
            path: "models/missing.yml".to_owned(),
        },
    )]
    .into_iter()
    .collect();
    let out = tmp("headless_aa_token_matrix.html");
    let _ = std::fs::remove_file(&out);
    render_report(
        &out,
        &m,
        &in_scope,
        &models,
        &InScopeSet::new(),
        &HashMap::new(),
        &HashMap::new(),
        &sql_diffs,
        &model_yaml,
        &HashMap::new(),
        "",
        ScopeSource::PrDiff,
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let url = format!("file://{}", out.to_str().expect("path is valid UTF-8"));

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);

    let raw = eval_string(&tab, REPORT_AA_TOKEN_MATRIX_SWEEP_JS);
    let measured: Vec<serde_json::Value> =
        serde_json::from_str(&raw).expect("the aa-matrix sweep returns valid JSON");
    assert_eq!(
        measured.len(),
        8 * 9,
        "8 themes x 9 surfaces measured, got: {raw}",
    );
    let mut failures = Vec::new();
    for m in &measured {
        let theme = m["theme"].as_str().expect("theme is a string");
        let el = m["el"].as_str().expect("el is a string");
        let ratio = m["ratio"].as_f64().expect("ratio is a number");
        let fg = m["fg"].as_str().unwrap_or("?");
        let bg = m["bg"].as_str().unwrap_or("?");
        assert!(
            ratio > 0.0,
            "the {theme}/{el} surface resolved no painted backdrop — the \
             composite walk must end on an opaque fill",
        );
        let dimmed = m["dimmed"].as_array().expect("dimmed is an array");
        if !dimmed.is_empty() {
            failures.push(format!(
                "{theme}/{el}: opacity-dimmed via {dimmed:?} — composited \
                 opacity is not used for text (the #227 rule; the token \
                 ratio must BE the painted one)"
            ));
        }
        eprintln!("aa-matrix contrast {theme:>9} / {el:<22} = {ratio:.2}  ({fg} on {bg})");
        if ratio < 4.5 {
            failures.push(format!("{theme}/{el} = {ratio:.2} ({fg} on {bg})"));
        }
    }
    assert!(
        failures.is_empty(),
        "report surfaces below the WCAG AA 4.5:1 floor (cute-dbt#251): {failures:#?}",
    );

    // The split-view `.ds-num` line numbers: decorative (the unified
    // `.diff-gutter` idiom) — pinned to aria-hidden PARITY, not to a
    // contrast floor (the #251 AC's either/or, decided decorative).
    assert!(
        eval_bool(
            &tab,
            "(function(){\
               var n = document.querySelectorAll('.model-sql .diff-split td.ds-num');\
               if (!n.length) { throw new Error('no split-view .ds-num cells rendered'); }\
               for (var i = 0; i < n.length; i++) {\
                 if (n[i].getAttribute('aria-hidden') !== 'true') return false;\
               }\
               return true;\
             })()"
        ),
        "every split-view .ds-num cell carries aria-hidden=\"true\" — parity \
         with the unified .diff-gutter (screen readers read the code, not \
         the numbering)",
    );

    let _ = tab.close(true);
}

// ===== cute-dbt#266 — the "Project definition changed" panel =============
//
// Three guards over the server-rendered panel:
//   1. the COMMITTED diff-showcase golden carries the categorized panel
//      (the dogfood surface — a regression dropping the panel from the
//      downloadable artifact fails here, not months later);
//   2. the Shape-A fallback row renders its explicit copy + raw diff
//      lines (corrupt-YAML / unparseable-new-side state);
//   3. the absence-note arm renders when dbt_project.yml is in the diff
//      but unreadable from the project root.
//
// Measurement hygiene (the #242 lessons): transitions/animations are
// killed before measuring, and visibility is asserted via
// checkVisibility() over ALL matching instances — never rect>0, never
// first-match sampling.

/// Kill transitions/animations, then return how many elements matching
/// `selector` are check-visible.
fn visible_count(tab: &Tab, selector: &str) -> i64 {
    let _ = eval(
        tab,
        "(() => { const s = document.createElement('style'); \
           s.textContent = '* { transition: none !important; animation: none !important; }'; \
           document.head.appendChild(s); return true; })()",
    );
    match eval(
        tab,
        &format!(
            "Array.from(document.querySelectorAll('{selector}'))\
               .filter(el => el.checkVisibility()).length"
        ),
    ) {
        serde_json::Value::Number(n) => n.as_i64().expect("integer count"),
        other => panic!("visible_count returned non-number: {other:?}"),
    }
}

/// Render a minimal PR-diff report carrying the given project facts.
fn render_with_project_facts(filename: &str, facts: &ProjectFacts) -> String {
    let m = manifest(vec![model_node("model.shop.dim_a")], Vec::new());
    let models: ModelInScopeSet = [NodeId::new("model.shop.dim_a")].into_iter().collect();
    let out = tmp(filename);
    let _ = std::fs::remove_file(&out);
    render_report_with_externals(
        &out,
        &m,
        &InScopeSet::new(),
        &models,
        &InScopeSet::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "",
        ScopeSource::PrDiff,
        DEFAULT_REPORT_TITLE,
        None,
        &cute_dbt::domain::CheckPolicy::default(),
        facts,
    )
    .expect("render writes the report");
    format!("file://{}", out.to_str().expect("UTF-8 path"))
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn project_panel_renders_categorized_on_the_committed_showcase() {
    // The dogfood surface: the committed diff-showcase golden must carry
    // the categorized panel — one vars row (with the locked
    // blast-radius copy) + one config-tree row.
    let showcase = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("diff-showcase-report.html");
    let url = format!("file://{}", showcase.to_str().expect("UTF-8 path"));

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);

    assert_eq!(
        visible_count(&tab, "[data-testid=\"project-def-panel\"]"),
        1,
        "the showcase renders exactly one visible project-definition panel",
    );
    assert_eq!(
        visible_count(&tab, ".project-def-row[data-category=\"vars\"]"),
        1,
        "the vars row (dq_quarantine_threshold 10→5) is visible",
    );
    assert_eq!(
        visible_count(&tab, ".project-def-row[data-category=\"config_tree\"]"),
        1,
        "the config-tree row (marts +materialized view→table) is visible",
    );
    let note = eval_string(
        &tab,
        "document.querySelector('.project-def-row[data-category=\"vars\"] .project-def-note').textContent",
    );
    assert_eq!(
        note, "blast radius not attributed",
        "the vars row carries the locked interim honesty copy",
    );

    // cute-dbt#269 — the purpose-built hooks + dispatch rows on the same
    // committed golden (the showcase patch edits the on-run-start hook
    // and reorders dispatch).
    assert_eq!(
        visible_count(&tab, ".project-def-row[data-category=\"hooks\"]"),
        1,
        "the hooks row (on-run-start rewrite) is visible",
    );
    assert_eq!(
        visible_count(
            &tab,
            ".project-def-row.is-banner[data-category=\"dispatch\"]"
        ),
        1,
        "the dispatch row renders as the banner",
    );
    let chip = eval_string(
        &tab,
        "document.querySelector('.project-def-row[data-category=\"dispatch\"] .tier-chip.tier-unknown').textContent",
    );
    assert_eq!(
        chip, "UNKNOWN",
        "the dispatch banner carries the UNKNOWN tier chip"
    );
    let hook_note = eval_string(
        &tab,
        "document.querySelector('.project-def-row[data-category=\"hooks\"] .project-def-note').textContent",
    );
    assert!(
        hook_note.contains(
            "runs in the manifest as \
             operation.healthcare_analytics.healthcare_analytics-on-run-start-0"
        ),
        "the hooks row names the manifest operation node: {hook_note}",
    );
    // The JS fills the slot with the #111 renderer: diff rows + the SQL
    // syntax palette inside the hook diff.
    assert_eq!(
        visible_count(&tab, ".project-hook-sql .diff-line.diff-added"),
        1,
        "the hook diff's added line renders via renderBlockDiff",
    );
    assert_eq!(
        visible_count(&tab, ".project-hook-sql .diff-line.diff-removed"),
        1,
        "the hook diff's removed line renders via renderBlockDiff",
    );
    // (`eval` here is this file's headless-Chrome harness helper —
    // JS evaluated in the sandboxed tab against our own report.)
    let kw_count = eval(
        &tab,
        "document.querySelectorAll('.project-hook-sql .sql-keyword').length",
    );
    assert!(
        kw_count.as_i64().unwrap_or(0) > 0,
        "the hook diff is SQL-tokenized (got {kw_count:?} keywords)",
    );

    // cute-dbt#267 — the config-tree row's affected-models listing (the
    // marts +materialized edit selects the 20 marts models, fusion's
    // fqn-prefix descent) with the R1b cap exercised: 20 > the inline
    // cap, so the count sentence shows and the names collapse into the
    // <details> listing.
    assert_eq!(
        visible_count(&tab, "[data-testid=\"project-def-affected\"]"),
        1,
        "the config-tree row carries the affected-models sentence",
    );
    let affected = eval_string(
        &tab,
        "document.querySelector('[data-testid=\"project-def-affected\"]').textContent",
    );
    assert_eq!(
        affected, "affects 20 models — widened into report scope, listed below",
        "the count is explicit (TOTAL tier) and past the R1b cap the names collapse",
    );
    assert_eq!(
        visible_count(&tab, "[data-testid=\"project-def-affected-list\"]"),
        1,
        "the collapsed name listing renders",
    );
    let names = eval_string(
        &tab,
        "document.querySelector('.project-def-affected-names').textContent",
    );
    assert!(
        names.contains("dim_payers") && names.contains("v_encounter_summary"),
        "the overflow listing carries the widened model names: {names}",
    );
    assert!(
        !names.contains("stg_synthea__patients"),
        "staging models are NOT under the edited marts subtree: {names}",
    );

    // The landing model (first with an updated test — a marts model) is
    // config-widened too, so its provenance chip row is visible on load.
    assert_eq!(
        visible_count(&tab, "[data-testid=\"model-attribution\"]"),
        1,
        "the provenance chip row renders for the widened landing model",
    );
    let chip = eval_string(
        &tab,
        "document.querySelector('[data-testid=\"model-attribution\"] .config-attribution-chip').textContent",
    );
    assert_eq!(
        chip, "+materialized via dbt_project.yml · models.healthcare_analytics.marts",
        "the chip names the contributing subtree",
    );
    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn config_attribution_chip_renders_for_the_widened_model_and_hides_otherwise() {
    // cute-dbt#267 — the synthetic chip guard: a model carrying a
    // config attribution shows the chip row; a model without one keeps
    // the row hidden (out of the accessibility tree).
    let facts = ProjectFacts {
        definition: None,
        panel: None,
        config_attributions: BTreeMap::from([(
            "model.shop.dim_a".to_owned(),
            vec![cute_dbt::domain::ConfigAttribution {
                key: "materialized".to_owned(),
                path: "models.shop.marts".to_owned(),
            }],
        )]),
        var_references: BTreeMap::new(),
    };
    let url = render_with_project_facts("headless_project_chip.html", &facts);

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);

    assert_eq!(
        visible_count(&tab, "[data-testid=\"model-attribution\"]"),
        1,
        "the chip row is visible for the attributed model",
    );
    let chip = eval_string(
        &tab,
        "document.querySelector('[data-testid=\"model-attribution\"] .config-attribution-chip').textContent",
    );
    assert_eq!(
        chip,
        "+materialized via dbt_project.yml · models.shop.marts"
    );

    // The unattributed control: same render, no attributions — hidden.
    let url = render_with_project_facts("headless_project_no_chip.html", &ProjectFacts::default());
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);
    assert_eq!(
        visible_count(&tab, "[data-testid=\"model-attribution\"]"),
        0,
        "no chip row for a model without attribution (hidden element)",
    );
    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn project_panel_fallback_row_renders_for_unparseable_yaml() {
    // The Shape-A degrade: the new side could not be parsed → explicit
    // "could not be categorized" copy + the raw diff lines, visible.
    let hunks = vec![Hunk {
        new_start: 1,
        new_len: 1,
        removed_lines: vec!["vars: {old: 1}".to_owned()],
        added_lines: vec!["vars: [broken".to_owned()],
    }];
    let facts = ProjectFacts {
        definition: None,
        panel: Some(ProjectChangePanel::Fallback {
            reason: ProjectFallbackReason::NewParseFailed,
            raw: raw_hunk_lines(&hunks),
        }),
        config_attributions: BTreeMap::new(),
        var_references: BTreeMap::new(),
    };
    let url = render_with_project_facts("headless_project_fallback.html", &facts);

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);

    assert_eq!(
        visible_count(&tab, "[data-testid=\"project-def-fallback\"]"),
        1,
        "the fallback copy renders visibly",
    );
    let copy = eval_string(
        &tab,
        "document.querySelector('[data-testid=\"project-def-fallback\"]').textContent",
    );
    assert!(
        copy.contains("could not be categorized"),
        "the copy states the degrade plainly: {copy}",
    );
    assert_eq!(
        visible_count(&tab, ".project-def-raw-line.is-removed"),
        1,
        "the removed raw line is visible",
    );
    assert_eq!(
        visible_count(&tab, ".project-def-raw-line.is-added"),
        1,
        "the added raw line is visible",
    );
    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn project_panel_hook_diff_renders_with_emphasis_via_the_111_renderer() {
    // cute-dbt#269 — a hooks row carrying HookChangeFacts.sql_diff gets
    // its slot filled by renderProjectHookDiffs: renderBlockDiff +
    // tokenizeSql, including the intra-line <strong> emphasis the
    // Rust-side diff computed. Asserted off a synthetic panel (no
    // showcase dependency) so the contract pins independent of fixture
    // churn.
    let facts = ProjectFacts {
        definition: None,
        panel: Some(ProjectChangePanel::Categorized {
            changes: vec![ProjectChange {
                category: ProjectChangeCategory::Hooks,
                label: "on-run-end".to_owned(),
                old: Some(serde_json::json!(["analyze table analytics.fct_x"])),
                new: Some(serde_json::json!(["analyze table analytics.fct_y"])),
                hook: Some(HookChangeFacts {
                    sql_diff: Some(BlockDiff {
                        lines: vec![
                            DiffLine {
                                kind: DiffLineKind::Removed,
                                text: "analyze table analytics.fct_x".to_owned(),
                                emphasis: Some((28, 29)),
                            },
                            DiffLine {
                                kind: DiffLineKind::Added,
                                text: "analyze table analytics.fct_y".to_owned(),
                                emphasis: Some((28, 29)),
                            },
                        ],
                    }),
                    operation_ids: vec!["operation.shop.shop-on-run-end-0".to_owned()],
                    manifest: HookManifestPresence::Matched,
                }),
                tree: None,
                vars: None,
            }],
        }),
        config_attributions: BTreeMap::new(),
        var_references: BTreeMap::new(),
    };
    let url = render_with_project_facts("headless_project_hook_diff.html", &facts);

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);

    assert_eq!(
        visible_count(&tab, ".project-def-hook-slot .project-hook-sql"),
        1,
        "the JS fills the hook slot with the rendered diff",
    );
    assert_eq!(
        visible_count(&tab, ".project-hook-sql .diff-line.diff-removed"),
        1,
    );
    assert_eq!(
        visible_count(&tab, ".project-hook-sql .diff-line.diff-added"),
        1,
    );
    // The intra-line emphasis overlays as <strong> inside the tokenized
    // line (the #132 contract, reused verbatim).
    // (`eval` = this file's headless-Chrome harness helper.)
    let strong_count = eval(
        &tab,
        "document.querySelectorAll('.project-hook-sql .diff-line strong').length",
    );
    assert!(
        strong_count.as_i64().unwrap_or(0) >= 2,
        "both sides carry the intra-line emphasis overlay (got {strong_count:?})",
    );
    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn project_panel_absence_note_renders_when_file_unreadable() {
    let hunks = vec![Hunk {
        new_start: 1,
        new_len: 1,
        removed_lines: vec!["name: old".to_owned()],
        added_lines: vec!["name: new".to_owned()],
    }];
    let facts = ProjectFacts {
        definition: None,
        panel: Some(ProjectChangePanel::Fallback {
            reason: ProjectFallbackReason::FileUnreadable,
            raw: raw_hunk_lines(&hunks),
        }),
        config_attributions: BTreeMap::new(),
        var_references: BTreeMap::new(),
    };
    let url = render_with_project_facts("headless_project_absence.html", &facts);

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.navigate_to(&url).expect("navigate");
    tab.wait_until_navigated().expect("await navigation");
    wait_for_document_ready(&tab);

    assert_eq!(
        visible_count(&tab, "[data-testid=\"project-def-panel\"]"),
        1,
        "the panel still renders — the change is never silently invisible",
    );
    let copy = eval_string(
        &tab,
        "document.querySelector('[data-testid=\"project-def-fallback\"]').textContent",
    );
    assert!(
        copy.contains("could not be read from the project root"),
        "the absence note names exactly what is missing: {copy}",
    );
    let _ = tab.close(true);
}
