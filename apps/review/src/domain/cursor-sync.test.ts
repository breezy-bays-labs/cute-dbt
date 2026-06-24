// The cursor⇄node sync state-machine tests (S6a). The machine is a PURE reducer
// over (SyncState, SyncMaps) — DOM-free, store-free — so every transition + the
// anti-loop guarantee is asserted in isolation, exactly the dispatch.ts pattern.
//
// The ANTI-LOOP contract is the load-bearing thing here: a transition returns the
// SAME state OBJECT REFERENCE when nothing observable changed (the prototype's
// functional-setState bail, lifted to a pure reducer). `===` identity is the
// mechanical proof that no extra render/scroll fires — the test for AC#3 (a
// forward→reverse→forward roundtrip with zero extra renders) reduces to a chain
// of `toBe` identity asserts.

import { describe, it, expect } from "vitest";
import {
  innermostSpan,
  nodeForLine,
  rawTargetForLine,
  spanForNode,
  inSpanCursor,
  initialSyncState,
  selectNode,
  selectZone,
  syncForward,
  syncFromCursor,
  type SyncState,
  type SyncMaps,
  type LineSpan,
  type RawTarget,
} from "./cursor-sync";

// A span helper — the SourceSpan shape the spine emits (start/end line+col+byte),
// but the machine only reads `.start.line`/`.end.line`, so the test fixtures keep
// col/byte at 0 (they never participate in resolution).
function sp(startLine: number, endLine: number): LineSpan {
  return { start: { line: startLine, col: 0, byte: 0 }, end: { line: endLine, col: 0, byte: 0 } };
}

// A small representative model: three compiled CTE spans, two of which NEST (the
// innermost-span-wins case), plus the raw side with one {% for %} zone enclosing
// a templated node and a base node outside it.
//   compiled node spans:
//     base       lines 1..3
//     stg        lines 5..20     (outer)
//     stg_inner  lines 8..12     (nested inside stg — innermost wins on 8..12)
//     final      lines 22..30
//   raw zones:
//     zone:loop  lines 10..18, body node "status_orders", boundary lines 10 & 18
//   raw node spans:
//     base           lines 1..3
//     status_orders  lines 11..17  (inside the zone body)
//     final          lines 20..28
const MAPS: SyncMaps = {
  nodeSpans: {
    base: sp(1, 3),
    stg: sp(5, 20),
    stg_inner: sp(8, 12),
    final: sp(22, 30),
  },
  rawNodeSpans: {
    base: sp(1, 3),
    status_orders: sp(11, 17),
    final: sp(20, 28),
  },
  zones: [{ id: "zone:loop", startLine: 10, endLine: 18, nodeId: "status_orders" }],
};

describe("innermostSpan — smallest containing span wins", () => {
  it("picks the only span containing the line", () => {
    expect(innermostSpan(MAPS.nodeSpans, 2)).toBe("base");
  });
  it("picks the NESTED (smaller) span when two overlap", () => {
    // line 10 is in stg (5..20, len 15) AND stg_inner (8..12, len 4) → inner wins.
    expect(innermostSpan(MAPS.nodeSpans, 10)).toBe("stg_inner");
  });
  it("falls back to the outer span outside the nested region", () => {
    // line 18 is in stg (5..20) but NOT stg_inner (8..12) → outer.
    expect(innermostSpan(MAPS.nodeSpans, 18)).toBe("stg");
  });
  it("is inclusive on both span endpoints", () => {
    expect(innermostSpan(MAPS.nodeSpans, 5)).toBe("stg"); // start edge
    expect(innermostSpan(MAPS.nodeSpans, 30)).toBe("final"); // end edge
  });
  it("returns null for a line in no span (the gaps between CTEs)", () => {
    expect(innermostSpan(MAPS.nodeSpans, 4)).toBeNull(); // between base(1..3) and stg(5..)
    expect(innermostSpan(MAPS.nodeSpans, 21)).toBeNull(); // between stg(..20) and final(22..)
  });
  it("returns null for a null/absent line (honest no-op)", () => {
    expect(innermostSpan(MAPS.nodeSpans, null)).toBeNull();
  });
  it("returns null on an empty span table", () => {
    expect(innermostSpan({}, 5)).toBeNull();
  });
  it("on an exact tie (equal length) keeps the FIRST encountered (deterministic)", () => {
    // two spans of identical length covering the same line — first key wins.
    const tie = { a: sp(1, 5), b: sp(1, 5) };
    expect(innermostSpan(tie, 3)).toBe("a");
  });
});

