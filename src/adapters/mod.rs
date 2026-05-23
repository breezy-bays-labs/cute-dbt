//! Outward adapters. Depend on `domain` and `ports`; never imported by
//! `domain`.
//!
//! - [`manifest`] — serde structs against dbt schema v12
//!   (`#[serde(default)]`, no `deny_unknown_fields`); Stage-1 pre-flight
//!   (`Unreadable` / `SchemaUnsupported` / `BaselineUnusable`); the
//!   real-file manifest-source port impl.
//! - [`cte_engine`] — sqlparser-rs 0.62 parser-AST pass; CTE dependency
//!   graph + edge-type classification (`From`, the five joins,
//!   `UnionAll`, `UnionDistinct`).
//! - [`asset_embed`] — the vendored frontend bundle embedded via
//!   `include_str!` into `.rodata` + the `assets/MANIFEST.toml`
//!   provenance contract. Every vendored asset is text, so there is no
//!   `include_bytes!` user.
//! - [`render`] — askama 0.16 template + per-model payload assembly +
//!   node-role classification + import-CTE binding (produces the v0.1
//!   `report.html`).

pub mod asset_embed;
pub mod cte_engine;
pub mod manifest;
pub mod render;
