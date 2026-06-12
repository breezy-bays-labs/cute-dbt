//! Manifest ingestion adapter — dbt `manifest.json` (schema v12) → the
//! domain [`Manifest`].
//!
//! This is the **Stage-1 pre-flight** boundary of ADR-2's two-stage
//! fail-closed contract. The wire JSON is deserialized through tolerant
//! `Wire*` structs (`#[serde(default)]` on optionals, **no
//! `deny_unknown_fields`** — dbt adds sibling keys every minor and
//! cute-dbt must fail closed only on *missing compiled SQL*, never on
//! dbt evolution; ADR-5), the `dbt_schema_version` floor is checked, and
//! the result is translated into the post-normalized domain shape.
//!
//! ## Wire vs domain
//!
//! The domain types (PR 3) are the *post-normalized* shape. Two wire
//! quirks are absorbed here so no quirk leaks past this file:
//!
//! - **Node identity.** dbt keys the `nodes` map by `unique_id`; the
//!   node object itself carries `unique_id`, not `id`. [`Node`] wants an
//!   `id`, so the authoritative map key is folded into each node during
//!   translation — the `WireNode` projection therefore has no id field.
//! - **Macros.** dbt's `macros` map values are macro *objects*; the
//!   domain stores the macro *body string*. The `WireMacro` projection
//!   keeps only `macro_sql`.
//!
//! Every other consumed type ([`ManifestMetadata`], [`Checksum`],
//! [`DependsOn`], [`UnitTest`] and its `given` / `expect`) already
//! deserializes from the wire shape unchanged — PR 3 designed the domain
//! types as the post-normalized shape — so the `Wire*` set is
//! deliberately minimal.
//!
//! ## Container shape
//!
//! PR 4a (#5) confirmed **shape A** against the real jaffle-shop
//! fixture: unit tests live in a top-level `unit_tests` map, not
//! embedded in `nodes`. The serde layout commits to that shape; the
//! embedded-in-`nodes` shape is not produced by dbt ≥1.8 and is not
//! handled.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::domain::{
    Checksum, ColumnFacts, Constraint, DependsOn, Exposure, Group, Manifest, ManifestMetadata,
    Node, NodeConfig, NodeId, Owner, PreflightError, SourceNode, TestMetadata, UnitTest,
    UnitTestExpect, UnitTestGiven, UnitTestOverrides,
};
use crate::ports::ManifestSource;
use serde_json::Value;

/// Minimum supported dbt manifest schema major version. Schema v12 is
/// the dbt 1.8 era — the floor at which unit tests went GA. dbt 1.8
/// through 1.11+ all still emit schema v12.
const SUPPORTED_SCHEMA_FLOOR: u32 = 12;

/// Human-facing label for [`SUPPORTED_SCHEMA_FLOOR`], passed verbatim
/// into [`PreflightError::SchemaUnsupported`]'s `minimum` field. A unit
/// test asserts it stays in sync with the numeric floor so the message
/// and the check cannot drift apart.
const SUPPORTED_SCHEMA_MIN_LABEL: &str = "v12";

// ---------------------------------------------------------------------
// Wire structs — the tolerant projection of dbt `manifest.json` v12.
// ---------------------------------------------------------------------

/// Tolerant wire projection of a dbt `manifest.json`.
///
/// Only `metadata` is structurally required; `nodes` / `unit_tests` /
/// `macros` default to empty so a degenerate-but-valid manifest still
/// deserializes (ADR-5 tolerance). No `deny_unknown_fields`.
#[derive(Debug, Deserialize)]
struct WireManifest {
    metadata: ManifestMetadata,
    #[serde(default)]
    nodes: HashMap<String, WireNode>,
    #[serde(default)]
    unit_tests: HashMap<String, WireUnitTest>,
    #[serde(default)]
    macros: HashMap<String, WireMacro>,
    #[serde(default)]
    sources: HashMap<String, WireSource>,
    #[serde(default)]
    exposures: HashMap<String, WireExposure>,
    #[serde(default)]
    groups: HashMap<String, WireGroup>,
}

/// Wire projection of one `nodes` entry.
///
/// No `id` field: dbt keys the `nodes` map by `unique_id`, and the map
/// key is the authoritative identity that [`WireManifest::into_domain`]
/// folds into [`Node`].
///
/// `original_file_path` is a top-level dbt-emitted field
/// (e.g. `models/marts/core/dim_payers.sql`); ADR-5-tolerant default to
/// `None` for older or synthetic manifests.
///
/// `config`, `relation_name`, and `columns` are the v0.2 `state:modified`
/// sub-selector inputs (cute-dbt#17). The nested wire `config` block is
/// lifted into the domain [`NodeConfig`] by [`WireNodeConfig::into_domain`]
/// (the config dict passes through; `config.contract.enforced` is hoisted
/// to a flat bool). `columns` is the model's column map; only each
/// column's declared `data_type` is consumed (the contract column-set
/// diff). All ADR-5-tolerant: each defaults so older / synthetic
/// manifests still deserialize.
#[derive(Debug, Deserialize)]
struct WireNode {
    resource_type: String,
    /// `Option<WireChecksum>` since cute-dbt#256 — the checksum-cliff
    /// hardening. A required [`Checksum`] struct made one checksum-less
    /// (or bare-string-checksum) node fail the WHOLE manifest parse, the
    /// exact ADR-5 violation the issue names. Every shape now degrades
    /// per node via [`fold_checksum`]; the dbt-faithful sentinel and the
    /// comparison semantics are documented there.
    #[serde(default)]
    checksum: Option<WireChecksum>,
    #[serde(default)]
    compiled_code: Option<String>,
    #[serde(default)]
    raw_code: Option<String>,
    #[serde(default)]
    depends_on: DependsOn,
    #[serde(default)]
    original_file_path: Option<String>,
    #[serde(default)]
    config: WireNodeConfig,
    #[serde(default)]
    relation_name: Option<String>,
    #[serde(default)]
    columns: BTreeMap<String, WireColumn>,
    /// Test-node attribution (cute-dbt#165): `column_name` is set iff the
    /// test is column-scoped; `attached_node` is the declaring model;
    /// `test_metadata` is the generic-test descriptor (absent on singular
    /// SQL-file tests — fusion `#[skip_serializing_none]`s the key away).
    /// The domain [`TestMetadata`] already deserializes the wire shape
    /// verbatim (`name` + defaulted `namespace`/`kwargs`), so no `Wire*`
    /// twin is needed — the [`Checksum`]/[`DependsOn`] reuse precedent.
    #[serde(default)]
    column_name: Option<String>,
    #[serde(default)]
    attached_node: Option<NodeId>,
    #[serde(default)]
    test_metadata: Option<TestMetadata>,
    /// Authored model description (cute-dbt#200) — a top-level wire
    /// field (fusion `ManifestMaterializableCommonAttributes.description`,
    /// `dbt-schemas` `manifest_nodes.rs` @ `9977b6cb…`). fusion omits an
    /// unset description (`#[skip_serializing_none]`); dbt-core emits
    /// `""`. The translation drops the empty-string shape (the
    /// cute-dbt#165 column-description precedent) so the domain carries
    /// only real prose.
    #[serde(default)]
    description: Option<String>,
    /// Resolved model tags (cute-dbt#200) — the top-level wire list,
    /// which is the authoritative DEDUPLICATED set. The nested
    /// `config.tags` is deliberately not read: real dbt-core manifests
    /// carry project-level + model-level merge duplicates there
    /// (verified on the committed playground fixture, e.g.
    /// `dim_conditions` `config.tags` = the same three tags twice).
    /// `Option` (not a bare `Vec`) for the #145 null-fill tolerance.
    #[serde(default)]
    tags: Option<Vec<String>>,
    /// Schema-properties YAML path (cute-dbt#105) — a top-level wire
    /// field both engines serialize as a **package URI**
    /// (`<package>://models/schema.yml`): fusion's
    /// `normalize_manifest_patch_path` / `package_uri_path`
    /// (`dbt-schemas` `manifest/manifest.rs` @ `9977b6cb…`) mirrors
    /// dbt-core's emission, verified on both committed fixtures
    /// (jaffle-shop = dbt-core 1.11, playground = fusion). The
    /// translation strips the scheme ([`strip_package_uri_scheme`]) so
    /// the domain carries a plain relative path. ADR-5-tolerant:
    /// dbt-core null-fills unpatched nodes (`"patch_path": null`),
    /// fusion omits the key — `#[serde(default)]` + `Option` covers
    /// both.
    #[serde(default)]
    patch_path: Option<String>,
    /// Identity (cute-dbt#256): the node's authored bare name + owning
    /// package (fusion `ManifestCommonAttributes.name`/`package_name`,
    /// `dbt-schemas` `manifest/manifest_nodes.rs:84-88` @ `9977b6cb…`).
    /// Always populated on both engines' real wire (verified on the
    /// committed jaffle-shop + playground fixtures); empty strings are
    /// dropped in translation so [`Node::bare_name`]'s leaf fallback
    /// stays sound.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    package_name: Option<String>,
    /// Governance (cute-dbt#256): the model's group NAME + access level
    /// (fusion `ManifestModel.group: Option<String>` / `access:
    /// Option<Access>`, `manifest_nodes.rs:782+` @ `9977b6cb…`). Both
    /// committed real fixtures emit `"group": null` + `"access":
    /// "protected"` on every model (the null-fill shape); a live fusion
    /// 2.0-preview.177 compile emits populated values and omits an
    /// unset `group` entirely. `access` stays a tolerant string —
    /// unknown future levels must never fail ingestion (ADR-5).
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    access: Option<String>,
    /// Versioning (cute-dbt#256, deferred from #254): fusion
    /// `StringOrInteger` on the wire (`dbt-schemas` `serde.rs:419-422`
    /// @ `9977b6cb…`) — a live fusion compile emits the bare integer
    /// `2`, so the untyped [`Value`] is normalized by
    /// [`version_string`]. Both committed fixtures null-fill all three
    /// on every (unversioned) model.
    #[serde(default)]
    version: Option<Value>,
    #[serde(default)]
    latest_version: Option<Value>,
    #[serde(default)]
    deprecation_date: Option<String>,
    /// Structure (cute-dbt#257): the engine-built fully-qualified name
    /// path (fusion `get_node_fqn`, `dbt-parser` `utils.rs:132-159` @
    /// `9977b6cb…`) — the #262 C2 config-tree prefix-matcher input.
    /// `Option` for the #145 null-fill tolerance; both committed real
    /// fixtures populate it on every node.
    #[serde(default)]
    fqn: Option<Vec<String>>,
    /// Contract family (cute-dbt#257): model-level declared
    /// `constraints` (fusion `ModelConstraint`,
    /// `properties/model_properties.rs:31-51` @ `9977b6cb…` — the
    /// domain [`Constraint`] deserializes the wire verbatim, the
    /// `Checksum`/`DependsOn` reuse precedent), the engine-inferred
    /// `primary_key` (`ManifestModel.primary_key`), and the TOP-LEVEL
    /// `contract` block whose `checksum` is hoisted flat. Both
    /// committed fixtures emit `constraints: []` / populated
    /// `primary_key` / `contract.checksum: null`.
    #[serde(default)]
    constraints: Option<Vec<Constraint>>,
    #[serde(default)]
    primary_key: Option<Vec<String>>,
    #[serde(default)]
    contract: Option<WireContract>,
}

/// Tolerant wire projection of a node's TOP-LEVEL `contract` block
/// (cute-dbt#257) — distinct from the `config.contract` sub-object the
/// `.contract` sub-selector hoists `enforced` from. Only `checksum` is
/// consumed; fusion types it `Option<YmlValue>` (`DbtContract`,
/// `dbt-schemas` `common.rs:762-770` @ `9977b6cb…`), so a non-string
/// value degrades to `None` (ADR-5). dbt-core emits a hex string when
/// the contract is enforced and `null` otherwise; fusion 2.0-preview
/// omits the key even when enforced (live-verified 2026-06-11).
#[derive(Debug, Default, Deserialize)]
struct WireContract {
    #[serde(default)]
    checksum: Option<Value>,
}

impl WireContract {
    /// The contract checksum when the wire carried a string; any other
    /// shape (null, absent, defensive non-string) is `None`.
    fn checksum_string(self) -> Option<String> {
        match self.checksum {
            Some(Value::String(s)) if !s.is_empty() => Some(s),
            _ => None,
        }
    }
}

/// Strip the `<package>://` URI scheme off a wire `patch_path`,
/// yielding the package-relative file path (`jaffle_shop://models/
/// schema.yml` → `models/schema.yml`). A path without a scheme (a
/// synthetic fixture or a domain round-trip) passes through verbatim —
/// the inverse of fusion's `package_uri_path`, which likewise leaves
/// already-schemed paths alone.
fn strip_package_uri_scheme(path: String) -> String {
    match path.split_once("://") {
        Some((_, rest)) => rest.to_owned(),
        None => path,
    }
}

/// Tolerant wire projection of a node's `checksum` (cute-dbt#256).
///
/// Mirrors fusion's `DbtChecksum` untagged enum (`dbt-schemas`
/// `common.rs:929+` @ `9977b6cb…`): the wire value is the familiar
/// `{name, checksum}` object **or** a bare hash string. The trailing
/// [`Self::Other`] arm absorbs any other shape so one malformed checksum
/// can never fail the whole manifest parse (ADR-5) — it degrades per
/// node to the sentinel in [`fold_checksum`]. The object arm's fields
/// default so a partial object keeps whatever it carried.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WireChecksum {
    Object {
        #[serde(default)]
        name: String,
        #[serde(default)]
        checksum: String,
    },
    Bare(String),
    /// Catch-all: any other JSON shape is accepted and discarded
    /// ([`serde::de::IgnoredAny`] — the payload is never read; the fold
    /// substitutes the sentinel).
    Other(serde::de::IgnoredAny),
}

