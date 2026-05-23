//! Structured resource-ref lint over the committed example HTML.
//!
//! Secondary zero-egress gate. The primary is the headless network-block
//! test in `tests/headless_zero_egress.rs`. The two gates complement each
//! other: this one catches template-layer regressions in milliseconds
//! against the committed `examples/jaffle-shop-report.html`; the headless
//! test proves the runtime property in a real browser.
//!
//! Forbidden constructs, per `SECURITY.md` and `ARCHITECTURE.md` §5.
//! The rendered chrome is a single self-contained HTML file — there are
//! no sibling files to load. The lint therefore allows only inline
//! values and rejects every reference that would resolve to a separate
//! resource:
//!   - `<script src="...">`  — only `data:` URIs / empty allowed
//!   - `<link href="...">`   — only `data:` URIs / empty / `#fragment` allowed
//!   - `<img src="...">`     — only `data:` URIs / empty allowed
//!   - CSS `@import`         — banned outright (no exception)
//!   - CSS `url(...)`        — only `data:` URIs / empty allowed
//!
//! Relative paths (`./style.css`, `style.css`, `/abs/path`),
//! protocol-relative (`//host/...`), and any `scheme:`-prefixed value
//! other than `data:` / `mailto:` are violations. There is no
//! legitimate file in the same directory as the report; the proof
//! breaks if the rendered chrome can resolve a sibling.
//!
//! Why structured parsing, not `grep`: the inlined Mermaid + DataTables +
//! jQuery bundles contain hundreds of inert URL string literals inside
//! regex constants, template strings, and comments. They never trigger
//! a real request. A raw `grep http` would drown the signal under
//! noise; the parser walks ELEMENTS so only real attributes count.
//!
//! Tracked: breezy-bays-labs/cute-dbt#12.

#[path = "common/mod.rs"]
mod common;

use std::path::PathBuf;

use common::ResourceRefViolation as Violation;

fn example_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("jaffle-shop-report.html")
}

/// Scan HTML for forbidden resource references. Thin wrapper over
/// `common::scan_resource_refs` so this test surface and the BDD
/// `zero_egress.feature` step share the exact same lint code path.
fn scan_violations(html: &str) -> Vec<Violation> {
    common::scan_resource_refs(html)
}

