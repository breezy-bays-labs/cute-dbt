//! Outward adapters. Depend on `domain` and `ports`; never imported by
//! `domain`.
//!
//! - [`manifest`] — serde structs against dbt schema v12
//!   (`#[serde(default)]`, no `deny_unknown_fields`); Stage-1 pre-flight
//!   (`Unreadable` / `SchemaUnsupported` / `BaselineUnusable`); the
//!   real-file manifest-source port impl.
//! - [`config_reader`] — TOML loader for the operator-supplied
//!   `--config <PATH>` (PR 14, #24). Failures are clap usage errors
//!   surfaced via the value-parser fn in [`crate::cli`], **never** a
//!   `PreflightError` variant.
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
//! - [`explore`] — the `cute-dbt explore` two-page renderer
//!   (cute-dbt#100, cute-dbt#101): the interactive full-manifest model
//!   lineage (`dag.html` — Cytoscape + cytoscape-dagre over the
//!   `LineagePayload` carrier, with hand-rolled fuzzy search and the
//!   epic-#99 highlight/focus interaction) + the unit-test index
//!   (`tests.html`). Reuses [`render`]'s engine-agnostic
//!   `build_payload` output; fail-open on uncompiled models (rendered
//!   as "not compiled").
//! - [`findings_emit`] — the machine-readable findings-envelope sidecar
//!   (cute-dbt#386, epic #261): collects the in-scope findings via the
//!   SAME `model_findings → apply_check_policy` pipeline the renderer
//!   runs, wraps them in the versioned `FindingsEnvelope` POD, and writes
//!   the JSON beside the HTML report (`--findings-out`). Purely additive
//!   — never touches `report.html` or [`render`].
//! - [`project_file`] — the v0.2 `ProjectFileReader` port impl
//!   (`FsProjectFileReader`). Reads project-relative files for the
//!   authoring-YAML drawer (cute-dbt#69) and the external unit-test
//!   fixture reader (cute-dbt#126). Soft failure path: `NotFound` is the
//!   "no content to surface for this test" signal, not a fatal error.
//! - [`pr_comments`] — the gen-time GitHub PR review-thread ingestion
//!   rung (cute-dbt#395): spawns `gh api graphql` to pull a PR's
//!   `reviewThreads` + general comments into the [`crate::domain::PrComments`]
//!   POD. Fail-soft like the `review` verb's `gh pr view` rung (`gh`
//!   missing / non-zero exit / unparseable response ⇒ empty result,
//!   never a panic). Ingestion only — anchoring lands a later slice.
//! - [`project_def`] — the `dbt_project.yml` parser (cute-dbt#266):
//!   dbt-yaml (the engine's own published serde-yaml fork — fusion's
//!   exact loading semantics: Overwrite duplicate-key policy +
//!   `apply_merge`) into the domain `ProjectDefinition` POD. A plain
//!   `fn parse(&str)`, no port trait (one impl; the serde-saphyr
//!   contingency swaps behind the same seam); file access stays on the
//!   `ProjectFileReader` port.

pub mod asset_embed;
pub mod config_reader;
pub mod cte_engine;
pub mod explore;
pub mod findings_emit;
pub mod manifest;
pub mod pr_comments;
pub mod project_def;
pub mod project_file;
pub mod render;
