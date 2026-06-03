//! `UnitTest` + `UnitTestGiven` + `UnitTestExpect` — dbt unit-test
//! fixtures projected into the domain.
//!
//! Field set is the v0.1 consumption subset per ADR-5 ("tolerant
//! deserialization"). The `rows` / `format` fields are stored as
//! `serde_json::Value` so we accept the dbt shape verbatim (dbt accepts
//! both `format: dict` row maps and `format: csv` text blobs); the
//! renderer (PR 8b) and any future format-aware logic decide what to do
//! with each.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::manifest::{DependsOn, NodeId};

/// A single `given` block on a dbt unit test.
///
/// `input` is the upstream model reference (e.g. `ref('stg_orders')`),
/// `rows` is the fixture data (typed `Value` per ADR-5 tolerance),
/// `format` is the dbt `format` field (e.g. `"dict"`, `"csv"`,
/// `"sql"`) — `None` when the manifest omits it (dbt defaults to
/// `dict`) — and `fixture` is the name of an external fixture CSV the
/// rows live in (`None` for an inline-rows given). When `fixture` is
/// `Some` and `rows` is `Value::Null`, the fixture data is **not** in the
/// manifest at all (a reference-only external fixture) — the render layer
/// surfaces an affordance and falls back to the YAML text view rather
/// than a silently-empty grid (cute-dbt#98, #126).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitTestGiven {
    input: String,
    /// `#[serde(default)]` so an external-fixture given (which carries
    /// `fixture` instead of inline `rows`) still deserializes, defaulting
    /// to `Value::Null` (ADR-5 tolerance).
    #[serde(default)]
    rows: Value,
    #[serde(default)]
    format: Option<String>,
    /// Name of the external fixture file this given's rows live in (dbt's
    /// `fixture:` key). `None` for inline-rows givens.
    #[serde(default)]
    fixture: Option<String>,
}

impl UnitTestGiven {
    /// Canonical constructor.
    #[must_use]
    pub fn new(
        input: impl Into<String>,
        rows: Value,
        format: Option<String>,
        fixture: Option<String>,
    ) -> Self {
        Self {
            input: input.into(),
            rows,
            format,
            fixture,
        }
    }

    /// Upstream model reference text (e.g. `ref('stg_orders')`).
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Fixture rows (verbatim JSON per ADR-5 tolerance).
    #[must_use]
    pub fn rows(&self) -> &Value {
        &self.rows
    }

    /// dbt `format` field — `None` if the manifest omitted it.
    #[must_use]
    pub fn format(&self) -> Option<&str> {
        self.format.as_deref()
    }

    /// External fixture file name (dbt's `fixture:` key) — `None` for an
    /// inline-rows given.
    #[must_use]
    pub fn fixture(&self) -> Option<&str> {
        self.fixture.as_deref()
    }
}

/// The `expect` block on a dbt unit test — same `rows` / `format` /
/// `fixture` shape as a `given` minus the `input` reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitTestExpect {
    /// `#[serde(default)]` so an external-fixture `expect` (which carries
    /// `fixture` instead of inline `rows`) still deserializes, defaulting
    /// to `Value::Null` (ADR-5 tolerance).
    #[serde(default)]
    rows: Value,
    #[serde(default)]
    format: Option<String>,
    /// Name of the external fixture file this `expect`'s rows live in
    /// (dbt's `fixture:` key). `None` for inline-rows expects.
    #[serde(default)]
    fixture: Option<String>,
}

impl UnitTestExpect {
    /// Canonical constructor.
    #[must_use]
    pub fn new(rows: Value, format: Option<String>, fixture: Option<String>) -> Self {
        Self {
            rows,
            format,
            fixture,
        }
    }

    /// Expected rows (verbatim JSON per ADR-5 tolerance).
    #[must_use]
    pub fn rows(&self) -> &Value {
        &self.rows
    }

    /// dbt `format` field — `None` if the manifest omitted it.
    #[must_use]
    pub fn format(&self) -> Option<&str> {
        self.format.as_deref()
    }

    /// External fixture file name (dbt's `fixture:` key) — `None` for an
    /// inline-rows expect.
    #[must_use]
    pub fn fixture(&self) -> Option<&str> {
        self.fixture.as_deref()
    }
}

/// A dbt unit test — the central artifact the report visualizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitTest {
    name: String,
    model: NodeId,
    #[serde(default)]
    given: Vec<UnitTestGiven>,
    expect: UnitTestExpect,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    depends_on: DependsOn,
    /// dbt `config.tags` for this unit test (`None` when omitted by the
    /// manifest). Populated by the adapter from the nested `config` block;
    /// stored flat here per ADR-5 tolerant shape.
    #[serde(default)]
    tags: Option<Vec<String>>,
    /// dbt `config.meta` for this unit test — arbitrary key/value map
    /// (`None` when omitted). Stored as `serde_json::Value` (passthrough)
    /// per ADR-5: the renderer decides how to surface individual keys.
    #[serde(default)]
    meta: Option<Value>,
    /// Path to the `.yml` file that declares this unit test, relative to the
    /// dbt project root. Top-level field on the unit-test node in the wire
    /// manifest (NOT under `config`). `None` when the manifest omits it.
    #[serde(default)]
    original_file_path: Option<String>,
}

