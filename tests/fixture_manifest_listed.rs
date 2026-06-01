//! Synthetic-only fixture gate (test-side mirror of the CI grep).
//!
//! Every file committed under `tests/fixtures/` MUST be listed in
//! `tests/fixtures/MANIFEST.toml` with `synthetic_only = true`. The
//! structural enforcement is the CI job `fixture-manifest-gate` (failing
//! the PR if any file is unlisted); this `cargo test` is the
//! belt-and-braces local signal — same constraint, fast feedback before
//! push.
//!
//! The test asserts three things over `[[fixture]]`:
//!
//! 1. Every regular file under `tests/fixtures/` (excluding `MANIFEST.toml`
//!    itself) appears in the manifest's `[[fixture]]` table by `path`.
//! 2. Every entry has `synthetic_only = true` (the synthetic-only invariant).
//! 3. Every entry's `sha256` matches the SHA-256 of the file on disk.
//!
//! Empty-fixture set is a valid state (assertion #1 vacuously true) — this
//! lets PR 4a land the gate infrastructure before any fixture is committed
//! and lets PR 4b grow the set without re-shaping the test.
//!
//! cute-dbt#115 widens the gate to the same three assertions over
//! `[[project_data]]` — the DATA carriers of the embedded `dbt-project/`
//! (the committed `target/manifest.json` plus the seed CSVs under
//! `dbt-project/seeds/`). The covered set is enumerated via `git ls-files`
//! (never a filesystem walk — a dev's local `dbt compile` leaves
//! build-output under `dbt-project/target/` that a walk would list and
//! fail on), scoped to the data carriers: the seeds directory exhaustively
//! plus the one explicitly-pinned committed manifest. dbt-project source
//! SQL / YAML / config is code, not data, and is intentionally not listed.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

/// The one dbt-project data carrier that lives outside `dbt-project/seeds/`
/// (which is scanned exhaustively). Pinned explicitly, repo-root relative.
const PROJECT_MANIFEST_PATH: &str = "dbt-project/target/manifest.json";

