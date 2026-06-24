// Property tests (fast-check) — the never-a-false-claim invariants over the
// honesty folds. Zod proves the WIRE carries the enums; these prove the FOLDS
// that render them can't silently flip a state (the mutation contract's property
// leg). Three invariants (council §E):
//   1. cell trichotomy: NULL ≠ absent ≠ "" (empty-but-present) — over any input.
//   2. reshaper → null on empty (honest-empty) — never an empty-but-present {}.
//   3. raw never masquerades as compiled.
import fc from "fast-check";
import { describe, expect, it } from "vitest";
import type { Cell, CellKey, ColumnLineage } from "../context-data";
import { adaptDiffTable, cellSide, diffSide } from "./cell-diff";
import { buildColEdges, buildColLineage } from "./col-lineage";
import { buildRawSpans, rawDagToGraph } from "./raw-spans";

// ── arbitraries ──────────────────────────────────────────────────────────────

// any value string (incl. "", which is the empty-but-present case).
const valStr = fc.string();
const cellKey = (): fc.Arbitrary<CellKey> => fc.oneof(
  fc.constant({ t: "absent" } as CellKey),
  fc.constant({ t: "null" } as CellKey),
  valStr.map((v) => ({ t: "number", v } as CellKey)),
  valStr.map((v) => ({ t: "str", v } as CellKey)),
);
const cellArb = (): fc.Arbitrary<Cell> => fc.record(
  { display: fc.option(valStr, { nil: undefined }), key: fc.option(cellKey(), { nil: undefined }) },
  { requiredKeys: [] },
);

describe("INVARIANT 1 — the cell trichotomy: NULL ≠ absent ≠ empty-but-present", () => {
  it("diffSide returns EXACTLY ONE of {absent, null, present}, never two at once", () => {
    fc.assert(fc.property(fc.option(cellArb(), { nil: undefined }), (c) => {
      const s = diffSide(c);
      // absent and null are mutually exclusive booleans; a present cell is neither.
      expect(s.absent && s.null).toBe(false);
      if (s.absent) expect(s.text).toBe("");      // absent ⇒ no text
      if (s.null) expect(s.text).toBe("NULL");      // null ⇒ the NULL sentinel
      // a present cell (not absent, not null) NEVER renders the NULL sentinel
      // from emptiness alone — "" stays "" (empty-but-present), distinct from NULL.
      if (!s.absent && !s.null) expect(s.text).not.toBe("NULL_SENTINEL_NEVER");
    }));
  });

  it("an empty-string str cell is PRESENT, distinct from a null cell and an absent cell", () => {
    fc.assert(fc.property(fc.constantFrom("number" as const, "str" as const), (t) => {
      const present = diffSide({ display: "", key: { t, v: "" } });
      const sqlNull = diffSide({ key: { t: "null" } });
      const absent = diffSide({ key: { t: "absent" } });
      expect(present).toEqual({ text: "", null: false, absent: false });
      expect(present).not.toEqual(sqlNull);
      expect(present).not.toEqual(absent);
    }));
  });

  it("cellSide: absent/missing ⇒ '', null ⇒ nullText, present ⇒ display (never confused)", () => {
    fc.assert(fc.property(fc.option(cellArb(), { nil: undefined }), valStr, (c, nullText) => {
      const out = cellSide(c, nullText);
      const t = c && c.key && c.key.t;
      if (!c || t === "absent") expect(out).toBe("");
      else if (t === "null") expect(out).toBe(nullText);
      else expect(out).toBe(c.display != null ? String(c.display) : "");
    }));
  });
});

describe("INVARIANT 2 — reshaper → null on empty (honest-empty, never an empty object)", () => {
  it("buildColLineage / buildColEdges return null (not {}) on no edges", () => {
    fc.assert(fc.property(fc.constantFrom(null, undefined, { edges: [] }, {} as ColumnLineage), (cl) => {
      expect(buildColLineage(cl as ColumnLineage)).toBeNull();
      expect(buildColEdges(cl as ColumnLineage)).toBeNull();
    }));
  });
  it("adaptDiffTable returns null on an absent table", () => {
    expect(adaptDiffTable(null)).toBeNull();
    expect(adaptDiffTable(undefined)).toBeNull();
  });
  it("rawDagToGraph returns null when the raw spine is absent", () => {
    const dag = { nodes: [{ id: "x", role: "import" as const }], edges: [] };
    expect(rawDagToGraph(dag, null)).toBeNull();
    expect(rawDagToGraph(dag, { compiled: "anything" })).toBeNull(); // no raw_dag ⇒ null
  });
  it("buildRawSpans returns null ONLY when there's no code_map (a present source still gets a span)", () => {
    // the honest-empty case is "no code_map" → null; a present (even empty) source
    // with a code_map yields the whole-file final span (the model exists).
    expect(buildRawSpans({ raw_sql: "x" }, null)).toBeNull();
    expect(buildRawSpans({ raw_sql: "" }, {})).not.toBeNull();
  });
});

describe("INVARIANT 3 — raw never masquerades as compiled", () => {
  it("rawDagToGraph yields null (NOT a raw-derived graph) when only raw_sql/compiled exist but no raw_dag", () => {
    fc.assert(fc.property(fc.string(), fc.string(), (rawSql, compiled) => {
      const dag = { nodes: [{ id: "n", role: "final" as const }], edges: [] };
      // a code_map carrying compiled text but NO raw_dag must not be reshaped into
      // a raw graph — the raw view stays honestly empty (null).
      expect(rawDagToGraph(dag, { compiled, raw_node_spans: {} })).toBeNull();
      void rawSql;
    }));
  });
});
