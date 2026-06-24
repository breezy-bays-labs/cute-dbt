// The Zod parity-gate tests — the schema must ACCEPT the full spine fixture AND
// tolerate the thin since-review shape, while REJECTING honesty-enum downgrades
// (a bool where a string-literal union is required) and a missing required field.
import { describe, it, expect } from "vitest";
import {
  parseContext, parseContextEnvelope, ContextDataSchema, ContextEnvelopeSchema,
  PresenceSchema, ColumnConfidenceSchema, CoverageStatusSchema, CellKeyTypeSchema,
} from "./schema";
import { rawFixture, FIXTURE_IDS, loadFixture } from "../data/fixtures";

describe("ContextDataSchema (parity / drift gate)", () => {
  it("accepts the full 16-model context.440 spine", () => {
    const parsed = parseContext(rawFixture("context.440"));
    expect(parsed.models.length).toBe(16);
    expect(parsed.pr_ref?.number).toBe(440);
  });

  it("accepts the 2-model minimal context.sample", () => {
    const parsed = parseContext(rawFixture("context.sample"));
    expect(parsed.models.length).toBeGreaterThanOrEqual(1);
    expect(typeof parsed.baseline).toBe("string");
  });

  it("tolerates the thin since-review shape (optional spine fields)", () => {
    const parsed = parseContext(rawFixture("context.440.since-review"));
    expect(parsed.models.length).toBeGreaterThan(0);
  });

  it("loads + validates every catalogued fixture without throwing", () => {
    for (const id of FIXTURE_IDS) {
      expect(() => loadFixture(id), `fixture ${id} failed to validate`).not.toThrow();
    }
  });

  it("REJECTS a missing required field (baseline)", () => {
    const bad = { models: [] };
    expect(() => parseContext(bad)).toThrow();
  });

  it("REJECTS an honesty-enum downgrade (edge_type as a bool, never silently coerced)", () => {
    const bad = {
      baseline: "x",
      models: [
        {
          name: "m",
          dag: {
            nodes: [{ id: "a", role: "import" }],
            // edge_type MUST be the string-literal union — a bool fails loudly.
            edges: [{ from: "a", to: "a", edge_type: true }],
          },
          compiled_sql: {},
          is_recursive: false,
          tests: [],
        },
      ],
    };
    expect(() => parseContext(bad)).toThrow();
  });

  it("REJECTS an unknown NodeRole (the honesty union is closed)", () => {
    const bad = {
      baseline: "x",
      models: [
        {
          name: "m",
          dag: { nodes: [{ id: "a", role: "bogus" }], edges: [] },
          compiled_sql: {},
          is_recursive: false,
          tests: [],
        },
      ],
    };
    const r = ContextDataSchema.safeParse(bad);
    expect(r.success).toBe(false);
  });
});

describe("the four honesty axes are z.enum string unions (never a bool)", () => {
  it("PRESENCE accepts the 3 states, rejects a bool", () => {
    ["compiled_in", "compiled_out", "structural"].forEach((s) => expect(PresenceSchema.safeParse(s).success).toBe(true));
    expect(PresenceSchema.safeParse(true).success).toBe(false);
    expect(PresenceSchema.safeParse("nope").success).toBe(false);
  });
  it("CONFIDENCE accepts the 3 states, rejects a bool", () => {
    ["resolved", "opaque", "ambiguous"].forEach((s) => expect(ColumnConfidenceSchema.safeParse(s).success).toBe(true));
    expect(ColumnConfidenceSchema.safeParse(false).success).toBe(false);
  });
  it("COVERAGE accepts the 3 states, rejects a bool", () => {
    ["covered", "uncovered", "unknown"].forEach((s) => expect(CoverageStatusSchema.safeParse(s).success).toBe(true));
    expect(CoverageStatusSchema.safeParse(true).success).toBe(false);
  });
  it("CELL key.t accepts the 4 states, rejects a bool", () => {
    ["absent", "null", "number", "str"].forEach((s) => expect(CellKeyTypeSchema.safeParse(s).success).toBe(true));
    expect(CellKeyTypeSchema.safeParse(true).success).toBe(false);
  });

  it("REJECTS a presence DOWNGRADE on a real-shaped code_map zone (bool where the enum belongs)", () => {
    const bad = {
      baseline: "x",
      models: [{
        name: "m", dag: { nodes: [{ id: "a", role: "import" }], edges: [] }, compiled_sql: {},
        code_map: { raw_zones: [{ kind: "incremental_guard", start: { line: 1, col: 1, byte: 0 }, end: { line: 2, col: 1, byte: 10 }, presence: true }] },
      }],
    };
    expect(ContextDataSchema.safeParse(bad).success).toBe(false);
  });

  it("REJECTS a confidence DOWNGRADE on a column edge (bool where the enum belongs)", () => {
    const bad = {
      baseline: "x",
      models: [{
        name: "m", dag: { nodes: [{ id: "a", role: "import" }], edges: [] }, compiled_sql: {},
        column_lineage: { edges: [{ from_col: { scope: { intra: { node_id: "a" } }, column: "x" }, to_col: { scope: { intra: { node_id: "b" } }, column: "y" }, kind: "derived", confidence: false }] },
      }],
    };
    expect(ContextDataSchema.safeParse(bad).success).toBe(false);
  });

  it("REJECTS a coverage DOWNGRADE on a finding verdict (bool where the enum belongs)", () => {
    const bad = {
      baseline: "x",
      models: [{
        name: "m", dag: { nodes: [{ id: "a", role: "import" }], edges: [] }, compiled_sql: {},
        findings: [{ check: "grain.x", tier: "high", verdict: { status: true } }],
      }],
    };
    expect(ContextDataSchema.safeParse(bad).success).toBe(false);
  });

  it("REJECTS a cell key.t DOWNGRADE (bool where the trichotomy belongs)", () => {
    const bad = {
      baseline: "x",
      models: [{
        name: "m", dag: { nodes: [{ id: "a", role: "import" }], edges: [] }, compiled_sql: {},
        tests: [{ id: "t", name: "t", expected: { table: { columns: ["c"], rows: [{ cells: [{ display: "1", key: { t: true } }] }] } } }],
      }],
    };
    expect(ContextDataSchema.safeParse(bad).success).toBe(false);
  });
});

describe("ContextEnvelopeSchema — the real --context-out wrapper (S3a)", () => {
  it("parses a wrapped { metadata: { schema_version }, data } and returns the inner payload", () => {
    const wrapped = { metadata: { schema_version: 1 }, data: rawFixture("context.440") };
    const inner = parseContextEnvelope(wrapped);
    expect(inner.models.length).toBe(16);
  });
  it("the envelope round-trips every catalogued fixture as its data member", () => {
    for (const id of FIXTURE_IDS) {
      const wrapped = { metadata: { schema_version: 1 }, data: rawFixture(id) };
      expect(() => parseContextEnvelope(wrapped), `envelope(${id}) failed`).not.toThrow();
    }
  });
  it("REJECTS a missing schema_version (the version anchor is required)", () => {
    expect(ContextEnvelopeSchema.safeParse({ data: rawFixture("context.sample") }).success).toBe(false);
    expect(ContextEnvelopeSchema.safeParse({ metadata: {}, data: rawFixture("context.sample") }).success).toBe(false);
  });
  it("REJECTS a drifted inner payload (the gate is bidirectional through the wrapper)", () => {
    const bad = { metadata: { schema_version: 1 }, data: { models: [] } }; // no baseline
    expect(ContextEnvelopeSchema.safeParse(bad).success).toBe(false);
  });
});
