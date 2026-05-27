//! Trait seams used by the run loop where >1 real-or-test impl exists.
//!
//! v0.1 introduced exactly one port — the manifest source — with PR 4b
//! (#6): real-file impl in `adapters/manifest.rs`, in-memory test impl
//! in the test suite.
//!
//! v0.2 adds the source-YAML reader port (`SourceYamlReader`, cute-dbt#69):
//! real-file impl in `adapters/source_yaml.rs` plus an in-memory test
//! impl used by BDD scenarios that inject synthetic YAML without
//! touching the filesystem.
//!
//! The renderer is NOT a port — v0.x has one output format.

pub mod manifest_source;
pub mod source_yaml;

pub use manifest_source::ManifestSource;
pub use source_yaml::SourceYamlReader;
