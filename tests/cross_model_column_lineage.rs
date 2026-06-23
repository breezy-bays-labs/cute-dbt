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
fn computed_columns_are_never_attributed_to_a_source_renames_trace_to_real_field() {
    // A staging model `select *`-ing a source, carrying TWO same-name
    // pass-through columns (`id`, `amount`), RENAMING one deep in the chain
    // (`legacy_qty AS qty`), and injecting computed columns
    // (`current_timestamp as _loaded_at`, a surrogate `row_number()` key).
    //
    // ROBUST contract (cute-dbt#450 round-4): the same-name pass-throughs trace
    // to the source under their own name; the RENAMED column traces to the
    // source under its REAL ORIGINAL field name `legacy_qty` (NEVER the
    // fabricated downstream name `qty`); the COMPUTED columns NEVER trace to a
    // source. The floor — no source field that does not exist is ever named —
    // holds in every case: `source.qty` is NEVER emitted.
    let stg = "\
with source as (
    select * from \"db\".\"raw\".\"raw_orders\"
),
renamed as (
    select id, amount, legacy_qty as qty from source
),
final as (
    select
        row_number() over (order by id) as order_key
        , id
        , amount
        , qty
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
    let down_to_source: std::collections::BTreeSet<&str> = graph
        .edges()
        .iter()
        .filter(|e| e.upstream == nid("source.p.raw.raw_orders"))
        .map(|e| e.downstream_column.as_str())
        .collect();
    let source_fields_named: std::collections::BTreeSet<&str> = graph
        .edges()
        .iter()
        .filter(|e| e.upstream == nid("source.p.raw.raw_orders"))
        .map(|e| e.upstream_column.as_str())
        .collect();
    // The SAME-NAME pass-through columns reach the source.
    assert!(
        down_to_source.contains("id"),
        "a same-name pass-through column traces to the source"
    );
    assert!(down_to_source.contains("amount"));
    // The RENAMED column NOW traces to the source under its REAL field name.
    assert!(
        down_to_source.contains("qty"),
        "the renamed column flows to the source (robust name-tracking)"
    );
    assert!(
        source_fields_named.contains("legacy_qty"),
        "the rename names the REAL source field legacy_qty"
    );
    // THE FLOOR: the fabricated `source.qty` field is NEVER named.
    assert!(
        !source_fields_named.contains("qty"),
        "the source field `qty` does NOT exist (real field is legacy_qty); \
         it must NEVER be named as a source field"
    );
    // The COMPUTED columns must NOT reach a source under any name.
    assert!(
        !down_to_source.contains("_loaded_at"),
        "current_timestamp as _loaded_at is computed in-model — NOT a source field"
    );
    assert!(
        !down_to_source.contains("order_key"),
        "a surrogate row_number() key is computed in-model — NOT a source field"
    );
    // The renamed column traces to source under legacy_qty; the computed
    // columns never claim a source origin.
    let qty_trace = graph.trace_to_source(&manifest, &nid("model.p.stg_orders"), "qty");
    assert_eq!(
        qty_trace.termination,
        TraceTermination::Source,
        "the renamed column traces all the way to its REAL source field"
    );
    let qty_last = qty_trace.hops.last().expect("non-empty trace");
    assert_eq!(qty_last.node, nid("source.p.raw.raw_orders"));
    assert_eq!(
        qty_last.column, "legacy_qty",
        "the source hop names the REAL field legacy_qty, never qty"
    );
    for col in ["_loaded_at", "order_key"] {
        let trace = graph.trace_to_source(&manifest, &nid("model.p.stg_orders"), col);
        assert_ne!(
            trace.termination,
            TraceTermination::Source,
            "a computed column ({col}) never claims a source origin"
        );
    }
}

// ---- never-a-false-claim: a LITERAL / COMPUTED alias is NOT a source -----
// The round-5 adversarial finding (#450): a literal/constant projection with
// an explicit alias (`42 AS magic`, `1 AS x`, `current_timestamp AS t`) reads
// the external leaf but emits NO inbound column edge. The pre-fix star-
// passthrough fallback fired on "leaf-reading node with NO inbound edge" and
// fabricated `source.<leaf>.<alias>` for a column the source does not have.
// A source edge must be emitted ONLY when a PROVABLE pass-through/rename chain
// (or a genuine star over the leaf) terminates in a REAL source field. Every
// literal/computed alias degrades to None — no fabricated source field.

