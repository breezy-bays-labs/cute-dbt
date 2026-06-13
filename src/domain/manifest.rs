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

    /// `config.severity` — the data-test failure severity, typed
    /// (cute-dbt#258). A pure POD read over the config dict (the
    /// [`Self::materialized`] / [`Self::unique_key`] precedent).
    ///
    /// Real wire carries three case variants (live-probed 2026-06-12):
    /// dbt-core's default `"ERROR"`, dbt-core's authored-case `"warn"`
    /// (the committed playground fixture carries both), and fusion
    /// 2.0-preview's `"Warn"` — so the read is case-insensitive. An
    /// unrecognized value degrades to
    /// [`TestSeverity::Unrecognized`], never an error (the
    /// [`UniqueKey::Unrecognized`] posture); absent / `null` ⇒ `None`.
    #[must_use]
    pub fn severity(&self) -> Option<TestSeverity> {
        match self.config.get("severity") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.eq_ignore_ascii_case("error") => Some(TestSeverity::Error),
            Some(Value::String(s)) if s.eq_ignore_ascii_case("warn") => Some(TestSeverity::Warn),
            Some(_) => Some(TestSeverity::Unrecognized),
        }
    }

    /// `config.where` — the data-test row filter (fusion
    /// `DataTestConfig.where_`, `dbt-schemas`
    /// `project/configs/data_test_config.rs` @ `9977b6cb…`), applied via
    /// `get_where_subquery`. `None` when unset (both committed fixtures
    /// null-fill) or a non-string (ADR-5).
    #[must_use]
    pub fn where_filter(&self) -> Option<&str> {
        self.config.get("where").and_then(Value::as_str)
    }

    /// `config.limit` — the data-test failing-row cap (fusion
    /// `Option<i32>`). `None` when unset or non-integer (ADR-5).
    #[must_use]
    pub fn limit(&self) -> Option<i64> {
        self.config.get("limit").and_then(Value::as_i64)
    }

    /// `config.enabled` — nodes in the `nodes` map are enabled (disabled
    /// ones live in [`Manifest::disabled`]), so real wire carries `true`
    /// here; `None` when the key is absent (synthetic fixtures).
    #[must_use]
    pub fn enabled(&self) -> Option<bool> {
        self.config.get("enabled").and_then(Value::as_bool)
    }

    /// `config.store_failures` — whether failing rows persist to the
    /// audit schema. `None` when unset (both committed fixtures
    /// null-fill; the coverage consumer treats that as the adapter
    /// default).
    #[must_use]
    pub fn store_failures(&self) -> Option<bool> {
        self.config.get("store_failures").and_then(Value::as_bool)
    }
}

