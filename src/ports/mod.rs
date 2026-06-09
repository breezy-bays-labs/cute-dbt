//! Trait seams used by the run loop where >1 real-or-test impl exists.
//!
//! v0.1 introduced exactly one port — the manifest source — with PR 4b
//! (#6): real-file impl in `adapters/manifest.rs`, in-memory test impl
//! in the test suite.
//!
//! v0.2 adds the project-file reader port (`ProjectFileReader`,
//! cute-dbt#69 + cute-dbt#126): real-file impl in
//! `adapters/project_file.rs` plus an in-memory test impl used by BDD
//! scenarios that inject synthetic file contents without touching the
//! filesystem. Two consumers read through it — the authoring-YAML
//! drawer and the external unit-test fixture reader.
//!
//! The renderer is NOT a port — v0.x has one output format.

pub mod manifest_source;
pub mod project_file;

pub use manifest_source::ManifestSource;
pub use project_file::ProjectFileReader;
