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
}