/// Fold the tolerant wire checksum into the domain [`Checksum`]
/// (cute-dbt#256 checksum-cliff hardening).
///
/// Absent / null / unrecognizable shapes become dbt's own no-checksum
/// sentinel — `FileHash.empty()` = `{name: "none", checksum: ""}`
/// (fusion `DbtChecksum::default`; the REAL wire value on every
/// generic-test node in both committed fixtures). A bare string keeps
/// the hex with no algorithm name (fusion's `DbtChecksum::String` arm).
///
/// Comparison semantics stay verbatim in `BodyChecksumModifier`,
/// matching dbt exactly (fusion `check_modified_content`: identical
/// checksums ⇒ unchanged): sentinel == sentinel deliberately compares
/// EQUAL (fusion documents that a divergent sentinel name made every
/// generic test appear state:modified on every run), while sentinel vs
/// real DIFFERS — a unit-test target model whose checksum vanishes from
/// one side is conservatively modified ⇒ in scope (fail-closed for
/// in-scope unit-test targets, tolerant passthrough for everything
/// else).
fn fold_checksum(wire: Option<WireChecksum>) -> Checksum {
    match wire {
        Some(WireChecksum::Object { name, checksum }) => Checksum::new(name, checksum),
        Some(WireChecksum::Bare(hex)) => Checksum::new("", hex),
        Some(WireChecksum::Other(_)) | None => Checksum::new("none", ""),
    }
}

/// Normalize a wire `version` / `latest_version` value (cute-dbt#256).
///
/// The wire is fusion `StringOrInteger` (`dbt-schemas` `serde.rs:419-422`
/// @ `9977b6cb…`): a live fusion 2.0-preview compile emits the bare
/// integer `2`, an authored string version passes verbatim. Integers
/// render in decimal; fusion's empty-string default and every other
/// shape (float, bool, object — not versions) degrade to `None`, never
/// an error (ADR-5).
fn version_string(value: Option<Value>) -> Option<String> {
    match value? {
        Value::String(s) if !s.is_empty() => Some(s),
        Value::Number(n) if n.is_i64() || n.is_u64() => Some(n.to_string()),
        _ => None,
    }
}

/// Tolerant wire projection of a `DbtOwner` block (cute-dbt#256) —
/// carried by groups and exposures. `email` is fusion
/// `Option<StringOrArrayOfStrings>` (`dbt-schemas`
/// `manifest/common.rs:39-44` @ `9977b6cb…`; dbt-core emits a single
/// string), kept untyped here and normalized in [`Self::into_domain`].
/// fusion `#[serialize_always]`s `name`, so `{"name": null}` is a real
/// wire shape.
#[derive(Debug, Default, Deserialize)]
struct WireOwner {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<Value>,
}

