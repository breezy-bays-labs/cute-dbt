//! Asset-inlining infrastructure — the vendored frontend bundle embedded
//! into the binary's `.rodata` at compile time, plus a placeholder smoke
//! renderer that proves the bundle assembles into one self-contained,
//! zero-egress HTML document.
//!
//! ## What this module embeds
//!
//! Five vendored assets — Sakura CSS, jQuery, `DataTables` (JS + CSS)
//! and the Mermaid UMD bundle — are pulled in with [`include_str!`] so their
//! bytes land in the binary's read-only data section. There is **no
//! runtime asset directory and no `--assets-dir` flag**: the only way the
//! bytes reach the report is inline interpolation. Each asset's pinned
//! version, canonical source URL, SHA-256 and SPDX license live in
//! `assets/MANIFEST.toml` (the supply-chain artifact), enforced by
//! `tests/assets_manifest.rs` and the `assets-manifest-gate` CI job.
//!
//! Every vendored asset is text (CSS / JS), so every one is embedded with
//! `include_str!` as a `&'static str`. v0.1's bundle carries no binary
//! asset: the favicon is an empty `data:` URI ([`FAVICON_DATA_URI`]), the
//! alternative ADR-4 §5 sanctions, so there is no `include_bytes!` user.
//!
//! ## The smoke renderer
//!
//! [`smoke_report_html`] is a **placeholder**. It takes a real
//! [`CteGraph`] and emits a single self-contained HTML document that
//! bundles all five assets inline and renders the graph as a Mermaid
//! `graph LR` diagram — enough to prove the embedding infrastructure
//! produces valid, offline-capable output. PR 8b replaces it with the
//! askama renderer reproducing the Claude Design report, and wires that
//! into the run loop; the smoke renderer is not called from `cli`.

use std::fmt::Write as _;

use crate::domain::{CteEdge, CteGraph, CteNode, JoinType};

/// Sakura 1.5.0 — the classless base stylesheet. Vendored at
/// `assets/sakura-1.5.0.css`; see `assets/MANIFEST.toml` for provenance.
pub const SAKURA_CSS: &str = include_str!("../../assets/sakura-1.5.0.css");

/// jQuery 3.7.1 — the `DataTables` runtime dependency. Vendored at
/// `assets/jquery-3.7.1.min.js`.
pub const JQUERY_JS: &str = include_str!("../../assets/jquery-3.7.1.min.js");

/// `DataTables` 2.1.8 — sortable / searchable table behaviour. Vendored
/// at `assets/datatables-2.1.8.min.js`.
pub const DATATABLES_JS: &str = include_str!("../../assets/datatables-2.1.8.min.js");

/// `DataTables` 2.1.8 stylesheet. Vendored at
/// `assets/datatables-2.1.8.min.css`.
pub const DATATABLES_CSS: &str = include_str!("../../assets/datatables-2.1.8.min.css");

/// Mermaid 11.15.0 — the **UMD** bundle (never the ESM `type="module"`
/// variant; ADR-4 §5). Vendored at `assets/mermaid-11.15.0.umd.min.js`.
pub const MERMAID_JS: &str = include_str!("../../assets/mermaid-11.15.0.umd.min.js");

/// The mandatory Mermaid initialization statement (ADR-4 §5).
///
/// `securityLevel: 'strict'` and the explicit non-webfont `fontFamily`
/// stack together suppress Mermaid's default Google Fonts fetch — proven
/// empirically in the R1 spike. Any edit here can silently reintroduce a
/// network request, so `the_mermaid_init_is_pinned_to_the_adr_contract`
/// pins this string exactly.
pub const MERMAID_INIT: &str = "mermaid.initialize({ startOnLoad: true, securityLevel: 'strict', fontFamily: 'system-ui,-apple-system,\"Segoe UI\",sans-serif' });";

/// An empty `data:` URI favicon.
///
/// Emitted as `<link rel="icon" href="data:,">`, this resolves the
/// browser's automatic favicon request to an empty in-document resource
/// — never a network call. ADR-4 §5 sanctions this as the alternative to
/// an embedded binary favicon; v0.1's vendored bundle is entirely text,
/// so there is no binary asset and no `include_bytes!` user.
pub const FAVICON_DATA_URI: &str = "data:,";

