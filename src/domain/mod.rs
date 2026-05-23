//! Domain types and pure computation.
//!
//! Depends on `std` and `serde` derive only. No I/O, no parser libraries,
//! no `clap`, no `askama`. POD-only (owned data, constructors, no method
//! machinery beyond what the run loop calls). The single-crate
//! `non-mirror-guard` cannot directly enforce the inward-dependency
//! discipline (one crate cannot fail-to-compile on an inward `use`), so
//! `tests/domain_clean_arch.rs` greps `src/domain/**/*.rs` for
//! `use crate::adapters` to give ADR-1 a build-break.
//!
//! Filled across PRs 3 / 5 / 6:
//!
//! - **PR 3 (#4)** — core types: [`Manifest`], [`Node`], [`UnitTest`],
//!   [`CteGraph`] / [`CteNode`] / [`CteEdge`], [`EdgeType`],
//!   [`ModifiedSet`], [`PreflightError`] (`#[non_exhaustive]`, 4
//!   variants).
//! - **PR 5 (#7)** — `state` submodule additions: `StateModifier`
//!   trait (object-safe, deliberately not `Send + Sync`),
//!   `BodyChecksumModifier`, `StateComparator` (`body_only`,
//!   `modified_set`, `in_scope_unit_tests`), `InScopeSet`, and
//!   `resolve_target_model` (bare `model:` name → full node).
//! - **PR 6 (#8)** — `preflight::preflight_compiled` (the Stage-2
//!   compiled-SQL presence check — runs AFTER scope selects the
//!   in-scope set) and `state::BANNER_EMPTY_SCOPE` (the shared
//!   empty-scope banner constant).

// `domain::manifest::Manifest` / `unit_test::UnitTest` / `cte::CteGraph`
// are the cleanest names from inside the module (the module name
// disambiguates), but clippy::pedantic's `module_name_repetitions`
// objects at the re-export site. The module names ARE the type
// categories — repetition is intentional, so silence the lint locally.
#![allow(clippy::module_name_repetitions)]

pub mod cte;
pub mod manifest;
pub mod preflight;
pub mod state;
pub mod unit_test;

pub use cte::{CteEdge, CteGraph, CteNode, EdgeType, Span};
pub use manifest::{Checksum, DependsOn, Manifest, ManifestMetadata, Node, NodeId};
pub use preflight::{PreflightError, preflight_compiled};
pub use state::{
    BANNER_EMPTY_SCOPE, BodyChecksumModifier, InScopeSet, ModifiedSet, ModifierKind,
    StateComparator, StateModifier, resolve_target_model,
};
pub use unit_test::{UnitTest, UnitTestExpect, UnitTestGiven};
