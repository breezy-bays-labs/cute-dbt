//! Gather outcome for a model's authored schema-file YAML block
//! (cute-dbt#247 — the Model-YAML drawer).
//!
//! The cli's `gather_model_yaml` stage resolves, for every in-scope
//! model, the schema/properties file the manifest's `patch_path` points
//! at (scheme-stripped at ingestion — `adapters::manifest::
//! strip_package_uri_scheme`), reads it through the `ProjectFileReader`
//! port, and slices the model's `models:` entry with
//! [`crate::domain::unit_test_yaml::extract_model_block`]. This module
//! owns the POD describing what that stage found — including every
//! honest-degrade arm, because the rendered Model-YAML section must
//! always say something truthful (a missing block names what is missing;
//! it never renders an empty or misleading drawer).
//!
//! On the `--pr-diff` arm the run loop additionally attaches an inline
//! block diff to each [`ModelYamlOutcome::Found`] entry via
//! [`crate::domain::pr_diff::attach_model_yaml_diffs`] — the
//! `reconstruct_block_diffs` sibling keyed on `patch_path` instead of a
//! unit test's `original_file_path`.
//!
//! POD-only, `std` + `serde` derive: the outcome map flows
//! cli → domain (diff attach) → render (payload mapping) in-process.

use serde::{Deserialize, Serialize};

use crate::domain::pr_diff::BlockDiff;
use crate::domain::unit_test_yaml::UnitTestYamlBlock;

/// What the `gather_model_yaml` stage found for one in-scope model.
///
/// Exactly one variant per model in the gather map. The render layer
/// translates each degrade variant into the truthful placeholder copy the
/// Model-YAML section shows — the wording lives there (adapter), the
/// facts live here (domain).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelYamlOutcome {
    /// The schema file was read and the model's `models:` entry sliced.
    Found {
        /// Project-relative schema file path (the scheme-stripped
        /// manifest `patch_path`) — the code-card header label.
        path: String,
        /// The sliced authored block (raw text + 1-based source span —
        /// the span feeds the pr-diff hunk-overlap math exactly like
        /// cute-dbt#96's unit-test blocks).
        block: UnitTestYamlBlock,
        /// Inline diff of the block, attached on the `--pr-diff` arm
        /// when the diff genuinely edited it
        /// ([`crate::domain::pr_diff::attach_model_yaml_diffs`]).
        /// `None` in baseline mode, for untouched blocks, stale diffs,
        /// and whitespace-only edits — the section then shows the plain
        /// File view.
        diff: Option<BlockDiff>,
    },
    /// The manifest records no `patch_path` for this model — no schema
    /// file declares it, so there is no authored entry to show.
    NoPatchPath,
    /// A `patch_path` exists but no project root was resolvable (no
    /// `--project-root`, and the `target/manifest.json` derive failed) —
    /// the file was never read.
    NoProjectRoot {
        /// The schema file the manifest points at (unread).
        path: String,
    },
    /// The schema file was not found under the project root. Also the
    /// honest arm for a package model whose patch file lives outside the
    /// project root (the path-safety guard rejects escaping paths).
    FileMissing {
        /// The schema file path that failed to resolve.
        path: String,
    },
    /// The schema file exists but could not be read (I/O error other
    /// than not-found).
    Unreadable {
        /// The schema file path that failed to read.
        path: String,
    },
    /// The schema file was read but contains no `models:` entry named
    /// after this model (the slicer returned `None`).
    EntryNotFound {
        /// The schema file that was searched.
        path: String,
    },
}
