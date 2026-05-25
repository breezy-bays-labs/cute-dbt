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

use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use headless_chrome::Browser;
use headless_chrome::LaunchOptionsBuilder;
use headless_chrome::protocol::cdp::types::Event;
use headless_chrome::protocol::cdp::{Network, Runtime};

/// Every committed example HTML the headless proof must verify. Adding
/// a new `examples/<name>-report.html` requires appending its filename
/// here so the primary zero-egress gate runs against it on every PR.
const COMMITTED_EXAMPLES: &[&str] = &["jaffle-shop-report.html", "playground-report.html"];

fn report_path(filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(filename)
}

fn report_file_url(filename: &str) -> String {
    let path = report_path(filename);
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
    for filename in COMMITTED_EXAMPLES {
        let path = report_path(filename);
        assert!(
            path.exists(),
            "examples/{filename} missing — regenerate via the \
             `example-report-up-to-date` CI step or run:\n  cargo run --bin cute-dbt -- \
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
    for filename in COMMITTED_EXAMPLES {
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
