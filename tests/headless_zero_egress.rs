//! Headless `file://` zero-egress proof.
//!
//! The PRIMARY zero-egress gate. A real Chromium opens the committed
//! `examples/jaffle-shop-report.html` via a real `file://` URL with DNS
//! denied at the browser level, and we subscribe to every
//! `Network.requestWillBeSent` event. The proof: zero external requests
//! (http / https / ws / wss / ftp) are emitted by the rendered chrome.
//!
//! Why this matters: the v0.x adoption gate is "your data stays on your
//! machine." The renderer makes that property *structurally* true by
//! inlining every asset (Sakura CSS / jQuery / DataTables / Mermaid UMD
//! bundle) at compile time. This test makes the property *trivially
//! auditable*: a non-engineer with the repo checked out can re-run
//! `cargo test --test headless_zero_egress` and observe the empty
//! request log themselves.
//!
//! ## Hard gate
//!
//! The test asserts a REAL `file://` URL. NEVER `127.0.0.1` loopback —
//! Chromium treats real `file://` as a stricter null-origin context
//! than loopback, and the proof is invalid against any other origin.
//! See ADR-4 (asset embedding + zero-egress gate) and ARCHITECTURE.md §5.
//!
//! ## DNS denial vs event capture
//!
//! `--host-resolver-rules=MAP * ~NOTFOUND` is belt-and-braces — even if
//! the page tried to fetch, DNS would fail. The LOAD-BEARING assertion
//! is the captured event log: subscribe to `Network.requestWillBeSent`
//! before navigate, filter to external schemes, assert empty. Local
//! schemes (`file:`, `data:`, `blob:`) are excluded from the filter,
//! never blocked — the `data:` URI favicon is part of the design.
//!
//! ## `#[ignore]` opt-in
//!
//! The test is `#[ignore]` by default so the standard `cargo nextest
//! run --all-targets` invocation does not pull in a Chrome dependency.
//! It runs explicitly in the dedicated `headless-zero-egress` CI job
//! (which installs Chrome via `browser-actions/setup-chrome`) via
//! `cargo test --test headless_zero_egress -- --ignored`. Locally:
//!
//! ```bash
//! cargo test --test headless_zero_egress -- --ignored
//! ```
//!
//! Tracked: breezy-bays-labs/cute-dbt#12.

#[path = "common/mod.rs"]
mod common;

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use headless_chrome::Browser;
use headless_chrome::LaunchOptionsBuilder;
use headless_chrome::browser::tab::point::Point;
use headless_chrome::protocol::cdp::types::Event;
use headless_chrome::protocol::cdp::{Network, Runtime};

use cute_dbt::adapters::explore::render_explore;
use cute_dbt::adapters::render::build_payload;
use cute_dbt::domain::{
    Checksum, DependsOn, InScopeSet, Manifest, ManifestMetadata, Node, NodeConfig, NodeId,
    all_models,
};

fn report_file_url(filename: &str) -> String {
    let path = common::example_path(filename);
    let p = path.to_str().expect("report path must be valid UTF-8");
    format!("file://{p}")
}

#[derive(Debug, Clone)]
struct ExternalRequest {
    url: String,
    initiator_type: String,
    initiator_url: Option<String>,
    initiator_line: Option<f64>,
}

impl std::fmt::Display for ExternalRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let init = self
            .initiator_url
            .as_deref()
            .map(|u| {
                let ln = self
                    .initiator_line
                    .map_or(String::new(), |l| format!(":{l}"));
                format!("{u}{ln}")
            })
            .unwrap_or_else(|| "<unknown>".to_string());
        write!(
            f,
            "  - {url}\n      initiator: {kind} from {init}",
            url = self.url,
            kind = self.initiator_type,
        )
    }
}

