//! `src/domain/source_map.rs` — the per-model source-map primitive.
//!
//! Pure POD + serde derive only (no parser, no I/O — `tests/domain_clean_arch.rs`
//! gate). The cute-dbt analogue of dbt-fusion's `MacroSpan`: per node a set of
//! [`SourceMapEntry`] correspondences, each tagged with a [`SpanRole`] and
//! carrying optional raw/compiled [`SourceSpan`]s. This is the ONE primitive the
//! source-map spine grows on — every future lineage capability (raw-zone sync,
//! column-level lineage) is an additive [`SpanRole`] variant, NOT a new payload
//! section or a new map beside `compiled_sql`/`dag`.
//!
//! The faithful full [`SourceMap::compiled`] text is the single source of truth;
//! the legacy per-CTE `compiled_sql` map is a DERIVED PROJECTION
//! ([`SourceMap::compiled_slices`]), not stored twice.

use crate::domain::cte::CteGraph;
use crate::domain::span::{SourcePos, SourceSpan};
use serde::{Deserialize, Serialize};

/// WHY a source region corresponds to a node/zone/macro/column. The SECONDARY
/// tag (the PRIMARY axis is the span). `#[non_exhaustive]` because the
/// vocabulary GROWS — every future lineage kind is a new variant, NOT a new
/// payload section or a new map beside `compiled_sql`/`dag` (the `EdgeType`
/// precedent). Consumers match it exhaustively at one render site.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SpanRole {
    /// A compiled CTE / terminal-select body. `node_id` is a `dag.nodes[].id`.
    CteBody {
        /// The stable engine node id (a CTE alias, or the terminal node name).
        node_id: String,
    },
    /// A raw-Jinja control block (if/for/…). v1's `RawZone` kind.
    Zone {
        /// Which control-flow construct this zone is. Renamed on the wire to
        /// `zone_kind` so the field never collides with the internal `kind`
        /// discriminant tag (an internally-tagged serde enum cannot carry a
        /// variant field named the same as its tag).
        #[serde(rename = "zone_kind")]
        kind: ZoneKind,
    },
    // ── reserved, additive; NO payload reshape when they land ──
    // ExternalRef    { node_id: String },     // explorer cross-model: a ref() call site
    //                                          //   (fusion's nodes_with_ref_location pattern)
    // Source         { node_id: String },     // a source() call site
    // MacroExpansion { macro_id: String },    // {{ my_macro() }} → its compiled output
    //                                          //   (fusion's MacroSpan, raw=body compiled=site)
    // Column         { node_id: String, column: String }, // column-level lineage (CllEdge shape)
}

/// CONTROL-FLOW zone kinds ONLY. Macro expansion / lineage are [`SpanRole`]
/// variants, NOT `ZoneKind` — v1 conflated "control-flow zone" with "expansion
/// mapping" into one type; they are different and must not share a struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ZoneKind {
    /// A `{% if is_incremental() %}` guard.
    IncrementalGuard,
    /// A `{% for … %}` loop.
    ForLoop, /* Set, SnapshotGuard, … */
}

/// ONE correspondence between a raw region and a compiled region — the
/// cute-dbt analogue of fusion's `MacroSpan` (raw = `macro_span`, compiled =
/// `expanded_span`) and a JS source-map Token. This single type SUBSUMES v1's
/// `node_spans` (`CteBody`, both sides present) AND `raw_zones` (`Zone`,
/// compiled maybe `None`). `compiled: None` IS the honest "pruned this build"
/// verdict (fusion's locations-with-no-mapping) — never fabricated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMapEntry {
    /// Why this region exists.
    pub role: SpanRole,
    /// Region in `raw_code`. `None` for a pure-compiled entry whose raw origin
    /// cute-dbt cannot soundly locate (no raw scanner yet — S4/S5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<SourceSpan>,
    /// Region in `compiled_code`. `None` ⇒ tokens absent from this build (the
    /// honest `CompiledOut`). NEVER fabricated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiled: Option<SourceSpan>,
}

