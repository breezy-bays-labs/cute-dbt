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
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
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

/// A model's typed `config.unique_key` (cute-dbt#169).
///
/// Mirrors dbt-fusion's `DbtUniqueKey` — an untagged
/// `Single(String) | Multiple(Vec<String>)` enum, so the wire value is a
/// JSON string **or** an array of strings
/// (`dbt-schemas/src/schemas/common.rs`, dbt-fusion
/// `9977b6cbb1b761065536300037560d8e3c037011`; the manifest node config
/// carries it as `Option<DbtUniqueKey>` in
/// `project/configs/model_config.rs`). cute-dbt adds the tolerant
/// [`UniqueKey::Unrecognized`] arm for a present-but-unparseable value
/// (ADR-5: one odd config value must never fail anything; the check
/// engine maps it to an honest `UNKNOWN` verdict instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UniqueKey {
    /// `unique_key: "order_id"` — a single column.
    Single(String),
    /// `unique_key: ["customer_id", "order_date"]` — a composite key.
    Multiple(Vec<String>),
    /// Present but not a non-empty string / array-of-strings (e.g. a
    /// number, an object, or a mixed-type array). The declared grain is
    /// not statically recoverable.
    Unrecognized,
}

impl UniqueKey {
    /// The declared key columns, or `None` when the value shape is
    /// [`UniqueKey::Unrecognized`]. A composite key returns every column
    /// (the set is the grain — callers must never flatten it into
    /// per-column claims).
    #[must_use]
    pub fn columns(&self) -> Option<Vec<&str>> {
        match self {
            Self::Single(column) => Some(vec![column.as_str()]),
            Self::Multiple(columns) => Some(columns.iter().map(String::as_str).collect()),
            Self::Unrecognized => None,
        }
    }
}

/// dbt's per-node `config` sub-object — the v0.2 `state:modified`
/// sub-selector inputs (cute-dbt#17).
///
/// dbt nests a model's resolved configuration under `config`. Two
/// `state:modified` sub-selectors read from it:
///
/// - **`.configs`** ([`ConfigsModifier`](crate::domain::state::ConfigsModifier))
///   — the whole config dict (key set + value set). Stored as a
///   [`BTreeMap`] so the comparison is order-independent and
///   deterministic: two manifests that serialize the same keys in a
///   different order still compare equal.
/// - **`.contract`** ([`ContractModifier`](crate::domain::state::ContractModifier))
///   — `config.contract.enforced` (whether the model enforces a data
///   contract). The column-set half of the contract diff lives on
///   [`Node::columns`] (a top-level sibling of `config` in the wire
///   manifest, not nested under it).
///
/// Tolerant per ADR-5: every field defaults (`config` → empty map,
/// `contract_enforced` → `false`) so older or synthetic manifests
/// without a `config` block still deserialize. The map values are
/// `serde_json::Value` passthrough — `.configs` compares them verbatim,
/// never interpreting individual config keys.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NodeConfig {
    config: BTreeMap<String, Value>,
    contract_enforced: bool,
}

impl NodeConfig {
    /// Canonical constructor.
    #[must_use]
    pub fn new(config: BTreeMap<String, Value>, contract_enforced: bool) -> Self {
        Self {
            config,
            contract_enforced,
        }
    }

    /// The resolved config dict (key set + value set), in deterministic
    /// [`BTreeMap`] key order. Compared verbatim by `ConfigsModifier`.
    #[must_use]
    pub fn config(&self) -> &BTreeMap<String, Value> {
        &self.config
    }

    /// `config.contract.enforced` — `true` when the model enforces a
    /// data contract. Compared by `ContractModifier`.
    #[must_use]
    pub fn contract_enforced(&self) -> bool {
        self.contract_enforced
    }

    /// `config.materialized` — the materialization strategy (`"table"` /
    /// `"view"` / `"incremental"` / …), or `None` when the manifest omits
    /// it or the value is not a string. A pure POD read over the config
    /// dict (DRYs the inline `config().get("materialized")` reads); the
    /// `== "incremental"` derivation lives in the render layer
    /// (cute-dbt#145).
    #[must_use]
    pub fn materialized(&self) -> Option<&str> {
        self.config.get("materialized").and_then(Value::as_str)
    }