describe("nodeForLine — compiled reverse resolution (innermost)", () => {
  it("resolves a compiled line to its innermost node", () => {
    expect(nodeForLine(MAPS, 10)).toBe("stg_inner");
  });
  it("returns null outside every span (never fabricates a node)", () => {
    expect(nodeForLine(MAPS, 4)).toBeNull();
  });
  it("returns null on a null line", () => {
    expect(nodeForLine(MAPS, null)).toBeNull();
  });
});

describe("spanForNode — forward lookup (never fabricates)", () => {
  it("returns the compiled span for a known node", () => {
    expect(spanForNode(MAPS, "base")).toEqual(sp(1, 3));
  });
  it("returns null for an unknown node id (honest none-sentinel)", () => {
    expect(spanForNode(MAPS, "ghost")).toBeNull();
  });
  it("returns null for a null id", () => {
    expect(spanForNode(MAPS, null)).toBeNull();
  });
});

describe("rawTargetForLine — raw reverse resolution (zone-vs-node mutual exclusion)", () => {
  it("a zone BOUNDARY line selects the ZONE", () => {
    expect(rawTargetForLine(MAPS, 10)).toEqual<RawTarget>({ kind: "zone", id: "zone:loop" });
    expect(rawTargetForLine(MAPS, 18)).toEqual<RawTarget>({ kind: "zone", id: "zone:loop" });
  });
  it("a line INSIDE the zone body selects the templated NODE", () => {
    expect(rawTargetForLine(MAPS, 13)).toEqual<RawTarget>({ kind: "node", id: "status_orders" });
  });
  it("a base-node line OUTSIDE any zone selects the base NODE", () => {
    expect(rawTargetForLine(MAPS, 2)).toEqual<RawTarget>({ kind: "node", id: "base" });
  });
  it("falls back to the ZONE when the body has no nodeId", () => {
    const maps: SyncMaps = { ...MAPS, zones: [{ id: "zone:bare", startLine: 10, endLine: 18 }] };
    expect(rawTargetForLine(maps, 13)).toEqual<RawTarget>({ kind: "zone", id: "zone:bare" });
  });
  it("the innermost (smallest) zone wins on overlap", () => {
    const maps: SyncMaps = {
      ...MAPS,
      zones: [
        { id: "zone:outer", startLine: 5, endLine: 30, nodeId: "outer_n" },
        { id: "zone:inner", startLine: 12, endLine: 16, nodeId: "inner_n" },
      ],
    };
    // line 14 is in both; inner (len 4) beats outer (len 25).
    expect(rawTargetForLine(maps, 14)).toEqual<RawTarget>({ kind: "node", id: "inner_n" });
  });
  it("returns null in a raw gap (no zone, no node) — never fabricates", () => {
    expect(rawTargetForLine(MAPS, 19)).toBeNull(); // after the zone, before final(20)
  });
  it("returns null on a null line", () => {
    expect(rawTargetForLine(MAPS, null)).toBeNull();
  });
});

describe("selectNode — forward DAG→state (clears the zone; mutual exclusion)", () => {
  it("selects a node and clears any prior zone", () => {
    const s0 = selectZone(initialSyncState(), "zone:loop");
    const s1 = selectNode(s0, "base");
    expect(s1.node).toBe("base");
    expect(s1.zone).toBeNull();
  });
  it("is a NO-OP (same reference) when the node is already selected and no zone is set", () => {
    const s0 = selectNode(initialSyncState(), "base");
    const s1 = selectNode(s0, "base");
    expect(s1).toBe(s0); // ← identity: no render
  });
  it("selecting null clears the node", () => {
    const s0 = selectNode(initialSyncState(), "base");
    const s1 = selectNode(s0, null);
    expect(s1.node).toBeNull();
  });
});

