//! `insta` golden snapshot for the rendered report's DOM structure.
//!
//! Scope (per advisor on PR 10): the snapshot is a **structural
//! slice** — embedded `<style>` and `<script>` bodies are dropped
//! before snapshotting, leaving the DOM skeleton (banner text, section
//! ids, table column headers, CTE-DAG node + edge counts per section).
//! This complements the existing `example-report-up-to-date` CI gate:
//!
//! - `example-report-up-to-date` pins **byte-exact** equality on the
//!   full `examples/jaffle-shop-report.html` (so the bundled
//!   Mermaid / DataTables / jQuery / Sakura asset contents are stable).
//! - This snapshot pins **layout stability** — the renderer's DOM
//!   skeleton over the same fixture pair. A bundle bump fails the
//!   first gate alone; a layout regression fails this one alone.
//!
//! Snapshot review uses `cargo insta review`. The snapshot file lives
//! at `tests/snapshots/golden_report__rendered_report_skeleton.snap`.

#[path = "common/mod.rs"]
mod common;

use std::path::PathBuf;

use cute4dbt::adapters::manifest::FileManifestSource;
use cute4dbt::adapters::render::render_report;
use cute4dbt::domain::{DEFAULT_REPORT_TITLE, Manifest, StateComparator};
use cute4dbt::ports::ManifestSource;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn load(name: &str) -> Manifest {
    FileManifestSource
        .load(&fixture(name))
        .unwrap_or_else(|err| panic!("fixture {name} is a valid v12 manifest: {err:?}"))
}

/// Strip every `<script>...</script>` and `<style>...</style>` block
/// from the rendered HTML so the snapshot captures the DOM skeleton
/// only — the bundled Mermaid / DataTables / jQuery / Sakura bytes
/// are pinned separately by `example-report-up-to-date`.
fn structural_slice(html: &str) -> String {
    // Reuse the same strip-script-bodies helper the resource-ref lint
    // uses, then run the same shape over `<style>` blocks.
    let no_scripts = common::strip_script_bodies(html);
    strip_style_bodies(&no_scripts)
}

fn strip_style_bodies(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(open_rel) = lower[cursor..].find("<style") {
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
        match lower[open_end..].find("</style>") {
            Some(close_rel) => {
                let close_start = open_end + close_rel;
                out.push_str("</style>");
                cursor = close_start + "</style>".len();
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

#[test]
fn rendered_report_skeleton() {
    let current = load("jaffle-shop-current.json");
    let baseline = load("jaffle-shop-baseline.json");
    let comparator = StateComparator::body_only();
    let in_scope = comparator.in_scope_unit_tests(&current, &baseline);
    let models_in_scope = comparator.models_in_scope(&current, &baseline);
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("golden_report.html");
    let _ = std::fs::remove_file(&out);
    render_report(
        &out,
        &current,
        &in_scope,
        &models_in_scope,
        "jaffle-shop-baseline.json",
        DEFAULT_REPORT_TITLE,
        None,
    )
    .expect("render writes the report");
    let html = std::fs::read_to_string(&out).expect("report exists");
    let skeleton = structural_slice(&html);
    insta::assert_snapshot!(skeleton);
}