    /// `config.unique_key` — the model's declared grain, typed
    /// (cute-dbt#169). A pure POD read over the config dict, mirroring
    /// the [`Self::materialized`] accessor.
    ///
    /// Wire shapes (verified against dbt-fusion `DbtUniqueKey`,
    /// `9977b6cbb1b761065536300037560d8e3c037011`, and the committed
    /// `playground-current.json` fixture which carries both):
    ///
    /// - absent key or explicit JSON `null` (fusion null-fills unset
    ///   `Option` fields) ⇒ `None` — no grain declared;
    /// - a non-empty string ⇒ [`UniqueKey::Single`];
    /// - an array of strings ⇒ [`UniqueKey::Multiple`] (kept composite —
    ///   the set is the grain);
    /// - anything else (empty string, mixed-type array, number, object)
    ///   ⇒ [`UniqueKey::Unrecognized`] — present but not statically
    ///   recoverable, never an error (ADR-5).
    #[must_use]
    pub fn unique_key(&self) -> Option<UniqueKey> {
        match self.config.get("unique_key") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.trim().is_empty() => Some(UniqueKey::Single(s.clone())),
            Some(Value::Array(items)) => {
                let columns: Vec<String> = items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                if columns.len() == items.len() {
                    Some(UniqueKey::Multiple(columns))
                } else {
                    Some(UniqueKey::Unrecognized)
                }
            }
            Some(_) => Some(UniqueKey::Unrecognized),
        }
    }
}

/// dbt's `test_metadata` block on a **generic-test** node (cute-dbt#165).
///
/// Present only on tests instantiated from a generic test definition
/// (`unique`, `not_null`, `accepted_values`, `relationships`,
/// `dbt_utils.*`, …); a singular (SQL-file) test carries no
/// `test_metadata` (fusion's `DbtTestAttr.test_metadata:
/// Option<TestMetadata>`, `dbt-schemas` `nodes.rs`).
///
/// - `name` — the generic test's bare name (e.g. `"unique"`).
/// - `namespace` — the providing package when the test is
///   package-qualified (e.g. `"dbt_utils"`, `"dbt_expectations"`);
///   `None`/`null` for dbt-core built-ins.
/// - `kwargs` — the rendered test arguments, kept as untyped [`Value`]
///   passthrough (fusion types this `BTreeMap<String, YmlValue>`); the
///   render layer reads the key args it summarizes
///   (`accepted_values.values`, `relationships.to`/`field`) and never
///   interprets the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestMetadata {
    name: String,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    kwargs: Value,
}

impl TestMetadata {
    /// Canonical constructor.
    #[must_use]
    pub fn new(name: impl Into<String>, namespace: Option<String>, kwargs: Value) -> Self {
        Self {
            name: name.into(),
            namespace,
            kwargs,
        }
    }

    /// Bare generic-test name (e.g. `"unique"`, `"accepted_values"`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Providing package for a package-qualified test (e.g.
    /// `"dbt_utils"`); `None` for dbt-core built-ins.
    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }

    /// Rendered test arguments, untyped passthrough.
    #[must_use]
    pub fn kwargs(&self) -> &Value {
        &self.kwargs
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
///
/// `original_file_path` is the path of the declaring `.sql` / `.yml`
/// file relative to the dbt project root (e.g.
/// `models/marts/core/dim_payers.sql`). Populated by dbt 1.8+ on every
/// node; tolerated as `None` so older manifests and synthetic test
/// fixtures still deserialize. The
/// [`select_in_scope`](crate::domain::scope::select_in_scope) PR-diff
/// path matches changed file paths against this field.
///
/// `config`, `relation_name`, and `columns` are the v0.2 `state:modified`
/// sub-selector inputs (cute-dbt#17), all ADR-5-tolerant additive fields:
///
/// - `config` ([`NodeConfig`]) feeds `.configs` (the config dict) and
///   `.contract` (`config.contract.enforced`).
/// - `relation_name` is dbt's fully-qualified `"database"."schema"."identifier"`
///   string — the single field `.relation` compares (it encodes
///   database / schema / alias / identifier together, mirroring dbt's
///   own relation diff). `None` for non-relational or synthetic nodes.
/// - `columns` is the model's column set (name → declared `data_type`),
///   the column-set half of the `.contract` diff. A top-level wire
///   sibling of `config`, stored as a [`BTreeMap`] for deterministic
///   comparison.
// `attached_node` (clippy::struct_field_names: ends with the struct's
// name) mirrors the dbt v12 wire key verbatim — renaming it would force
// a serde rename and obscure the field↔wire correspondence ADR-5 leans
// on.
#[allow(clippy::struct_field_names)]
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
    #[serde(default)]
    original_file_path: Option<String>,
    #[serde(default)]
    config: NodeConfig,
    #[serde(default)]
    relation_name: Option<String>,
    #[serde(default)]
    columns: BTreeMap<String, Option<String>>,
    /// Authored per-column descriptions from the model node's `columns`
    /// map (cute-dbt#165) — only columns with a **non-empty** description
    /// appear (fusion serializes an unset description as `""`, never
    /// omitting the key — `serialize_dbt_column_desc` in `dbt-schemas`
    /// `dbt_column.rs`). A separate field from [`Self::columns`] so the
    /// `.contract` sub-selector's column-set comparison stays
    /// name + `data_type` only — a description edit must never flag
    /// `state:modified.contract`.
    #[serde(default)]
    column_descriptions: BTreeMap<String, String>,
    /// `column_name` on a generic-test node (cute-dbt#165) — set iff the
    /// test is **column-scoped** (declared under a column's `tests:`).
    /// `None` on non-test nodes AND on model-level tests that merely take
    /// a column argument.
    #[serde(default)]
    column_name: Option<String>,
    /// `attached_node` on a test node — the model the test is declared
    /// on (fusion's `DbtTestAttr.attached_node`). `None` on non-test
    /// nodes.
    #[serde(default)]
    attached_node: Option<NodeId>,
    /// `test_metadata` on a generic-test node; `None` on singular
    /// (SQL-file) tests and non-test nodes.
    #[serde(default)]
    test_metadata: Option<TestMetadata>,
    /// Authored model description from the node's top-level wire
    /// `description` (cute-dbt#200) — only **non-empty** prose appears
    /// (the cute-dbt#165 precedent: fusion serializes an unset
    /// description as `None`/absent, dbt-core as `""`; the adapter drops
    /// both). Feeds [`ModelPayload::description`] and the report's
    /// `manifest_nodes` lookup.
    #[serde(default)]
    description: Option<String>,
    /// Resolved model tags from the node's top-level wire `tags`
    /// (cute-dbt#200). The TOP-LEVEL list is the authoritative
    /// deduplicated set (fusion `ManifestMaterializableCommonAttributes
    /// .tags`, `dbt-schemas` `manifest_nodes.rs` @ `9977b6cb…`); the
    /// nested `config.tags` carries project-level + model-level merge
    /// DUPLICATES on real dbt-core manifests and is deliberately not
    /// read. Empty for untagged nodes and every pre-#200 fixture.
    #[serde(default)]
    tags: Vec<String>,
}