describe("selectZone — forward DAG→state (clears the node; mutual exclusion)", () => {
  it("selects a zone and clears any prior node", () => {
    const s0 = selectNode(initialSyncState(), "base");
    const s1 = selectZone(s0, "zone:loop");
    expect(s1.zone).toBe("zone:loop");
    expect(s1.node).toBeNull();
  });
  it("is a NO-OP (same reference) when the zone is already selected and no node is set", () => {
    const s0 = selectZone(initialSyncState(), "zone:loop");
    const s1 = selectZone(s0, "zone:loop");
    expect(s1).toBe(s0);
  });
});

describe("syncForward — DAG node → code cursor + scroll nonce (anti-loop)", () => {
  it("moves the cursor INTO the selected node's span and bumps the scroll nonce", () => {
    const s0 = selectNode(initialSyncState(), "stg"); // span 5..20
    const s1 = syncForward(s0, MAPS);
    expect(s1.cursor).toBe(5); // landed on the span start
    expect(s1.scrollNonce).toBe(s0.scrollNonce + 1); // a fresh node → scroll
    expect(s1.lastScrolled).toBe("stg");
  });
  it("KEEPS an in-span cursor (functional bail) but still bumps scroll on a NEW node", () => {
    // cursor already at line 7, inside stg (5..20): the cursor must NOT jump to 5.
    let s: SyncState = { ...selectNode(initialSyncState(), "stg"), cursor: 7 };
    s = syncForward(s, MAPS);
    expect(s.cursor).toBe(7); // in-span → unchanged (the bail)
    expect(s.lastScrolled).toBe("stg"); // but the node is newly selected → scrolled
  });
  it("does NOT re-bump the scroll nonce when re-run with the SAME node (anti-loop)", () => {
    const s0 = selectNode(initialSyncState(), "stg");
    const s1 = syncForward(s0, MAPS);
    const s2 = syncForward(s1, MAPS); // idempotent re-entry (the loop case)
    expect(s2).toBe(s1); // ← identity: zero extra renders/scrolls
    expect(s2.scrollNonce).toBe(s1.scrollNonce);
  });
  it("is an honest NO-OP (same reference) when the node has no compiled span", () => {
    // an incremental-only node: selected, but absent from nodeSpans → no scroll, no cursor move.
    const s0 = selectNode(initialSyncState(), "incremental_only");
    const s1 = syncForward(s0, MAPS);
    expect(s1).toBe(s0);
    expect(s1.cursor).toBeNull();
    expect(s1.scrollNonce).toBe(0);
  });
  it("is a NO-OP (same reference) when no node is selected (a zone is, or nothing)", () => {
    const s0 = selectZone(initialSyncState(), "zone:loop");
    const s1 = syncForward(s0, MAPS);
    expect(s1).toBe(s0);
  });
  it("re-scrolls when the selection CHANGES to a different node", () => {
    let s = syncForward(selectNode(initialSyncState(), "stg"), MAPS);
    const afterStg = s;
    s = syncForward(selectNode(s, "final"), MAPS); // span 22..30
    expect(s.cursor).toBe(22);
    expect(s.scrollNonce).toBe(afterStg.scrollNonce + 1);
    expect(s.lastScrolled).toBe("final");
  });
});

