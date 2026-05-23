//! Structured resource-ref lint over the committed example HTML.
//!
//! Secondary zero-egress gate. The primary is the headless network-block
//! test in `tests/headless_zero_egress.rs`. The two gates complement each
//! other: this one catches template-layer regressions in milliseconds
//! against the committed `examples/jaffle-shop-report.html`; the headless
//! test proves the runtime property in a real browser.
//!
//! Forbidden constructs, per `SECURITY.md` and `ARCHITECTURE.md` §5:
//!   - `<script src="...">` to any non-`data:` value
//!   - `<link href="...">` to any non-`data:` value
//!   - `<img src="...">` to any non-`data:` value
//!   - CSS `@import` (banned outright; even `data:` imports are an
//!     unwanted construct in the rendered chrome)
//!   - CSS `url(...)` to any non-`data:` value
//!   - Protocol-relative `//host/...` in any of the above
//!
//! Why structured parsing, not `grep`: the inlined Mermaid + DataTables +
//! jQuery bundles contain hundreds of inert URL string literals inside
//! regex constants, template strings, and comments. They never trigger
//! a real request. A raw `grep http` would drown the signal under
//! noise; the parser walks ELEMENTS so only real attributes count.
//!
//! Tracked: breezy-bays-labs/cute-dbt#12.

use std::path::PathBuf;

fn example_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("jaffle-shop-report.html")
}

#[derive(Debug)]
struct Violation {
    kind: &'static str,
    value: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.value)
    }
}

fn scan_violations(html: &str) -> Vec<Violation> {
    let dom = tl::parse(html, tl::ParserOptions::default()).expect("HTML must be parseable");
    let parser = dom.parser();
    let mut out = Vec::new();

    for node_handle in dom.nodes() {
        let Some(tag) = node_handle.as_tag() else {
            continue;
        };
        let name_lc = tag.name().as_utf8_str().to_ascii_lowercase();
        let attrs = tag.attributes();

        match name_lc.as_str() {
            "script" => check_attr(attrs, "src", "<script src>", &mut out),
            "link" => check_attr(attrs, "href", "<link href>", &mut out),
            "img" => {
                check_attr(attrs, "src", "<img src>", &mut out);
                check_attr(attrs, "srcset", "<img srcset>", &mut out);
            }
            "style" => {
                let css = tag.inner_text(parser);
                find_css_external_refs(&css, &mut out);
            }
            _ => {}
        }
    }
    out
}

fn check_attr(
    attrs: &tl::Attributes<'_>,
    attr_name: &str,
    kind: &'static str,
    out: &mut Vec<Violation>,
) {
    let Some(Some(raw)) = attrs.get(attr_name) else {
        return;
    };
    let value = raw.as_utf8_str();
    if is_external_url(&value) {
        out.push(Violation {
            kind,
            value: value.into_owned(),
        });
    }
}

fn is_external_url(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() || v.starts_with('#') {
        return false;
    }
    // Inline / non-network schemes — allowed.
    if v.starts_with("data:") || v.starts_with("mailto:") {
        return false;
    }
    // Protocol-relative — forbidden by the contract.
    if v.starts_with("//") {
        return true;
    }
    // Network schemes — forbidden.
    if let Some((scheme, _)) = v.split_once(':') {
        let s = scheme.to_ascii_lowercase();
        return matches!(
            s.as_str(),
            "http" | "https" | "ws" | "wss" | "ftp" | "ftps" | "file",
        );
    }
    false
}

fn find_css_external_refs(css: &str, out: &mut Vec<Violation>) {
    // @import — banned outright. Even data: imports are an unwanted
    // construct in the rendered chrome (the contract is "no @import",
    // not "no external @import").
    let lower = css.to_ascii_lowercase();
    let mut i = 0;
    while let Some(rel) = lower[i..].find("@import") {
        let start = i + rel;
        let end_of_snippet = (start + 80).min(css.len());
        out.push(Violation {
            kind: "@import",
            value: css[start..end_of_snippet]
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string(),
        });
        i = start + "@import".len();
    }

    // url(...) — only external (non-data, non-empty) values are forbidden.
    let mut i = 0;
    while let Some(rel) = lower[i..].find("url(") {
        let start = i + rel + 4;
        let end = lower[start..].find(')').map_or(lower.len(), |n| start + n);
        let inner = css[start..end]
            .trim()
            .trim_matches(|c| c == '"' || c == '\'');
        if !inner.is_empty() && !inner.starts_with("data:") && !inner.starts_with('#') {
            out.push(Violation {
                kind: "url()",
                value: inner.to_string(),
            });
        }
        i = end + 1;
    }
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