impl UnitTest {
    /// Canonical constructor — every field is owned and explicit.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        model: NodeId,
        given: Vec<UnitTestGiven>,
        expect: UnitTestExpect,
        description: Option<String>,
        depends_on: DependsOn,
        tags: Option<Vec<String>>,
        meta: Option<Value>,
        original_file_path: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            model,
            given,
            expect,
            description,
            depends_on,
            tags,
            meta,
            original_file_path,
        }
    }

    /// Unit-test name (e.g. `"test_stg_orders_dedup"`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `model:` field — the unit test's target model node id.
    #[must_use]
    pub fn model(&self) -> &NodeId {
        &self.model
    }

    /// Ordered `given` blocks (input fixtures).
    #[must_use]
    pub fn given(&self) -> &[UnitTestGiven] {
        &self.given
    }

    /// `expect` block (assertion fixture).
    #[must_use]
    pub fn expect(&self) -> &UnitTestExpect {
        &self.expect
    }

    /// Free-text description (`None` when the YAML omits it).
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Manifest-declared forward dependency edges.
    #[must_use]
    pub fn depends_on(&self) -> &DependsOn {
        &self.depends_on
    }

    /// dbt `config.tags` for this unit test (`None` when absent in the
    /// manifest).
    #[must_use]
    pub fn tags(&self) -> Option<&[String]> {
        self.tags.as_deref()
    }

    /// dbt `config.meta` for this unit test (`None` when absent in the
    /// manifest). The value is passthrough JSON per ADR-5 tolerance.
    #[must_use]
    pub fn meta(&self) -> Option<&Value> {
        self.meta.as_ref()
    }

    /// Path to the `.yml` file that declares this unit test (`None` when
    /// the manifest omits `original_file_path`).
    #[must_use]
    pub fn original_file_path(&self) -> Option<&str> {
        self.original_file_path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_given() -> UnitTestGiven {
        UnitTestGiven::new(
            "ref('stg_orders')",
            json!([{"order_id": 1}, {"order_id": 2}]),
            Some("dict".to_owned()),
            None,
        )
    }

    fn sample_expect() -> UnitTestExpect {
        UnitTestExpect::new(json!([{"order_id": 1}]), Some("dict".to_owned()), None)
    }

    #[test]
    fn given_constructor_and_getters() {
        let g = sample_given();
        assert_eq!(g.input(), "ref('stg_orders')");
        assert!(g.rows().is_array());
        assert_eq!(g.format(), Some("dict"));
        assert_eq!(g.fixture(), None, "inline-rows given has no fixture");
    }

    #[test]
    fn given_format_optional_defaults_none_on_wire() {
        let json = r#"{ "input": "ref('a')", "rows": [] }"#;
        let g: UnitTestGiven = serde_json::from_str(json).unwrap();
        assert!(g.format().is_none());
        assert!(g.rows().is_array());
    }

    #[test]
    fn given_fixture_field_deserializes_from_wire() {
        // dbt emits a top-level `fixture: <name>` (sibling of rows/format).
        let json = r#"{ "input": "ref('a')", "rows": null, "format": "csv", "fixture": "stg_orders_fixture" }"#;
        let g: UnitTestGiven = serde_json::from_str(json).unwrap();
        assert_eq!(g.fixture(), Some("stg_orders_fixture"));
        assert!(
            g.rows().is_null(),
            "external-fixture given carries null rows; data is in the file"
        );
    }

    #[test]
    fn given_with_absent_rows_defaults_to_null() {
        // A reference-only external given may omit `rows` entirely — the
        // #[serde(default)] keeps deserialization from failing.
        let json = r#"{ "input": "ref('a')", "fixture": "ext" }"#;
        let g: UnitTestGiven = serde_json::from_str(json).unwrap();
        assert!(g.rows().is_null(), "absent rows default to Value::Null");
        assert_eq!(g.fixture(), Some("ext"));
    }

    #[test]
    fn expect_fixture_field_deserializes_from_wire() {
        let json = r#"{ "rows": null, "format": "csv", "fixture": "expected_fixture" }"#;
        let e: UnitTestExpect = serde_json::from_str(json).unwrap();
        assert_eq!(e.fixture(), Some("expected_fixture"));
        assert!(e.rows().is_null());
    }

    #[test]
    fn given_serde_roundtrip() {
        let g = sample_given();
        let back: UnitTestGiven =
            serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn given_serde_roundtrip_over_fixture_x_rows_matrix() {
        // CodeRabbit PR #130: the new `fixture`/`rows` wire shapes add
        // optional/nested permutations that are easy to regress. Exhaust the
        // {fixture present/absent} × {rows null/inline-array/inline-object}
        // matrix and assert serialize→deserialize is the identity, pinning
        // the tolerant-deserialization contract across every combination.
        let fixtures = [None, Some("ext_fixture".to_owned())];
        let row_shapes = [
            Value::Null,
            json!([{"order_id": 1}, {"order_id": 2}]),
            json!({"order_id": 1}),
        ];
        let formats = [None, Some("dict".to_owned()), Some("csv".to_owned())];
        for fixture in &fixtures {
            for rows in &row_shapes {
                for format in &formats {
                    let g = UnitTestGiven::new(
                        "ref('a')",
                        rows.clone(),
                        format.clone(),
                        fixture.clone(),
                    );
                    let back: UnitTestGiven =
                        serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
                    assert_eq!(back, g, "given round-trip failed for {g:?}");

                    let e = UnitTestExpect::new(rows.clone(), format.clone(), fixture.clone());
                    let back: UnitTestExpect =
                        serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
                    assert_eq!(back, e, "expect round-trip failed for {e:?}");
                }
            }
        }
    }

    #[test]
    fn given_rows_omitted_on_wire_roundtrips_through_null() {
        // The omitted-vs-null tolerance: a wire payload that omits `rows`
        // entirely deserializes to `Value::Null`, and re-serializing then
        // re-deserializing is stable (the omitted key normalizes to null).
        let wire = r#"{ "input": "ref('a')", "fixture": "ext" }"#;
        let g: UnitTestGiven = serde_json::from_str(wire).unwrap();
        assert!(g.rows().is_null());
        let back: UnitTestGiven =
            serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn expect_constructor_and_getters() {
        let e = sample_expect();
        assert!(e.rows().is_array());
        assert_eq!(e.format(), Some("dict"));
    }

    #[test]
    fn expect_format_optional_defaults_none_on_wire() {
        let json = r#"{ "rows": [] }"#;
        let e: UnitTestExpect = serde_json::from_str(json).unwrap();
        assert!(e.format().is_none());
    }

    #[test]
    fn expect_serde_roundtrip() {
        let e = sample_expect();
        let back: UnitTestExpect =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn unit_test_constructor_and_getters() {
        let model = NodeId::new("model.shop.stg_orders");
        let deps = DependsOn::new(
            vec!["macro.shop.helper".to_owned()],
            vec![NodeId::new("seed.shop.raw_orders")],
        );
        let ut = UnitTest::new(
            "test_stg_orders_dedup",
            model.clone(),
            vec![sample_given()],
            sample_expect(),
            Some("dedup test".to_owned()),
            deps.clone(),
            None,
            None,
            None,
        );
        assert_eq!(ut.name(), "test_stg_orders_dedup");
        assert_eq!(ut.model(), &model);
        assert_eq!(ut.given().len(), 1);
        assert_eq!(ut.expect(), &sample_expect());
        assert_eq!(ut.description(), Some("dedup test"));
        assert_eq!(ut.depends_on(), &deps);
        assert_ne!(
            ut.depends_on(),
            &DependsOn::default(),
            "getter must return the actual DependsOn, not a manufactured default"
        );
    }

    #[test]
    fn unit_test_tolerates_missing_optionals() {
        let json = r#"{
            "name": "t",
            "model": "model.shop.stg_orders",
            "expect": { "rows": [] }
        }"#;
        let ut: UnitTest = serde_json::from_str(json).unwrap();
        assert_eq!(ut.name(), "t");
        assert!(ut.given().is_empty());
        assert!(ut.description().is_none());
        assert!(ut.expect().rows().is_array());
        assert!(ut.tags().is_none(), "tags should default to None");
        assert!(ut.meta().is_none(), "meta should default to None");
        assert!(
            ut.original_file_path().is_none(),
            "original_file_path should default to None"
        );
    }

    #[test]
    fn unit_test_serde_roundtrip_preserves_all_fields() {
        let ut = UnitTest::new(
            "t",
            NodeId::new("model.shop.stg_orders"),
            vec![sample_given()],
            sample_expect(),
            Some("desc".to_owned()),
            DependsOn::new(
                vec!["macro.shop.foo".to_owned()],
                vec![NodeId::new("model.shop.upstream")],
            ),
            Some(vec!["quality".to_owned(), "smoke".to_owned()]),
            Some(json!({"owner": "data-eng", "priority": 1})),
            Some("models/staging/unit_tests.yml".to_owned()),
        );
        let back: UnitTest = serde_json::from_str(&serde_json::to_string(&ut).unwrap()).unwrap();
        assert_eq!(back, ut);
    }

    #[test]
    fn unit_test_metadata_getters_return_populated_values() {
        let ut = UnitTest::new(
            "tagged_test",
            NodeId::new("model.shop.stg_orders"),
            vec![],
            sample_expect(),
            None,
            DependsOn::default(),
            Some(vec!["quality".to_owned(), "smoke".to_owned()]),
            Some(json!({"owner": "data-eng"})),
            Some("models/staging/unit_tests.yml".to_owned()),
        );
        let expected_tags: Vec<String> = vec!["quality".to_owned(), "smoke".to_owned()];
        assert_eq!(
            ut.tags(),
            Some(expected_tags.as_slice()),
            "tags getter must return the vec as a slice"
        );
        let meta = ut.meta().expect("meta should be Some");
        assert_eq!(meta["owner"], json!("data-eng"));
        assert_eq!(
            ut.original_file_path(),
            Some("models/staging/unit_tests.yml")
        );
    }
}