/// A data-test's typed failure severity (cute-dbt#258).
///
/// Mirrors fusion's two-variant `Severity` enum (`dbt-schemas`
/// `common.rs:1341` @ `9977b6cb…`: `Error` default, `Warn`) plus the
/// tolerant [`Self::Unrecognized`] arm for a present-but-unknown value
/// (the [`UniqueKey::Unrecognized`] precedent — one odd config value
/// must never fail anything, ADR-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestSeverity {
    /// A failing test fails the run (dbt's default).
    Error,
    /// A failing test only warns — the coverage-truthfulness signal
    /// that a "covered" model's guarantee is advisory.
    Warn,
    /// Present but neither `error` nor `warn` in any case variant.
    Unrecognized,
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
    /// both). Feeds the render payload's `ModelPayload.description` and
    /// the report's `manifest_nodes` lookup (no link — domain docs never
    /// reference adapter items, the inward-dependency discipline).
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
    /// Path of the schema-properties `.yml` file that patches this node
    /// (cute-dbt#105) — the `models:` block carrying its description /
    /// columns / tests. Stored **package-relative, scheme-stripped**:
    /// both engines serialize the wire `patch_path` as a package URI
    /// (`<package>://models/schema.yml` — fusion
    /// `normalize_manifest_patch_path` / `package_uri_path`,
    /// `dbt-schemas` `manifest/manifest.rs` @ `9977b6cb…`, mirroring
    /// dbt-core), and the manifest adapter strips the scheme on
    /// ingestion so the domain carries a plain relative path (for
    /// root-project nodes this is project-relative — the
    /// `original_file_path` shape). `None` for nodes without a schema
    /// patch and every pre-#105 fixture.
    #[serde(default)]
    patch_path: Option<String>,
    /// The node's authored bare name from the top-level wire `name`
    /// (cute-dbt#256; fusion `ManifestCommonAttributes.name`,
    /// `dbt-schemas` `manifest/manifest_nodes.rs:84-88` @ `9977b6cb…`).
    /// For a **versioned model** this is the only truthful handle — the
    /// `unique_id` leaf segment is the version suffix (`.v2`), verified
    /// live on fusion 2.0-preview (`model.jaffle_shop.versioned_demo.v2`
    /// carries `name: "versioned_demo"`). `None` for pre-#256 fixtures;
    /// [`Self::bare_name`] falls back to the leaf segment.
    /// Every new #256 Option field carries `skip_serializing_if` so a
    /// serialized Node without governance data stays byte-identical to
    /// its pre-#256 serialization (payload byte-stability).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// The package/project owning this node (cute-dbt#256) — the
    /// own-project-vs-installed-package partition input. Joined against
    /// [`ManifestMetadata::project_name`]. Always populated on real
    /// dbt-core 1.11 / fusion 2.0-preview wire; `None` tolerated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package_name: Option<String>,
    /// The model's governance group NAME (cute-dbt#256) — joins
    /// [`Manifest::groups`] via [`Manifest::group_by_name`]. Both
    /// engines emit `null` for ungrouped models (the committed fixtures'
    /// shape); fusion ≥2.0-preview.177 may omit the key entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    /// The model's access level (cute-dbt#256) — `"private"` /
    /// `"protected"` / `"public"` (fusion `Access`, `dbt-schemas`
    /// `common.rs:524-529` @ `9977b6cb…`). Kept a tolerant string —
    /// unknown future levels must never fail ingestion (ADR-5). Both
    /// committed real fixtures populate the default `"protected"` on
    /// every model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access: Option<String>,
    /// The model's version (cute-dbt#256), post-normalized to a string:
    /// the wire is fusion `StringOrInteger` (`dbt-schemas`
    /// `serde.rs:419-422` @ `9977b6cb…`) and a real fusion compile emits
    /// the bare integer `2` — the adapter renders integers in decimal.
    /// `None` for unversioned models (both engines emit explicit `null`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    /// The latest declared version of this model's family (cute-dbt#256)
    /// — same wire shape + normalization as [`Self::version`]. An
    /// unpinned `ref()` resolves to the node whose `version` equals
    /// `latest_version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_version: Option<String>,
    /// The model's declared deprecation date (cute-dbt#256), verbatim
    /// wire string (fusion `Option<String>`). `None` when undeclared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deprecation_date: Option<String>,
    /// The node's fully-qualified name path (cute-dbt#257) —
    /// `[package, ...folder components..., name(.vN)]`, built by the
    /// engine from the package name + the resource-path-stripped file
    /// path (fusion `get_node_fqn`, `dbt-parser` `utils.rs:132-159` @
    /// `9977b6cb…`). The config-tree prefix-matcher input (#262 C2):
    /// a `dbt_project.yml` `models: <pkg>: <folder>: +config` path is
    /// exactly an fqn prefix. Populated on every node of both committed
    /// real fixtures; empty for pre-#257 synthetic fixtures.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    fqn: Vec<String>,
    /// Model-level declared constraints (cute-dbt#257) — the static-ERD
    /// edge input (FK `to`/`to_columns`) and the contract surface. Both
    /// engines emit `[]` when none are declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    constraints: Vec<Constraint>,
    /// The engine-INFERRED primary-key column set (cute-dbt#257) —
    /// fusion `ManifestModel.primary_key: Option<Vec<String>>`
    /// (`manifest_nodes.rs:782+` @ `9977b6cb…`). Derived by the engine
    /// from PK constraints and `unique`+`not_null` tests; POPULATED on real
    /// wire for most models (both committed fixtures carry e.g.
    /// `["payer_key"]`) — the grain-intelligence sibling of
    /// `config.unique_key`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    primary_key: Vec<String>,
    /// The contracted schema's checksum from the node's TOP-LEVEL
    /// `contract` block (cute-dbt#257), hoisted flat (the
    /// `config.contract.enforced` precedent). dbt-core emits a hex
    /// string when the contract is enforced and `null` otherwise;
    /// fusion 2.0-preview omits the key even when enforced
    /// (live-verified) — `None` covers both unset shapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contract_checksum: Option<String>,
    /// Per-column facts beyond `data_type`/description (cute-dbt#257):
    /// meta / tags / `policy_tags` / constraints. Only columns with at
    /// least one fact appear (the `column_descriptions` precedent).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    column_facts: BTreeMap<String, ColumnFacts>,
    /// The node's AUTHORED pre-Jinja config values (cute-dbt#258) —
    /// fusion `NodeBaseAttributes.unrendered_config:
    /// BTreeMap<String, YmlValue>` (`dbt-schemas` `nodes.rs:4536-4541`
    /// @ `9977b6cb…`), the dbt-core-compatible `state:*` comparison
    /// surface. The #262 C3 provenance input: a key here was set by the
    /// author (model file / properties YAML / `dbt_project.yml`); a
    /// resolved [`Self::config`] key absent here came from defaults.
    /// Values pass through verbatim (nested objects like
    /// `docs.node_color` included). Engine divergence (live-probed
    /// 2026-06-12): dbt-core emits every authored key; fusion
    /// 2.0-preview emits a SPARSER map (e.g. drops `materialized` /
    /// `enabled` authored in `dbt_project.yml` or SQL `config()`) and
    /// `{}` on test nodes — consumers must treat absence as
    /// "provenance unknown", never "not authored".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    unrendered_config: BTreeMap<String, Value>,
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
            patch_path: None,
            name: None,
            package_name: None,
            group: None,
            access: None,
            version: None,
            latest_version: None,
            deprecation_date: None,
            fqn: Vec::new(),
            constraints: Vec::new(),
            primary_key: Vec::new(),
            contract_checksum: None,
            column_facts: BTreeMap::new(),
            unrendered_config: BTreeMap::new(),
        }
    }

    /// Attach the node's fully-qualified name path (cute-dbt#257).
    /// Builder for the same reason as
    /// [`Self::with_column_descriptions`] — no constructor churn.
    #[must_use]
    pub fn with_fqn(mut self, fqn: Vec<String>) -> Self {
        self.fqn = fqn;
        self
    }

    /// Attach the contract family (cute-dbt#257): model-level declared
    /// `constraints`, the engine-inferred `primary_key`, and the
    /// contracted schema's `contract_checksum`.
    #[must_use]
    pub fn with_contract_facts(
        mut self,
        constraints: Vec<Constraint>,
        primary_key: Vec<String>,
        contract_checksum: Option<String>,
    ) -> Self {
        self.constraints = constraints;
        self.primary_key = primary_key;
        self.contract_checksum = contract_checksum;
        self
    }

    /// Attach the per-column facts map (cute-dbt#257) — the adapter
    /// passes only columns with at least one fact.
    #[must_use]
    pub fn with_column_facts(mut self, column_facts: BTreeMap<String, ColumnFacts>) -> Self {
        self.column_facts = column_facts;
        self
    }

    /// Attach the node's authored pre-Jinja config values (cute-dbt#258)
    /// — the adapter passes the wire `unrendered_config` map verbatim.
    #[must_use]
    pub fn with_unrendered_config(mut self, unrendered_config: BTreeMap<String, Value>) -> Self {
        self.unrendered_config = unrendered_config;
        self
    }

    /// Attach the node's identity fields (cute-dbt#256): the authored
    /// bare `name` and the owning `package_name`. Builder for the same
    /// reason as [`Self::with_column_descriptions`] — no constructor
    /// churn across the many existing test call sites.
    #[must_use]
    pub fn with_identity(mut self, name: Option<String>, package_name: Option<String>) -> Self {
        self.name = name;
        self.package_name = package_name;
        self
    }

    /// Attach the node's governance fields (cute-dbt#256): the group
    /// NAME and the access level.
    #[must_use]
    pub fn with_governance(mut self, group: Option<String>, access: Option<String>) -> Self {
        self.group = group;
        self.access = access;
        self
    }

    /// Attach the model-version fields (cute-dbt#256, deferred from
    /// #254): `version` / `latest_version` (post-normalized strings) and
    /// the declared `deprecation_date`.
    #[must_use]
    pub fn with_versions(
        mut self,
        version: Option<String>,
        latest_version: Option<String>,
        deprecation_date: Option<String>,
    ) -> Self {
        self.version = version;
        self.latest_version = latest_version;
        self.deprecation_date = deprecation_date;
        self
    }

    /// Attach the schema-properties YAML path (cute-dbt#105) — the
    /// package-relative, scheme-stripped `patch_path` (the adapter
    /// strips the `<package>://` URI scheme on ingestion). Builder for
    /// the same reason as [`Self::with_column_descriptions`] — no
    /// constructor churn across the many existing test call sites.
    #[must_use]
    pub fn with_patch_path(mut self, patch_path: Option<String>) -> Self {
        self.patch_path = patch_path;
        self
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

    /// Path of the schema-properties `.yml` file that patches this node
    /// (cute-dbt#105) — package-relative, scheme-stripped (the adapter
    /// drops the wire's `<package>://` URI prefix). `None` for nodes
    /// without a schema patch.
    #[must_use]
    pub fn patch_path(&self) -> Option<&str> {
        self.patch_path.as_deref()
    }

    /// The node's authored bare name (cute-dbt#256), when the manifest
    /// carried one. `None` for pre-#256 fixtures — prefer
    /// [`Self::bare_name`] for lookups.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The package/project owning this node (cute-dbt#256).
    #[must_use]
    pub fn package_name(&self) -> Option<&str> {
        self.package_name.as_deref()
    }

    /// The node's governance group NAME (cute-dbt#256) — resolve the
    /// full [`Group`] via [`Manifest::group_by_name`]. `None` for
    /// ungrouped nodes.
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// The model's access level (cute-dbt#256) — `"private"` /
    /// `"protected"` / `"public"`, tolerant of unknown future values.
    #[must_use]
    pub fn access(&self) -> Option<&str> {
        self.access.as_deref()
    }

    /// The model's version (cute-dbt#256), post-normalized to a string
    /// (the wire integer `2` arrives as `"2"`). `None` for unversioned
    /// models.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// The latest declared version of this model's family
    /// (cute-dbt#256). `None` for unversioned models.
    #[must_use]
    pub fn latest_version(&self) -> Option<&str> {
        self.latest_version.as_deref()
    }

    /// The model's declared deprecation date (cute-dbt#256), verbatim.
    #[must_use]
    pub fn deprecation_date(&self) -> Option<&str> {
        self.deprecation_date.as_deref()
    }

    /// The node's fully-qualified name path (cute-dbt#257) —
    /// `[package, ...folders..., name]`. Empty for pre-#257 fixtures.
    #[must_use]
    pub fn fqn(&self) -> &[String] {
        &self.fqn
    }

    /// Model-level declared constraints (cute-dbt#257).
    #[must_use]
    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// The engine-inferred primary-key column set (cute-dbt#257).
    /// Empty when the engine could not infer one.
    #[must_use]
    pub fn primary_key(&self) -> &[String] {
        &self.primary_key
    }

    /// The contracted schema's checksum (cute-dbt#257) — `None` for
    /// unenforced contracts, every fusion 2.0-preview manifest
    /// (live-verified omission), and pre-#257 fixtures.
    #[must_use]
    pub fn contract_checksum(&self) -> Option<&str> {
        self.contract_checksum.as_deref()
    }

    /// Per-column facts beyond `data_type`/description (cute-dbt#257) —
    /// only columns with at least one fact appear.
    #[must_use]
    pub fn column_facts(&self) -> &BTreeMap<String, ColumnFacts> {
        &self.column_facts
    }

    /// The node's AUTHORED pre-Jinja config values (cute-dbt#258) — the
    /// #262 C3 provenance surface. Empty when nothing was authored, on
    /// fusion test nodes, and on pre-#258 fixtures.
    #[must_use]
    pub fn unrendered_config(&self) -> &BTreeMap<String, Value> {
        &self.unrendered_config
    }

    /// `true` for a **singular** (SQL-file) data test: a `test` node
    /// without `test_metadata` (cute-dbt#258). Both engines omit
    /// `test_metadata` / `attached_node` on singular tests (live-probed
    /// 2026-06-12), so linkage to the tested model travels ONLY through
    /// [`Self::depends_on`] — the committed playground fixture's
    /// `assert_*` tests are the real specimens.
    #[must_use]
    pub fn is_singular_test(&self) -> bool {
        self.resource_type == "test" && self.test_metadata.is_none()
    }

    /// The node's bare name for `ref(...)` / `models:`-entry / display
    /// resolution: the ingested wire [`Self::name`] when present (and
    /// non-empty — defensive), else the final dot-segment of the id —
    /// the exact pre-#256 behavior, preserved for synthetic fixtures
    /// that carry no `name`. For a versioned model the ingested name is
    /// the only correct answer (the leaf segment is the `.vN` suffix);
    /// the fallback keeps the documented pre-#256 wart in that case.
    #[must_use]
    pub fn bare_name(&self) -> &str {
        match self.name.as_deref() {
            Some(name) if !name.is_empty() => name,
            _ => self
                .id
                .as_str()
                .rsplit('.')
                .next()
                .unwrap_or(self.id.as_str()),
        }
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
    /// Authored per-column descriptions (cute-dbt#235) — only columns
    /// with a non-empty description appear, mirroring
    /// [`Node::column_descriptions`] (the cute-dbt#165 ingestion rule).
    /// Both engines serialize source `columns` like node columns
    /// (fusion `ManifestSource.columns` via `serialize_dbt_columns`,
    /// `dbt-schemas` `manifest_nodes.rs` @ `9977b6cb…`). Feeds the
    /// given-table column-header tooltips for `source(...)` inputs.
    #[serde(default)]
    column_descriptions: BTreeMap<String, String>,
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
            column_descriptions: BTreeMap::new(),
        }
    }

    /// Attach authored per-column descriptions (cute-dbt#235) — the
    /// [`Node::with_column_descriptions`] builder precedent. The adapter
    /// passes only non-empty prose (empty-string unset shapes dropped).
    #[must_use]
    pub fn with_column_descriptions(
        mut self,
        column_descriptions: BTreeMap<String, String>,
    ) -> Self {
        self.column_descriptions = column_descriptions;
        self
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

    /// Authored per-column descriptions (cute-dbt#235) — only columns
    /// with a non-empty description appear. Empty for sources without a
    /// columns block (and for every pre-#235 serialization — tolerant
    /// `#[serde(default)]`).
    #[must_use]
    pub fn column_descriptions(&self) -> &BTreeMap<String, String> {
        &self.column_descriptions
    }
}

