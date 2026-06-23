//! Cross-model column lineage (cute-dbt#450, CLL-4) — the REAL multi-model
//! manifest path.
//!
//! These tests build a genuine multi-model manifest with real `compiled_code`,
//! run it through the EXPLORER's project-graph builder
//! (`adapters::explore::build_cross_model_columns`, which parses every model
//! via the public `parse_cte_graph` and stitches on the normalized join key),
//! and assert the honesty-critical seams: the normalized `(db,schema,ident)`
//! join key never mis-attributes, the cross-model trace walks to a source/seed
//! leaf, the blast-radius reaches downstream columns, the star discipline
//! resolves over a known modeled upstream / stays Opaque over an unknown
//! external, and the SCOPE-AS-PARAMETER boundary holds (the project graph is
//! built ONLY in the explorer arm — never the report path).
//!
//! The join is exercised through the real parse path; it is never
//! hand-authored.

use std::collections::{BTreeMap, HashMap};

use cute_dbt::adapters::cte_engine::{TERMINAL_NODE_NAME, parse_cte_graph};
use cute_dbt::adapters::explore::build_cross_model_columns;
use cute_dbt::domain::{
    Checksum, DependsOn, Manifest, ManifestMetadata, ModelLineage, ModelOutputs, Node, NodeConfig,
    NodeId, ProjectColumnGraph, RelationIndex, SourceNode, TraceTermination,
};

fn nid(s: &str) -> NodeId {
    NodeId::new(s)
}

fn model(id: &str, relation_name: &str, compiled: &str, producers: &[&str]) -> Node {
    Node::new(
        nid(id),
        "model",
        Checksum::new("sha256", "ck"),
        Some(compiled.to_owned()),
        None,
        DependsOn::new(Vec::new(), producers.iter().map(|p| nid(p)).collect()),
        None,
        NodeConfig::default(),
        Some(relation_name.to_owned()),
        BTreeMap::new(),
    )
}

fn source(id: &str, schema: &str, database: &str, relation_name: &str) -> SourceNode {
    SourceNode::new(
        nid(id),
        "raw",
        id.rsplit('.').next().unwrap_or(id),
        None,
        schema,
        Some(database.to_owned()),
        Some(relation_name.to_owned()),
    )
}

fn manifest_of(nodes: Vec<Node>, sources: Vec<SourceNode>) -> Manifest {
    let mut node_map: HashMap<NodeId, Node> = HashMap::new();
    for n in nodes {
        node_map.insert(n.id().clone(), n);
    }
    let mut source_map: HashMap<NodeId, SourceNode> = HashMap::new();
    for s in sources {
        source_map.insert(s.id().clone(), s);
    }
    Manifest::new(
        ManifestMetadata::new("v12"),
        node_map,
        HashMap::new(),
        HashMap::new(),
    )
    .with_sources(source_map)
}

/// A real source → staging → mart chain, each layer `select *`-ing the
/// previous over its FULL quoted relation (the dbt-compiled shape).
fn chain_manifest() -> Manifest {
    let stg = "\
with source as (
    select * from \"db\".\"raw\".\"raw_orders\"
)
select
      order_id
    , customer_id
    , amount as order_amount
from source";
    let dim = "\
with renamed as (
    select * from \"db\".\"staging\".\"stg_orders\"
)
select * from renamed";
    manifest_of(
        vec![
            model(
                "model.p.stg_orders",
                "\"db\".\"staging\".\"stg_orders\"",
                stg,
                &["source.p.raw.raw_orders"],
            ),
            model(
                "model.p.dim_orders",
                "\"db\".\"marts\".\"dim_orders\"",
                dim,
                &["model.p.stg_orders"],
            ),
        ],
        vec![source(
            "source.p.raw.raw_orders",
            "raw",
            "db",
            "\"db\".\"raw\".\"raw_orders\"",
        )],
    )
}

/// Re-derive the domain ProjectColumnGraph through the SAME public parse path
/// the explorer uses (the integration oracle for the trace/blast affordances,
/// which the serialized payload flattens).
fn project_graph(manifest: &Manifest) -> ProjectColumnGraph {
    let index = RelationIndex::from_manifest(manifest);
    let lineage = ModelLineage::from_manifest(manifest);
    let mut outputs: BTreeMap<NodeId, ModelOutputs> = BTreeMap::new();
    for (id, node) in manifest.nodes() {
        if node.resource_type() != "model" {
            continue;
        }
        let graph = parse_cte_graph(node.compiled_code().expect("compiled")).expect("parse");
        outputs.insert(id.clone(), graph.model_outputs(TERMINAL_NODE_NAME));
    }
    ProjectColumnGraph::build(manifest, &lineage, &index, &outputs)
}