impl Node {
    /// Canonical constructor — every field is owned and explicit.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: NodeId,
        resource_type: impl Into<String>,
        checksum: Checksum,
        compiled_code: Option<String>,
        raw_code: Option<String>,
        depends_on: DependsOn,
        original_file_path: Option<String>,
        config: NodeConfig,
        relation_name: Option<String>,
        columns: BTreeMap<String, Option<String>>,
    ) -> Self {
        Self {
            id,
            resource_type: resource_type.into(),
            checksum,
            compiled_code,
            raw_code,
            depends_on,
            original_file_path,
            config,
            relation_name,
            columns,
            column_descriptions: BTreeMap::new(),
            column_name: None,
            attached_node: None,
            test_metadata: None,
            description: None,
            tags: Vec::new(),
        }
    }

    /// Attach authored per-column descriptions (cute-dbt#165). A builder
    /// rather than an 11th positional `new` param keeps the many existing
    /// test constructors unchanged (the
    /// [`UnitTest::with_incremental_mode`](crate::domain::unit_test::UnitTest::with_incremental_mode)
    /// precedent).
    #[must_use]
    pub fn with_column_descriptions(
        mut self,
        column_descriptions: BTreeMap<String, String>,
    ) -> Self {
        self.column_descriptions = column_descriptions;
        self
    }

    /// Attach the authored model metadata (cute-dbt#200): the model's
    /// top-level `description` (the adapter passes `None` for an
    /// empty-string description — the cute-dbt#165 drop-empty precedent)
    /// and the top-level resolved `tags` list. Builder for the same
    /// reason as [`Self::with_column_descriptions`] — no constructor
    /// churn across the many existing test call sites.
    #[must_use]
    pub fn with_model_metadata(mut self, description: Option<String>, tags: Vec<String>) -> Self {
        self.description = description;
        self.tags = tags;
        self
    }

    /// Attach the test-node attribution fields (cute-dbt#165):
    /// `column_name` (set iff the test is column-scoped), `attached_node`
    /// (the model the test is declared on), and `test_metadata` (the
    /// generic-test name + namespace + kwargs; `None` for singular
    /// tests). Builder for the same reason as
    /// [`Self::with_column_descriptions`].
    #[must_use]
    pub fn with_test_attachment(
        mut self,
        column_name: Option<String>,
        attached_node: Option<NodeId>,
        test_metadata: Option<TestMetadata>,
    ) -> Self {
        self.column_name = column_name;
        self.attached_node = attached_node;
        self.test_metadata = test_metadata;
        self
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

    /// Path of the declaring `.sql` (or `.yml` for unit-test-shaped
    /// nodes) file, relative to the dbt project root, if dbt populated
    /// it. `None` for synthetic test manifests and pre-1.8 inputs.
    #[must_use]
    pub fn original_file_path(&self) -> Option<&str> {
        self.original_file_path.as_deref()
    }

    /// The node's resolved `config` block (cute-dbt#17). Read by the
    /// `.configs` and `.contract` `state:modified` sub-selectors.
    #[must_use]
    pub fn config(&self) -> &NodeConfig {
        &self.config
    }

    /// dbt's fully-qualified relation name
    /// (`"database"."schema"."identifier"`), if the node is relational.
    /// The single field the `.relation` sub-selector compares (it
    /// encodes database / schema / alias / identifier together). `None`
    /// for non-relational or synthetic nodes.
    #[must_use]
    pub fn relation_name(&self) -> Option<&str> {
        self.relation_name.as_deref()
    }

    /// The node's column set — name → declared `data_type` (`None` when
    /// the column has no declared type). The column-set half of the
    /// `.contract` sub-selector diff. Empty for nodes without a columns
    /// block.
    #[must_use]
    pub fn columns(&self) -> &BTreeMap<String, Option<String>> {
        &self.columns
    }

    /// Authored per-column descriptions (cute-dbt#165) — only columns
    /// with a non-empty description appear. Empty for nodes without a
    /// columns block (and for every pre-#165 fixture).
    #[must_use]
    pub fn column_descriptions(&self) -> &BTreeMap<String, String> {
        &self.column_descriptions
    }

    /// `column_name` on a test node — `Some` iff the test is
    /// column-scoped (cute-dbt#165). `None` for non-test nodes and
    /// model-level tests.
    #[must_use]
    pub fn column_name(&self) -> Option<&str> {
        self.column_name.as_deref()
    }

    /// `attached_node` on a test node — the model the test is declared
    /// on. `None` for non-test nodes.
    #[must_use]
    pub fn attached_node(&self) -> Option<&NodeId> {
        self.attached_node.as_ref()
    }

    /// `test_metadata` on a generic-test node. `None` for singular
    /// (SQL-file) tests and non-test nodes.
    #[must_use]
    pub fn test_metadata(&self) -> Option<&TestMetadata> {
        self.test_metadata.as_ref()
    }

    /// Authored model description (cute-dbt#200) — `None` for an
    /// undescribed model (the adapter drops dbt-core's empty-string
    /// unset shape) and for non-model nodes.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Resolved model tags (cute-dbt#200) — the deduplicated top-level
    /// wire list. Empty for untagged nodes.
    #[must_use]
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
}

