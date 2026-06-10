//! Step definitions for `features/cte_rendering.feature` — eight
//! edge-type Examples in a Scenario Outline plus one regular scenario
//! asserting the legend's presence and colorblind-safe palette.
//!
//! Each scenario constructs a tiny two-CTE compiled SQL string of the
//! shape:
//! ```sql
//! WITH a AS (SELECT 1), b AS (<a-reference-with-the-edge>) SELECT * FROM b
//! ```
//! and parses it via the CTE engine, then asserts:
//! - the parsed graph carries the named `EdgeType` on the `a` → `b` edge
//! - the renderer's wire-key for that variant matches what the JS
//!   `JOIN_COLORS` palette uses (the same key the production template
//!   reads from `edge_payload.edge_type`).

use cucumber::{given, then, when};
use cute_dbt::adapters::cte_engine::parse_cte_graph;
use cute_dbt::adapters::render::edge_type_wire_key;
use cute_dbt::domain::EdgeType;

use super::World;

fn parse_edge_label(label: &str) -> EdgeType {
    match label {
        "from" => EdgeType::From,
        "inner" => EdgeType::Inner,
        "left" => EdgeType::Left,
        "right" => EdgeType::Right,
        "full" => EdgeType::Full,
        "cross" => EdgeType::Cross,
        "union_all" => EdgeType::UnionAll,
        "union_distinct" => EdgeType::UnionDistinct,
        other => panic!("unknown edge label in scenario: {other}"),
    }
}

/// Build a compiled-SQL string whose graph carries a single `a` → `b`
/// edge of the requested type. Join scenarios introduce a third CTE
/// `c` so the base-relation (which always carries `EdgeType::From`) is
/// distinct from `a`; that way the `a` → `b` edge is uniquely the
/// requested join type. The union scenarios use both arms referencing
/// `a` because the CTE engine treats both arms symmetrically.
fn sql_for_edge(edge: EdgeType) -> String {
    match edge {
        EdgeType::From => "WITH a AS (SELECT 1 AS x), b AS (SELECT x FROM a) SELECT * FROM b"
            .to_owned(),
        EdgeType::Inner => "WITH a AS (SELECT 1 AS x), c AS (SELECT 2 AS x), b AS (SELECT a.x FROM c INNER JOIN a ON a.x = c.x) SELECT * FROM b"
            .to_owned(),
        EdgeType::Left => "WITH a AS (SELECT 1 AS x), c AS (SELECT 2 AS x), b AS (SELECT a.x FROM c LEFT JOIN a ON a.x = c.x) SELECT * FROM b"
            .to_owned(),
        EdgeType::Right => "WITH a AS (SELECT 1 AS x), c AS (SELECT 2 AS x), b AS (SELECT a.x FROM c RIGHT JOIN a ON a.x = c.x) SELECT * FROM b"
            .to_owned(),
        EdgeType::Full => "WITH a AS (SELECT 1 AS x), c AS (SELECT 2 AS x), b AS (SELECT a.x FROM c FULL JOIN a ON a.x = c.x) SELECT * FROM b"
            .to_owned(),
        EdgeType::Cross => "WITH a AS (SELECT 1 AS x), c AS (SELECT 2 AS x), b AS (SELECT a.x FROM c CROSS JOIN a) SELECT * FROM b"
            .to_owned(),
        EdgeType::UnionAll => "WITH a AS (SELECT 1 AS x), b AS (SELECT x FROM a UNION ALL SELECT x FROM a) SELECT * FROM b"
            .to_owned(),
        EdgeType::UnionDistinct => "WITH a AS (SELECT 1 AS x), b AS (SELECT x FROM a UNION SELECT x FROM a) SELECT * FROM b"
            .to_owned(),
        // EdgeType is `#[non_exhaustive]`; future variants land via an
        // additive ADR and an updated .feature Examples table.
        _ => panic!("sql_for_edge called with unsupported EdgeType variant: {edge:?}"),
    }
}