describe("syncFromCursor — code cursor → DAG node/zone (reverse, anti-loop)", () => {
  it("compiled reverse: an in-span cursor selects the innermost node", () => {
    const s0 = initialSyncState();
    const s1 = syncFromCursor(s0, MAPS, 10, "compiled");
    expect(s1.cursor).toBe(10);
    expect(s1.node).toBe("stg_inner");
    expect(s1.zone).toBeNull();
  });
  it("raw reverse: a zone-boundary cursor selects the zone (clears the node)", () => {
    const s0 = selectNode(initialSyncState(), "base");
    const s1 = syncFromCursor(s0, MAPS, 10, "raw");
    expect(s1.zone).toBe("zone:loop");
    expect(s1.node).toBeNull();
  });
  it("raw reverse: a zone-body cursor selects the templated node (clears the zone)", () => {
    const s0 = selectZone(initialSyncState(), "zone:loop");
    const s1 = syncFromCursor(s0, MAPS, 13, "raw");
    expect(s1.node).toBe("status_orders");
    expect(s1.zone).toBeNull();
  });
  it("moves the cursor even when the target node is UNCHANGED (cursor tracks, node bails)", () => {
    // lines 6 and 7 are both inside stg (5..20) and OUTSIDE stg_inner (8..12) →
    // both resolve to stg, so the node bails while the cursor still tracks.
    let s = syncFromCursor(initialSyncState(), MAPS, 6, "compiled"); // stg, cursor 6
    expect(s.node).toBe("stg");
    const before = s;
    s = syncFromCursor(s, MAPS, 7, "compiled"); // still stg, but a new cursor line
    expect(s).not.toBe(before); // cursor changed → a new state
    expect(s.cursor).toBe(7);
    expect(s.node).toBe("stg"); // node unchanged (the functional bail kept it === )
  });
  it("is a NO-OP (same reference) when the cursor lands in a gap (no target)", () => {
    const s0 = syncFromCursor(initialSyncState(), MAPS, 10, "compiled"); // selects stg_inner, cursor 10
    const s1 = syncFromCursor(s0, MAPS, 4, "compiled"); // line 4 is in no span
    // The cursor does NOT move and the selection is untouched: a no-target line is
    // an honest no-op (never clears a valid selection, never fabricates).
    expect(s1).toBe(s0);
  });
  it("is a NO-OP (same reference) when nothing changes (cursor + target identical)", () => {
    const s0 = syncFromCursor(initialSyncState(), MAPS, 10, "compiled");
    const s1 = syncFromCursor(s0, MAPS, 10, "compiled");
    expect(s1).toBe(s0);
  });
  it("does NOT bump the scroll nonce on a reverse sync (reverse never scrolls)", () => {
    const s0 = initialSyncState();
    const s1 = syncFromCursor(s0, MAPS, 10, "compiled");
    expect(s1.scrollNonce).toBe(s0.scrollNonce); // reverse moves selection, never scrolls
  });
});

// ── AC#3: the forward→reverse→forward roundtrip asserts ZERO extra renders. ────
// "render" here = a NEW state object. The anti-loop guard means once the machine
// settles, re-driving it in the opposite direction with a consistent cursor/node
// produces the SAME reference — the React `setState` bail, proven by identity.
describe("anti-loop: forward→reverse→forward settles with zero extra renders", () => {
  it("a consistent roundtrip reaches a fixed point (=== identity holds)", () => {
    // 1. FORWARD: select a node on the DAG → cursor scrolls into its span.
    const forward1 = syncForward(selectNode(initialSyncState(), "stg"), MAPS);
    expect(forward1.cursor).toBe(5);
    expect(forward1.node).toBe("stg");

    // 2. REVERSE: the cursor (now at line 5, inside stg) syncs back to the DAG.
    //    line 5 is the start of stg and in no nested span → resolves to "stg",
    //    which is ALREADY selected and the cursor is unchanged → NO-OP (===).
    const reverse = syncFromCursor(forward1, MAPS, forward1.cursor, "compiled");
    expect(reverse).toBe(forward1); // ← the loop is broken: identical state

    // 3. FORWARD again with the same selection → idempotent → still ===.
    const forward2 = syncForward(reverse, MAPS);
    expect(forward2).toBe(reverse); // ← zero extra renders across the whole roundtrip

    // The scroll nonce bumped EXACTLY once (the single intentional forward scroll).
    expect(forward2.scrollNonce).toBe(initialSyncState().scrollNonce + 1);
  });

});

// ── MUTATION-KILL block: each assertion below pins a specific Stryker mutant on
//    the resolution + anti-loop logic (cute-dbt#496, the strict >=90% kill gate).
//    These are not redundant with the behavioral tests above — they isolate the
//    exact arithmetic/boundary/conjunct each survivor would otherwise hide in. ──
describe("mutation-kill: innermostSpan length arithmetic (− not +)", () => {
  it("picks the span with the smaller (end − start), NOT the smaller (end + start)", () => {
    // Both spans contain line 9. By LENGTH: wide(9) vs narrow(4) → narrow wins.
    // The `+` mutant would compare SUMS: wide(11) vs narrow(20) → wide wins (wrong).
    // The two orderings DISAGREE, so this kills the `end.line - start.line` → `+` mutant.
    const spans = { wide: sp(1, 10), narrow: sp(8, 12) };
    expect(innermostSpan(spans, 9)).toBe("narrow");
  });
});

