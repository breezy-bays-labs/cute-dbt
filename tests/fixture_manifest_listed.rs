//! PHI-safe fixture gate (test-side mirror of the CI grep).
//!
//! Per ADR-5 (`ops/decisions/cute-dbt/adr-mvp-architecture.md`), every file
//! committed under `tests/fixtures/` MUST be listed in
//! `tests/fixtures/MANIFEST.toml` with `synthetic_only = true`. The
//! structural enforcement is the CI job `fixture-manifest-gate` (failing
//! the PR if any file is unlisted); this `cargo test` is the
//! belt-and-braces local signal — same constraint, fast feedback before
//! push.
//!
//! The test asserts three things:
//!
//! 1. Every regular file under `tests/fixtures/` (excluding `MANIFEST.toml`
//!    itself) appears in the manifest's `[[fixture]]` table by `path`.
//! 2. Every entry has `synthetic_only = true` (the PHI-safe invariant).
//! 3. Every entry's `sha256` matches the SHA-256 of the file on disk.
//!
//! Empty-fixture set is a valid state (assertion #1 vacuously true) — this
//! lets PR 4a land the gate infrastructure before any fixture is committed
//! and lets PR 4b grow the set without re-shaping the test.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

#[derive(Debug, serde::Deserialize)]
struct ManifestFile {
    #[serde(default)]
    fixture: Vec<FixtureEntry>,
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

fn manifest_path() -> PathBuf {
    fixtures_dir().join("MANIFEST.toml")
}

fn load_manifest() -> ManifestFile {
    let bytes = fs::read_to_string(manifest_path())
        .expect("tests/fixtures/MANIFEST.toml must exist (PHI-safe invariant)");
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
        "ADR-5 PHI-safe invariant violation: files under tests/fixtures/ \
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
        "ADR-5 PHI-safe invariant violation: fixtures must have \
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