#[test]
fn literal_alias_in_a_leaf_cte_never_fabricates_a_source_field() {
    // `42 AS magic` in the leaf-reading CTE `a` (which `select *`s nothing —
    // it names two explicit projections, one a real pass-through `order_id`
    // and one a LITERAL `42 AS magic`). `magic` has NO inbound edge: it is the
    // constant 42, not a column the source emits.
    let m = "\
with a as (
    select order_id, 42 as magic from \"db\".\"raw\".\"orders\"
)
select order_id, magic from a";
    let manifest = manifest_of(
        vec![model(
            "model.p.stg",
            "\"db\".\"staging\".\"stg\"",
            m,
            &["source.p.raw.orders"],
        )],
        vec![source(
            "source.p.raw.orders",
            "raw",
            "db",
            "\"db\".\"raw\".\"orders\"",
        )],
    );
    let graph = project_graph(&manifest);
    let source_fields_named: std::collections::BTreeSet<&str> = graph
        .edges()
        .iter()
        .filter(|e| e.upstream == nid("source.p.raw.orders"))
        .map(|e| e.upstream_column.as_str())
        .collect();
    // THE FLOOR: the literal alias `magic` is NEVER named as a source field.
    assert!(
        !source_fields_named.contains("magic"),
        "`42 as magic` is a literal — the source field `magic` does NOT exist \
         and must NEVER be fabricated as a source attribution"
    );
    // The real pass-through `order_id` still reaches the source.
    assert!(
        source_fields_named.contains("order_id"),
        "the real pass-through `order_id` still traces to the source"
    );
    // And `magic`'s trace degrades — it never claims a source origin.
    let magic_trace = graph.trace_to_source(&manifest, &nid("model.p.stg"), "magic");
    assert_ne!(
        magic_trace.termination,
        TraceTermination::Source,
        "a literal alias never claims a source origin"
    );
}

