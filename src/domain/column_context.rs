//! Per-column **context** — the Tier-2 column-lineage fold (cute-dbt#446,
//! CLL-1). Pure POD + a pure manifest fold; **no parser, no I/O** (the
//! `tests/domain_clean_arch.rs` gate). This is the EASY half of the
//! column-lineage feasibility study (`docs/design/column-lineage-feasibility.md`
//! §6 A.a/A.c, Tier 2): per-column **definition** (`data_type` +
//! `description`) and **tested-by** (each test's kind + kwargs + node id),
//! both read straight off the already-ingested manifest — the render index
//! the report's column tooltips use (`render.rs` `column_meta_for_model`)
//! already proves the data is present.
//!
//! It ships BEFORE any column edges (the Tier-1 provenance pass is CLL-2):
//! the `column_lineage.context` payload section is useful on definition +
//! tests alone, so the column-selection UX is validated before the heavier
//! edge work. The honesty contract (never-a-false-claim): a column that the
//! SQL projects but the author never documented is NOT silently dropped —
//! it surfaces with `documented: false` ("derived in SQL, undocumented") and
//! an empty `tests` list, so the panel can say "this column exists but has no
//! declared type / prose / test", never inventing one.
//!
//! The fold is keyed by `(attached_node, column)` — the same key the render
//! index uses — and is deterministic: a `BTreeMap` over column names, with
//! each column's tests sorted by `(kind, node_id)` (`Manifest::nodes` is a
//! `HashMap` with no inherent order).

use crate::domain::manifest::{Manifest, Node, NodeId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// One test attached to a column — the "tested by" fact (cute-dbt#446).
/// A pure projection of a manifest `test` node whose `attached_node` is the
/// owning model/seed/source and whose `column_name` names the column.
///
/// - `kind` — the generic test's bare name (`"not_null"`, `"unique"`,
///   `"accepted_values"`, …) from `test_metadata.name`; for a **singular**
///   (SQL-file) test there is no `test_metadata`, so `kind` is the sentinel
///   [`SINGULAR_KIND`] (the panel renders it as a custom/singular chip).
/// - `kwargs` — the rendered test arguments verbatim (`accepted_values.values`,
///   `relationships.to`/`field`, …), an untyped passthrough of the manifest
///   `test_metadata.kwargs`. Empty (`{}`) for `not_null`/`unique` and for
///   singular tests.
/// - `node_id` — the test node's id (`test.…`), the stable handle the report
///   can link back to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestFact {
    /// Generic-test name, or [`SINGULAR_KIND`] for a SQL-file test.
    pub kind: String,
    /// Rendered test arguments, untyped passthrough. `{}` when none.
    #[serde(default)]
    pub kwargs: Value,
    /// The test node's id (`test.…`).
    pub node_id: String,
}

/// The `kind` of a singular (SQL-file) column-attached test — there is no
/// `test_metadata.name` to read, so this sentinel stands in. Public so the
/// render layer (and tests) can match it without a magic string.
pub const SINGULAR_KIND: &str = "singular";

/// Per-column context (cute-dbt#446, CLL-1): the declared definition plus the
/// tested-by list. A PURE manifest fold — `(attached_node, column) → tests`,
/// merged with the column's declared `data_type` + `description`.
///
/// `documented` is the honesty flag: `true` when the column appears in the
/// node's authored `columns:` block (so a `data_type` and/or `description`
/// may be present), `false` when the column is only known via an attached
/// test or the projection — "derived in SQL, undocumented". A `documented:
/// false` column never carries a fabricated type or prose; both stay `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnContext {
    /// Declared SQL `data_type` — `Some` only for a documented column whose
    /// `columns:` entry carries a type. Never inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    /// Authored prose — `Some` only for a documented column with a non-empty
    /// `description`. Never invented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Tests attached to this column, deterministic (sorted by
    /// `(kind, node_id)`). Empty for an untested column.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<TestFact>,
    /// `true` ⇒ the column is in the node's authored `columns:` block; `false`
    /// ⇒ "derived in SQL, undocumented" (known only via a test / projection).
    pub documented: bool,
}