impl SourceMapEntry {
    /// The CORE presence read — honest-by-construction. In core S2 every entry
    /// is a `CteBody` whose `compiled` is `Some` (the engine always produces a
    /// compiled span for a parsed CTE / terminal select), so presence is
    /// TRIVIALLY `compiled.is_some()` — no 3-state, no containment scan. The
    /// only producer of a `compiled: None` entry, or of a span nested INSIDE
    /// another's range, is the deferred `RawZoneSync` raw-zone path (S5), where
    /// the 3-state `presence()` lands WITH it.
    #[must_use]
    pub fn is_compiled_in(&self) -> bool {
        self.compiled.is_some()
    }
}

/// RESERVED 3-state presence (cheap, forward-compatible) — the deriving METHOD
/// lands behind `Experiment::RawZoneSync` (S5), NOT in core. The enum is kept
/// now so the wire/derive surface is stable; the deriving method, `Structural`
/// detection, and the containment scan are S5 work, because only the S5
/// raw-zone path can ever produce a `compiled: None` or a span strictly nested
/// inside a CTE body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Tokens present in this build's compiled output.
    CompiledIn,
    /// Tokens pruned from this build (the honest verdict).
    CompiledOut,
    /// Compiled in, but strictly nested inside a larger CTE body.
    Structural,
}

/// One model's full raw↔compiled source map: the faithful text + the
/// correspondence table. The SINGLE source of truth; per-CTE `compiled_sql`
/// slices are a DERIVED PROJECTION of `(compiled, entries)` — not stored twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceMap {
    /// `Node::compiled_code` VERBATIM — the one faithful text every `compiled`
    /// span indexes into (leading comment + with/comma glue intact). Rejoining
    /// `compiled_sql` slices is NOT byte-equal (v1 gap-(a), confirmed).
    pub compiled: String,
    /// The correspondence table — one entry per CTE/terminal body (and, in
    /// later slices, per raw zone / column).
    pub entries: Vec<SourceMapEntry>,
}

impl SourceMap {
    /// Assemble the per-model `SourceMap` from a parsed [`CteGraph`] and the
    /// model's full compiled text — the cute-dbt#40 retain-don't-recompute
    /// pattern: the engine already computed each node's `source_span` (S1);
    /// this folds those facts into one `CteBody` entry apiece over the faithful
    /// `compiled` text. No re-parse, no new compute — a pure domain fold.
    ///
    /// `terminal_node_name` is the stable id the engine assigns the terminal
    /// select (passed in so the domain never imports the adapter const). For a
    /// WITH-bearing model every node carries its retained span. For a WITH-less
    /// model the graph has zero nodes (the engine emits no terminal node when
    /// there is no `WITH`); this synthesizes ONE terminal `CteBody` entry over
    /// the whole text so `compiled_slices().keys()` agree with the DAG by
    /// construction (the WITH-less DAG is empty, so the gate is vacuous; the
    /// derived `compiled_sql` keys on the terminal id, fixing v1's empty-graph
    /// path that keyed by model name).
    ///
    /// Returns `None` when the model has no compiled code (a seed/source) — the
    /// honest "no source map" verdict, never a fabricated empty map.
    #[must_use]
    pub fn from_cte_graph(
        graph: &CteGraph,
        compiled: &str,
        terminal_node_name: &str,
    ) -> Option<Self> {
        if compiled.is_empty() {
            return None;
        }
        let mut entries: Vec<SourceMapEntry> = graph
            .nodes()
            .iter()
            .filter_map(|node| {
                node.source_span().map(|span| SourceMapEntry {
                    role: SpanRole::CteBody {
                        node_id: node.name().to_owned(),
                    },
                    raw: None,
                    compiled: Some(*span),
                })
            })
            .collect();
        // WITH-less model: the engine emits no nodes, so synthesize the single
        // terminal CteBody entry over the whole faithful text. The span carries
        // honest line/col endpoints (1-based start; the line count is the
        // last-line number) so the JS gutter sync resolves it like any node.
        //
        // Gate on GRAPH emptiness, NOT on `entries.is_empty()` (cute-dbt#445
        // CodeRabbit): `entries` is also empty when the graph HAS nodes but
        // every `source_span()` is `None` (the degrade-not-lie drop above). In
        // that case synthesizing a whole-text terminal span would FABRICATE a
        // claim the engine never made — preserve honest absence instead.
        if graph.nodes().is_empty() {
            entries.push(SourceMapEntry {
                role: SpanRole::CteBody {
                    node_id: terminal_node_name.to_owned(),
                },
                raw: None,
                compiled: Some(whole_text_span(compiled)),
            });
        }
        Some(Self {
            compiled: compiled.to_owned(),
            entries,
        })
    }

