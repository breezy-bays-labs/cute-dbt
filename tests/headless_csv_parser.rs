//! RFC 4180 CSV parser correctness — exercised through the rendered
//! report via a real headless Chromium runtime.
//!
//! Why this test exists:
//!
//! dbt-fusion 2.0-preview emits `format: csv` unit-test fixtures as a
//! raw CSV body in the manifest (verified empirically against a
//! minimal probe project, 2026-05-26). dbt-core 1.11+ pre-parses csv
//! to an array of dicts. cute-dbt's JS renderer (`templates/report.html`)
//! contains a hand-rolled RFC 4180 parser so reports look identical
//! regardless of which engine compiled the manifest. The parser is
//! exposed as `window.__cuteParseCsv` for this test.
//!
//! The locked design (cute-dbt#66) requires comprehensive coverage
//! for: quoted fields, embedded commas, escaped double-quotes,
//! embedded newlines, trailing newline, empty input, header-only
//! input, CRLF terminator. Each case is a separate sub-assertion so a
//! parser regression points at the exact failing case.
//!
//! ## Runtime cost
//!
//! Shares the same CI job as `headless_zero_egress` and is `#[ignore]`
//! by default. One Chrome cold-start covers both tests. Locally:
//!
//! ```bash
//! cargo test --test headless_csv_parser -- --ignored
//! ```
//!
//! Tracked: breezy-bays-labs/cute-dbt#66.

#[path = "common/mod.rs"]
mod common;

use std::ffi::OsStr;
use std::path::PathBuf;

use headless_chrome::Browser;
use headless_chrome::LaunchOptionsBuilder;
use headless_chrome::protocol::cdp::Runtime;

fn report_file_url(filename: &str) -> String {
    let path = common::example_path(filename);
    let p = path.to_str().expect("report path must be valid UTF-8");
    format!("file://{p}")
}

/// One CSV parser case — the input fed to `window.__cuteParseCsv` and
/// the expected JSON output as a Rust string literal. We compare the
/// `JSON.stringify`'d parser output to `expected_json` for an exact
/// structural match (key order in the row dicts is determined by the
/// header row, so the comparison is well-defined).
struct ParserCase {
    name: &'static str,
    input: &'static str,
    /// Exact `JSON.stringify` form the parser must produce. Use
    /// canonical no-whitespace JSON to match what the JS engine emits.
    expected_json: &'static str,
}

const CASES: &[ParserCase] = &[
    ParserCase {
        name: "empty input",
        input: "",
        expected_json: "[]",
    },
    ParserCase {
        name: "header-only input",
        input: "id,name",
        expected_json: "[]",
    },
    ParserCase {
        name: "header-only with trailing newline",
        input: "id,name\n",
        expected_json: "[]",
    },
    ParserCase {
        name: "single row, no trailing newline",
        input: "id,name\n1,alice",
        expected_json: r#"[{"id":"1","name":"alice"}]"#,
    },
    ParserCase {
        name: "single row, trailing LF",
        input: "id,name\n1,alice\n",
        expected_json: r#"[{"id":"1","name":"alice"}]"#,
    },
    ParserCase {
        name: "two rows, trailing LF",
        input: "id,name\n1,alice\n2,bob\n",
        expected_json: r#"[{"id":"1","name":"alice"},{"id":"2","name":"bob"}]"#,
    },
    ParserCase {
        name: "CRLF terminator",
        input: "id,name\r\n1,alice\r\n2,bob\r\n",
        expected_json: r#"[{"id":"1","name":"alice"},{"id":"2","name":"bob"}]"#,
    },
    ParserCase {
        name: "quoted field stripped",
        input: "id,name\n1,\"alice\"\n",
        expected_json: r#"[{"id":"1","name":"alice"}]"#,
    },
    ParserCase {
        name: "quoted field with embedded comma",
        input: "id,name\n1,\"alice, the brave\"\n",
        expected_json: r#"[{"id":"1","name":"alice, the brave"}]"#,
    },
    ParserCase {
        name: "RFC 4180 double-quote escape",
        input: "id,note\n1,\"she said \"\"hello\"\"\"\n",
        expected_json: r#"[{"id":"1","note":"she said \"hello\""}]"#,
    },
    ParserCase {
        name: "quoted field with embedded LF",
        input: "id,memo\n1,\"line one\nline two\"\n",
        expected_json: r#"[{"id":"1","memo":"line one\nline two"}]"#,
    },
    ParserCase {
        name: "quoted field with embedded CRLF",
        input: "id,memo\n1,\"line one\r\nline two\"\n",
        expected_json: r#"[{"id":"1","memo":"line one\r\nline two"}]"#,
    },
    ParserCase {
        name: "empty fields via consecutive commas",
        input: "a,b,c\n1,,3\n",
        expected_json: r#"[{"a":"1","b":"","c":"3"}]"#,
    },
    ParserCase {
        name: "trailing empty field",
        input: "a,b,c\n1,2,\n",
        expected_json: r#"[{"a":"1","b":"2","c":""}]"#,
    },
    ParserCase {
        name: "mixed quoted and unquoted",
        input: "a,b,c\n\"x\",y,\"z\"\n",
        expected_json: r#"[{"a":"x","b":"y","c":"z"}]"#,
    },
    ParserCase {
        name: "row shorter than header — missing trailing fields filled with empty string",
        input: "a,b,c\n1,2\n",
        expected_json: r#"[{"a":"1","b":"2","c":""}]"#,
    },
];

