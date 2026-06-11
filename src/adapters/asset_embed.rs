//! Asset-inlining infrastructure — the vendored frontend bundle embedded
//! into the binary's `.rodata` at compile time.
//!
//! ## What this module embeds
//!
//! Six vendored assets — Sakura CSS, jQuery, `DataTables` (JS + CSS),
//! the Mermaid UMD bundle and the Cytoscape UMD bundle (cute-dbt#180) —
//! are pulled in with [`include_str!`] so their
//! bytes land in the binary's read-only data section. There is **no
//! runtime asset directory and no `--assets-dir` flag**: the only way the
//! bytes reach the report is inline interpolation by the askama renderer
//! ([`crate::adapters::render`]). Each asset's pinned version, canonical
//! source URL, SHA-256 and SPDX license live in `assets/MANIFEST.toml`
//! (the supply-chain artifact), enforced by `tests/assets_manifest.rs`
//! and the `assets-manifest-gate` CI job.
//!
//! Every vendored asset is text (CSS / JS), so every one is embedded with
//! `include_str!` as a `&'static str`. v0.1's bundle carries no binary
//! asset: the favicon is an empty `data:` URI ([`FAVICON_DATA_URI`]), the
//! alternative `ARCHITECTURE.md` §5 sanctions, so there is no
//! `include_bytes!` user.
//!
//! ## Mermaid initialization
//!
//! The mandatory Mermaid init (`startOnLoad: false`,
//! `securityLevel: 'strict'`, system `fontFamily`) lives inside the
//! askama template's inlined interaction script — there is no Rust-side
//! constant for it. The init is exercised at render time per selected
//! model, not at page load; pinning a Rust string constant would invite
//! drift from the JS that actually runs.

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
/// variant; see `ARCHITECTURE.md` §5). Vendored at
/// `assets/mermaid-11.15.0.umd.min.js`.
pub const MERMAID_JS: &str = include_str!("../../assets/mermaid-11.15.0.umd.min.js");

/// Cytoscape 3.30.2 — the second graph engine (cute-dbt#180), behind the
/// report's Mermaid ⇄ Cytoscape DAG-engine picker (Mermaid stays the
/// static default), and the engine of the explore page's interactive
/// model-lineage DAG (cute-dbt#101). The minified **UMD** bundle (never
/// ESM), core only. The REPORT page loads no layout extension (its
/// node positions come from the first-party preset layout in
/// [`CYTO_DAG_JS`] — the ADR-4-amendment init contract); the EXPLORE
/// page pairs this core with [`CYTOSCAPE_DAGRE_JS`]. `cytoscape-elk`
/// stays forbidden everywhere (EPL). Vendored at
/// `assets/cytoscape-3.30.2.min.js`; provenance in `assets/MANIFEST.toml`.
pub const CYTOSCAPE_JS: &str = include_str!("../../assets/cytoscape-3.30.2.min.js");

/// cytoscape-dagre 4.0.0 — the left-to-right rank layout for the EXPLORE
/// page's interactive model-lineage DAG (cute-dbt#101). **Never loaded by
/// the report page** (whose init contract stays layout-plugin-free). The
/// minified **UMD** bundle; v4.0.0 bundles its layout dependency
/// internally (`@dagrejs/dagre` 3.0.0 + `@dagrejs/graphlib` 4.0.1, both
/// MIT), so there is no separate dagre asset. The upstream
/// `//# sourceMappingURL=…` trailer is stripped at vendoring (it
/// references a sibling `.map` file the self-contained page must never
/// point at); the upstream dist banner reads "cytoscape-dagre 3.0.0" — a
/// stale banner inside the 4.0.0 tarball (upstream packaging quirk),
/// documented in `assets/MANIFEST.toml`. Vendored at
/// `assets/cytoscape-dagre-4.0.0.min.js`.
pub const CYTOSCAPE_DAGRE_JS: &str = include_str!("../../assets/cytoscape-dagre-4.0.0.min.js");

