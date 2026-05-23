//! Shared helpers for the BDD outer-loop (`tests/bdd.rs`) and the
//! resource-ref lint (`tests/resource_ref_lint.rs`).
//!
//! Each test binary that uses this module declares `mod common;` (via
//! `#[path = "common/mod.rs"]` for the bdd binary, which lives at
//! `tests/bdd.rs`).
//!
//! The fast integration tests in `tests/run_loop.rs` and friends still
//! carry their own copies of `fixture()` / `tmp()` / `run()` so each
//! reads top-to-bottom; a future PR can consolidate by switching them
//! to `mod common;` too.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Absolute path to a committed fixture under `tests/fixtures/`.
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A path inside the cargo-provided integration-test temp directory.
pub fn tmp(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

/// Best-effort delete so a re-run starts from a known-absent file.
pub fn clear(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Stringify a path argument (every test path is valid UTF-8).
pub fn s(path: &Path) -> &str {
    path.to_str().expect("test paths are valid UTF-8")
}

/// Run the `cute-dbt` binary with `args`; return its captured output.
pub fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cute-dbt"))
        .args(args)
        .output()
        .expect("the cute-dbt binary spawns")
}

// ===== Resource-ref lint shared with tests/resource_ref_lint.rs =====
//
// Same shape as the standalone lint test — the BDD scenario for
// "report has no external resource references" runs the same scan
// against a freshly-generated report.html. The lint walks the parsed
// DOM via `tl` so minified bundles' inert URL string literals do not
// false-positive (the rationale lives in resource_ref_lint.rs).

#[derive(Debug)]
pub struct ResourceRefViolation {
    pub kind: &'static str,
    pub value: String,
}

impl std::fmt::Display for ResourceRefViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.value)
    }
}

/// Strip `<script>` bodies before handing the HTML to `tl`. tl does not
/// fully enforce the HTML5 "script content is raw text" rule, so
/// template-literal substrings like `<img src="${e}">` inside a
/// minified bundle can otherwise be materialized as spurious DOM nodes.
/// The lint only cares about the OPENING tag's attributes; the body
/// strip loses no real signal.
pub fn strip_script_bodies(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(open_rel) = lower[cursor..].find("<script") {
        let open_start = cursor + open_rel;
        out.push_str(&html[cursor..open_start]);
        let Some(open_end_rel) = lower[open_start..].find('>') else {
            out.push_str(&html[open_start..]);
            return out;
        };
        let open_end = open_start + open_end_rel + 1;
        out.push_str(&html[open_start..open_end]);
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
                out.push_str(&html[open_end..]);
                return out;
            }
        }
    }
    out.push_str(&html[cursor..]);
    out
}

/// Whether an `href` / `src` / `srcset` value is forbidden under the
/// single-self-contained-file invariant. Allowlist: empty,
/// `#fragment`, `data:` URI, `mailto:`.
pub fn is_forbidden_resource_ref(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() || v.starts_with('#') {
        return false;
    }
    if v.starts_with("data:") || v.starts_with("mailto:") {
        return false;
    }
    true
}

fn check_attr(
    attrs: &tl::Attributes<'_>,
    attr_name: &str,
    kind: &'static str,
    out: &mut Vec<ResourceRefViolation>,
) {
    let Some(Some(raw)) = attrs.get(attr_name) else {
        return;
    };
    let value = raw.as_utf8_str();
    if is_forbidden_resource_ref(&value) {
        out.push(ResourceRefViolation {
            kind,
            value: value.into_owned(),
        });
    }
}

fn find_css_external_refs(css: &str, out: &mut Vec<ResourceRefViolation>) {
    let lower = css.to_ascii_lowercase();
    let mut i = 0;
    while let Some(rel) = lower[i..].find("@import") {
        let start = i + rel;
        let end_of_snippet = (start + 80).min(css.len());
        out.push(ResourceRefViolation {
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

    let mut i = 0;
    while let Some(rel) = lower[i..].find("url(") {
        let start = i + rel + 4;
        let end = lower[start..].find(')').map_or(lower.len(), |n| start + n);
        let inner = css[start..end]
            .trim()
            .trim_matches(|c| c == '"' || c == '\'');
        if !inner.is_empty() && !inner.starts_with("data:") && !inner.starts_with('#') {
            out.push(ResourceRefViolation {
                kind: "url()",
                value: inner.to_string(),
            });
        }
        i = end + 1;
    }
}

/// Scan an HTML string and return every forbidden resource reference
/// found. Used by the BDD `zero_egress.feature` scenario and by the
/// standalone `tests/resource_ref_lint.rs` test.
pub fn scan_resource_refs(html: &str) -> Vec<ResourceRefViolation> {
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

/// Assertion helper for the BDD `zero_egress.feature` scenarios and for
/// the report_generation "self-contained" assertion. Panics on any
/// violation.
pub fn assert_no_external_refs(html: &str) {
    let violations = scan_resource_refs(html);
    assert!(
        violations.is_empty(),
        "report contains {} external resource reference(s) — the zero-egress invariant is broken:\n{}",
        violations.len(),
        violations
            .iter()
            .map(|v| format!("  - {v}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
