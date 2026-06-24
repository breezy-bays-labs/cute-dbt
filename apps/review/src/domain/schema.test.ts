// The Zod parity-gate tests — the schema must ACCEPT the full spine fixture AND
// tolerate the thin since-review shape, while REJECTING honesty-enum downgrades
// (a bool where a string-literal union is required) and a missing required field.
import { describe, it, expect } from "vitest";
import { parseContext, ContextDataSchema } from "./schema";
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