/// One entry of the manifest's top-level `sources` map — a dbt
/// `source()` target (cute-dbt#57).
///
/// A **separate POD** from [`Node`], not a kind-variant: dbt itself keeps
/// sources a distinct type end-to-end (fusion's `ManifestSource` /
/// `DbtSource` and the dedicated top-level `sources` map in the v12
/// wire, never merged into `nodes`), and source entries carry **no
/// `checksum` key**, so reusing [`Node`] would force Option-ing a
/// required field. Mirrors the `StateComparator` precedent the issue
/// cites: additive widening, never a domain restructure.
///
/// Keyed in [`Manifest::sources`] by the wire map key
/// (`source.<package>.<source_name>.<name>`); the adapter folds that key
/// into [`Self::id`] (the [`Node`] identity precedent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceNode {
    id: NodeId,
    /// `source()`'s **first** argument — the YAML `sources:` block name.
    source_name: String,
    /// `source()`'s **second** argument — the table name within the block.
    name: String,
    /// Physical table identifier. dbt defaults it to `name`; users may
    /// override it, and dbt preserves it **verbatim including embedded
    /// quote characters** (the reserved-word `"GROUP"` case). `None` when
    /// an engine omits the key (tolerant ingestion, cute-dbt#145 rule).
    #[serde(default)]
    identifier: Option<String>,
    /// Resolved schema name (required by both engines' schemas).
    schema: String,
    /// Resolved database. `Option` — dbt-core emits an explicit `null`
    /// on some adapters; fusion may emit an empty string.
    #[serde(default)]
    database: Option<String>,
    /// dbt's fully-resolved relation (`"db"."schema"."identifier"`).
    /// `Option` in **both** engines' schemas — parsed defensively even
    /// though fusion always populates it.
    #[serde(default)]
    relation_name: Option<String>,
}

