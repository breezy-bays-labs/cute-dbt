//! Real-file `ProjectFileReader` impl + path-safety guard.
//!
//! `FsProjectFileReader` reads UTF-8 files from a fixed `project_root`
//! (unit-test source YAML for the authoring drawer, and external
//! `given`/`expect` fixture CSV/SQL files), with the project-relative
//! path validated against absolute-path and `..` traversal to keep this
//! adapter from being weaponized into an arbitrary-read primitive.
//!
//! Soft failure surface — see `ports::project_file::ProjectFileReader`
//! for the contract: `NotFound` is the "this test has no surfaceable
//! content" signal, not a fatal error.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::ports::ProjectFileReader;

/// Reads project-relative files from a fixed `project_root`.
///
/// The project-relative path passed to [`read`](Self::read) (or to the
/// `ProjectFileReader` trait impl) must:
///
/// - be a relative path (no leading `/`);
/// - contain no `..` components (no parent traversal);
/// - contain no prefix or root-dir components (Windows-style).
///
/// Violations return [`io::ErrorKind::InvalidInput`] without touching
/// the filesystem. Files outside `project_root` are unreachable
/// through this adapter.
#[derive(Debug, Clone)]
pub struct FsProjectFileReader {
    project_root: PathBuf,
}

impl FsProjectFileReader {
    /// Construct a reader rooted at `project_root`. The path is taken
    /// verbatim (no canonicalization); callers are expected to have
    /// resolved it from `--project-root` or the manifest-path derive.
    #[must_use]
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }

    /// Project root this reader is anchored at.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}

impl ProjectFileReader for FsProjectFileReader {
    fn read(&self, project_relative: &str) -> io::Result<String> {
        validate_project_relative(project_relative)?;
        let full = self.project_root.join(project_relative);
        fs::read_to_string(&full)
    }
}

/// Reject project-relative paths that would escape the project root or
/// reference absolute filesystem locations. Returns `InvalidInput` on
/// any rejection — semantically distinct from `NotFound` so callers
/// can distinguish a misconfigured manifest from a missing file.
fn validate_project_relative(p: &str) -> io::Result<()> {
    let path = Path::new(p);
    for c in path.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("project-relative path may not contain `..`: {p}"),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("project-relative path may not be absolute: {p}"),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::pedantic, clippy::cargo)]
mod tests {
    use super::*;

    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "cute-dbt-fs-project-file-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn reads_a_project_relative_file() {
        let root = temp_root("read-relative");
        let nested = root.join("models").join("marts");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("foo.yml"), "version: 2\n").unwrap();

        let r = FsProjectFileReader::new(root.clone());
        let contents = r.read("models/marts/foo.yml").unwrap();
        assert_eq!(contents, "version: 2\n");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_a_project_relative_fixture_csv() {
        // The second consumer (cute-dbt#126): an external fixture path
        // under `tests/fixtures/` reads through the same adapter.
        let root = temp_root("read-fixture-csv");
        let nested = root.join("tests").join("fixtures");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("stg_orders.csv"), "id,name\n1,alice\n").unwrap();

        let r = FsProjectFileReader::new(root.clone());
        let contents = r.read("tests/fixtures/stg_orders.csv").unwrap();
        assert_eq!(contents, "id,name\n1,alice\n");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_absolute_path_with_invalid_input() {
        let root = temp_root("reject-abs");
        let r = FsProjectFileReader::new(root.clone());
        let err = r.read("/etc/passwd").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_parent_traversal_with_invalid_input() {
        let root = temp_root("reject-dotdot");
        let r = FsProjectFileReader::new(root.clone());
        let err = r.read("models/../../../etc/passwd").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_file_surfaces_as_not_found() {
        let root = temp_root("missing");
        let r = FsProjectFileReader::new(root.clone());
        let err = r.read("models/does_not_exist.yml").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cur_dir_components_are_allowed() {
        // dbt's `original_file_path` is always shaped without leading
        // `./`, but defensive: a `./models/foo.yml` should not be
        // rejected. (Component::CurDir is a no-op.)
        let root = temp_root("curdir");
        let nested = root.join("models");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("bar.yml"), "x: 1\n").unwrap();

        let r = FsProjectFileReader::new(root.clone());
        let contents = r.read("./models/bar.yml").unwrap();
        assert_eq!(contents, "x: 1\n");
        let _ = fs::remove_dir_all(&root);
    }
}