impl WireOwner {
    /// Normalize into the domain [`Owner`]: a string email becomes a
    /// one-element list, an array keeps its string elements, empty
    /// strings drop everywhere, and an owner with no remaining content
    /// collapses to `None` (the `WireUnitTestOverrides::into_grouped`
    /// no-effective-content precedent).
    fn into_domain(self) -> Option<Owner> {
        let email: Vec<String> = match self.email {
            Some(Value::String(s)) if !s.is_empty() => vec![s],
            Some(Value::Array(items)) => items
                .into_iter()
                .filter_map(|item| match item {
                    Value::String(s) if !s.is_empty() => Some(s),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let name = self.name.filter(|n| !n.is_empty());
        (name.is_some() || !email.is_empty()).then(|| Owner::new(name, email))
    }
}

/// Wire projection of one top-level `exposures` entry (cute-dbt#256).
///
/// Like [`WireNode`]/[`WireSource`], no id field — the map key
/// (`exposure.<package>.<name>`) is folded into the domain
/// [`Exposure::id`]. Only the consumed subset is projected (fusion
/// `ManifestExposure`, `dbt-schemas` `manifest/manifest_nodes.rs:1526+`
/// @ `9977b6cb…`); `label` / `maturity` / `config` / `refs` and the
/// `depends_on.nodes_with_ref_location` sibling pass through untouched
/// (no `deny_unknown_fields`, ADR-5). Every field defaults — one
/// malformed exposure entry must never fail the manifest.
#[derive(Debug, Deserialize)]
struct WireExposure {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "type")]
    exposure_type: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    owner: Option<WireOwner>,
    #[serde(default)]
    depends_on: DependsOn,
}

impl WireExposure {
    /// Translate into the domain [`Exposure`], folding the authoritative
    /// map `key` into the id (the [`WireNode`] precedent). A missing
    /// `name` degrades to `""` — fail-open, the [`WireSource`] posture.
    fn into_domain(self, key: String) -> Exposure {
        Exposure::new(
            NodeId::new(key),
            self.name.unwrap_or_default(),
            self.exposure_type,
            self.url,
            self.owner.and_then(WireOwner::into_domain),
            self.depends_on,
        )
    }
}

/// Wire projection of one top-level `groups` entry (cute-dbt#256).
///
/// fusion requires `owner:` at parse on an authored group
/// (`GroupProperties.owner: DbtOwner`, no default — `dbt-schemas`
/// `properties/properties.rs:120-125` @ `9977b6cb…`; live-verified on
/// 2.0-preview.177: `dbt1013 missing field 'owner'`), so real fusion
/// manifests always carry an owner OBJECT — but both of its fields are
/// optional, so the content may still be empty. cute-dbt tolerates
/// every shape (ADR-5) and collapses a content-free owner to `None`.
#[derive(Debug, Deserialize)]
struct WireGroup {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    owner: Option<WireOwner>,
}

impl WireGroup {
    /// Translate into the domain [`Group`]. A missing `name` degrades to
    /// `""` — it can never match a node's `group` reference (fail-open).
    fn into_domain(self) -> Group {
        Group::new(
            self.name.unwrap_or_default(),
            self.owner.and_then(WireOwner::into_domain),
        )
    }
}

/// Tolerant wire projection of a node's `config` sub-object.
///
/// The **whole** config dict is captured verbatim (`#[serde(flatten)]`
/// into a `BTreeMap<String, Value>`) so `.configs` diffs the complete key
/// set + value set dbt emitted, the nested `contract` sub-object included.
/// `config.contract.enforced` is then read back out of the captured dict
/// and hoisted to a flat bool for `.contract`. No `deny_unknown_fields`
/// (ADR-5).
#[derive(Debug, Default, Deserialize)]
struct WireNodeConfig {
    #[serde(flatten)]
    config: BTreeMap<String, Value>,
}

/// Tolerant wire projection of one `columns` map entry — the declared
/// `data_type` feeds the `.contract` column-set diff, and the authored
/// `description` feeds the column-header tooltips (cute-dbt#165). fusion
/// always serializes `description` (an unset one as `""` — `dbt-schemas`
/// `dbt_column.rs` `serialize_dbt_column_desc`); the translation drops
/// empty descriptions so the domain map carries only real prose.
#[derive(Debug, Default, Deserialize)]
struct WireColumn {
    #[serde(default)]
    data_type: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// The cute-dbt#257 column-level extension (fusion `DbtColumn`,
    /// `dbt-schemas` `dbt_column.rs:38-60` @ `9977b6cb…`): authored
    /// `meta` (core emits `{}` when unset), resolved top-level `tags`
    /// (the nested `config` mirror is deliberately not read — the #200
    /// precedent), BigQuery `policy_tags` (fusion-only first-class
    /// field; core never serializes the key), and declared
    /// `constraints`. All fold into [`ColumnFacts`] via
    /// [`Self::into_facts`]; fact-free columns store nothing.
    #[serde(default)]
    meta: Option<Value>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    policy_tags: Option<Vec<String>>,
    #[serde(default)]
    constraints: Option<Vec<Constraint>>,
}

/// Fold a wire column's #257 extension into a [`ColumnFacts`], dropping
/// every unset shape (absent key, engine null-fill, core's empty `{}` /
/// `[]`); a fact-free column yields `None` so the domain map keeps only
/// real facts (the column_descriptions precedent). A non-object `meta`
/// passes through verbatim (the untyped-passthrough posture of
/// `config.meta`).
fn fold_column_facts(
    meta: Option<Value>,
    tags: Option<Vec<String>>,
    policy_tags: Option<Vec<String>>,
    constraints: Option<Vec<Constraint>>,
) -> Option<ColumnFacts> {
    let meta = meta.filter(|m| m.as_object().is_none_or(|obj| !obj.is_empty()));
    let facts = ColumnFacts::new(
        meta,
        tags.unwrap_or_default(),
        policy_tags.unwrap_or_default(),
        constraints.unwrap_or_default(),
    );
    (!facts.is_empty()).then_some(facts)
}

impl WireNodeConfig {
    /// Fold the captured config dict into the domain [`NodeConfig`],
    /// reading `config.contract.enforced` out of the dict and hoisting it
    /// to the flat `contract_enforced` bool.
    ///
    /// The dict itself is kept whole — `.configs` sees the complete
    /// config block dbt emitted (the full `contract` sub-object included),
    /// not a lossy reconstruction.
    fn into_domain(self) -> NodeConfig {
        let enforced = self
            .config
            .get("contract")
            .and_then(|c| c.get("enforced"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        NodeConfig::new(self.config, enforced)
    }
}

/// Wire projection of one `macros` entry — only the body is consumed.
#[derive(Debug, Deserialize)]
struct WireMacro {
    macro_sql: String,
}

/// Wire projection of one top-level `sources` entry (cute-dbt#57).
///
/// Like [`WireNode`], no `id` field — dbt keys the `sources` map by
/// `unique_id` (`source.<package>.<source_name>.<name>`) and the map key
/// is folded into the domain [`SourceNode`] during translation.
///
/// **Every** field is `#[serde(default)] Option<…>` — the cute-dbt#145
/// engine-divergence rule applied verbatim: dbt-core emits explicit
/// `null` for unset fields (an `Option` is required; a bare `String`
/// rejects `null`), while fusion's `#[skip_serializing_none]` omits the
/// keys entirely (`default` covers absence). One malformed source entry
/// must never fail the whole manifest parse (ADR-5); a source whose
/// `source_name` / `name` defaults to `""` simply never matches a parsed
/// `source('a', 'b')` given — the same fail-open posture as an
/// unresolvable `ref(...)`.
#[derive(Debug, Deserialize)]
struct WireSource {
    #[serde(default)]
    source_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    identifier: Option<String>,
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    relation_name: Option<String>,
    /// Source `columns` map (cute-dbt#235) — the same wire shape as a
    /// node's (fusion `ManifestSource.columns` serializes through the
    /// shared `serialize_dbt_columns`, `dbt-schemas`
    /// `manifest/manifest_nodes.rs` @ `9977b6cb…`), so [`WireColumn`] is
    /// reused verbatim. Feeds the `source(...)`-given column-header
    /// tooltips; only `description` is consumed.
    #[serde(default)]
    columns: BTreeMap<String, WireColumn>,
}

impl WireSource {
    /// Translate into the domain [`SourceNode`], folding the
    /// authoritative map `key` into the id (the [`WireNode`] precedent)
    /// and keeping only non-empty column descriptions (the cute-dbt#165
    /// empty-string-unset drop, applied to sources for cute-dbt#235).
    fn into_domain(self, key: String) -> SourceNode {
        let column_descriptions = self
            .columns
            .into_iter()
            .filter_map(|(name, col)| {
                col.description
                    .filter(|d| !d.is_empty())
                    .map(|desc| (name, desc))
            })
            .collect();
        SourceNode::new(
            NodeId::new(key),
            self.source_name.unwrap_or_default(),
            self.name.unwrap_or_default(),
            self.identifier,
            self.schema.unwrap_or_default(),
            self.database,
            self.relation_name,
        )
        .with_column_descriptions(column_descriptions)
    }
}

/// Wire projection of one `unit_tests` map entry.
///
/// The domain [`UnitTest`] type stores `tags` and `meta` flat, but the
/// dbt manifest nests them under a `config` sub-object. This wire struct
/// keeps the raw nesting; [`WireManifest::into_domain`] lifts the nested
/// values into the flat domain constructor. `original_file_path` is a
/// top-level sibling of `config`, not nested inside it.
///
/// All other fields (`name`, `model`, `given`, `expect`, `description`,
/// `depends_on`) match the domain shape directly and are passed through.
#[derive(Debug, Deserialize)]
struct WireUnitTest {
    name: String,
    model: NodeId,
    #[serde(default)]
    given: Vec<UnitTestGiven>,
    expect: UnitTestExpect,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    depends_on: DependsOn,
    /// Nested `config` block — carries `tags` and `meta` per ADR-5.
    #[serde(default)]
    config: WireUnitTestConfig,
    /// Top-level path to the declaring `.yml` file (not under `config`).
    #[serde(default)]
    original_file_path: Option<String>,
    /// Top-level `overrides` block — carries the `macros.is_incremental`
    /// flag (cute-dbt#145). Sibling of `config`, not nested inside it.
    /// `Option` because dbt-fusion emits an unset `overrides` as an
    /// explicit JSON `null` (not an absent key): `#[serde(default)]`
    /// covers the absent case, the `Option` covers the explicit-`null`
    /// case — a bare struct field rejects `null` and fails the parse.
    #[serde(default)]
    overrides: Option<WireUnitTestOverrides>,
    /// Engine-resolved `unique_id` of the tested model (cute-dbt#254).
    /// Top-level sibling of `model` (`ManifestUnitTest
    /// .tested_node_unique_id`, `dbt-schemas`
    /// `manifest/manifest_nodes.rs:273` @ `9977b6cb…`). For versioned
    /// models this is the `.vN`-suffixed `unique_id` — the only handle
    /// that binds them (the bare `model:` name never leaf-matches a
    /// versioned id). `Option` + `#[serde(default)]` tolerate both the
    /// absent key (every committed fixture predates the field) and an
    /// explicit `null` (the engine null-fill shape).
    #[serde(default)]
    tested_node_unique_id: Option<NodeId>,
    /// Engine-resolved `unique_id` of the node backing a `this` given
    /// input — the [`Self::tested_node_unique_id`] twin
    /// (`manifest_nodes.rs:274` @ `9977b6cb…`; fusion never populates
    /// it as of that SHA — dbt-core parity field). Same tolerance
    /// contract.
    #[serde(default)]
    this_input_node_unique_id: Option<NodeId>,
}

/// Tolerant wire projection of the `config` sub-object on a dbt unit-test
/// node. Only `tags` and `meta` are consumed; `enabled`, `static_analysis`,
/// and any future dbt additions are accepted and discarded (ADR-5 — no
/// `deny_unknown_fields`, `#[serde(default)]` on all fields).
#[derive(Debug, Default, Deserialize)]
struct WireUnitTestConfig {
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    meta: Option<Value>,
}

/// Tolerant wire projection of a unit test's top-level `overrides` block.
///
/// dbt-fusion types `overrides` as three sibling open maps — `env_vars`,
/// `macros`, and `vars` — each a `name -> arbitrary value` map
/// (`UnitTestOverrides` in `dbt-schemas`
/// `properties/unit_test_properties.rs` @ `9977b6cb…`). Since
/// cute-dbt#200 all three channels are retained as untyped passthrough
/// maps (mirroring how `config.meta` is carried) and fold into the
/// domain's grouped [`UnitTestOverrides`] via [`Self::into_grouped`];
/// the `macros.is_incremental` flag keeps its dedicated typed lift
/// (cute-dbt#145) in [`WireUnitTestOverrides::is_incremental_mode`].
#[derive(Debug, Default, Deserialize)]
struct WireUnitTestOverrides {
    /// `overrides.macros`. `Option` (not a bare map) because dbt-fusion
    /// emits each unset override channel as an explicit JSON `null` rather
    /// than omitting the key — verified against the committed jaffle-shop
    /// fixture (`"overrides": null`, and when an overrides object is
    /// present its unset channels serialize as `"macros": null`). A bare
    /// `BTreeMap` rejects `null` and would fail the whole manifest parse
    /// (ADR-5 violation). Values stay untyped passthrough.
    #[serde(default)]
    macros: Option<BTreeMap<String, Value>>,
    /// `overrides.vars` (cute-dbt#200) — same tolerance contract as
    /// [`Self::macros`]. The committed playground fixture additionally
    /// shows dbt-core emitting an unset channel as an EMPTY map
    /// (`"vars": {}`); both null and `{}` collapse to "no overrides in
    /// this group".
    #[serde(default)]
    vars: Option<BTreeMap<String, Value>>,
    /// `overrides.env_vars` (cute-dbt#200) — same tolerance contract as
    /// [`Self::macros`]/[`Self::vars`].
    #[serde(default)]
    env_vars: Option<BTreeMap<String, Value>>,
}

impl WireUnitTestOverrides {
    /// Collapse `macros.is_incremental` to the domain's typed flag.
    ///
    /// Tolerant truthiness, never an error (so one oddly-typed override can
    /// never fail the whole manifest, ADR-5): a JSON bool is taken as-is;
    /// the canonical `"true"` / `"false"` strings parse (faithful to
    /// fusion's runtime, which stubs `is_incremental()` to return the raw
    /// override value, and a string is Jinja-truthy); any other shape
    /// (number, null, absent/null macros, absent key) ⇒ `None`, the
    /// full-refresh default.
    fn is_incremental_mode(&self) -> Option<bool> {
        match self.macros.as_ref()?.get("is_incremental") {
            Some(Value::Bool(b)) => Some(*b),
            Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    /// Fold the three wire channels into the domain's grouped
    /// [`UnitTestOverrides`] (cute-dbt#200): group name → name → untyped
    /// passthrough [`Value`] (native scalars preserved end-to-end — the
    /// cute-dbt#197 founder decision). Null and EMPTY channels are
    /// dropped (both unset shapes observed on real fixtures: fusion
    /// null-fills, dbt-core emits `{}`), and a blob with no effective
    /// override collapses to `None` so the payload key is omitted.
    fn into_grouped(self) -> Option<UnitTestOverrides> {
        let grouped: UnitTestOverrides = [
            ("env_vars", self.env_vars),
            ("macros", self.macros),
            ("vars", self.vars),
        ]
        .into_iter()
        .filter_map(|(group, channel)| {
            channel
                .filter(|map| !map.is_empty())
                .map(|map| (group.to_owned(), map))
        })
        .collect();
        (!grouped.is_empty()).then_some(grouped)
    }
}

impl WireUnitTest {
    /// Translate the wire projection into the domain [`UnitTest`], lifting
    /// `config.tags` and `config.meta` out of the nested `config` block and
    /// keeping `original_file_path` from the top level.
    fn into_domain(self) -> UnitTest {
        let is_incremental_mode = self
            .overrides
            .as_ref()
            .and_then(WireUnitTestOverrides::is_incremental_mode);
        // cute-dbt#200 — the FULL grouped blob rides alongside the lifted
        // incremental flag (additive context, not a replacement).
        let overrides = self.overrides.and_then(WireUnitTestOverrides::into_grouped);
        UnitTest::new(
            self.name,
            self.model,
            self.given,
            self.expect,
            self.description,
            self.depends_on,
            self.config.tags,
            self.config.meta,
            self.original_file_path,
        )
        .with_incremental_mode(is_incremental_mode)
        .with_overrides(overrides)
        .with_tested_node_unique_id(self.tested_node_unique_id)
        .with_this_input_node_unique_id(self.this_input_node_unique_id)
    }
}

impl WireManifest {
    /// Translate the tolerant wire projection into the post-normalized
    /// domain [`Manifest`]: fold each node's map key into its `id`,
    /// lift `config.tags` / `config.meta` out of each unit test's nested
    /// `config` block, and reduce each macro object to its body string.
    fn into_domain(self) -> Manifest {
        let nodes = self
            .nodes
            .into_iter()
            .map(|(key, wire)| {
                let id = NodeId::new(key);
                // One pass over the wire columns feeds three domain
                // maps: name → data_type (the `.contract` column-set
                // diff), name → non-empty description (the cute-dbt#165
                // column-header tooltips; fusion serializes an unset
                // description as `""`, which is dropped here), and
                // name → ColumnFacts (the cute-dbt#257 extension —
                // fact-free columns store nothing).
                let mut columns = BTreeMap::new();
                let mut column_descriptions = BTreeMap::new();
                let mut column_facts = BTreeMap::new();
                for (name, col) in wire.columns {
                    if let Some(desc) = col.description.filter(|d| !d.is_empty()) {
                        column_descriptions.insert(name.clone(), desc);
                    }
                    if let Some(facts) =
                        fold_column_facts(col.meta, col.tags, col.policy_tags, col.constraints)
                    {
                        column_facts.insert(name.clone(), facts);
                    }
                    columns.insert(name, col.data_type);
                }
                let config = wire.config.into_domain();
                let node = Node::new(
                    id.clone(),
                    wire.resource_type,
                    // cute-dbt#256 — per-node checksum degrade (the
                    // dbt FileHash.empty() sentinel), never a
                    // whole-manifest parse failure.
                    fold_checksum(wire.checksum),
                    wire.compiled_code,
                    wire.raw_code,
                    wire.depends_on,
                    wire.original_file_path,
                    config,
                    wire.relation_name,
                    columns,
                )
                .with_column_descriptions(column_descriptions)
                .with_test_attachment(wire.column_name, wire.attached_node, wire.test_metadata)
                // cute-dbt#200 — model description (dbt-core's
                // empty-string unset shape dropped, the #165 precedent)
                // + the authoritative deduplicated top-level tags.
                .with_model_metadata(
                    wire.description.filter(|d| !d.is_empty()),
                    wire.tags.unwrap_or_default(),
                )
                // cute-dbt#105 — the schema-properties YAML path, with
                // the wire's `<package>://` URI scheme stripped.
                .with_patch_path(wire.patch_path.map(strip_package_uri_scheme))
                // cute-dbt#256 — identity (empty-string unset shapes
                // dropped so bare_name's leaf fallback stays sound),
                // governance, and the StringOrInteger-normalized
                // version fields.
                .with_identity(
                    wire.name.filter(|n| !n.is_empty()),
                    wire.package_name.filter(|p| !p.is_empty()),
                )
                .with_governance(wire.group, wire.access)
                .with_versions(
                    version_string(wire.version),
                    version_string(wire.latest_version),
                    wire.deprecation_date,
                )
                // cute-dbt#257 — structure (fqn) + the contract family
                // (model constraints, engine-inferred primary_key, the
                // top-level contract checksum hoisted flat) + the
                // per-column facts gathered above.
                .with_fqn(wire.fqn.unwrap_or_default())
                .with_contract_facts(
                    wire.constraints.unwrap_or_default(),
                    wire.primary_key.unwrap_or_default(),
                    wire.contract.and_then(WireContract::checksum_string),
                )
                .with_column_facts(column_facts);
                (id, node)
            })
            .collect();
        let unit_tests = self
            .unit_tests
            .into_iter()
            .map(|(key, wire)| (key, wire.into_domain()))
            .collect();
        let macros = self
            .macros
            .into_iter()
            .map(|(key, wire)| (key, wire.macro_sql))
            .collect();
        let sources = self
            .sources
            .into_iter()
            .map(|(key, wire)| (NodeId::new(key.clone()), wire.into_domain(key)))
            .collect();
        // cute-dbt#256 — exposures keyed by the wire map key folded
        // into the id (the sources precedent); groups keyed by the wire
        // map key, joined from nodes by NAME via `group_by_name`.
        let exposures = self
            .exposures
            .into_iter()
            .map(|(key, wire)| (NodeId::new(key.clone()), wire.into_domain(key)))
            .collect();
        let groups = self
            .groups
            .into_iter()
            .map(|(key, wire)| (key, wire.into_domain()))
            .collect();
        Manifest::new(self.metadata, nodes, unit_tests, macros)
            .with_sources(sources)
            .with_exposures(exposures)
            .with_groups(groups)
    }
}

// ---------------------------------------------------------------------
// Stage-1 pre-flight.
// ---------------------------------------------------------------------

/// Extract the major version integer from a `dbt_schema_version` value.
///
/// dbt emits a URL like
/// `https://schemas.getdbt.com/dbt/manifest/v12.json`; a bare `v12` is
/// also accepted. Returns `None` when no `v<integer>` token can be
/// recovered, which the caller treats as an unsupported schema.
fn extract_schema_major(raw: &str) -> Option<u32> {
    let segment = raw.trim().rsplit('/').next().unwrap_or_default();
    let segment = segment.strip_suffix(".json").unwrap_or(segment);
    let digits = segment
        .strip_prefix('v')
        .or_else(|| segment.strip_prefix('V'))?;
    digits.parse::<u32>().ok()
}

/// Reject a `dbt_schema_version` below the dbt ≥1.8 floor.
///
/// A version that cannot be parsed into a `v<N>` token is also rejected:
/// the `dbt_schema_version` key is present (so this is not `Unreadable`)
/// but it is not a version cute-dbt recognizes as supported.
///
/// # Errors
///
/// [`PreflightError::SchemaUnsupported`] when the major version is below
/// [`SUPPORTED_SCHEMA_FLOOR`] or cannot be recovered at all.
fn check_schema_floor(raw: &str) -> Result<(), PreflightError> {
    match extract_schema_major(raw) {
        Some(major) if major >= SUPPORTED_SCHEMA_FLOOR => Ok(()),
        _ => Err(PreflightError::SchemaUnsupported {
            found: raw.to_owned(),
            minimum: SUPPORTED_SCHEMA_MIN_LABEL,
        }),
    }
}

/// Deserialize + Stage-1 pre-flight a **primary** manifest from its raw
/// JSON text.
///
/// # Errors
///
/// - [`PreflightError::Unreadable`] — invalid JSON, or a missing
///   structurally required key (`metadata.dbt_schema_version`). serde
///   reports both as a deserialization error.
/// - [`PreflightError::SchemaUnsupported`] — `dbt_schema_version` is
///   below the dbt ≥1.8 floor or is not a recognizable `v<N>` token.
fn parse_manifest(text: &str) -> Result<Manifest, PreflightError> {
    let wire: WireManifest =
        serde_json::from_str(text).map_err(|err| PreflightError::Unreadable {
            detail: err.to_string(),
        })?;
    check_schema_floor(wire.metadata.dbt_schema_version())?;
    Ok(wire.into_domain())
}

// ---------------------------------------------------------------------
// The real-file port impl + baseline loading.
// ---------------------------------------------------------------------

/// The production [`ManifestSource`] — reads manifest JSON from a file.
///
/// A zero-field unit struct: the path is supplied per call so a single
/// instance loads both the primary and the baseline manifest.
#[derive(Debug, Default, Clone, Copy)]
pub struct FileManifestSource;

impl ManifestSource for FileManifestSource {
    fn load(&self, path: &Path) -> Result<Manifest, PreflightError> {
        let text = fs::read_to_string(path).map_err(|err| PreflightError::Unreadable {
            detail: format!("{}: {err}", path.display()),
        })?;
        parse_manifest(&text)
    }
}

/// Load + Stage-1 pre-flight the **baseline** manifest, remapping every
/// failure to [`PreflightError::BaselineUnusable`].
///
/// The baseline is a reference input: when it is broken there is nothing
/// to diff against, so the tool fails closed rather than emitting a
/// partial report. The remap keeps the underlying *reason* in `detail`
/// while the variant tells the run loop the failure was the baseline,
/// not the primary manifest.
///
/// # Errors
///
/// [`PreflightError::BaselineUnusable`] when the baseline could not be
/// read or did not pass Stage-1 pre-flight.
pub fn load_baseline(source: &dyn ManifestSource, path: &Path) -> Result<Manifest, PreflightError> {
    source
        .load(path)
        .map_err(|err| PreflightError::BaselineUnusable {
            detail: baseline_detail(&err),
        })
}

/// Flatten a Stage-1 failure into the `detail` string of
/// [`PreflightError::BaselineUnusable`] without nesting the
/// `"baseline manifest unusable: …"` prefix that its `Display` adds.
fn baseline_detail(err: &PreflightError) -> String {
    match err {
        PreflightError::Unreadable { detail } => detail.clone(),
        PreflightError::SchemaUnsupported { found, minimum } => {
            format!("dbt schema {found} is below minimum {minimum}")
        }
        // Unreachable from `ManifestSource::load` (Stage-2 / baseline
        // variants never originate there), but the match must be total.
        PreflightError::NotCompiled { .. } | PreflightError::BaselineUnusable { .. } => {
            err.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ConstraintKind;

    const V12_URL: &str = "https://schemas.getdbt.com/dbt/manifest/v12.json";

    /// A minimal but complete schema-v12 manifest exercising every
    /// translated field: a node, a unit test, a macro.
    fn minimal_v12_manifest() -> String {
        format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.stg_orders": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "deadbeef" }},
                  "compiled_code": "select 1",
                  "depends_on": {{ "macros": [], "nodes": ["seed.shop.raw_orders"] }}
                }}
              }},
              "unit_tests": {{
                "unit_test.shop.t1": {{
                  "name": "t1",
                  "model": "stg_orders",
                  "given": [
                    {{ "input": "ref('raw_orders')", "rows": [{{"id":1}}], "format": "dict" }}
                  ],
                  "expect": {{ "rows": [{{"id":1}}], "format": "dict" }},
                  "description": "dedup test"
                }}
              }},
              "macros": {{
                "macro.shop.helper": {{ "macro_sql": "{{% macro helper() %}}{{% endmacro %}}" }}
              }}
            }}"#
        )
    }

