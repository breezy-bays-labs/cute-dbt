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
fn computed_and_renamed_columns_are_never_attributed_to_a_source() {
    // A staging model `select *`-ing a source, carrying TWO same-name
    // pass-through columns (`id`, `amount`), RENAMING one (`legacy_qty AS qty`),
    // and injecting computed columns (`current_timestamp as _loaded_at`, a
    // surrogate `row_number()` key). Only the SAME-NAME pass-throughs may trace
    // to the source. The computed columns and the RENAMED column must NOT —
    // attributing them is a fabricated source field (#450 never-a-false-claim;
    // a rename means the source field name differs from the downstream name, so
    // `source.qty` does not exist — the source field is `legacy_qty`).
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
    let to_source: std::collections::BTreeSet<&str> = graph
        .edges()
        .iter()
        .filter(|e| e.upstream == nid("source.p.raw.raw_orders"))
        .map(|e| e.downstream_column.as_str())
        .collect();
    // The SAME-NAME pass-through columns reach the source.
    assert!(
        to_source.contains("id"),
        "a same-name pass-through column traces to the source"
    );
    assert!(to_source.contains("amount"));
    // The RENAMED column must NOT — `source.qty` does not exist (the source
    // field is `legacy_qty`); name-carrying `qty` would fabricate it (#450).
    assert!(
        !to_source.contains("qty"),
        "legacy_qty AS qty is a RENAME — the source field is legacy_qty, NOT qty; \
         name-carrying qty would fabricate a source.qty"
    );
    // The COMPUTED columns must NOT — never a fabricated source field.
    assert!(
        !to_source.contains("_loaded_at"),
        "current_timestamp as _loaded_at is computed in-model — NOT a source field"
    );
    assert!(
        !to_source.contains("order_key"),
        "a surrogate row_number() key is computed in-model — NOT a source field"
    );
    // And the computed + renamed columns' traces terminate honestly (NOT Source).
    for col in ["_loaded_at", "qty"] {
        let trace = graph.trace_to_source(&manifest, &nid("model.p.stg_orders"), col);
        assert_ne!(
            trace.termination,
            TraceTermination::Source,
            "a computed/renamed column ({col}) never claims a source origin"
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

// ---- never-a-false-claim: a RENAMED column is NOT a same-name source field
// (cute-dbt#450 round-3, the open-fabrication fix). `amount AS order_amount`
// over a source: the source field is `amount`, the downstream output is
// `order_amount`. The source name-carry must NOT fabricate a
// `source.order_amount` (a Resolved trace to a source column that does not
// exist). A renamed column degrades (no source edge) until a terminal→leaf
// original-column mapping exists. A pure pass-through (same name both sides)
// STILL traces correctly.

/// stg `select id as order_id, amount as order_amount from <source>`: BOTH
/// outputs are RENAMES of the source fields (id→order_id, amount→order_amount).
/// The source is referenced directly (not a `select *`), so there is no clean
/// pass-through column — neither output may name-carry to the source.
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
fn renamed_column_does_not_fabricate_a_same_name_source_field() {
    let manifest = rename_over_source_manifest();
    let graph = project_graph(&manifest);

    // NO cross-model edge attributes a renamed output column to the source
    // under that downstream name — `source.order_amount` does not exist.
    let fabricated = graph.edges().iter().any(|e| {
        e.upstream == nid("source.p.raw.raw_orders")
            && (e.downstream_column == "order_amount" || e.upstream_column == "order_amount")
    });
    assert!(
        !fabricated,
        "a renamed column (amount AS order_amount) must NOT fabricate a \
         source.order_amount edge — the source field is `amount`, not `order_amount`"
    );

    // And trace_to_source for the renamed column does NOT claim a source origin
    // named order_amount: it degrades (Root/Opaque), never a Resolved
    // fabrication.
    let trace = graph.trace_to_source(&manifest, &nid("model.p.stg_orders"), "order_amount");
    assert_ne!(
        trace.termination,
        TraceTermination::Source,
        "a renamed column never claims a (fabricated) same-name source origin"
    );
    assert!(
        trace
            .hops
            .iter()
            .all(|h| !(h.node == nid("source.p.raw.raw_orders") && h.column == "order_amount")),
        "the trace fabricates no source.order_amount hop for a renamed column"
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
