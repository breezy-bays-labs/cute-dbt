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
    Checksum, DependsOn, InScopeSet, Manifest, ManifestMetadata, ModelInScopeSet, Node, NodeConfig,
    NodeId, TestMetadata, all_models,
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
            // cute-dbt#102 — the unit-test viewer booted: explore-tests.js
            // populated the shared partial's selector from the payload
            // (the playground fixture carries unit tests).
            let viewer_ok = eval(
                &tab,
                "document.querySelectorAll('#test-select option').length",
            )
            .as_u64()
            .unwrap_or(0)
                > 0;
            if !viewer_ok {
                failures.push(format!(
                    "examples/{filename}: the unit-test viewer did not populate \
                     #test-select — explore-tests.js failed to boot offline.",
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

/// A synthetic model node with explicit `depends_on.nodes` edges and an
/// explicit compiled body (`None` = the fail-open `dbt parse` shape) —
/// the in-process explore-page harness's base builder.
fn explore_node(id: &str, deps: &[&str], compiled: Option<&str>) -> Node {
    Node::new(
        NodeId::new(id),
        "model",
        Checksum::new("sha256", "ck"),
        compiled.map(str::to_owned),
        None,
        DependsOn::new(Vec::new(), deps.iter().map(|d| NodeId::new(*d)).collect()),
        None,
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
}

/// A synthetic compiled model node (`select 1` body — no CTE structure).
fn explore_model(id: &str, deps: &[&str]) -> Node {
    explore_node(id, deps, Some("select 1"))
}

/// Render the explore pages in-process (the headless fixture discipline:
/// domain objects -> `render_explore`, no subprocess) and return the
/// out directory. `unit_tests` is keyed by manifest unit-test id.
/// `changed` is the optional cute-dbt#106 change context (`None` = the
/// no-`--pr-diff` render).
fn render_explore_pages(
    stem: &str,
    nodes: Vec<Node>,
    unit_tests: HashMap<String, cute_dbt::domain::UnitTest>,
    changed: Option<&ModelInScopeSet>,
) -> PathBuf {
    let manifest = Manifest::new(
        ManifestMetadata::new("v12"),
        nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
        unit_tests,
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
    render_explore(&dir, &manifest, &models, changed, &payload).expect("explore renders");
    dir
}

/// Render an explore page in-process and return the `file://` URL of
/// the emitted `dag.html`.
fn render_explore_dag(stem: &str, nodes: Vec<Node>) -> String {
    let dir = render_explore_pages(stem, nodes, HashMap::new(), None);
    let p = dir.join("dag.html");
    format!("file://{}", p.to_str().expect("page path is valid UTF-8"))
}

/// Render an explore dag.html with cute-dbt#106 change context and
/// return its `file://` URL. `changed_ids` are full model node ids.
fn render_explore_dag_with_context(stem: &str, nodes: Vec<Node>, changed_ids: &[&str]) -> String {
    let changed: ModelInScopeSet = changed_ids.iter().map(|id| NodeId::new(*id)).collect();
    let dir = render_explore_pages(stem, nodes, HashMap::new(), Some(&changed));
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

    // cute-dbt#103 — every node renders the test-count badge as the
    // canvas label's second line, EXPLICIT at 0/0 (these synthetic
    // models carry no tests).
    assert_eq!(
        eval(
            &tab,
            "window.CuteExploreLineage.cyInstance()\
               .getElementById('model.shop.stg_orders').data('label')"
        ),
        serde_json::Value::String("stg_orders\n0 data-tests \u{b7} 0 unit-tests".to_owned()),
        "the 0/0 test-count badge rides the canvas label",
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

// ===== cute-dbt#104 — model-detail card (highlight) + hover tooltip =====

/// A described / tagged / configured model with a meta grain and one
/// described column — the cute-dbt#104 detail-card input shape
/// (in-process domain objects, the headless fixture discipline).
fn detailed_explore_model(id: &str) -> Node {
    let mut config: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    config.insert("materialized".to_owned(), serde_json::json!("table"));
    config.insert(
        "meta".to_owned(),
        serde_json::json!({ "grain": "order_id + order_date", "owner": "analytics" }),
    );
    let mut columns: BTreeMap<String, Option<String>> = BTreeMap::new();
    columns.insert("order_id".to_owned(), Some("varchar".to_owned()));
    let mut descriptions = BTreeMap::new();
    descriptions.insert("order_id".to_owned(), "Primary key.".to_owned());
    Node::new(
        NodeId::new(id),
        "model",
        Checksum::new("sha256", "ck"),
        Some("select 1".to_owned()),
        None,
        DependsOn::default(),
        None,
        NodeConfig::new(config, false),
        None,
        columns,
    )
    .with_column_descriptions(descriptions)
    .with_model_metadata(
        Some("One row per order.".to_owned()),
        vec!["marts".to_owned()],
    )
}

/// A native `unique` data test attached to `target` — the inferred-grain
/// signal that must ride the card's "all detected" list.
fn unique_test_node(id: &str, target: &str, column: &str) -> Node {
    Node::new(
        NodeId::new(id),
        "test",
        Checksum::new("none", ""),
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
        Some(NodeId::new(target)),
        Some(TestMetadata::new(
            "unique",
            None,
            serde_json::json!({ "column_name": column }),
        )),
    )
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn explore_dag_detail_card_renders_on_highlight_and_tooltip_never_steals_it() {
    // cute-dbt#104 — the HIGHLIGHT-card / hover-tooltip contract, driven
    // through the real CDP input pipeline. Proof points, in order:
    //   1. the pristine page shows neither card nor tooltip.
    //   2. a real pointer click HIGHLIGHTS a model -> the detail card
    //      opens with description, materialization, tags, meta, the
    //      resolved grain (+ source + every detected signal) and the
    //      described columns.
    //   3. hovering ANOTHER node shows the transient tooltip with its
    //      key facts — and does NOT change the highlighted model, does
    //      NOT retarget the card, and never writes the focus-commit
    //      attribute (document.body.dataset.selectedModel stays absent:
    //      commitFocus is the single write site).
    //   4. moving the pointer off the node hides the tooltip.
    //   5. highlighting a signal-less model renders the grain
    //      EXPLICITLY as "unknown" (never silently guessed).
    //   6. clearing the highlight (background tap) hides the card.
    // Rider: the whole session emits zero console warnings/errors.
    let dir = render_explore_pages(
        "explore-detail-card",
        vec![
            detailed_explore_model("model.shop.dim_orders"),
            explore_model("model.shop.mystery", &[]),
            unique_test_node(
                "test.shop.unique_dim_orders_order_id",
                "model.shop.dim_orders",
                "order_id",
            ),
        ],
        HashMap::new(),
        None,
    );
    let page = dir.join("dag.html");
    let url = format!("file://{}", page.to_str().expect("page path is UTF-8"));

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");

    // Console recorder (the cute-dbt#101 console-clean rider).
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

    // --- 1. pristine: no card, no tooltip --------------------------------
    assert_eq!(
        eval(&tab, "document.querySelector('.model-detail-card').hidden"),
        serde_json::Value::Bool(true),
        "the detail card starts hidden",
    );
    assert_eq!(
        eval(&tab, "document.querySelector('.lineage-tooltip').hidden"),
        serde_json::Value::Bool(true),
        "the tooltip starts hidden",
    );

    // Rendered coordinates of a node, page-relative.
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-canvas').scrollIntoView({block:'center'})",
    );
    let node_point = |id: &str| -> Point {
        let coords = eval(
            &tab,
            &format!(
                "(function () {{ \
                   var cy = window.CuteExploreLineage.cyInstance(); \
                   var n = cy.getElementById('{id}'); \
                   var rp = n.renderedPosition(); \
                   var r = cy.container().getBoundingClientRect(); \
                   return JSON.stringify({{x: r.left + rp.x, y: r.top + rp.y}}); \
                 }})()"
            ),
        );
        let coords: serde_json::Value =
            serde_json::from_str(coords.as_str().expect("coords JSON")).expect("coords parse");
        Point {
            x: coords["x"].as_f64().expect("x"),
            y: coords["y"].as_f64().expect("y"),
        }
    };

    // --- 2. click highlights -> the full detail card ----------------------
    tab.click_point(node_point("model.shop.dim_orders"))
        .expect("real pointer click on dim_orders");
    assert_eq!(
        eval(&tab, "window.CuteExploreLineage.highlightedId()"),
        serde_json::Value::String("model.shop.dim_orders".to_owned()),
        "the click highlights dim_orders",
    );
    assert_eq!(
        eval(&tab, "document.querySelector('.model-detail-card').hidden"),
        serde_json::Value::Bool(false),
        "highlighting opens the detail card",
    );
    let card_text = eval(
        &tab,
        "document.querySelector('.model-detail-card').textContent",
    );
    let card_text = card_text.as_str().expect("card text").to_owned();
    for expected in [
        "dim_orders",
        "One row per order.",                   // description
        "table",                                // materialization
        "marts",                                // tag
        "owner",                                // meta key
        "analytics",                            // meta value
        "order_id + order_date",                // resolved grain (meta rung)
        "config.meta.grain",                    // grain source
        "unique test",                          // the also-detected inferred signal
        "test.shop.unique_dim_orders_order_id", // its origin
        "Primary key.",                         // column description
        "varchar",                              // column type
    ] {
        assert!(
            card_text.contains(expected),
            "the detail card must show {expected:?}; got:\n{card_text}",
        );
    }

    // --- 3. hovering another node = transient tooltip, highlight intact ---
    // Park the pointer off-node first so the move ONTO the node emits a
    // clean mouseover.
    let mystery_point = node_point("model.shop.mystery");
    tab.move_mouse_to_point(Point {
        x: mystery_point.x,
        y: (mystery_point.y - 60.0).max(1.0),
    })
    .expect("park the pointer off-node");
    tab.move_mouse_to_point(mystery_point)
        .expect("hover the mystery node");
    assert_eq!(
        eval(&tab, "document.querySelector('.lineage-tooltip').hidden"),
        serde_json::Value::Bool(false),
        "hovering a node shows the tooltip",
    );
    let tip_text = eval(
        &tab,
        "document.querySelector('.lineage-tooltip').textContent",
    );
    let tip_text = tip_text.as_str().expect("tooltip text").to_owned();
    assert!(
        tip_text.contains("mystery") && tip_text.contains("grain: unknown"),
        "the tooltip carries the hovered node's key facts (grain explicit \
         even when unknown); got:\n{tip_text}",
    );
    // The tooltip is TRANSIENT: highlight unchanged, card untouched,
    // and the focus-commit attribute never written.
    assert_eq!(
        eval(&tab, "window.CuteExploreLineage.highlightedId()"),
        serde_json::Value::String("model.shop.dim_orders".to_owned()),
        "hover must NOT change the highlighted model",
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('.model-detail-card h2').textContent"
        ),
        serde_json::Value::String("dim_orders".to_owned()),
        "hover must NOT retarget the detail card",
    );
    assert_eq!(
        eval(&tab, "'selectedModel' in document.body.dataset"),
        serde_json::Value::Bool(false),
        "hover must NEVER write data-selected-model (commitFocus stays \
         the single write site)",
    );

    // --- 4. moving off the node hides the tooltip --------------------------
    tab.move_mouse_to_point(Point {
        x: mystery_point.x,
        y: (mystery_point.y - 60.0).max(1.0),
    })
    .expect("move the pointer off the node");
    assert_eq!(
        eval(&tab, "document.querySelector('.lineage-tooltip').hidden"),
        serde_json::Value::Bool(true),
        "the tooltip hides on mouseout (transient, never sticky)",
    );

    // --- 5. a signal-less model renders the explicit unknown grain ---------
    tab.click_point(node_point("model.shop.mystery"))
        .expect("click the mystery node");
    let card_text = eval(
        &tab,
        "document.querySelector('.model-detail-card').textContent",
    );
    let card_text = card_text.as_str().expect("card text").to_owned();
    assert!(
        card_text.contains("mystery") && card_text.contains("unknown"),
        "a model with no grain signal renders the grain EXPLICITLY as \
         unknown; got:\n{card_text}",
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('.model-detail-card .detail-grain-unknown')\
               .textContent"
        ),
        serde_json::Value::String("unknown".to_owned()),
        "the unknown grain renders through its dedicated affordance",
    );

    // --- 6. clearing the highlight hides the card ----------------------------
    // A background tap clears the highlight. The canvas top-left corner
    // (inside the fit padding) is reliably node-free.
    let corner = eval(
        &tab,
        "(function () { \
           var r = document.querySelector('.lineage-canvas').getBoundingClientRect(); \
           return JSON.stringify({x: r.left + 8, y: r.top + 8}); \
         })()",
    );
    let corner: serde_json::Value =
        serde_json::from_str(corner.as_str().expect("corner JSON")).expect("corner parse");
    tab.click_point(Point {
        x: corner["x"].as_f64().expect("x"),
        y: corner["y"].as_f64().expect("y"),
    })
    .expect("background tap");
    assert_eq!(
        eval(&tab, "window.CuteExploreLineage.highlightedId()"),
        serde_json::Value::Null,
        "the background tap clears the highlight",
    );
    assert_eq!(
        eval(&tab, "document.querySelector('.model-detail-card').hidden"),
        serde_json::Value::Bool(true),
        "clearing the highlight hides the detail card",
    );

    let noise = console_noise.lock().unwrap().clone();
    assert!(
        noise.is_empty(),
        "the detail-card session must emit zero console warnings/errors; got:\n{}",
        noise.join("\n"),
    );

    let _ = tab.close(true);
}

// ===== cute-dbt#102 — the CTE ⇄ model view toggle + the tests.html =====
// ===== unit-test viewer                                            =====

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn explore_dag_cte_toggle_renders_cte_view_and_preserves_lineage_state() {
    // cute-dbt#102 — the CTE ⇄ model view toggle, driven end to end:
    //   1. boot: the CTE arm is DISABLED (no highlight yet).
    //   2. a search-select highlight enables it.
    //   3. clicking the CTE arm renders the highlighted model's CTE DAG
    //      with the page's Cytoscape + dagre engine (nodes + the
    //      join-typed edge off the cte_dags carrier).
    //   4. toggling back to lineage reveals the SAME lineage instance —
    //      its highlight classes survived (chrome + selection persist;
    //      local state, same page, no reload).
    //   5. highlighting the uncompiled model and re-entering the CTE
    //      view renders the labeled fail-open degraded view — never an
    //      error.
    // Riders: zero console warnings/errors and zero external requests
    // for the whole session.
    let cte_sql = "with src_orders as (select * from db.sch.raw_orders) \
                   select * from src_orders";
    let url = render_explore_dag(
        "explore-cte-toggle",
        vec![
            explore_node("model.shop.dim_orders", &[], Some(cte_sql)),
            explore_model("model.shop.stg_orders", &[]),
            // The fail-open witness: no compiled SQL (dbt parse).
            explore_node("model.shop.mart_orders", &["model.shop.dim_orders"], None),
        ],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");

    // Console + network recorders, subscribed BEFORE navigate.
    tab.call_method(Runtime::Enable(None))
        .expect("enable Runtime domain");
    tab.call_method(headless_chrome::protocol::cdp::Log::Enable(None))
        .expect("enable Log domain");
    let console_noise = Arc::new(Mutex::new(Vec::<String>::new()));
    let console_recorder = console_noise.clone();
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
        Event::NetworkRequestWillBeSent(e) => {
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
        _ => {}
    }))
    .expect("subscribe console + network events");

    tab.navigate_to(&url).expect("navigate to file:// URL");
    tab.wait_until_navigated().expect("await navigation");
    tab.wait_for_element_with_custom_timeout(".lineage-canvas canvas", Duration::from_secs(15))
        .expect("the lineage Cytoscape canvas boots offline");

    // --- 1. boot: the CTE arm is gated --------------------------------------
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('.view-toggle [data-view=\"cte\"]').disabled"
        ),
        serde_json::Value::Bool(true),
        "the CTE arm starts disabled — no model is highlighted yet",
    );

    // --- 2. a search-select highlight enables it ------------------------------
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-search-input').focus()",
    );
    tab.type_str("dim").expect("type the query");
    tab.press_key("Enter").expect("select the top match");
    assert_eq!(
        eval(&tab, "window.CuteExploreLineage.highlightedId()"),
        serde_json::Value::String("model.shop.dim_orders".to_owned()),
        "search-select highlights dim_orders",
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('.view-toggle [data-view=\"cte\"]').disabled"
        ),
        serde_json::Value::Bool(false),
        "the highlight unlocks the CTE arm",
    );

    // --- 3. the CTE view renders the highlighted model's CTE DAG --------------
    let _ = eval(
        &tab,
        "document.querySelector('.view-toggle [data-view=\"cte\"]').click()",
    );
    tab.wait_for_element_with_custom_timeout(
        ".cte-view .cte-canvas canvas",
        Duration::from_secs(15),
    )
    .expect("the CTE Cytoscape canvas renders offline");
    assert_eq!(
        eval(&tab, "window.CuteExploreCte.activeView()"),
        serde_json::Value::String("cte".to_owned()),
        "the active view is the CTE view",
    );
    assert_eq!(
        eval(&tab, "window.CuteExploreCte.renderedModelId()"),
        serde_json::Value::String("model.shop.dim_orders".to_owned()),
        "the CTE view binds to the HIGHLIGHTED model",
    );
    assert_eq!(
        eval(
            &tab,
            "JSON.stringify([window.CuteExploreCte.cyInstance().nodes().length, \
              window.CuteExploreCte.cyInstance().edges().length])"
        ),
        serde_json::Value::String("[2,1]".to_owned()),
        "the CTE DAG carries the src_orders CTE + the terminal and one edge",
    );
    assert_eq!(
        eval(&tab, "document.querySelector('.lineage-view').hidden"),
        serde_json::Value::Bool(true),
        "the lineage host hides while the CTE view is active (same page)",
    );

    // --- 4. toggling back preserves the lineage instance + highlight ----------
    let _ = eval(
        &tab,
        "document.querySelector('.view-toggle [data-view=\"lineage\"]').click()",
    );
    assert_eq!(
        eval(&tab, "document.querySelector('.lineage-view').hidden"),
        serde_json::Value::Bool(false),
        "the lineage view returns",
    );
    assert_eq!(
        eval(
            &tab,
            "window.CuteExploreLineage.cyInstance()\
               .getElementById('model.shop.dim_orders').hasClass('sel')"
        ),
        serde_json::Value::Bool(true),
        "the lineage highlight SURVIVES the round trip — the instance \
         was never rebuilt (selection persists)",
    );

    // --- 5. the uncompiled model renders the labeled degraded view ------------
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-search-input').focus()",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-search-input').value = ''",
    );
    tab.type_str("mart").expect("type the second query");
    tab.press_key("Enter").expect("select the uncompiled model");
    let _ = eval(
        &tab,
        "document.querySelector('.view-toggle [data-view=\"cte\"]').click()",
    );
    assert_eq!(
        eval(&tab, "document.querySelector('.cte-degraded').hidden"),
        serde_json::Value::Bool(false),
        "an uncompiled model shows the labeled fail-open degraded view",
    );
    let degraded = eval(&tab, "document.querySelector('.cte-degraded').textContent");
    assert!(
        degraded
            .as_str()
            .is_some_and(|t| t.contains("mart_orders") && t.contains("dbt compile")),
        "the degraded view names the model + the remediation: {degraded:?}",
    );
    assert_eq!(
        eval(&tab, "document.querySelector('.cte-canvas').hidden"),
        serde_json::Value::Bool(true),
        "no canvas pretends to be a DAG on the degraded view",
    );

    // --- riders: console-clean + zero egress ----------------------------------
    let noise = console_noise.lock().unwrap().clone();
    assert!(
        noise.is_empty(),
        "the toggle session must emit zero console warnings/errors; got:\n{}",
        noise.join("\n"),
    );
    let captured = external.lock().unwrap().clone();
    assert!(
        captured.is_empty(),
        "the toggle session must make zero external requests; got:\n{}",
        captured
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn explore_tests_viewer_renders_fixture_grids_offline() {
    // cute-dbt#102 — the tests.html unit-test viewer: the shared
    // test-card partial filled by explore-tests.js from the embedded
    // payload. Proof points: the selector populates, the given grid
    // renders the Rust FixtureTable POD (incl. the NULL-cell
    // vocabulary), the expected row count lands, and the index row's
    // jump button drives the viewer. Riders: zero console noise, zero
    // external requests.
    use cute_dbt::domain::{UnitTest, UnitTestExpect, UnitTestGiven};

    let given = UnitTestGiven::new(
        "ref('stg_orders')",
        serde_json::json!([
            {"id": 1, "name": "a"},
            {"id": 2, "name": null}
        ]),
        Some("dict".to_owned()),
        None,
    );
    let ut = UnitTest::new(
        "test_dim_orders_grain",
        NodeId::new("dim_orders"),
        vec![given],
        UnitTestExpect::new(serde_json::json!([{"id": 1}]), None, None),
        Some("one row per order".to_owned()),
        DependsOn::default(),
        None,
        None,
        None,
    );
    let mut unit_tests = HashMap::new();
    unit_tests.insert(
        "unit_test.shop.dim_orders.test_dim_orders_grain".to_owned(),
        ut,
    );
    let dir = render_explore_pages(
        "explore-tests-viewer",
        vec![
            explore_model("model.shop.dim_orders", &[]),
            explore_model("model.shop.stg_orders", &[]),
        ],
        unit_tests,
        None,
    );
    let p = dir.join("tests.html");
    let url = format!("file://{}", p.to_str().expect("page path is valid UTF-8"));

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

    // The selector populated from the payload.
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('#test-select option').length"
        ),
        serde_json::Value::from(1u64),
        "the unit-test selector lists the one declared test",
    );
    // The given grid renders the FixtureTable POD: 2 rows x 2 columns,
    // with the NULL cell in the report's vocabulary.
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.left-panel-body table.given-table tbody td').length"
        ),
        serde_json::Value::from(4u64),
        "the given grid renders 2x2 cells",
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('.left-panel-body td.cell-null').textContent"
        ),
        serde_json::Value::String("NULL".to_owned()),
        "a null fixture value renders the NULL cell affordance",
    );
    // The expected panel's row count.
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('.expected-rowcount').textContent"
        ),
        serde_json::Value::String("1 row".to_owned()),
        "the expected row count lands in the panel header",
    );
    // The description landed and is visible.
    assert_eq!(
        eval(&tab, "document.querySelector('.test-description').hidden"),
        serde_json::Value::Bool(false),
        "the authored description is visible",
    );
    // The index row's jump button drives the viewer (same test here —
    // the click must keep the card coherent, not throw).
    let _ = eval(
        &tab,
        "document.querySelector('.test-jump[data-test-id]').click()",
    );
    assert_eq!(
        eval(&tab, "document.getElementById('test-select').value"),
        serde_json::Value::String("unit_test.shop.dim_orders.test_dim_orders_grain".to_owned()),
        "the jump button selects its test in the viewer",
    );
    // No graph engine on this page.
    assert_eq!(
        eval(&tab, "typeof window.cytoscape"),
        serde_json::Value::String("undefined".to_owned()),
        "tests.html ships no Cytoscape global",
    );

    let captured = external.lock().unwrap().clone();
    assert!(
        captured.is_empty(),
        "the tests.html viewer must make zero external requests; got:\n{}",
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

// ===== cute-dbt#105 — the external-drive contract: host bridge      =====
// ===== (postMessage + DOM attr dual binding), focusModel/setView,   =====
// ===== payload file paths                                           =====

/// A compiled model node with the full path complement (SQL source +
/// schema YAML patch) — the commit event's `paths` witness.
fn pathed_explore_model(id: &str, sql: &str, schema_yaml: &str) -> Node {
    Node::new(
        NodeId::new(id),
        "model",
        Checksum::new("sha256", "ck"),
        Some("select 1".to_owned()),
        None,
        DependsOn::default(),
        Some(sql.to_owned()),
        NodeConfig::default(),
        None,
        BTreeMap::new(),
    )
    .with_patch_path(Some(schema_yaml.to_owned()))
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn explore_dag_host_bridge_dual_binds_the_commit_and_forward_hooks_never_echo() {
    // cute-dbt#105 — the external-drive contract, driven end to end with
    // an INJECTED fake host bridge (window.cuteDbtHostBridge, planted via
    // Page.addScriptToEvaluateOnNewDocument so it exists BEFORE the page
    // scripts parse — the detection-at-boot contract). The proof points:
    //   1. the contract surface: <body data-cute-dbt-contract> is
    //      server-rendered, window.cuteDbtContract mirrors it (the
    //      attribute is the single source), both hooks are callable.
    //   2. focusModel(id) highlights + centers and NOTHING else — no
    //      data-selected-model write, ZERO bridge events (the NO-ECHO
    //      rule: host-pushed editor sync never bounces back as a
    //      commit). An unknown id returns false (fail-open).
    //   3. setView(kind) switches lineage ⇄ cte with the V3 vocabulary;
    //      a bogus kind returns false.
    //   4. search-select and a real pointer click highlight WITHOUT a
    //      single bridge event.
    //   5. Space commits DUAL-BOUND: the attribute writes AND exactly
    //      one versioned commit event arrives — type, contractVersion,
    //      modelId, the active view, and the committed node's
    //      project-relative paths.
    //   6. a second commit from the CTE view carries view: "cte".
    // Rider: the whole session emits zero console warnings/errors.
    let url = render_explore_dag(
        "explore-host-bridge",
        vec![
            pathed_explore_model(
                "model.shop.stg_orders",
                "models/staging/stg_orders.sql",
                "models/staging/_staging__models.yml",
            ),
            explore_model("model.shop.dim_orders", &["model.shop.stg_orders"]),
            explore_model("model.shop.lonely", &[]),
        ],
    );

    let browser = launch_browser();
    let tab = browser.new_tab().expect("new tab");

    // The fake host bridge, injected BEFORE any page script parses —
    // the same seam a VS Code webview or cmux host uses. Pure recorder:
    // no network-adjacent calls.
    tab.call_method(
        headless_chrome::protocol::cdp::Page::AddScriptToEvaluateOnNewDocument {
            source: "window.__cuteBridgeEvents = []; \
                     window.cuteDbtHostBridge = { postMessage: function (m) { \
                       window.__cuteBridgeEvents.push(m); } };"
                .to_owned(),
            world_name: None,
            include_command_line_api: None,
            run_immediately: None,
        },
    )
    .expect("inject the fake host bridge");

    // Console recorder (warnings + errors), subscribed BEFORE navigate.
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

    // --- 1. the contract surface ---------------------------------------------
    assert_eq!(
        eval(&tab, "document.body.dataset.cuteDbtContract"),
        serde_json::Value::String("1".to_owned()),
        "the contract version is server-rendered on <body>",
    );
    assert_eq!(
        eval(&tab, "window.cuteDbtContract.version"),
        serde_json::Value::String("1".to_owned()),
        "the JS global mirrors the attribute (the single source)",
    );
    assert_eq!(
        eval(
            &tab,
            "typeof window.focusModel === 'function' && typeof window.setView === 'function'"
        ),
        serde_json::Value::Bool(true),
        "both forward hooks are callable",
    );

    // --- 2. focusModel: highlight + center, NO echo ---------------------------
    assert_eq!(
        eval(&tab, "window.focusModel('model.shop.stg_orders')"),
        serde_json::Value::Bool(true),
        "focusModel resolves a known id",
    );
    assert_eq!(
        eval(&tab, "window.CuteExploreLineage.highlightedId()"),
        serde_json::Value::String("model.shop.stg_orders".to_owned()),
        "focusModel highlights the model",
    );
    assert_eq!(
        eval(
            &tab,
            "window.CuteExploreLineage.cyInstance()\
               .getElementById('model.shop.lonely').hasClass('dim')"
        ),
        serde_json::Value::Bool(true),
        "focusModel dims the complement (the same highlight a click drives)",
    );
    assert_eq!(
        eval(&tab, "'selectedModel' in document.body.dataset"),
        serde_json::Value::Bool(false),
        "focusModel must NOT write data-selected-model (the no-echo rule)",
    );
    assert_eq!(
        eval(&tab, "window.__cuteBridgeEvents.length"),
        serde_json::Value::Number(0.into()),
        "focusModel must NOT post a bridge event (the no-echo rule)",
    );
    assert_eq!(
        eval(&tab, "window.focusModel('model.shop.no_such_model')"),
        serde_json::Value::Bool(false),
        "an unknown id is a fail-open no-op returning false",
    );

    // --- 3. setView: programmatic view switch ---------------------------------
    assert_eq!(
        eval(&tab, "window.setView('cte')"),
        serde_json::Value::Bool(true),
        "setView('cte') succeeds while a model is highlighted",
    );
    assert_eq!(
        eval(&tab, "window.CuteExploreCte.activeView()"),
        serde_json::Value::String("cte".to_owned()),
        "the CTE view is active",
    );
    assert_eq!(
        eval(&tab, "window.setView('lineage')"),
        serde_json::Value::Bool(true),
        "setView('lineage') switches back",
    );
    assert_eq!(
        eval(&tab, "window.CuteExploreCte.activeView()"),
        serde_json::Value::String("lineage".to_owned()),
        "the lineage view is active again",
    );
    assert_eq!(
        eval(&tab, "window.setView('bogus')"),
        serde_json::Value::Bool(false),
        "an unknown view kind is rejected with false",
    );

    // --- 4. search-select + pointer click: zero bridge events -----------------
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-search-input').focus()",
    );
    tab.type_str("stg").expect("type the query");
    tab.press_key("Enter").expect("select the top match");
    assert_eq!(
        eval(&tab, "window.CuteExploreLineage.highlightedId()"),
        serde_json::Value::String("model.shop.stg_orders".to_owned()),
        "search-select highlights",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-canvas').scrollIntoView({block:'center'})",
    );
    // The earlier focusModel/center calls panned the viewport — re-fit
    // so every node's rendered position is inside the canvas before
    // computing the click point.
    let _ = eval(
        &tab,
        "(function () { window.CuteExploreLineage.cyInstance().fit(); return true; })()",
    );
    let coords = eval(
        &tab,
        "(function () { \
           var cy = window.CuteExploreLineage.cyInstance(); \
           var n = cy.getElementById('model.shop.dim_orders'); \
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
        serde_json::Value::String("model.shop.dim_orders".to_owned()),
        "the pointer click highlights",
    );
    assert_eq!(
        eval(&tab, "window.__cuteBridgeEvents.length"),
        serde_json::Value::Number(0.into()),
        "neither search-select nor click posts a bridge event",
    );
    assert_eq!(
        eval(&tab, "'selectedModel' in document.body.dataset"),
        serde_json::Value::Bool(false),
        "neither search-select nor click writes the attribute",
    );

    // --- 5. Space commits dual-bound -------------------------------------------
    // Re-target the pathed model so the event's paths block is the
    // witness; the search-select hands focus to the canvas host.
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-search-input').focus()",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-search-input').value = ''",
    );
    tab.type_str("stg").expect("re-type the query");
    tab.press_key("Enter").expect("re-select stg_orders");
    tab.press_key(" ").expect("press Space");
    assert_eq!(
        eval(&tab, "document.body.dataset.selectedModel"),
        serde_json::Value::String("model.shop.stg_orders".to_owned()),
        "the Space commit writes the attribute (binding one)",
    );
    let events = eval(&tab, "JSON.stringify(window.__cuteBridgeEvents)");
    let events: serde_json::Value =
        serde_json::from_str(events.as_str().expect("events JSON")).expect("events parse");
    let events = events.as_array().expect("events array");
    assert_eq!(events.len(), 1, "exactly one commit event: {events:?}");
    let event = &events[0];
    assert_eq!(event["type"], "cute-dbt/commit", "{event}");
    assert_eq!(event["contractVersion"], "1", "{event}");
    assert_eq!(event["modelId"], "model.shop.stg_orders", "{event}");
    assert_eq!(event["view"], "lineage", "{event}");
    assert_eq!(
        event["paths"]["sql"], "models/staging/stg_orders.sql",
        "the committed node's project-relative SQL path rides the event: {event}",
    );
    assert_eq!(
        event["paths"]["schema_yaml"], "models/staging/_staging__models.yml",
        "{event}",
    );
    assert!(
        event["paths"]["unit_tests"].is_array(),
        "the paths shape is complete: {event}",
    );

    // --- 6. a CTE-view commit carries view: "cte" -------------------------------
    assert_eq!(
        eval(&tab, "window.setView('cte')"),
        serde_json::Value::Bool(true),
        "switch to the CTE view for the second commit",
    );
    tab.press_key(" ").expect("press Space in the CTE view");
    let second = eval(
        &tab,
        "JSON.stringify(window.__cuteBridgeEvents[window.__cuteBridgeEvents.length - 1])",
    );
    let second: serde_json::Value =
        serde_json::from_str(second.as_str().expect("event JSON")).expect("event parse");
    assert_eq!(
        second["view"], "cte",
        "the commit event carries the ACTIVE view: {second}",
    );

    // The whole session stayed console-clean.
    let noise = console_noise.lock().unwrap().clone();
    assert!(
        noise.is_empty(),
        "the host-bridge session must emit zero console warnings/errors; got:\n{}",
        noise.join("\n"),
    );

    let _ = tab.close(true);

    // --- standalone (no bridge): the attribute is the only binding -------------
    // A SECOND tab without the injected bridge: detection finds nothing,
    // the page behaves exactly as before — Space writes the attribute,
    // the contract surface still reads, no bridge global materializes.
    let tab = browser.new_tab().expect("standalone tab");
    tab.navigate_to(&url).expect("navigate to file:// URL");
    tab.wait_until_navigated().expect("await navigation");
    tab.wait_for_element_with_custom_timeout(".lineage-canvas canvas", Duration::from_secs(15))
        .expect("the Cytoscape canvas boots offline (standalone)");
    assert_eq!(
        eval(
            &tab,
            "typeof window.cuteDbtHostBridge === 'undefined' \
             && typeof window.acquireVsCodeApi === 'undefined'"
        ),
        serde_json::Value::Bool(true),
        "standalone file:// has no host bridge to detect",
    );
    assert_eq!(
        eval(&tab, "window.cuteDbtContract.version"),
        serde_json::Value::String("1".to_owned()),
        "the contract surface still reads standalone",
    );
    let _ = eval(
        &tab,
        "document.querySelector('.lineage-search-input').focus()",
    );
    tab.type_str("dim").expect("type the query (standalone)");
    tab.press_key("Enter")
        .expect("select the match (standalone)");
    tab.press_key(" ").expect("press Space (standalone)");
    assert_eq!(
        eval(&tab, "document.body.dataset.selectedModel"),
        serde_json::Value::String("model.shop.dim_orders".to_owned()),
        "standalone Space writes the attribute — the dual binding's \
         always-on half; zero behavior change without a host",
    );

    let _ = tab.close(true);
}

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn explore_dag_change_context_marks_changed_nodes_and_composes_with_highlight() {
    // cute-dbt#106 — PR-diff change context on the live Cytoscape page.
    // The proof points:
    //   1. a NO-context render (no --pr-diff) carries ZERO changed
    //      markings: no `[changed = 1]` nodes, no amber underlay, no
    //      legend chip.
    //   2. a contexted render marks EXACTLY the expected nodes: the
    //      changed node carries data(changed)=1 + a visible underlay;
    //      every other node carries neither. The full graph still
    //      renders every model (context never narrows scope) and the
    //      banner counts the changed models.
    //   3. the changed treatment COMPOSES with highlight: highlighting
    //      the changed node adds .sel (the magenta border channel)
    //      while the underlay glow stays on — and the detail card shows
    //      the "changed in this diff" chip. Highlighting a DIFFERENT
    //      node dims the changed one (context never fights emphasis)
    //      without dropping its data flag.
    //   4. bridge/commit semantics are untouched: highlight alone never
    //      writes data-selected-model; the deliberate Space commit
    //      still writes it.
    // Rider: the whole session emits zero console warnings/errors.
    let nodes = || {
        vec![
            explore_model("model.shop.stg_orders", &[]),
            explore_model("model.shop.dim_orders", &["model.shop.stg_orders"]),
            explore_model("model.shop.mart_orders", &["model.shop.dim_orders"]),
            // Disconnected — the dim-composition witness.
            explore_model("model.shop.lonely", &[]),
        ]
    };

    let browser = launch_browser();

    // --- 1. the no-context render carries zero changed markings -----------
    let plain_url = render_explore_dag("explore-change-context-plain", nodes());
    let tab = browser.new_tab().expect("new tab (plain)");
    tab.navigate_to(&plain_url).expect("navigate (plain)");
    tab.wait_until_navigated()
        .expect("await navigation (plain)");
    tab.wait_for_element_with_custom_timeout(".lineage-canvas canvas", Duration::from_secs(15))
        .expect("the Cytoscape canvas boots offline (plain)");
    assert_eq!(
        eval(
            &tab,
            "window.CuteExploreLineage.cyInstance().nodes('[changed = 1]').length"
        ),
        serde_json::Value::from(0),
        "a no-context render marks no node changed",
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.legend-chip.changed').length"
        ),
        serde_json::Value::from(0),
        "no change-context legend chip without --pr-diff",
    );
    let _ = tab.close(true);

    // --- 2.-4. the contexted render --------------------------------------
    let url = render_explore_dag_with_context(
        "explore-change-context",
        nodes(),
        &["model.shop.dim_orders"],
    );
    let tab = browser.new_tab().expect("new tab");

    // Console recorder (warnings + errors), subscribed BEFORE navigate.
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

    // Exactly the expected node is marked; the full graph still renders.
    assert_eq!(
        eval(
            &tab,
            "window.CuteExploreLineage.cyInstance().nodes().length"
        ),
        serde_json::Value::from(4),
        "change context never narrows scope — every model renders",
    );
    assert_eq!(
        eval(
            &tab,
            "window.CuteExploreLineage.cyInstance().nodes('[changed = 1]')\
               .map(function (n) { return n.id(); }).join(',')"
        ),
        serde_json::Value::String("model.shop.dim_orders".to_owned()),
        "exactly the expected node carries the changed mark",
    );
    assert!(
        eval(
            &tab,
            "window.CuteExploreLineage.cyInstance()\
               .getElementById('model.shop.dim_orders')\
               .numericStyle('underlay-opacity')"
        )
        .as_f64()
        .unwrap_or(0.0)
            > 0.0,
        "the changed node renders the amber underlay glow",
    );
    assert_eq!(
        eval(
            &tab,
            "window.CuteExploreLineage.cyInstance()\
               .getElementById('model.shop.stg_orders')\
               .numericStyle('underlay-opacity')"
        )
        .as_f64()
        .unwrap_or(-1.0),
        0.0,
        "an unchanged node renders no underlay",
    );
    // The banner + legend chrome.
    assert!(
        eval(
            &tab,
            "document.querySelector('.explore-counts').textContent"
        )
        .as_str()
        .unwrap_or("")
        .contains("1 changed in this diff"),
        "the header counts the changed models",
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.legend-chip.changed').length"
        ),
        serde_json::Value::from(1),
        "the legend explains the changed treatment",
    );

    // --- 3. composition with highlight ------------------------------------
    assert_eq!(
        eval(&tab, "window.focusModel('model.shop.dim_orders')"),
        serde_json::Value::Bool(true),
        "focusModel highlights the changed node",
    );
    assert_eq!(
        eval(
            &tab,
            "(function () { \
               var n = window.CuteExploreLineage.cyInstance()\
                 .getElementById('model.shop.dim_orders'); \
               return n.hasClass('sel') && n.data('changed') === 1 \
                 && n.numericStyle('underlay-opacity') > 0; \
             })()"
        ),
        serde_json::Value::Bool(true),
        "highlight (.sel border) and the changed underlay compose on one node",
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.model-detail-card .detail-changed').length"
        ),
        serde_json::Value::from(1),
        "the detail card carries the changed-in-this-diff chip",
    );
    // Highlighting a DIFFERENT lineage dims the changed node without
    // dropping its data flag (context never fights emphasis).
    assert_eq!(
        eval(&tab, "window.focusModel('model.shop.lonely')"),
        serde_json::Value::Bool(true),
    );
    assert_eq!(
        eval(
            &tab,
            "(function () { \
               var n = window.CuteExploreLineage.cyInstance()\
                 .getElementById('model.shop.dim_orders'); \
               return n.hasClass('dim') && n.data('changed') === 1; \
             })()"
        ),
        serde_json::Value::Bool(true),
        "a changed node outside the highlighted lineage dims with its glow",
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.model-detail-card .detail-changed').length"
        ),
        serde_json::Value::from(0),
        "an unchanged model's card carries no changed chip",
    );

    // --- 4. bridge/commit semantics untouched ------------------------------
    assert_eq!(
        eval(&tab, "'selectedModel' in document.body.dataset"),
        serde_json::Value::Bool(false),
        "highlight alone never writes the focus-commit attribute",
    );
    tab.press_key(" ").expect("press Space");
    assert_eq!(
        eval(&tab, "document.body.dataset.selectedModel"),
        serde_json::Value::String("model.shop.lonely".to_owned()),
        "the deliberate Space commit still writes the attribute",
    );

    let noise = console_noise.lock().unwrap().clone();
    assert!(
        noise.is_empty(),
        "the contexted explore page must emit zero console warnings/errors; got:\n{}",
        noise.join("\n"),
    );

    let _ = tab.close(true);
}