/// Fold one node's per-column context (cute-dbt#446) — the `column_lineage.context`
/// section. Pure: reads `node.columns()` (declared set + `data_type`),
/// `node.column_descriptions()` (authored prose), and every manifest `test`
/// node attached to this node at a named column.
///
/// A column appears in the result if it is EITHER declared in the node's
/// `columns:` block OR named by ≥1 attached column-scoped test — so an
/// undocumented-but-tested column is never lost, and a documented column with
/// no tests still surfaces its definition. The honesty rule: `documented`
/// reflects ONLY whether the column is in the authored `columns:` block;
/// `data_type` / `description` are filled only from that block.
///
/// Deterministic: the outer map is a `BTreeMap` over column names; each
/// column's `tests` is sorted by `(kind, node_id)`.
#[must_use]
pub fn column_contexts(manifest: &Manifest, node: &Node) -> BTreeMap<String, ColumnContext> {
    let mut out: BTreeMap<String, ColumnContext> = BTreeMap::new();

    // Declared columns drive `documented` + the definition fields.
    for (column, data_type) in node.columns() {
        let ctx = out.entry(column.clone()).or_insert_with(|| ColumnContext {
            data_type: None,
            description: None,
            tests: Vec::new(),
            documented: false,
        });
        ctx.documented = true;
        ctx.data_type.clone_from(data_type);
    }
    for (column, description) in node.column_descriptions() {
        let ctx = out.entry(column.clone()).or_insert_with(|| ColumnContext {
            data_type: None,
            description: None,
            tests: Vec::new(),
            documented: true,
        });
        // A description implies the column is documented even if the
        // `columns:` value map omitted it (defensive — fusion/core both
        // emit both, but the two maps are independent accessors).
        ctx.documented = true;
        ctx.description = Some(description.clone());
    }

    // Attach every column-scoped test (generic OR singular). A test names its
    // column via `column_name` and its owner via `attached_node`; an attached
    // test on an UNDOCUMENTED column adds a `documented: false` entry (the
    // honesty surface — the column exists, the author never documented it).
    for fact in attached_column_tests(manifest, node.id()) {
        let (column, fact) = fact;
        out.entry(column)
            .or_insert_with(|| ColumnContext {
                data_type: None,
                description: None,
                tests: Vec::new(),
                documented: false,
            })
            .tests
            .push(fact);
    }

    // Deterministic test order within each column.
    for ctx in out.values_mut() {
        ctx.tests.sort_by(|a, b| {
            (a.kind.as_str(), a.node_id.as_str()).cmp(&(b.kind.as_str(), b.node_id.as_str()))
        });
    }
    out
}

