//! Model **grain** detection for the explore model-detail card
//! (cute-dbt#104, epic cute-dbt#99 V5).
//!
//! A model's grain is the column set (or the authored statement) that
//! uniquely identifies one row. This module resolves it by the locked
//! precedence ladder:
//!
//! 1. explicit **`config.meta.grain`** — the authored declaration,
//!    surfaced verbatim (free text on real manifests:
//!    `"patient_id + year_actual"` on the committed playground fixture);
//! 2. a **primary-key-class** data test (`dbt_constraints.primary_key`
//!    — primary key = uniqueness + non-null, the strongest tested claim);
//! 3. a **compound-unique** data test (≥ 2 columns proven unique
//!    together);
//! 4. a **single `unique`** data test (1 column proven unique);
//! 5. nothing detected — the caller renders the grain **explicitly as
//!    "unknown"**, never silently guessed.
//!
//! Inference matches tests to models by the manifest **`attached_node`**
//! linkage (the cute-dbt#103 counting linkage — fusion mirrors
//! dbt-core's `_lookup_attached_node`;
//! `dbt-parser/src/resolve/resolve_tests/resolve_data_tests.rs` @
//! `9977b6cbb1b761065536300037560d8e3c037011`) and recognizes the five
//! uniqueness signatures on the **`(namespace, name)`** tuple of
//! `test_metadata` — a same-named test from a foreign namespace does
//! NOT count (the AC's tuple-matching rule):
//!
//! | namespace          | name                                    | columns kwarg                                  |
//! |--------------------|-----------------------------------------|------------------------------------------------|
//! | (none)             | `unique`                                | `column_name` (string; node-level fallback)    |
//! | `dbt_utils`        | `unique_combination_of_columns`         | `combination_of_columns` (string array)        |
//! | `dbt_constraints`  | `primary_key`                           | `column_names` (string array) / `column_name`  |
//! | `dbt_constraints`  | `unique_key`                            | `column_names` (string array) / `column_name`  |
//! | `dbt_expectations` | `expect_compound_columns_to_be_unique`  | `column_list` (string array)                   |
//! | `dbt_expectations` | `expect_column_values_to_be_unique`     | `column_name` (string; node-level fallback)    |
//!
//! (`dbt_expectations` contributes the compound/single pair — one AC
//! signature family.) Kwarg keys verified against the engine and the
//! providing packages:
//!
//! - fusion's own primary-key inference reads exactly `column_name` /
//!   `combination_of_columns` out of `test_metadata.kwargs`
//!   (`extract_columns_from_metadata`,
//!   `dbt-parser/src/resolve/primary_key_inference.rs` @
//!   `9977b6cbb1b761065536300037560d8e3c037011`) and gates on
//!   `attached_node` + enabled-with-default;
//! - `dbt_constraints` `primary_key`/`unique_key` accept
//!   `column_name`/`column_names` and fold the single form into the
//!   list (`macros/create_constraints.sql`, Snowflake-Labs/dbt_constraints
//!   @ `205b5cfdd51349ab3f4b6db7e8e6faf5224a8e14`);
//! - `dbt_expectations` `expect_compound_columns_to_be_unique(model,
//!   column_list, …)` and `expect_column_values_to_be_unique(model,
//!   column_name, …)` (metaplane/dbt-expectations @
//!   `991651f0afa725e32834d0ee51bb0c08f385e418`).
//!
//! Non-primary-key signatures classify by **arity**: ≥ 2 recovered
//! columns ⇒ compound-unique, exactly 1 ⇒ single-unique (a
//! `unique_combination_of_columns` over one column proves single-column
//! uniqueness — what the test *proves* ranks it, not which macro spelled
//! it). A signature whose columns are not statically recoverable
//! (missing/empty/mixed-type kwargs) yields **no signal** — never a
//! guess. Disabled tests (`config.enabled: false`) assert nothing and
//! are skipped, mirroring fusion's `get_enabled_with_default`.
//!
//! **Every** detected signal is returned, precedence-ordered — the card
//! surfaces all of them (the AC's "all detected grains surfaced").
//!
//! The sibling report-side check `grain.unique-key-unbacked`
//! ([`crate::domain::checks`]) keeps its own narrower recognizer on
//! purpose: it answers "does an enabled uniqueness test back the
//! declared `config.unique_key` subset-soundly", a coverage question
//! with frozen v0.x semantics — not a grain-resolution question. The
//! shared seam between the two is [`test_is_enabled`].

use serde_json::Value;

use crate::domain::manifest::{Manifest, Node};

