//! `Manifest` + `Node` + `NodeId` + `Checksum` + `DependsOn` — the parsed
//! projection of a dbt `manifest.json` that the run loop consumes.
//!
//! POD-only owned data per ADR-1 (single-crate hexagonal,
//! `domain → ports → adapters → cli`). Constructors are the canonical
//! entry points so additive fields stay mechanical — adding a field
//! touches the constructor, not every call site.
//!
//! ## Container-shape contract for PR 4b
//!
//! The dbt manifest schema v12 permits unit tests to appear either as a
//! top-level `unit_tests` map keyed by `unit_test.<package>.<name>` **or**
//! embedded in `nodes` with `resource_type == "unit_test"`. ADR-5
//! ("tolerant deserialization") commits to resolving the container shape
//! against the real fixture from PR 4a — **not** by branching the public
//! domain shape.
//!
//! This module defines the **post-normalized** domain shape:
//! `Manifest { metadata, nodes, unit_tests, macros }` where `unit_tests`
//! is a separate map regardless of how the wire format laid them out.
//! PR 4b's `adapters::manifest` owns the wire→domain translation
//! (top-level passes through; embedded-in-`nodes` partitions on
//! `resource_type` during deserialization). That keeps every
//! manifest-format quirk inside one adapter file.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Stable identifier for a dbt node (model, seed, snapshot, source, test,
/// unit test, macro, …). Newtype over `String` so the adapter and
/// detector layers cannot accidentally swap a node id with arbitrary
/// strings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(String);

impl NodeId {
    /// Construct a `NodeId` from any string-like value.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the underlying id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for NodeId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for NodeId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// dbt's `checksum` block — `{ name: "sha256", checksum: "<hex>" }`.
/// Compared verbatim by `BodyChecksumModifier` (PR 5, ADR-3).
///
/// Field names mirror dbt's `FileHash` shape (`dbt-core` source
/// `core/dbt/contracts/graph/nodes.py`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Checksum {
    name: String,
    checksum: String,
}

impl Checksum {
    /// Canonical constructor.
    #[must_use]
    pub fn new(name: impl Into<String>, checksum: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            checksum: checksum.into(),
        }
    }

    /// Hash algorithm name (e.g. `"sha256"`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Hex-encoded hash bytes.
    #[must_use]
    pub fn checksum(&self) -> &str {
        &self.checksum
    }
}

/// dbt's `depends_on` block — both macros and node refs are simple
/// string lists in the manifest, but we newtype the node side so
/// downstream code does not have to remember which list is which.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DependsOn {
    #[serde(default)]
    macros: Vec<String>,
    #[serde(default)]
    nodes: Vec<NodeId>,
}

impl DependsOn {
    /// Canonical constructor.
    #[must_use]
    pub fn new(macros: Vec<String>, nodes: Vec<NodeId>) -> Self {
        Self { macros, nodes }
    }

    /// Macros this node depends on (unique fully-qualified macro ids).
    #[must_use]
    pub fn macros(&self) -> &[String] {
        &self.macros
    }

    /// Nodes this node depends on.
    #[must_use]
    pub fn nodes(&self) -> &[NodeId] {
        &self.nodes
    }
}

/// A dbt node (model / seed / snapshot / source / test / `unit_test`).
///
/// Field set is the v0.1 consumption subset — see ADR-5 ("tolerant
/// deserialization: model only the fields cute-dbt consumes"). Adding
/// fields is mechanically additive via [`Node::new`].
///
/// `compiled_code` is `Option<String>` so a parse-only manifest can
/// deserialize without an early error; the Stage-2 fail-closed check
/// (PR 6, `domain/preflight.rs`) inspects this field **after** scope
/// selects the in-scope set, raising
/// [`PreflightError::NotCompiled`](crate::domain::preflight::PreflightError::NotCompiled)
/// only for in-scope unit-test targets that have `compiled_code: null`.
///
/// `raw_code` is the model's Jinja source (pre-compile) — populated by
/// dbt 1.8+ on every node. The renderer (cute-dbt#47) surfaces it in
/// the per-model Model SQL section. `None` is tolerated so older
/// manifests still deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    id: NodeId,
    resource_type: String,
    checksum: Checksum,
    #[serde(default)]
    compiled_code: Option<String>,
    #[serde(default)]
    raw_code: Option<String>,
    #[serde(default)]
    depends_on: DependsOn,
}

impl Node {
    /// Canonical constructor — every field is owned and explicit.
    #[must_use]
    pub fn new(
        id: NodeId,
        resource_type: impl Into<String>,
        checksum: Checksum,
        compiled_code: Option<String>,
        raw_code: Option<String>,
        depends_on: DependsOn,
    ) -> Self {
        Self {
            id,
            resource_type: resource_type.into(),
            checksum,
            compiled_code,
            raw_code,
            depends_on,
        }
    }

    /// Node id (unique within a manifest).
    #[must_use]
    pub fn id(&self) -> &NodeId {
        &self.id
    }