impl SourceNode {
    /// Canonical constructor — every field is owned and explicit.
    #[must_use]
    pub fn new(
        id: NodeId,
        source_name: impl Into<String>,
        name: impl Into<String>,
        identifier: Option<String>,
        schema: impl Into<String>,
        database: Option<String>,
        relation_name: Option<String>,
    ) -> Self {
        Self {
            id,
            source_name: source_name.into(),
            name: name.into(),
            identifier,
            schema: schema.into(),
            database,
            relation_name,
        }
    }

    /// Source id (`source.<package>.<source_name>.<name>`).
    #[must_use]
    pub fn id(&self) -> &NodeId {
        &self.id
    }

    /// The YAML `sources:` block name — `source()`'s first argument.
    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// The table name within the block — `source()`'s second argument.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Physical table identifier (verbatim, may carry embedded quotes).
    /// `None` when the engine omitted the key.
    #[must_use]
    pub fn identifier(&self) -> Option<&str> {
        self.identifier.as_deref()
    }

    /// Resolved schema name.
    #[must_use]
    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// Resolved database, when the engine emitted one.
    #[must_use]
    pub fn database(&self) -> Option<&str> {
        self.database.as_deref()
    }

    /// dbt's fully-resolved relation (`"db"."schema"."identifier"`),
    /// when the engine emitted one.
    #[must_use]
    pub fn relation_name(&self) -> Option<&str> {
        self.relation_name.as_deref()
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
    /// The manifest's top-level `sources` map (cute-dbt#57), keyed by
    /// the wire map key (`source.<package>.<source_name>.<name>`).
    /// Defaults to empty so pre-#57 serialized manifests (and synthetic
    /// test fixtures without sources) still deserialize.
    #[serde(default)]
    sources: HashMap<NodeId, SourceNode>,
}

impl Manifest {
    /// Canonical constructor — every field is owned. `sources` starts
    /// empty; attach a parsed sources map via [`Self::with_sources`]
    /// (builder rather than a 5th positional param, the
    /// [`Node::with_column_descriptions`] precedent — keeps the many
    /// existing test constructors unchanged).
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
            sources: HashMap::new(),
        }
    }

    /// Attach the manifest's parsed `sources` map (cute-dbt#57).
    #[must_use]
    pub fn with_sources(mut self, sources: HashMap<NodeId, SourceNode>) -> Self {
        self.sources = sources;
        self
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

    /// All sources keyed by id (cute-dbt#57).
    #[must_use]
    pub fn sources(&self) -> &HashMap<NodeId, SourceNode> {
        &self.sources
    }

    /// Look up a source by its `(source_name, name)` pair — the two
    /// arguments of a `source('a', 'b')` given input (cute-dbt#57).
    ///
    /// Case-insensitive on both halves, keeping the contract symmetric
    /// with the renderer's `ref(...)` binding (`eq_ignore_ascii_case`
    /// throughout). Linear scan — manifests carry few sources and the
    /// lookup runs once per `source(...)` given.
    #[must_use]
    pub fn source_by_name(&self, source_name: &str, name: &str) -> Option<&SourceNode> {
        self.sources.values().find(|source| {
            source.source_name().eq_ignore_ascii_case(source_name)
                && source.name().eq_ignore_ascii_case(name)
        })
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
            Some("models/staging/stg_orders.sql".to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
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
        assert_eq!(
            n.original_file_path(),
            Some("models/staging/stg_orders.sql")
        );
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
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        );
        let json = serde_json::to_string(&n).unwrap();
        let back: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(back, n);
        assert!(back.compiled_code().is_none());
    }

    #[test]
    fn node_tolerates_missing_optional_fields() {
        // Wire shape with only the strictly-required keys present —
        // critical regression guard: every committed fixture (jaffle-shop,
        // playground) predates the `original_file_path` field; they must
        // continue to deserialize without re-baselining golden snapshots.
        let json = r#"{
            "id": "model.shop.stg_orders",
            "resource_type": "model",
            "checksum": { "name": "sha256", "checksum": "deadbeef" }
        }"#;
        let n: Node = serde_json::from_str(json).unwrap();
        assert!(n.compiled_code().is_none());
        assert!(n.depends_on().macros().is_empty());
        assert!(n.depends_on().nodes().is_empty());
        assert!(n.original_file_path().is_none());
        // cute-dbt#17 sub-selector inputs all tolerate absence (ADR-5).
        assert!(n.config().config().is_empty());
        assert!(!n.config().contract_enforced());
        assert!(n.relation_name().is_none());
        assert!(n.columns().is_empty());
    }

    #[test]
    fn node_config_constructor_and_getters() {
        let mut map = BTreeMap::new();
        map.insert("materialized".to_owned(), Value::from("table"));
        let cfg = NodeConfig::new(map, true);
        assert_eq!(
            cfg.config().get("materialized"),
            Some(&Value::from("table"))
        );
        assert!(cfg.contract_enforced());
    }

    #[test]
    fn node_config_default_is_empty_and_unenforced() {
        let cfg = NodeConfig::default();
        assert!(cfg.config().is_empty());
        assert!(!cfg.contract_enforced());
    }

    #[test]
    fn node_config_materialized_reads_string_value() {
        let mut map = BTreeMap::new();
        map.insert("materialized".to_owned(), Value::from("incremental"));
        assert_eq!(
            NodeConfig::new(map, false).materialized(),
            Some("incremental")
        );
        // absent ⇒ None (tolerant — a config dict need not carry it)
        assert_eq!(NodeConfig::default().materialized(), None);
        // non-string value ⇒ None (tolerant — never panics on a bad shape)
        let mut bad = BTreeMap::new();
        bad.insert("materialized".to_owned(), Value::from(42));
        assert_eq!(NodeConfig::new(bad, false).materialized(), None);
    }

    // ----- cute-dbt#169 — NodeConfig::unique_key typed accessor -----

    fn config_with_unique_key(value: Value) -> NodeConfig {
        let mut map = BTreeMap::new();
        map.insert("unique_key".to_owned(), value);
        NodeConfig::new(map, false)
    }

    #[test]
    fn unique_key_absent_is_none() {
        assert_eq!(NodeConfig::default().unique_key(), None);
    }

    #[test]
    fn unique_key_explicit_null_is_none() {
        // fusion null-fills unset Option fields — an explicit JSON null is
        // the ABSENT shape, not an unrecognized one (cute-dbt#145 lesson).
        assert_eq!(config_with_unique_key(Value::Null).unique_key(), None);
    }

    #[test]
    fn unique_key_string_is_single() {
        assert_eq!(
            config_with_unique_key(Value::from("order_id")).unique_key(),
            Some(UniqueKey::Single("order_id".to_owned()))
        );
    }

    #[test]
    fn unique_key_string_array_is_multiple_kept_composite() {
        let key = config_with_unique_key(serde_json::json!(["customer_id", "order_date"]))
            .unique_key()
            .expect("array of strings parses");
        assert_eq!(
            key,
            UniqueKey::Multiple(vec!["customer_id".to_owned(), "order_date".to_owned()])
        );
        // columns() exposes the WHOLE composite set, in declared order.
        assert_eq!(key.columns(), Some(vec!["customer_id", "order_date"]));
    }

    #[test]
    fn unique_key_unrecognized_shapes() {
        // Number, object, mixed-type array, empty string — present but
        // not a statically recoverable grain (never an error, ADR-5).
        for value in [
            Value::from(42),
            serde_json::json!({ "column": "id" }),
            serde_json::json!(["order_id", 7]),
            Value::from(""),
            Value::from("   "),
        ] {
            assert_eq!(
                config_with_unique_key(value.clone()).unique_key(),
                Some(UniqueKey::Unrecognized),
                "value {value} should be Unrecognized"
            );
        }
        assert_eq!(UniqueKey::Unrecognized.columns(), None);
    }

    #[test]
    fn unique_key_empty_array_is_multiple_with_no_columns() {
        // Faithful to the wire: [] parses as Multiple([]); the check
        // engine treats an empty column list as not statically decidable.
        let key = config_with_unique_key(serde_json::json!([]))
            .unique_key()
            .expect("empty array still parses");
        assert_eq!(key, UniqueKey::Multiple(Vec::new()));
        assert_eq!(key.columns(), Some(Vec::new()));
    }

    #[test]
    fn unique_key_single_columns_is_one_element() {
        assert_eq!(
            UniqueKey::Single("encounter_id".to_owned()).columns(),
            Some(vec!["encounter_id"])
        );
    }

    #[test]
    fn node_sub_selector_getters_return_populated_values() {
        let mut config_map = BTreeMap::new();
        config_map.insert("materialized".to_owned(), Value::from("view"));
        let mut columns = BTreeMap::new();
        columns.insert("id".to_owned(), Some("integer".to_owned()));
        let n = Node::new(
            NodeId::new("model.shop.dim_payers"),
            "model",
            sample_checksum(),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config_map, true),
            Some("\"db\".\"main\".\"dim_payers\"".to_owned()),
            columns,
        );
        assert_eq!(
            n.config().config().get("materialized"),
            Some(&Value::from("view"))
        );
        assert!(n.config().contract_enforced());
        assert_eq!(n.relation_name(), Some("\"db\".\"main\".\"dim_payers\""));
        assert_eq!(n.columns().get("id"), Some(&Some("integer".to_owned())));
    }

    #[test]
    fn node_sub_selector_fields_round_trip_through_serde() {
        let mut config_map = BTreeMap::new();
        config_map.insert("materialized".to_owned(), Value::from("table"));
        let mut columns = BTreeMap::new();
        columns.insert("id".to_owned(), Some("bigint".to_owned()));
        columns.insert("name".to_owned(), None);
        let n = Node::new(
            NodeId::new("model.shop.x"),
            "model",
            sample_checksum(),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config_map, false),
            Some("\"db\".\"main\".\"x\"".to_owned()),
            columns,
        );
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn node_original_file_path_round_trips_through_serde() {
        let n = Node::new(
            NodeId::new("model.shop.dim_payers"),
            "model",
            sample_checksum(),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            Some("models/marts/core/dim_payers.sql".to_owned()),
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        );
        let json = serde_json::to_string(&n).unwrap();
        let back: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(back, n);
        assert_eq!(
            back.original_file_path(),
            Some("models/marts/core/dim_payers.sql")
        );
    }

    // ----- cute-dbt#165 — column descriptions + test attribution -----

    #[test]
    fn node_new_defaults_column_meta_fields_empty() {
        let n = Node::new(
            NodeId::new("model.shop.bare"),
            "model",
            sample_checksum(),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        );
        assert!(n.column_descriptions().is_empty());
        assert!(n.column_name().is_none());
        assert!(n.attached_node().is_none());
        assert!(n.test_metadata().is_none());
    }

    #[test]
    fn with_column_descriptions_sets_field() {
        let mut descriptions = BTreeMap::new();
        descriptions.insert("id".to_owned(), "Primary key".to_owned());
        let n = Node::new(
            NodeId::new("model.shop.x"),
            "model",
            sample_checksum(),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_column_descriptions(descriptions);
        assert_eq!(
            n.column_descriptions().get("id").map(String::as_str),
            Some("Primary key")
        );
    }

    #[test]
    fn with_test_attachment_sets_fields() {
        let tm = TestMetadata::new(
            "accepted_values",
            None,
            serde_json::json!({ "values": ["a", "b"] }),
        );
        let n = Node::new(
            NodeId::new("test.shop.accepted_values_x_status"),
            "test",
            sample_checksum(),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            Some("status".to_owned()),
            Some(NodeId::new("model.shop.x")),
            Some(tm.clone()),
        );
        assert_eq!(n.column_name(), Some("status"));
        assert_eq!(n.attached_node(), Some(&NodeId::new("model.shop.x")));
        assert_eq!(n.test_metadata(), Some(&tm));
    }

    // ----- cute-dbt#200 — model description + tags -----

    #[test]
    fn node_new_defaults_model_metadata_empty() {
        let n = Node::new(
            NodeId::new("model.shop.bare"),
            "model",
            sample_checksum(),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        );
        assert!(n.description().is_none());
        assert!(n.tags().is_empty());
    }

    #[test]
    fn with_model_metadata_sets_description_and_tags() {
        let n = Node::new(
            NodeId::new("model.shop.dim_payers"),
            "model",
            sample_checksum(),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_model_metadata(
            Some("One row per payer.".to_owned()),
            vec!["marts".to_owned(), "finance".to_owned()],
        );
        assert_eq!(n.description(), Some("One row per payer."));
        assert_eq!(n.tags(), ["marts".to_owned(), "finance".to_owned()]);
    }

    #[test]
    fn node_model_metadata_round_trips_through_serde() {
        let n = Node::new(
            NodeId::new("model.shop.dim_payers"),
            "model",
            sample_checksum(),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_model_metadata(
            Some("One row per payer.".to_owned()),
            vec!["marts".to_owned()],
        );
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
        assert_eq!(back.description(), Some("One row per payer."));
        assert_eq!(back.tags(), ["marts".to_owned()]);
    }

    #[test]
    fn node_without_model_metadata_deserializes_from_pre_200_json() {
        // ADR-5 tolerance: a serialized pre-#200 Node (no description /
        // tags keys) still deserializes, defaulting both fields.
        let n = Node::new(
            NodeId::new("model.shop.x"),
            "model",
            sample_checksum(),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        );
        let mut value = serde_json::to_value(&n).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.remove("description");
        obj.remove("tags");
        let back: Node = serde_json::from_value(value).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn test_metadata_constructor_and_getters() {
        let tm = TestMetadata::new(
            "expect_column_values_to_be_between",
            Some("dbt_expectations".to_owned()),
            serde_json::json!({ "min_value": 0 }),
        );
        assert_eq!(tm.name(), "expect_column_values_to_be_between");
        assert_eq!(tm.namespace(), Some("dbt_expectations"));
        assert_eq!(tm.kwargs()["min_value"], serde_json::json!(0));
    }

    #[test]
    fn test_metadata_tolerates_missing_optional_fields() {
        // Only `name` is structurally required; `namespace` and `kwargs`
        // default (ADR-5 tolerance — fusion's TestMetadata defaults
        // kwargs and namespace is an Option).
        let tm: TestMetadata = serde_json::from_str(r#"{ "name": "unique" }"#).unwrap();
        assert_eq!(tm.name(), "unique");
        assert_eq!(tm.namespace(), None);
        assert!(tm.kwargs().is_null());
    }

    #[test]
    fn node_column_meta_fields_round_trip_through_serde() {
        let mut descriptions = BTreeMap::new();
        descriptions.insert("id".to_owned(), "Primary key".to_owned());
        let n = Node::new(
            NodeId::new("test.shop.unique_x_id"),
            "test",
            sample_checksum(),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_column_descriptions(descriptions)
        .with_test_attachment(
            Some("id".to_owned()),
            Some(NodeId::new("model.shop.x")),
            Some(TestMetadata::new("unique", None, Value::Null)),
        );
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
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
            UnitTestExpect::new(serde_json::Value::Null, None, None),
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
                None,
                NodeConfig::default(),
                None,
                BTreeMap::new(),
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
        assert!(m.sources().is_empty());
    }

    // ===== SourceNode (cute-dbt#57) =====

    fn sample_source(id: &str, source_name: &str, name: &str) -> SourceNode {
        SourceNode::new(
            NodeId::new(id),
            source_name,
            name,
            Some(name.to_owned()),
            "main",
            Some("memory".to_owned()),
            Some(format!("\"memory\".\"main\".\"{name}\"")),
        )
    }

    #[test]
    fn source_node_constructor_and_accessors() {
        let s = sample_source("source.shop.raw.patients", "raw", "patients");
        assert_eq!(s.id().as_str(), "source.shop.raw.patients");
        assert_eq!(s.source_name(), "raw");
        assert_eq!(s.name(), "patients");
        assert_eq!(s.identifier(), Some("patients"));
        assert_eq!(s.schema(), "main");
        assert_eq!(s.database(), Some("memory"));
        assert_eq!(s.relation_name(), Some("\"memory\".\"main\".\"patients\""));
    }

    #[test]
    fn source_node_serde_round_trips() {
        let s = sample_source("source.shop.raw.patients", "raw", "patients");
        let json = serde_json::to_string(&s).unwrap();
        let back: SourceNode = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn source_node_optional_fields_round_trip_as_none() {
        // The fusion-style minimal entry: identifier / database /
        // relation_name keys absent (tolerant `#[serde(default)]`).
        let json = r#"{
            "id": "source.shop.raw.orders",
            "source_name": "raw",
            "name": "orders",
            "schema": "main"
        }"#;
        let s: SourceNode = serde_json::from_str(json).unwrap();
        assert_eq!(s.identifier(), None);
        assert_eq!(s.database(), None);
        assert_eq!(s.relation_name(), None);
        let back: SourceNode = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn manifest_with_sources_round_trips_through_serde() {
        let source = sample_source("source.shop.raw.patients", "raw", "patients");
        let mut sources = HashMap::new();
        sources.insert(source.id().clone(), source);
        let m = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
        .with_sources(sources);
        let back: Manifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(m, back);
        assert_eq!(back.sources().len(), 1);
    }

    #[test]
    fn source_by_name_resolves_the_given_pair_case_insensitively() {
        let source = sample_source(
            "source.shop.synthea_raw.patients",
            "synthea_raw",
            "patients",
        );
        let mut sources = HashMap::new();
        sources.insert(source.id().clone(), source);
        let m = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
        .with_sources(sources);
        assert!(m.source_by_name("synthea_raw", "patients").is_some());
        assert!(
            m.source_by_name("SYNTHEA_RAW", "Patients").is_some(),
            "lookup is case-insensitive on both halves (symmetric with the ref binding contract)",
        );
    }

    #[test]
    fn source_by_name_requires_both_halves_to_match() {
        let source = sample_source(
            "source.shop.synthea_raw.patients",
            "synthea_raw",
            "patients",
        );
        let mut sources = HashMap::new();
        sources.insert(source.id().clone(), source);
        let m = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
        .with_sources(sources);
        assert!(m.source_by_name("synthea_raw", "encounters").is_none());
        assert!(m.source_by_name("other_block", "patients").is_none());
        assert!(m.source_by_name("", "").is_none());
    }
}