    /// DERIVE the legacy per-CTE map (keys == `CteBody` node ids by
    /// construction, which FIXES v1's false `node_spans.keys() ==
    /// compiled_sql.keys()` invariant — the empty-graph path keyed
    /// `compiled_sql` by model name; here both derive from one table so keys
    /// agree).
    ///
    /// BYTE-FAITHFULNESS INVARIANT (Blocker-1). `compiled[c.start.byte ..
    /// c.end.byte]` byte-EQUALS the legacy slice for EVERY `CteBody` entry —
    /// but ONLY because the terminal entry's `start.byte` was advanced past the
    /// leading-trim prefix at CONSTRUCTION time in the engine (cute-dbt#444). A
    /// non-terminal `CteBody` span is the untrimmed `name AS ( … )` range and
    /// needs no adjustment.
    #[must_use]
    pub fn compiled_slices(&self) -> std::collections::BTreeMap<String, String> {
        self.entries
            .iter()
            .filter_map(|e| match (&e.role, e.compiled) {
                (SpanRole::CteBody { node_id }, Some(c)) => Some((
                    node_id.clone(),
                    self.compiled[c.start.byte as usize..c.end.byte as usize].to_owned(),
                )),
                _ => None,
            })
            .collect()
    }

    /// The `CteBody` node-span table — every entry whose role is `CteBody` and
    /// whose `compiled` span is present, keyed by node id. The render
    /// projection (`CodeMapPayload.node_spans`) is this map; the JS DAG↔code
    /// sync indexes the compiled `<pre>` through it.
    #[must_use]
    pub fn node_spans(&self) -> std::collections::BTreeMap<String, SourceSpan> {
        self.entries
            .iter()
            .filter_map(|e| match (&e.role, e.compiled) {
                (SpanRole::CteBody { node_id }, Some(c)) => Some((node_id.clone(), c)),
                _ => None,
            })
            .collect()
    }
}

