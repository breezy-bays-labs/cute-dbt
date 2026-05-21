//! `ManifestSource` — the single v0.1 port: where a dbt `manifest.json`
//! is loaded from.
//!
//! ADR-1 sets the bar for introducing a trait seam at ">1 real-or-test
//! impl". The manifest source clears it: the run loop must load a
//! manifest the same way whether the bytes come from disk (production,
//! [`FileManifestSource`](crate::adapters::manifest::FileManifestSource))
//! or from a string (the in-memory impl in the test suite). The
//! renderer is deliberately *not* a port — v0.1 has one output format.
//!
//! `load` performs the **Stage-1 pre-flight** of ADR-2's two-stage
//! fail-closed contract: it deserializes the manifest and rejects
//! unreadable / pre-1.8 input before the run loop ever sees a
//! [`Manifest`]. The baseline manifest is loaded through
//! [`load_baseline`](crate::adapters::manifest::load_baseline), which
//! remaps any Stage-1 failure to [`PreflightError::BaselineUnusable`].

use std::path::Path;

use crate::domain::{Manifest, PreflightError};

/// A source of dbt manifests, addressed by path.
///
/// The path is a method parameter rather than constructor state so a
/// single source instance loads both the primary and the baseline
/// manifest. The trait is object-safe (`&self`, a `&Path` argument, an
/// owned return) so the run loop can hold a `&dyn ManifestSource`.
pub trait ManifestSource {
    /// Load and Stage-1 pre-flight the **primary** manifest at `path`.
    ///
    /// # Errors
    ///
    /// - [`PreflightError::Unreadable`] — the bytes could not be read,
    ///   were not valid JSON, or were missing a structurally required
    ///   key (`metadata.dbt_schema_version`).
    /// - [`PreflightError::SchemaUnsupported`] — the manifest's
    ///   `dbt_schema_version` is below the dbt ≥1.8 floor (schema v12)
    ///   or is not a recognizable `v<N>` token.
    fn load(&self, path: &Path) -> Result<Manifest, PreflightError>;
}
