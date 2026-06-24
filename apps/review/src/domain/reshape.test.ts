// Reshaper unit tests — the pure context→render-surface folds. Covers the
// Pierre-side fix (Left→deletions), the BlockDiff→patch serializer, the
// buildContexts file assembly, and the toFlow edge-legend completeness (no
// silent legend gap — the EdgeType-completeness contract).
import { describe, it, expect } from "vitest";
import {
  pierreSide,
  isLiveThread,
  blockDiffToPatch,
  sourceToContextPatch,
  buildContexts,
  mentionCandidates,
  toFlow,
  toPrFlow,
} from "./reshape";
import type { BlockDiff, ContextData, PrDagPayload, RenderedThread } from "./context-data";

describe("pierreSide (the deletion-comment fix)", () => {
  it("maps Left → deletions and Right → additions", () => {
    expect(pierreSide("Left")).toBe("deletions");
    expect(pierreSide("Right")).toBe("additions");
  });
});

describe("isLiveThread", () => {
  const base: RenderedThread = {
    path: "a.sql", side: "Right", within_hunk: true, resolved: false, outdated: false, comments: [],
  };
  it("is live with a line and not outdated", () => {
    expect(isLiveThread({ ...base, line: 3 })).toBe(true);
  });
  it("is not live when outdated", () => {
    expect(isLiveThread({ ...base, line: 3, outdated: true })).toBe(false);
  });
  it("is not live with a null line", () => {
    expect(isLiveThread({ ...base, line: null })).toBe(false);
  });
});

describe("mentionCandidates (the @-mention picker source)", () => {
  // Minimal ContextData stub — only pr_ref matters for this fold.
  const ctx = (pr_ref: ContextData["pr_ref"]): ContextData =>
    ({ baseline: "b", models: [], pr_comments: { total: 0, by_model: [] }, pr_ref }) as unknown as ContextData;

  it("includes the PR author login and every reviewer login as plain strings", () => {
    const out = mentionCandidates(
      ctx({
        number: 1,
        title: "t",
        url: "u",
        author: "octocat",
        reviewers: [{ login: "alice" }, { login: "bob", state: "APPROVED" }],
      }),
    );
    expect(out).toEqual(["octocat", "alice", "bob"]);
    // every candidate is a string, so the picker's `r.toLowerCase()` never throws.
    expect(out.every((r) => typeof r === "string")).toBe(true);
    expect(() => out.map((r) => r.toLowerCase())).not.toThrow();
  });

  it("the author leads the picker and is deduped against a reviewer of the same login", () => {
    const out = mentionCandidates(
      ctx({ number: 1, title: "t", url: "u", author: "octocat", reviewers: [{ login: "octocat" }, { login: "alice" }] }),
    );
    expect(out).toEqual(["octocat", "alice"]);
    expect(out.indexOf("octocat")).toBe(0);
  });

  it("degrades to [] when pr_ref / author / reviewers are absent (no throw)", () => {
    expect(mentionCandidates(ctx(undefined))).toEqual([]);
    expect(mentionCandidates(ctx({ number: 1, title: "t", url: "u" }))).toEqual([]);
  });
});

describe("blockDiffToPatch", () => {
  it("serializes a unified patch with correct old/new counts + sigils", () => {
    const diff: BlockDiff = {
      lines: [
        { kind: "context", text: "select", emphasis: null },
        { kind: "removed", text: "  a", emphasis: null },
        { kind: "added", text: "  b", emphasis: null },
      ],
    };
    const patch = blockDiffToPatch(diff, "m.sql");
    expect(patch).toContain("--- a/m.sql");
    expect(patch).toContain("+++ b/m.sql");
    // 2 old lines (context + removed), 2 new lines (context + added).
    expect(patch).toContain("@@ -1,2 +1,2 @@");
    expect(patch).toContain("\n-  a");
    expect(patch).toContain("\n+  b");
  });
});

describe("sourceToContextPatch", () => {
  it("renders an all-context patch (every line a space sigil)", () => {
    const patch = sourceToContextPatch("x\ny", "m.sql");
    expect(patch).toContain("@@ -1,2 +1,2 @@");
    expect(patch).toContain("\n x");
    expect(patch).toContain("\n y");
  });
});

describe("buildContexts", () => {
  it("assembles a model's changed files + anchors threads by path", () => {
    const ctx: ContextData = {
      baseline: "b",
      models: [
        {
          name: "orders",
          path: "models/orders.sql",
          dag: { nodes: [], edges: [] },
          compiled_sql: {},
          raw_sql: "select 1",
          tests: [],
          is_recursive: false,
        },
      ],
      pr_comments: {
        total: 1,
        by_model: [
          {
            model: "orders",
            model_path: "models/orders.sql",
            count: 1,
            threads: [
              {
                path: "models/orders.sql", side: "Right", within_hunk: true,
                resolved: false, outdated: false, line: 1, comments: [{ author: "a", body: "hi" }],
              },
            ],
          },
        ],
      },
    };
    const built = buildContexts(ctx);
    expect(built).toHaveLength(1);
    expect(built[0]!.name).toBe("orders");
    expect(built[0]!.files).toHaveLength(1);
    expect(built[0]!.files[0]!.threads).toHaveLength(1);
    expect(built[0]!.files[0]!.lang).toBe("sql");
  });
});

describe("toFlow legend completeness", () => {
  it("lists exactly the distinct edge types present (no silent gap, no spurious entry)", () => {
    const flow = toFlow({
      nodes: [
        { id: "a", role: "import" },
        { id: "b", role: "transform" },
        { id: "c", role: "final" },
      ],
      edges: [
        { from: "a", to: "b", edge_type: "inner" },
        { from: "b", to: "c", edge_type: "union_all" },
      ],
    });
    expect(flow.legend).toEqual(["inner", "union_all"]);
    expect(flow.nodes).toHaveLength(3);
    expect(flow.edges[0]!.style!.stroke).toBeTruthy();
  });
});

describe("toPrFlow", () => {
  const view = {
    graph: {
      nodes: [
        { id: "a", name: "a", state: "new" as const, is_connector: false, lines_added: 5, lines_removed: 0 },
        { id: "b", name: "b", state: "modified" as const, is_connector: true, is_halo: true, lines_added: 1, lines_removed: 2 },
      ],
      edges: [{ from: "a", to: "b" }],
    },
    modified_count: 1, connector_count: 1, halo_count: 1, deleted_count: 0, collapsed: false,
  };

  it("reshapes the default PR view with counts + node data", () => {
    const pr: PrDagPayload = { ...view };
    const flow = toPrFlow(pr);
    expect(flow.counts.modified).toBe(1);
    expect(flow.nodes).toHaveLength(2);
    expect(flow.nodes[1]!.data.isHalo).toBe(true);
    expect(flow.edges).toHaveLength(1);
  });

  it("selects the axis view when an axis is given", () => {
    const axisView = { ...view, modified_count: 9 };
    const pr: PrDagPayload = { ...view, by_axis: { body: axisView, config: view, unit_test: view } };
    const flow = toPrFlow(pr, "body");
    expect(flow.counts.modified).toBe(9);
  });
});
