// The zone-presence 3-state classifier (S6c) — the never-a-false-claim core of
// the zone-region/detail-shelf slice. A {% for %} zone is one of THREE honest
// presence states, NEVER a fabricated body:
//   • compiled_in   — the loop genuinely generated CTEs this build (fan-out
//                     collapse exists); the DAG node is real.
//   • compiled_out  — is_incremental() (or another build-state guard) STRIPPED
//                     the loop this build, so it generated nothing — the honest
//                     "incremental-only" explainer, never a fake structure.
//   • structural    — a wrapper loop that never emits a CTE of its own (its body
//                     is a region, not a generating template).
//
// This module is pure: it reads the §3a `presence` axis + the `node_map.raw`
// generation oracle and returns a CLASSIFICATION, never a span or a body.
import { describe, expect, it } from "vitest";
import {
  classifyZonePresence,
  zonePresenceTreatments,
  type ZonePresenceTreatment,
} from "./zone-presence";
import type { RawZone } from "../context-data";

const forLoop = (presence: RawZone["presence"], extra: Partial<RawZone> = {}): RawZone => ({
  kind: "for_loop",
  presence,
  start: { line: 8, col: 1, byte: 0 },
  end: { line: 12, col: 1, byte: 0 },
  ...extra,
});

describe("classifyZonePresence", () => {
  it("a compiled_in for_loop that generated CTEs → presence 'compiled_in', generated true", () => {
    const t = classifyZonePresence(forLoop("compiled_in", { template: "{{ status }}_orders" }), 0, {
      "zone:0": ["completed_orders", "pending_orders"],
    });
    expect(t.presence).toBe("compiled_in");
    expect(t.generated).toBe(true);
    expect(t.genCount).toBe(2);
    // the explainer copy is HONEST — it names the loop, never fabricates a body.
    expect(t.incrementalOnly).toBe(false);
  });

  it("a compiled_out for_loop that generated NOTHING → 'compiled_out', incrementalOnly true (is_incremental stripped it)", () => {
    const t = classifyZonePresence(forLoop("compiled_out"), 0, { "zone:0": [] });
    expect(t.presence).toBe("compiled_out");
    expect(t.generated).toBe(false);
    expect(t.genCount).toBe(0);
    // the honest 3-state: this loop EXISTS in raw but was stripped this build —
    // distinct from a TEMPLATE collapse (compiled_in). The explainer must say so.
    expect(t.incrementalOnly).toBe(true);
    expect(t.explainer).toMatch(/is_incremental|incremental/i);
    // never a fabricated structure — the treatment carries no generated nodes.
    expect(t.genIds).toEqual([]);
  });

  it("a structural wrapper for_loop (generates no CTE of its own) → 'structural', not incrementalOnly", () => {
    // zone:1 in order_status_pivot — the `for region` wrapper that emits no CTE
    // directly (its nested `for status` does).
    const t = classifyZonePresence(forLoop("structural"), 1, { "zone:1": [] });
    expect(t.presence).toBe("structural");
    expect(t.generated).toBe(false);
    // a structural wrapper is NOT incremental-only — it is honestly present, it
    // just templates a region, not a node. The compiled_out treatment must not
    // be borrowed (never-a-false-claim).
    expect(t.incrementalOnly).toBe(false);
    expect(t.explainer).not.toMatch(/is_incremental/i);
  });

  it("an absent presence defaults to 'structural' (honest — never claims compiled_in/out)", () => {
    const z = forLoop("structural");
    // a malformed wire shape with no presence must never silently become a
    // generated/compiled_in claim.
    const noPresence = { ...z, presence: undefined } as unknown as RawZone;
    const t = classifyZonePresence(noPresence, 0, {});
    expect(t.presence).toBe("structural");
    expect(t.incrementalOnly).toBe(false);
  });

  it("genCount reflects the node_map.raw fan-out array length exactly (never inflated)", () => {
    const t = classifyZonePresence(forLoop("compiled_in"), 2, {
      "zone:2": ["us_completed_summary", "us_pending_summary", "eu_completed_summary", "eu_pending_summary"],
    });
    expect(t.genCount).toBe(4);
    expect(t.genIds).toEqual([
      "us_completed_summary",
      "us_pending_summary",
      "eu_completed_summary",
      "eu_pending_summary",
    ]);
  });

  it("normalizes a stray scalar node_map.raw arm to a single-element array (never throws)", () => {
    const t = classifyZonePresence(forLoop("compiled_in"), 0, {
      "zone:0": "only_one" as unknown as string[],
    });
    expect(t.genCount).toBe(1);
    expect(t.genIds).toEqual(["only_one"]);
    expect(t.generated).toBe(true);
  });

  it("an incremental_guard zone is not a region — classifier still reports its presence honestly", () => {
    const guard: RawZone = { kind: "incremental_guard", presence: "compiled_out" };
    const t = classifyZonePresence(guard, 0, {});
    expect(t.kind).toBe("incremental_guard");
    expect(t.presence).toBe("compiled_out");
    // a compiled_out incremental_guard is also an incremental-only treatment.
    expect(t.incrementalOnly).toBe(true);
  });
});

describe("zonePresenceTreatments", () => {
  it("classifies every for_loop/guard zone in raw_zones order (parallel-index aligned)", () => {
    const zones: RawZone[] = [
      forLoop("compiled_in", { template: "{{ status }}_orders" }),
      forLoop("structural", { loop: "for region in regions" }),
      forLoop("compiled_in", { template: "{{ region }}_{{ status }}_summary" }),
    ];
    const nodeMapRaw: Record<string, string | string[]> = {
      "zone:0": ["completed_orders", "pending_orders"],
      "zone:1": [],
      "zone:2": ["us_completed_summary", "us_pending_summary", "eu_completed_summary", "eu_pending_summary"],
    };
    const out = zonePresenceTreatments(zones, nodeMapRaw);
    expect(out).toHaveLength(3);
    expect(out.map((t) => t.zi)).toEqual([0, 1, 2]);
    expect(out.map((t) => t.presence)).toEqual(["compiled_in", "structural", "compiled_in"]);
    expect(out[0]!.zoneId).toBe("z0");
    expect(out[2]!.genCount).toBe(4);
  });

  it("undefined / empty zones → empty list (honest-empty, never fabricated)", () => {
    expect(zonePresenceTreatments(undefined, undefined)).toEqual([]);
    expect(zonePresenceTreatments([], {})).toEqual([]);
  });

  it("a compiled_out for_loop surfaces as the incremental-only treatment in the list", () => {
    const out = zonePresenceTreatments([forLoop("compiled_out")], { "zone:0": [] });
    expect(out).toHaveLength(1);
    const t: ZonePresenceTreatment = out[0]!;
    expect(t.incrementalOnly).toBe(true);
    expect(t.presence).toBe("compiled_out");
  });
});
