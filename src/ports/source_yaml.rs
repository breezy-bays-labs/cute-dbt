//! `SourceYamlReader` — the v0.2 port for reading a `unit_test`'s source
//! YAML at render time.
//!
//! ADR-1's bar for introducing a trait seam is ">1 real-or-test impl".
//! The source-YAML reader clears it: the run loop needs to read
//! project-relative YAML files from disk in production
//! ([`FsSourceYamlReader`](crate::adapters::source_yaml::`FsSourceYamlReader`)),
//! but BDD scenarios and unit tests need to inject synthetic YAML
//! without touching the filesystem — an in-memory impl backed by a
//! `HashMap<String, String>`.
//!
//! Reading is intentionally **soft-failing**: the run loop continues
//! the report even if no source-YAML pipeline is wired or a specific
//! file cannot be read. The `Authoring YAML` drawer in the report is
//! an enhancement, not a load-bearing report fact. A missing file
//! produces an `io::ErrorKind::NotFound`, which the caller treats as
//! "no YAML to surface for this test."
//!
//! ## Path discipline
//!
//! `read` takes a project-relative path of the form
//! `models/marts/core/_core__models.yml` — exactly the shape carried
//! by `unit_tests.<id>.original_file_path` in the manifest. The real
//! adapter joins this against a `project_root` (resolved by the CLI
//! from `--project-root` or the manifest-path derive). The path is
//! validated to reject absolute paths and `..` traversal to keep the
//! adapter from being weaponized into a read primitive against
//! arbitrary filesystem locations.

use std::io;

/// A source of project-relative YAML file contents, for the
/// authoring-YAML drawer in the report.
///
/// The trait is object-safe (`&self`, a `&str` argument, an owned
/// return) so the run loop can hold a `&dyn SourceYamlReader`.
pub trait SourceYamlReader {
    /// Read the file at the given project-relative path and return
    /// its contents as a UTF-8 string.
    ///
    /// # Errors
    ///
    /// - [`io::ErrorKind::NotFound`] — the file does not exist (this
    ///   is the soft-failure path; the caller treats it as "no YAML
    ///   to surface for this test").
    /// - [`io::ErrorKind::InvalidInput`] — the requested path is
    ///   absolute or contains `..` components and was rejected by the
    ///   adapter (defense against arbitrary-read).
    /// - Any other [`io::Error`] — surfaced as-is from the underlying
    ///   read.
    fn read(&self, project_relative: &str) -> io::Result<String>;
}