#[test]
fn computed_and_constant_aliases_in_a_single_model_never_fabricate_a_source() {
    // A single leaf-reading model projecting a real pass-through alongside
    // assorted constant/computed aliases: `1 as x`, `a.id + 1 as y`,
    // `current_timestamp as t`, `'US' as country`. NONE of these is a source
    // field; only the real pass-through `order_id` reaches the source.
    let m = "\
select
      order_id
    , 1 as x
    , order_id + 1 as y
    , current_timestamp as t
    , 'US' as country
from \"db\".\"raw\".\"orders\"";
    let manifest = manifest_of(
        vec![model(
            "model.p.stg",
            "\"db\".\"staging\".\"stg\"",
            m,
            &["source.p.raw.orders"],
        )],
        vec![source(
            "source.p.raw.orders",
            "raw",
            "db",
            "\"db\".\"raw\".\"orders\"",
        )],
    );
    let graph = project_graph(&manifest);
    let source_fields_named: std::collections::BTreeSet<&str> = graph
        .edges()
        .iter()
        .filter(|e| e.upstream == nid("source.p.raw.orders"))
        .map(|e| e.upstream_column.as_str())
        .collect();
    for fabricated in ["x", "y", "t", "country"] {
        assert!(
            !source_fields_named.contains(fabricated),
            "`{fabricated}` is a literal/computed alias — never a fabricated source field"
        );
    }
    assert!(
        source_fields_named.contains("order_id"),
        "the real pass-through `order_id` still traces to the source"
    );
    for col in ["x", "y", "t", "country"] {
        let trace = graph.trace_to_source(&manifest, &nid("model.p.stg"), col);
        assert_ne!(
            trace.termination,
            TraceTermination::Source,
            "a literal/computed alias ({col}) never claims a source origin"
        );
    }
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

// ---- never-a-false-claim: downstream NARROWING (no phantom edge) ---------
// The decisive CLL-4 adversarial finding (#450): when a downstream NARROWS
// the upstream's projection (drops a column), the dropped column must NOT
// become a phantom cross-model edge. The flowed upstream columns are
// intersected against the downstream model's OWN terminal output_columns.

/// stg exposes (order_id, amount); dim NARROWS to `select order_id from stg`.
/// The graph's outputs map correctly says dim outputs = {order_id}, so the
/// edge set must NOT claim a `stg.amount → dim.amount` flow.
fn narrowing_manifest() -> Manifest {
    let stg = "select order_id, amount from \"db\".\"raw\".\"raw_o\"";
    let dim = "select order_id from \"db\".\"staging\".\"stg\"";
    manifest_of(
        vec![
            model(
                "model.p.stg",
                "\"db\".\"staging\".\"stg\"",
                stg,
                &["source.p.raw.raw_o"],
            ),
            model(
                "model.p.dim",
                "\"db\".\"marts\".\"dim\"",
                dim,
                &["model.p.stg"],
            ),
        ],
        vec![source(
            "source.p.raw.raw_o",
            "raw",
            "db",
            "\"db\".\"raw\".\"raw_o\"",
        )],
    )
}

#[test]
fn downstream_narrowing_emits_no_phantom_edge_for_dropped_column() {
    let manifest = narrowing_manifest();
    let graph = project_graph(&manifest);

    // The downstream's REAL terminal outputs are {order_id} — `amount` dropped.
    assert_eq!(
        graph.outputs().get(&nid("model.p.dim")).cloned().flatten(),
        Some(vec!["order_id".to_owned()]),
        "dim narrows to a single column — the outputs map is the catalog truth"
    );

    // No fabricated `stg.amount → dim.amount` edge: dim never exposes amount.
    let phantom = graph.edges().iter().any(|e| {
        e.upstream == nid("model.p.stg")
            && e.upstream_column == "amount"
            && e.downstream == nid("model.p.dim")
    });
    assert!(
        !phantom,
        "a column the downstream NARROWS away gets NO cross-model edge \
         (never-a-false-claim): the stg.amount→dim.amount edge is phantom"
    );
    // Defensively: NO dim edge carries the `amount` downstream column at all.
    assert!(
        graph
            .edges()
            .iter()
            .all(|e| !(e.downstream == nid("model.p.dim") && e.downstream_column == "amount")),
        "dim has no inbound edge on a column it does not output"
    );
}

#[test]
fn blast_radius_excludes_a_narrowed_away_column() {
    let manifest = narrowing_manifest();
    let graph = project_graph(&manifest);
    // stg.amount is dropped by dim → blast_radius(stg, amount) must NOT list dim.
    let reached = graph.blast_radius(&nid("model.p.stg"), "amount");
    assert!(
        reached.iter().all(|h| h.node != nid("model.p.dim")),
        "blast_radius(stg, amount) does NOT falsely list dim — the column is \
         narrowed away downstream"
    );
}

#[test]
fn trace_to_source_does_not_fabricate_a_chain_for_a_narrowed_away_column() {
    let manifest = narrowing_manifest();
    let graph = project_graph(&manifest);
    // dim does NOT expose `amount`; tracing it must NOT return a confident
    // dim.amount → stg.amount Root/Source chain.
    let trace = graph.trace_to_source(&manifest, &nid("model.p.dim"), "amount");
    assert_ne!(
        trace.termination,
        TraceTermination::Source,
        "a column dim does not expose never claims a fabricated source origin"
    );
    // The trace must not contain a phantom stg.amount hop.
    assert!(
        trace
            .hops
            .iter()
            .all(|h| !(h.node == nid("model.p.stg") && h.column == "amount")),
        "the trace fabricates no stg.amount hop for a column dim never exposes"
    );
}

#[test]
fn exposed_column_still_gets_its_cross_model_edge_after_narrowing() {
    // Regression guard: the column the downstream DOES expose (order_id) keeps
    // its correct, sound cross-model edge — the fix removes ONLY the phantom.
    let manifest = narrowing_manifest();
    let graph = project_graph(&manifest);
    let kept = graph.edges().iter().any(|e| {
        e.upstream == nid("model.p.stg")
            && e.upstream_column == "order_id"
            && e.downstream == nid("model.p.dim")
            && e.downstream_column == "order_id"
    });
    assert!(
        kept,
        "the exposed column keeps its sound stg.order_id → dim.order_id edge"
    );
    // And it still traces all the way to the source.
    let trace = graph.trace_to_source(&manifest, &nid("model.p.dim"), "order_id");
    assert_eq!(
        trace.termination,
        TraceTermination::Source,
        "the exposed column still traces to its source"
    );
}

// ---- never-a-false-claim: a RENAMED column names its REAL source field
// (cute-dbt#450 round-4, ROBUST name-tracking). `amount AS order_amount` over a
// source: the source field is `amount`, the downstream output is
// `order_amount`. The robust source name-carry traces `order_amount` to the
// REAL field `amount` — NEVER fabricates a `source.order_amount` (a source
// column that does not exist). A pure pass-through (same name both sides) STILL
// traces correctly.

/// stg `select id as order_id, amount as order_amount from <source>`: BOTH
/// outputs are RENAMES of the source fields (id→order_id, amount→order_amount).
/// The source is referenced directly (not a `select *`); the robust chain
/// name-tracking carries each ORIGINAL source-field name through the rename.
fn rename_over_source_manifest() -> Manifest {
    let stg = "select id as order_id, amount as order_amount \
               from \"db\".\"raw\".\"raw_orders\"";
    manifest_of(
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
    )
}

#[test]
fn renamed_column_traces_to_its_real_source_field_never_a_fabricated_name() {
    let manifest = rename_over_source_manifest();
    let graph = project_graph(&manifest);

    // The renamed output column traces to the source under its REAL field name
    // (order_amount → amount), and NEVER under the fabricated downstream name.
    assert_rename_traces_to_real_source(
        &graph,
        &manifest,
        "model.p.stg_orders",
        "order_amount",
        "amount",
        "source.p.raw.raw_orders",
    );

    // THE FLOOR: no edge ever names the fabricated `source.order_amount` field.
    let fabricated = graph.edges().iter().any(|e| {
        e.upstream == nid("source.p.raw.raw_orders") && e.upstream_column == "order_amount"
    });
    assert!(
        !fabricated,
        "the source field `order_amount` does not exist (real field is `amount`); \
         it must NEVER be named as a source field"
    );
}

#[test]
fn pure_passthrough_over_source_still_traces_to_source() {
    // Regression guard: a SAME-NAME pass-through to the source still
    // name-carries correctly. stg `select order_id from <source>` — order_id
    // flows unchanged, so it honestly originates at the source.
    let stg = "select order_id from \"db\".\"raw\".\"raw_orders\"";
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
    let trace = graph.trace_to_source(&manifest, &nid("model.p.stg_orders"), "order_id");
    assert_eq!(
        trace.termination,
        TraceTermination::Source,
        "a same-name pass-through column still traces to its source (the fix \
         removes ONLY the renamed fabrication)"
    );
    assert!(
        graph
            .edges()
            .iter()
            .any(|e| e.upstream == nid("source.p.raw.raw_orders")
                && e.downstream_column == "order_id"),
        "the pass-through column keeps its sound source edge"
    );
}

// ---- mixed-case output column normalization (cute-dbt#450, #346 fix) ------
// `CteGraph::model_outputs` uses `with_passthrough` on the real explorer path;
// it must lowercase output_columns / leaf_refs / passthrough names exactly like
// `new()`, so a mixed-Case output column is not silently missed by the
// lowercased trace_to_source / blast_radius lookups.

#[test]
fn mixed_case_output_column_is_found_by_trace_and_blast() {
    // stg projects a MIXED-CASE output column `OrderId` (quoted to survive the
    // parser as-cased) over a source, and dim consumes it. The lowercased
    // trace/blast lookups must still find it (with_passthrough lowercases).
    let stg = "select \"OrderId\" from \"db\".\"raw\".\"raw_orders\"";
    let dim = "select * from \"db\".\"staging\".\"stg_orders\"";
    let manifest = manifest_of(
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
    );
    let graph = project_graph(&manifest);

    // The stg output column is normalized to lowercase `orderid`. trace_to_source
    // accepts either case (it lowercases the query) and finds the column.
    for query in ["OrderId", "orderid"] {
        let trace = graph.trace_to_source(&manifest, &nid("model.p.stg_orders"), query);
        assert_eq!(
            trace.termination,
            TraceTermination::Source,
            "trace_to_source({query}) finds the lowercased output column — \
             with_passthrough must normalize like new()"
        );
    }

    // blast_radius likewise finds the lowercased column flowing into dim.
    let reached = graph.blast_radius(&nid("model.p.stg_orders"), "OrderId");
    let nodes: std::collections::BTreeSet<&str> = reached.iter().map(|h| h.node.as_str()).collect();
    assert!(
        nodes.contains("model.p.dim_orders"),
        "blast_radius on a mixed-case column reaches the downstream consumer \
         (the lookup lowercases and the output is normalized)"
    );
}

#[test]
fn cte_import_join_narrowing_emits_no_phantom_for_unexposed_column() {
    // The CTE-import-join shape: dim imports stg_a (order_id, a_val) and stg_b
    // (order_id, b_val), but its terminal projection exposes only
    // {order_id, b_val}. The a_val column is narrowed away → no phantom
    // `stg_a.a_val → dim.a_val` edge.
    let stg_a = "select order_id, a_val from \"db\".\"raw\".\"raw_a\"";
    let stg_b = "select order_id, b_val from \"db\".\"raw\".\"raw_b\"";
    let dim = "\
with a as (
    select * from \"db\".\"staging\".\"stg_a\"
),
b as (
    select * from \"db\".\"staging\".\"stg_b\"
)
select a.order_id, b.b_val from a join b on a.order_id = b.order_id";
    let manifest = manifest_of(
        vec![
            model(
                "model.p.stg_a",
                "\"db\".\"staging\".\"stg_a\"",
                stg_a,
                &["source.p.raw.raw_a"],
            ),
            model(
                "model.p.stg_b",
                "\"db\".\"staging\".\"stg_b\"",
                stg_b,
                &["source.p.raw.raw_b"],
            ),
            model(
                "model.p.dim",
                "\"db\".\"marts\".\"dim\"",
                dim,
                &["model.p.stg_a", "model.p.stg_b"],
            ),
        ],
        vec![
            source(
                "source.p.raw.raw_a",
                "raw",
                "db",
                "\"db\".\"raw\".\"raw_a\"",
            ),
            source(
                "source.p.raw.raw_b",
                "raw",
                "db",
                "\"db\".\"raw\".\"raw_b\"",
            ),
        ],
    );
    let graph = project_graph(&manifest);
    let dim_outputs = graph.outputs().get(&nid("model.p.dim")).cloned().flatten();
    // Whatever the terminal projection resolves to, a_val is NOT exposed.
    if let Some(cols) = &dim_outputs {
        assert!(
            !cols.contains(&"a_val".to_owned()),
            "dim does not expose a_val (it projects order_id, b_val)"
        );
    }
    // No phantom a_val edge into dim regardless.
    assert!(
        graph
            .edges()
            .iter()
            .all(|e| !(e.downstream == nid("model.p.dim") && e.downstream_column == "a_val")),
        "the narrowed-away a_val produces no phantom cross-model edge"
    );
}