#[derive(Debug, serde::Deserialize)]
struct ManifestFile {
    #[serde(default)]
    fixture: Vec<FixtureEntry>,
    /// dbt-project/ data carriers (cute-dbt#115). Same provenance shape as
    /// `[[fixture]]`, but `path` is repo-root relative because these files
    /// live outside `tests/fixtures/`.
    #[serde(default)]
    project_data: Vec<FixtureEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct FixtureEntry {
    path: String,
    #[serde(default)]
    origin: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    origin_url: Option<String>,
    sha256: String,
    synthetic_only: bool,
    #[serde(default)]
    synthetic_handcrafted: Option<bool>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The crate manifest dir IS the repo root (single-crate layout), so
/// `[[project_data]]` repo-root-relative paths resolve against it.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The git-tracked dbt-project DATA carriers, repo-root relative.
///
/// Enumerated via `git ls-files` (never a filesystem walk) so a dev's
/// local `dbt compile` output under `dbt-project/target/` cannot leak in
/// and fail the gate. Scope = the seeds directory (exhaustive) plus the
/// one explicitly-pinned committed manifest.
fn git_tracked_project_data() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let output = Command::new("git")
        .args(["ls-files", "dbt-project/seeds/", PROJECT_MANIFEST_PATH])
        .current_dir(repo_root())
        .output()
        .expect("`git ls-files` runs (the gate enumerates tracked data carriers)");
    assert!(
        output.status.success(),
        "`git ls-files` failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    for line in String::from_utf8(output.stdout)
        .expect("git ls-files output is UTF-8")
        .lines()
    {
        let line = line.trim();
        if !line.is_empty() {
            out.insert(line.to_string());
        }
    }
    out
}

fn manifest_path() -> PathBuf {
    fixtures_dir().join("MANIFEST.toml")
}

fn load_manifest() -> ManifestFile {
    let bytes = fs::read_to_string(manifest_path())
        .expect("tests/fixtures/MANIFEST.toml must exist (synthetic-only invariant)");
    toml::from_str(&bytes).expect("tests/fixtures/MANIFEST.toml must be valid TOML")
}

fn walk_committed_fixtures() -> BTreeSet<String> {
    let root = fixtures_dir();
    let mut out = BTreeSet::new();
    if !root.exists() {
        return out;
    }
    for entry in WalkDir::new(&root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let abs = entry.path();
        let name = abs
            .file_name()
            .and_then(|n| n.to_str())
            .expect("fixture filename must be UTF-8");
        if name == "MANIFEST.toml" {
            continue;
        }
        let rel = abs
            .strip_prefix(&root)
            .expect("walkdir entries are under fixtures_dir")
            .to_str()
            .expect("fixture path must be UTF-8")
            .to_string();
        out.insert(rel);
    }
    out
}

fn sha256_hex(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hex::encode(hasher.finalize())
}

#[test]
fn every_committed_fixture_is_listed_in_manifest() {
    let manifest = load_manifest();
    let listed: BTreeSet<String> = manifest.fixture.iter().map(|f| f.path.clone()).collect();
    let committed = walk_committed_fixtures();

    let unlisted: Vec<&String> = committed.difference(&listed).collect();
    assert!(
        unlisted.is_empty(),
        "synthetic-only invariant violation: files under tests/fixtures/ \
         are not listed in MANIFEST.toml: {unlisted:?}\n\
         Every committed fixture must have a [[fixture]] entry with \
         synthetic_only = true."
    );

    let missing: Vec<&String> = listed.difference(&committed).collect();
    assert!(
        missing.is_empty(),
        "MANIFEST.toml lists fixtures that are not present on disk: {missing:?}"
    );
}

#[test]
fn every_listed_fixture_is_synthetic_only() {
    let manifest = load_manifest();
    let non_synthetic: Vec<&str> = manifest
        .fixture
        .iter()
        .filter(|f| !f.synthetic_only)
        .map(|f| f.path.as_str())
        .collect();
    assert!(
        non_synthetic.is_empty(),
        "synthetic-only invariant violation: fixtures must have \
         synthetic_only = true: {non_synthetic:?}"
    );
}

#[test]
fn every_listed_sha256_matches_disk() {
    let manifest = load_manifest();
    let root = fixtures_dir();
    for entry in &manifest.fixture {
        let path = root.join(&entry.path);
        if !path.exists() {
            // Reported by `every_committed_fixture_is_listed_in_manifest`.
            continue;
        }
        let actual = sha256_hex(&path);
        assert_eq!(
            actual, entry.sha256,
            "SHA-256 mismatch for {} — MANIFEST.toml is out of sync with disk. \
             Recompute with `shasum -a 256 tests/fixtures/{}`.",
            entry.path, entry.path
        );
        // Touch the structural fields so a future schema-evolution drop
        // surfaces as a deserialization failure, not a silent ignore.
        let _ = (
            &entry.origin,
            &entry.source,
            &entry.origin_url,
            &entry.synthetic_handcrafted,
            &entry.license,
            &entry.description,
        );
    }
}

// ---------------------------------------------------------------------------
// dbt-project/ data-carrier gate (cute-dbt#115). Same three assertions as
// the `[[fixture]]` gate above, over the `[[project_data]]` table — the
// committed `dbt-project/target/manifest.json` plus the seed CSVs under
// `dbt-project/seeds/`. This closes the #114-review gap (seeds previously
// outside the synthetic-only scan, doc-enforced only).
// ---------------------------------------------------------------------------

#[test]
fn every_dbt_project_data_carrier_is_listed_in_manifest() {
    let manifest = load_manifest();
    let listed: BTreeSet<String> = manifest
        .project_data
        .iter()
        .map(|f| f.path.clone())
        .collect();
    let tracked = git_tracked_project_data();

    let unlisted: Vec<&String> = tracked.difference(&listed).collect();
    assert!(
        unlisted.is_empty(),
        "synthetic-only invariant violation: git-tracked dbt-project data \
         carriers are not listed in MANIFEST.toml [[project_data]]: {unlisted:?}\n\
         Every committed seed under dbt-project/seeds/ and the committed \
         dbt-project/target/manifest.json must have a [[project_data]] entry \
         with synthetic_only = true."
    );

    let missing: Vec<&String> = listed.difference(&tracked).collect();
    assert!(
        missing.is_empty(),
        "MANIFEST.toml [[project_data]] lists paths that are not git-tracked \
         dbt-project data carriers: {missing:?}"
    );
}

#[test]
fn every_listed_project_data_is_synthetic_only() {
    let manifest = load_manifest();
    let non_synthetic: Vec<&str> = manifest
        .project_data
        .iter()
        .filter(|f| !f.synthetic_only)
        .map(|f| f.path.as_str())
        .collect();
    assert!(
        non_synthetic.is_empty(),
        "synthetic-only invariant violation: [[project_data]] entries must \
         have synthetic_only = true: {non_synthetic:?}"
    );
}

#[test]
fn every_listed_project_data_sha256_matches_disk() {
    let manifest = load_manifest();
    let root = repo_root();
    for entry in &manifest.project_data {
        let path = root.join(&entry.path);
        if !path.exists() {
            // Reported by `every_dbt_project_data_carrier_is_listed_in_manifest`.
            continue;
        }
        let actual = sha256_hex(&path);
        assert_eq!(
            actual, entry.sha256,
            "SHA-256 mismatch for {} — MANIFEST.toml [[project_data]] is out of \
             sync with disk. Recompute with `shasum -a 256 {}`.",
            entry.path, entry.path
        );
        let _ = (
            &entry.origin,
            &entry.source,
            &entry.origin_url,
            &entry.synthetic_handcrafted,
            &entry.license,
            &entry.description,
        );
    }
}
