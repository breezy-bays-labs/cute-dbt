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

use cute_dbt::adapters::manifest::FileManifestSource;
use cute_dbt::adapters::render::{ScopeSource, render_report};
use cute_dbt::domain::{
    BlockDiff, Checksum, DEFAULT_REPORT_TITLE, DependsOn, DiffLine, DiffLineKind, FileHunks, Hunk,
    InScopeSet, Manifest, ManifestMetadata, ModelInScopeSet, Node, NodeConfig, NodeId,
    NormalizedDiffIndex, PrDiff, UnitTest, UnitTestDataDiff, UnitTestExpect, UnitTestGiven,
    UnitTestYamlBlock, reconstruct_table_diffs,
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
    eval(tab, expr).as_i64().unwrap_or(-1)
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
        CellChange, CellValue, ColumnStatus, DiffColumn, FixtureTableDiff, NamedTableDiff,
        RowChange, RowChangeKind,
    };
    UnitTestDataDiff {
        given: vec![NamedTableDiff {
            input: input.to_owned(),
            diff: FixtureTableDiff {
                columns: vec![DiffColumn {
                    name: "id".into(),
                    status: ColumnStatus::Present,
                }],
                rows: vec![RowChange {
                    kind: RowChangeKind::Modified,
                    cells: vec![CellChange {
                        old: CellValue::Number("1".into()),
                        new: CellValue::Number("2".into()),
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
        CellChange, CellValue, ColumnStatus, DiffColumn, FixtureTableDiff, NamedTableDiff,
        RowChange, RowChangeKind,
    };
    UnitTestDataDiff {
        given: vec![NamedTableDiff {
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
                            old: CellValue::Number("1".into()),
                            new: CellValue::Null,
                            changed: true,
                        },
                        // new side is the STRING "null" -> a normal value
                        // (.cell-new), NOT .cell-null.
                        CellChange {
                            old: CellValue::Str("completed".into()),
                            new: CellValue::Str("null".into()),
                            changed: true,
                        },
                    ],
                }],
            },
        }],
        expect: None,
    }
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

    // ===== Value-change report: Diff view tints the changed cell 9 → 1 =====
    tab.navigate_to(&url_value_change)
        .expect("navigate value-change");
    tab.wait_until_navigated()
        .expect("await value-change navigation");
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

    // The controls strip renders in PR-diff mode, context input defaults to 3.
    assert_eq!(
        eval_string(
            &tab,
            "String(document.querySelectorAll('.diff-view-controls').length)"
        ),
        "1",
        "the diff-view controls strip renders in PR-diff mode",
    );
    assert_eq!(
        eval_string(&tab, "document.querySelector('.diff-context-input').value"),
        "3",
        "the context-lines input defaults to 3",
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
