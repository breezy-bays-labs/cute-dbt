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
  forwardSnapTarget,
  initialSyncState,
  selectNode,
  selectZone,
  syncForward,
  syncFromCursor,
  type SyncState,
  type SyncMaps,
  type LineSpan,
  type RawTarget,
  type ZoneSpan,
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
  // ── SIDE-awareness (cute-dbt#497 finding 1) ──────────────────────────────────
  it("resolves the RAW span on the raw side (a raw-only node has no compiled span)", () => {
    // `status_orders` is a RAW-ONLY node — present in rawNodeSpans, ABSENT from
    // nodeSpans (the analog of a `zone:N` collapse / synthesized `(final select)`).
    expect(spanForNode(MAPS, "status_orders", "raw")).toEqual(sp(11, 17));
    // on the compiled side it has no span → honest null (would be a silent no-op).
    expect(spanForNode(MAPS, "status_orders", "compiled")).toBeNull();
  });
  it("picks the SIDE's table for a node that exists in BOTH at different lines", () => {
    // `final` is compiled 22..30 but raw 20..28 — the side must pick the right one.
    expect(spanForNode(MAPS, "final", "compiled")).toEqual(sp(22, 30));
    expect(spanForNode(MAPS, "final", "raw")).toEqual(sp(20, 28));
  });
  it("defaults to the compiled side when no side is passed (source-compatible)", () => {
    expect(spanForNode(MAPS, "final")).toEqual(sp(22, 30));
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

  // ── SIDE-aware forward sync (cute-dbt#497 finding 1) ─────────────────────────
  // On the RAW shelf the DAG carries raw-only ids (`zone:N` collapses, the
  // synthesized `(final select)`) that are ABSENT from `nodeSpans`. Forward sync
  // MUST resolve those against `rawNodeSpans` or the scroll/ring-flash is a silent
  // no-op — the exact bug clicking a {% for %} collapse node on the raw shelf hit.
  it("RAW side: a raw-only node (absent from nodeSpans) scrolls to its RAW span", () => {
    // `status_orders` ∈ rawNodeSpans (11..17), ∉ nodeSpans — the zone:N analog.
    const s0 = selectNode(initialSyncState(), "status_orders");
    // compiled side (the OLD behavior): no compiled span → silent no-op (the bug).
    const compiled = syncForward(s0, MAPS, "compiled");
    expect(compiled).toBe(s0);
    expect(compiled.scrollNonce).toBe(0);
    // raw side (the FIX): resolves the raw span → cursor snaps + scroll nonce bumps.
    const raw = syncForward(s0, MAPS, "raw");
    expect(raw.cursor).toBe(11); // landed on the raw span start
    expect(raw.scrollNonce).toBe(s0.scrollNonce + 1); // a fresh node → scroll/flash
    expect(raw.lastScrolled).toBe("status_orders");
  });
  it("RAW side: a node in BOTH tables lands on its RAW span line, not the compiled one", () => {
    // `final` is compiled 22..30 but raw 20..28 — the raw forward sync uses the raw line.
    const s = syncForward(selectNode(initialSyncState(), "final"), MAPS, "raw");
    expect(s.cursor).toBe(20); // raw span start, NOT the compiled 22
    expect(s.scrollNonce).toBe(1);
  });
  it("defaults to the compiled side when no side is passed (source-compatible)", () => {
    const s = syncForward(selectNode(initialSyncState(), "final"), MAPS);
    expect(s.cursor).toBe(22); // compiled span start
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

  // ── shared-start nesting: forward→reverse→forward must STILL be a fixed point
  //    that keeps the SELECTED node (cute-dbt#496 finding 2). An outer span whose
  //    start line is also the start of a NESTED span (a wrapper/model span sharing
  //    its first CTE's start) used to make forward snap the cursor onto a line that
  //    reverse-resolved to the INNER node — flipping the selection away from the
  //    node the user picked AND firing a second scroll. The forward snap now targets
  //    a line that round-trips back to the selected node, so a single click on the
  //    OUTER node settles on the outer node with exactly ONE scroll. ──────────────
  it("a SHARED-START nested span does not flip the selection or double-scroll", () => {
    // outer 5..20 with a nested inner 5..8 that SHARES the start line 5.
    const nested: SyncMaps = {
      nodeSpans: { outer: sp(5, 20), inner: sp(5, 8) },
    };
    // FORWARD: user clicks the OUTER node on the DAG.
    const forward1 = syncForward(selectNode(initialSyncState(), "outer"), nested);
    expect(forward1.node).toBe("outer");
    // the snapped cursor must reverse-resolve back to "outer" (line 5 would resolve
    // to the smaller "inner" — innermost-span-wins — so forward snaps past it).
    expect(nodeForLine(nested, forward1.cursor)).toBe("outer");
    expect(forward1.scrollNonce).toBe(initialSyncState().scrollNonce + 1);

    // REVERSE on the snapped cursor: it resolves to "outer" (unchanged) → NO-OP (===).
    const reverse = syncFromCursor(forward1, nested, forward1.cursor, "compiled");
    expect(reverse).toBe(forward1); // selection NOT flipped to "inner"
    expect(reverse.node).toBe("outer");

    // FORWARD again → idempotent fixed point: still "outer", still ONE scroll.
    const forward2 = syncForward(reverse, nested);
    expect(forward2).toBe(reverse);
    expect(forward2.node).toBe("outer");
    expect(forward2.scrollNonce).toBe(initialSyncState().scrollNonce + 1);
  });

});

// ── re-selection after a deselect / zone-switch must RE-SCROLL (cute-dbt#496
//    finding 1). `lastScrolled` is the scroll-nonce anti-loop guard: it freezes
//    re-scrolls while the SAME node stays selected. But clearing the selection
//    (selectNode(null)) or replacing it with a zone (selectZone) must reset it, so
//    a later RE-selection of the same node counts as fresh and scrolls back to it. ─
describe("anti-loop: re-selecting a node after a deselect/zone-switch re-scrolls", () => {
  it("deselect → re-select the SAME node bumps the scroll nonce again", () => {
    // 1. select + forward → first scroll.
    const s0 = syncForward(selectNode(initialSyncState(), "stg"), MAPS);
    expect(s0.scrollNonce).toBe(initialSyncState().scrollNonce + 1);
    expect(s0.lastScrolled).toBe("stg");
    // 2. DESELECT — clearing the node must also clear lastScrolled.
    const cleared = selectNode(s0, null);
    expect(cleared.node).toBeNull();
    expect(cleared.lastScrolled).toBeNull();
    // 3. RE-SELECT the same node → forward must scroll AGAIN (genuine re-selection).
    const s1 = syncForward(selectNode(cleared, "stg"), MAPS);
    expect(s1.lastScrolled).toBe("stg");
    expect(s1.scrollNonce).toBe(s0.scrollNonce + 1); // ← re-scroll happened
  });

  it("zone-switch → re-select the SAME node bumps the scroll nonce again", () => {
    // 1. select + forward → first scroll.
    const s0 = syncForward(selectNode(initialSyncState(), "stg"), MAPS);
    // 2. pick a ZONE (mutual exclusion clears the node) → lastScrolled must reset.
    const zoned = selectZone(s0, "zone:loop");
    expect(zoned.node).toBeNull();
    expect(zoned.zone).toBe("zone:loop");
    expect(zoned.lastScrolled).toBeNull();
    // 3. RE-SELECT the node → forward scrolls again.
    const s1 = syncForward(selectNode(zoned, "stg"), MAPS);
    expect(s1.scrollNonce).toBe(s0.scrollNonce + 1);
  });

  it("re-selecting a DIFFERENT node after deselect still scrolls (lastScrolled cleared)", () => {
    const s0 = syncForward(selectNode(initialSyncState(), "stg"), MAPS);
    const cleared = selectNode(s0, null);
    const s1 = syncForward(selectNode(cleared, "final"), MAPS); // span 22..30
    expect(s1.node).toBe("final");
    expect(s1.scrollNonce).toBe(s0.scrollNonce + 1);
    expect(s1.lastScrolled).toBe("final");
  });

  it("selectNode(null) on an already-cleared state is still an identity no-op", () => {
    // the bail must survive the lastScrolled reset: clearing nothing changes nothing.
    const s0 = initialSyncState(); // node null, lastScrolled null
    expect(selectNode(s0, null)).toBe(s0); // ← identity preserved (no render)
  });

  it("selecting a node leaves a NON-null lastScrolled untouched (only clears reset it)", () => {
    // selecting a real node must NOT clobber lastScrolled — only node→null resets it.
    const s0 = syncForward(selectNode(initialSyncState(), "stg"), MAPS); // lastScrolled "stg"
    const s1 = selectNode(s0, "final"); // switch to another node directly
    expect(s1.lastScrolled).toBe("stg"); // carried forward (forward sync will re-scroll)
  });
});

// ── forwardSnapTarget — the round-trip-stable forward snap line (cute-dbt#496
//    finding 2). Reverse resolution stays innermost-span-wins; forward just picks
//    a landing line that resolves BACK to the selected node so a shared-start nest
//    can't flip the selection. These pin the resolver + the loop scan + the fallback. ─
describe("forwardSnapTarget — the round-trip-stable forward snap line", () => {
  it("returns the span START when the start resolves to this node (the common case)", () => {
    // stg 5..20, stg_inner 8..12: line 5 resolves to stg (inner starts at 8) → start.
    expect(forwardSnapTarget(MAPS, "stg", sp(5, 20))).toBe(5);
  });
  it("scans PAST a shared-start nested span to the first line that resolves to this node", () => {
    // outer 5..20, inner 5..8 SHARE the start: line 5 resolves to inner (smaller),
    // 6,7,8 also resolve to inner; the first line resolving to outer is 9.
    const nested: SyncMaps = { nodeSpans: { outer: sp(5, 20), inner: sp(5, 8) } };
    expect(forwardSnapTarget(nested, "outer", sp(5, 20))).toBe(9);
    // and the inner node itself snaps to its own start (5, which resolves to inner).
    expect(forwardSnapTarget(nested, "inner", sp(5, 8))).toBe(5);
  });
  it("the chosen line ALWAYS reverse-resolves to the selected node (round-trip invariant)", () => {
    const nested: SyncMaps = { nodeSpans: { outer: sp(5, 20), inner: sp(5, 8) } };
    const target = forwardSnapTarget(nested, "outer", sp(5, 20));
    expect(nodeForLine(nested, target)).toBe("outer");
  });
  it("falls back to the span START when the span is WHOLLY shadowed (genuine ambiguity)", () => {
    // outer 5..8 fully covered by an equal-or-smaller inner 5..8 that wins every line
    // (tie → first key 'inner' wins). No line in outer's span resolves to outer →
    // honest fallback to the start rather than fabricate a position.
    const shadowed: SyncMaps = { nodeSpans: { inner: sp(5, 8), outer: sp(5, 8) } };
    expect(forwardSnapTarget(shadowed, "outer", sp(5, 8))).toBe(5);
  });
  it("scans through to the END line when it is the ONLY resolving line (kills `<= end` → `< end`)", () => {
    // tail 5..8 with a nested head 5..7 winning lines 5,6,7 → line 8 (the END) is the
    // ONLY line resolving to tail. The real `line <= sp.end.line` reaches 8; a `<`
    // mutant stops at 7, never matches, and falls back to the start (5) instead.
    const tailNest: SyncMaps = { nodeSpans: { head: sp(5, 7), tail: sp(5, 8) } };
    expect(forwardSnapTarget(tailNest, "tail", sp(5, 8))).toBe(8);
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
  it("selectNode(null) RESETS a stale lastScrolled — the `lastScrolled === next` conjunct is load-bearing", () => {
    // node already null, zone null, but lastScrolled pins a previously-scrolled node.
    // Clearing must NOT bail: it must reset lastScrolled so a later re-select scrolls.
    // The `s.lastScrolled === nextLastScrolled` → `true` mutant would bail and leave
    // lastScrolled stale (re-selection then never re-scrolls — finding 1's bug).
    const stale: SyncState = { node: null, zone: null, cursor: 7, scrollNonce: 1, lastScrolled: "stg" };
    const out = selectNode(stale, null);
    expect(out).not.toBe(stale); // did NOT bail
    expect(out.lastScrolled).toBeNull(); // reset
  });
  it("selectZone RESETS a stale lastScrolled — the `lastScrolled === null` conjunct is load-bearing", () => {
    // zone already selected (node null) but lastScrolled is stale. Re-selecting the
    // SAME zone must reset lastScrolled (a later node re-select scrolls). The
    // `s.lastScrolled === null` → `true` mutant would bail and leave it stale.
    const stale: SyncState = { node: null, zone: "z", cursor: 0, scrollNonce: 1, lastScrolled: "stg" };
    const out = selectZone(stale, "z");
    expect(out).not.toBe(stale); // did NOT bail
    expect(out.lastScrolled).toBeNull(); // reset
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

// ── TOTALITY: the resolution helpers must be PANIC-FREE against malformed /
//    inverted / missing-field span objects (gemini-code-assist #518, cute-dbt#496).
//    The public types declare start/end/line non-optional, but the maps are built
//    from a runtime payload — a missing field, a non-numeric line, an inverted
//    range, or an inherited prototype property must SKIP (never throw, never
//    mis-resolve). Well-formed input is UNCHANGED (the totality tests above prove
//    the happy path; these only constrain the malformed path). Each malformed span
//    is cast through `unknown` because it deliberately violates the static type. ──

// A deliberately-malformed line span (any field may be missing or non-numeric).
type BadSpan = Partial<{ start: Partial<{ line: unknown }>; end: Partial<{ line: unknown }> }>;
function bad(sp: BadSpan): LineSpan {
  return sp as unknown as LineSpan;
}
// A deliberately-malformed zone (startLine/endLine may be missing or non-numeric).
function badZone(z: Record<string, unknown>): SyncMaps["zones"] extends (infer Z)[] | undefined ? Z : never {
  return z as never;
}

describe("totality: innermostSpan skips malformed / inverted / inherited-prop spans", () => {
  // A single WELL-FORMED span the malformed neighbour must not corrupt: line 5 in
  // only `good` must still resolve to `good` no matter what junk sits beside it.
  function withJunk(junk: Record<string, LineSpan>): Record<string, LineSpan> {
    return { good: sp(1, 10), ...junk };
  }
  it("skips a span with a MISSING start (no throw, resolves the good span)", () => {
    const spans = withJunk({ junk: bad({ end: { line: 10 } }) });
    expect(innermostSpan(spans, 5)).toBe("good");
  });
  it("skips a span with a MISSING end (no throw)", () => {
    const spans = withJunk({ junk: bad({ start: { line: 1 } }) });
    expect(innermostSpan(spans, 5)).toBe("good");
  });
  it("skips a span with a STRING-COERCIBLE start.line (kills isFiniteLine — coercion would otherwise match)", () => {
    // `{start:"1", end:10}` would, UNGUARDED, match line 5 via JS coercion
    // (`5 >= "1"` is true) and STEAL it from `good` (negative-ish length wins). The
    // isFiniteLine guard rejects the string start → `good` wins. Kills the `isFinite`
    // half + the `typeof` early-return mutants (a string IS resolution-relevant here).
    const spans = withJunk({ junk: bad({ start: { line: "1" }, end: { line: 10 } }) });
    expect(innermostSpan(spans, 5)).toBe("good");
  });
  it("skips a span with a STRING-COERCIBLE end.line (kills the end-side isFinite guard)", () => {
    const spans = withJunk({ junk: bad({ start: { line: 1 }, end: { line: "20" } }) });
    // {start:1, end:"20"} would coerce-match line 5 (len 19 > good's 9, so good wins
    // the TIE only because the string span is rejected; without the guard it could
    // still be a candidate). Resolve via good either way → the guard keeps it honest.
    expect(innermostSpan(spans, 5)).toBe("good");
  });
  it("skips an INFINITY-bounded span (kills isFiniteLine `&&`→`||` — Infinity is a number but not finite)", () => {
    // `{start:1, end:Infinity}` contains EVERY line. The `&&`→`||` mutant of
    // isFiniteLine accepts it (typeof Infinity === "number" short-circuits the `||`),
    // making it match line 5 and shadow `good`. The real `Number.isFinite` rejects it.
    const spans = withJunk({ junk: bad({ start: { line: 1 }, end: { line: Infinity } }) });
    expect(innermostSpan(spans, 5)).toBe("good"); // good(1..10) is the only VALID span containing 5
  });
  it("skips a NaN-lined span (NaN poisons comparisons, but the guard also drops it explicitly)", () => {
    const spans = withJunk({ junk: bad({ start: { line: NaN }, end: { line: NaN } }) });
    expect(innermostSpan(spans, 5)).toBe("good");
  });
  it("skips a span whose VALUE is null/undefined (kills the `sp != null`→`true` guard, no throw)", () => {
    // an explicitly-null span value: the `sp != null` conjunct is load-bearing — a
    // `true` mutant would dereference `sp.start` and THROW. The guard skips it.
    const spans: Record<string, LineSpan> = { good: sp(1, 10), nullish: null as unknown as LineSpan };
    expect(innermostSpan(spans, 5)).toBe("good");
  });
  it("skips an INVERTED span (end.line < start.line) — kills `end >= start` → drop", () => {
    // An inverted junk span [20..10] would, unguarded, "contain" line 15 by the
    // `>= start && <= end` test reading start=20,end=10 as FALSE — but a smaller
    // NEGATIVE length (10-20=-10 < the good span's 9) would let it WIN the tie if it
    // ever matched. The inversion guard makes it never a candidate. A junk span
    // [10..1] would otherwise CONTAIN line 5 (1<=5<=10 after swap reading) and steal
    // it from `good` with a negative length. The guard rejects it → `good` wins.
    const spans = withJunk({ junk: bad({ start: { line: 10 }, end: { line: 1 } }) });
    expect(innermostSpan(spans, 5)).toBe("good");
  });
  it("ignores an inherited prototype property on the spans object (kills the hasOwnProperty guard)", () => {
    // Pollute Object.prototype with an enumerable span-shaped key. A bare `for…in`
    // would iterate it; hasOwnProperty must skip it. Restored in finally.
    const proto = Object.prototype as unknown as Record<string, unknown>;
    proto.polluted = sp(1, 10);
    try {
      const spans: Record<string, LineSpan> = { good: sp(3, 4) };
      // line 5 is OUTSIDE good(3..4); only the polluted span (1..10) would contain
      // it. With the hasOwnProperty guard the inherited key is skipped → null.
      expect(innermostSpan(spans, 5)).toBeNull();
      // and a line inside `good` still resolves to good (own key wins, not polluted).
      expect(innermostSpan(spans, 3)).toBe("good");
    } finally {
      delete proto.polluted;
    }
  });
  it("returns null when EVERY span is malformed (honest none-sentinel, no throw)", () => {
    const allBad: Record<string, LineSpan> = {
      a: bad({}),
      b: bad({ start: { line: 1 } }),
      c: bad({ start: { line: 5 }, end: { line: 1 } }),
    };
    expect(innermostSpan(allBad, 3)).toBeNull();
  });
  it("a WELL-FORMED zero-length span (start === end) is STILL valid (not treated as inverted)", () => {
    // end === start is NOT inverted — a single-line span must still resolve. This
    // kills an over-strict `end > start` mutant of the inversion guard.
    expect(innermostSpan({ point: bad({ start: { line: 7 }, end: { line: 7 } }) }, 7)).toBe("point");
  });
});

describe("totality: rawTargetForLine skips malformed / inverted zones", () => {
  it("skips a zone with a MISSING startLine (resolves via the raw node span instead, no throw)", () => {
    const maps: SyncMaps = {
      nodeSpans: {},
      rawNodeSpans: { base: sp(1, 3) },
      zones: [badZone({ id: "z", endLine: 18, nodeId: "n" })],
    };
    // line 2 is in no valid zone → falls through to the raw node span `base`.
    expect(rawTargetForLine(maps, 2)).toEqual<RawTarget>({ kind: "node", id: "base" });
  });
  it("skips a zone with a MISSING endLine (no throw)", () => {
    const maps: SyncMaps = {
      nodeSpans: {},
      rawNodeSpans: { base: sp(1, 3) },
      zones: [badZone({ id: "z", startLine: 1, nodeId: "n" })],
    };
    expect(rawTargetForLine(maps, 2)).toEqual<RawTarget>({ kind: "node", id: "base" });
  });
  it("skips a zone with a STRING-COERCIBLE startLine (kills the `z != null ||`/precedence isFinite guards)", () => {
    // `{startLine:"1", endLine:18}` would, UNGUARDED, coerce-match line 13
    // (`13 >= "1"` is true) and return its node — STEALING resolution from the raw
    // node span. The isFiniteLine(startLine) guard rejects the string → falls through
    // to the raw node `inner`. This is the input where the guard is RESOLUTION-relevant
    // (kills the `z != null || …` short-circuit + the `&&`-precedence LogicalOperator).
    const maps: SyncMaps = {
      nodeSpans: {},
      rawNodeSpans: { inner: sp(10, 16) },
      zones: [badZone({ id: "z", startLine: "1", endLine: 18, nodeId: "zone_n" })],
    };
    expect(rawTargetForLine(maps, 13)).toEqual<RawTarget>({ kind: "node", id: "inner" });
  });
  it("skips a zone with a STRING-COERCIBLE endLine (kills the end-side isFinite guard)", () => {
    const maps: SyncMaps = {
      nodeSpans: {},
      rawNodeSpans: { inner: sp(10, 16) },
      zones: [badZone({ id: "z", startLine: 1, endLine: "18", nodeId: "zone_n" })],
    };
    expect(rawTargetForLine(maps, 13)).toEqual<RawTarget>({ kind: "node", id: "inner" });
  });
  it("skips a zone whose VALUE is null (kills the `z != null`→`true` guard, no throw)", () => {
    // an explicitly-null zone in the array: `z != null` is load-bearing — a `true`
    // mutant would dereference `z.startLine` and THROW. The guard skips it.
    const maps: SyncMaps = {
      nodeSpans: {},
      rawNodeSpans: { base: sp(1, 3) },
      zones: [null as unknown as ZoneSpan, { id: "z:good", startLine: 10, endLine: 18, nodeId: "n" }],
    };
    expect(rawTargetForLine(maps, 2)).toEqual<RawTarget>({ kind: "node", id: "base" });
    expect(rawTargetForLine(maps, 13)).toEqual<RawTarget>({ kind: "node", id: "n" }); // valid zone still resolves
  });
  it("skips an INVERTED zone (endLine < startLine) — kills `endLine >= startLine` → drop", () => {
    // An inverted zone [18..10] would otherwise be a candidate; the guard drops it,
    // so line 2 (outside everything except base) resolves to the raw node `base`.
    const maps: SyncMaps = {
      nodeSpans: {},
      rawNodeSpans: { base: sp(1, 3) },
      zones: [badZone({ id: "z", startLine: 18, endLine: 10, nodeId: "n" })],
    };
    expect(rawTargetForLine(maps, 2)).toEqual<RawTarget>({ kind: "node", id: "base" });
  });
  it("a malformed zone does not shadow a VALID zone (the valid one still resolves)", () => {
    const maps: SyncMaps = {
      nodeSpans: {},
      zones: [
        badZone({ id: "z:bad", startLine: NaN, endLine: NaN, nodeId: "bad_n" }),
        { id: "z:good", startLine: 10, endLine: 18, nodeId: "good_n" },
      ],
    };
    expect(rawTargetForLine(maps, 13)).toEqual<RawTarget>({ kind: "node", id: "good_n" });
  });
  it("a WELL-FORMED zero-length zone (startLine === endLine) is STILL valid", () => {
    // single-line zone — its sole line is a boundary → selects the zone. Kills an
    // over-strict `endLine > startLine` inversion mutant.
    const maps: SyncMaps = { nodeSpans: {}, zones: [{ id: "z", startLine: 7, endLine: 7, nodeId: "n" }] };
    expect(rawTargetForLine(maps, 7)).toEqual<RawTarget>({ kind: "zone", id: "z" });
  });
});

describe("totality: inSpanCursor returns null on a malformed span (no throw)", () => {
  it("returns null when the span is MISSING start (no throw)", () => {
    expect(inSpanCursor(5, bad({ end: { line: 10 } }))).toBeNull();
  });
  it("returns null when the span is MISSING end", () => {
    expect(inSpanCursor(5, bad({ start: { line: 1 } }))).toBeNull();
  });
  it("returns null when start.line is NON-NUMERIC (kills the typeof/isFinite guard)", () => {
    expect(inSpanCursor(5, bad({ start: { line: "1" }, end: { line: 10 } }))).toBeNull();
  });
  it("returns null when end.line is NON-NUMERIC (string '20' would coerce-match without the guard)", () => {
    // `5 <= "20"` coerces to true — the guard rejecting the string is load-bearing
    // (without it inSpanCursor would claim 5 is in [1.."20"]).
    expect(inSpanCursor(5, bad({ start: { line: 1 }, end: { line: "20" } }))).toBeNull();
  });
  it("returns null on an INFINITY end (kills isFiniteLine `&&`→`||`: Infinity is a number, not finite)", () => {
    // {start:1, end:Infinity} contains 5 (`5 <= Infinity`). The `||` mutant accepts it
    // and returns 5; the real guard returns null (Infinity is not a finite line).
    expect(inSpanCursor(5, bad({ start: { line: 1 }, end: { line: Infinity } }))).toBeNull();
  });
  it("returns null when start.line is NaN", () => {
    expect(inSpanCursor(5, bad({ start: { line: NaN }, end: { line: 10 } }))).toBeNull();
  });
  it("returns null when the span VALUE is null (kills `sp != null`→`true`, no throw)", () => {
    expect(inSpanCursor(5, null as unknown as LineSpan)).toBeNull();
  });
  it("a WELL-FORMED span still returns the in-span cursor (totality guard does not break the happy path)", () => {
    expect(inSpanCursor(7, sp(5, 20))).toBe(7);
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