// ---- ROBUST rename name-tracking (cute-dbt#450 round-4) -------------------
// The open-fabrication class the conservative round-3 floor could not close:
// a column RENAMED *in the leaf-reading CTE itself* (the canonical dbt staging
// shape `renamed as (select legacy_qty as qty from {{source}}) select qty from
// renamed`). Round-3's `column_reaches_leaf` early-returned `true` the moment
// it reached the leaf-reading node — BEFORE inspecting the inbound edge name —
// so `qty` entered source-name-carry under its RENAMED name and emitted
// `source.raw_orders.qty` (a Resolved trace to a source field that does not
// exist; the real field is `legacy_qty`). The robust fix CARRIES the original
// source column name through the chain: the trace now terminates at the REAL
// source field, `legacy_qty`, NEVER `qty`. The floor is unchanged — no trace
// ever names a source field that does not exist.

/// Assert: the renamed downstream column traces to the source under its REAL
/// (original) source-field name `expect_source_col`, and NEVER under the
/// fabricated downstream name `forbidden`.
fn assert_rename_traces_to_real_source(
    graph: &ProjectColumnGraph,
    manifest: &Manifest,
    model_id: &str,
    down_col: &str,
    expect_source_col: &str,
    source_id: &str,
) {
    // The cross-model edge attributes the renamed downstream column to the
    // source under the ORIGINAL source-field name (upstream_column), with the
    // downstream name on the downstream side.
    let edge = graph.edges().iter().find(|e| {
        e.upstream == nid(source_id)
            && e.downstream == nid(model_id)
            && e.downstream_column == down_col
    });
    let edge = edge.unwrap_or_else(|| {
        panic!("{model_id}.{down_col} must carry a sound source edge to {source_id}")
    });
    assert_eq!(
        edge.upstream_column, expect_source_col,
        "the source edge names the REAL source field ({expect_source_col}), \
         never the renamed downstream name ({down_col})"
    );
    // NEVER a fabricated source field under the downstream (renamed) name. Only
    // meaningful when the rename actually changed the name — a genuine same-name
    // pass-through legitimately names the source field under the shared name.
    if down_col != expect_source_col {
        assert!(
            graph
                .edges()
                .iter()
                .all(|e| !(e.upstream == nid(source_id) && e.upstream_column == down_col)),
            "no source edge ever names the fabricated field {source_id}.{down_col}"
        );
    }
    // The trace terminates at the source, and its LAST hop names the REAL field.
    let trace = graph.trace_to_source(manifest, &nid(model_id), down_col);
    assert_eq!(
        trace.termination,
        TraceTermination::Source,
        "{model_id}.{down_col} traces all the way to its source field (the headline)"
    );
    let last = trace.hops.last().expect("a non-empty trace");
    assert_eq!(last.node, nid(source_id));
    assert_eq!(
        last.column, expect_source_col,
        "the source hop names the REAL field {expect_source_col}, never {down_col}"
    );
    // And NO hop anywhere fabricates the source.<down_col> field (only when the
    // rename changed the name — a same-name pass-through shares the name).
    if down_col != expect_source_col {
        assert!(
            trace
                .hops
                .iter()
                .all(|h| !(h.node == nid(source_id) && h.column == down_col)),
            "the trace never fabricates a {source_id}.{down_col} hop"
        );
    }
}