/// `metadata` block — the `dbt_schema_version` floor check (ADR-2
/// Stage-1, PR 4b) plus the project identity (cute-dbt#256).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestMetadata {
    /// `metadata.dbt_schema_version` URL/string. Read verbatim — the
    /// adapter (PR 4b) is responsible for parsing the embedded version
    /// number for the floor comparison.
    dbt_schema_version: String,
    /// `metadata.project_name` (cute-dbt#256) — the root project's name,
    /// the own-project half of the package partition (joined against
    /// each node's [`Node::package_name`]). fusion types it
    /// `#[serde(default)] String` (`dbt-schemas`
    /// `manifest/manifest.rs:72-73` @ `9977b6cb…`) so an unset name
    /// arrives as `""`; both committed real fixtures populate it.
    /// Stored verbatim — [`Self::project_name`] drops the empty-string
    /// unset shape (the #165/#200 precedent). `skip_serializing_if`
    /// keeps pre-#256 serializations byte-stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_name: Option<String>,
    /// `metadata.adapter_type` (cute-dbt#260) — the warehouse adapter the
    /// manifest was compiled against (`"duckdb"` / `"postgres"` /
    /// `"snowflake"` / `"bigquery"` / …), a HEADER-level field (not a
    /// node). The enforcement-reality surface keys the
    /// adapter × constraint support matrix on it. `None` on pre-#260
    /// fixtures (both committed real fixtures populate it as `"duckdb"`).
    /// `skip_serializing_if` keeps pre-#260 serializations byte-stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adapter_type: Option<String>,
}

impl ManifestMetadata {
    /// Canonical constructor.
    #[must_use]
    pub fn new(dbt_schema_version: impl Into<String>) -> Self {
        Self {
            dbt_schema_version: dbt_schema_version.into(),
            project_name: None,
            adapter_type: None,
        }
    }

    /// Attach the root project's name (cute-dbt#256) — builder, the
    /// [`Node::with_column_descriptions`] precedent (no constructor
    /// churn across existing call sites).
    #[must_use]
    pub fn with_project_name(mut self, project_name: Option<String>) -> Self {
        self.project_name = project_name;
        self
    }

    /// Attach the warehouse adapter type (cute-dbt#260) — builder, the
    /// [`Self::with_project_name`] precedent (no constructor churn).
    #[must_use]
    pub fn with_adapter_type(mut self, adapter_type: Option<String>) -> Self {
        self.adapter_type = adapter_type;
        self
    }

    /// `dbt_schema_version` value (verbatim from the manifest).
    #[must_use]
    pub fn dbt_schema_version(&self) -> &str {
        &self.dbt_schema_version
    }

    /// The root project's name (cute-dbt#256). `None` when the manifest
    /// omitted it or carried fusion's empty-string unset default.
    #[must_use]
    pub fn project_name(&self) -> Option<&str> {
        self.project_name.as_deref().filter(|name| !name.is_empty())
    }

    /// The warehouse adapter type (cute-dbt#260) — `"duckdb"` /
    /// `"postgres"` / `"snowflake"` / `"bigquery"` / … . `None` when the
    /// manifest omitted it or carried an empty-string unset shape (the
    /// [`Self::project_name`] precedent).
    #[must_use]
    pub fn adapter_type(&self) -> Option<&str> {
        self.adapter_type.as_deref().filter(|t| !t.is_empty())
    }
}

/// A dbt owner block (cute-dbt#256) — carried by [`Group`]s and
/// [`Exposure`]s, the only ownership signal in the manifest artifact
/// chain (review routing, findings-envelope assignees).
///
/// **Post-normalized shape**: the wire `email` is fusion's
/// `Option<StringOrArrayOfStrings>` (`DbtOwner`, `dbt-schemas`
/// `manifest/common.rs:39-44` @ `9977b6cb…` — dbt-core emits a single
/// string); the adapter normalizes both to a list (a lone string becomes
/// a one-element list, empty strings dropped). fusion serializes an
/// unset owner `name` as an explicit `null` (`#[serialize_always]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Vec<String>,
}

impl Owner {
    /// Canonical constructor.
    #[must_use]
    pub fn new(name: Option<String>, email: Vec<String>) -> Self {
        Self { name, email }
    }

    /// The owner's display name, when declared.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The owner's email address(es) — post-normalized list (a wire
    /// string arrives as one element). Empty when undeclared.
    #[must_use]
    pub fn email(&self) -> &[String] {
        &self.email
    }
}

/// One entry of the manifest's top-level `exposures` map (cute-dbt#256)
/// — a downstream consumer (dashboard / notebook / analysis / ml /
/// application) declared in properties YAML. The highest-leverage
/// P1 signal in the ignored-key inventory: a changed model whose
/// `depends_on` chain terminates in an exposure is "this PR affects
/// dashboard X, owner Y".
///
/// A separate POD from [`Node`] (the [`SourceNode`] precedent): dbt
/// keeps exposures a distinct type end-to-end (fusion
/// `ManifestExposure`, `dbt-schemas` `manifest/manifest_nodes.rs:1526+`
/// @ `9977b6cb…`) and exposure entries carry no `checksum`. Keyed in
/// [`Manifest::exposures`] by the wire map key
/// (`exposure.<package>.<name>`), folded into [`Self::id`].
// `exposure_type` (clippy::struct_field_names: starts with the struct's
// name) — the wire key is `type`, a Rust keyword; `exposure_type` keeps
// the field and its accessor self-describing (fusion's own workaround is
// `type_`). The Node `attached_node` allow precedent.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exposure {
    id: NodeId,
    /// The exposure's bare name (the YAML `- name:`).
    name: String,
    /// The exposure kind — `"dashboard"` / `"notebook"` / `"analysis"` /
    /// `"ml"` / `"application"` (fusion `ExposureType`, `dbt-schemas`
    /// `nodes.rs:4594-4602` @ `9977b6cb…`). Kept a tolerant string
    /// (ADR-5: unknown future kinds must never fail ingestion). The
    /// wire key is `type`; the field name avoids a Rust keyword.
    #[serde(default)]
    exposure_type: Option<String>,
    /// The exposure's URL, when declared.
    #[serde(default)]
    url: Option<String>,
    /// The owning team/person. fusion requires `owner` at parse on
    /// authored exposures; tolerated as `None` here (ADR-5, and an
    /// owner with no content collapses to `None` in the adapter).
    #[serde(default)]
    owner: Option<Owner>,
    /// The models/sources/metrics this exposure reads — the lineage
    /// terminus edge set.
    #[serde(default)]
    depends_on: DependsOn,
}

impl Exposure {
    /// Canonical constructor — every field is owned and explicit.
    #[must_use]
    pub fn new(
        id: NodeId,
        name: impl Into<String>,
        exposure_type: Option<String>,
        url: Option<String>,
        owner: Option<Owner>,
        depends_on: DependsOn,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            exposure_type,
            url,
            owner,
            depends_on,
        }
    }

    /// Exposure id (`exposure.<package>.<name>`).
    #[must_use]
    pub fn id(&self) -> &NodeId {
        &self.id
    }

    /// The exposure's bare name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The exposure kind (`"dashboard"`, …), when declared.
    #[must_use]
    pub fn exposure_type(&self) -> Option<&str> {
        self.exposure_type.as_deref()
    }

    /// The exposure's URL, when declared.
    #[must_use]
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// The owning team/person, when declared with content.
    #[must_use]
    pub fn owner(&self) -> Option<&Owner> {
        self.owner.as_ref()
    }

    /// The nodes (and macros) this exposure depends on.
    #[must_use]
    pub fn depends_on(&self) -> &DependsOn {
        &self.depends_on
    }
}