describe("mutation-kill: innermostSpan boundary inclusivity (>= and <=)", () => {
  const one = { only: sp(5, 10) };
  it("the START line is INCLUDED (kills >= → >)", () => {
    expect(innermostSpan(one, 5)).toBe("only");
  });
  it("the line just below start is EXCLUDED (kills >= → > would still match start)", () => {
    expect(innermostSpan(one, 4)).toBeNull();
  });
  it("the END line is INCLUDED (kills <= → <)", () => {
    expect(innermostSpan(one, 10)).toBe("only");
  });
  it("the line just above end is EXCLUDED (kills <= → < drops the end line)", () => {
    expect(innermostSpan(one, 11)).toBeNull();
  });
});

describe("mutation-kill: rawTargetForLine zone arithmetic + tie-break + boundary", () => {
  it("zone span length uses (end − start): the narrower zone wins, not the larger-sum one", () => {
    const maps: SyncMaps = {
      nodeSpans: {},
      zones: [
        { id: "zone:wide", startLine: 1, endLine: 10, nodeId: "wide_n" }, // len 9, sum 11
        { id: "zone:narrow", startLine: 8, endLine: 12, nodeId: "narrow_n" }, // len 4, sum 20
      ],
    };
    // line 9 ∈ both; by LENGTH narrow wins (kills `endLine - startLine` → `+`).
    expect(rawTargetForLine(maps, 9)).toEqual<RawTarget>({ kind: "node", id: "narrow_n" });
  });
  it("on an equal-length zone overlap the FIRST zone wins (kills `<` → `<=`)", () => {
    const maps: SyncMaps = {
      nodeSpans: {},
      zones: [
        { id: "zone:first", startLine: 10, endLine: 20, nodeId: "first_n" },
        { id: "zone:second", startLine: 10, endLine: 20, nodeId: "second_n" },
      ],
    };
    // identical spans → strict `<` keeps the first; a `<=` mutant would let the
    // second override. Body line 15 → the FIRST zone's node.
    expect(rawTargetForLine(maps, 15)).toEqual<RawTarget>({ kind: "node", id: "first_n" });
  });
  it("a zone END boundary selects the zone (kills the `=== endLine` disjunct)", () => {
    // distinct start/end so the start-edge test can't also satisfy the end-edge clause.
    const maps: SyncMaps = { nodeSpans: {}, zones: [{ id: "z", startLine: 4, endLine: 9, nodeId: "n" }] };
    expect(rawTargetForLine(maps, 9)).toEqual<RawTarget>({ kind: "zone", id: "z" });
    expect(rawTargetForLine(maps, 4)).toEqual<RawTarget>({ kind: "zone", id: "z" });
    expect(rawTargetForLine(maps, 6)).toEqual<RawTarget>({ kind: "node", id: "n" }); // interior → node
  });
  it("with NO zones array the raw node spans are still consulted (kills `?? []` → `[]` no-op only)", () => {
    const maps: SyncMaps = { nodeSpans: {}, rawNodeSpans: { base: sp(1, 3) } }; // zones omitted
    expect(rawTargetForLine(maps, 2)).toEqual<RawTarget>({ kind: "node", id: "base" });
  });
});

describe("inSpanCursor — the in-span cursor or null (boundary contract)", () => {
  const span = sp(5, 20);
  it("returns the cursor when inside the span", () => {
    expect(inSpanCursor(7, span)).toBe(7);
  });
  it("includes the START line (kills >= → > and the early-return → true)", () => {
    expect(inSpanCursor(5, span)).toBe(5);
  });
  it("includes the END line (kills <= → <)", () => {
    expect(inSpanCursor(20, span)).toBe(20);
  });
  it("returns null one line below the start", () => {
    expect(inSpanCursor(4, span)).toBeNull();
  });
  it("returns null one line above the end (kills <= → true)", () => {
    expect(inSpanCursor(21, span)).toBeNull();
  });
  it("returns null for a null cursor (kills the early-return → true would still null; → false equivalent)", () => {
    expect(inSpanCursor(null, span)).toBeNull();
  });
});