    // ----- extract_schema_major -------------------------------------

    #[test]
    fn extract_schema_major_reads_the_canonical_url() {
        assert_eq!(extract_schema_major(V12_URL), Some(12));
    }

    #[test]
    fn extract_schema_major_reads_a_bare_token() {
        assert_eq!(extract_schema_major("v12"), Some(12));
        assert_eq!(extract_schema_major("V12"), Some(12));
    }

    #[test]
    fn extract_schema_major_reads_a_newer_schema() {
        assert_eq!(
            extract_schema_major("https://schemas.getdbt.com/dbt/manifest/v13.json"),
            Some(13)
        );
    }

    #[test]
    fn extract_schema_major_reads_an_older_schema() {
        assert_eq!(
            extract_schema_major("https://schemas.getdbt.com/dbt/manifest/v9.json"),
            Some(9)
        );
    }

    #[test]
    fn extract_schema_major_tolerates_surrounding_whitespace() {
        assert_eq!(extract_schema_major("  v12  "), Some(12));
    }

    #[test]
    fn extract_schema_major_rejects_empty_and_garbage() {
        assert_eq!(extract_schema_major(""), None);
        assert_eq!(extract_schema_major("garbage"), None);
        assert_eq!(extract_schema_major("manifest.json"), None);
        assert_eq!(extract_schema_major("vNaN"), None);
    }

    // ----- check_schema_floor ---------------------------------------

    #[test]
    fn check_schema_floor_accepts_the_floor_and_newer() {
        assert!(check_schema_floor(V12_URL).is_ok());
        assert!(check_schema_floor("v12").is_ok());
        assert!(check_schema_floor("v13").is_ok());
    }

    #[test]
    fn check_schema_floor_rejects_pre_1_8_with_both_versions() {
        let err = check_schema_floor("https://schemas.getdbt.com/dbt/manifest/v11.json")
            .expect_err("v11 is below the floor");
        match err {
            PreflightError::SchemaUnsupported { found, minimum } => {
                assert!(
                    found.contains("v11"),
                    "found should echo the input: {found}"
                );
                assert_eq!(minimum, "v12");
            }
            other => panic!("expected SchemaUnsupported, got {other:?}"),
        }
    }

    #[test]
    fn check_schema_floor_rejects_an_unparseable_version() {
        let err = check_schema_floor("garbage").expect_err("garbage is not a version");
        assert!(matches!(err, PreflightError::SchemaUnsupported { .. }));
    }

    #[test]
    fn schema_floor_label_matches_the_numeric_floor() {
        // Drift guard: the &'static str baked into the error message
        // must agree with the integer the check actually uses.
        assert_eq!(
            SUPPORTED_SCHEMA_MIN_LABEL,
            format!("v{SUPPORTED_SCHEMA_FLOOR}")
        );
    }

    // ----- cute-dbt#254 — engine-resolved unit-test target ids ------