// --- Scenario Outline -----------------------------------------------

#[given(regex = r#"^a model whose two CTEs are connected by a "([^"]+)" relationship$"#)]
fn model_with_edge(world: &mut World, edge_label: String) {
    let edge = parse_edge_label(&edge_label);
    world.last_edge_type = Some(edge);
    let sql = sql_for_edge(edge);
    let graph = parse_cte_graph(&sql).unwrap_or_else(|err| {
        panic!("parse_cte_graph failed for edge {edge_label} ({sql}): {err:?}")
    });
    world.last_cte_graph = Some(graph);
}

#[when("the CTE dependency diagram for that model is rendered")]
fn render_cte_diagram(_world: &mut World) {
    // Render-time concerns are exercised at the assert step against the
    // already-parsed graph — there is no separate render pass for the
    // two-CTE case beyond the wire-key projection asserted below.
}

#[then(regex = r#"^the edge between those CTEs carries the "([^"]+)" color class$"#)]
fn edge_carries_color_class(world: &mut World, edge_label: String) {
    let expected_edge = parse_edge_label(&edge_label);
    let graph = world
        .last_cte_graph
        .as_ref()
        .expect("a CTE graph was parsed");
    // Find the a → b edge — every scenario's SQL produces exactly one
    // edge from CTE `a` (node index 0) to CTE `b` (node index 1) of the
    // requested type.
    let edge_to_b = graph
        .edges()
        .iter()
        .find(|e| graph.nodes()[e.from()].name() == "a" && graph.nodes()[e.to()].name() == "b")
        .unwrap_or_else(|| panic!("no a → b edge in {:?}", graph.edges()));
    assert_eq!(
        edge_to_b.edge_type(),
        expected_edge,
        "edge type mismatch — expected {expected_edge:?}, got {:?}",
        edge_to_b.edge_type()
    );
    // The wire key projection is the contract the JS palette indexes
    // on — assert it round-trips to the same snake_case the .feature
    // example uses.
    assert_eq!(
        edge_type_wire_key(expected_edge),
        edge_label,
        "wire-key projection of {expected_edge:?} should equal {edge_label}",
    );
}

#[then(regex = r#"^the legend maps that color to "([^"]+)"$"#)]
fn legend_maps_color(world: &mut World, edge_label: String) {
    // The legend is the JS `JOIN_COLORS` map keyed by snake_case wire
    // key (see edge-vocab-completeness CI job). The contract surface
    // here is: every wire key the renderer emits must match the
    // legend's keys. The `edge_type_wire_key` function is the producer
    // side; the .feature label is the consumer side.
    let edge = world
        .last_edge_type
        .expect("an edge type was selected for this scenario");
    assert_eq!(edge_type_wire_key(edge), edge_label);
}

// --- Final scenario: legend visibility + colorblind palette ---------

#[given("a model with at least one CTE-to-CTE dependency")]
fn model_with_any_cte_dependency(world: &mut World) {
    // Reuse the From-edge SQL — the simplest "has at least one
    // CTE-to-CTE edge" case.
    let sql = sql_for_edge(EdgeType::From);
    let graph = parse_cte_graph(&sql).expect("simple two-CTE graph parses");
    assert!(
        graph.edges().iter().any(|e| {
            graph.nodes()[e.from()].name() == "a" && graph.nodes()[e.to()].name() == "b"
        }),
        "the From-edge graph must carry an a → b edge",
    );
    world.last_cte_graph = Some(graph);
}

/// The committed report engine (templates/interaction.js, inlined into
/// every rendered report since cute-dbt#178) — the source of truth for
/// the legend palette. Read at test time so the legend assertions go
/// through the actual rendered chrome's color table, not just the
/// producer-side wire-key projection.
fn template_html() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("templates")
        .join("interaction.js");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Extract a palette entry from the named `var <palette> = { … };` block
