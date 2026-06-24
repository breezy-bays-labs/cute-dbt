// Fuzz — rawDagToGraph / buildRawSpans byte-span parsing (the Q4 mandate; the TS
// analog of the Rust --pr-diff bolero target). Random/adversarial inputs must:
//   - NEVER throw (fail-closed: a malformed code_map yields a graph or null).
//   - NEVER fabricate a span outside the source bounds (buildRawSpans line spans
//     stay within [1, total]; rawDagToGraph emits only ids drawn from the input).
import fc from "fast-check";
import { describe, expect, it } from "vitest";
import type { CodeMap, DagNode, DagPayload, RawZone, SourcePos } from "../context-data";
import { buildRawSpans, rawDagToGraph } from "./raw-spans";

// arbitrary byte position (lines/bytes incl. degenerate/out-of-order values).
const posArb = (): fc.Arbitrary<SourcePos> => fc.record({
  line: fc.integer({ min: -5, max: 200 }),
  col: fc.integer({ min: 0, max: 80 }),
  byte: fc.integer({ min: -10, max: 5000 }),
});
const zoneArb = (): fc.Arbitrary<RawZone> => fc.record({
  kind: fc.constantFrom("for_loop", "incremental_guard", "if_block", "weird"),
  template: fc.option(fc.string(), { nil: undefined }),
  loop: fc.option(fc.string(), { nil: undefined }),
  start: fc.option(posArb(), { nil: undefined }),
  end: fc.option(posArb(), { nil: undefined }),
  presence: fc.constantFrom("compiled_in", "compiled_out", "structural"),
}, { requiredKeys: ["kind", "presence"] });

const nodeArb = (): fc.Arbitrary<DagNode> => fc.record({
  id: fc.string({ minLength: 1 }),
  label: fc.option(fc.string(), { nil: undefined }),
  role: fc.constantFrom("import", "transform", "final", "cte", "zone", "terminal"),
}, { requiredKeys: ["id", "role"] });

// A WELL-FORMED dag (the contract: the spine never emits dangling edges — every
// edge endpoint is a declared node). With no nodes there are no edges.
const dagArb = (): fc.Arbitrary<DagPayload> => fc.array(nodeArb(), { maxLength: 6 }).chain((nodes) => {
  const ids = nodes.map((n) => n.id);
  if (!ids.length) return fc.constant({ nodes, edges: [] });
  const edge = fc.record({ from: fc.constantFrom(...ids), to: fc.constantFrom(...ids), edge_type: fc.constant("from" as const) });
  return fc.record({ nodes: fc.constant(nodes), edges: fc.array(edge, { maxLength: 6 }) });
});

const spanArb = () => fc.record({ start: posArb(), end: posArb() });
const codeMapArb = (): fc.Arbitrary<CodeMap> => fc.record({
  compiled: fc.option(fc.string(), { nil: undefined }),
  raw_node_spans: fc.option(fc.dictionary(fc.string({ minLength: 1 }), spanArb()), { nil: undefined }),
  raw_zones: fc.option(fc.array(zoneArb(), { maxLength: 4 }), { nil: undefined }),
  raw_dag: fc.option(fc.record({ nodes: fc.constant([] as DagNode[]) }), { nil: undefined }),
  node_map: fc.option(fc.record({
    raw: fc.dictionary(fc.string({ minLength: 1 }), fc.array(fc.string(), { maxLength: 3 })),
  }), { nil: undefined }),
}, { requiredKeys: [] });

describe("fuzz: rawDagToGraph never throws + never fabricates an out-of-input id", () => {
  it("returns null or a graph whose node ids are input ids or zone:N (never invented)", () => {
    fc.assert(fc.property(dagArb(), codeMapArb(), (dag, cm) => {
      let g: ReturnType<typeof rawDagToGraph>;
      expect(() => { g = rawDagToGraph(dag, cm); }).not.toThrow();
      g = rawDagToGraph(dag, cm);
      if (g === null) return;
      const inputIds = new Set(dag.nodes.map((n) => n.id));
      g.nodes.forEach((n) => {
        const ok = inputIds.has(n.id) || /^zone:\d+$/.test(n.id);
        expect(ok).toBe(true);
      });
      // every edge endpoint is a node present in the graph.
      const present = new Set(g.nodes.map((n) => n.id));
      g.edges.forEach(([a, b]) => { expect(present.has(a) || /^zone:\d+$/.test(a)).toBe(true); expect(present.has(b) || /^zone:\d+$/.test(b)).toBe(true); });
    }), { numRuns: 600 });
  });
});