#[test]
fn rename_in_leaf_reading_cte_traces_to_the_real_source_field() {
    // THE round-4 bug shape: the rename happens INSIDE the CTE that reads the
    // source directly. `renamed` reads {{source}} (a leaf-reading boundary) and
    // renames `legacy_qty as qty`; the terminal passes `qty` through. The trace
    // must reach source.raw_orders.legacy_qty — NEVER source.raw_orders.qty.
    let stg = "\
with renamed as (
    select legacy_qty as qty from \"db\".\"raw\".\"raw_orders\"
)
select qty from renamed";
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
    assert_rename_traces_to_real_source(
        &graph,
        &manifest,
        "model.p.stg_orders",
        "qty",
        "legacy_qty",
        "source.p.raw.raw_orders",
    );
}

#[test]
fn direct_terminal_rename_traces_to_the_real_source_field() {
    // The terminal itself reads the source directly and renames: `select amount
    // as order_amount from {{source}}`. order_amount traces to source.amount.
    let manifest = rename_over_source_manifest();
    let graph = project_graph(&manifest);
    assert_rename_traces_to_real_source(
        &graph,
        &manifest,
        "model.p.stg_orders",
        "order_amount",
        "amount",
        "source.p.raw.raw_orders",
    );
    assert_rename_traces_to_real_source(
        &graph,
        &manifest,
        "model.p.stg_orders",
        "order_id",
        "id",
        "source.p.raw.raw_orders",
    );
}

