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
    // Strip <script> bodies BEFORE handing the HTML to `tl`. tl does not
    // perfectly enforce the HTML5 "script content is raw text" rule —
    // template-literal substrings like `<img src="${e}">` inside a
    // minified bundle get materialized as spurious DOM nodes the lint
    // would otherwise false-positive on. The lint only cares about the
    // OPENING `<script src="...">` tag's attributes, never the body, so
    // stripping content loses no real coverage.
    let stripped = strip_script_bodies(html);
    let dom = tl::parse(&stripped, tl::ParserOptions::default()).expect("HTML must be parseable");
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

/// Replace every `<script ...>BODY</script>` content with an empty
/// string while preserving the opening tag (so `<script src="...">`
/// attribute checks still fire). The matching is case-insensitive and
/// tolerates self-closing `<script.../>` (rare in HTML, but cheap to
/// pass through unchanged).
fn strip_script_bodies(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(open_rel) = lower[cursor..].find("<script") {
        let open_start = cursor + open_rel;
        out.push_str(&html[cursor..open_start]);
        let Some(open_end_rel) = lower[open_start..].find('>') else {
            // Malformed; preserve the rest verbatim.
            out.push_str(&html[open_start..]);
            return out;
        };
        let open_end = open_start + open_end_rel + 1;
        out.push_str(&html[open_start..open_end]);
        // Self-closing variant — no body to strip.
        if html[open_start..open_end].ends_with("/>") {
            cursor = open_end;
            continue;
        }
        match lower[open_end..].find("</script>") {
            Some(close_rel) => {
                let close_start = open_end + close_rel;
                out.push_str("</script>");
                cursor = close_start + "</script>".len();
            }
            None => {
                // Unclosed <script> — preserve the rest verbatim.
                out.push_str(&html[open_end..]);
                return out;
            }
        }
    }
    out.push_str(&html[cursor..]);
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
    if is_forbidden_resource_ref(&value) {
        out.push(Violation {
            kind,
            value: value.into_owned(),
        });
    }
}

/// Whether an `href` / `src` / `srcset` value is a forbidden resource
/// reference for the self-contained-report contract.
///
/// The rendered chrome must not load *any* separate file. So the
/// allowlist is narrow: empty, `#fragment`, `data:` URI, or `mailto:`
/// (the last so a future "report a bug" link does not trip the lint).
/// Everything else — relative paths, absolute paths, network schemes,
/// protocol-relative, `file:` — is a violation.
fn is_forbidden_resource_ref(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() || v.starts_with('#') {
        return false;
    }
    if v.starts_with("data:") || v.starts_with("mailto:") {
        return false;
    }
    // Everything else — relative paths (`script.js`, `./x`, `../y`,
    // `/abs/path`), protocol-relative (`//host/x`), and any other
    // `scheme:`-prefixed value (`http:`, `https:`, `file:`, etc.) —
    // would resolve to a separate file or network endpoint and is a
    // violation of the single-self-contained-file invariant.
    true
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