describe("mutation-kill: selectNode / selectZone each conjunct of the bail matters", () => {
  it("selectNode bails ONLY when the node matches AND zone is null (both conjuncts)", () => {
    // node matches but a zone IS set → must NOT bail (it must clear the zone).
    const withZone: SyncState = { ...initialSyncState(), node: "base", zone: "z" };
    const out = selectNode(withZone, "base");
    expect(out).not.toBe(withZone); // the `s.zone === null` conjunct is load-bearing
    expect(out.zone).toBeNull();
    // node differs, zone null → must NOT bail either (the `s.node === id` conjunct).
    const other: SyncState = { ...initialSyncState(), node: "base" };
    expect(selectNode(other, "stg")).not.toBe(other);
    expect(selectNode(other, "stg").node).toBe("stg");
  });
  it("selectZone bails ONLY when the zone matches AND node is null (both conjuncts)", () => {
    const withNode: SyncState = { ...initialSyncState(), zone: "z", node: "base" };
    const out = selectZone(withNode, "z");
    expect(out).not.toBe(withNode); // the `s.node === null` conjunct is load-bearing
    expect(out.node).toBeNull();
    const other: SyncState = { ...initialSyncState(), zone: "z" };
    expect(selectZone(other, "z2")).not.toBe(other);
  });
});

describe("mutation-kill: syncForward in-span boundary + fresh conjunct", () => {
  it("a cursor on the span END line counts as IN-span (kills `<= end` → `< end`)", () => {
    // stg span 5..20; place the cursor exactly on 20, the same node already scrolled.
    const s: SyncState = { node: "stg", zone: null, cursor: 20, scrollNonce: 1, lastScrolled: "stg" };
    const out = syncForward(s, MAPS);
    expect(out).toBe(s); // in-span (incl. end) + already scrolled → identity; a `<` mutant would yank cursor to 5
    expect(out.cursor).toBe(20);
  });
  it("a cursor on the span START line counts as IN-span (kills `>= start` → `> start`)", () => {
    const s: SyncState = { node: "stg", zone: null, cursor: 5, scrollNonce: 1, lastScrolled: "stg" };
    const out = syncForward(s, MAPS);
    expect(out).toBe(s); // start edge is in-span; a `> start` mutant would treat 5 as outside
  });
  it("a cursor one line BELOW the span start is OUTSIDE → moved to start", () => {
    const s: SyncState = { node: "stg", zone: null, cursor: 4, scrollNonce: 1, lastScrolled: "stg" };
    const out = syncForward(s, MAPS);
    expect(out.cursor).toBe(5); // 4 ∉ [5,20] → snapped to the span start
  });
  it("a cursor PAST the span end is OUTSIDE → snapped back to start (kills `<= end` → `true`)", () => {
    // stg span 5..20; cursor at 25 (beyond end), same already-scrolled node. The
    // real `<= end` makes inSpan FALSE → cursor snaps to 5. A `true`-mutant of the
    // end clause would treat 25 as in-span and PRESERVE 25 (wrong).
    const s: SyncState = { node: "stg", zone: null, cursor: 25, scrollNonce: 1, lastScrolled: "stg" };
    const out = syncForward(s, MAPS);
    expect(out.cursor).toBe(5);
  });
  it("the `nextCursor === cursor` AND `!fresh` BOTH gate the identity bail", () => {
    // same cursor but a FRESH node (lastScrolled differs) → must NOT bail (must scroll).
    const freshNode: SyncState = { node: "stg", zone: null, cursor: 7, scrollNonce: 3, lastScrolled: "other" };
    const out = syncForward(freshNode, MAPS);
    expect(out).not.toBe(freshNode); // `!fresh` conjunct: a fresh node forces a scroll bump
    expect(out.scrollNonce).toBe(4);
    expect(out.cursor).toBe(7); // in-span cursor preserved (the `nextCursor === cursor` half held)
  });
});