/// The report's first-party chassis CSS (cute-dbt#177) — semantic token
/// layer, the five `[data-theme]` blocks, the four `html[data-style]`
/// direction packs, the density layer and the tokenized component rules,
/// merged from the Claude Design Phase-1 handoff in its prescribed
/// cascade order (tokens → styles → chrome → base) plus the cute-dbt
/// reconciliation + PR-1 bridge layers.
///
/// First-party, NOT vendored: it lives at `templates/report.css` (beside
/// the template it styles), deliberately outside `assets/` so the
/// third-party provenance gate (`assets/MANIFEST.toml` +
/// `tests/assets_manifest.rs`, which walk every file under `assets/`)
/// never demands an upstream pin for code this repo authors. Its
/// integrity gates are the banner-pin test (head banner + end-of-file
/// sentinel, so a truncated copy fails) and the comment-balance test
/// (the handoff's documented `*` + `/` porting bug cannot silently eat
/// a rule) in this module's test block.
pub const REPORT_CSS: &str = include_str!("../../templates/report.css");

/// The report's first-party interaction engine (cute-dbt#178) — model/test
/// selectors, the Mermaid DAG, the unified + split diff renderers with
/// word-level emphasis and hunk folds, the fixture grids, copy buttons,
/// code-card headers and the #139 settings rows.
///
/// First-party, NOT vendored: lives at `templates/interaction.js` (the
/// same rationale as [`REPORT_CSS`] — the `assets/` provenance gate walks
/// every file there and is reserved for third-party pins). Integrity
/// gates: the banner-pin + end-of-file sentinel test in this module.
pub const INTERACTION_JS: &str = include_str!("../../templates/interaction.js");

/// The report's first-party appearance engine (cute-dbt#178) — theme /
/// style / accent / density / diff-style / diff-layout wiring with
/// `localStorage` persistence (key `cute-dbt.appearance.v1`) and the
/// `DataTables` dark-mode sync (`html.dark`).
///
/// First-party, NOT vendored: lives at `templates/theme.js`. Integrity
/// gates: the banner-pin + end-of-file sentinel test in this module.
pub const THEME_JS: &str = include_str!("../../templates/theme.js");

/// The explore page's first-party interactive lineage engine
/// (cute-dbt#101) — boots Cytoscape + the dagre left-to-right layout
/// over the embedded `explore-dag-data` [`LineagePayload`] carrier,
/// hand-rolled dependency-free fuzzy search, click / search-select
/// **highlight** (emphasize + dim complement, in-place class mutation,
/// no commit signal) and the deliberate **Space** focus commit
/// (center + `document.body.dataset.selectedModel`). Same init hygiene
/// as [`CYTO_DAG_JS`]: canvas-text labels (XSS-safe by construction),
/// non-webfont system `fontFamily`, no workers, handlers bound from our
/// JS, never a re-render per interaction.
///
/// First-party, NOT vendored: lives at `templates/explore-lineage.js`.
/// Integrity gates: the banner-pin + end-of-file sentinel test in this
/// module.
///
/// [`LineagePayload`]: crate::adapters::explore::LineagePayload
pub const EXPLORE_LINEAGE_JS: &str = include_str!("../../templates/explore-lineage.js");

/// The report's first-party Cytoscape DAG engine (cute-dbt#180) — the
/// opt-in interactive alternative behind the settings-panel engine
/// picker: longest-path preset layout (no layout plugin), canvas-text
/// labels (XSS-safe by construction), hover context card, and
/// click-to-highlight lineage with dim-complement. Reads its palette
/// through the `window.cuteDagPalette()` hook `interaction.js` exposes
/// (the single `JOIN_COLORS_LIGHT`/`_DARK` source the
/// edge-vocab-completeness CI gate greps), so the two engines can never
/// drift apart on edge colors.
///
/// First-party, NOT vendored: lives at `templates/cyto-dag.js`.
/// Integrity gates: the banner-pin + end-of-file sentinel test in this
/// module.
pub const CYTO_DAG_JS: &str = include_str!("../../templates/cyto-dag.js");

