//! Asset-inlining infrastructure — the vendored frontend bundle embedded
//! into the binary's `.rodata` at compile time.
//!
//! ## What this module embeds
//!
//! Five vendored assets — Sakura CSS, jQuery, `DataTables` (JS + CSS)
//! and the Mermaid UMD bundle — are pulled in with [`include_str!`] so their
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
        // DataTables CSS ships no banner comment; assert a DataTables-only
        // custom property is present.
        assert!(
            DATATABLES_CSS.contains("--dt-row-selected"),
            "datatables css content",
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