/// One entry of the manifest's top-level `groups` map (cute-dbt#256) —
/// a named governance group with an owner. Nodes reference groups by
/// NAME (the [`Node::group`] field), not by map key; join via
/// [`Manifest::group_by_name`].
///
/// fusion **requires** `owner:` at parse on an authored group
/// (`GroupProperties.owner: DbtOwner`, no default — `dbt-schemas`
/// `properties/properties.rs:120-125` @ `9977b6cb…`; verified live on
/// fusion 2.0-preview: a group without `owner:` fails compile with
/// `dbt1013 missing field 'owner'`). cute-dbt still tolerates an absent
/// or content-free owner (ADR-5 — synthetic fixtures and engine drift
/// must never fail the parse).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    name: String,
    #[serde(default)]
    owner: Option<Owner>,
}

impl Group {
    /// Canonical constructor.
    #[must_use]
    pub fn new(name: impl Into<String>, owner: Option<Owner>) -> Self {
        Self {
            name: name.into(),
            owner,
        }
    }

    /// The group's name — the value a node's [`Node::group`] carries.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The owning team/person, when declared with content.
    #[must_use]
    pub fn owner(&self) -> Option<&Owner> {
        self.owner.as_ref()
    }
}

/// A declared dbt constraint (cute-dbt#257) — model-level or
/// column-level. The static-ERD edge input (FK `to`/`to_columns` +
/// `relationships` tests) and the contract-intelligence surface.
///
/// One POD for both levels, mirroring the wire: fusion splits
/// `ModelConstraint` (`dbt-schemas` `properties/model_properties.rs:31-51`
/// @ `9977b6cb…`, with `columns`) from the column `Constraint`
/// (`common.rs:888-906`, without) but the shapes are otherwise
/// identical — here `columns` simply stays empty on column-level
/// entries. Deserializes the wire verbatim (the `Checksum`/`DependsOn`
/// reuse precedent — no `Wire*` twin): every field defaults, unknown
/// siblings (`warn_unenforced`/`warn_unsupported`) are ignored, and a
/// missing `type` degrades to the [`ConstraintKind::Other`] arm rather
/// than failing the parse (ADR-5).
///
/// **Engine divergence on FK `to` (live-verified 2026-06-11):**
/// dbt-core 1.11 RESOLVES it to the quoted relation
/// (`"db"."schema"."dim_payers"`), fusion 2.0-preview keeps the
/// AUTHORED `ref('dim_payers')`. Stored verbatim — the ERD consumer
/// owns the normalization.
// `constraint_type` (clippy::struct_field_names: starts with the
// struct's name) — the wire key is `type`, a Rust keyword;
// `constraint_type` keeps the field and its accessor self-describing.
// The Exposure `exposure_type` allow precedent.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Constraint {
    /// The wire `type` string, verbatim ([`Self::kind`] types it).
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    constraint_type: String,
    /// Constrained columns — model-level constraints only (a
    /// column-level constraint's column is its map key).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    columns: Vec<String>,
    /// `check`/`custom` constraint expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expression: Option<String>,
    /// Optional constraint name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// FK target relation — engine-divergent shape, verbatim (see type
    /// docs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    /// FK target columns. Both engines emit `[]` when unset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    to_columns: Vec<String>,
}

/// The typed dbt constraint vocabulary (cute-dbt#257) — the six wire
/// kinds plus the unknown-tolerant [`Self::Other`] arm (fusion
/// `ConstraintType`, `dbt-schemas` `common.rs:917-925` @ `9977b6cb…`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKind {
    /// `primary_key`
    PrimaryKey,
    /// `foreign_key` — carries [`Constraint::to`]/[`Constraint::to_columns`].
    ForeignKey,
    /// `unique`
    Unique,
    /// `not_null`
    NotNull,
    /// `check` — carries [`Constraint::expression`].
    Check,
    /// `custom`
    Custom,
    /// Anything outside the dbt vocabulary (or a missing `type`) —
    /// present but not statically typed, never an error (ADR-5).
    Other,
}

impl Constraint {
    /// Canonical constructor — every field is owned and explicit.
    #[must_use]
    pub fn new(
        constraint_type: impl Into<String>,
        columns: Vec<String>,
        expression: Option<String>,
        name: Option<String>,
        to: Option<String>,
        to_columns: Vec<String>,
    ) -> Self {
        Self {
            constraint_type: constraint_type.into(),
            columns,
            expression,
            name,
            to,
            to_columns,
        }
    }

    /// The wire `type` string, verbatim.
    #[must_use]
    pub fn constraint_type(&self) -> &str {
        &self.constraint_type
    }

    /// The typed constraint vocabulary — unknown-tolerant.
    #[must_use]
    pub fn kind(&self) -> ConstraintKind {
        match self.constraint_type.as_str() {
            "primary_key" => ConstraintKind::PrimaryKey,
            "foreign_key" => ConstraintKind::ForeignKey,
            "unique" => ConstraintKind::Unique,
            "not_null" => ConstraintKind::NotNull,
            "check" => ConstraintKind::Check,
            "custom" => ConstraintKind::Custom,
            _ => ConstraintKind::Other,
        }
    }

    /// Constrained columns (model-level constraints only).
    #[must_use]
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// `check`/`custom` expression, when declared.
    #[must_use]
    pub fn expression(&self) -> Option<&str> {
        self.expression.as_deref()
    }

    /// Constraint name, when declared.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// FK target relation, verbatim (engine-divergent shape — see type
    /// docs).
    #[must_use]
    pub fn to(&self) -> Option<&str> {
        self.to.as_deref()
    }

    /// FK target columns.
    #[must_use]
    pub fn to_columns(&self) -> &[String] {
        &self.to_columns
    }
}

/// The cute-dbt#257 column-level extension — the per-column facts
/// beyond the already-ingested `data_type` ([`Node::columns`]) and
/// `description` ([`Node::column_descriptions`]): authored `meta`,
/// resolved `tags`, `BigQuery` `policy_tags`, and declared `constraints`
/// (fusion `DbtColumn`, `dbt-schemas` `dbt_column.rs:38-60` @
/// `9977b6cb…`). Grouped in one POD (rather than four more parallel
/// maps) since the whole family arrived together; only columns with at
/// least one fact appear in [`Node::column_facts`] (the
/// `column_descriptions` drop-empty precedent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnFacts {
    /// Authored column `meta`, untyped passthrough (the `config.meta`
    /// precedent). dbt-core emits `{}` when unset — dropped to `None`
    /// by the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<Value>,
    /// Resolved column tags. fusion authoring requires column
    /// `meta`/`tags` under the column's `config:` (top-level is a
    /// dbt1060 error, live-verified) while dbt-core accepts top-level —
    /// both engines SERIALIZE the merged result top-level on the wire,
    /// which is what the adapter reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    /// `BigQuery` policy tags — a first-class fusion `DbtColumn` field
    /// (column governance that escapes `meta`); dbt-core 1.11 does not
    /// serialize the key at all (live-verified).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    policy_tags: Vec<String>,
    /// Declared column-level constraints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    constraints: Vec<Constraint>,
}

impl ColumnFacts {
    /// Canonical constructor — every field is owned and explicit.
    #[must_use]
    pub fn new(
        meta: Option<Value>,
        tags: Vec<String>,
        policy_tags: Vec<String>,
        constraints: Vec<Constraint>,
    ) -> Self {
        Self {
            meta,
            tags,
            policy_tags,
            constraints,
        }
    }

    /// Authored column `meta`, when non-empty.
    #[must_use]
    pub fn meta(&self) -> Option<&Value> {
        self.meta.as_ref()
    }

    /// Resolved column tags.
    #[must_use]
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// `BigQuery` policy tags (fusion first-class field).
    #[must_use]
    pub fn policy_tags(&self) -> &[String] {
        &self.policy_tags
    }

    /// Declared column-level constraints.
    #[must_use]
    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// `true` when the entry carries no fact at all — the adapter never
    /// stores an empty one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.meta.is_none()
            && self.tags.is_empty()
            && self.policy_tags.is_empty()
            && self.constraints.is_empty()
    }
}

/// One entry of the manifest's top-level `disabled` map (cute-dbt#258)
/// — a node the project declares but excludes via `enabled: false`.
///
/// A tolerant LIGHT projection, not a [`Node`]: the wire entries are
/// whole heterogeneous node payloads (models, tests, seeds, sources,
/// future kinds — fusion `build_disabled_map`, `dbt-schemas`
/// `manifest/manifest.rs:648` @ `9977b6cb…`, returns
/// `BTreeMap<String, Vec<YmlValue>>`), and the coverage-truthfulness
/// consumer needs only identity + test linkage. Every field defaults so
/// any object shape ingests (ADR-5).
///
/// Linkage truth (live-probed on both engines 2026-06-12): a disabled
/// GENERIC test keeps `attached_node` / `column_name` /
/// `test_metadata`; a disabled SINGULAR test and a disabled model carry
/// none of them AND an empty `depends_on` (disabled nodes are never
/// resolved) — absence here is honest "linkage unknown".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DisabledEntry {
    #[serde(default)]
    resource_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    column_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attached_node: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    test_metadata: Option<TestMetadata>,
}

