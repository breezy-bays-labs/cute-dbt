//! Fixture parse + container-shape detection.
//!
//! For each fixture listed in `tests/fixtures/MANIFEST.toml`, this test:
//!
//! 1. Opens the file and parses it as a `serde_json::Value`.
//! 2. Asserts top-level keys expected of a dbt manifest exist
//!    (`metadata`, `nodes`).
//! 3. Asserts the container shape is **A** — a top-level `unit_tests`
//!    map (vs. **B**: embedded in `nodes` with
//!    `resource_type == "unit_test"`; **C**: both; **none**: neither).
//!
//! PR 4a left the container shape as a build-phase resolution (ADR-5);
//! PR 4b (#6) resolved it to **shape A** against the real jaffle-shop
//! fixture and committed `adapters::manifest`'s serde layout to it. This
//! test is now the guard for that commitment: a committed fixture in any
//! other shape would silently ingest zero unit tests, so it must fail
//! loudly here. The empty-fixture-set state still passes vacuously.
//!
//! For tests/fixtures-empty PR 4a: a sentinel test exercises the parse +
//! shape-detection logic against an in-memory minimal manifest so the
//! mechanical surface has coverage from day one (PR 4b grows real
//! coverage with the real fixture).

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

#[derive(Debug, serde::Deserialize)]
struct ManifestFile {
    #[serde(default)]
    fixture: Vec<FixtureEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct FixtureEntry {
    path: String,
}

/// Which container shape carries unit-test entries in a dbt manifest.
///
/// Labels mirror PR-body letters so the test output is grep-able by the
/// human / agent reading PR 4a → PR 4b: "Container shape: A | B | C | None".
#[derive(Debug, PartialEq, Eq)]
enum ContainerShape {
    /// **A**: top-level `unit_tests` map populated; no `unit_test` nodes.
    TopLevelMap,
    /// **B**: `unit_tests` absent/empty; `nodes` contains
    /// `resource_type:"unit_test"`.
    EmbeddedInNodes,
    /// **C**: both shapes populated (some dbt versions for backward compat).
    Both,
    /// **None**: neither shape carries unit-test entries.
    None,
}

/// JSON fixtures that are deliberately **not** dbt manifests, so the
/// manifest-shape assertions below do not apply to them. The non-`.json`
/// skip (config TOML, pr-diff patches) is by suffix; these are JSON by
/// extension but a different wire entirely, so they are named explicitly
/// (and greppably) here. Each has its own dedicated assertions.
///
/// - `pr-review-threads.json` — the synthetic `gh api graphql`
///   `reviewThreads` response for the PR review-thread ingestion adapter
///   (cute-dbt#395); pinned by `tests/pr_comments_ingestion.rs` and the
///   `adapters::pr_comments` unit suite.
/// - `comments-showcase-pr-comments.json` — the synthetic `gh api graphql`
///   `reviewThreads` payload that drives the comments-showcase GOLDEN
///   (cute-dbt#419–#422); fed to `report --pr-comments @<file>` and pinned
///   by the `example-report-check` byte gate (the comments-showcase row).
const NON_MANIFEST_JSON_FIXTURES: &[&str] = &[
    "pr-review-threads.json",
    "comments-showcase-pr-comments.json",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_manifest() -> ManifestFile {
    let bytes = fs::read_to_string(fixtures_dir().join("MANIFEST.toml"))
        .expect("tests/fixtures/MANIFEST.toml must exist");
    toml::from_str(&bytes).expect("tests/fixtures/MANIFEST.toml must be valid TOML")
}

fn detect_container_shape(manifest: &Value) -> ContainerShape {
    let top_level_populated = manifest
        .get("unit_tests")
        .and_then(Value::as_object)
        .is_some_and(|m| !m.is_empty());

    let nodes_has_unit_test = manifest
        .get("nodes")
        .and_then(Value::as_object)
        .is_some_and(|nodes| {
            nodes
                .values()
                .any(|v| v.get("resource_type").and_then(Value::as_str) == Some("unit_test"))
        });

    match (top_level_populated, nodes_has_unit_test) {
        (true, true) => ContainerShape::Both,
        (true, false) => ContainerShape::TopLevelMap,
        (false, true) => ContainerShape::EmbeddedInNodes,
        (false, false) => ContainerShape::None,
    }
}

#[test]
fn every_listed_fixture_parses_and_reports_shape() {
    let manifest = load_manifest();
    if manifest.fixture.is_empty() {
        // Empty-set state of PR 4a — the gate infra ships before any real
        // fixture lands. PR 4b's first real-fixture commit will populate
        // [[fixture]] and this test will run real container-shape detection.
        println!("fixture_parse: no fixtures listed (PR 4a empty-set state)");
        return;
    }
    let root = fixtures_dir();
    for entry in &manifest.fixture {
        let path = root.join(&entry.path);
        if !path.exists() {
            // Reported by fixture_manifest_listed; skip here.
            continue;
        }
        // Skip non-JSON fixtures — dbt manifests are JSON by definition;
        // non-`.json` fixtures (e.g. the PR 14 `config-*.toml` fixtures
        // for the `--config <PATH>` value-parser scenarios) are
        // intentionally not dbt manifests and have their own dedicated
        // assertions in `tests/adapters/config_reader.rs` /
        // `features/config.feature`.
        if !entry.path.ends_with(".json") {
            println!(
                "fixture_parse: {} is non-JSON (manifest-shape assertions do not apply)",
                entry.path
            );
            continue;
        }
        // JSON, but a deliberately non-manifest wire (e.g. the #395 gh
        // GraphQL response) — skip the manifest-shape assertions; the
        // fixture has its own dedicated parse coverage.
        if NON_MANIFEST_JSON_FIXTURES.contains(&entry.path.as_str()) {
            println!(
                "fixture_parse: {} is non-manifest JSON (manifest-shape assertions do not apply)",
                entry.path
            );
            continue;
        }
        let bytes = fs::read_to_string(&path).expect("read listed fixture");
        let value: Value = serde_json::from_str(&bytes)
            .unwrap_or_else(|e| panic!("fixture {} must parse as JSON: {e}", entry.path));

        assert!(
            value.get("metadata").is_some(),
            "fixture {} missing top-level `metadata` (dbt manifest invariant)",
            entry.path,
        );
        assert!(
            value.get("nodes").is_some(),
            "fixture {} missing top-level `nodes` (dbt manifest invariant)",
            entry.path,
        );

        let shape = detect_container_shape(&value);
        println!("fixture_parse: {} container shape = {shape:?}", entry.path);
        assert_eq!(
            shape,
            ContainerShape::TopLevelMap,
            "fixture {} must be container shape A (top-level `unit_tests` map) — \
             `adapters::manifest` (PR 4b, #6) only ingests shape A",
            entry.path,
        );
    }
}

// Sentinel test — exercises the container-shape detection logic against
// in-memory inputs so the mechanical surface has coverage from PR 4a even
// when no real fixture is committed. PR 4b grows real coverage; this stays.
#[test]
fn container_shape_detection_handles_each_arm() {
    let none = serde_json::json!({"metadata": {}, "nodes": {}});
    assert_eq!(detect_container_shape(&none), ContainerShape::None);

    let a = serde_json::json!({
        "metadata": {},
        "nodes": {},
        "unit_tests": {"u1": {"name": "u1"}},
    });
    assert_eq!(detect_container_shape(&a), ContainerShape::TopLevelMap);

    let b = serde_json::json!({
        "metadata": {},
        "nodes": {
            "unit_test.proj.t1": {"resource_type": "unit_test", "name": "t1"},
        },
    });
    assert_eq!(detect_container_shape(&b), ContainerShape::EmbeddedInNodes);

    let c = serde_json::json!({
        "metadata": {},
        "nodes": {
            "unit_test.proj.t1": {"resource_type": "unit_test"},
        },
        "unit_tests": {"u1": {"name": "u1"}},
    });
    assert_eq!(detect_container_shape(&c), ContainerShape::Both);
}