#[test]
fn multi_hop_passthrough_above_a_rename_traces_to_the_real_source_field() {
    // The rename is at the leaf, and SEVERAL pure pass-through hops sit above
    // it. The original source name must survive every hop.
    let stg = "\
with renamed as (
    select legacy_qty as qty from \"db\".\"raw\".\"raw_orders\"
),
mid as (
    select qty from renamed
),
top as (
    select qty from mid
)
select qty from top";
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
    assert_rename_traces_to_real_source(
        &graph,
        &manifest,
        "model.p.stg_orders",
        "qty",
        "legacy_qty",
        "source.p.raw.raw_orders",
    );
}

#[test]
fn chained_rename_traces_to_the_first_original_source_field() {
    // a → b → c: `legacy_qty as a` at the leaf, then `a as b`, then `b as c`.
    // The downstream output is `c`; the real source field is `legacy_qty`.
    // Every link is a rename — the original name must survive the whole chain,
    // and `c`/`b`/`a` must NEVER be fabricated as source fields.
    let stg = "\
with leaf as (
    select legacy_qty as a from \"db\".\"raw\".\"raw_orders\"
),
relabel1 as (
    select a as b from leaf
),
relabel2 as (
    select b as c from relabel1
)
select c from relabel2";
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
    assert_rename_traces_to_real_source(
        &graph,
        &manifest,
        "model.p.stg_orders",
        "c",
        "legacy_qty",
        "source.p.raw.raw_orders",
    );
    // No intermediate rename name is ever fabricated as a source field.
    for fabricated in ["a", "b", "c"] {
        assert!(
            graph
                .edges()
                .iter()
                .all(|e| !(e.upstream == nid("source.p.raw.raw_orders")
                    && e.upstream_column == fabricated)),
            "no source edge ever names the intermediate rename {fabricated} as a source field"
        );
    }
}

