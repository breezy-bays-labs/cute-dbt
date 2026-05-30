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
    Checksum, DEFAULT_REPORT_TITLE, DependsOn, DiffLine, DiffLineKind, InScopeSet, Manifest,
    ManifestMetadata, ModelInScopeSet, Node, NodeConfig, NodeId, UnitTest, UnitTestExpect,
    UnitTestYamlBlock, YamlBlockDiff,
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
    assert!(
        !affirm_present(&tab),
        "baseline mode never renders the PR-diff-specific affirmation",
    );

    let _ = tab.close(true);
}

// --- cute-dbt#96 concern 2: inline YAML diff drawer -----------------

/// PR-diff render that injects an authoring-YAML map and an inline-diff map
/// so the Authored↔Diff drawer renders. `authoring` is `(test_id, raw)`;
/// `diffs` is `(test_id, YamlBlockDiff)`. The block span is irrelevant to
/// the JS drawer (only `raw` + the diff lines are surfaced), so it is pinned
/// to `[1, line_count]`.
fn render_pr_diff_with_diffs(
    filename: &str,
    nodes: Vec<Node>,
    tests: Vec<(&str, UnitTest)>,
    model_ids: &[&str],
    changed_ids: &[&str],
    authoring: Vec<(&str, &str)>,
    diffs: Vec<(&str, YamlBlockDiff)>,
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
    let yaml_diffs: HashMap<String, YamlBlockDiff> = diffs
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
    let diff = YamlBlockDiff {
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