fn scheme_is_external(url: &str) -> bool {
    let (scheme, _) = match url.split_once(':') {
        Some(parts) => parts,
        None => return false,
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "ws" | "wss" | "ftp" | "ftps",
    )
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn every_committed_example_makes_zero_external_requests_when_opened_via_file_url() {
    // Validate the example files exist BEFORE launching Chrome — a
    // missing file is a config error, not a Chrome failure.
    for filename in common::COMMITTED_EXAMPLES {
        let path = common::example_path(filename);
        assert!(
            path.exists(),
            "examples/{filename} missing — regenerate via the \
             `example-report-up-to-date` CI step or run:\n  cargo run --bin cute-dbt -- report \
             --manifest <fixture-current.json> --baseline-manifest <fixture-baseline.json> \
             --out examples/{filename}",
        );
    }

    // CI provides Chrome via `browser-actions/setup-chrome` and exports
    // CHROME=<path>. Locally we fall back to headless_chrome's discovery
    // (it picks the system Chrome / Chromium binary). Pinning the CI
    // path explicitly prevents the auto-fetch path from silently hitting
    // the network during CI startup.
    let chrome_path = std::env::var_os("CHROME").map(PathBuf::from);

    // Args: DNS-denial + standard CI flags. Order matches Chromium's
    // documented short forms.
    let host_resolver = OsStr::new("--host-resolver-rules=MAP * ~NOTFOUND");
    let no_first_run = OsStr::new("--no-first-run");
    let no_default_check = OsStr::new("--no-default-browser-check");
    let disable_breakpad = OsStr::new("--disable-breakpad");

    let mut builder = LaunchOptionsBuilder::default();
    builder
        .headless(true)
        .sandbox(false) // GitHub Actions runners need --no-sandbox
        .args(vec![
            host_resolver,
            no_first_run,
            no_default_check,
            disable_breakpad,
        ]);
    if let Some(p) = chrome_path.as_ref() {
        builder.path(Some(p.clone()));
    }
    let opts = builder.build().expect("LaunchOptions must build");

    let browser = Browser::new(opts).expect("Chromium must launch");

    // One Chrome instance, fresh tab per example — keeps the test
    // light (no per-example launch) while ensuring per-example event
    // capture is isolated.
    let mut failures: Vec<String> = Vec::new();
    for filename in common::COMMITTED_EXAMPLES {
        let url = report_file_url(filename);
        assert!(
            url.starts_with("file://"),
            "zero-egress proof MUST run against a real file:// origin; got {url}",
        );

        let tab = browser.new_tab().expect("new tab");

        // Enable the Network domain BEFORE navigate so
        // RequestWillBeSent events fire for the navigation and any
        // subsequent fetches.
        tab.call_method(Network::Enable {
            max_total_buffer_size: None,
            max_resource_buffer_size: None,
            max_post_data_size: None,
            report_direct_socket_traffic: None,
            enable_durable_messages: None,
        })
        .expect("enable Network domain");

        let external = Arc::new(Mutex::new(Vec::<ExternalRequest>::new()));
        let external_recorder = external.clone();
        tab.add_event_listener(Arc::new(move |event: &Event| {
            if let Event::NetworkRequestWillBeSent(e) = event {
                let req_url = e.params.request.url.clone();
                if scheme_is_external(&req_url) {
                    external_recorder.lock().unwrap().push(ExternalRequest {
                        url: req_url,
                        initiator_type: format!("{:?}", e.params.initiator.Type),
                        initiator_url: e.params.initiator.url.clone(),
                        initiator_line: e.params.initiator.line_number,
                    });
                }
            }
        }))
        .expect("subscribe Network.requestWillBeSent");

        tab.navigate_to(&url).expect("navigate to file:// URL");
        tab.wait_until_navigated().expect("await navigation");

        // Mermaid renders on-demand (ADR-4 amendment 2026-05-22:
        // `startOnLoad: false`). The SVG appears inside
        // `.cte-dag-mermaid` once `renderDag()` runs + `mermaid.render()`
        // resolves. We wait for the SVG element to assert the inlined
        // Mermaid UMD bundle actually works offline.
        let mermaid_ok = tab
            .wait_for_element_with_custom_timeout(".cte-dag-mermaid svg", Duration::from_secs(15))
            .is_ok();

        // DataTables initialization signal — once
        // `$('table').DataTable()` resolves, the table element gets
        // the `dataTable` class. Use `Runtime::Evaluate` directly with
        // `returnByValue: true` so the result lands in
        // `RemoteObject.value` as a deserialized JSON bool regardless
        // of whether the runtime would otherwise return an `objectId`
        // handle.
        let dt_eval = tab
            .call_method(Runtime::Evaluate {
                expression: "(function () { \
                       try { \
                         return !!(window.jQuery \
                           && window.jQuery.fn \
                           && window.jQuery.fn.DataTable \
                           && document.querySelector('table.dataTable')); \
                       } catch (_) { return false; } \
                     })()"
                    .to_string(),
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
            .expect("evaluate DataTables init probe");
        let datatable_ok = dt_eval
            .result
            .value
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let captured = external.lock().unwrap().clone();

        if !captured.is_empty() {
            let listing = captured
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            failures.push(format!(
                "examples/{filename}: {n} external request(s):\n{listing}",
                n = captured.len(),
            ));
        }
        if !mermaid_ok {
            failures.push(format!(
                "examples/{filename}: Mermaid SVG never appeared inside .cte-dag-mermaid \
                 — either the inlined UMD bundle is broken or the rendering path tried to \
                 fetch something blocked by DNS denial.",
            ));
        }
        if !datatable_ok {
            failures.push(format!(
                "examples/{filename}: DataTables did not initialize — the inlined jQuery + \
                 DataTables bundle is broken or one of them tried to fetch externally.",
            ));
        }

        // Close the tab to free the resources before opening the next.
        let _ = tab.close(true);
    }

    assert!(
        failures.is_empty(),
        "zero-egress proof FAILED on one or more committed examples — each is a hole in the auditability story:\n{}",
        failures.join("\n"),
    );
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn every_committed_explore_page_makes_zero_external_requests_when_opened_via_file_url() {
    // cute-dbt#100 — the explore pages' arm of the primary gate. The
    // zero-egress request-log assertion is UNIFORM with the report
    // examples above; the LIVENESS oracle is page-aware:
    //   - dag.html renders the model lineage through the inlined Mermaid
    //     UMD bundle → wait for the SVG inside .lineage-dag;
    //   - tests.html is a static server-rendered page with NO Mermaid
    //     and NO DataTables → assert DOM facts (the per-model sections
    //     exist and the embedded cute-dbt-data payload parses). Applying
    //     the report's Mermaid/DataTables probes here would be a
    //     category error — there is nothing to initialize.
    for filename in common::COMMITTED_EXPLORE_PAGES {
        let path = common::example_path(filename);
        assert!(
            path.exists(),
            "examples/{filename} missing — regenerate via:\n  cargo run --bin cute-dbt -- \
             explore --manifest tests/fixtures/playground-current.json --out-dir examples/explore",
        );
    }

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
    let browser = Browser::new(opts).expect("Chromium must launch");

    let mut failures: Vec<String> = Vec::new();
    for filename in common::COMMITTED_EXPLORE_PAGES {
        let url = report_file_url(filename);
        assert!(
            url.starts_with("file://"),
            "zero-egress proof MUST run against a real file:// origin; got {url}",
        );
        let tab = browser.new_tab().expect("new tab");
        tab.call_method(Network::Enable {
            max_total_buffer_size: None,
            max_resource_buffer_size: None,
            max_post_data_size: None,
            report_direct_socket_traffic: None,
            enable_durable_messages: None,
        })
        .expect("enable Network domain");

        let external = Arc::new(Mutex::new(Vec::<ExternalRequest>::new()));
        let external_recorder = external.clone();
        tab.add_event_listener(Arc::new(move |event: &Event| {
            if let Event::NetworkRequestWillBeSent(e) = event {
                let req_url = e.params.request.url.clone();
                if scheme_is_external(&req_url) {
                    external_recorder.lock().unwrap().push(ExternalRequest {
                        url: req_url,
                        initiator_type: format!("{:?}", e.params.initiator.Type),
                        initiator_url: e.params.initiator.url.clone(),
                        initiator_line: e.params.initiator.line_number,
                    });
                }
            }
        }))
        .expect("subscribe Network.requestWillBeSent");

        tab.navigate_to(&url).expect("navigate to file:// URL");
        tab.wait_until_navigated().expect("await navigation");

        // Page-aware liveness oracle (cute-dbt#100; dag.html re-oracled
        // for the interactive Cytoscape engine in cute-dbt#101).
        if filename.ends_with("dag.html") {
            let lineage_ok = tab
                .wait_for_element_with_custom_timeout(
                    ".lineage-canvas canvas",
                    Duration::from_secs(15),
                )
                .is_ok();
            if !lineage_ok {
                failures.push(format!(
                    "examples/{filename}: the Cytoscape canvas never appeared inside \
                     .lineage-canvas — either the inlined Cytoscape/cytoscape-dagre UMD \
                     bundles are broken offline or the explore lineage engine failed to boot.",
                ));
            }
        } else {
            // `eval` is this file's Runtime.Evaluate helper (a CDP probe
            // into OUR OWN generated page inside a hermetic headless
            // test) — not JS eval() over untrusted input.
            let sections = eval(
                &tab,
                "document.querySelectorAll('section.explore-model').length",
            )
            .as_u64()
            .unwrap_or(0);
            if sections == 0 {
                failures.push(format!(
                    "examples/{filename}: no section.explore-model rendered — the static \
                     unit-test index is empty or broken.",
                ));
            }
            let payload_ok = eval(
                &tab,
                "(function () { \
                   try { \
                     var el = document.getElementById('cute-dbt-data'); \
                     return !!(el && JSON.parse(el.textContent).models.length); \
                   } catch (_) { return false; } \
                 })()",
            )
            .as_bool()
            .unwrap_or(false);
            if !payload_ok {
                failures.push(format!(
                    "examples/{filename}: the embedded cute-dbt-data payload is missing or \
                     does not parse to a non-empty models array.",
                ));
            }
        }

        let captured = external.lock().unwrap().clone();
        if !captured.is_empty() {
            let listing = captured
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            failures.push(format!(
                "examples/{filename}: {n} external request(s):\n{listing}",
                n = captured.len(),
            ));
        }
        let _ = tab.close(true);
    }

    assert!(
        failures.is_empty(),
        "zero-egress proof FAILED on one or more committed explore pages:\n{}",
        failures.join("\n"),
    );
}

// ===== cute-dbt#101 — explore lineage interaction (real keyboard + =====
// ===== pointer through the CDP input pipeline)                     =====
//
// NOTE on `eval` below: it is this file's `Runtime.Evaluate` CDP helper
// (a probe into OUR OWN generated page inside a hermetic headless test)
// — not JS `eval()` over untrusted input.

/// A synthetic compiled model node with explicit `depends_on.nodes`
/// edges — the in-process explore-page harness's only builder.
fn explore_model(id: &str, deps: &[&str]) -> Node {
    Node::new(
        NodeId::new(id),
        "model",
        Checksum::new("sha256", "ck"),
        Some("select 1".to_owned()),
        None,
        DependsOn::new(Vec::new(), deps.iter().map(|d| NodeId::new(*d)).collect()),
        None,
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
}

/// Render an explore page in-process (the headless fixture discipline:
/// domain objects -> `render_explore`, no subprocess) and return the
/// `file://` URL of the emitted `dag.html`.
fn render_explore_dag(stem: &str, nodes: Vec<Node>) -> String {
    let manifest = Manifest::new(
        ManifestMetadata::new("v12"),
        nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
        HashMap::new(),
        HashMap::new(),
    );
    let models = all_models(&manifest);
    let payload = build_payload(
        &manifest,
        &InScopeSet::new(),
        &models,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        "",
    );
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(stem);
    let _ = std::fs::remove_dir_all(&dir);
    render_explore(&dir, &manifest, &models, &payload).expect("explore renders");
    let p = dir.join("dag.html");
    format!("file://{}", p.to_str().expect("page path is valid UTF-8"))
}

/// Launch the shared DNS-denied headless Chromium (same flag battery as
/// the zero-egress gates above).
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
fn explore_dag_search_highlight_and_space_commit_drive_real_keyboard_and_pointer() {
    // cute-dbt#101 — the HIGHLIGHT vs FOCUS contract, driven through the
    // REAL CDP input pipeline (Input.dispatchKeyEvent for typing/Enter/
    // Space, Input.dispatchMouseEvent for the node click), never synthetic
    // JS dispatchEvent. The proof points, in order:
    //   1. search-type + Enter SELECTS a match -> highlight applied,
    //      focus moved off the input onto the canvas host (hard AC a),
    //      and NO data-selected-model written (highlight is exploratory).
    //   2. Space with the canvas focused -> the focus COMMIT: the
    //      selected-model dataset write appears AND the page does not
    //      scroll (the handler preventDefaults — hard AC b).
    //   3. Space typed INTO the focused search input stays typing (the
    //      gate's other half): the input value grows, the commit signal
    //      does not change.
    //   4. a real pointer click on another node re-highlights (dimming
    //      the complement) WITHOUT committing; the next Space commits it.
    // Plus two contract riders: pan/zoom/drag are live on the booted
    // instance, and the whole session emits ZERO console warnings/errors
    // (the Discovery item — no `wheelSensitivity` notice, no deprecated
    // `width: label` warning).
    let url = render_explore_dag(
        "explore-interaction",
        vec![
            explore_model("model.shop.stg_orders", &[]),
            explore_model("model.shop.dim_orders", &["model.shop.stg_orders"]),
            explore_model("model.shop.mart_orders", &["model.shop.dim_orders"]),
            // Disconnected — the dim-complement witness.
            explore_model("model.shop.lonely", &[]),
        ],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");

    // Console recorder (warnings + errors), subscribed BEFORE navigate:
    // both the page-JS channel (Runtime.consoleAPICalled — where
    // Cytoscape's wheelSensitivity / `width: label` warnings would land)
    // and the browser channel (Log.entryAdded — deprecations etc.).
    tab.call_method(Runtime::Enable(None))
        .expect("enable Runtime domain");
    tab.call_method(headless_chrome::protocol::cdp::Log::Enable(None))
        .expect("enable Log domain");
    let console_noise = Arc::new(Mutex::new(Vec::<String>::new()));
    let console_recorder = console_noise.clone();
    tab.add_event_listener(Arc::new(move |event: &Event| match event {
        Event::RuntimeConsoleAPICalled(e) => {
            use headless_chrome::protocol::cdp::Runtime::ConsoleAPICalledEventTypeOption as T;
            if matches!(e.params.Type, T::Warning | T::Error) {
                let text = e
                    .params
                    .args
                    .first()
                    .and_then(|a| {
                        a.description
                            .clone()
                            .or_else(|| a.value.as_ref().map(ToString::to_string))
                    })
                    .unwrap_or_default();
                console_recorder
                    .lock()
                    .unwrap()
                    .push(format!("console.{:?}: {text}", e.params.Type));
            }
        }
        Event::LogEntryAdded(e) => {
            use headless_chrome::protocol::cdp::Log::LogEntryLevel as L;
            if matches!(e.params.entry.level, L::Warning | L::Error) {
                console_recorder.lock().unwrap().push(format!(
                    "log.{:?}: {}",
                    e.params.entry.level, e.params.entry.text
                ));
            }
        }
        _ => {}
    }))
    .expect("subscribe console events");

    tab.navigate_to(&url).expect("navigate to file:// URL");
    tab.wait_until_navigated().expect("await navigation");
    tab.wait_for_element_with_custom_timeout(".lineage-canvas canvas", Duration::from_secs(15))
        .expect("the Cytoscape canvas boots offline");

    // The pristine page carries NO commit signal.
    assert_eq!(
        eval(&tab, "'selectedModel' in document.body.dataset"),
        serde_json::Value::Bool(false),
        "no data-selected-model before any interaction",
    );

    // Pan / zoom / drag-reposition are live (the AC's interactivity
    // floor): user panning + zooming enabled, nodes grabbable.
    assert_eq!(
        eval(
            &tab,
            "(function () { \
               var cy = window.CuteExploreLineage.cyInstance(); \
               return cy.userPanningEnabled() && cy.userZoomingEnabled() \
                 && cy.nodes()[0].grabbable(); \
             })()",
        ),
        serde_json::Value::Bool(true),
        "pan, zoom and node drag must all be enabled",
    );

    // --- 1. fuzzy-search select via real typing + Enter ------------------
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-search-input').focus()",
    );
    tab.type_str("dim").expect("type the query");
    assert!(
        eval(
            &tab,
            "document.querySelectorAll('#lineage-search-results li[role=option]').length"
        )
        .as_u64()
        .unwrap_or(0)
            >= 1,
        "typing offers ranked matches",
    );
    tab.press_key("Enter").expect("select the top match");
    assert_eq!(
        eval(&tab, "window.CuteExploreLineage.highlightedId()"),
        serde_json::Value::String("model.shop.dim_orders".to_owned()),
        "search-select highlights the chosen model",
    );
    assert_eq!(
        eval(
            &tab,
            "document.activeElement.classList.contains('lineage-canvas')"
        ),
        serde_json::Value::Bool(true),
        "search-select hands focus to the canvas host (hard AC a)",
    );
    assert_eq!(
        eval(&tab, "'selectedModel' in document.body.dataset"),
        serde_json::Value::Bool(false),
        "highlight is exploratory — it must NOT write data-selected-model",
    );

    // --- 2. Space commits focus ------------------------------------------
    let scroll_before = eval(&tab, "window.scrollY");
    tab.press_key(" ").expect("press Space");
    assert_eq!(
        eval(&tab, "document.body.dataset.selectedModel"),
        serde_json::Value::String("model.shop.dim_orders".to_owned()),
        "Space commits focus: data-selected-model is written",
    );
    assert_eq!(
        eval(&tab, "window.scrollY"),
        scroll_before,
        "the Space handler preventDefaults — the page must not scroll (hard AC b)",
    );

    // --- 3. Space INSIDE the search input keeps typing ---------------------
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-search-input').focus()",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-search-input').value = ''",
    );
    tab.type_str("st").expect("type a second query");
    tab.press_key(" ").expect("press Space inside the input");
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('.lineage-search-input').value"
        ),
        serde_json::Value::String("st ".to_owned()),
        "Space in the focused search input types a space — never swallowed",
    );
    assert_eq!(
        eval(&tab, "document.body.dataset.selectedModel"),
        serde_json::Value::String("model.shop.dim_orders".to_owned()),
        "Space in the search input must NOT re-commit",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-search-input').blur()",
    );

    // --- 4. real pointer click highlights without committing ---------------
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-canvas').scrollIntoView({block:'center'})",
    );
    let coords = eval(
        &tab,
        "(function () { \
           var cy = window.CuteExploreLineage.cyInstance(); \
           var n = cy.getElementById('model.shop.stg_orders'); \
           var rp = n.renderedPosition(); \
           var r = cy.container().getBoundingClientRect(); \
           return JSON.stringify({x: r.left + rp.x, y: r.top + rp.y}); \
         })()",
    );
    let coords: serde_json::Value =
        serde_json::from_str(coords.as_str().expect("coords JSON")).expect("coords parse");
    tab.click_point(Point {
        x: coords["x"].as_f64().expect("x"),
        y: coords["y"].as_f64().expect("y"),
    })
    .expect("real pointer click on the node");
    assert_eq!(
        eval(&tab, "window.CuteExploreLineage.highlightedId()"),
        serde_json::Value::String("model.shop.stg_orders".to_owned()),
        "a pointer click highlights the node",
    );
    assert_eq!(
        eval(
            &tab,
            "window.CuteExploreLineage.cyInstance()\
               .getElementById('model.shop.lonely').hasClass('dim')"
        ),
        serde_json::Value::Bool(true),
        "the complement of the highlighted lineage dims",
    );
    assert_eq!(
        eval(&tab, "document.body.dataset.selectedModel"),
        serde_json::Value::String("model.shop.dim_orders".to_owned()),
        "click never commits — the previous Space commit stands",
    );
    tab.press_key(" ").expect("press Space after the click");
    assert_eq!(
        eval(&tab, "document.body.dataset.selectedModel"),
        serde_json::Value::String("model.shop.stg_orders".to_owned()),
        "the deliberate Space re-commits onto the clicked model",
    );

    // The whole session stayed console-clean (Discovery: the Cytoscape
    // wheelSensitivity notice and the deprecated `width: label` warning
    // are silenced by construction — neither knob is used).
    let noise = console_noise.lock().unwrap().clone();
    assert!(
        noise.is_empty(),
        "the explore page must emit zero console warnings/errors; got:\n{}",
        noise.join("\n"),
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn explore_dag_canvas_labels_stay_xss_safe_and_highlight_cost_is_subframe() {
    // cute-dbt#101 Discovery, both halves:
    //   - canvas-text label safety: a hostile model name (markup + an
    //     egress-attempting onerror) must draw as glyphs — zero <img>
    //     elements materialize, zero external requests fire, and the
    //     hostile string survives as element DATA.
    //   - highlight+dim cost on one large connected component (20 ranks x
    //     20 nodes = 400 models, ~760 edges): the full-lineage transitive
    //     highlight stays sub-frame. The measurement wraps the tap emit
    //     (handler runs synchronously; paint excluded, as in the epic's
    //     1.5 ms/click prototype number).
    //
    // The hostile name is deliberately DOT-FREE: the rendered label is
    // the id's last dotted segment (`leaf_segment`), so a dot inside the
    // payload would truncate the label and the oracle with it.
    let hostile = "evil</script><img src=x onerror=fetch('//y')>";
    let mut nodes = vec![explore_model(&format!("model.gen.{hostile}"), &[])];
    for layer in 0..20u32 {
        for i in 0..20u32 {
            let id = format!("model.gen.l{layer:02}_n{i:02}");
            let deps: Vec<String> = if layer == 0 {
                Vec::new()
            } else {
                vec![
                    format!("model.gen.l{:02}_n{:02}", layer - 1, i),
                    format!("model.gen.l{:02}_n{:02}", layer - 1, (i + 1) % 20),
                ]
            };
            let dep_refs: Vec<&str> = deps.iter().map(String::as_str).collect();
            nodes.push(explore_model(&id, &dep_refs));
        }
    }
    let url = render_explore_dag("explore-xss-cost", nodes);

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");
    tab.call_method(Network::Enable {
        max_total_buffer_size: None,
        max_resource_buffer_size: None,
        max_post_data_size: None,
        report_direct_socket_traffic: None,
        enable_durable_messages: None,
    })
    .expect("enable Network domain");
    let external = Arc::new(Mutex::new(Vec::<ExternalRequest>::new()));
    let external_recorder = external.clone();
    tab.add_event_listener(Arc::new(move |event: &Event| {
        if let Event::NetworkRequestWillBeSent(e) = event {
            let req_url = e.params.request.url.clone();
            if scheme_is_external(&req_url) {
                external_recorder.lock().unwrap().push(ExternalRequest {
                    url: req_url,
                    initiator_type: format!("{:?}", e.params.initiator.Type),
                    initiator_url: e.params.initiator.url.clone(),
                    initiator_line: e.params.initiator.line_number,
                });
            }
        }
    }))
    .expect("subscribe Network.requestWillBeSent");

    tab.navigate_to(&url).expect("navigate to file:// URL");
    tab.wait_until_navigated().expect("await navigation");
    tab.wait_for_element_with_custom_timeout(".lineage-canvas canvas", Duration::from_secs(30))
        .expect("the 401-node Cytoscape canvas boots offline");

    // --- XSS safety ---------------------------------------------------------
    assert_eq!(
        eval(&tab, "document.querySelectorAll('img').length"),
        serde_json::Value::from(0u64),
        "a hostile model name must never materialize an <img> element \
         (canvas-text labels draw glyphs, never parse HTML)",
    );
    let label = eval(
        &tab,
        "(function () { \
           var cy = window.CuteExploreLineage.cyInstance(); \
           var hit = cy.nodes().filter(function (n) { \
             return n.data('label').indexOf('evil') === 0; \
           }); \
           return hit.length ? hit[0].data('label') : null; \
         })()",
    );
    assert!(
        label.as_str().is_some_and(|l| l.contains("<img")),
        "the hostile name survives as canvas-label DATA: {label:?}",
    );

    // --- highlight+dim cost on the large component ---------------------------
    // Three witnesses: a root (successors = the whole component below), a
    // middle node, and a sink (predecessors = the whole component above).
    let cost = eval(
        &tab,
        "(function () { \
           var cy = window.CuteExploreLineage.cyInstance(); \
           var out = []; \
           ['model.gen.l00_n00', 'model.gen.l10_n10', 'model.gen.l19_n19']\
             .forEach(function (id) { \
               var t0 = performance.now(); \
               cy.getElementById(id).emit('tap'); \
               out.push(performance.now() - t0); \
             }); \
           return JSON.stringify(out); \
         })()",
    );
    let costs: Vec<f64> =
        serde_json::from_str(cost.as_str().expect("cost JSON")).expect("cost parse");
    println!("cute-dbt#101 highlight+dim cost on the 400-node component (ms): {costs:?}");
    let max = costs.iter().copied().fold(0.0f64, f64::max);
    assert!(
        max < 150.0,
        "full-lineage highlight+dim must stay interactive on a 400-node \
         component; measured {costs:?} ms (CI bound 150 ms; locally this \
         is single-digit)",
    );

    // --- zero egress held throughout -----------------------------------------
    let captured = external.lock().unwrap().clone();
    assert!(
        captured.is_empty(),
        "the hostile-name page must make zero external requests; got:\n{}",
        captured
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    );

    let _ = tab.close(true);
}

/// Evaluate `expr` in the page; panic on a thrown JS exception so a
/// missing selector can never silently pass the gate.
fn eval(tab: &headless_chrome::Tab, expr: &str) -> serde_json::Value {
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

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn every_committed_example_stays_zero_egress_with_the_cytoscape_engine_selected() {
    // cute-dbt#180 — the per-engine arm of the primary gate. The default
    // test above proves the Mermaid-rendered page; this one flips the
    // settings-panel DAG-engine picker to Cytoscape (the opt-in interactive
    // engine), waits for the live cy canvas, and asserts the SAME property:
    // zero external requests over a real file:// origin with DNS denied.
    // The picker path matters because the Cytoscape bundle + the cyto-dag
    // engine only EXECUTE after the flip — a static golden can never
    // exercise them (the dogfood guard from the Bucket-2 plan).
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
    let browser = Browser::new(opts).expect("Chromium must launch");

    let mut failures: Vec<String> = Vec::new();
    for filename in common::COMMITTED_EXAMPLES {
        let url = report_file_url(filename);
        assert!(
            url.starts_with("file://"),
            "zero-egress proof MUST run against a real file:// origin; got {url}",
        );
        let tab = browser.new_tab().expect("new tab");
        tab.call_method(Network::Enable {
            max_total_buffer_size: None,
            max_resource_buffer_size: None,
            max_post_data_size: None,
            report_direct_socket_traffic: None,
            enable_durable_messages: None,
        })
        .expect("enable Network domain");

        let external = Arc::new(Mutex::new(Vec::<ExternalRequest>::new()));
        let external_recorder = external.clone();
        tab.add_event_listener(Arc::new(move |event: &Event| {
            if let Event::NetworkRequestWillBeSent(e) = event {
                let req_url = e.params.request.url.clone();
                if scheme_is_external(&req_url) {
                    external_recorder.lock().unwrap().push(ExternalRequest {
                        url: req_url,
                        initiator_type: format!("{:?}", e.params.initiator.Type),
                        initiator_url: e.params.initiator.url.clone(),
                        initiator_line: e.params.initiator.line_number,
                    });
                }
            }
        }))
        .expect("subscribe Network.requestWillBeSent");

        tab.navigate_to(&url).expect("navigate to file:// URL");
        tab.wait_until_navigated().expect("await navigation");

        // Let the default Mermaid render settle first (the boot path),
        // then flip the picker to Cytoscape.
        let _ = tab
            .wait_for_element_with_custom_timeout(".cte-dag-mermaid svg", Duration::from_secs(15));
        let _ = eval(&tab, "document.querySelector('.settings-cog').click()");
        let _ = eval(
            &tab,
            "document.querySelector('.engine-seg button[data-engine=\"cytoscape\"]').click()",
        );
        let cyto_ok = tab
            .wait_for_element_with_custom_timeout(".cte-dag-cyto canvas", Duration::from_secs(15))
            .is_ok();

        let captured = external.lock().unwrap().clone();
        if !captured.is_empty() {
            let listing = captured
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            failures.push(format!(
                "examples/{filename} (cytoscape selected): {n} external request(s):\n{listing}",
                n = captured.len(),
            ));
        }
        if !cyto_ok {
            failures.push(format!(
                "examples/{filename}: the Cytoscape canvas never appeared inside \
                 .cte-dag-cyto after selecting the engine — either the inlined \
                 Cytoscape UMD bundle is broken offline or the picker wiring failed.",
            ));
        }
        let _ = tab.close(true);
    }

    assert!(
        failures.is_empty(),
        "zero-egress proof FAILED with the Cytoscape engine selected:\n{}",
        failures.join("\n"),
    );
}