#[test]
#[ignore = "requires Chrome; runs explicitly in the headless-zero-egress CI job via `-- --ignored`"]
fn cute_parse_csv_handles_rfc4180_edge_cases() {
    // Reuse one of the committed example reports — any of them works
    // since the parser is exposed on `window.__cuteParseCsv` regardless
    // of which fixture seeded the report.
    let filename = "playground-report.html";
    let path = common::example_path(filename);
    assert!(
        path.exists(),
        "examples/{filename} missing — regenerate via the example-report-up-to-date \
         CI step or run: cargo run --bin cute-dbt -- \
         --manifest tests/fixtures/playground-current.json \
         --baseline-manifest tests/fixtures/playground-baseline.json \
         --out examples/{filename}",
    );

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

    let tab = browser.new_tab().expect("new tab");
    let url = report_file_url(filename);
    assert!(url.starts_with("file://"));
    tab.navigate_to(&url).expect("navigate to file:// URL");
    tab.wait_until_navigated().expect("await navigation");

    // Confirm the parser is exposed before driving cases — a missing
    // seam is a different failure mode from a parser bug and the error
    // message should reflect that.
    let probe = tab
        .call_method(Runtime::Evaluate {
            expression: "typeof window.__cuteParseCsv === 'function'".to_string(),
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
        .expect("evaluate parser-exposure probe");
    let exposed = probe
        .result
        .value
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        exposed,
        "window.__cuteParseCsv is not exposed — the parser cannot be \
         driven through this seam. Re-check the test-seam guard at the \
         bottom of the IIFE in templates/report.html.",
    );

    let mut failures: Vec<String> = Vec::new();
    for case in CASES {
        // JSON.stringify the input to safely embed it in the eval
        // expression — covers embedded quotes, newlines, etc.
        let input_json = serde_json::to_string(case.input).expect("input encodes as JSON");
        let expression = format!(
            "JSON.stringify(window.__cuteParseCsv({input_json}))",
            input_json = input_json,
        );
        let eval = tab
            .call_method(Runtime::Evaluate {
                expression,
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
            .expect("evaluate parser case");
        let actual_json = eval
            .result
            .value
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "<no value>".to_string());
        if actual_json != case.expected_json {
            failures.push(format!(
                "case `{name}`:\n  input    = {input:?}\n  expected = {expected}\n  actual   = {actual}",
                name = case.name,
                input = case.input,
                expected = case.expected_json,
                actual = actual_json,
            ));
        }
    }

    let _ = tab.close(true);

    assert!(
        failures.is_empty(),
        "RFC 4180 parser failed {n} case(s):\n{listing}",
        n = failures.len(),
        listing = failures.join("\n\n"),
    );
}