    #[test]
    fn parse_manifest_threads_engine_resolved_target_ids() {
        // The versioned-model wire shape: fusion resolves each per-version
        // unit test's target to the `.vN`-suffixed unique_id
        // (`tested_node_unique_id` / `this_input_node_unique_id`,
        // `dbt-schemas` `manifest/manifest_nodes.rs:273-274` @ `9977b6cb…`).
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.dim_customers.v2": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "deadbeef" }},
                  "compiled_code": "select 1"
                }}
              }},
              "unit_tests": {{
                "unit_test.shop.dim_customers.t1.v2": {{
                  "name": "t1",
                  "model": "dim_customers",
                  "expect": {{ "rows": [{{"id":1}}], "format": "dict" }},
                  "tested_node_unique_id": "model.shop.dim_customers.v2",
                  "this_input_node_unique_id": "model.shop.dim_customers.v2"
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("versioned manifest parses");
        let ut = manifest
            .unit_test("unit_test.shop.dim_customers.t1.v2")
            .expect("unit test ingested");
        assert_eq!(
            ut.tested_node_unique_id().map(NodeId::as_str),
            Some("model.shop.dim_customers.v2"),
        );
        assert_eq!(
            ut.this_input_node_unique_id().map(NodeId::as_str),
            Some("model.shop.dim_customers.v2"),
        );
    }

    #[test]
    fn parse_manifest_tolerates_absent_and_null_resolved_ids() {
        // Absent keys: every committed fixture predates the fields.
        let manifest = parse_manifest(&minimal_v12_manifest()).expect("valid v12 manifest");
        let ut = manifest.unit_test("unit_test.shop.t1").expect("ingested");
        assert!(ut.tested_node_unique_id().is_none());
        assert!(ut.this_input_node_unique_id().is_none());

        // Explicit nulls: the engine null-fill shape (the cute-dbt#145
        // `"overrides": null` precedent) must not fail the parse.
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "unit_tests": {{
                "unit_test.shop.t1": {{
                  "name": "t1",
                  "model": "stg_orders",
                  "expect": {{ "rows": [] }},
                  "tested_node_unique_id": null,
                  "this_input_node_unique_id": null
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("null-filled ids parse");
        let ut = manifest.unit_test("unit_test.shop.t1").expect("ingested");
        assert!(ut.tested_node_unique_id().is_none());
        assert!(ut.this_input_node_unique_id().is_none());
    }

    // ----- parse_manifest: happy path + translation -----------------

    #[test]
    fn parse_manifest_translates_every_field() {
        let manifest = parse_manifest(&minimal_v12_manifest()).expect("valid v12 manifest");

        assert_eq!(manifest.metadata().dbt_schema_version(), V12_URL);

        // Node: the map key is folded into `id`; checksum, compiled
        // code, and dependency edges all survive translation.
        let node_id = NodeId::new("model.shop.stg_orders");
        let node = manifest.node(&node_id).expect("stg_orders node present");
        assert_eq!(node.id(), &node_id);
        assert_eq!(node.resource_type(), "model");
        assert_eq!(node.checksum().checksum(), "deadbeef");
        assert_eq!(node.compiled_code(), Some("select 1"));
        assert_eq!(
            node.depends_on().nodes(),
            &[NodeId::new("seed.shop.raw_orders")]
        );

        // Unit test: passes through unchanged (the wire shape already
        // matches the domain shape).
        let unit_test = manifest
            .unit_test("unit_test.shop.t1")
            .expect("unit test present");
        assert_eq!(unit_test.name(), "t1");
        assert_eq!(unit_test.model(), &NodeId::new("stg_orders"));
        assert_eq!(unit_test.given().len(), 1);
        assert_eq!(unit_test.description(), Some("dedup test"));

        // Macro: the wire macro *object* is reduced to its body string.
        assert_eq!(
            manifest
                .macros()
                .get("macro.shop.helper")
                .map(String::as_str),
            Some("{% macro helper() %}{% endmacro %}")
        );
    }

    #[test]
    fn parse_manifest_threads_raw_code_through_to_domain_node() {
        // cute-dbt#47 — `raw_code` is the model's Jinja source; the
        // adapter must thread it from the wire DTO into the domain Node
        // verbatim (Jinja comments + refs preserved). Newline + nested
        // quotes are JSON-escaped in the literal below.
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.stg_x": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "abc" }},
                  "compiled_code": "select 1",
                  "raw_code": "{{# header #}}\nselect * from {{{{ ref('upstream') }}}}"
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid v12 manifest");
        let node = manifest
            .node(&NodeId::new("model.shop.stg_x"))
            .expect("stg_x present");
        assert_eq!(
            node.raw_code(),
            Some("{# header #}\nselect * from {{ ref('upstream') }}")
        );
    }

    #[test]
    fn parse_manifest_translates_sub_selector_fields() {
        // cute-dbt#17 — the `.configs` / `.relation` / `.contract` inputs.
        // The wire `config` dict flattens into NodeConfig::config (with
        // `config.contract` re-inserted), `config.contract.enforced` hoists
        // to a flat bool, `relation_name` passes through, and each
        // `columns` entry reduces to its `data_type`.
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.dim_x": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "abc" }},
                  "relation_name": "\"db\".\"main\".\"dim_x\"",
                  "config": {{
                    "materialized": "table",
                    "enabled": true,
                    "contract": {{ "enforced": true, "alias_types": true }}
                  }},
                  "columns": {{
                    "id": {{ "name": "id", "data_type": "integer" }},
                    "label": {{ "name": "label" }}
                  }}
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid v12 manifest");
        let node = manifest
            .node(&NodeId::new("model.shop.dim_x"))
            .expect("dim_x present");

        // .relation
        assert_eq!(node.relation_name(), Some("\"db\".\"main\".\"dim_x\""));

        // .configs — the flattened dict carries every config key including
        // the whole `contract` sub-object dbt emitted (verbatim, not
        // reduced).
        let config = node.config().config();
        assert_eq!(config.get("materialized"), Some(&Value::from("table")));
        assert_eq!(config.get("enabled"), Some(&Value::from(true)));
        assert_eq!(
            config.get("contract"),
            Some(&serde_json::json!({ "enforced": true, "alias_types": true })),
            "the whole contract sub-object is preserved so .configs sees it",
        );

        // .contract — enforced hoisted to a flat bool out of the config
        // dict; columns reduced to name → data_type (a column without a
        // declared type → None).
        assert!(node.config().contract_enforced());
        assert_eq!(node.columns().get("id"), Some(&Some("integer".to_owned())));
        assert_eq!(node.columns().get("label"), Some(&None));
    }

    #[test]
    fn parse_manifest_tolerates_a_node_missing_sub_selector_fields() {
        // ADR-5: a node without `config` / `relation_name` / `columns`
        // (older or synthetic manifests) deserializes to the empty
        // defaults rather than failing closed.
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.bare": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "abc" }}
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid v12 manifest");
        let node = manifest
            .node(&NodeId::new("model.shop.bare"))
            .expect("bare present");
        assert!(node.config().config().is_empty());
        assert!(!node.config().contract_enforced());
        assert!(node.relation_name().is_none());
        assert!(node.columns().is_empty());
    }

    #[test]
    fn parse_manifest_tolerates_a_node_missing_raw_code() {
        // Older fixtures / hand-crafted stubs may lack `raw_code`. The
        // adapter accepts it (`#[serde(default)]`) and surfaces `None`.
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.stg_y": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "def" }},
                  "compiled_code": "select 1"
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid v12 manifest");
        let node = manifest
            .node(&NodeId::new("model.shop.stg_y"))
            .expect("stg_y present");
        assert!(node.raw_code().is_none());
    }

    #[test]
    fn parse_manifest_tolerates_a_manifest_with_only_metadata() {
        let json = format!(r#"{{ "metadata": {{ "dbt_schema_version": "{V12_URL}" }} }}"#);
        let manifest = parse_manifest(&json).expect("metadata-only manifest is valid");
        assert!(manifest.nodes().is_empty());
        assert!(manifest.unit_tests().is_empty());
        assert!(manifest.macros().is_empty());
        assert!(manifest.sources().is_empty());
    }

    // ----- cute-dbt#57: top-level `sources` block ---------------------

    #[test]
    fn parse_manifest_translates_a_core_style_source_entry() {
        // dbt-core 1.11 dialect: unset Option fields serialize as
        // explicit `null` (here `database`), and the entry carries
        // sibling keys cute-dbt does not consume (ADR-5 tolerance).
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "sources": {{
                "source.shop.synthea_raw.patients": {{
                  "database": null,
                  "schema": "main",
                  "name": "patients",
                  "resource_type": "source",
                  "source_name": "synthea_raw",
                  "identifier": "patients",
                  "relation_name": "\"memory\".\"main\".\"patients\"",
                  "loaded_at_field": null,
                  "freshness": null,
                  "quoting": {{ "database": null }}
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("core-style source entry parses");
        assert_eq!(manifest.sources().len(), 1);
        let source = manifest
            .source_by_name("synthea_raw", "patients")
            .expect("the (source_name, name) pair resolves");
        assert_eq!(source.id().as_str(), "source.shop.synthea_raw.patients");
        assert_eq!(source.schema(), "main");
        assert_eq!(source.identifier(), Some("patients"));
        assert_eq!(source.database(), None, "explicit null → None");
        assert_eq!(
            source.relation_name(),
            Some("\"memory\".\"main\".\"patients\"")
        );
    }

    #[test]
    fn parse_manifest_translates_a_fusion_style_source_entry() {
        // dbt-fusion dialect: `#[skip_serializing_none]` OMITS unset keys
        // entirely (no `identifier`, `database`, `relation_name`) — the
        // cute-dbt#145 absent-key half of the engine-divergence rule.
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "sources": {{
                "source.shop.synthea_raw.encounters": {{
                  "schema": "main",
                  "name": "encounters",
                  "source_name": "synthea_raw"
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("fusion-style source entry parses");
        let source = manifest
            .source_by_name("synthea_raw", "encounters")
            .expect("the (source_name, name) pair resolves");
        assert_eq!(source.identifier(), None);
        assert_eq!(source.database(), None);
        assert_eq!(source.relation_name(), None);
    }

    #[test]
    fn parse_manifest_extracts_source_column_descriptions_and_drops_empty_ones() {
        // cute-dbt#235 — source `columns` ride the same wire shape as
        // node columns (fusion serializes both through
        // `serialize_dbt_columns`; an unset description is `""`). Only
        // non-empty prose reaches the domain map — the #165 drop rule.
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "sources": {{
                "source.shop.synthea_raw.patients": {{
                  "schema": "main",
                  "name": "patients",
                  "source_name": "synthea_raw",
                  "columns": {{
                    "Id": {{ "name": "Id", "description": "Unique patient identifier (UUID)" }},
                    "FIRST": {{ "name": "FIRST", "description": "" }},
                    "LAST": {{ "name": "LAST" }}
                  }}
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("source entry with columns parses");
        let source = manifest
            .source_by_name("synthea_raw", "patients")
            .expect("the (source_name, name) pair resolves");
        assert_eq!(
            source.column_descriptions().get("Id"),
            Some(&"Unique patient identifier (UUID)".to_owned()),
        );
        assert!(
            !source.column_descriptions().contains_key("FIRST"),
            "fusion's empty-string unset description is dropped",
        );
        assert!(
            !source.column_descriptions().contains_key("LAST"),
            "a column with no description key contributes nothing",
        );
    }

    #[test]
    fn parse_manifest_tolerates_a_degenerate_source_entry() {
        // A sources entry with every consumed key absent must not fail
        // the whole manifest (ADR-5); it translates to empty-string
        // names that can never match a parsed `source('a','b')` given —
        // the fail-open posture of an unresolvable ref.
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "sources": {{ "source.shop.broken.entry": {{}} }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("degenerate source entry tolerated");
        assert_eq!(manifest.sources().len(), 1);
        assert!(manifest.source_by_name("broken", "entry").is_none());
    }

    // ----- cute-dbt#165: column descriptions + test attribution ------

    #[test]
    fn parse_manifest_extracts_column_descriptions_and_drops_empty_ones() {
        // fusion ALWAYS serializes `description` — an unset one as `""`
        // (`serialize_dbt_column_desc`, dbt-schemas dbt_column.rs). The
        // adapter keeps only non-empty prose; `columns` (name → data_type)
        // is unchanged so `.contract` semantics cannot drift.
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.dim_x": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "abc" }},
                  "columns": {{
                    "id": {{ "name": "id", "description": "Primary key", "data_type": "integer" }},
                    "label": {{ "name": "label", "description": "" }},
                    "untyped": {{ "name": "untyped" }}
                  }}
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid v12 manifest");
        let node = manifest
            .node(&NodeId::new("model.shop.dim_x"))
            .expect("dim_x present");
        assert_eq!(
            node.column_descriptions().get("id").map(String::as_str),
            Some("Primary key")
        );
        assert!(
            !node.column_descriptions().contains_key("label"),
            "an empty description (fusion's unset shape) is dropped"
        );
        assert!(!node.column_descriptions().contains_key("untyped"));
        // The .contract input is untouched by the description ingest.
        assert_eq!(node.columns().get("id"), Some(&Some("integer".to_owned())));
        assert_eq!(node.columns().get("label"), Some(&None));
    }

    #[test]
    fn parse_manifest_extracts_test_attribution_fields() {
        // A column-scoped generic test (the jaffle-shop wire shape):
        // column_name + attached_node + test_metadata incl. a
        // package-qualified namespace and kwargs passthrough.
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "test.shop.accepted_values_orders_status.abc123": {{
                  "resource_type": "test",
                  "checksum": {{ "name": "none", "checksum": "" }},
                  "column_name": "status",
                  "attached_node": "model.shop.orders",
                  "test_metadata": {{
                    "name": "accepted_values",
                    "kwargs": {{
                      "column_name": "status",
                      "values": ["placed", "shipped"],
                      "model": "{{{{ get_where_subquery(ref('orders')) }}}}"
                    }},
                    "namespace": null
                  }}
                }},
                "test.shop.expect_between_orders_amount.def456": {{
                  "resource_type": "test",
                  "checksum": {{ "name": "none", "checksum": "" }},
                  "column_name": "amount",
                  "attached_node": "model.shop.orders",
                  "test_metadata": {{
                    "name": "expect_column_values_to_be_between",
                    "kwargs": {{ "min_value": 0 }},
                    "namespace": "dbt_expectations"
                  }}
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid v12 manifest");
        let t1 = manifest
            .node(&NodeId::new(
                "test.shop.accepted_values_orders_status.abc123",
            ))
            .expect("test node present");
        assert_eq!(t1.column_name(), Some("status"));
        assert_eq!(t1.attached_node(), Some(&NodeId::new("model.shop.orders")));
        let tm1 = t1.test_metadata().expect("test_metadata present");
        assert_eq!(tm1.name(), "accepted_values");
        assert_eq!(tm1.namespace(), None, "explicit-null namespace → None");
        assert_eq!(
            tm1.kwargs()["values"],
            serde_json::json!(["placed", "shipped"])
        );
        let t2 = manifest
            .node(&NodeId::new(
                "test.shop.expect_between_orders_amount.def456",
            ))
            .expect("test node present");
        let tm2 = t2.test_metadata().expect("test_metadata present");
        assert_eq!(tm2.namespace(), Some("dbt_expectations"));
    }

    #[test]
    fn parse_manifest_tolerates_singular_and_model_level_tests() {
        // The real playground fixture carries both shapes: singular
        // (SQL-file) tests OMIT test_metadata entirely (fusion
        // `#[skip_serializing_none]`), and model-level tests carry
        // `column_name: null`. Explicit nulls must also parse (ADR-5 —
        // fusion null-fills unset Options on other structs).
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "test.shop.singular_check": {{
                  "resource_type": "test",
                  "checksum": {{ "name": "sha256", "checksum": "aa" }}
                }},
                "test.shop.model_level_check": {{
                  "resource_type": "test",
                  "checksum": {{ "name": "sha256", "checksum": "bb" }},
                  "column_name": null,
                  "attached_node": null,
                  "test_metadata": null
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("tolerant parse");
        for id in ["test.shop.singular_check", "test.shop.model_level_check"] {
            let node = manifest.node(&NodeId::new(id)).expect("node present");
            assert!(node.column_name().is_none(), "{id}");
            assert!(node.attached_node().is_none(), "{id}");
            assert!(node.test_metadata().is_none(), "{id}");
        }
    }

    // ----- parse_manifest: Stage-1 failure arms ---------------------

    #[test]
    fn parse_manifest_rejects_non_json_as_unreadable() {
        let err = parse_manifest("this is not json").expect_err("not JSON");
        assert!(matches!(err, PreflightError::Unreadable { .. }));
    }

    #[test]
    fn parse_manifest_rejects_a_missing_metadata_key_as_unreadable() {
        let err = parse_manifest(r#"{ "nodes": {} }"#).expect_err("metadata is required");
        match err {
            PreflightError::Unreadable { detail } => {
                assert!(
                    detail.contains("metadata"),
                    "detail names the key: {detail}"
                );
            }
            other => panic!("expected Unreadable, got {other:?}"),
        }
    }

    #[test]
    fn parse_manifest_rejects_a_missing_schema_version_as_unreadable() {
        let err =
            parse_manifest(r#"{ "metadata": {} }"#).expect_err("dbt_schema_version is required");
        match err {
            PreflightError::Unreadable { detail } => {
                assert!(
                    detail.contains("dbt_schema_version"),
                    "detail names the key: {detail}"
                );
            }
            other => panic!("expected Unreadable, got {other:?}"),
        }
    }

    #[test]
    fn parse_manifest_rejects_a_pre_1_8_manifest_as_schema_unsupported() {
        let json = r#"{ "metadata": { "dbt_schema_version":
            "https://schemas.getdbt.com/dbt/manifest/v11.json" } }"#;
        let err = parse_manifest(json).expect_err("v11 is below the floor");
        assert!(matches!(err, PreflightError::SchemaUnsupported { .. }));
    }

    #[test]
    fn parse_manifest_rejects_a_garbage_schema_version_as_schema_unsupported() {
        let json = r#"{ "metadata": { "dbt_schema_version": "not-a-version" } }"#;
        let err = parse_manifest(json).expect_err("garbage version");
        assert!(matches!(err, PreflightError::SchemaUnsupported { .. }));
    }

    // ----- tolerant deserialization (the AC's "property test") ------

    /// The property under test: **no consumed struct uses
    /// `deny_unknown_fields`**. Coverage is exhaustive over the nine
    /// deserialization targets (`WireManifest`, `WireNode`,
    /// `WireMacro`, and the reused domain `ManifestMetadata`,
    /// `Checksum`, `DependsOn`, `UnitTest`, `UnitTestGiven`,
    /// `UnitTestExpect`) rather than randomized — exhaustive struct
    /// coverage is strictly stronger than sampling for a per-struct
    /// attribute, and adds no proptest dev-dependency.
    #[test]
    fn unknown_keys_do_not_break_deserialization_at_any_consumed_struct() {
        let json = format!(
            r#"{{
              "__unknown_top": 1,
              "metadata": {{
                "dbt_schema_version": "{V12_URL}", "__unknown_metadata": "x"
              }},
              "nodes": {{
                "model.p.m": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "ab", "__unknown_checksum": true }},
                  "compiled_code": "select 1",
                  "depends_on": {{ "macros": [], "nodes": [], "__unknown_depends_on": [] }},
                  "__unknown_node": 9
                }}
              }},
              "unit_tests": {{
                "unit_test.p.t": {{
                  "name": "t",
                  "model": "m",
                  "given": [
                    {{ "input": "ref('a')", "rows": [], "format": "dict", "__unknown_given": 1 }}
                  ],
                  "expect": {{ "rows": [], "format": "dict", "__unknown_expect": 2 }},
                  "description": "d",
                  "depends_on": {{ "macros": [], "nodes": [] }},
                  "__unknown_unit_test": "x"
                }}
              }},
              "macros": {{
                "macro.p.x": {{ "macro_sql": "/* m */", "__unknown_macro": 3 }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json)
            .expect("unknown sibling keys must never fail deserialization (ADR-5)");
        assert_eq!(manifest.nodes().len(), 1);
        assert_eq!(manifest.unit_tests().len(), 1);
        assert_eq!(manifest.macros().len(), 1);
    }

    // ----- baseline_detail ------------------------------------------

    #[test]
    fn baseline_detail_unwraps_an_unreadable_reason() {
        let err = PreflightError::Unreadable {
            detail: "expected value at line 1 column 1".to_owned(),
        };
        assert_eq!(baseline_detail(&err), "expected value at line 1 column 1");
    }

    #[test]
    fn baseline_detail_flattens_a_schema_reason() {
        let err = PreflightError::SchemaUnsupported {
            found: "v11".to_owned(),
            minimum: "v12",
        };
        assert_eq!(baseline_detail(&err), "dbt schema v11 is below minimum v12");
    }

    #[test]
    fn baseline_detail_falls_back_to_display_for_other_variants() {
        let err = PreflightError::NotCompiled {
            node_id: "model.p.m".to_owned(),
            unit_test: Some("t".to_owned()),
        };
        assert_eq!(baseline_detail(&err), err.to_string());
    }

    // ----- the in-memory port impl + load_baseline ------------------

    /// The in-memory [`ManifestSource`] — the test-suite counterpart of
    /// the real-file [`FileManifestSource`]. Two impls is the bar ADR-1
    /// sets for a port; this is the second one. Registered paths
    /// deserialize through the same Stage-1 [`parse_manifest`] the file
    /// impl uses, so the seam is honest — tests never bypass pre-flight.
    #[derive(Default)]
    struct InMemoryManifestSource {
        entries: std::collections::HashMap<std::path::PathBuf, String>,
    }

    impl InMemoryManifestSource {
        fn with(mut self, path: &str, content: impl Into<String>) -> Self {
            self.entries
                .insert(std::path::PathBuf::from(path), content.into());
            self
        }
    }

    impl ManifestSource for InMemoryManifestSource {
        fn load(&self, path: &Path) -> Result<Manifest, PreflightError> {
            match self.entries.get(path) {
                Some(text) => parse_manifest(text),
                None => Err(PreflightError::Unreadable {
                    detail: format!("{}: not registered in the in-memory source", path.display()),
                }),
            }
        }
    }

    #[test]
    fn in_memory_source_loads_a_registered_manifest() {
        let source = InMemoryManifestSource::default().with("primary.json", minimal_v12_manifest());
        let manifest = source.load(Path::new("primary.json")).expect("registered");
        assert_eq!(manifest.metadata().dbt_schema_version(), V12_URL);
    }

    #[test]
    fn in_memory_source_reports_an_unregistered_path_as_unreadable() {
        let source = InMemoryManifestSource::default();
        let err = source
            .load(Path::new("missing.json"))
            .expect_err("not registered");
        assert!(matches!(err, PreflightError::Unreadable { .. }));
    }

    #[test]
    fn manifest_source_load_works_behind_a_trait_object() {
        // The run loop holds `&dyn ManifestSource`; prove the trait is
        // object-safe and dispatches through the vtable.
        let source = InMemoryManifestSource::default().with("p.json", minimal_v12_manifest());
        let dynamic: &dyn ManifestSource = &source;
        assert!(dynamic.load(Path::new("p.json")).is_ok());
    }

    #[test]
    fn load_baseline_passes_a_good_baseline_through() {
        let source =
            InMemoryManifestSource::default().with("baseline.json", minimal_v12_manifest());
        let manifest = load_baseline(&source, Path::new("baseline.json")).expect("good baseline");
        assert_eq!(manifest.unit_tests().len(), 1);
    }

    #[test]
    fn load_baseline_remaps_an_unreadable_baseline() {
        let source = InMemoryManifestSource::default().with("baseline.json", "not json");
        let err =
            load_baseline(&source, Path::new("baseline.json")).expect_err("bad baseline JSON");
        match err {
            PreflightError::BaselineUnusable { detail } => {
                // The reason is preserved; the "baseline manifest
                // unusable:" prefix is added once by Display, never
                // nested inside `detail`.
                assert!(
                    !detail.contains("baseline manifest unusable"),
                    "no nested prefix in detail: {detail}"
                );
            }
            other => panic!("expected BaselineUnusable, got {other:?}"),
        }
    }

    #[test]
    fn load_baseline_remaps_a_schema_failure() {
        let json = r#"{ "metadata": { "dbt_schema_version":
            "https://schemas.getdbt.com/dbt/manifest/v11.json" } }"#;
        let source = InMemoryManifestSource::default().with("baseline.json", json);
        let err = load_baseline(&source, Path::new("baseline.json")).expect_err("pre-1.8 baseline");
        match err {
            PreflightError::BaselineUnusable { detail } => {
                assert!(
                    detail.contains("below minimum v12"),
                    "schema reason flattened into detail: {detail}"
                );
            }
            other => panic!("expected BaselineUnusable, got {other:?}"),
        }
    }

    #[test]
    fn load_baseline_remaps_a_missing_baseline_path() {
        let source = InMemoryManifestSource::default();
        let err =
            load_baseline(&source, Path::new("missing.json")).expect_err("no such baseline path");
        assert!(matches!(err, PreflightError::BaselineUnusable { .. }));
    }

    // ----- unit-test metadata fields (PR #29) -----------------------

    /// Regression: `config.tags`, `config.meta`, and the top-level
    /// `original_file_path` on a unit-test node must survive the
    /// wire→domain translation with their values intact.
    #[test]
    fn parse_manifest_extracts_unit_test_metadata_fields() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "unit_tests": {{
                "unit_test.shop.tagged": {{
                  "name": "tagged",
                  "model": "stg_orders",
                  "given": [],
                  "expect": {{ "rows": [] }},
                  "config": {{
                    "tags": ["quality", "smoke"],
                    "meta": {{ "owner": "data-eng", "priority": 1 }},
                    "enabled": true
                  }},
                  "original_file_path": "models/staging/unit_tests.yml"
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest with metadata fields");
        let ut = manifest
            .unit_test("unit_test.shop.tagged")
            .expect("unit test present");
        let expected_tags: Vec<String> = vec!["quality".to_owned(), "smoke".to_owned()];
        assert_eq!(
            ut.tags(),
            Some(expected_tags.as_slice()),
            "config.tags must be extracted"
        );
        let meta = ut.meta().expect("config.meta must be present");
        assert_eq!(meta["owner"], serde_json::json!("data-eng"));
        assert_eq!(meta["priority"], serde_json::json!(1));
        assert_eq!(
            ut.original_file_path(),
            Some("models/staging/unit_tests.yml"),
            "original_file_path must be extracted from top level"
        );
    }

    /// Regression: a unit-test node with an empty `config` block (or no
    /// `config` key at all) must produce `None` for all three new
    /// optional fields — no default panic, no hard error.
    #[test]
    fn parse_manifest_unit_test_metadata_defaults_to_none_when_absent() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "unit_tests": {{
                "unit_test.shop.t": {{
                  "name": "t",
                  "model": "stg_orders",
                  "given": [],
                  "expect": {{ "rows": [] }}
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest without optional fields");
        let ut = manifest
            .unit_test("unit_test.shop.t")
            .expect("unit test present");
        assert!(ut.tags().is_none(), "tags should be None when absent");
        assert!(ut.meta().is_none(), "meta should be None when absent");
        assert!(
            ut.original_file_path().is_none(),
            "original_file_path should be None when absent"
        );
        assert!(
            ut.is_incremental_mode().is_none(),
            "is_incremental_mode should be None when overrides absent"
        );
    }

    /// cute-dbt#145: the incremental-mode flag is lifted from the nested
    /// `overrides.macros.is_incremental` into the flat domain
    /// `is_incremental_mode`. A JSON bool round-trips directly.
    #[test]
    fn parse_manifest_extracts_incremental_mode_from_overrides() {
        for (flag, expected) in [("true", Some(true)), ("false", Some(false))] {
            let json = format!(
                r#"{{
                  "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
                  "unit_tests": {{
                    "unit_test.shop.t": {{
                      "name": "t", "model": "order_events",
                      "given": [], "expect": {{ "rows": [] }},
                      "overrides": {{ "macros": {{ "is_incremental": {flag} }} }}
                    }}
                  }}
                }}"#
            );
            let manifest = parse_manifest(&json).expect("valid manifest");
            let ut = manifest
                .unit_test("unit_test.shop.t")
                .expect("unit test present");
            assert_eq!(ut.is_incremental_mode(), expected, "is_incremental: {flag}");
        }
    }

    /// cute-dbt#145 regression: `is_incremental_mode` is `None` whenever the
    /// `overrides.macros.is_incremental` channel is absent — no `overrides`,
    /// an empty `overrides` block, an empty `macros` map, or only sibling
    /// channels (`vars` / `env_vars`) and other macro stubs present. Since
    /// cute-dbt#200 the sibling channels and other macro keys are RETAINED
    /// on [`UnitTest::overrides`] — but they never feed the typed
    /// incremental flag, which reads only `macros.is_incremental`.
    #[test]
    fn parse_manifest_incremental_mode_none_when_channel_absent() {
        let tails = [
            "",                                     // no overrides key at all
            r#", "overrides": null"#,               // explicit null — dbt-fusion's unset shape
            r#", "overrides": {}"#,                 // empty overrides
            r#", "overrides": { "macros": {} }"#,   // empty macros map
            r#", "overrides": { "macros": null }"#, // null macros channel (fusion null-fills)
            // fusion's all-channels-present-but-null shape:
            r#", "overrides": { "macros": null, "vars": null, "env_vars": null }"#,
            // sibling channels + an unrelated macro stub, but no is_incremental:
            r#", "overrides": { "vars": { "x": 1 }, "env_vars": { "E": "v" }, "macros": { "other_macro": true } }"#,
        ];
        for tail in tails {
            let json = format!(
                r#"{{
                  "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
                  "unit_tests": {{
                    "unit_test.shop.t": {{
                      "name": "t", "model": "order_events",
                      "given": [], "expect": {{ "rows": [] }}{tail}
                    }}
                  }}
                }}"#
            );
            let manifest = parse_manifest(&json).expect("valid manifest (tolerant)");
            let ut = manifest
                .unit_test("unit_test.shop.t")
                .expect("unit test present");
            assert_eq!(ut.is_incremental_mode(), None, "tail = {tail:?}");
        }
    }

    /// cute-dbt#145 + ADR-5: a non-bool `is_incremental` override must NEVER
    /// fail the whole manifest parse — dbt-fusion types the value as an
    /// untyped `BTreeMap<String, YmlValue>`, so a quoted/odd value is a
    /// legal manifest cute-dbt must read. Canonical `"true"`/`"false"`
    /// strings are honored (faithful to fusion's truthy runtime stub); any
    /// other shape degrades to `None`. Every case parses successfully.
    #[test]
    fn parse_manifest_incremental_mode_tolerates_non_bool_override() {
        for (value, expected) in [
            (r#""true""#, Some(true)),
            (r#""false""#, Some(false)),
            (r#""TRUE""#, Some(true)),
            (r"1", None),
            (r"null", None),
            (r#""yes""#, None),
        ] {
            let json = format!(
                r#"{{
                  "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
                  "unit_tests": {{
                    "unit_test.shop.t": {{
                      "name": "t", "model": "order_events",
                      "given": [], "expect": {{ "rows": [] }},
                      "overrides": {{ "macros": {{ "is_incremental": {value} }} }}
                    }}
                  }}
                }}"#
            );
            let manifest = parse_manifest(&json).unwrap_or_else(|e| {
                panic!("non-bool override must not fail parse: value={value}, err={e:?}")
            });
            let ut = manifest
                .unit_test("unit_test.shop.t")
                .expect("unit test present");
            assert_eq!(ut.is_incremental_mode(), expected, "value = {value}");
        }
    }

    // ----- cute-dbt#200 — full overrides blob + model description/tags -----

    /// cute-dbt#200: the full `overrides` blob is retained as the grouped
    /// domain map with NATIVE scalar values (the cute-dbt#197 founder
    /// decision: serde `Value` passthrough, never stringified). All three
    /// channels survive; the lifted `is_incremental_mode` flag rides
    /// alongside unchanged.
    #[test]
    fn parse_manifest_retains_full_overrides_with_native_scalars() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "unit_tests": {{
                "unit_test.shop.t": {{
                  "name": "t", "model": "order_events",
                  "given": [], "expect": {{ "rows": [] }},
                  "overrides": {{
                    "macros": {{ "is_incremental": true, "current_timestamp": "'2025-02-01'" }},
                    "vars": {{ "encounter_lookback_days": 7, "dq_quarantine_threshold": 0.05 }},
                    "env_vars": {{ "DBT_TARGET": "ci" }}
                  }}
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest");
        let ut = manifest
            .unit_test("unit_test.shop.t")
            .expect("unit test present");
        let overrides = ut.overrides().expect("full blob retained");
        assert_eq!(
            overrides["macros"]["is_incremental"],
            serde_json::json!(true),
            "bool stays a JSON bool"
        );
        assert_eq!(
            overrides["macros"]["current_timestamp"],
            serde_json::json!("'2025-02-01'"),
            "string stays a JSON string"
        );
        assert_eq!(
            overrides["vars"]["encounter_lookback_days"],
            serde_json::json!(7),
            "integer stays a JSON number"
        );
        assert_eq!(
            overrides["vars"]["dq_quarantine_threshold"],
            serde_json::json!(0.05),
            "float stays a JSON number"
        );
        assert_eq!(
            overrides["env_vars"]["DBT_TARGET"],
            serde_json::json!("ci"),
            "env_vars channel retained"
        );
        assert_eq!(
            ut.is_incremental_mode(),
            Some(true),
            "the #145 lifted flag is unchanged by the #200 retention"
        );
    }

    /// cute-dbt#200 tolerance matrix over BOTH engine dialects: the
    /// grouped blob is `None` for every no-effective-override wire shape —
    /// absent key, fusion's explicit `null` (whole blob or per channel),
    /// and dbt-core's empty-map channels (the committed playground fixture
    /// emits `"env_vars": {}, "vars": {}` beside a populated `macros`).
    #[test]
    fn parse_manifest_overrides_none_for_every_unset_wire_shape() {
        let tails = [
            "",                                   // fusion/core: key absent
            r#", "overrides": null"#,             // fusion: explicit null blob
            r#", "overrides": {}"#,               // empty blob
            r#", "overrides": { "macros": {} }"#, // core: empty channel
            r#", "overrides": { "macros": null, "vars": null, "env_vars": null }"#,
            r#", "overrides": { "macros": {}, "vars": {}, "env_vars": {} }"#,
        ];
        for tail in tails {
            let json = format!(
                r#"{{
                  "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
                  "unit_tests": {{
                    "unit_test.shop.t": {{
                      "name": "t", "model": "order_events",
                      "given": [], "expect": {{ "rows": [] }}{tail}
                    }}
                  }}
                }}"#
            );
            let manifest = parse_manifest(&json).expect("valid manifest (tolerant)");
            let ut = manifest
                .unit_test("unit_test.shop.t")
                .expect("unit test present");
            assert_eq!(ut.overrides(), None, "tail = {tail:?}");
        }
    }

    /// cute-dbt#200: empty/null channels are dropped INDIVIDUALLY — a
    /// populated `macros` beside core's `"vars": {}` / fusion's
    /// `"vars": null` yields a one-group blob (the committed playground
    /// fixture's exact shape).
    #[test]
    fn parse_manifest_overrides_drops_only_the_empty_channels() {
        for empties in [
            r#""vars": {}, "env_vars": {}"#,
            r#""vars": null, "env_vars": null"#,
        ] {
            let json = format!(
                r#"{{
                  "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
                  "unit_tests": {{
                    "unit_test.shop.t": {{
                      "name": "t", "model": "order_events",
                      "given": [], "expect": {{ "rows": [] }},
                      "overrides": {{ "macros": {{ "is_incremental": true }}, {empties} }}
                    }}
                  }}
                }}"#
            );
            let manifest = parse_manifest(&json).expect("valid manifest");
            let ut = manifest
                .unit_test("unit_test.shop.t")
                .expect("unit test present");
            let overrides = ut.overrides().expect("macros group retained");
            assert_eq!(
                overrides.keys().collect::<Vec<_>>(),
                ["macros"],
                "only the populated channel appears (empties = {empties:?})"
            );
        }
    }

    /// cute-dbt#200: model `description` + `tags` ingest from the node's
    /// TOP-LEVEL wire fields. dbt-core's empty-string unset description is
    /// dropped (the #165 precedent); fusion's absent key and a defensive
    /// explicit `null` both default. `config.tags` is NOT read (real
    /// dbt-core manifests carry merge duplicates there).
    #[test]
    fn parse_manifest_extracts_model_description_and_tags() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.described": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "aa" }},
                  "description": "One row per payer.",
                  "tags": ["marts", "finance"],
                  "config": {{ "tags": ["marts", "finance", "marts", "finance"] }}
                }},
                "model.shop.core_unset": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "bb" }},
                  "description": "",
                  "tags": []
                }},
                "model.shop.fusion_unset": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "cc" }}
                }},
                "model.shop.null_filled": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "dd" }},
                  "description": null,
                  "tags": null
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest");
        let node = |id: &str| manifest.node(&NodeId::new(id)).expect("node present");

        let described = node("model.shop.described");
        assert_eq!(described.description(), Some("One row per payer."));
        assert_eq!(
            described.tags(),
            ["marts".to_owned(), "finance".to_owned()],
            "top-level tags verbatim — never the duplicate-carrying config.tags"
        );

        for id in [
            "model.shop.core_unset",   // core: "" + []
            "model.shop.fusion_unset", // fusion: keys absent
            "model.shop.null_filled",  // defensive: explicit nulls
        ] {
            let n = node(id);
            assert!(n.description().is_none(), "unset description on {id}");
            assert!(n.tags().is_empty(), "unset tags on {id}");
        }
    }

    // ===== cute-dbt#256 — governance + identity wire family ==========

    /// cute-dbt#256: `metadata.project_name` ingests verbatim (both
    /// committed real fixtures populate it — jaffle-shop = dbt-core
    /// 1.11, playground = fusion 2.0-preview); fusion's empty-string
    /// unset default and an absent key both read back as `None` via the
    /// accessor's drop-empty rule.
    #[test]
    fn parse_manifest_extracts_metadata_project_name() {
        let json = format!(
            r#"{{ "metadata": {{ "dbt_schema_version": "{V12_URL}", "project_name": "jaffle_shop" }} }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest");
        assert_eq!(manifest.metadata().project_name(), Some("jaffle_shop"));

        for tail in ["", r#", "project_name": """#, r#", "project_name": null"#] {
            let json =
                format!(r#"{{ "metadata": {{ "dbt_schema_version": "{V12_URL}"{tail} }} }}"#);
            let manifest = parse_manifest(&json).expect("valid manifest");
            assert_eq!(manifest.metadata().project_name(), None, "tail = {tail:?}");
        }
    }

    /// cute-dbt#256: per-node identity (`name` + `package_name`) ingests
    /// from the top-level wire fields. Both engines always populate them
    /// on real wire; absence / explicit null / empty string all degrade
    /// to `None` so [`Node::bare_name`] keeps its leaf fallback.
    #[test]
    fn parse_manifest_extracts_node_identity() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.dim_customers.v2": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "aa" }},
                  "name": "dim_customers",
                  "package_name": "shop"
                }},
                "model.shop.unset": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "bb" }}
                }},
                "model.shop.null_filled": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "cc" }},
                  "name": null,
                  "package_name": null
                }},
                "model.shop.empty": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "dd" }},
                  "name": "",
                  "package_name": ""
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest");
        let node = |id: &str| manifest.node(&NodeId::new(id)).expect("node present");

        let versioned = node("model.shop.dim_customers.v2");
        assert_eq!(versioned.name(), Some("dim_customers"));
        assert_eq!(versioned.package_name(), Some("shop"));
        assert_eq!(
            versioned.bare_name(),
            "dim_customers",
            "the ingested name wins over the .v2 leaf segment"
        );

        for id in [
            "model.shop.unset",
            "model.shop.null_filled",
            "model.shop.empty",
        ] {
            let n = node(id);
            assert!(n.name().is_none(), "unset name on {id}");
            assert!(n.package_name().is_none(), "unset package_name on {id}");
        }
    }

    /// cute-dbt#256: governance fields (`group` + `access`) ingest
    /// verbatim. The committed fixtures' real shapes: dbt-core 1.11 AND
    /// fusion 2.0-preview both emit `"group": null` + `"access":
    /// "protected"` on ungrouped models; a live fusion 2.0-preview.177
    /// compile emits populated `"group": "finance"` / `"access":
    /// "private"` and may OMIT `group` entirely.
    #[test]
    fn parse_manifest_extracts_node_governance() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.customers": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "aa" }},
                  "group": "finance",
                  "access": "private"
                }},
                "model.shop.orders": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "bb" }},
                  "group": null,
                  "access": "protected"
                }},
                "model.shop.fusion_omits": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "cc" }}
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest");
        let node = |id: &str| manifest.node(&NodeId::new(id)).expect("node present");

        assert_eq!(node("model.shop.customers").group(), Some("finance"));
        assert_eq!(node("model.shop.customers").access(), Some("private"));
        assert_eq!(node("model.shop.orders").group(), None);
        assert_eq!(node("model.shop.orders").access(), Some("protected"));
        assert_eq!(node("model.shop.fusion_omits").group(), None);
        assert_eq!(node("model.shop.fusion_omits").access(), None);
    }

    /// cute-dbt#256: `version` / `latest_version` are fusion
    /// `StringOrInteger` on the wire (`dbt-schemas` `serde.rs:419-422` @
    /// `9977b6cb…`) — a live fusion compile emits the bare integer `2`.
    /// Integers normalize to decimal strings; strings pass verbatim;
    /// every other shape (incl. fusion's empty-string default, floats,
    /// bools) degrades to `None`, never an error (ADR-5).
    /// `deprecation_date` is a verbatim optional string.
    #[test]
    fn parse_manifest_normalizes_model_versions() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.versioned_demo.v2": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "aa" }},
                  "version": 2,
                  "latest_version": 3,
                  "deprecation_date": "2027-01-01"
                }},
                "model.shop.string_version.vfinal": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "bb" }},
                  "version": "final",
                  "latest_version": "final"
                }},
                "model.shop.unversioned": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "cc" }},
                  "version": null,
                  "latest_version": null,
                  "deprecation_date": null
                }},
                "model.shop.odd_shapes": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "dd" }},
                  "version": 2.5,
                  "latest_version": ""
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest");
        let node = |id: &str| manifest.node(&NodeId::new(id)).expect("node present");

        let v2 = node("model.shop.versioned_demo.v2");
        assert_eq!(v2.version(), Some("2"), "wire integer → decimal string");
        assert_eq!(v2.latest_version(), Some("3"));
        assert_eq!(v2.deprecation_date(), Some("2027-01-01"));

        let vfinal = node("model.shop.string_version.vfinal");
        assert_eq!(vfinal.version(), Some("final"));
        assert_eq!(vfinal.latest_version(), Some("final"));

        let unversioned = node("model.shop.unversioned");
        assert_eq!(unversioned.version(), None);
        assert_eq!(unversioned.latest_version(), None);
        assert_eq!(unversioned.deprecation_date(), None);

        let odd = node("model.shop.odd_shapes");
        assert_eq!(odd.version(), None, "a float is not a version");
        assert_eq!(odd.latest_version(), None, "empty string is unset");
    }

    // ----- exposures ---------------------------------------------------

    /// cute-dbt#256: a fusion-emitted exposure (shape captured from a
    /// live fusion 2.0-preview.177 compile — owner email as a single
    /// string, `depends_on` carrying the fusion-only
    /// `nodes_with_ref_location` sibling, label/maturity/config extras)
    /// translates to the domain [`Exposure`] with the map key folded
    /// into the id.
    #[test]
    fn parse_manifest_translates_a_fusion_style_exposure() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "exposures": {{
                "exposure.shop.weekly_revenue_dashboard": {{
                  "unique_id": "exposure.shop.weekly_revenue_dashboard",
                  "name": "weekly_revenue_dashboard",
                  "package_name": "shop",
                  "fqn": ["shop", "weekly_revenue_dashboard"],
                  "path": "models/schema.yml",
                  "original_file_path": "models/schema.yml",
                  "description": "Weekly revenue rollup.",
                  "tags": [],
                  "meta": {{}},
                  "depends_on": {{
                    "macros": [],
                    "nodes": ["model.shop.orders"],
                    "nodes_with_ref_location": [["model.shop.orders", {{"line": 1, "col": 1}}]]
                  }},
                  "refs": [{{"name": "orders", "package": null, "version": null}}],
                  "sources": [],
                  "unrendered_config": {{}},
                  "metrics": [],
                  "owner": {{ "email": "data@example.com", "name": "Data Team" }},
                  "label": "Weekly Revenue Dashboard",
                  "maturity": "high",
                  "type": "dashboard",
                  "url": "https://bi.example.com/dashboards/revenue",
                  "config": {{ "enabled": true, "meta": {{}}, "tags": [] }},
                  "resource_type": "exposure"
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("fusion exposure parses");
        assert_eq!(manifest.exposures().len(), 1);
        let exposure = manifest
            .exposures()
            .get(&NodeId::new("exposure.shop.weekly_revenue_dashboard"))
            .expect("keyed by the wire map key");
        assert_eq!(exposure.name(), "weekly_revenue_dashboard");
        assert_eq!(exposure.exposure_type(), Some("dashboard"));
        assert_eq!(
            exposure.url(),
            Some("https://bi.example.com/dashboards/revenue")
        );
        let owner = exposure.owner().expect("owner present");
        assert_eq!(owner.name(), Some("Data Team"));
        assert_eq!(
            owner.email(),
            ["data@example.com".to_owned()],
            "a wire string email normalizes to a one-element list"
        );
        assert_eq!(
            exposure.depends_on().nodes(),
            &[NodeId::new("model.shop.orders")]
        );
    }

    /// cute-dbt#256: the dbt-core dialect (explicit nulls for unset
    /// fields) and fusion's widened `email` array
    /// (`StringOrArrayOfStrings`) both tolerate; an owner whose every
    /// channel is null/empty collapses to `None` (the
    /// `WireUnitTestOverrides::into_grouped` precedent).
    #[test]
    fn parse_manifest_tolerates_exposure_owner_shapes() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "exposures": {{
                "exposure.shop.core_nulls": {{
                  "name": "core_nulls",
                  "type": "notebook",
                  "url": null,
                  "owner": {{ "name": null, "email": null }},
                  "depends_on": {{ "macros": [], "nodes": [] }}
                }},
                "exposure.shop.email_array": {{
                  "name": "email_array",
                  "type": "ml",
                  "owner": {{ "email": ["a@example.com", "b@example.com"] }}
                }},
                "exposure.shop.degenerate": {{}}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("tolerant parse");
        let exposure = |id: &str| {
            manifest
                .exposures()
                .get(&NodeId::new(id))
                .expect("exposure present")
        };

        let core = exposure("exposure.shop.core_nulls");
        assert_eq!(core.url(), None);
        assert!(
            core.owner().is_none(),
            "an all-null owner (fusion serialize_always emits name: null) collapses to None"
        );

        let array = exposure("exposure.shop.email_array");
        let owner = array.owner().expect("owner with email list");
        assert_eq!(owner.name(), None);
        assert_eq!(
            owner.email(),
            ["a@example.com".to_owned(), "b@example.com".to_owned()]
        );

        let degenerate = exposure("exposure.shop.degenerate");
        assert_eq!(
            degenerate.name(),
            "",
            "fail-open empty name (the WireSource precedent)"
        );
        assert!(degenerate.exposure_type().is_none());
        assert!(degenerate.owner().is_none());
        assert!(degenerate.depends_on().nodes().is_empty());
    }

    // ----- groups ------------------------------------------------------

    /// cute-dbt#256: a fusion-emitted group (shape captured from a live
    /// fusion 2.0-preview.177 compile) translates with its owner; the
    /// map key stays the lookup key while nodes join by NAME.
    ///
    /// Discovery (the issue's #145-pattern risk): fusion REQUIRES
    /// `owner:` at parse on an authored group (`GroupProperties.owner:
    /// DbtOwner`, no default — `dbt-schemas` `properties/properties.rs
    /// :120-125` @ `9977b6cb…`; live-verified: compile fails `dbt1013
    /// missing field 'owner'`), so real fusion manifests always carry an
    /// owner OBJECT — but its content may be empty (`DbtOwner` fields
    /// are both optional). cute-dbt stays tolerant on every shape.
    #[test]
    fn parse_manifest_translates_groups() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "groups": {{
                "group.shop.finance": {{
                  "name": "finance",
                  "description": "",
                  "package_name": "shop",
                  "path": "models/schema.yml",
                  "original_file_path": "models/schema.yml",
                  "unique_id": "group.shop.finance",
                  "owner": {{ "email": "finance@example.com", "name": "Finance Team" }},
                  "resource_type": "group"
                }},
                "group.shop.ownerless": {{ "name": "ownerless" }},
                "group.shop.empty_owner": {{ "name": "empty_owner", "owner": {{ "name": null }} }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("groups parse");
        assert_eq!(manifest.groups().len(), 3);

        let finance = manifest.group_by_name("finance").expect("finance group");
        let owner = finance.owner().expect("owner present");
        assert_eq!(owner.name(), Some("Finance Team"));
        assert_eq!(owner.email(), ["finance@example.com".to_owned()]);

        assert!(
            manifest
                .group_by_name("ownerless")
                .expect("ownerless group")
                .owner()
                .is_none(),
            "a group without owner still parses (ADR-5, despite fusion's strictness)"
        );
        assert!(
            manifest
                .group_by_name("empty_owner")
                .expect("empty_owner group")
                .owner()
                .is_none(),
            "a content-free owner collapses to None"
        );
    }

    // ----- checksum-cliff hardening -------------------------------------

    /// cute-dbt#256: a checksum-less (or otherwise malformed-checksum)
    /// node degrades PER NODE to dbt's own no-checksum sentinel —
    /// `FileHash.empty()` = `{name: "none", checksum: ""}` (fusion
    /// `DbtChecksum::default`, `dbt-schemas` `common.rs:929+` @
    /// `9977b6cb…`; the REAL wire shape on every generic-test node in
    /// both committed fixtures) — never a whole-manifest parse failure.
    ///
    /// Comparison semantics follow dbt verbatim (fusion
    /// `check_modified_content` fast path: identical checksums ⇒
    /// unchanged): two sentinel checksums compare EQUAL (deliberate —
    /// generic tests must not show as state:modified on every run),
    /// while sentinel-vs-real DIFFER, so a unit-test target model that
    /// loses its checksum in only one manifest is conservatively
    /// modified ⇒ in scope (the fail-closed direction).
    #[test]
    fn parse_manifest_degrades_checksum_shapes_per_node() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.good": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "deadbeef" }}
                }},
                "model.shop.absent": {{ "resource_type": "model" }},
                "model.shop.null": {{ "resource_type": "model", "checksum": null }},
                "model.shop.bare_string": {{
                  "resource_type": "model",
                  "checksum": "cafebabe"
                }},
                "model.shop.name_only": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256" }}
                }},
                "model.shop.garbage": {{ "resource_type": "model", "checksum": 42 }}
              }}
            }}"#
        );
        let manifest =
            parse_manifest(&json).expect("no checksum shape may fail the whole manifest");
        let checksum = |id: &str| {
            manifest
                .node(&NodeId::new(id))
                .expect("node present")
                .checksum()
                .clone()
        };

        assert_eq!(
            checksum("model.shop.good"),
            Checksum::new("sha256", "deadbeef")
        );
        assert_eq!(
            checksum("model.shop.absent"),
            Checksum::new("none", ""),
            "absent → the dbt FileHash.empty() sentinel"
        );
        assert_eq!(checksum("model.shop.null"), Checksum::new("none", ""));
        assert_eq!(
            checksum("model.shop.bare_string"),
            Checksum::new("", "cafebabe"),
            "fusion's DbtChecksum::String arm keeps the hex, no algorithm name"
        );
        assert_eq!(
            checksum("model.shop.name_only"),
            Checksum::new("sha256", ""),
            "a partial object keeps what it carried"
        );
        assert_eq!(
            checksum("model.shop.garbage"),
            Checksum::new("none", ""),
            "an unrecognizable shape degrades to the sentinel"
        );
    }

    // ===== cute-dbt#257 — contract + column + structure wire family ====

    /// cute-dbt#257: `fqn` ingests verbatim — the #262 C2 config-tree
    /// prefix-matcher input (fusion `get_node_fqn`, `dbt-parser`
    /// `utils.rs:132-159` @ `9977b6cb…`). Populated on every node of
    /// both committed real fixtures; absence and an engine null-fill
    /// both degrade to empty.
    #[test]
    fn parse_manifest_extracts_node_fqn() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.stg_orders": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "aa" }},
                  "fqn": ["shop", "staging", "stg_orders"]
                }},
                "model.shop.unset": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "bb" }}
                }},
                "model.shop.null_filled": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "cc" }},
                  "fqn": null
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest");
        let node = |id: &str| manifest.node(&NodeId::new(id)).expect("node present");
        assert_eq!(
            node("model.shop.stg_orders").fqn(),
            [
                "shop".to_owned(),
                "staging".to_owned(),
                "stg_orders".to_owned()
            ]
        );
        assert!(node("model.shop.unset").fqn().is_empty());
        assert!(node("model.shop.null_filled").fqn().is_empty());
    }

    /// cute-dbt#257: model-level `constraints` + the engine-inferred
    /// `primary_key` + the top-level contract `checksum` ingest from
    /// the wire. The populated constraint shapes are the LIVE-PROBED
    /// engine outputs (dbt-core 1.11.2 resolves FK `to` to the quoted
    /// relation; fusion 2.0-preview keeps the authored `ref(...)`).
    #[test]
    fn parse_manifest_extracts_model_constraints_primary_key_and_contract_checksum() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.fct_encounters": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "aa" }},
                  "constraints": [
                    {{"type":"primary_key","name":null,"expression":null,"warn_unenforced":true,"warn_unsupported":true,"to":null,"to_columns":[],"columns":["encounter_key"]}},
                    {{"type":"foreign_key","name":null,"expression":null,"warn_unenforced":true,"warn_unsupported":true,"to":"\"memory\".\"main_marts\".\"dim_payers\"","to_columns":["payer_key"],"columns":["payer_key"]}}
                  ],
                  "primary_key": ["encounter_key"],
                  "contract": {{"enforced": true, "alias_types": true, "checksum": "0cb79927be0760dd"}}
                }},
                "model.shop.fusion_style": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "bb" }},
                  "constraints": [
                    {{"type":"foreign_key","expression":null,"name":null,"to":"ref('orders')","to_columns":["customer_id"],"columns":["customer_id"],"warn_unsupported":null,"warn_unenforced":null}}
                  ],
                  "primary_key": ["customer_id"],
                  "contract": {{"alias_types": true, "enforced": true}}
                }},
                "model.shop.unset": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "cc" }},
                  "constraints": [],
                  "primary_key": [],
                  "contract": {{"enforced": false, "alias_types": true, "checksum": null}}
                }},
                "model.shop.absent": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "dd" }}
                }},
                "model.shop.null_filled": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "ee" }},
                  "constraints": null,
                  "primary_key": null,
                  "contract": null
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest");
        let node = |id: &str| manifest.node(&NodeId::new(id)).expect("node present");

        // dbt-core dialect: resolved FK `to`, warn_* siblings ignored.
        let core = node("model.shop.fct_encounters");
        assert_eq!(core.constraints().len(), 2);
        assert_eq!(core.constraints()[0].kind(), ConstraintKind::PrimaryKey);
        assert_eq!(
            core.constraints()[0].columns(),
            ["encounter_key".to_owned()]
        );
        let fk = &core.constraints()[1];
        assert_eq!(fk.kind(), ConstraintKind::ForeignKey);
        assert_eq!(fk.to(), Some("\"memory\".\"main_marts\".\"dim_payers\""));
        assert_eq!(fk.to_columns(), ["payer_key".to_owned()]);
        assert_eq!(core.primary_key(), ["encounter_key".to_owned()]);
        assert_eq!(core.contract_checksum(), Some("0cb79927be0760dd"));

        // fusion dialect: authored ref() in `to`, checksum key OMITTED
        // even when enforced (live-verified).
        let fusion = node("model.shop.fusion_style");
        assert_eq!(fusion.constraints()[0].to(), Some("ref('orders')"));
        assert_eq!(fusion.contract_checksum(), None);

        // The real fixture shape: empty arrays + null checksum.
        let unset = node("model.shop.unset");
        assert!(unset.constraints().is_empty());
        assert!(unset.primary_key().is_empty());
        assert_eq!(unset.contract_checksum(), None);

        // Absent keys and engine null-fills both degrade.
        for id in ["model.shop.absent", "model.shop.null_filled"] {
            let n = node(id);
            assert!(n.constraints().is_empty(), "{id}");
            assert!(n.primary_key().is_empty(), "{id}");
            assert_eq!(n.contract_checksum(), None, "{id}");
        }
    }

    /// cute-dbt#257: a non-string contract checksum (defensive — the
    /// fusion type is `Option<YmlValue>`) degrades to `None`, never an
    /// error.
    #[test]
    fn parse_manifest_tolerates_a_non_string_contract_checksum() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.odd": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "aa" }},
                  "contract": {{"enforced": true, "checksum": 42}}
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("tolerant parse");
        assert_eq!(
            manifest
                .node(&NodeId::new("model.shop.odd"))
                .expect("node present")
                .contract_checksum(),
            None
        );
    }

    /// cute-dbt#257: the column-level extension (meta / tags /
    /// policy_tags / constraints) folds into [`ColumnFacts`], keeping
    /// only columns with at least one fact. The populated entry is the
    /// live-probed wire shape (core mirrors meta/tags under the
    /// column's `config` — the nested copy is deliberately not read,
    /// the #200 top-level-tags precedent); the unset entry is the
    /// verbatim dbt-core fixture shape (`meta: {{}}`, `tags: []`,
    /// `constraints: []`, no `policy_tags` key). fusion null-fills
    /// `policy_tags` on undeclared columns.
    #[test]
    fn parse_manifest_extracts_column_facts_and_drops_factless_columns() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.dim_payers": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "aa" }},
                  "columns": {{
                    "payer_key": {{
                      "name": "payer_key",
                      "description": "Surrogate key",
                      "meta": {{"pii": false, "owner": "clinical-quality"}},
                      "data_type": null,
                      "constraints": [{{"type":"not_null","name":null,"expression":null,"warn_unenforced":true,"warn_unsupported":true,"to":null,"to_columns":[]}}],
                      "quote": null,
                      "config": {{"meta": {{"pii": false}}, "tags": ["dimension_key"]}},
                      "tags": ["dimension_key"],
                      "granularity": null,
                      "doc_blocks": []
                    }},
                    "governed": {{
                      "name": "governed",
                      "policy_tags": ["projects/example/locations/us/taxonomies/1/policyTags/2"]
                    }},
                    "factless": {{
                      "name": "factless",
                      "description": "Documented but fact-free",
                      "meta": {{}},
                      "data_type": "integer",
                      "constraints": [],
                      "tags": [],
                      "policy_tags": null
                    }}
                  }}
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest");
        let node = manifest
            .node(&NodeId::new("model.shop.dim_payers"))
            .expect("node present");

        let payer = node
            .column_facts()
            .get("payer_key")
            .expect("facts stored for the populated column");
        assert_eq!(
            payer.meta().and_then(|m| m.get("owner")),
            Some(&serde_json::json!("clinical-quality"))
        );
        assert_eq!(payer.tags(), ["dimension_key".to_owned()]);
        assert!(payer.policy_tags().is_empty());
        assert_eq!(payer.constraints()[0].kind(), ConstraintKind::NotNull);

        let governed = node
            .column_facts()
            .get("governed")
            .expect("policy_tags alone is a fact");
        assert_eq!(
            governed.policy_tags(),
            ["projects/example/locations/us/taxonomies/1/policyTags/2".to_owned()]
        );

        assert!(
            !node.column_facts().contains_key("factless"),
            "core's empty-meta/empty-tags/empty-constraints shape stores nothing"
        );
        // The pre-#257 column surfaces are untouched by the extension.
        assert_eq!(
            node.columns().get("factless"),
            Some(&Some("integer".to_owned()))
        );
        assert_eq!(
            node.column_descriptions()
                .get("factless")
                .map(String::as_str),
            Some("Documented but fact-free")
        );
    }

    /// cute-dbt#105: the schema-properties `patch_path` ingests from the
    /// node's top-level wire field with the `<package>://` URI scheme
    /// stripped (both engines emit the package-URI shape — fusion
    /// `normalize_manifest_patch_path`, `dbt-schemas`
    /// `manifest/manifest.rs` @ `9977b6cb…`, mirroring dbt-core).
    /// dbt-core null-fills unpatched nodes; fusion omits the key; a
    /// scheme-less path (a synthetic fixture or a domain round-trip)
    /// passes through verbatim.
    #[test]
    fn parse_manifest_extracts_patch_path_with_the_package_uri_scheme_stripped() {
        let json = format!(
            r#"{{
              "metadata": {{ "dbt_schema_version": "{V12_URL}" }},
              "nodes": {{
                "model.shop.patched": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "aa" }},
                  "patch_path": "shop://models/marts/_core__models.yml"
                }},
                "model.shop.null_filled": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "bb" }},
                  "patch_path": null
                }},
                "model.shop.fusion_unset": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "cc" }}
                }},
                "model.shop.schemeless": {{
                  "resource_type": "model",
                  "checksum": {{ "name": "sha256", "checksum": "dd" }},
                  "patch_path": "models/schema.yml"
                }}
              }}
            }}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest");
        let node = |id: &str| manifest.node(&NodeId::new(id)).expect("node present");

        assert_eq!(
            node("model.shop.patched").patch_path(),
            Some("models/marts/_core__models.yml"),
            "the package-URI scheme is stripped to the relative path",
        );
        assert!(
            node("model.shop.null_filled").patch_path().is_none(),
            "dbt-core's explicit null tolerates to None",
        );
        assert!(
            node("model.shop.fusion_unset").patch_path().is_none(),
            "fusion's omitted key defaults to None",
        );
        assert_eq!(
            node("model.shop.schemeless").patch_path(),
            Some("models/schema.yml"),
            "a scheme-less path passes through verbatim",
        );
    }
}