// ---- normalized join key + cross-model stitch (TDD 1, 2) ----------------

#[test]
fn explorer_builds_cross_model_edges_over_the_real_parse_path() {
    let manifest = chain_manifest();
    let payload =
        build_cross_model_columns(&manifest).expect("the chain produces cross-model edges");
    // dim_orders `select *`s stg_orders → stg's three output columns flow in.
    let dim_edges: Vec<_> = payload
        .edges
        .iter()
        .filter(|e| e.downstream == "model.p.dim_orders")
        .collect();
    assert!(
        !dim_edges.is_empty(),
        "dim_orders stitches to stg_orders across the ref() boundary"
    );
    assert!(
        dim_edges.iter().all(|e| e.upstream == "model.p.stg_orders"),
        "every dim edge is attributed to the REAL producer, never a wrong upstream"
    );
    // The stg output columns (order_id, customer_id, order_amount) are the
    // ones flowing in (the project-wide output map = the catalog-equivalent).
    let flowed: std::collections::BTreeSet<&str> = dim_edges
        .iter()
        .map(|e| e.downstream_column.as_str())
        .collect();
    assert!(
        flowed.contains("order_amount"),
        "the renamed column flows through"
    );
    assert!(flowed.contains("order_id"));
}

#[test]
fn normalized_join_never_mis_attributes_to_a_non_producer() {
    // Two relations share the bare leaf "orders" in different schemas; the
    // downstream's producer set is the authority, so the stitch resolves ONLY
    // to the real producer — never the same-leaf decoy.
    let decoy = "select 1 as x from \"db\".\"other\".\"orders\"";
    let stg = "select * from \"db\".\"raw\".\"orders\"";
    let dim = "select * from \"db\".\"staging\".\"stg\"";
    let manifest = manifest_of(
        vec![
            model("model.p.decoy", "\"db\".\"other\".\"orders\"", decoy, &[]),
            model(
                "model.p.stg",
                "\"db\".\"staging\".\"stg\"",
                stg,
                &["source.p.raw.orders"],
            ),
            model(
                "model.p.dim",
                "\"db\".\"marts\".\"dim\"",
                dim,
                &["model.p.stg"],
            ),
        ],
        vec![source(
            "source.p.raw.orders",
            "raw",
            "db",
            "\"db\".\"raw\".\"orders\"",
        )],
    );
    let graph = project_graph(&manifest);
    // No edge in the whole graph ever attributes to model.p.decoy as an
    // upstream — it is nobody's declared producer.
    assert!(
        graph
            .edges()
            .iter()
            .all(|e| e.upstream != nid("model.p.decoy")),
        "a same-leaf non-producer is NEVER a stitch target"
    );
}

// ---- C: trace-to-source (TDD 4) -----------------------------------------

#[test]
fn trace_to_source_walks_to_the_source_leaf() {
    let manifest = chain_manifest();
    let graph = project_graph(&manifest);
    // dim_orders.order_id traces dim → stg → the raw source leaf.
    let trace = graph.trace_to_source(&manifest, &nid("model.p.dim_orders"), "order_id");
    assert_eq!(
        trace.termination,
        TraceTermination::Source,
        "the trace terminates at the source() leaf — the founder headline"
    );
    let nodes: Vec<&str> = trace.hops.iter().map(|h| h.node.as_str()).collect();
    assert_eq!(
        nodes,
        vec![
            "model.p.dim_orders",
            "model.p.stg_orders",
            "source.p.raw.raw_orders"
        ],
        "trace-to-source crosses both ref() boundaries to the source field"
    );
}

#[test]
fn renamed_column_trace_follows_the_rename() {
    let manifest = chain_manifest();
    let graph = project_graph(&manifest);
    // order_amount is `amount as order_amount` in stg — downstream it is
    // order_amount; the cross-model edge carries the stg-side name through.
    let trace = graph.trace_to_source(&manifest, &nid("model.p.dim_orders"), "order_amount");
    let nodes: Vec<&str> = trace.hops.iter().map(|h| h.node.as_str()).collect();
    assert!(
        nodes.contains(&"model.p.stg_orders"),
        "the renamed column traces back through staging"
    );
}