/// Assemble a self-contained smoke report for `graph`.
///
/// **Placeholder.** The returned HTML inlines every vendored asset and
/// renders `graph` as a Mermaid `graph LR` diagram — enough to prove the
/// embedding infrastructure produces valid, offline-capable output. PR 8b
/// replaces this with the askama renderer reproducing the Claude Design
/// report; only that renderer is wired into the run loop.
#[must_use]
pub fn smoke_report_html(graph: &CteGraph) -> String {
    let mermaid = mermaid_block(graph);
    format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>cute-dbt: asset bundle smoke</title>\n\
         <link rel=\"icon\" href=\"{FAVICON_DATA_URI}\">\n\
         <style>\n{SAKURA_CSS}\n{DATATABLES_CSS}\n</style>\n\
         </head>\n\
         <body>\n\
         <main>\n\
         <h1>cute-dbt: asset bundle smoke</h1>\n\
         <p>Placeholder output: proves the vendored frontend bundle embeds \
         and renders offline. PR 8b lands the real report.</p>\n\
         <pre class=\"mermaid\">\n{mermaid}</pre>\n\
         </main>\n\
         <script>\n{JQUERY_JS}\n</script>\n\
         <script>\n{DATATABLES_JS}\n</script>\n\
         <script>\n{MERMAID_JS}\n</script>\n\
         <script>\n{MERMAID_INIT}\n</script>\n\
         </body>\n\
         </html>\n"
    )
}

/// Render `graph` as a Mermaid `graph LR` definition.
///
/// An empty graph yields the bare `graph LR` header — a valid (if empty)
/// Mermaid diagram.
fn mermaid_block(graph: &CteGraph) -> String {
    let mut block = String::from("graph LR\n");
    block.push_str(&node_lines(graph.nodes()));
    block.push_str(&edge_lines(graph.edges()));
    block
}

/// One Mermaid node declaration per CTE node, in declaration order.
///
/// Nodes are keyed `n{index}` so edges can reference them by the same
/// `usize` index a [`CteEdge`] already stores.
fn node_lines(nodes: &[CteNode]) -> String {
    let mut out = String::new();
    for (index, node) in nodes.iter().enumerate() {
        let _ = writeln!(out, "  n{index}[\"{}\"]", node.name());
    }
    out
}

/// One Mermaid edge per [`CteEdge`], labelled with its join kind.
fn edge_lines(edges: &[CteEdge]) -> String {
    let mut out = String::new();
    for edge in edges {
        let _ = writeln!(
            out,
            "  n{} -->|{}| n{}",
            edge.from(),
            join_label(edge.join_type()),
            edge.to(),
        );
    }
    out
}