// in-bounds spans for a known source: every span's lines fall within [1, total]
// (the spine's actual guarantee — it never emits a span past EOF). This fuzzes
// the COMPUTATION path (the (final select) heuristic) rather than re-asserting
// that a faithfully-COPIED out-of-bounds wire span got copied.
const sourceWithSpans = (): fc.Arbitrary<{ src: string; cm: CodeMap }> =>
  fc.integer({ min: 1, max: 60 }).chain((total) => {
    // build a source whose de-trailing-newline line count is EXACTLY `total`: a
    // non-blank first + last line (so no trailing-blank collapse skews the count),
    // interior blanks to exercise the blank-skip in the final-span heuristic.
    const src = Array.from({ length: total }, (_, i) =>
      i === 0 || i === total - 1 ? "line" + i : (i % 3 === 0 ? "" : "line" + i)).join("\n");
    const inBoundsPair = fc.tuple(fc.integer({ min: 1, max: total }), fc.integer({ min: 1, max: total }))
      .map(([a, b]) => [Math.min(a, b), Math.max(a, b)] as const);
    const inBoundsSpan = inBoundsPair.map(([s, e]) => ({ start: { line: s, col: 1, byte: s }, end: { line: e, col: 1, byte: e } }));
    const inBoundsZone = (): fc.Arbitrary<RawZone> => inBoundsPair.chain(([s, e]) => fc.record({
      kind: fc.constantFrom("for_loop", "incremental_guard"),
      start: fc.constant({ line: s, col: 1, byte: s }),
      end: fc.constant({ line: e, col: 1, byte: e }),
      presence: fc.constantFrom("compiled_in", "compiled_out", "structural"),
    }, { requiredKeys: ["kind", "presence", "start", "end"] }));
    return fc.record({
      src: fc.constant(src),
      cm: fc.record({
        raw_node_spans: fc.option(fc.dictionary(fc.string({ minLength: 1 }), inBoundsSpan, { maxKeys: 4 }), { nil: undefined }),
        raw_zones: fc.option(fc.array(inBoundsZone(), { maxLength: 3 }), { nil: undefined }),
      }, { requiredKeys: [] }) as fc.Arbitrary<CodeMap>,
    });
  });

describe("fuzz: buildRawSpans never throws + the COMPUTED (final select) span stays within [1, total]", () => {
  it("the heuristic-computed final span never fabricates a line past EOF", () => {
    fc.assert(fc.property(sourceWithSpans(), ({ src, cm }) => {
      let out: ReturnType<typeof buildRawSpans>;
      expect(() => { out = buildRawSpans({ raw_sql: src }, cm); }).not.toThrow();
      out = buildRawSpans({ raw_sql: src }, cm);
      if (out === null) return;
      const total = src.replace(/\n$/, "").split("\n").length;
      // every emitted span (copied OR computed) is within source bounds, because
      // the inputs are in-bounds AND the heuristic clamps to [.., total].
      for (const id of Object.keys(out)) {
        const s = out[id]!;
        expect(s.start.line).toBeGreaterThanOrEqual(1);
        expect(s.end.line).toBeLessThanOrEqual(total);
        expect(s.start.line).toBeLessThanOrEqual(s.end.line);
      }
    }), { numRuns: 600 });
  });

  it("never throws on adversarial (incl. out-of-bounds) code_map input — fail-closed", () => {
    fc.assert(fc.property(fc.string(), codeMapArb(), (rawSql, cm) => {
      expect(() => buildRawSpans({ raw_sql: rawSql }, cm)).not.toThrow();
    }), { numRuns: 400 });
  });
});