/// The precedence class of one detected grain signal, ordered
/// strongest-first (the ladder's rungs 1–4; rung 5 — "unknown" — is the
/// empty result, not a variant: absence must never look like a signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GrainKind {
    /// Explicit `config.meta.grain` — the authored declaration.
    Meta,
    /// A primary-key-class data test (`dbt_constraints.primary_key`).
    PrimaryKey,
    /// A compound-unique data test (≥ 2 columns proven unique together).
    CompoundUnique,
    /// A single-column `unique`-class data test.
    Unique,
}

/// One detected grain signal — the kind, the pre-rendered value
/// (`config.meta.grain` verbatim, or the test's columns joined
/// `", "`), and the origin (`"config.meta.grain"` or the test node id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrainSignal {
    /// Precedence class.
    pub kind: GrainKind,
    /// Rendered grain value (never empty for a returned signal).
    pub value: String,
    /// Where the signal came from: the literal `"config.meta.grain"`
    /// or the detecting test's manifest node id.
    pub origin: String,
}

/// Detect every grain signal for `model`, precedence-ordered
/// ([`GrainKind`] first, then origin — deterministic across runs).
///
/// An empty result is the ladder's explicit-"unknown" rung: the caller
/// renders it as such, never guessing. The resolved grain is the first
/// element; the rest are the also-detected signals the card surfaces.
#[must_use]
pub fn model_grain_signals(manifest: &Manifest, model: &Node) -> Vec<GrainSignal> {
    let mut signals: Vec<GrainSignal> = Vec::new();
    if let Some(value) = meta_grain(model) {
        signals.push(GrainSignal {
            kind: GrainKind::Meta,
            value,
            origin: "config.meta.grain".to_owned(),
        });
    }
    for (id, node) in manifest.nodes() {
        if node.resource_type() != "test" || node.attached_node() != Some(model.id()) {
            continue;
        }
        if !test_is_enabled(node) {
            continue;
        }
        if let Some((kind, columns)) = uniqueness_signature(node) {
            signals.push(GrainSignal {
                kind,
                value: columns.join(", "),
                origin: id.as_str().to_owned(),
            });
        }
    }
    signals.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.origin.cmp(&b.origin)));
    signals
}