/// The Mermaid edge label for a [`JoinType`].
fn join_label(join_type: JoinType) -> &'static str {
    match join_type {
        JoinType::Inner => "INNER",
        JoinType::Left => "LEFT",
        JoinType::Right => "RIGHT",
        JoinType::Full => "FULL",
        JoinType::Cross => "CROSS",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare CTE node carrying only a name.
    fn node(name: &str) -> CteNode {
        CteNode::new(name, None, None, None)
    }

    /// The HTML cute-dbt itself emits, with the five inlined asset bodies
    /// stripped out. Scanning *this* for egress constructs avoids the
    /// false positives the minified bundles' inert URL literals would
    /// otherwise produce (ADR-4 §6 — a raw `grep http` is worse than
    /// useless against a minified bundle).
    fn chrome_only(html: &str) -> String {
        let mut chrome = html.to_owned();
        for asset in [
            SAKURA_CSS,
            DATATABLES_CSS,
            JQUERY_JS,
            DATATABLES_JS,
            MERMAID_JS,
        ] {
            chrome = chrome.replace(asset, "<<inlined-asset>>");
        }
        chrome
    }

    #[test]
    fn each_embedded_asset_carries_its_pinned_version_banner() {
        // Proves `include_str!` grabbed the right file at the right
        // version — a renamed or re-pinned asset fails here.
        assert!(SAKURA_CSS.contains("Sakura.css v1.5.0"), "sakura banner");
        assert!(JQUERY_JS.contains("jQuery v3.7.1"), "jquery banner");
        assert!(
            DATATABLES_JS.contains("DataTables 2.1.8"),
            "datatables js banner",
        );
        assert!(MERMAID_JS.contains("11.15.0"), "mermaid version string");
        // DataTables CSS ships no banner comment; assert a DataTables-only
        // custom property is present.
        assert!(
            DATATABLES_CSS.contains("--dt-row-selected"),
            "datatables css content",
        );
    }

    #[test]
    fn the_mermaid_init_is_pinned_to_the_adr_contract() {
        // ADR-4 §5: `securityLevel: 'strict'` plus the explicit
        // non-webfont system `fontFamily` together suppress Mermaid's
        // Google Fonts fetch. This exact-match pin makes any edit that
        // could reintroduce egress fail the build.
        assert_eq!(
            MERMAID_INIT,
            "mermaid.initialize({ startOnLoad: true, securityLevel: 'strict', fontFamily: 'system-ui,-apple-system,\"Segoe UI\",sans-serif' });",
        );
    }

    #[test]
    fn the_favicon_is_an_empty_data_uri() {
        assert_eq!(FAVICON_DATA_URI, "data:,");
    }

    #[test]
    fn join_label_covers_every_join_kind() {
        assert_eq!(join_label(JoinType::Inner), "INNER");
        assert_eq!(join_label(JoinType::Left), "LEFT");
        assert_eq!(join_label(JoinType::Right), "RIGHT");
        assert_eq!(join_label(JoinType::Full), "FULL");
        assert_eq!(join_label(JoinType::Cross), "CROSS");
    }

    #[test]
    fn an_empty_graph_renders_a_bare_mermaid_block() {
        assert_eq!(mermaid_block(&CteGraph::default()), "graph LR\n");
    }

    #[test]
    fn mermaid_block_emits_one_node_line_per_cte() {
        let graph = CteGraph::new(vec![node("stg_orders"), node("final")], vec![]);
        let block = mermaid_block(&graph);
        assert!(block.contains("n0[\"stg_orders\"]"), "{block}");
        assert!(block.contains("n1[\"final\"]"), "{block}");
    }

    #[test]
    fn mermaid_block_labels_each_edge_with_its_join_kind() {
        let graph = CteGraph::new(
            vec![node("a"), node("b")],
            vec![CteEdge::new(0, 1, JoinType::Left)],
        );
        assert!(
            mermaid_block(&graph).contains("n0 -->|LEFT| n1"),
            "the edge carries its join label",
        );
    }

    #[test]
    fn the_smoke_report_inlines_every_vendored_asset() {
        let html = smoke_report_html(&CteGraph::default());
        assert!(html.contains(SAKURA_CSS), "sakura inlined");
        assert!(html.contains(DATATABLES_CSS), "datatables css inlined");
        assert!(html.contains(JQUERY_JS), "jquery inlined");
        assert!(html.contains(DATATABLES_JS), "datatables js inlined");
        assert!(html.contains(MERMAID_JS), "mermaid inlined");
        assert!(html.contains(MERMAID_INIT), "mermaid init present");
    }

    #[test]
    fn the_smoke_report_is_a_well_formed_html_document() {
        let html = smoke_report_html(&CteGraph::default());
        assert!(html.starts_with("<!doctype html>"), "doctype first");
        assert!(html.trim_end().ends_with("</html>"), "html closed");
        assert!(
            html.contains("<pre class=\"mermaid\">"),
            "mermaid container present",
        );
    }

    #[test]
    fn the_smoke_report_renders_the_passed_graph() {
        let graph = CteGraph::new(
            vec![node("orders"), node("customers")],
            vec![CteEdge::new(0, 1, JoinType::Inner)],
        );
        let html = smoke_report_html(&graph);
        assert!(html.contains("n0[\"orders\"]"), "first node rendered");
        assert!(html.contains("n1[\"customers\"]"), "second node rendered");
        assert!(html.contains("n0 -->|INNER| n1"), "edge rendered");
    }

    #[test]
    fn the_smoke_report_emits_no_external_resource_constructs() {
        // The egress self-test (ADR-4 §6): scan cute-dbt's own HTML, not
        // the inlined bundles. PR 9's headless test is the primary proof;
        // this is the fast local guard against the wrapper drifting.
        let html = smoke_report_html(&CteGraph::default());
        let chrome = chrome_only(&html);
        assert!(!chrome.contains("src="), "no src= attributes: {chrome}");
        assert!(!chrome.contains("@import"), "no CSS @import");
        assert!(!chrome.contains("http://"), "no http URL");
        assert!(!chrome.contains("https://"), "no https URL");
        assert!(!chrome.contains("\"//"), "no protocol-relative reference");
        // The document's only `href` is the empty `data:` favicon.
        assert_eq!(chrome.matches("href=").count(), 1, "exactly one href");
        assert!(chrome.contains("href=\"data:,\""), "favicon is a data: URI");
    }
}