impl DisabledEntry {
    /// Canonical constructor — the resource kind discriminator.
    #[must_use]
    pub fn new(resource_type: impl Into<String>) -> Self {
        Self {
            resource_type: resource_type.into(),
            ..Self::default()
        }
    }

    /// Attach the authored bare name.
    #[must_use]
    pub fn with_name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    /// Attach the project-relative source path.
    #[must_use]
    pub fn with_original_file_path(mut self, original_file_path: Option<String>) -> Self {
        self.original_file_path = original_file_path;
        self
    }

    /// Attach the test-linkage triplet (the [`Node::with_test_attachment`]
    /// shape) — populated only for disabled generic tests.
    #[must_use]
    pub fn with_attachment(
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

    /// `resource_type` discriminator (`"model"` / `"test"` / `"seed"` /
    /// …); empty when the wire entry omitted it (degenerate, tolerated).
    #[must_use]
    pub fn resource_type(&self) -> &str {
        &self.resource_type
    }

    /// The authored bare name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The project-relative source path.
    #[must_use]
    pub fn original_file_path(&self) -> Option<&str> {
        self.original_file_path.as_deref()
    }

    /// `column_name` — set iff a disabled column-scoped generic test.
    #[must_use]
    pub fn column_name(&self) -> Option<&str> {
        self.column_name.as_deref()
    }

    /// The model a disabled generic test is declared on.
    #[must_use]
    pub fn attached_node(&self) -> Option<&NodeId> {
        self.attached_node.as_ref()
    }

    /// The generic-test descriptor; `None` on disabled singular tests
    /// and non-test entries.
    #[must_use]
    pub fn test_metadata(&self) -> Option<&TestMetadata> {
        self.test_metadata.as_ref()
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
    /// The manifest's top-level `exposures` map (cute-dbt#256), keyed by
    /// the wire map key (`exposure.<package>.<name>`). Defaults to empty
    /// (pre-#256 serializations); `skip_serializing_if` keeps an
    /// exposure-free serialization byte-stable.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    exposures: HashMap<NodeId, Exposure>,
    /// The manifest's top-level `groups` map (cute-dbt#256), keyed by
    /// the wire map key (`group.<package>.<name>`). Same tolerance as
    /// [`Self::exposures`].
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    groups: HashMap<String, Group>,
    /// The manifest's top-level `disabled` map (cute-dbt#258), keyed by
    /// `unique_id` with per-id ARRAYS of entries — dbt's shape is never
    /// 1:1 (multiple disabled definitions can share an id across
    /// versions/packages). [`BTreeMap`] for deterministic order. Same
    /// tolerance + byte-stable omission as [`Self::exposures`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    disabled: BTreeMap<String, Vec<DisabledEntry>>,
    /// The macro reference family (cute-dbt#271): macro `unique_id` →
    /// the `depends_on.macros` list, verbatim wire order (fusion
    /// `MacroDependsOn.macros: Vec<String>`, `dbt-schemas`
    /// `macros.rs:52-57` @ `9977b6cb…`; dbt-core mirrors the shape).
    /// A PARALLEL map beside [`Self::macros`] (id → body string), not a
    /// value-type change — pre-#271 serialized manifests keep
    /// deserializing (payload byte-stability). Only macros with a
    /// non-empty list appear (drop-empty); dispatch indirection is
    /// ALREADY RESOLVED on the wire — both engines record the
    /// adapter-resolved impl (e.g. `macro.dbt.create_table_as` →
    /// `macro.dbt_duckdb.duckdb__create_table_as`), so the closure is
    /// target-adapter-specific. The #262 vars-attribution macro-closure
    /// input and the #265 macro-perspective building block.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    macro_depends_on: BTreeMap<String, Vec<String>>,
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
            exposures: HashMap::new(),
            groups: HashMap::new(),
            disabled: BTreeMap::new(),
            macro_depends_on: BTreeMap::new(),
        }
    }

    /// Attach the manifest's parsed `sources` map (cute-dbt#57).
    #[must_use]
    pub fn with_sources(mut self, sources: HashMap<NodeId, SourceNode>) -> Self {
        self.sources = sources;
        self
    }

    /// Attach the manifest's parsed `exposures` map (cute-dbt#256).
    #[must_use]
    pub fn with_exposures(mut self, exposures: HashMap<NodeId, Exposure>) -> Self {
        self.exposures = exposures;
        self
    }

    /// Attach the manifest's parsed `groups` map (cute-dbt#256).
    #[must_use]
    pub fn with_groups(mut self, groups: HashMap<String, Group>) -> Self {
        self.groups = groups;
        self
    }

    /// Attach the manifest's parsed `disabled` map (cute-dbt#258).
    #[must_use]
    pub fn with_disabled(mut self, disabled: BTreeMap<String, Vec<DisabledEntry>>) -> Self {
        self.disabled = disabled;
        self
    }

