//! Handoff §2.2 display-mapping coverage over the adopted demo schema
//! YAML (cute-dbt#179).
//!
//! `tests/fixtures/healthcare-analytics-schema.yml` is the report-redesign
//! handoff's worked fixture (`demo/healthcare_analytics.yml`, adopted
//! verbatim — healthcare-SHAPED pure synthetic data). It carries the two
//! §2.2 arms the real fusion-compiled playground manifest does NOT
//! exercise (`tests/render_integration.rs` covers the real-fixture arms):
//!
//! - `dbt_utils.accepted_range` in both bound forms — min-only (`≥ 0`)
//!   and both-bounds (`0–1` / `0–100`, en dash);
//! - boolean `accepted_values` pills (`[true, false]`).
//!
//! The test extracts every authored `data_tests` entry from the YAML
//! (a fixture-shaped line walker — cute-dbt has no YAML dependency by
//! design; the schema-YAML path was the handoff's fallback, cute-dbt
//! ships the manifest-first path), folds each into the same
//! [`TestMetadata`] shape the manifest reader produces, and asserts the
//! [`column_test_payload`] §2.2 display mapping over all 47 of them. A
//! fixture drift (count, bounds, pill list) fails here, keeping the
//! adopted file load-bearing rather than decorative.

use cute_dbt::adapters::render::column_test_payload;
use cute_dbt::domain::TestMetadata;
use serde_json::{Map, Value};

const SCHEMA_YAML: &str = include_str!("fixtures/healthcare-analytics-schema.yml");

/// One authored column test extracted from the schema YAML.
struct AuthoredTest {
    column: String,
    metadata: TestMetadata,
}

/// Parse a YAML scalar token as authored: single-quoted → string,
/// `true`/`false` → bool, integer → number, anything else verbatim.
fn scalar(token: &str) -> Value {
    let t = token.trim();
    if let Some(stripped) = t.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return Value::String(stripped.to_owned());
    }
    match t {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => t
            .parse::<i64>()
            .map_or_else(|_| Value::String(t.to_owned()), Value::from),
    }
}

/// Parse a YAML flow sequence (`['a', 'b']` / `[true, false]`) — the only
/// list shape the fixture authors. No embedded commas in this fixture.
fn flow_seq(raw: &str) -> Value {
    let inner = raw
        .trim()
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or_else(|| panic!("expected a flow sequence, got {raw:?}"));
    Value::Array(inner.split(',').map(scalar).collect())
}

/// Split a possibly package-qualified test identifier into
/// `(namespace, bare name)` — `dbt_utils.accepted_range` →
/// `(Some("dbt_utils"), "accepted_range")`, matching how the manifest's
/// `test_metadata` carries them.
fn split_qualified(ident: &str) -> (Option<String>, String) {
    match ident.split_once('.') {
        Some((ns, name)) => (Some(ns.to_owned()), name.to_owned()),
        None => (None, ident.to_owned()),
    }
}

/// Extract every authored `data_tests` entry as `(column, TestMetadata)`.
///
/// Fixture-shaped on purpose: model entries at `  - name:`, column
/// entries at `      - name:`, test entries at `          - `, and
/// nested test args at 14 spaces — the indentation the committed file
/// uses throughout.
fn extract_authored_tests() -> Vec<AuthoredTest> {
    const COLUMN: &str = "      - name: ";
    const DATA_TESTS: &str = "        data_tests:";
    const ENTRY: &str = "          - ";
    const ARG: &str = "              ";

    let lines: Vec<&str> = SCHEMA_YAML.lines().collect();
    let mut column: Option<String> = None;
    let mut in_data_tests = false;
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(name) = line.strip_prefix(COLUMN) {
            column = Some(name.trim().to_owned());
            in_data_tests = false;
        } else if line.starts_with(DATA_TESTS) {
            in_data_tests = true;
        } else if in_data_tests && line.starts_with(ENTRY) {
            let entry = line[ENTRY.len()..].trim();
            let col = column.clone().expect("a data_tests entry follows a column");
            if let Some(ident) = entry.strip_suffix(':') {
                // Arg-carrying test: fold the indented `key: value` block
                // into kwargs exactly as the manifest carries them.
                let mut kwargs = Map::new();
                while i + 1 < lines.len() && lines[i + 1].starts_with(ARG) {
                    i += 1;
                    let (key, value) = lines[i]
                        .trim()
                        .split_once(": ")
                        .unwrap_or_else(|| panic!("malformed arg line {:?}", lines[i]));
                    let parsed = if value.trim_start().starts_with('[') {
                        flow_seq(value)
                    } else {
                        scalar(value)
                    };
                    kwargs.insert(key.to_owned(), parsed);
                }
                let (namespace, name) = split_qualified(ident);
                out.push(AuthoredTest {
                    column: col,
                    metadata: TestMetadata::new(name, namespace, Value::Object(kwargs)),
                });
            } else {
                let (namespace, name) = split_qualified(entry);
                out.push(AuthoredTest {
                    column: col,
                    metadata: TestMetadata::new(name, namespace, Value::Null),
                });
            }
        }
        i += 1;
    }
    out
}

/// The mapped payload for the (unique-in-fixture) `(column, test name)`
/// pair, panicking if absent — the fixture-drift guard.
fn payload_for(
    tests: &[AuthoredTest],
    column: &str,
    bare_name: &str,
) -> cute_dbt::adapters::render::ColumnTestPayload {
    let t = tests
        .iter()
        .find(|t| t.column == column && t.metadata.name() == bare_name)
        .unwrap_or_else(|| panic!("fixture authors a {bare_name} test on column {column}"));
    column_test_payload(&t.metadata)
}

