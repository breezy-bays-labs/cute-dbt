//! cute-dbt — zero-compute Rust CLI that parses a dbt `manifest.json` and emits
//! one self-contained interactive HTML report visualizing dbt unit tests.
//!
//! The library surface is **internal-only** during v0.x; the binary
//! (`cute-dbt`) is the product. Module layout follows hexagonal
//! inward-dependency discipline:
//!
//! - [`domain`] — owned data + pure computation (no I/O, no parser deps)
//! - [`ports`] — trait seams with >1 real-or-test impl
//! - [`adapters`] — serde manifest reader, sqlparser CTE engine, askama
//!   renderer, asset-inlining infra
//! - [`cli`] — clap derive, `ExitCode` mapping, run-loop composition
//!
//! See `ARCHITECTURE.md` at the repo root for the full layering invariant,
//! the two-stage fail-closed contract, and the conscious design
//! simplifications.

#![warn(clippy::pedantic, clippy::cargo, missing_docs)]
#![forbid(unsafe_code)]

pub mod adapters;
pub mod cli;
pub mod domain;
pub mod ports;