    /// `resource_type` discriminator (e.g. `"model"`, `"unit_test"`).
    #[must_use]
    pub fn resource_type(&self) -> &str {
        &self.resource_type
    }

    /// Body checksum used by `BodyChecksumModifier` (PR 5).
    #[must_use]
    pub fn checksum(&self) -> &Checksum {
        &self.checksum
    }

    /// Compiled SQL body, if `dbt compile` (or `dbt run`) produced one.
    /// `None` when the manifest was produced by `dbt parse`.
    #[must_use]
    pub fn compiled_code(&self) -> Option<&str> {
        self.compiled_code.as_deref()
    }

    /// Raw Jinja source of the model file (`models/**/*.sql`), if the
    /// manifest carries one. dbt 1.8+ populates this on every node.
    #[must_use]
    pub fn raw_code(&self) -> Option<&str> {
        self.raw_code.as_deref()
    }

    /// Forward dependency edges declared in the manifest.
    #[must_use]
    pub fn depends_on(&self) -> &DependsOn {
        &self.depends_on
    }
}

/// `metadata` block — currently consumed only for the
/// `dbt_schema_version` floor check (ADR-2 Stage-1, PR 4b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestMetadata {
    /// `metadata.dbt_schema_version` URL/string. Read verbatim — the
    /// adapter (PR 4b) is responsible for parsing the embedded version
    /// number for the floor comparison.
    dbt_schema_version: String,
}

impl ManifestMetadata {
    /// Canonical constructor.
    #[must_use]
    pub fn new(dbt_schema_version: impl Into<String>) -> Self {
        Self {
            dbt_schema_version: dbt_schema_version.into(),
        }
    }

    /// `dbt_schema_version` value (verbatim from the manifest).
    #[must_use]
    pub fn dbt_schema_version(&self) -> &str {
        &self.dbt_schema_version
    }
}

/// Parsed dbt `manifest.json` projection.
///
/// **Post-normalized shape** (see module docs) — `unit_tests` is a
/// separate map regardless of whether the wire format laid them out at
/// top level or embedded under `nodes`. PR 4b owns the wire→domain
/// translation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    metadata: ManifestMetadata,
    #[serde(default)]
    nodes: HashMap<NodeId, Node>,
    #[serde(default)]
    unit_tests: HashMap<String, crate::domain::unit_test::UnitTest>,
    #[serde(default)]
    macros: HashMap<String, String>,
}

impl Manifest {
    /// Canonical constructor — every field is owned.
    #[must_use]
    pub fn new(
        metadata: ManifestMetadata,
        nodes: HashMap<NodeId, Node>,
        unit_tests: HashMap<String, crate::domain::unit_test::UnitTest>,
        macros: HashMap<String, String>,
    ) -> Self {
        Self {
            metadata,
            nodes,
            unit_tests,
            macros,
        }
    }

    /// `metadata` block.
    #[must_use]
    pub fn metadata(&self) -> &ManifestMetadata {
        &self.metadata
    }

    /// All nodes keyed by id (models, sources, tests, …).
    #[must_use]
    pub fn nodes(&self) -> &HashMap<NodeId, Node> {
        &self.nodes
    }

    /// Look up a node by id.
    #[must_use]
    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// All unit tests keyed by their manifest id (e.g.
    /// `unit_test.jaffle_shop.test_stg_orders_dedup`).
    #[must_use]
    pub fn unit_tests(&self) -> &HashMap<String, crate::domain::unit_test::UnitTest> {
        &self.unit_tests
    }

    /// Look up a unit test by manifest id.
    #[must_use]
    pub fn unit_test(&self, id: &str) -> Option<&crate::domain::unit_test::UnitTest> {
        self.unit_tests.get(id)
    }