/// `config.enabled` on a test node, defaulting to enabled — mirrors
/// fusion's `get_enabled_with_default` (a disabled test asserts
/// nothing). Shared with the report-side `grain.unique-key-unbacked`
/// detector ([`crate::domain::checks`]).
#[must_use]
pub fn test_is_enabled(node: &Node) -> bool {
    node.config()
        .config()
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// The authored `config.meta.grain` declaration, rendered:
///
/// - a non-empty string ⇒ verbatim (free text on real manifests);
/// - an array of non-empty strings ⇒ joined `", "`;
/// - any other non-null shape ⇒ its compact JSON (surfaced, not
///   guessed at);
/// - absent / `null` / empty ⇒ `None` (no declaration).
fn meta_grain(model: &Node) -> Option<String> {
    let grain = model.config().config().get("meta")?.get("grain")?;
    match grain {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        // Explicit null and a blank string are both "no declaration".
        Value::Null | Value::String(_) => None,
        Value::Array(items) => {
            let columns: Vec<&str> = items
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .collect();
            if columns.len() == items.len() && !columns.is_empty() {
                Some(columns.join(", "))
            } else {
                Some(grain.to_string())
            }
        }
        other => Some(other.to_string()),
    }
}

/// Recognize one enabled test node as a uniqueness signature on the
/// `(namespace, name)` tuple, returning its precedence class and the
/// proven column set. `None` ⇒ not a recognized uniqueness test or its
/// columns are not statically recoverable (no signal — never a guess).
fn uniqueness_signature(node: &Node) -> Option<(GrainKind, Vec<String>)> {
    let test_metadata = node.test_metadata()?;
    let columns = match (test_metadata.namespace(), test_metadata.name()) {
        (None, "unique") | (Some("dbt_expectations"), "expect_column_values_to_be_unique") => {
            single_column(node)?
        }
        (Some("dbt_utils"), "unique_combination_of_columns") => {
            string_array_kwarg(node, "combination_of_columns")?
        }
        (Some("dbt_constraints"), "primary_key" | "unique_key") => {
            // The package folds a single `column_name` into the
            // `column_names` list itself; both wire shapes occur.
            string_array_kwarg(node, "column_names").or_else(|| single_column(node))?
        }
        (Some("dbt_expectations"), "expect_compound_columns_to_be_unique") => {
            string_array_kwarg(node, "column_list")?
        }
        _ => return None,
    };
    let kind = match (test_metadata.namespace(), test_metadata.name()) {
        (Some("dbt_constraints"), "primary_key") => GrainKind::PrimaryKey,
        _ if columns.len() >= 2 => GrainKind::CompoundUnique,
        _ => GrainKind::Unique,
    };
    Some((kind, columns))
}

/// The single proven column: `kwargs.column_name` (string), falling
/// back to the node-level `column_name` (the cute-dbt#166 ingestion) —
/// fusion's `extract_columns_from_metadata` order.
fn single_column(node: &Node) -> Option<Vec<String>> {
    let column = node
        .test_metadata()
        .and_then(|m| m.kwargs().get("column_name"))
        .and_then(Value::as_str)
        .or_else(|| node.column_name())?;
    let column = column.trim();
    (!column.is_empty()).then(|| vec![column.to_owned()])
}

/// A non-empty all-strings array kwarg, returned **whole** (the set is
/// the grain — kept composite, never flattened per column; the
/// cute-dbt#169 soundness rule). Mixed-type or empty arrays are not
/// statically recoverable ⇒ `None`.
fn string_array_kwarg(node: &Node, key: &str) -> Option<Vec<String>> {
    let items = node.test_metadata()?.kwargs().get(key)?.as_array()?;
    let columns: Vec<String> = items
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    (columns.len() == items.len() && !columns.is_empty()).then_some(columns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap};

    use serde_json::json;

    use crate::domain::manifest::{
        Checksum, DependsOn, ManifestMetadata, NodeConfig, NodeId, TestMetadata,
    };

    const MODEL_ID: &str = "model.shop.dim_orders";

    fn model_with_config(config: BTreeMap<String, Value>) -> Node {
        Node::new(
            NodeId::new(MODEL_ID),
            "model",
            Checksum::new("sha256", "ck"),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        )
    }

    fn plain_model() -> Node {
        model_with_config(BTreeMap::new())
    }

    fn meta_grain_model(grain: Value) -> Node {
        let mut meta = serde_json::Map::new();
        meta.insert("grain".to_owned(), grain);
        let mut config = BTreeMap::new();
        config.insert("meta".to_owned(), Value::Object(meta));
        model_with_config(config)
    }

    /// A generic uniqueness-test node attached to the model under test.
    fn test_node(
        id: &str,
        namespace: Option<&str>,
        name: &str,
        kwargs: Value,
        enabled: Option<bool>,
    ) -> Node {
        let mut config = BTreeMap::new();
        if let Some(enabled) = enabled {
            config.insert("enabled".to_owned(), Value::Bool(enabled));
        }
        Node::new(
            NodeId::new(id),
            "test",
            Checksum::new("none", ""),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::new(config, false),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            None,
            Some(NodeId::new(MODEL_ID)),
            Some(TestMetadata::new(
                name,
                namespace.map(str::to_owned),
                kwargs,
            )),
        )
    }

    fn manifest_of(nodes: Vec<Node>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn signals_for(model: Node, tests: Vec<Node>) -> Vec<GrainSignal> {
        let model_id = model.id().clone();
        let mut nodes = vec![model];
        nodes.extend(tests);
        let manifest = manifest_of(nodes);
        let model = manifest.node(&model_id).expect("model under test present");
        model_grain_signals(&manifest, model)
    }

    // ----- rung 1: explicit config.meta.grain -------------------------

    #[test]
    fn meta_grain_wins_over_every_inferred_signal() {
        let pk = test_node(
            "test.shop.pk",
            Some("dbt_constraints"),
            "primary_key",
            json!({ "column_names": ["order_id"] }),
            None,
        );
        let signals = signals_for(
            meta_grain_model(Value::String("order_id + order_date".to_owned())),
            vec![pk],
        );
        assert_eq!(signals[0].kind, GrainKind::Meta);
        assert_eq!(signals[0].value, "order_id + order_date");
        assert_eq!(signals[0].origin, "config.meta.grain");
        // ALL detected grains surfaced — the PK signal rides along.
        assert_eq!(signals.len(), 2, "{signals:?}");
        assert_eq!(signals[1].kind, GrainKind::PrimaryKey);
    }

    #[test]
    fn meta_grain_array_of_strings_joins_composite() {
        let signals = signals_for(
            meta_grain_model(json!(["customer_id", "order_date"])),
            vec![],
        );
        assert_eq!(signals[0].value, "customer_id, order_date");
    }

    #[test]
    fn meta_grain_non_string_shape_is_surfaced_not_guessed() {
        let signals = signals_for(meta_grain_model(json!(7)), vec![]);
        assert_eq!(signals[0].kind, GrainKind::Meta);
        assert_eq!(signals[0].value, "7", "compact JSON, surfaced verbatim");
    }

    #[test]
    fn meta_grain_null_or_empty_is_no_declaration() {
        assert!(signals_for(meta_grain_model(Value::Null), vec![]).is_empty());
        assert!(signals_for(meta_grain_model(json!("  ")), vec![]).is_empty());
    }

    // ----- rung 2: primary-key-class test ------------------------------

    #[test]
    fn primary_key_test_beats_compound_unique() {
        let pk = test_node(
            "test.shop.pk",
            Some("dbt_constraints"),
            "primary_key",
            json!({ "column_names": ["order_id"] }),
            None,
        );
        let combo = test_node(
            "test.shop.combo",
            Some("dbt_utils"),
            "unique_combination_of_columns",
            json!({ "combination_of_columns": ["customer_id", "order_date"] }),
            None,
        );
        let signals = signals_for(plain_model(), vec![combo, pk]);
        assert_eq!(signals[0].kind, GrainKind::PrimaryKey);
        assert_eq!(signals[0].value, "order_id");
        assert_eq!(signals[0].origin, "test.shop.pk");
        assert_eq!(signals[1].kind, GrainKind::CompoundUnique);
        assert_eq!(signals[1].value, "customer_id, order_date");
    }

    #[test]
    fn primary_key_class_holds_for_compound_keys_and_column_name_fallback() {
        // Compound column_names stays PrimaryKey-class (the class is the
        // claim, not the arity).
        let compound = test_node(
            "test.shop.pk2",
            Some("dbt_constraints"),
            "primary_key",
            json!({ "column_names": ["a", "b"] }),
            None,
        );
        let signals = signals_for(plain_model(), vec![compound]);
        assert_eq!(signals[0].kind, GrainKind::PrimaryKey);
        assert_eq!(signals[0].value, "a, b");
        // The package's single column_name form.
        let single = test_node(
            "test.shop.pk1",
            Some("dbt_constraints"),
            "primary_key",
            json!({ "column_name": "order_id" }),
            None,
        );
        let signals = signals_for(plain_model(), vec![single]);
        assert_eq!(signals[0].kind, GrainKind::PrimaryKey);
        assert_eq!(signals[0].value, "order_id");
    }

    // ----- rung 3: compound-unique test --------------------------------

    #[test]
    fn compound_unique_beats_single_unique() {
        let combo = test_node(
            "test.shop.combo",
            Some("dbt_utils"),
            "unique_combination_of_columns",
            json!({ "combination_of_columns": ["customer_id", "order_date"] }),
            None,
        );
        let single = test_node(
            "test.shop.unique",
            None,
            "unique",
            json!({ "column_name": "surrogate_key" }),
            None,
        );
        let signals = signals_for(plain_model(), vec![single, combo]);
        assert_eq!(signals[0].kind, GrainKind::CompoundUnique);
        assert_eq!(signals[0].value, "customer_id, order_date");
        assert_eq!(signals[1].kind, GrainKind::Unique);
        assert_eq!(signals[1].value, "surrogate_key");
    }

    #[test]
    fn dbt_constraints_unique_key_classifies_by_arity() {
        let compound = test_node(
            "test.shop.uk2",
            Some("dbt_constraints"),
            "unique_key",
            json!({ "column_names": ["a", "b"] }),
            None,
        );
        let signals = signals_for(plain_model(), vec![compound]);
        assert_eq!(signals[0].kind, GrainKind::CompoundUnique);
        let single = test_node(
            "test.shop.uk1",
            Some("dbt_constraints"),
            "unique_key",
            json!({ "column_names": ["a"] }),
            None,
        );
        let signals = signals_for(plain_model(), vec![single]);
        assert_eq!(signals[0].kind, GrainKind::Unique);
    }

    #[test]
    fn dbt_expectations_compound_and_single_are_recognized() {
        let compound = test_node(
            "test.shop.exp2",
            Some("dbt_expectations"),
            "expect_compound_columns_to_be_unique",
            json!({ "column_list": ["a", "b"] }),
            None,
        );
        let single = test_node(
            "test.shop.exp1",
            Some("dbt_expectations"),
            "expect_column_values_to_be_unique",
            json!({ "column_name": "c" }),
            None,
        );
        let signals = signals_for(plain_model(), vec![compound, single]);
        assert_eq!(signals[0].kind, GrainKind::CompoundUnique);
        assert_eq!(signals[0].value, "a, b");
        assert_eq!(signals[1].kind, GrainKind::Unique);
        assert_eq!(signals[1].value, "c");
    }

    // ----- rung 4: single unique test -----------------------------------

    #[test]
    fn single_native_unique_infers_the_grain() {
        let single = test_node(
            "test.shop.unique",
            None,
            "unique",
            json!({ "column_name": "order_id" }),
            None,
        );
        let signals = signals_for(plain_model(), vec![single]);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].kind, GrainKind::Unique);
        assert_eq!(signals[0].value, "order_id");
        assert_eq!(signals[0].origin, "test.shop.unique");
    }

    #[test]
    fn native_unique_falls_back_to_node_level_column_name() {
        // The kwargs-less wire shape: column_name only at node level
        // (the cute-dbt#166 ingestion path fusion's extractor mirrors).
        let mut node = test_node("test.shop.unique", None, "unique", json!({}), None);
        node = node.with_test_attachment(
            Some("order_id".to_owned()),
            Some(NodeId::new(MODEL_ID)),
            Some(TestMetadata::new("unique", None, json!({}))),
        );
        let signals = signals_for(plain_model(), vec![node]);
        assert_eq!(signals[0].value, "order_id");
    }

    // ----- rung 5: explicit unknown --------------------------------------

    #[test]
    fn no_signal_is_an_empty_result_never_a_guess() {
        assert!(signals_for(plain_model(), vec![]).is_empty());
        // A not_null test is not a uniqueness signature.
        let not_null = test_node(
            "test.shop.not_null",
            None,
            "not_null",
            json!({ "column_name": "order_id" }),
            None,
        );
        assert!(signals_for(plain_model(), vec![not_null]).is_empty());
    }

    // ----- the (namespace, name) tuple rule ------------------------------

    #[test]
    fn foreign_namespace_does_not_match_a_signature() {
        // A same-named test from an unrecognized namespace must NOT
        // count — the tuple is the signature, not the bare name.
        let foreign_combo = test_node(
            "test.shop.combo",
            Some("acme_utils"),
            "unique_combination_of_columns",
            json!({ "combination_of_columns": ["a", "b"] }),
            None,
        );
        let namespaced_unique = test_node(
            "test.shop.unique",
            Some("acme_utils"),
            "unique",
            json!({ "column_name": "a" }),
            None,
        );
        assert!(signals_for(plain_model(), vec![foreign_combo, namespaced_unique]).is_empty());
    }

    // ----- guards ---------------------------------------------------------

    #[test]
    fn disabled_tests_never_signal() {
        let disabled = test_node(
            "test.shop.unique",
            None,
            "unique",
            json!({ "column_name": "order_id" }),
            Some(false),
        );
        assert!(signals_for(plain_model(), vec![disabled]).is_empty());
    }

    #[test]
    fn tests_attached_to_other_models_never_signal() {
        let elsewhere = test_node(
            "test.shop.unique",
            None,
            "unique",
            json!({ "column_name": "order_id" }),
            None,
        )
        .with_test_attachment(
            None,
            Some(NodeId::new("model.shop.other")),
            Some(TestMetadata::new(
                "unique",
                None,
                json!({ "column_name": "order_id" }),
            )),
        );
        assert!(signals_for(plain_model(), vec![elsewhere]).is_empty());
    }

    #[test]
    fn unrecoverable_kwargs_yield_no_signal() {
        // Mixed-type combination array.
        let mixed = test_node(
            "test.shop.combo",
            Some("dbt_utils"),
            "unique_combination_of_columns",
            json!({ "combination_of_columns": ["a", 3] }),
            None,
        );
        // Empty column_list.
        let empty = test_node(
            "test.shop.exp",
            Some("dbt_expectations"),
            "expect_compound_columns_to_be_unique",
            json!({ "column_list": [] }),
            None,
        );
        // unique with no column anywhere.
        let bare = test_node("test.shop.unique", None, "unique", json!({}), None);
        assert!(signals_for(plain_model(), vec![mixed, empty, bare]).is_empty());
    }

    #[test]
    fn signals_are_deterministically_ordered_within_a_kind() {
        let b = test_node(
            "test.shop.b_unique",
            None,
            "unique",
            json!({ "column_name": "b" }),
            None,
        );
        let a = test_node(
            "test.shop.a_unique",
            None,
            "unique",
            json!({ "column_name": "a" }),
            None,
        );
        let signals = signals_for(plain_model(), vec![b, a]);
        let origins: Vec<&str> = signals.iter().map(|s| s.origin.as_str()).collect();
        assert_eq!(origins, vec!["test.shop.a_unique", "test.shop.b_unique"]);
    }
}