#[test]
fn star_over_a_renaming_leaf_cte_traces_to_the_real_source_field() {
    // `select *` over a CTE that renamed at the leaf. The star carries the
    // post-rename name (`qty`) forward; the trace must still reach the REAL
    // source field `legacy_qty`, never `qty`.
    let stg = "\
with renamed as (
    select legacy_qty as qty from \"db\".\"raw\".\"raw_orders\"
)
select * from renamed";
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
    assert_rename_traces_to_real_source(
        &graph,
        &manifest,
        "model.p.stg_orders",
        "qty",
        "legacy_qty",
        "source.p.raw.raw_orders",
    );
}

#[test]
fn same_name_passthrough_over_source_still_names_the_correct_source_field() {
    // Regression: a genuine SAME-NAME pass-through still carries the correct
    // (identical) source field name — the robust name-tracking does not perturb
    // the clean case.
    let stg = "select order_id from \"db\".\"raw\".\"raw_orders\"";
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
    assert_rename_traces_to_real_source(
        &graph,
        &manifest,
        "model.p.stg_orders",
        "order_id",
        "order_id",
        "source.p.raw.raw_orders",
    );
}

#[test]
fn computed_column_on_a_leaf_reading_node_never_traces_to_source() {
    // A computed column projected DIRECTLY in the leaf-reading node
    // (`current_timestamp as _loaded_at from {{source}}`) must NOT resolve to a
    // source field — it has no source provenance at all. The robust resolution
    // must not over-claim just because the node reads a leaf.
    let stg = "select order_id, current_timestamp as _loaded_at \
               from \"db\".\"raw\".\"raw_orders\"";
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
    // order_id (a real pass-through) DOES trace to source.
    assert_rename_traces_to_real_source(
        &graph,
        &manifest,
        "model.p.stg_orders",
        "order_id",
        "order_id",
        "source.p.raw.raw_orders",
    );
    // _loaded_at NEVER does.
    assert!(
        graph
            .edges()
            .iter()
            .all(|e| !(e.upstream == nid("source.p.raw.raw_orders")
                && e.downstream_column == "_loaded_at")),
        "a computed column on a leaf-reading node fabricates no source edge"
    );
    let trace = graph.trace_to_source(&manifest, &nid("model.p.stg_orders"), "_loaded_at");
    assert_ne!(
        trace.termination,
        TraceTermination::Source,
        "current_timestamp as _loaded_at never claims a source origin"
    );
}