    /// All macros keyed by manifest id (body string).
    #[must_use]
    pub fn macros(&self) -> &HashMap<String, String> {
        &self.macros
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::unit_test::{UnitTest, UnitTestExpect, UnitTestGiven};

    fn sample_checksum() -> Checksum {
        Checksum::new("sha256", "deadbeef")
    }

    #[test]
    fn node_id_new_and_as_str_roundtrip() {
        let id = NodeId::new("model.shop.stg_orders");
        assert_eq!(id.as_str(), "model.shop.stg_orders");
        assert_eq!(id.to_string(), "model.shop.stg_orders");
    }

    #[test]
    fn node_id_from_string_and_str_are_equivalent() {
        let from_owned = NodeId::from(String::from("model.x"));
        let from_borrowed = NodeId::from("model.x");
        assert_eq!(from_owned, from_borrowed);
    }

    #[test]
    fn node_id_serde_is_transparent_string() {
        let id = NodeId::new("model.shop.stg_orders");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"model.shop.stg_orders\"");
        let back: NodeId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn checksum_constructor_and_getters() {
        let c = Checksum::new("sha256", "abc123");
        assert_eq!(c.name(), "sha256");
        assert_eq!(c.checksum(), "abc123");
    }

    #[test]
    fn checksum_serde_roundtrip() {
        let c = Checksum::new("sha256", "abc123");
        let json = serde_json::to_string(&c).unwrap();
        // Field order matters for the wire — name first, checksum second.
        assert!(json.contains("\"name\":\"sha256\""));
        assert!(json.contains("\"checksum\":\"abc123\""));
        let back: Checksum = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn depends_on_default_is_empty() {
        let d = DependsOn::default();
        assert!(d.macros().is_empty());
        assert!(d.nodes().is_empty());
    }

    #[test]
    fn depends_on_new_and_getters() {
        let d = DependsOn::new(
            vec!["macro.shop.foo".into()],
            vec![NodeId::new("model.shop.stg_orders")],
        );
        assert_eq!(d.macros(), &["macro.shop.foo".to_owned()]);
        assert_eq!(d.nodes(), &[NodeId::new("model.shop.stg_orders")]);
    }

    #[test]
    fn depends_on_serde_roundtrip_with_missing_fields() {
        // Tolerant deserialization (ADR-5) — missing fields default to
        // empty vec, never error.
        let json = "{}";
        let d: DependsOn = serde_json::from_str(json).unwrap();
        assert_eq!(d, DependsOn::default());
    }

    #[test]
    fn node_constructor_and_getters() {
        let id = NodeId::new("model.shop.stg_orders");
        let n = Node::new(
            id.clone(),
            "model",
            sample_checksum(),
            Some("select 1".to_owned()),
            Some("{{ config(materialized='view') }} select 1".to_owned()),
            DependsOn::default(),
        );
        assert_eq!(n.id(), &id);
        assert_eq!(n.resource_type(), "model");
        assert_eq!(n.checksum(), &sample_checksum());
        assert_eq!(n.compiled_code(), Some("select 1"));
        assert_eq!(
            n.raw_code(),
            Some("{{ config(materialized='view') }} select 1")
        );
        assert_eq!(n.depends_on(), &DependsOn::default());
    }

    #[test]
    fn node_compiled_code_none_round_trips() {
        let n = Node::new(
            NodeId::new("model.shop.parsed_only"),
            "model",
            sample_checksum(),
            None,
            None,
            DependsOn::default(),
        );
        let json = serde_json::to_string(&n).unwrap();
        let back: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(back, n);
        assert!(back.compiled_code().is_none());
    }

    #[test]
    fn node_tolerates_missing_optional_fields() {
        // Wire shape with only the strictly-required keys present.
        let json = r#"{
            "id": "model.shop.stg_orders",
            "resource_type": "model",
            "checksum": { "name": "sha256", "checksum": "deadbeef" }
        }"#;
        let n: Node = serde_json::from_str(json).unwrap();
        assert!(n.compiled_code().is_none());
        assert!(n.depends_on().macros().is_empty());
        assert!(n.depends_on().nodes().is_empty());
    }

    #[test]
    fn manifest_metadata_constructor_and_getter() {
        let m = ManifestMetadata::new("https://schemas.getdbt.com/dbt/manifest/v12.json");
        assert_eq!(
            m.dbt_schema_version(),
            "https://schemas.getdbt.com/dbt/manifest/v12.json"
        );
    }

    fn empty_unit_test() -> UnitTest {
        UnitTest::new(
            "t".to_owned(),
            NodeId::new("model.shop.stg_orders"),
            Vec::<UnitTestGiven>::new(),
            UnitTestExpect::new(serde_json::Value::Null, None),
            None,
            DependsOn::default(),
            None,
            None,
            None,
        )
    }

    #[test]
    fn manifest_constructor_and_lookups() {
        let mut nodes = HashMap::new();
        let id = NodeId::new("model.shop.stg_orders");
        nodes.insert(
            id.clone(),
            Node::new(
                id.clone(),
                "model",
                sample_checksum(),
                Some("select 1".to_owned()),
                None,
                DependsOn::default(),
            ),
        );

        let mut unit_tests = HashMap::new();
        unit_tests.insert("unit_test.shop.t".to_owned(), empty_unit_test());

        let mut macros = HashMap::new();
        macros.insert("macro.shop.foo".to_owned(), "/* body */".to_owned());

        let m = Manifest::new(ManifestMetadata::new("v12"), nodes, unit_tests, macros);

        assert!(m.node(&id).is_some());
        assert_eq!(m.metadata().dbt_schema_version(), "v12");
        assert!(m.unit_test("unit_test.shop.t").is_some());
        assert!(m.unit_test("missing").is_none());
        assert_eq!(m.macros().len(), 1);
        assert_eq!(m.nodes().len(), 1);
        assert_eq!(m.unit_tests().len(), 1);
    }

    #[test]
    fn manifest_tolerates_missing_collections() {
        // Only `metadata` is structurally required; the rest default to
        // empty maps.
        let json = r#"{
            "metadata": { "dbt_schema_version": "v12" }
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert!(m.nodes().is_empty());
        assert!(m.unit_tests().is_empty());
        assert!(m.macros().is_empty());
    }
}