#[test]
fn the_fixture_carries_the_authored_test_inventory() {
    // Drift guard: the adopted file's authored inventory is stable. If
    // someone edits the fixture, this fails first and points at the
    // MANIFEST.toml sha256 + the assertions below to re-verify.
    let tests = extract_authored_tests();
    assert_eq!(tests.len(), 47, "47 authored data_tests entries");
    let count = |name: &str| tests.iter().filter(|t| t.metadata.name() == name).count();
    assert_eq!(count("accepted_range"), 21);
    assert_eq!(count("not_null"), 13);
    assert_eq!(count("accepted_values"), 9);
    assert_eq!(count("unique"), 2);
    assert_eq!(count("relationships"), 2);
    // Every accepted_range in the fixture is package-qualified the way
    // it really ships (`dbt_utils.accepted_range`).
    assert!(
        tests
            .iter()
            .filter(|t| t.metadata.name() == "accepted_range")
            .all(|t| t.metadata.namespace() == Some("dbt_utils")),
        "accepted_range is authored package-qualified",
    );
}

#[test]
fn accepted_range_bounds_format_per_the_handoff_table() {
    // §2.2: `accepted_range (min/max)` → `accepted range` with detail
    // `"0–100"` (both bounds, en dash) / `"≥ 0"` (min only) — the arm NO
    // real committed manifest fixture exercises (the playground project
    // uses dbt_expectations range tests instead).
    let tests = extract_authored_tests();
    let ranges: Vec<_> = tests
        .iter()
        .filter(|t| t.metadata.name() == "accepted_range")
        .map(|t| (t.column.as_str(), column_test_payload(&t.metadata)))
        .collect();
    for (column, payload) in &ranges {
        assert_eq!(payload.name, "accepted range", "prose name on {column}");
        assert!(payload.values.is_empty(), "no pills on {column}");
        assert!(payload.detail.is_some(), "a range detail on {column}");
    }
    // Both bound forms are present and format exactly per the table.
    assert_eq!(
        payload_for(&tests, "average_quality_of_life_score", "accepted_range")
            .detail
            .as_deref(),
        Some("0\u{2013}1"),
        "both bounds join with an en dash (0–1)",
    );
    assert_eq!(
        payload_for(&tests, "quarantine_rate_pct", "accepted_range")
            .detail
            .as_deref(),
        Some("0\u{2013}100"),
        "both bounds join with an en dash (0–100)",
    );
    assert_eq!(
        payload_for(&tests, "amount_covered", "accepted_range")
            .detail
            .as_deref(),
        Some("\u{2265} 0"),
        "min-only renders as ≥ min (the handoff §2.2 sample JSON form)",
    );
    let min_only = ranges
        .iter()
        .filter(|(_, p)| p.detail.as_deref() == Some("\u{2265} 0"))
        .count();
    assert_eq!(
        min_only, 19,
        "the fixture's 19 min-only ranges all map to ≥ 0"
    );
}

#[test]
fn accepted_values_render_as_pills_including_booleans() {
    // §2.2: `accepted_values {values: […]}` → `accepted values` + one
    // pill per authored value. The fixture adds the BOOLEAN pill arm
    // ([true, false] — scalars render via their JSON form) on top of the
    // string lists the real manifest fixture already covers.
    let tests = extract_authored_tests();
    let encounter_type = payload_for(&tests, "encounter_type", "accepted_values");
    assert_eq!(encounter_type.name, "accepted values");
    assert_eq!(
        encounter_type.values,
        vec![
            "ambulatory",
            "emergency",
            "inpatient",
            "outpatient",
            "wellness",
            "urgentcare"
        ],
        "the handoff README's worked example list, one pill per value",
    );
    assert!(encounter_type.detail.is_none());

    let is_dq_valid = payload_for(&tests, "is_dq_valid", "accepted_values");
    assert_eq!(
        is_dq_valid.values,
        vec!["true", "false"],
        "boolean pills render their JSON form, never prose-mangled",
    );
    let boolean_pills = tests
        .iter()
        .filter(|t| t.metadata.name() == "accepted_values")
        .map(|t| column_test_payload(&t.metadata))
        .filter(|p| p.values == ["true", "false"])
        .count();
    assert_eq!(
        boolean_pills, 8,
        "all 8 boolean flag columns map to true/false pills"
    );
}

#[test]
fn relationships_and_builtins_map_to_prose_names() {
    // §2.2: `relationships {to: ref('m'), field: f}` → detail "m.f";
    // unique / not_null carry bare prose names with no args.
    let tests = extract_authored_tests();
    assert_eq!(
        payload_for(&tests, "patient_id", "relationships")
            .detail
            .as_deref(),
        Some("stg_synthea__patients.patient_id"),
        "the handoff §2.2 sample JSON's relationships detail",
    );
    assert_eq!(
        payload_for(&tests, "provider_id", "relationships")
            .detail
            .as_deref(),
        Some("stg_synthea__providers.provider_id"),
    );
    let unique = payload_for(&tests, "payer_key", "unique");
    assert_eq!(unique.name, "unique");
    assert!(unique.values.is_empty() && unique.detail.is_none());
    let not_null = payload_for(&tests, "encounter_id", "not_null");
    assert_eq!(not_null.name, "not null");
    assert!(not_null.values.is_empty() && not_null.detail.is_none());
}