    /// Attach the macro reference family (cute-dbt#271) — the adapter
    /// passes only macros with a non-empty `depends_on.macros` list.
    #[must_use]
    pub fn with_macro_depends_on(
        mut self,
        macro_depends_on: BTreeMap<String, Vec<String>>,
    ) -> Self {
        self.macro_depends_on = macro_depends_on;
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

    /// All exposures keyed by id (cute-dbt#256).
    #[must_use]
    pub fn exposures(&self) -> &HashMap<NodeId, Exposure> {
        &self.exposures
    }

    /// All groups keyed by the wire map key (`group.<package>.<name>`)
    /// (cute-dbt#256).
    #[must_use]
    pub fn groups(&self) -> &HashMap<String, Group> {
        &self.groups
    }

    /// The manifest's `disabled` map (cute-dbt#258), keyed by
    /// `unique_id` — per-id arrays of light [`DisabledEntry`]
    /// projections. Empty on every pre-#258 fixture and on projects
    /// with nothing disabled (both engines emit `{}`).
    #[must_use]
    pub fn disabled(&self) -> &BTreeMap<String, Vec<DisabledEntry>> {
        &self.disabled
    }

    /// The macro reference map (cute-dbt#271): macro `unique_id` → its
    /// `depends_on.macros` list, wire order. Empty on pre-#271
    /// serializations; only macros with references appear.
    #[must_use]
    pub fn macro_depends_on(&self) -> &BTreeMap<String, Vec<String>> {
        &self.macro_depends_on
    }

    /// The macro ids a macro references (cute-dbt#271) — the empty
    /// slice for reference-free / unknown ids (the drop-empty store
    /// makes the two indistinguishable, and the closure consumer treats
    /// both as leaves).
    #[must_use]
    pub fn macro_refs(&self, macro_id: &str) -> &[String] {
        self.macro_depends_on
            .get(macro_id)
            .map_or(&[], Vec::as_slice)
    }

    /// Look up a group by its NAME — the value a node's [`Node::group`]
    /// field carries (cute-dbt#256). Linear scan, the
    /// [`Self::source_by_name`] precedent: manifests carry few groups
    /// and the lookup runs per grouped node.
    #[must_use]
    pub fn group_by_name(&self, name: &str) -> Option<&Group> {
        self.groups.values().find(|group| group.name() == name)
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
        assert!(m.macro_depends_on().is_empty());
    }

    // ----- cute-dbt#271: macro reference family ------------------------

    #[test]
    fn manifest_macro_depends_on_builder_lookup_round_trip_and_byte_stable_omission() {
        let bare = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        assert!(bare.macro_depends_on().is_empty());
        assert_eq!(
            bare.macro_refs("macro.dbt.create_table_as"),
            &[] as &[String],
            "an unknown / reference-free macro reads as the empty slice",
        );
        // Byte-stability: a macro-reference-free Manifest serializes
        // without the key (pre-#271 shape).
        assert!(
            !serde_json::to_string(&bare)
                .unwrap()
                .contains("macro_depends_on"),
        );

        let mut macro_depends_on = BTreeMap::new();
        macro_depends_on.insert(
            "macro.dbt.create_table_as".to_owned(),
            vec!["macro.dbt_duckdb.duckdb__create_table_as".to_owned()],
        );
        macro_depends_on.insert(
            "macro.shop.add_dq_flags".to_owned(),
            vec![
                "macro.shop._all_validations_pass".to_owned(),
                "macro.shop._collect_failed_tests".to_owned(),
            ],
        );
        let m = bare.with_macro_depends_on(macro_depends_on);
        assert_eq!(m.macro_depends_on().len(), 2);
        assert_eq!(
            m.macro_refs("macro.shop.add_dq_flags"),
            &[
                "macro.shop._all_validations_pass".to_owned(),
                "macro.shop._collect_failed_tests".to_owned(),
            ],
            "wire order preserved — never sorted or deduplicated",
        );
        let back: Manifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
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
    fn source_node_defaults_column_descriptions_empty_and_builder_attaches() {
        // cute-dbt#235 — constructor leaves the map empty; the builder
        // attaches descriptions (the Node::with_column_descriptions
        // precedent for source(...)-given column tooltips).
        let s = sample_source("source.shop.raw.patients", "raw", "patients");
        assert!(s.column_descriptions().is_empty());
        let mut descs = BTreeMap::new();
        descs.insert("Id".to_owned(), "Unique patient identifier".to_owned());
        let s = s.with_column_descriptions(descs.clone());
        assert_eq!(s.column_descriptions(), &descs);
    }

    #[test]
    fn source_node_column_descriptions_round_trip_and_tolerate_absence() {
        // cute-dbt#235 — a pre-#235 serialization (key absent) must still
        // deserialize (tolerant `#[serde(default)]`), and a populated map
        // must survive the round trip.
        let json = r#"{
            "id": "source.shop.raw.orders",
            "source_name": "raw",
            "name": "orders",
            "schema": "main"
        }"#;
        let s: SourceNode = serde_json::from_str(json).unwrap();
        assert!(s.column_descriptions().is_empty());
        let mut descs = BTreeMap::new();
        descs.insert("order_id".to_owned(), "Raw order key".to_owned());
        let s = s.with_column_descriptions(descs);
        let back: SourceNode = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
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

    // ===== cute-dbt#256 — governance + identity wire family =====

    fn bare_node(id: &str) -> Node {
        Node::new(
            NodeId::new(id),
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
    }

    #[test]
    fn node_new_defaults_identity_governance_and_versions_empty() {
        let n = bare_node("model.shop.bare");
        assert!(n.name().is_none());
        assert!(n.package_name().is_none());
        assert!(n.group().is_none());
        assert!(n.access().is_none());
        assert!(n.version().is_none());
        assert!(n.latest_version().is_none());
        assert!(n.deprecation_date().is_none());
    }

    #[test]
    fn with_identity_sets_name_and_package_name() {
        let n = bare_node("model.shop.dim_customers.v2")
            .with_identity(Some("dim_customers".to_owned()), Some("shop".to_owned()));
        assert_eq!(n.name(), Some("dim_customers"));
        assert_eq!(n.package_name(), Some("shop"));
    }

    #[test]
    fn with_governance_sets_group_and_access() {
        let n = bare_node("model.shop.dim_payers")
            .with_governance(Some("finance".to_owned()), Some("private".to_owned()));
        assert_eq!(n.group(), Some("finance"));
        assert_eq!(n.access(), Some("private"));
    }

    #[test]
    fn with_versions_sets_version_fields() {
        let n = bare_node("model.shop.dim_customers.v2").with_versions(
            Some("2".to_owned()),
            Some("2".to_owned()),
            Some("2027-01-01".to_owned()),
        );
        assert_eq!(n.version(), Some("2"));
        assert_eq!(n.latest_version(), Some("2"));
        assert_eq!(n.deprecation_date(), Some("2027-01-01"));
    }

    #[test]
    fn bare_name_prefers_the_ingested_name_over_the_leaf_segment() {
        // The cute-dbt#254 handoff root fix: a versioned model's leaf
        // segment is the VERSION SUFFIX ("v2"), never the authored name.
        let n = bare_node("model.shop.dim_customers.v2")
            .with_identity(Some("dim_customers".to_owned()), None);
        assert_eq!(n.bare_name(), "dim_customers");
    }

    #[test]
    fn bare_name_falls_back_to_the_leaf_segment_without_a_name() {
        // Pre-#256 fixtures carry no `name` — the old leaf behavior is
        // the documented fallback (including the versioned-id "v2" wart).
        assert_eq!(bare_node("model.shop.dim_payers").bare_name(), "dim_payers");
        assert_eq!(bare_node("model.shop.dim_customers.v2").bare_name(), "v2");
        // Defensive: an empty ingested name never produces an empty bare
        // name — the leaf fallback applies.
        let n = bare_node("model.shop.dim_payers").with_identity(Some(String::new()), None);
        assert_eq!(n.bare_name(), "dim_payers");
    }

    #[test]
    fn node_identity_governance_versions_round_trip_through_serde() {
        let n = bare_node("model.shop.dim_customers.v2")
            .with_identity(Some("dim_customers".to_owned()), Some("shop".to_owned()))
            .with_governance(Some("finance".to_owned()), Some("private".to_owned()))
            .with_versions(Some("2".to_owned()), Some("3".to_owned()), None);
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
        assert_eq!(back.name(), Some("dim_customers"));
        assert_eq!(back.group(), Some("finance"));
        assert_eq!(back.version(), Some("2"));
    }

    #[test]
    fn node_serialization_omits_unset_256_fields_for_payload_byte_stability() {
        // `skip_serializing_if` on every new Option keeps serialized
        // payloads byte-stable: a pre-#256 Node and a post-#256 Node with
        // no governance data serialize identically.
        let value = serde_json::to_value(bare_node("model.shop.x")).unwrap();
        let obj = value.as_object().unwrap();
        for key in [
            "name",
            "package_name",
            "group",
            "access",
            "version",
            "latest_version",
            "deprecation_date",
        ] {
            assert!(!obj.contains_key(key), "unset `{key}` must be omitted");
        }
    }

    #[test]
    fn node_without_256_fields_deserializes_from_pre_256_json() {
        // ADR-5 tolerance: a serialized pre-#256 Node still deserializes,
        // defaulting every new field.
        let json = r#"{
            "id": "model.shop.x",
            "resource_type": "model",
            "checksum": { "name": "sha256", "checksum": "deadbeef" }
        }"#;
        let n: Node = serde_json::from_str(json).unwrap();
        assert!(n.name().is_none());
        assert!(n.package_name().is_none());
        assert!(n.group().is_none());
        assert!(n.access().is_none());
        assert!(n.version().is_none());
        assert!(n.latest_version().is_none());
        assert!(n.deprecation_date().is_none());
    }

    #[test]
    fn manifest_metadata_project_name_accessor_drops_the_empty_string() {
        // fusion defaults an unset `project_name` to `""`
        // (`#[serde(default)] pub project_name: String`, `dbt-schemas`
        // `manifest/manifest.rs:72-73` @ `9977b6cb…`) — the accessor
        // treats it as unset (the #165/#200 drop-empty precedent).
        let m = ManifestMetadata::new("v12");
        assert_eq!(m.project_name(), None);
        let m = m.with_project_name(Some("jaffle_shop".to_owned()));
        assert_eq!(m.project_name(), Some("jaffle_shop"));
        let m = ManifestMetadata::new("v12").with_project_name(Some(String::new()));
        assert_eq!(m.project_name(), None, "empty string is the unset shape");
    }

    #[test]
    fn manifest_metadata_project_name_round_trips_and_tolerates_absence() {
        let m = ManifestMetadata::new("v12").with_project_name(Some("shop".to_owned()));
        let back: ManifestMetadata =
            serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
        // Pre-#256 JSON (no project_name key) still deserializes.
        let back: ManifestMetadata =
            serde_json::from_str(r#"{ "dbt_schema_version": "v12" }"#).unwrap();
        assert_eq!(back.project_name(), None);
        // Wire-shape direct deserialization: fusion's empty default and
        // an explicit null both tolerate.
        let back: ManifestMetadata =
            serde_json::from_str(r#"{ "dbt_schema_version": "v12", "project_name": "" }"#).unwrap();
        assert_eq!(back.project_name(), None);
        let back: ManifestMetadata =
            serde_json::from_str(r#"{ "dbt_schema_version": "v12", "project_name": null }"#)
                .unwrap();
        assert_eq!(back.project_name(), None);
    }

    // ----- Owner / Exposure / Group PODs -----

    #[test]
    fn owner_constructor_and_accessors() {
        let o = Owner::new(
            Some("Finance Team".to_owned()),
            vec!["finance@example.com".to_owned()],
        );
        assert_eq!(o.name(), Some("Finance Team"));
        assert_eq!(o.email(), ["finance@example.com".to_owned()]);
    }

    #[test]
    fn owner_serde_round_trips() {
        let o = Owner::new(Some("Data Team".to_owned()), Vec::new());
        let back: Owner = serde_json::from_str(&serde_json::to_string(&o).unwrap()).unwrap();
        assert_eq!(back, o);
    }

    fn sample_exposure() -> Exposure {
        Exposure::new(
            NodeId::new("exposure.shop.weekly_revenue_dashboard"),
            "weekly_revenue_dashboard",
            Some("dashboard".to_owned()),
            Some("https://bi.example.com/dashboards/revenue".to_owned()),
            Some(Owner::new(
                Some("Data Team".to_owned()),
                vec!["data@example.com".to_owned()],
            )),
            DependsOn::new(Vec::new(), vec![NodeId::new("model.shop.orders")]),
        )
    }

    #[test]
    fn exposure_constructor_and_accessors() {
        let e = sample_exposure();
        assert_eq!(e.id().as_str(), "exposure.shop.weekly_revenue_dashboard");
        assert_eq!(e.name(), "weekly_revenue_dashboard");
        assert_eq!(e.exposure_type(), Some("dashboard"));
        assert_eq!(e.url(), Some("https://bi.example.com/dashboards/revenue"));
        assert_eq!(e.owner().and_then(Owner::name), Some("Data Team"));
        assert_eq!(e.depends_on().nodes(), &[NodeId::new("model.shop.orders")]);
    }

    #[test]
    fn exposure_serde_round_trips() {
        let e = sample_exposure();
        let back: Exposure = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn group_constructor_accessors_and_round_trip() {
        let g = Group::new(
            "finance",
            Some(Owner::new(
                Some("Finance Team".to_owned()),
                vec!["finance@example.com".to_owned()],
            )),
        );
        assert_eq!(g.name(), "finance");
        assert_eq!(g.owner().and_then(Owner::name), Some("Finance Team"));
        let back: Group = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn manifest_with_exposures_and_groups_round_trips() {
        let exposure = sample_exposure();
        let mut exposures = HashMap::new();
        exposures.insert(exposure.id().clone(), exposure);
        let mut groups = HashMap::new();
        groups.insert("group.shop.finance".to_owned(), Group::new("finance", None));
        let m = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
        .with_exposures(exposures)
        .with_groups(groups);
        assert_eq!(m.exposures().len(), 1);
        assert_eq!(m.groups().len(), 1);
        let back: Manifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn manifest_tolerates_missing_exposures_and_groups() {
        // Pre-#256 serialized manifests (and minimal wire JSON) carry
        // neither key — both default to empty.
        let json = r#"{ "metadata": { "dbt_schema_version": "v12" } }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert!(m.exposures().is_empty());
        assert!(m.groups().is_empty());
    }

    #[test]
    fn group_by_name_resolves_a_node_group_reference() {
        // A node's `group` field carries the group NAME, not the
        // `group.<package>.<name>` map key — the lookup joins them.
        let mut groups = HashMap::new();
        groups.insert("group.shop.finance".to_owned(), Group::new("finance", None));
        let m = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
        .with_groups(groups);
        assert!(m.group_by_name("finance").is_some());
        assert!(m.group_by_name("marketing").is_none());
    }

    // ===== cute-dbt#257 — contract + column + structure wire family =====

    #[test]
    fn constraint_constructor_accessors_and_kind_vocabulary() {
        let fk = Constraint::new(
            "foreign_key",
            vec!["payer_key".to_owned()],
            None,
            Some("fk_payer".to_owned()),
            Some("ref('dim_payers')".to_owned()),
            vec!["payer_key".to_owned()],
        );
        assert_eq!(fk.constraint_type(), "foreign_key");
        assert_eq!(fk.kind(), ConstraintKind::ForeignKey);
        assert_eq!(fk.columns(), ["payer_key".to_owned()]);
        assert_eq!(fk.name(), Some("fk_payer"));
        assert_eq!(fk.to(), Some("ref('dim_payers')"));
        assert_eq!(fk.to_columns(), ["payer_key".to_owned()]);
        assert_eq!(fk.expression(), None);

        // The full dbt vocabulary maps; anything else is Other —
        // unknown-tolerant, never an error (exhaustive over the set,
        // the StateComparator test posture).
        for (raw, kind) in [
            ("primary_key", ConstraintKind::PrimaryKey),
            ("foreign_key", ConstraintKind::ForeignKey),
            ("unique", ConstraintKind::Unique),
            ("not_null", ConstraintKind::NotNull),
            ("check", ConstraintKind::Check),
            ("custom", ConstraintKind::Custom),
            ("exotic_future_kind", ConstraintKind::Other),
            ("", ConstraintKind::Other),
        ] {
            let c = Constraint::new(raw, Vec::new(), None, None, None, Vec::new());
            assert_eq!(c.kind(), kind, "raw = {raw:?}");
        }
    }

    #[test]
    fn constraint_deserializes_both_engine_wire_shapes() {
        // dbt-core 1.11 dialect (live-probed): every key present, nulls
        // for unset, `to` RESOLVED to the quoted relation, and the
        // unconsumed warn_* siblings present.
        let core = r#"{
            "type": "foreign_key",
            "name": null,
            "expression": null,
            "warn_unenforced": true,
            "warn_unsupported": true,
            "to": "\"memory\".\"main_marts\".\"dim_payers\"",
            "to_columns": ["payer_key"],
            "columns": ["payer_key"]
        }"#;
        let c: Constraint = serde_json::from_str(core).unwrap();
        assert_eq!(c.kind(), ConstraintKind::ForeignKey);
        assert_eq!(c.to(), Some("\"memory\".\"main_marts\".\"dim_payers\""));
        assert_eq!(c.columns(), ["payer_key".to_owned()]);

        // fusion 2.0-preview dialect (live-probed): `to` stays the
        // AUTHORED ref expression; warn_* are null.
        let fusion = r#"{
            "type": "foreign_key",
            "expression": null,
            "name": null,
            "to": "ref('orders')",
            "to_columns": ["customer_id"],
            "columns": ["customer_id"],
            "warn_unsupported": null,
            "warn_unenforced": null
        }"#;
        let c: Constraint = serde_json::from_str(fusion).unwrap();
        assert_eq!(c.to(), Some("ref('orders')"));

        // Column-level shape: no `columns` key at all (the fusion
        // Constraint vs ModelConstraint type split) + a bare minimum
        // entry tolerates every absent key.
        let column_level = r#"{ "type": "not_null" }"#;
        let c: Constraint = serde_json::from_str(column_level).unwrap();
        assert_eq!(c.kind(), ConstraintKind::NotNull);
        assert!(c.columns().is_empty());
        assert!(c.to().is_none());
        assert!(c.to_columns().is_empty());

        // A missing `type` degrades to Other (tolerant), not an error.
        let degenerate = "{}";
        let c: Constraint = serde_json::from_str(degenerate).unwrap();
        assert_eq!(c.kind(), ConstraintKind::Other);
    }

    #[test]
    fn constraint_serde_round_trips_and_omits_empty_fields() {
        let c = Constraint::new(
            "primary_key",
            vec!["encounter_key".to_owned()],
            None,
            None,
            None,
            Vec::new(),
        );
        let json = serde_json::to_value(&c).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj["type"], "primary_key");
        for key in ["expression", "name", "to", "to_columns"] {
            assert!(!obj.contains_key(key), "unset `{key}` must be omitted");
        }
        let back: Constraint = serde_json::from_value(json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn column_facts_constructor_accessors_and_emptiness() {
        let facts = ColumnFacts::new(
            Some(serde_json::json!({ "pii": false, "owner": "clinical-quality" })),
            vec!["dimension_key".to_owned()],
            Vec::new(),
            vec![Constraint::new(
                "not_null",
                Vec::new(),
                None,
                None,
                None,
                Vec::new(),
            )],
        );
        assert_eq!(
            facts.meta().and_then(|m| m.get("owner")),
            Some(&serde_json::json!("clinical-quality"))
        );
        assert_eq!(facts.tags(), ["dimension_key".to_owned()]);
        assert!(facts.policy_tags().is_empty());
        assert_eq!(facts.constraints().len(), 1);
        assert!(!facts.is_empty());
        assert!(
            ColumnFacts::new(None, Vec::new(), Vec::new(), Vec::new()).is_empty(),
            "a fact-free entry is empty — the adapter never stores one"
        );
    }

    #[test]
    fn column_facts_serde_round_trips() {
        let facts = ColumnFacts::new(
            Some(serde_json::json!({ "pii": true })),
            vec!["governed".to_owned()],
            vec!["projects/example/locations/us/taxonomies/1/policyTags/2".to_owned()],
            vec![Constraint::new(
                "unique",
                Vec::new(),
                None,
                None,
                None,
                Vec::new(),
            )],
        );
        let back: ColumnFacts =
            serde_json::from_str(&serde_json::to_string(&facts).unwrap()).unwrap();
        assert_eq!(back, facts);
    }

    #[test]
    fn node_new_defaults_structure_and_contract_fields_empty() {
        let n = bare_node("model.shop.bare");
        assert!(n.fqn().is_empty());
        assert!(n.constraints().is_empty());
        assert!(n.primary_key().is_empty());
        assert!(n.contract_checksum().is_none());
        assert!(n.column_facts().is_empty());
    }

    #[test]
    fn node_structure_and_contract_builders_set_fields() {
        let mut facts = BTreeMap::new();
        facts.insert(
            "payer_key".to_owned(),
            ColumnFacts::new(
                None,
                vec!["dimension_key".to_owned()],
                Vec::new(),
                Vec::new(),
            ),
        );
        let n = bare_node("model.shop.fct_encounters")
            .with_fqn(vec![
                "shop".to_owned(),
                "marts".to_owned(),
                "fct_encounters".to_owned(),
            ])
            .with_contract_facts(
                vec![Constraint::new(
                    "primary_key",
                    vec!["encounter_key".to_owned()],
                    None,
                    None,
                    None,
                    Vec::new(),
                )],
                vec!["encounter_key".to_owned()],
                Some("0cb79927".to_owned()),
            )
            .with_column_facts(facts);
        assert_eq!(n.fqn().len(), 3);
        assert_eq!(n.fqn()[1], "marts");
        assert_eq!(n.constraints()[0].kind(), ConstraintKind::PrimaryKey);
        assert_eq!(n.primary_key(), ["encounter_key".to_owned()]);
        assert_eq!(n.contract_checksum(), Some("0cb79927"));
        assert_eq!(
            n.column_facts()["payer_key"].tags(),
            ["dimension_key".to_owned()]
        );
    }

    #[test]
    fn node_structure_fields_round_trip_through_serde() {
        let n = bare_node("model.shop.x")
            .with_fqn(vec!["shop".to_owned(), "x".to_owned()])
            .with_contract_facts(
                vec![Constraint::new(
                    "check",
                    Vec::new(),
                    Some("id > 0".to_owned()),
                    None,
                    None,
                    Vec::new(),
                )],
                vec!["id".to_owned()],
                None,
            );
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
        assert_eq!(back.constraints()[0].expression(), Some("id > 0"));
    }

    #[test]
    fn node_serialization_omits_unset_257_fields_for_payload_byte_stability() {
        let value = serde_json::to_value(bare_node("model.shop.x")).unwrap();
        let obj = value.as_object().unwrap();
        for key in [
            "fqn",
            "constraints",
            "primary_key",
            "contract_checksum",
            "column_facts",
        ] {
            assert!(!obj.contains_key(key), "unset `{key}` must be omitted");
        }
    }

    #[test]
    fn node_without_257_fields_deserializes_from_pre_257_json() {
        let json = r#"{
            "id": "model.shop.x",
            "resource_type": "model",
            "checksum": { "name": "sha256", "checksum": "deadbeef" }
        }"#;
        let n: Node = serde_json::from_str(json).unwrap();
        assert!(n.fqn().is_empty());
        assert!(n.constraints().is_empty());
        assert!(n.primary_key().is_empty());
        assert!(n.contract_checksum().is_none());
        assert!(n.column_facts().is_empty());
    }

    // ----- cute-dbt#258: test-config semantics (typed dict reads) ----

    fn config_with(key: &str, value: Value) -> NodeConfig {
        let mut map = BTreeMap::new();
        map.insert(key.to_owned(), value);
        NodeConfig::new(map, false)
    }

    #[test]
    fn severity_reads_case_insensitively_with_tolerant_fallback() {
        // Real wire carries three case variants: dbt-core default
        // "ERROR", dbt-core authored "warn", fusion "Warn"
        // (live-probed 2026-06-12).
        for raw in ["ERROR", "error", "Error"] {
            assert_eq!(
                config_with("severity", Value::String(raw.to_owned())).severity(),
                Some(TestSeverity::Error),
                "{raw}",
            );
        }
        for raw in ["warn", "Warn", "WARN"] {
            assert_eq!(
                config_with("severity", Value::String(raw.to_owned())).severity(),
                Some(TestSeverity::Warn),
                "{raw}",
            );
        }
        assert_eq!(
            config_with("severity", Value::String("fatal".to_owned())).severity(),
            Some(TestSeverity::Unrecognized),
        );
        assert_eq!(
            config_with("severity", Value::from(42)).severity(),
            Some(TestSeverity::Unrecognized),
        );
        assert_eq!(config_with("severity", Value::Null).severity(), None);
        assert_eq!(NodeConfig::default().severity(), None);
    }

    #[test]
    fn test_config_semantics_read_where_limit_enabled_store_failures() {
        let c = config_with("where", Value::String("payer_key != -1".to_owned()));
        assert_eq!(c.where_filter(), Some("payer_key != -1"));
        assert_eq!(config_with("where", Value::Null).where_filter(), None);
        assert_eq!(NodeConfig::default().where_filter(), None);

        assert_eq!(config_with("limit", Value::from(50)).limit(), Some(50));
        assert_eq!(config_with("limit", Value::Null).limit(), None);
        assert_eq!(
            config_with("limit", Value::String("50".to_owned())).limit(),
            None,
            "a non-integer limit degrades to None (ADR-5), never panics",
        );

        assert_eq!(
            config_with("enabled", Value::Bool(true)).enabled(),
            Some(true)
        );
        assert_eq!(NodeConfig::default().enabled(), None);

        assert_eq!(
            config_with("store_failures", Value::Bool(true)).store_failures(),
            Some(true),
        );
        assert_eq!(
            config_with("store_failures", Value::Null).store_failures(),
            None,
            "both committed fixtures null-fill store_failures when unset",
        );
    }

    // ----- cute-dbt#258: singular-test discrimination -----------------

    #[test]
    fn is_singular_test_discriminates_test_nodes() {
        let base = |resource_type: &str| {
            Node::new(
                NodeId::new("test.shop.assert_x"),
                resource_type,
                sample_checksum(),
                None,
                None,
                DependsOn::default(),
                None,
                NodeConfig::default(),
                None,
                BTreeMap::new(),
            )
        };
        // A test node WITHOUT test_metadata is a singular (SQL-file)
        // test — both engines omit the key (live-probed 2026-06-12).
        assert!(base("test").is_singular_test());
        // A generic test carries test_metadata.
        assert!(
            !base("test")
                .with_test_attachment(
                    None,
                    Some(NodeId::new("model.shop.x")),
                    Some(TestMetadata::new("unique", None, Value::Null)),
                )
                .is_singular_test()
        );
        // Non-test nodes are never singular tests.
        assert!(!base("model").is_singular_test());
    }

    // ----- cute-dbt#258: unrendered_config -----------------------------

    #[test]
    fn node_unrendered_config_round_trips_and_omits_when_empty() {
        let bare = Node::new(
            NodeId::new("model.shop.m"),
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
        // Byte-stability: an unrendered-config-free Node serializes
        // without the key (pre-#258 shape).
        let json = serde_json::to_string(&bare).unwrap();
        assert!(!json.contains("unrendered_config"));
        assert!(bare.unrendered_config().is_empty());

        let mut authored = BTreeMap::new();
        authored.insert("materialized".to_owned(), Value::String("view".to_owned()));
        authored.insert(
            "docs".to_owned(),
            serde_json::json!({ "node_color": "gold" }),
        );
        let n = bare.with_unrendered_config(authored.clone());
        assert_eq!(n.unrendered_config(), &authored);
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
    }

    // ----- cute-dbt#258: disabled map ----------------------------------

    #[test]
    fn disabled_entry_construction_and_linkage_accessors() {
        // A disabled GENERIC test keeps its attachment (live-probed on
        // both engines 2026-06-12) — the coverage-truthfulness linkage.
        let generic = DisabledEntry::new("test")
            .with_name(Some("accepted_values_dim_payers_payer_type".to_owned()))
            .with_original_file_path(Some("models/marts/core/_core.yml".to_owned()))
            .with_attachment(
                Some("payer_type".to_owned()),
                Some(NodeId::new("model.shop.dim_payers")),
                Some(TestMetadata::new("accepted_values", None, Value::Null)),
            );
        assert_eq!(generic.resource_type(), "test");
        assert_eq!(
            generic.name(),
            Some("accepted_values_dim_payers_payer_type"),
        );
        assert_eq!(
            generic.original_file_path(),
            Some("models/marts/core/_core.yml"),
        );
        assert_eq!(generic.column_name(), Some("payer_type"));
        assert_eq!(
            generic.attached_node().map(NodeId::as_str),
            Some("model.shop.dim_payers"),
        );
        assert_eq!(
            generic.test_metadata().map(TestMetadata::name),
            Some("accepted_values")
        );

        // A disabled MODEL (or singular test) carries no attachment —
        // honest None, never invented linkage.
        let model = DisabledEntry::new("model");
        assert_eq!(model.resource_type(), "model");
        assert_eq!(model.name(), None);
        assert_eq!(model.attached_node(), None);
        assert_eq!(model.test_metadata(), None);

        let back: DisabledEntry =
            serde_json::from_str(&serde_json::to_string(&generic).unwrap()).unwrap();
        assert_eq!(back, generic);
    }

    #[test]
    fn manifest_disabled_default_empty_with_builder_and_byte_stable_omission() {
        let bare = Manifest::new(
            ManifestMetadata::new("v12"),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        assert!(bare.disabled().is_empty());
        // Byte-stability: a disabled-free Manifest serializes without
        // the key (pre-#258 shape).
        assert!(
            !serde_json::to_string(&bare)
                .unwrap()
                .contains("\"disabled\"")
        );

        let mut disabled = BTreeMap::new();
        disabled.insert(
            "model.shop.archived".to_owned(),
            vec![DisabledEntry::new("model"), DisabledEntry::new("model")],
        );
        let m = bare.with_disabled(disabled);
        assert_eq!(
            m.disabled()["model.shop.archived"].len(),
            2,
            "per-id ARRAYS preserved — dbt's shape is never 1:1",
        );
        let back: Manifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
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