/// A [`SourceSpan`] over the whole `[0, text.len())` of `text`, with honest
/// 1-based line/col endpoints (the end position is the last line number, and
/// the column past the final character). The end `byte` is `text.len()` — the
/// guarded `usize → u32` cast caps at `u32::MAX` rather than wrapping, matching
/// the engine's single ingestion boundary (cute-dbt#444); 4 GB of compiled SQL
/// is not a real model.
fn whole_text_span(text: &str) -> SourceSpan {
    let end_byte = u32::try_from(text.len()).unwrap_or(u32::MAX);
    // 1-based line count; the trailing line of the half-open end.
    let end_line =
        u32::try_from(text.bytes().filter(|&b| b == b'\n').count() + 1).unwrap_or(u32::MAX);
    // 1-based column on the final line (chars after the last newline + 1).
    let last_line = text.rsplit('\n').next().unwrap_or("");
    let end_col = u32::try_from(last_line.chars().count() + 1).unwrap_or(u32::MAX);
    SourceSpan {
        start: SourcePos {
            line: 1,
            col: 1,
            byte: 0,
        },
        end: SourcePos {
            line: end_line,
            col: end_col,
            byte: end_byte,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::cte::{CteEdge, CteNode, EdgeType};

    const TERMINAL: &str = "(final select)";

    fn span(start: u32, end: u32) -> SourceSpan {
        SourceSpan {
            start: SourcePos {
                line: 1,
                col: start + 1,
                byte: start,
            },
            end: SourcePos {
                line: 1,
                col: end + 1,
                byte: end,
            },
        }
    }

    /// A `CteNode` carrying a name + retained span + raw slice — the S1 shape.
    fn cte(name: &str, sp: SourceSpan, raw: &str) -> CteNode {
        CteNode::new(name, Some(sp), Some(raw.to_owned()), None)
    }

    // ── TDD #1: SourceMap.compiled byte-equals Node::compiled_code ──
    #[test]
    fn compiled_is_the_faithful_full_text() {
        let compiled = "with a as (select 1) select * from a";
        let node_a = cte("a", span(5, 20), "a as (select 1)");
        let node_t = cte(TERMINAL, span(21, 36), "select * from a");
        let graph = CteGraph::new(vec![node_a, node_t], vec![]);
        let sm = SourceMap::from_cte_graph(&graph, compiled, TERMINAL).expect("compiled present");
        assert_eq!(
            sm.compiled, compiled,
            "SourceMap.compiled is Node::compiled_code verbatim"
        );
    }

    // ── TDD #2: derived compiled_sql byte-equals the legacy node slices ──
    #[test]
    fn compiled_slices_byte_equal_the_node_raw_slices() {
        // Each span slices the faithful text to the node's raw_sql by
        // construction (the S1 contract). The derived map must reproduce the
        // legacy per-node slices exactly.
        let compiled = "with a as (select 1), b as (select 2) select * from b";
        // byte ranges of each `name AS ( … )` extent and the terminal select.
        let a = span(5, 20); // "a as (select 1)"
        let b = span(22, 37); // "b as (select 2)"
        let t = span(38, 53); // "select * from b"
        assert_eq!(&compiled[a.byte_range()], "a as (select 1)");
        assert_eq!(&compiled[b.byte_range()], "b as (select 2)");
        assert_eq!(&compiled[t.byte_range()], "select * from b");
        let graph = CteGraph::new(
            vec![
                cte("a", a, "a as (select 1)"),
                cte("b", b, "b as (select 2)"),
                cte(TERMINAL, t, "select * from b"),
            ],
            vec![],
        );
        let sm = SourceMap::from_cte_graph(&graph, compiled, TERMINAL).unwrap();
        let slices = sm.compiled_slices();
        assert_eq!(slices.get("a").map(String::as_str), Some("a as (select 1)"));
        assert_eq!(slices.get("b").map(String::as_str), Some("b as (select 2)"));
        assert_eq!(
            slices.get(TERMINAL).map(String::as_str),
            Some("select * from b")
        );
    }

    // ── TDD #3: a no-WITH model is ONE terminal CteBody entry over the whole
    //    text; node_spans.keys() == compiled_sql.keys() by construction ──
    #[test]
    fn no_with_model_is_one_terminal_entry_over_whole_text() {
        let compiled = "select id, name from raw_customers";
        // The engine emits ZERO nodes for a WITH-less model.
        let graph = CteGraph::default();
        let sm = SourceMap::from_cte_graph(&graph, compiled, TERMINAL).unwrap();
        assert_eq!(
            sm.entries.len(),
            1,
            "exactly one synthesized terminal entry"
        );
        let entry = &sm.entries[0];
        assert_eq!(
            entry.role,
            SpanRole::CteBody {
                node_id: TERMINAL.to_owned()
            },
            "the single entry is the terminal CteBody"
        );
        let c = entry.compiled.expect("terminal span present");
        assert_eq!(c.start.byte, 0, "spans from byte 0");
        assert_eq!(
            c.end.byte as usize,
            compiled.len(),
            "spans to the end of the text"
        );
        // The whole text is recovered.
        assert_eq!(&sm.compiled[c.byte_range()], compiled);
        // keys agree by construction.
        let node_keys: Vec<String> = sm.node_spans().into_keys().collect();
        let slice_keys: Vec<String> = sm.compiled_slices().into_keys().collect();
        assert_eq!(
            node_keys, slice_keys,
            "node_spans.keys() == compiled_sql.keys()"
        );
        assert_eq!(slice_keys, vec![TERMINAL.to_owned()]);
    }

    #[test]
    fn no_compiled_code_yields_no_source_map() {
        // A seed/source with no compiled code → None, never a fabricated map.
        let graph = CteGraph::default();
        assert!(
            SourceMap::from_cte_graph(&graph, "", TERMINAL).is_none(),
            "empty compiled ⇒ None (honest absence)"
        );
    }

    #[test]
    fn whole_text_span_carries_honest_line_col_endpoints() {
        // Multi-line text: the synthesized terminal span's end line/col reflect
        // the real geometry (so the JS gutter sync resolves it).
        let text = "select\n  id,\n  name\nfrom t";
        let s = whole_text_span(text);
        assert_eq!(
            s.start,
            SourcePos {
                line: 1,
                col: 1,
                byte: 0
            }
        );
        assert_eq!(s.end.byte as usize, text.len());
        assert_eq!(s.end.line, 4, "four lines");
        // last line "from t" is 6 chars → end col 7 (1-based, past the last char).
        assert_eq!(s.end.col, 7);
    }

    #[test]
    fn node_spans_and_slices_keys_agree_for_with_model() {
        let compiled = "with a as (select 1) select * from a";
        let graph = CteGraph::new(
            vec![
                cte("a", span(5, 20), "a as (select 1)"),
                cte(TERMINAL, span(21, 36), "select * from a"),
            ],
            vec![],
        );
        let sm = SourceMap::from_cte_graph(&graph, compiled, TERMINAL).unwrap();
        // Both projections are BTreeMaps (sorted keys); they agree as SETS by
        // construction — "(final select)" sorts before "a".
        let node_keys: Vec<String> = sm.node_spans().into_keys().collect();
        let slice_keys: Vec<String> = sm.compiled_slices().into_keys().collect();
        assert_eq!(
            node_keys, slice_keys,
            "node_spans.keys() == compiled_sql.keys()"
        );
        assert_eq!(node_keys, vec![TERMINAL.to_owned(), "a".to_owned()]);
    }

    #[test]
    fn node_without_a_span_is_dropped_not_fabricated() {
        // A node the engine could not soundly locate (source_span == None) is
        // simply absent from the map — never a fabricated span (degrade-not-lie).
        let compiled = "with a as (select 1) select * from a";
        let node_a = CteNode::new("a", None, Some("a as (select 1)".to_owned()), None);
        let node_t = cte(TERMINAL, span(21, 36), "select * from a");
        let graph = CteGraph::new(vec![node_a, node_t], vec![]);
        let sm = SourceMap::from_cte_graph(&graph, compiled, TERMINAL).unwrap();
        let keys: Vec<String> = sm.compiled_slices().into_keys().collect();
        assert_eq!(
            keys,
            vec![TERMINAL.to_owned()],
            "the span-less node is dropped"
        );
    }

    #[test]
    fn with_graph_all_missing_spans_does_not_synthesize_terminal() {
        // A graph with nodes whose source_span() are all None must NOT fabricate
        // a whole-text terminal span (cute-dbt#445 CodeRabbit): the terminal
        // synthesis is gated on GRAPH emptiness, not `entries.is_empty()`. The
        // graph is non-empty here, so even though every node is span-less and
        // dropped (degrade-not-lie), no terminal entry is fabricated — honest
        // absence is preserved.
        let compiled = "with a as (select 1) select * from a";
        let node_a = CteNode::new("a", None, Some("a as (select 1)".to_owned()), None);
        let node_t = CteNode::new(TERMINAL, None, Some("select * from a".to_owned()), None);
        let graph = CteGraph::new(vec![node_a, node_t], vec![]);
        let sm = SourceMap::from_cte_graph(&graph, compiled, TERMINAL).unwrap();
        assert!(
            sm.entries.is_empty(),
            "no entry is fabricated when the graph has nodes but every span is None"
        );
        assert!(
            sm.compiled_slices().is_empty(),
            "no whole-text terminal slice is fabricated"
        );
    }

    // ── TDD #5: exhaustive property invariants ──
    #[test]
    fn is_compiled_in_reads_the_compiled_presence() {
        let with_span = SourceMapEntry {
            role: SpanRole::CteBody {
                node_id: "a".to_owned(),
            },
            raw: None,
            compiled: Some(span(0, 4)),
        };
        assert!(with_span.is_compiled_in());
        let pruned = SourceMapEntry {
            role: SpanRole::Zone {
                kind: ZoneKind::IncrementalGuard,
            },
            raw: Some(span(0, 4)),
            compiled: None,
        };
        assert!(!pruned.is_compiled_in(), "compiled: None ⇒ not compiled-in");
    }

    #[test]
    fn span_invariants_hold_for_every_emitted_entry() {
        // For an assembled SourceMap: every CteBody compiled span satisfies
        // start.byte <= end.byte <= compiled.len(), 1 <= start.line, and the
        // slice byte-equals the compiled_slices() value. Enumerate over a small
        // set of WITH/no-WITH shapes.
        let cases: &[(&str, CteGraph)] = &[
            ("select 1", CteGraph::default()),
            ("select * from t\nwhere x", CteGraph::default()),
        ];
        for (compiled, graph) in cases {
            let sm = SourceMap::from_cte_graph(graph, compiled, TERMINAL).unwrap();
            let slices = sm.compiled_slices();
            for e in &sm.entries {
                if let (SpanRole::CteBody { node_id }, Some(c)) = (&e.role, e.compiled) {
                    assert!(c.start.byte <= c.end.byte, "start.byte <= end.byte");
                    assert!(
                        c.end.byte as usize <= sm.compiled.len(),
                        "end.byte <= compiled.len()"
                    );
                    assert!(c.start.line >= 1, "1 <= start.line");
                    assert!(c.start.line <= c.end.line, "start.line <= end.line");
                    assert_eq!(
                        &sm.compiled[c.byte_range()],
                        slices.get(node_id).unwrap(),
                        "compiled[entry.byte_range()] byte-equals compiled_slices()[node_id]"
                    );
                }
            }
        }
    }

    // ── TDD #5 (cont.): serde round-trip over all new PODs ──
    #[test]
    fn serde_round_trip_source_map() {
        let compiled = "with a as (select 1) select * from a".to_owned();
        let sm = SourceMap {
            compiled,
            entries: vec![
                SourceMapEntry {
                    role: SpanRole::CteBody {
                        node_id: "a".to_owned(),
                    },
                    raw: None,
                    compiled: Some(span(5, 20)),
                },
                SourceMapEntry {
                    role: SpanRole::Zone {
                        kind: ZoneKind::ForLoop,
                    },
                    raw: Some(span(0, 4)),
                    compiled: None,
                },
            ],
        };
        let json = serde_json::to_string(&sm).unwrap();
        let back: SourceMap = serde_json::from_str(&json).unwrap();
        assert_eq!(sm, back);
    }

    #[test]
    fn span_role_cte_body_serialization_is_snake_case_tagged() {
        let cte_body = SpanRole::CteBody {
            node_id: "x".to_owned(),
        };
        let json = serde_json::to_string(&cte_body).unwrap();
        assert_eq!(json, r#"{"kind":"cte_body","node_id":"x"}"#);
    }

    #[test]
    fn span_role_zone_round_trips_despite_inner_kind_field() {
        // The `Zone { kind: ZoneKind }` field shares the serde discriminant
        // name `kind`; this pins that the round-trip is still lossless (the
        // reserved Zone role is not produced in core S2, but the wire surface
        // must stay stable for S4/S5).
        let zone = SpanRole::Zone {
            kind: ZoneKind::IncrementalGuard,
        };
        let json = serde_json::to_string(&zone).unwrap();
        let back: SpanRole = serde_json::from_str(&json).unwrap();
        assert_eq!(zone, back, "Zone role round-trips losslessly");
    }

    #[test]
    fn zone_kind_round_trips() {
        for k in [ZoneKind::IncrementalGuard, ZoneKind::ForLoop] {
            let json = serde_json::to_string(&k).unwrap();
            let back: ZoneKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, back);
        }
    }

    #[test]
    fn source_map_entry_omits_none_sides() {
        // skip_serializing_if keeps the wire (and goldens) lean: a both-None
        // entry serializes only its role.
        let entry = SourceMapEntry {
            role: SpanRole::CteBody {
                node_id: "a".to_owned(),
            },
            raw: None,
            compiled: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(json, r#"{"role":{"kind":"cte_body","node_id":"a"}}"#);
    }

    #[test]
    fn presence_variants_are_distinct() {
        // The reserved enum is stable now (the deriving method lands in S5).
        assert_ne!(Presence::CompiledIn, Presence::CompiledOut);
        assert_ne!(Presence::CompiledIn, Presence::Structural);
        assert_ne!(Presence::CompiledOut, Presence::Structural);
    }

    // unused-import guard: CteEdge/EdgeType used to keep the test module's
    // domain-cte surface honest (a CteGraph with an edge still assembles).
    #[test]
    fn graph_with_edges_assembles() {
        let compiled = "with a as (select 1) select * from a";
        let graph = CteGraph::new(
            vec![
                cte("a", span(5, 20), "a as (select 1)"),
                cte(TERMINAL, span(21, 36), "select * from a"),
            ],
            vec![CteEdge::new(0, 1, EdgeType::From)],
        );
        let sm = SourceMap::from_cte_graph(&graph, compiled, TERMINAL).unwrap();
        assert_eq!(sm.entries.len(), 2);
    }
}