describe("mutation-kill: syncFromCursor each identity conjunct matters", () => {
  it("a SAME-target line with a DIFFERENT cursor is NOT a no-op (kills the `cursor === line` conjunct)", () => {
    const s0 = syncFromCursor(initialSyncState(), MAPS, 6, "compiled"); // stg, cursor 6
    const s1 = syncFromCursor(s0, MAPS, 7, "compiled"); // still stg, cursor 7
    expect(s1).not.toBe(s0);
    expect(s1.cursor).toBe(7);
  });
  it("a target whose NODE differs is NOT a no-op (kills the `s.node === nextNode` conjunct)", () => {
    // start at stg via line 6, then jump the cursor into final's span (line 25).
    const s0 = syncFromCursor(initialSyncState(), MAPS, 6, "compiled");
    const s1 = syncFromCursor(s0, MAPS, 25, "compiled");
    expect(s1).not.toBe(s0);
    expect(s1.node).toBe("final");
  });
  it("a target whose ZONE differs is NOT a no-op (kills the `s.zone === nextZone` conjunct)", () => {
    // raw: select the zone via a boundary line, then re-resolve to a node line.
    const s0 = syncFromCursor(initialSyncState(), MAPS, 10, "raw"); // zone:loop
    expect(s0.zone).toBe("zone:loop");
    const s1 = syncFromCursor(s0, MAPS, 2, "raw"); // base node, no zone
    expect(s1).not.toBe(s0);
    expect(s1.zone).toBeNull();
    expect(s1.node).toBe("base");
  });
  it("a STALE node at the same cursor line is corrected (kills the `s.node === nextNode` → true)", () => {
    // hand-build a state whose cursor already equals the line but whose node is
    // WRONG for it. The real `s.node === nextNode` conjunct is FALSE → the state
    // is rewritten to the correct node. A `true`-mutant would early-return the
    // stale node (a false claim — exactly what never-a-false-claim forbids).
    const stale: SyncState = { node: "wrong", zone: null, cursor: 10, scrollNonce: 0, lastScrolled: null };
    const out = syncFromCursor(stale, MAPS, 10, "compiled"); // line 10 → stg_inner
    expect(out).not.toBe(stale);
    expect(out.node).toBe("stg_inner");
  });
  it("a STALE zone at the same cursor line is corrected (kills the `s.zone === nextZone` → true)", () => {
    // cursor already at a raw zone-boundary line, but the state carries a WRONG zone.
    const stale: SyncState = { node: null, zone: "wrong:zone", cursor: 10, scrollNonce: 0, lastScrolled: null };
    const out = syncFromCursor(stale, MAPS, 10, "raw"); // line 10 → zone:loop boundary
    expect(out).not.toBe(stale);
    expect(out.zone).toBe("zone:loop");
  });
  it("the compiled branch resolves via nodeForLine, the raw branch via rawTargetForLine (kills the side switch)", () => {
    // line 10: compiled → stg_inner (a NODE); raw → zone:loop (a ZONE). The side
    // parameter MUST route to different resolvers — a flipped switch would mismatch.
    expect(syncFromCursor(initialSyncState(), MAPS, 10, "compiled").node).toBe("stg_inner");
    expect(syncFromCursor(initialSyncState(), MAPS, 10, "raw").zone).toBe("zone:loop");
  });
});

describe("anti-loop (continued): a moving cursor inside one node re-syncs without re-scrolling", () => {
  it("walks the cursor through a node without re-scrolling, then scrolls once on a genuine node change", () => {
    // forward into stg, then walk the cursor down within stg's span: each reverse
    // sync moves the cursor but the node stays stg AND the scroll nonce is frozen
    // (the forward scroll already happened; reverse never scrolls).
    let s = syncForward(selectNode(initialSyncState(), "stg"), MAPS); // cursor 5, scroll 1
    const scrollAtRest = s.scrollNonce;
    for (const line of [6, 7, 8 /* enters stg_inner */, 9, 10]) {
      s = syncFromCursor(s, MAPS, line, "compiled");
      expect(s.cursor).toBe(line);
      expect(s.scrollNonce).toBe(scrollAtRest); // reverse NEVER bumps scroll
    }
    // line 8..10 are inside stg_inner (nested) → the node flipped to the innermost.
    expect(s.node).toBe("stg_inner");
    // re-running forward now scrolls to the NEW node once (a genuine change), then settles.
    const f = syncForward(s, MAPS);
    expect(f.lastScrolled).toBe("stg_inner");
    expect(f.scrollNonce).toBe(scrollAtRest + 1);
    expect(syncForward(f, MAPS)).toBe(f); // settled again
  });
});