/// Every column-scoped `test` node attached to `attached`, as
/// `(column, TestFact)` pairs — the tested-by half of the fold. Handles both
/// **generic** tests (a `test_metadata.name` → `kind` + its `kwargs`) and
/// **singular** SQL-file tests (no `test_metadata` → [`SINGULAR_KIND`], empty
/// kwargs). A test counts only when it carries a `column_name` (column-scoped);
/// model-level tests are out of scope here (they are not column context).
fn attached_column_tests(manifest: &Manifest, attached: &NodeId) -> Vec<(String, TestFact)> {
    manifest
        .nodes()
        .iter()
        .filter_map(|(id, node)| {
            if node.resource_type() != "test" || node.attached_node() != Some(attached) {
                return None;
            }
            let column = node.column_name()?;
            let fact = match node.test_metadata() {
                Some(tm) => TestFact {
                    kind: tm.name().to_owned(),
                    kwargs: tm.kwargs().clone(),
                    node_id: id.as_str().to_owned(),
                },
                None => TestFact {
                    kind: SINGULAR_KIND.to_owned(),
                    kwargs: Value::Null,
                    node_id: id.as_str().to_owned(),
                },
            };
            Some((column.to_owned(), fact))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{
        Checksum, DependsOn, Manifest, ManifestMetadata, Node, NodeConfig, TestMetadata,
    };
    use serde_json::json;
    use std::collections::{BTreeMap, HashMap};

    fn checksum(value: &str) -> Checksum {
        Checksum::new("sha256", value)
    }

    /// A model node with an authored `columns:` block (name → `data_type`) and
    /// per-column descriptions.
    fn documented_model(
        id: &str,
        columns: &[(&str, Option<&str>)],
        descriptions: &[(&str, &str)],
    ) -> Node {
        let cols: BTreeMap<String, Option<String>> = columns
            .iter()
            .map(|(n, t)| ((*n).to_owned(), t.map(str::to_owned)))
            .collect();
        let descs: BTreeMap<String, String> = descriptions
            .iter()
            .map(|(n, d)| ((*n).to_owned(), (*d).to_owned()))
            .collect();
        Node::new(
            NodeId::new(id),
            "model",
            checksum(id),
            Some("select 1".to_owned()),
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            cols,
        )
        .with_column_descriptions(descs)
    }

    /// A column-scoped GENERIC test node attached to `attached` at `column`.
    fn generic_test(id: &str, attached: &str, column: &str, name: &str, kwargs: Value) -> Node {
        Node::new(
            NodeId::new(id),
            "test",
            checksum(id),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            Some(column.to_owned()),
            Some(NodeId::new(attached)),
            Some(TestMetadata::new(name, None, kwargs)),
        )
    }

    /// A column-scoped SINGULAR test (no `test_metadata`).
    fn singular_test(id: &str, attached: &str, column: &str) -> Node {
        Node::new(
            NodeId::new(id),
            "test",
            checksum(id),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(Some(column.to_owned()), Some(NodeId::new(attached)), None)
    }

    fn manifest_of(nodes: Vec<Node>) -> Manifest {
        Manifest::new(
            ManifestMetadata::new("v12"),
            nodes.into_iter().map(|n| (n.id().clone(), n)).collect(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    #[test]
    fn documented_column_carries_type_description_and_tests() {
        let model = documented_model(
            "model.shop.customers",
            &[("customer_id", Some("integer"))],
            &[("customer_id", "PK of the customer")],
        );
        let not_null = generic_test(
            "test.shop.not_null_customers_customer_id",
            "model.shop.customers",
            "customer_id",
            "not_null",
            json!({}),
        );
        let unique = generic_test(
            "test.shop.unique_customers_customer_id",
            "model.shop.customers",
            "customer_id",
            "unique",
            json!({}),
        );
        let m = manifest_of(vec![model.clone(), not_null, unique]);
        let ctx = column_contexts(&m, &model);

        let col = ctx.get("customer_id").expect("documented column present");
        assert!(col.documented, "in the columns: block ⇒ documented");
        assert_eq!(col.data_type.as_deref(), Some("integer"));
        assert_eq!(col.description.as_deref(), Some("PK of the customer"));
        // Sorted by (kind, node_id): not_null < unique.
        assert_eq!(col.tests.len(), 2);
        assert_eq!(col.tests[0].kind, "not_null");
        assert_eq!(col.tests[1].kind, "unique");
        assert_eq!(
            col.tests[0].node_id,
            "test.shop.not_null_customers_customer_id"
        );
    }

    #[test]
    fn sql_only_column_is_undocumented_with_empty_tests() {
        // A column known ONLY via an attached test (never declared) ⇒
        // documented: false, honest "derived in SQL", and — if it had no
        // tests at all it would not appear; here it has one test, so it
        // appears but stays undocumented with no type/description.
        let model = documented_model("model.shop.orders", &[], &[]);
        let test = generic_test(
            "test.shop.not_null_orders_lifetime_value",
            "model.shop.orders",
            "lifetime_value",
            "not_null",
            json!({}),
        );
        let m = manifest_of(vec![model.clone(), test]);
        let ctx = column_contexts(&m, &model);

        let col = ctx.get("lifetime_value").expect("tested column present");
        assert!(!col.documented, "never declared ⇒ undocumented");
        assert_eq!(col.data_type, None);
        assert_eq!(col.description, None);
        assert_eq!(col.tests.len(), 1);
    }

    #[test]
    fn documented_column_without_tests_still_surfaces_its_definition() {
        // Tier-2 ships with ZERO edges; a documented untested column is
        // useful on definition alone.
        let model = documented_model(
            "model.shop.dim_date",
            &[("date_day", Some("date"))],
            &[("date_day", "Calendar day")],
        );
        let m = manifest_of(vec![model.clone()]);
        let ctx = column_contexts(&m, &model);

        let col = ctx.get("date_day").expect("documented column present");
        assert!(col.documented);
        assert_eq!(col.data_type.as_deref(), Some("date"));
        assert_eq!(col.description.as_deref(), Some("Calendar day"));
        assert!(col.tests.is_empty(), "no edges, no tests — still rendered");
    }

    #[test]
    fn tested_by_index_maps_generic_and_singular() {
        let model = documented_model("model.shop.payments", &[("amount", Some("numeric"))], &[]);
        let accepted = generic_test(
            "test.shop.accepted_values_payments_amount",
            "model.shop.payments",
            "amount",
            "accepted_values",
            json!({ "values": [1, 2, 3] }),
        );
        let singular = singular_test(
            "test.shop.assert_amount_positive",
            "model.shop.payments",
            "amount",
        );
        let m = manifest_of(vec![model.clone(), accepted, singular]);
        let ctx = column_contexts(&m, &model);

        let col = ctx.get("amount").expect("column present");
        assert_eq!(col.tests.len(), 2);
        // Sorted by (kind, node_id): "accepted_values" < "singular".
        assert_eq!(col.tests[0].kind, "accepted_values");
        assert_eq!(col.tests[0].kwargs, json!({ "values": [1, 2, 3] }));
        assert_eq!(col.tests[1].kind, SINGULAR_KIND);
        assert_eq!(col.tests[1].kwargs, Value::Null);
        assert_eq!(col.tests[1].node_id, "test.shop.assert_amount_positive");
    }

    #[test]
    fn context_renders_with_zero_edges() {
        // The whole point of CLL-1: no `edges` are produced here; the fold is
        // purely (definition + tested-by). A model with only documented
        // columns yields a non-empty context and references no edge data.
        let model = documented_model(
            "model.shop.stg_x",
            &[("a", Some("text")), ("b", None)],
            &[("a", "The a column")],
        );
        let m = manifest_of(vec![model.clone()]);
        let ctx = column_contexts(&m, &model);

        assert_eq!(ctx.len(), 2);
        assert!(ctx["a"].documented);
        assert_eq!(ctx["a"].description.as_deref(), Some("The a column"));
        assert!(
            ctx["b"].documented,
            "declared without a type is still documented"
        );
        assert_eq!(ctx["b"].data_type, None);
    }

    #[test]
    fn a_model_level_test_is_not_column_context() {
        // A test with NO column_name (model-level) must not leak into the
        // per-column index.
        let model = documented_model("model.shop.m", &[("k", Some("int"))], &[]);
        let model_level = Node::new(
            NodeId::new("test.shop.model_level"),
            "test",
            checksum("ml"),
            None,
            None,
            DependsOn::default(),
            None,
            NodeConfig::default(),
            None,
            BTreeMap::new(),
        )
        .with_test_attachment(
            None,
            Some(NodeId::new("model.shop.m")),
            Some(TestMetadata::new("custom", None, json!({}))),
        );
        let m = manifest_of(vec![model.clone(), model_level]);
        let ctx = column_contexts(&m, &model);

        assert!(
            ctx["k"].tests.is_empty(),
            "model-level test is not column context"
        );
    }

    #[test]
    fn serde_round_trip_over_column_context_and_test_fact() {
        let ctx = ColumnContext {
            data_type: Some("integer".to_owned()),
            description: Some("PK".to_owned()),
            documented: true,
            tests: vec![
                TestFact {
                    kind: "not_null".to_owned(),
                    kwargs: json!({}),
                    node_id: "test.x".to_owned(),
                },
                TestFact {
                    kind: "accepted_values".to_owned(),
                    kwargs: json!({ "values": ["a", "b"] }),
                    node_id: "test.y".to_owned(),
                },
            ],
        };
        let json = serde_json::to_string(&ctx).expect("serialize");
        let back: ColumnContext = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ctx, back);

        // An undocumented, untested column serializes minimally: only
        // `documented: false` survives (the skip_serializing_if elisions).
        let bare = ColumnContext {
            data_type: None,
            description: None,
            tests: Vec::new(),
            documented: false,
        };
        let json = serde_json::to_string(&bare).expect("serialize");
        assert_eq!(json, r#"{"documented":false}"#);
        let back: ColumnContext = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(bare, back);
    }
}
