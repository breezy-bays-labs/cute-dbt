//! Domain types and pure computation.
//!
//! Depends on `std` and `serde` derive only. No I/O, no parser libraries,
//! no `clap`, no `askama`. POD-only (owned data, constructors, no method
//! machinery beyond what the run loop calls).
//!
//! Filled across PRs 3 / 5 / 6:
//!
//! - **PR 3 (#TBD)** — core types: `Manifest`, `Node`, `UnitTest`,
//!   `CteGraph`/`CteNode`/`CteEdge`, `JoinType`, `ModifiedSet`,
//!   `PreflightError` (`#[non_exhaustive]`, 4 variants).
//! - **PR 5 (#TBD)** — `state` submodule: `StateModifier` trait
//!   (object-safe, deliberately not `Send + Sync`), `BodyChecksumModifier`,
//!   `StateComparator::body_only`, `modified_set`, in-scope selection.
//! - **PR 6 (#TBD)** — `preflight` submodule: Stage-2 compiled-SQL
//!   presence check (runs AFTER scope selects the in-scope set).
