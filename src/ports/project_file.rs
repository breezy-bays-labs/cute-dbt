//! `ProjectFileReader` — the port for reading a project-relative file
//! at render time.
//!
//! ADR-1's bar for introducing a trait seam is ">1 real-or-test impl".
//! The project-file reader clears it twice over: the run loop needs to
//! read project-relative files from disk in production
//! ([`FsProjectFileReader`](crate::adapters::project_file::FsProjectFileReader)),
//! but BDD scenarios and unit tests need to inject synthetic file
//! contents without touching the filesystem — an in-memory impl backed
//! by a `HashMap<String, String>`.
//!
//! It has **four consumers**, all reading project-relative files:
//!
//! - the **authoring-YAML drawer** (cute-dbt#69) — reads each in-scope
//!   unit test's source `.yml` (`original_file_path`) to slice the
//!   authored block;
//! - the **external fixture reader** (cute-dbt#126) — reads each external
//!   `given[i].fixture` / `expect.fixture` CSV/SQL file (the
//!   fully-resolved project-relative path) so a `fixture:`-sourced input
//!   renders a real cell grid instead of a silently-empty one;
//! - the **Model-YAML section** (cute-dbt#247) — reads the schema file
//!   each in-scope model's `patch_path` names to slice its authored
//!   `models:` entry;
//! - the **project-definition reader** (cute-dbt#266) — reads
//!   `dbt_project.yml` (a fixed project-relative name, not a manifest
//!   path) for the standing-metadata parse
//!   (`adapters::project_def::parse`) and the categorized
//!   project-change panel.
//!
//! Reading is intentionally **soft-failing**: the run loop continues
//! the report even if no project-file pipeline is wired or a specific
//! file cannot be read. Both surfaces are enhancements, not load-bearing
//! report facts. A missing file produces an `io::ErrorKind::NotFound`,
//! which the caller treats as "no content to surface for this test."
//!
//! ## Path discipline
//!
//! `read` takes a project-relative path of the form
//! `models/marts/core/_core__models.yml` or
//! `tests/fixtures/stg_orders.csv` — exactly the shapes carried by
//! `unit_tests.<id>.original_file_path` and `given[i].fixture` in the
//! manifest. The real adapter joins this against a `project_root`
//! (resolved by the CLI from `--project-root` or the manifest-path
//! derive). The path is validated to reject absolute paths and `..`
//! traversal to keep the adapter from being weaponized into a read
//! primitive against arbitrary filesystem locations.

use std::io;

/// A source of project-relative file contents, for the authoring-YAML
/// drawer and the external-fixture reader.
///
/// The trait is object-safe (`&self`, a `&str` argument, an owned
/// return) so the run loop can hold a `&dyn ProjectFileReader`.
pub trait ProjectFileReader {
    /// Read the file at the given project-relative path and return
    /// its contents as a UTF-8 string.
    ///
    /// # Errors
    ///
    /// - [`io::ErrorKind::NotFound`] — the file does not exist (this
    ///   is the soft-failure path; the caller treats it as "no content
    ///   to surface for this test").
    /// - [`io::ErrorKind::InvalidInput`] — the requested path is
    ///   absolute or contains `..` components and was rejected by the
    ///   adapter (defense against arbitrary-read).
    /// - Any other [`io::Error`] — surfaced as-is from the underlying
    ///   read.
    fn read(&self, project_relative: &str) -> io::Result<String>;
}
