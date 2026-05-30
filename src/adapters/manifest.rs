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
    Checksum, DependsOn, Manifest, ManifestMetadata, Node, NodeConfig, NodeId, PreflightError,
    UnitTest, UnitTestExpect, UnitTestGiven,
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
/// lifted into the domain [`NodeConfig`] by [`WireNode::into_node_config`]
/// (the config dict passes through; `config.contract.enforced` is hoisted
/// to a flat bool). `columns` is the model's column map; only each
/// column's declared `data_type` is consumed (the contract column-set
/// diff). All ADR-5-tolerant: each defaults so older / synthetic
/// manifests still deserialize.
#[derive(Debug, Deserialize)]
struct WireNode {
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
    config: WireNodeConfig,
    #[serde(default)]
    relation_name: Option<String>,
    #[serde(default)]
    columns: BTreeMap<String, WireColumn>,
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

/// Tolerant wire projection of one `columns` map entry — only the
/// declared `data_type` is consumed for the `.contract` column-set diff.
#[derive(Debug, Default, Deserialize)]
struct WireColumn {
    #[serde(default)]
    data_type: Option<String>,
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

impl WireUnitTest {
    /// Translate the wire projection into the domain [`UnitTest`], lifting
    /// `config.tags` and `config.meta` out of the nested `config` block and
    /// keeping `original_file_path` from the top level.
    fn into_domain(self) -> UnitTest {
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
                let columns = wire
                    .columns
                    .into_iter()
                    .map(|(name, col)| (name, col.data_type))
                    .collect();
                let config = wire.config.into_domain();
                let node = Node::new(
                    id.clone(),
                    wire.resource_type,
                    wire.checksum,
                    wire.compiled_code,
                    wire.raw_code,
                    wire.depends_on,
                    wire.original_file_path,
                    config,
                    wire.relation_name,
                    columns,
                );
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
        Manifest::new(self.metadata, nodes, unit_tests, macros)
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
    }
}