#[test]
fn committed_example_has_no_external_resource_refs() {
    let path = example_path();
    let html =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let violations = scan_violations(&html);
    assert!(
        violations.is_empty(),
        "committed examples/jaffle-shop-report.html contains {} external resource \
         reference(s) — the zero-egress invariant is broken:\n{}",
        violations.len(),
        violations
            .iter()
            .map(|v| format!("  - {v}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn hostile_script_src_is_caught() {
    let html = r#"<!DOCTYPE html><html><body>
        <script src="https://evil.example.com/track.js"></script>
    </body></html>"#;
    let v = scan_violations(html);
    assert_eq!(v.len(), 1, "expected one violation, got {v:?}");
    assert_eq!(v[0].kind, "<script src>");
    assert!(v[0].value.contains("evil.example.com"));
}

#[test]
fn hostile_link_href_is_caught() {
    let html = r#"<!DOCTYPE html><html><head>
        <link rel="stylesheet" href="https://cdn.example.com/style.css">
    </head></html>"#;
    let v = scan_violations(html);
    assert_eq!(v.len(), 1, "expected one violation, got {v:?}");
    assert_eq!(v[0].kind, "<link href>");
}

#[test]
fn hostile_img_src_is_caught() {
    let html = r#"<!DOCTYPE html><html><body>
        <img src="https://tracker.example.com/pixel.gif">
    </body></html>"#;
    let v = scan_violations(html);
    assert_eq!(v.len(), 1, "expected one violation, got {v:?}");
    assert_eq!(v[0].kind, "<img src>");
}

#[test]
fn protocol_relative_is_caught() {
    let html = r#"<!DOCTYPE html><html><head>
        <script src="//cdn.example.com/jquery.js"></script>
    </head></html>"#;
    let v = scan_violations(html);
    assert_eq!(v.len(), 1, "expected one violation, got {v:?}");
    assert_eq!(v[0].kind, "<script src>");
    assert!(v[0].value.starts_with("//"));
}

#[test]
fn css_import_in_style_is_caught() {
    let html = r#"<!DOCTYPE html><html><head>
        <style>@import url("https://fonts.googleapis.com/css2?family=Inter");</style>
    </head></html>"#;
    let v = scan_violations(html);
    assert!(
        v.iter().any(|x| x.kind == "@import"),
        "expected @import violation, got {v:?}"
    );
    assert!(
        v.iter().any(|x| x.kind == "url()"),
        "expected url() violation, got {v:?}"
    );
}

#[test]
fn css_url_external_in_style_is_caught() {
    let html = r#"<!DOCTYPE html><html><head>
        <style>.bg { background: url("https://example.com/img.png"); }</style>
    </head></html>"#;
    let v = scan_violations(html);
    assert_eq!(v.len(), 1, "expected one violation, got {v:?}");
    assert_eq!(v[0].kind, "url()");
    assert!(v[0].value.contains("example.com"));
}

#[test]
fn data_uri_favicon_is_allowed() {
    let html = r#"<!DOCTYPE html><html><head>
        <link rel="icon" href="data:,">
    </head></html>"#;
    let v = scan_violations(html);
    assert!(v.is_empty(), "data: URIs must be allowed, got {v:?}");
}

#[test]
fn data_uri_image_is_allowed() {
    let html = r#"<!DOCTYPE html><html><body>
        <img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=">
    </body></html>"#;
    let v = scan_violations(html);
    assert!(v.is_empty(), "data: URIs must be allowed, got {v:?}");
}

#[test]
fn inert_url_literal_inside_script_text_is_allowed() {
    // The point of structured parsing vs raw grep: minified script
    // bundles contain inert URL string literals (inside regex,
    // template strings, comments) that NEVER trigger a real request.
    // The lint must walk attributes, not text content of <script>.
    let html = r#"<!DOCTYPE html><html><body>
        <script>var INERT = "https://example.com/not-loaded";</script>
    </body></html>"#;
    let v = scan_violations(html);
    assert!(
        v.is_empty(),
        "inert URL literals inside <script> text must not fire — got {v:?}"
    );
}

#[test]
fn fragment_href_is_allowed() {
    // <a href="#anchor"> is user navigation, not a resource ref; the
    // lint should also allow <link href="#fragment"> (rare but legal).
    let html = r##"<!DOCTYPE html><html><head>
        <link rel="bookmark" href="#section">
    </head></html>"##;
    assert!(scan_violations(html).is_empty());
}

#[test]
fn empty_href_is_allowed() {
    let html = r#"<!DOCTYPE html><html><head>
        <link rel="icon" href="">
    </head></html>"#;
    assert!(scan_violations(html).is_empty());
}

#[test]
fn relative_path_script_is_caught() {
    // The single-self-contained-file invariant: nothing in the report
    // may resolve to a sibling file, even if it has no scheme. A
    // future regression that adds `<script src="bundle.js">` (a
    // tempting refactor for a developer who forgets the inlining
    // contract) would otherwise silently slip through a lint that
    // only looks for `http:` / `https:`.
    let html = r#"<!DOCTYPE html><html><body>
        <script src="bundle.js"></script>
    </body></html>"#;
    let v = scan_violations(html);
    assert_eq!(v.len(), 1, "expected one violation, got {v:?}");
    assert_eq!(v[0].kind, "<script src>");
    assert_eq!(v[0].value, "bundle.js");
}

#[test]
fn absolute_path_link_is_caught() {
    let html = r#"<!DOCTYPE html><html><head>
        <link rel="stylesheet" href="/css/main.css">
    </head></html>"#;
    let v = scan_violations(html);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].kind, "<link href>");
}

#[test]
fn parent_relative_img_is_caught() {
    let html = r#"<!DOCTYPE html><html><body>
        <img src="../assets/logo.png">
    </body></html>"#;
    let v = scan_violations(html);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].kind, "<img src>");
}

#[test]
fn file_scheme_is_caught() {
    let html = r#"<!DOCTYPE html><html><body>
        <img src="file:///etc/passwd">
    </body></html>"#;
    let v = scan_violations(html);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].kind, "<img src>");
}