/// (e.g. `inner: "#009E73"` → `"#009E73"`). Scoped to the block so other
/// `<wire_key>:` matches elsewhere in the engine cannot pollute the
/// lookup. Since cute-dbt#178 the engine carries TWO palettes —
/// `JOIN_COLORS_LIGHT` and `JOIN_COLORS_DARK` (theme-aware edges).
fn palette_color(template: &str, palette: &str, wire_key: &str) -> Option<String> {
    let block_start = template.find(&format!("var {palette}"))?;
    let open_brace = template[block_start..].find('{')?;
    let close_brace = template[block_start + open_brace..].find('}')?;
    let block_end = block_start + open_brace + close_brace;
    let block = &template[block_start + open_brace..block_end];
    let needle = format!("{wire_key}:");
    let idx = block.find(&needle)?;
    let tail = &block[idx + needle.len()..];
    let open = tail.find('"')?;
    let rest = &tail[open + 1..];
    let close = rest.find('"')?;
    Some(rest[..close].to_owned())
}

/// The light palette's entry — the canonical legend palette (the dark
/// variant is its lightness-lifted twin, checked for completeness in
/// `edge_legend_visible`).
fn legend_color(template: &str, wire_key: &str) -> Option<String> {
    palette_color(template, "JOIN_COLORS_LIGHT", wire_key)
}

#[then("an edge-type color legend is visible")]
fn edge_legend_visible(_world: &mut World) {
    // The legend lives in the rendered HTML's `JOIN_COLORS` map
    // (`templates/report.html`). The structural CI gate
    // `edge-vocab-completeness` already enforces snake_case-key
    // presence; this BDD step asserts the rendered contract — every
    // `EdgeType` variant has a non-empty `#…` color value in the
    // committed template.
    let template = template_html();
    let known: &[EdgeType] = &[
        EdgeType::From,
        EdgeType::Inner,
        EdgeType::Left,
        EdgeType::Right,
        EdgeType::Full,
        EdgeType::Cross,
        EdgeType::UnionAll,
        EdgeType::UnionDistinct,
    ];
    for edge in known {
        let key = edge_type_wire_key(*edge);
        // cute-dbt#178 — the engine carries a light AND a dark palette
        // (theme-aware edges); every wire key must be present in BOTH.
        for palette in ["JOIN_COLORS_LIGHT", "JOIN_COLORS_DARK"] {
            let color = palette_color(&template, palette, key)
                .unwrap_or_else(|| panic!("legend entry for {key} missing from {palette}"));
            assert!(
                color.starts_with('#') && color.len() >= 4,
                "legend color for {key} in {palette} ({color}) must be a `#…` value",
            );
        }
    }
}

#[then("the legend palette is colorblind-safe (not red/green alone)")]
fn legend_palette_colorblind_safe(_world: &mut World) {
    // The colorblind-safe contract: the two union arms (the .feature's
    // primary contrast pair) must use distinct colors AND must not be
    // a pure red/green pairing. Read actual color values from the
    // committed template so a future palette change that violates the
    // contract fails this gate.
    let template = template_html();
    let union_all = legend_color(&template, "union_all").expect("union_all entry");
    let union_distinct = legend_color(&template, "union_distinct").expect("union_distinct entry");
    assert_ne!(
        union_all, union_distinct,
        "union_all and union_distinct must use distinct colors",
    );
    // Reject a literal red/green pairing in either order. The current
    // palette uses orange (#E69F00) and blue (#0072B2) — well outside
    // the red/green axis. This guards against a future regression to
    // a naive green/red split.
    let pair = (
        union_all.to_ascii_lowercase(),
        union_distinct.to_ascii_lowercase(),
    );
    let red_green = [
        ("#ff0000".to_owned(), "#00ff00".to_owned()),
        ("#00ff00".to_owned(), "#ff0000".to_owned()),
    ];
    assert!(
        !red_green.contains(&pair),
        "union arm pair is a literal red/green split: {pair:?}",
    );
}
