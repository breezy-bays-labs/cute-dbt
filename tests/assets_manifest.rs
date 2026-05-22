//! Vendored-asset provenance gate (test-side mirror of the CI grep).
//!
//! Every file committed under `assets/` MUST be listed in
//! `assets/MANIFEST.toml` with a pinned version, a canonical source URL,
//! a SHA-256, and a permissive SPDX license. The structural enforcement
//! is the CI job `assets-manifest-gate`; this `cargo test` is the
//! belt-and-braces local signal — same constraint, fast feedback before
//! push. It mirrors `tests/fixture_manifest_listed.rs`.
//!
//! The four assertions:
//!
//! 1. Every regular file under `assets/` (excluding `MANIFEST.toml`)
//!    appears in the manifest's `[[asset]]` table by `path`, and every
//!    listed `path` is present on disk.
//! 2. Every entry's `sha256` matches the SHA-256 of the file on disk.
//! 3. Every entry's `license` is in the permissive (non-copyleft) set.
//! 4. Every entry records complete provenance (name, version, an
//!    `https://` source, a 64-hex-char SHA-256).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

/// SPDX licenses cute-dbt accepts for a vendored frontend asset — the
/// permissive (non-copyleft) set, consistent with `deny.toml`'s crate
/// allowlist. A copyleft asset in the bundle is a release blocker.
const PERMISSIVE_LICENSES: &[&str] = &[
    "MIT",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "Apache-2.0",
    "ISC",
    "0BSD",
];

#[derive(Debug, serde::Deserialize)]
struct ManifestFile {
    #[serde(default)]
    asset: Vec<AssetEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct AssetEntry {
    name: String,
    version: String,
    path: String,
    source: String,
    sha256: String,
    license: String,
}

fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}

fn manifest_path() -> PathBuf {
    assets_dir().join("MANIFEST.toml")
}

fn load_manifest() -> ManifestFile {
    let bytes = fs::read_to_string(manifest_path())
        .expect("assets/MANIFEST.toml must exist (asset-provenance invariant)");
    toml::from_str(&bytes).expect("assets/MANIFEST.toml must be valid TOML")
}

fn walk_committed_assets() -> BTreeSet<String> {
    let root = assets_dir();
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
            .expect("asset filename must be UTF-8");
        if name == "MANIFEST.toml" {
            continue;
        }
        // Normalize to POSIX separators so the walk matches the
        // forward-slash `path` values in MANIFEST.toml on every platform.
        let rel = abs
            .strip_prefix(&root)
            .expect("walkdir entries are under assets_dir")
            .to_string_lossy()
            .replace('\\', "/");
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
fn every_committed_asset_is_listed_in_manifest() {
    let manifest = load_manifest();
    let listed: BTreeSet<String> = manifest.asset.iter().map(|a| a.path.clone()).collect();
    let committed = walk_committed_assets();

    let unlisted: Vec<&String> = committed.difference(&listed).collect();
    assert!(
        unlisted.is_empty(),
        "asset-provenance violation: files under assets/ are not listed \
         in MANIFEST.toml: {unlisted:?}\n\
         Every vendored asset must have an [[asset]] entry."
    );

    let missing: Vec<&String> = listed.difference(&committed).collect();
    assert!(
        missing.is_empty(),
        "MANIFEST.toml lists assets that are not present on disk: {missing:?}"
    );
}

#[test]
fn every_listed_sha256_matches_disk() {
    let manifest = load_manifest();
    let root = assets_dir();
    for entry in &manifest.asset {
        let path = root.join(&entry.path);
        if !path.exists() {
            // Reported by `every_committed_asset_is_listed_in_manifest`.
            continue;
        }
        let actual = sha256_hex(&path);
        assert_eq!(
            actual, entry.sha256,
            "SHA-256 mismatch for {} — MANIFEST.toml is out of sync with disk. \
             Recompute with `shasum -a 256 assets/{}`.",
            entry.path, entry.path
        );
    }
}

#[test]
fn every_listed_license_is_permissive() {
    let manifest = load_manifest();
    let copyleft: Vec<(&str, &str)> = manifest
        .asset
        .iter()
        .filter(|a| !PERMISSIVE_LICENSES.contains(&a.license.as_str()))
        .map(|a| (a.path.as_str(), a.license.as_str()))
        .collect();
    assert!(
        copyleft.is_empty(),
        "asset-provenance violation: every vendored asset must carry a \
         permissive SPDX license {PERMISSIVE_LICENSES:?}; offenders \
         (path, license): {copyleft:?}"
    );
}

#[test]
fn every_entry_records_complete_provenance() {
    let manifest = load_manifest();
    assert!(
        !manifest.asset.is_empty(),
        "v0.1 vendors the five-asset frontend bundle"
    );
    for entry in &manifest.asset {
        assert!(!entry.name.is_empty(), "{}: name is recorded", entry.path);
        assert!(
            !entry.version.is_empty(),
            "{}: version is recorded",
            entry.path
        );
        assert!(
            entry.source.starts_with("https://"),
            "{}: source must be an https:// URL, got {:?}",
            entry.path,
            entry.source,
        );
        assert_eq!(
            entry.sha256.len(),
            64,
            "{}: sha256 is 64 hex characters",
            entry.path,
        );
        assert!(
            entry.sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "{}: sha256 is hexadecimal",
            entry.path,
        );
    }
}