// ---- never-a-false-claim: a COMPUTED column is NOT a source field --------

#[test]
fn computed_column_is_never_attributed_to_a_source() {
    // A staging model `select *`-ing a source AND injecting a computed column
    // (`current_timestamp as _loaded_at`, a surrogate `row_number()` key) —
    // the computed columns must NOT be attributed to the source (they do not
    // ORIGINATE there; claiming so is a fabricated lineage). Only the
    // pass-through columns trace to the source.
    let stg = "\
with source as (
    select * from \"db\".\"raw\".\"raw_orders\"
),
renamed as (
    select id as order_id, amount as order_amount from source
),
final as (
    select
        row_number() over (order by order_id) as order_key
        , order_id
        , order_amount
        , current_timestamp as _loaded_at
    from renamed
)
select * from final";
    let manifest = manifest_of(
        vec![model(
            "model.p.stg_orders",
            "\"db\".\"staging\".\"stg_orders\"",
            stg,
            &["source.p.raw.raw_orders"],
        )],
        vec![source(
            "source.p.raw.raw_orders",
            "raw",
            "db",
            "\"db\".\"raw\".\"raw_orders\"",
        )],
    );
    let graph = project_graph(&manifest);
    let to_source: std::collections::BTreeSet<&str> = graph
        .edges()
        .iter()
        .filter(|e| e.upstream == nid("source.p.raw.raw_orders"))
        .map(|e| e.downstream_column.as_str())
        .collect();
    // The pass-through columns reach the source.
    assert!(
        to_source.contains("order_id"),
        "a pass-through column traces to the source"
    );
    assert!(to_source.contains("order_amount"));
    // The COMPUTED columns must NOT — never a fabricated source field.
    assert!(
        !to_source.contains("_loaded_at"),
        "current_timestamp as _loaded_at is computed in-model — NOT a source field"
    );
    assert!(
        !to_source.contains("order_key"),
        "a surrogate row_number() key is computed in-model — NOT a source field"
    );
    // And the computed column's trace terminates honestly (NOT Source).
    let trace = graph.trace_to_source(&manifest, &nid("model.p.stg_orders"), "_loaded_at");
    assert_ne!(
        trace.termination,
        TraceTermination::Source,
        "a computed column never claims a source origin"
    );
}

// ---- B: blast-radius (TDD 3) --------------------------------------------

#[test]
fn blast_radius_reaches_downstream_columns() {
    let manifest = chain_manifest();
    let graph = project_graph(&manifest);
    // stg_orders.order_id flows downstream into dim_orders.
    let reached = graph.blast_radius(&nid("model.p.stg_orders"), "order_id");
    let nodes: std::collections::BTreeSet<&str> = reached.iter().map(|h| h.node.as_str()).collect();
    assert!(
        nodes.contains("model.p.dim_orders"),
        "downstream impact reaches the mart that consumes the staging column"
    );
}

// ---- star discipline (TDD 5) --------------------------------------------

#[test]
fn star_over_known_modeled_upstream_resolves() {
    // dim does `select * from renamed` where renamed = `select * from stg`
    // (a KNOWN modeled upstream) → the columns resolve through, not Opaque.
    let manifest = chain_manifest();
    let payload = build_cross_model_columns(&manifest).expect("edges");
    let dim_edges: Vec<_> = payload
        .edges
        .iter()
        .filter(|e| e.downstream == "model.p.dim_orders")
        .collect();
    assert!(
        dim_edges.iter().all(|e| e.via_star),
        "the star over the known modeled upstream EXPANDS to resolved edges"
    );
}

#[test]
fn star_over_unknown_external_stays_opaque_no_fabrication() {
    // A model `select *`-ing an UNKNOWN external relation (no producer, not in
    // the manifest) fabricates NO output columns → it never appears as an
    // enumerable upstream, and its own trace thins Opaque.
    let lonely = "select * from \"db\".\"external\".\"vendor_feed\"";
    let manifest = manifest_of(
        vec![model(
            "model.p.lonely",
            "\"db\".\"staging\".\"lonely\"",
            lonely,
            &[],
        )],
        vec![],
    );
    let graph = project_graph(&manifest);
    assert!(
        graph.edges().is_empty(),
        "a star over an unknown external relation fabricates no edges"
    );
    let trace = graph.trace_to_source(&manifest, &nid("model.p.lonely"), "anything");
    assert_eq!(
        trace.termination,
        TraceTermination::Opaque,
        "a `*` over an unknown external is non-enumerable — the trace degrades to \
         Opaque, never claiming a fabricated Root origin"
    );
    // And the explorer emits no carrier at all for a flow-free project.
    assert!(
        build_cross_model_columns(&manifest).is_none(),
        "no cross-model flow ⇒ no carrier ⇒ golden byte-stable"
    );
}