/// An empty `data:` URI favicon.
///
/// Emitted as `<link rel="icon" href="data:,">`, this resolves the
/// browser's automatic favicon request to an empty in-document resource
/// — never a network call. `ARCHITECTURE.md` §5 sanctions this as the
/// alternative to an embedded binary favicon; v0.1's vendored bundle is
/// entirely text, so there is no binary asset and no `include_bytes!`
/// user.
pub const FAVICON_DATA_URI: &str = "data:,";

#[cfg(test)]
mod tests {
    use super::*;

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
        // Cytoscape: the head license banner + the embedded version string
        // (`version:"3.30.2"` survives minification as a string literal).
        assert!(
            CYTOSCAPE_JS.contains("The Cytoscape Consortium"),
            "cytoscape license banner",
        );
        assert!(
            CYTOSCAPE_JS.contains("\"3.30.2\""),
            "cytoscape version string",
        );
        // cytoscape-dagre (cute-dbt#101): the head banner — upstream ships
        // a stale "3.0.0" banner inside the 4.0.0 tarball (the npm
        // package.json says 4.0.0; documented in assets/MANIFEST.toml),
        // so the pin is the banner line as shipped plus the UMD global.
        assert!(
            CYTOSCAPE_DAGRE_JS.contains("cytoscape-dagre"),
            "cytoscape-dagre banner",
        );
        assert!(
            CYTOSCAPE_DAGRE_JS.contains("cytoscapeDagre"),
            "cytoscape-dagre UMD global",
        );
        // The bundle is self-contained: dagre is inlined (no external
        // `require("dagre")` UMD factory argument like the 2.x builds).
        assert!(
            !CYTOSCAPE_DAGRE_JS.contains("require(\"dagre\")"),
            "cytoscape-dagre must bundle dagre internally, never require it",
        );
        // The sourceMappingURL trailer is stripped at vendoring — a .map
        // sibling reference has no place in a self-contained page.
        assert!(
            !CYTOSCAPE_DAGRE_JS.contains("sourceMappingURL"),
            "cytoscape-dagre sourceMappingURL trailer must stay stripped",
        );
        // DataTables CSS ships no banner comment; assert a DataTables-only
        // custom property is present.
        assert!(
            DATATABLES_CSS.contains("--dt-row-selected"),
            "datatables css content",
        );
    }

    #[test]
    fn the_explore_lineage_js_carries_its_banner_and_is_not_truncated() {
        assert!(
            EXPLORE_LINEAGE_JS.contains("cute-dbt explore lineage engine v1"),
            "explore-lineage.js head banner",
        );
        assert!(
            EXPLORE_LINEAGE_JS
                .trim_end()
                .ends_with("/* end of cute-dbt explore lineage engine v1 (cute-dbt#101) */"),
            "explore-lineage.js end-of-file sentinel (truncation guard)",
        );
        // The focus-commit contract (epic #99): the ONLY interaction that
        // writes the external-drive signal is the deliberate Space commit.
        // Pin the dataset write so a rename is a conscious act.
        assert_eq!(
            EXPLORE_LINEAGE_JS
                .matches("document.body.dataset.selectedModel =")
                .count(),
            1,
            "exactly one selectedModel write site (the Space focus commit)",
        );
    }

    #[test]
    fn the_favicon_is_an_empty_data_uri() {
        assert_eq!(FAVICON_DATA_URI, "data:,");
    }

    #[test]
    fn the_report_css_carries_its_banner_and_is_not_truncated() {
        // First-party sibling of the vendored banner-pin test above:
        // proves `include_str!` grabbed the chassis CSS at its declared
        // version. The HEAD banner catches a renamed/replaced file; the
        // END-OF-FILE sentinel catches a truncated copy (a head-only
        // check would pass on a file cut off mid-rule).
        assert!(
            REPORT_CSS.contains("cute-dbt report chassis CSS v1"),
            "report.css head banner",
        );
        assert!(
            REPORT_CSS
                .trim_end()
                .ends_with("/* end of cute-dbt report chassis v1 (cute-dbt#177) */"),
            "report.css end-of-file sentinel (truncation guard)",
        );
    }

    #[test]
    fn the_interaction_js_carries_its_banner_and_is_not_truncated() {
        // First-party sibling of the REPORT_CSS banner-pin test: proves
        // `include_str!` grabbed the interaction engine at its declared
        // version. The HEAD banner catches a renamed/replaced file; the
        // END-OF-FILE sentinel catches a truncated copy.
        assert!(
            INTERACTION_JS.contains("cute-dbt report interaction engine v1"),
            "interaction.js head banner",
        );
        assert!(
            INTERACTION_JS
                .trim_end()
                .ends_with("/* end of cute-dbt report interaction engine v1 (cute-dbt#178) */"),
            "interaction.js end-of-file sentinel (truncation guard)",
        );
    }

    #[test]
    fn the_theme_js_carries_its_banner_and_is_not_truncated() {
        assert!(
            THEME_JS.contains("cute-dbt appearance engine v1"),
            "theme.js head banner",
        );
        assert!(
            THEME_JS
                .trim_end()
                .ends_with("/* end of cute-dbt appearance engine v1 (cute-dbt#178) */"),
            "theme.js end-of-file sentinel (truncation guard)",
        );
        // The appearance persistence key is a stable consumer contract
        // (AC3, cute-dbt#178) — pin it here so a rename is a conscious act.
        assert!(
            THEME_JS.contains("\"cute-dbt.appearance.v1\""),
            "theme.js persists under the cute-dbt.appearance.v1 key",
        );
    }

    #[test]
    fn the_cyto_dag_js_carries_its_banner_and_is_not_truncated() {
        assert!(
            CYTO_DAG_JS.contains("cute-dbt cytoscape DAG engine v1"),
            "cyto-dag.js head banner",
        );
        assert!(
            CYTO_DAG_JS
                .trim_end()
                .ends_with("/* end of cute-dbt cytoscape DAG engine v1 (cute-dbt#180) */"),
            "cyto-dag.js end-of-file sentinel (truncation guard)",
        );
        // The engine reads its palette EXCLUSIVELY through the
        // interaction.js hook — a local palette table would dodge the
        // edge-vocab-completeness CI gate (which greps
        // JOIN_COLORS_LIGHT/_DARK in templates/interaction.js only).
        assert!(
            CYTO_DAG_JS.contains("window.cuteDagPalette"),
            "cyto-dag.js sources colors through the cuteDagPalette hook",
        );
        assert!(
            !CYTO_DAG_JS.contains("var JOIN_COLORS"),
            "cyto-dag.js must not declare its own edge palette table \
             (it would dodge the edge-vocab-completeness gate)",
        );
    }

    #[test]
    fn the_report_css_comments_are_balanced_and_keep_the_hidden_rule() {
        // The handoff's documented porting bug (its §2.1): a `*`
        // immediately followed by `/` INSIDE a CSS comment body closes
        // the comment early and silently eats the next rule — it once
        // ate `[hidden]{display:none!important}` and broke every
        // Diff/File toggle and diff fold. Two structural guards:
        //
        // 1. Comment delimiters balance. The bug's signature is an
        //    orphaned closer (`/* --fs-* / --pad-* /` written without
        //    the inner spaces yields one opener, two closers).
        let openers = REPORT_CSS.matches("/*").count();
        let closers = REPORT_CSS.matches("*/").count();
        assert_eq!(
            openers, closers,
            "CSS comment delimiters must balance — an excess `*/` means \
             a comment body contains the closer sequence and has eaten \
             the rules between (handoff §2.1 porting bug)",
        );
        // 2. The load-bearing rule the original bug ate survives
        //    comment-stripping — i.e. it is live CSS, not swallowed
        //    comment text.
        let mut stripped = String::with_capacity(REPORT_CSS.len());
        let mut rest = REPORT_CSS;
        while let Some(open) = rest.find("/*") {
            stripped.push_str(&rest[..open]);
            let Some(close) = rest[open + 2..].find("*/") else {
                rest = "";
                break;
            };
            rest = &rest[open + 2 + close + 2..];
        }
        stripped.push_str(rest);
        let normalized: String = stripped.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            normalized.contains("[hidden]{display:none!important;}"),
            "the [hidden] override (cute-dbt#121) must survive comment-stripping",
        );
    }
}
