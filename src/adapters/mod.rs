//! Outward adapters. Depend on `domain` and `ports`; never imported by
//! `domain`.
//!
//! Filled across PRs 4b / 7 / 8a / 8b:
//!
//! - **PR 4b (#TBD)** — `manifest.rs`: serde structs against dbt schema
//!   v12 (`#[serde(default)]`, no `deny_unknown_fields`); Stage-1
//!   pre-flight (`Unreadable` / `SchemaUnsupported` / `BaselineUnusable`);
//!   the real-file manifest-source port impl.
//! - **PR 7 (#TBD)** — `cte_engine.rs`: sqlparser-rs 0.62 parser-AST pass;
//!   CTE dependency graph + join-type edge classification (inner / left /
//!   right / full / cross). No tokenizer/comment pass (v0.2).
//! - **PR 8a (#TBD)** — `asset_embed.rs`: `include_str!` / `include_bytes!`
//!   embedding infra + `assets/MANIFEST.toml` provenance contract.
//! - **PR 8b (#TBD)** — `render.rs`: askama 0.16 template reproducing the
//!   returned Claude Design `report.html` DOM/class contract.