#[test]
fn star_chain_compounds_opaque_across_models() {
    // raw-feed model (unknown external, non-enumerable) → mid (`* over feed`)
    // → top (`* over mid`): the Opaque compounds; nothing downstream
    // fabricates a column list.
    let feed = "select * from \"db\".\"external\".\"vendor_feed\"";
    let mid = "select * from \"db\".\"staging\".\"feed_model\"";
    let top = "select * from \"db\".\"marts\".\"mid_model\"";
    let manifest = manifest_of(
        vec![
            model(
                "model.p.feed_model",
                "\"db\".\"staging\".\"feed_model\"",
                feed,
                &[],
            ),
            model(
                "model.p.mid_model",
                "\"db\".\"marts\".\"mid_model\"",
                mid,
                &["model.p.feed_model"],
            ),
            model(
                "model.p.top_model",
                "\"db\".\"marts\".\"top_model\"",
                top,
                &["model.p.mid_model"],
            ),
        ],
        vec![],
    );
    let graph = project_graph(&manifest);
    assert!(
        graph.edges().is_empty(),
        "a chain of stars over a non-enumerable feed fabricates nothing downstream"
    );
}

// ---- scope-as-parameter (TDD 6) -----------------------------------------

#[test]
fn report_path_does_not_build_the_project_graph() {
    // The report's per-model payload (built by adapters::render::build_payload)
    // carries NO project-wide cross-model field — the project graph is the
    // explorer's envelope alone. We assert the boundary structurally: the
    // ReportPayload type has no cross-model column carrier; only the explorer's
    // LineagePayload does, and it is populated only by render_explore /
    // build_cross_model_columns. This test documents the invariant by
    // confirming the explorer builder is the SOLE construction site (a grep
    // tripwire in CI would catch a report-path call; here we assert the
    // explorer arm DOES build it while the report payload remains scope-local).
    let manifest = chain_manifest();
    // The explorer arm builds it.
    assert!(
        build_cross_model_columns(&manifest).is_some(),
        "the explorer arm builds the project graph"
    );
    // The report payload is intra-model only: each model's CteGraph carries
    // ONLY Intra-scoped column edges (no Cross arm) — the cross-model stitch
    // never enters the per-model report fact.
    for node in manifest.nodes().values() {
        if node.resource_type() != "model" {
            continue;
        }
        let graph = parse_cte_graph(node.compiled_code().expect("compiled")).expect("parse");
        for edge in graph.column_edges() {
            let from_intra = matches!(
                edge.from_col.scope,
                cute_dbt::domain::ColumnScope::Intra { .. }
            );
            let to_intra = matches!(
                edge.to_col.scope,
                cute_dbt::domain::ColumnScope::Intra { .. }
            );
            assert!(
                from_intra && to_intra,
                "per-model report facts stay intra-model — no Cross arm leaks into the report path"
            );
        }
    }
}

// ---- DagFacts.lineage authority (TDD 7) ---------------------------------

#[test]
fn stitch_uses_dagfacts_lineage_not_a_fresh_inversion() {
    // The producer set comes from ModelLineage.backward (DagFacts.lineage, S0).
    // A model that reads a leaf but declares NO depends_on producer gets NO
    // cross-model edge — proving the stitch is gated on the lineage fact, not
    // on a name match.
    let dim = "select * from \"db\".\"staging\".\"stg_orders\"";
    let stg = "select order_id from \"db\".\"raw\".\"raw_orders\"";
    let manifest = manifest_of(
        vec![
            model(
                "model.p.stg_orders",
                "\"db\".\"staging\".\"stg_orders\"",
                stg,
                &[],
            ),
            // dim reads stg_orders by name but declares NO producer.
            model("model.p.dim_orders", "\"db\".\"marts\".\"dim\"", dim, &[]),
        ],
        vec![],
    );
    let graph = project_graph(&manifest);
    assert!(
        graph.edges().is_empty(),
        "no depends_on edge ⇒ no cross-model attribution (lineage is the authority)"
    );
}
