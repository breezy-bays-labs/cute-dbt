// The cursor⇄node sync state machine (S6a) — the #1 topology rabbit-hole, landed
// ALONE as a PURE, separately-tested module BEFORE any pane consumes it. This is
// the VERBATIM-port of the prototype topology.js cursor⇄node sync (the compiled +
// raw linked cursors), lifted out of the React component into a pure reducer so
// the resolution + anti-loop logic is exhaustively unit- and mutation-testable
// with NO DOM, NO zustand, NO React.
//
// LAYER: domain (pure). It depends ONLY on the spine span types (context-data.ts
// SourceSpan/SourcePos). It must NOT import view/chrome, the store, or any diff-
// viewer code (dependency-cruiser + eslint-plugin-boundaries enforce it). The
// future topology pane (a later slice) reads `store.getState()`, builds a
// `SyncState`+`SyncMaps`, calls the transitions here, and applies the result —
// the SAME shape as the dispatch.ts keyboard ladder / use-keydown.ts split.
//
// ── THE TWO HARD PROBLEMS THIS MODULE OWNS ───────────────────────────────────
//
//  (1) RESOLUTION (which span/node/zone a coordinate maps to), with the honesty
//      invariant — never resolve to a node/span that does not exist; return a
//      none-sentinel (`null`) instead of fabricating. Innermost-span-wins: when
//      coordinate ranges nest, the SMALLEST containing span is the answer (the
//      nested CTE inside a {% for %}, not the loop it sits in).
//
//  (2) THE ANTI-LOOP DISCIPLINE. The forward sync (DAG node → code cursor +
//      scroll) and the reverse sync (code cursor → DAG node) feed each other; a
//      naive wiring loops forever (forward scrolls → cursor moves → reverse
//      re-selects → forward re-scrolls → …). The prototype broke the loop with
//      THREE devices, all reproduced here as PURE-reducer analogs:
//
//        • functional-setState BAIL — a transition returns the SAME state OBJECT
//          REFERENCE when nothing observable changed (the prototype's
//          `setX((c) => c === next ? c : next)` — React bails on `===`, and so
//          does a consuming `useSyncExternalStore`/selector). Identity IS the
//          "no extra render" proof.
//        • a scroll NONCE bumped ONLY when the selected node CHANGES (tracked via
//          `lastScrolled`, the prototype's `lastScrolledRef`) — re-running the
//          forward sync with the same node is idempotent (no re-scroll).
//        • innermost-span-wins resolution + zone↔node MUTUAL EXCLUSION — a node
//          and a zone are never both selected; selecting one clears the other.

import type { SourceSpan } from "./context-data";

// ── span lookup tables (the inputs the machine resolves over) ─────────────────

/** A line span — the resolution-relevant projection of the spine's `SourceSpan`
 *  (the machine reads only `.start.line`/`.end.line`; col/byte never participate
 *  in line-cursor resolution). Re-exported as the spine type so a consumer can
 *  pass `code_map.node_spans` (Record<string, SourceSpan>) straight through. */
export type LineSpan = SourceSpan;

/** A raw {% for %} / incremental zone region — boundary lines select the ZONE,
 *  body lines select the templated `nodeId` (when present). */
export interface ZoneSpan {
  id: string;
  startLine: number;
  endLine: number;
  /** the templated DAG node the loop body generates; absent ⇒ body selects the zone. */
  nodeId?: string;
}

/** The span tables the machine resolves over (built by the consumer from the
 *  spine's `code_map`: `node_spans` → compiled, `raw_node_spans`+`raw_zones` →
 *  raw). All optional so a model with no raw side still flows through. */
export interface SyncMaps {
  /** compiled-pane node↔line spans (keyed by DAG node id). */
  nodeSpans: Record<string, LineSpan>;
  /** raw/diff-pane node↔line spans (keyed by DAG node id). */
  rawNodeSpans?: Record<string, LineSpan>;
  /** raw {% for %} zone regions. */
  zones?: ZoneSpan[];
}

/** Which code pane the cursor lives in — selects the resolution table + rules. */
export type CodeSide = "compiled" | "raw";

/** A reverse-resolution target — a node OR a zone (the two are mutually exclusive). */
export type RawTarget = { kind: "node"; id: string } | { kind: "zone"; id: string };

// ── the machine's state (node↔zone mutually exclusive; cursor + scroll nonce) ──

/**
 * The sync state — the selection + cursor + scroll spine the prototype spread
 * across ~6 useStates (`cteSel`, `zoneSel`, `compiledCursor`/`rawCursor`,
 * `compiledScroll`/`rawScroll`, and the `lastScrolledRef`), collapsed into ONE
 * immutable record so a transition is a pure (state, maps) → state reducer and
 * the anti-loop guard is a `===` identity check.
 *
 * INVARIANT (zone↔node mutual exclusion): at most one of `node`/`zone` is non-null.
 */
export interface SyncState {
  /** the selected DAG node id (null when a zone — or nothing — is selected). */
  node: string | null;
  /** the selected raw zone id (null when a node — or nothing — is selected). */
  zone: string | null;
  /** the 1-based line cursor in the active code pane (null = no cursor yet). */
  cursor: number | null;
  /** a monotonic scroll nonce — bumped ONLY by a forward sync to a NEW node, so a
   *  consumer effect keyed on it scrolls exactly once per genuine node change. */
  scrollNonce: number;
  /** the node id the last scroll nonce bump was FOR (the `lastScrolledRef` analog);
   *  the forward sync re-bumps only when `node !== lastScrolled`. */
  lastScrolled: string | null;
}

/** The pristine initial state — nothing selected, no cursor, scroll nonce 0. */
export function initialSyncState(): SyncState {
  return { node: null, zone: null, cursor: null, scrollNonce: 0, lastScrolled: null };
}

// ── totality guards — the resolution helpers must be PANIC-FREE ───────────────
//
// The public span types (SourceSpan/ZoneSpan) declare start/end/line non-optional,
// but the maps are built from a RUNTIME payload — a missing field, a non-numeric
// line, or an inverted range (`end < start`) would, unguarded, throw a TypeError or
// silently mis-resolve (a negative "length" winning the innermost tie). The two
// validators below make every resolver TOTAL: a malformed span/zone is SKIPPED
// (never throws, never a candidate), and well-formed input is unchanged.

/** A finite-number line predicate — rejects NaN/Infinity/non-number (the runtime
 *  payload could carry a string, null, or NaN where a line is typed). */
function isFiniteLine(n: unknown): n is number {
  // tracked: cute-dbt#517 — equivalent: the `typeof n === "number"`→`true`
  // ConditionalExpression mutant collapses this to `Number.isFinite(n)`, which is
  // BEHAVIORALLY IDENTICAL — `Number.isFinite` never coerces, so it already returns
  // false for every non-number (string/null/boolean). The `typeof` conjunct is a
  // readability witness for the `n is number` guard, not an observable branch. (The
  // `&&`→`||` LogicalOperator mutant on this line IS killed by the Infinity-bounded
  // span test — `typeof Infinity === "number"` short-circuits `||`.)
  // Stryker disable next-line ConditionalExpression
  return typeof n === "number" && Number.isFinite(n);
}

/** A line span is VALID iff both endpoints exist with finite line numbers and the
 *  range is not inverted (`end >= start`; a zero-length single-line span is valid). */
function validLineSpan(sp: LineSpan | undefined): sp is LineSpan {
  return (
    sp != null &&
    sp.start != null &&
    sp.end != null &&
    isFiniteLine(sp.start.line) &&
    isFiniteLine(sp.end.line) &&
    // tracked: cute-dbt#517 — equivalent: the `>= `→`true` ConditionalExpression
    // mutant accepts an INVERTED span (`end < start`), but every caller then applies
    // the INCLUSIVE `line >= start && line <= end` test, which is unsatisfiable when
    // `end < start` (no line is both ≥ a larger start and ≤ a smaller end). So an
    // accepted inverted span never matches → no observable difference. The clause is
    // a defensive totality guard (gemini #518) + a clarity witness, not a live branch.
    // Stryker disable next-line ConditionalExpression
    sp.end.line >= sp.start.line
  );
}

/** A zone is VALID iff its boundary lines are finite numbers and not inverted
 *  (`endLine >= startLine`; a zero-length single-line zone is valid). */
function validZone(z: ZoneSpan | undefined): z is ZoneSpan {
  return (
    z != null &&
    isFiniteLine(z.startLine) &&
    isFiniteLine(z.endLine) &&
    // tracked: cute-dbt#517 — equivalent: the `>= `→`true` ConditionalExpression
    // mutant accepts an INVERTED zone (`endLine < startLine`), but the loop then
    // applies the INCLUSIVE `line >= startLine && line <= endLine` test, which is
    // unsatisfiable when `endLine < startLine` → an accepted inverted zone never
    // matches → no observable difference. A defensive totality guard (gemini #518).
    // Stryker disable next-line ConditionalExpression
    z.endLine >= z.startLine
  );
}

// ── (1) resolution — innermost-span-wins, never-fabricate ─────────────────────

/**
 * innermostSpan — the SMALLEST span containing `line`, or null. The honesty +
 * nesting core: when several spans contain the line (a nested CTE inside its
 * parent), the one with the smallest `(end.line − start.line)` wins (the prototype
 * `if (len < bestLen)` rule). Endpoints are INCLUSIVE. A null/absent line, an
 * empty table, or a line in no span all return null — never a fabricated id. On
 * an exact length tie the FIRST-encountered key wins (deterministic; the prototype
 * relies on insertion order, which `for…in` over a plain object preserves for
 * string keys). The `< bestLen` (strict) comparison is what makes the tie
 * deterministic — a `<=` would let a later equal-length span override the first.
 */
export function innermostSpan(spans: Record<string, LineSpan>, line: number | null): string | null {
  // tracked: cute-dbt#517 — equivalent: with `line == null`, `null >= n` is
  // `false` in JS, so the loop matches nothing and the fn returns `null` even
  // without this early return. The guard is a clarity short-circuit, not a branch.
  // Stryker disable next-line ConditionalExpression
  if (line == null) return null;
  let best: string | null = null;
  let bestLen = Infinity;
  for (const id in spans) {
    // skip inherited prototype properties — a polluted `Object.prototype` (or a
    // non-null-proto map) must not inject phantom spans into the resolution.
    if (!Object.prototype.hasOwnProperty.call(spans, id)) continue;
    const sp = spans[id];
    // skip a malformed span (missing/non-numeric endpoint or an inverted range) so
    // the resolver is TOTAL — a negative-"length" inverted span never wins the tie.
    if (!validLineSpan(sp)) continue;
    if (line >= sp.start.line && line <= sp.end.line) {
      const len = sp.end.line - sp.start.line;
      if (len < bestLen) {
        best = id;
        bestLen = len;
      }
    }
  }
  return best;
}

/** The span table the forward sync resolves over, by SIDE: the compiled-pane
 *  `nodeSpans` on the compiled shelf, the raw-pane `rawNodeSpans` on the raw shelf.
 *  A raw-only DAG node (a `zone:N` collapsed {% for %} or the synthesized
 *  `(final select)`) has NO compiled span but DOES have a raw span — so a forward
 *  sync on the raw shelf must resolve against `rawNodeSpans`, not `nodeSpans`,
 *  or the scroll/flash is a silent no-op (cute-dbt#497 finding 1). */
function sideTable(maps: SyncMaps, side: CodeSide): Record<string, LineSpan> {
  return side === "raw" ? maps.rawNodeSpans ?? {} : maps.nodeSpans;
}

/** Forward lookup: the span for a node id in the active SIDE's table, or null
 *  (never fabricates). Compiled side ⇒ `nodeSpans`; raw side ⇒ `rawNodeSpans`. A
 *  null id, or an id absent from that side's table (e.g. an incremental-only node
 *  with no compiled span, OR — on the compiled side — a raw-only `zone:N`/
 *  `(final select)` node), is an honest null — the caller treats it as a no-op.
 *  `side` defaults to "compiled" so the historical compiled-only callers are
 *  source-compatible. */
export function spanForNode(maps: SyncMaps, id: string | null, side: CodeSide = "compiled"): LineSpan | null {
  // tracked: cute-dbt#517 — equivalent: a falsy `id` indexes the side table to
  // `undefined`, which the `?? null` already maps to `null`; dropping this guard
  // yields the identical result. A clarity short-circuit, not a behavioral branch.
  // Stryker disable next-line ConditionalExpression
  if (!id) return null;
  return sideTable(maps, side)[id] ?? null;
}

/** Compiled reverse resolution: a compiled line → its innermost DAG node (or null).
 *  `side` selects the table (compiled `nodeSpans` vs raw `rawNodeSpans`); it
 *  defaults to "compiled" so the historical compiled-only callers are unchanged. */
export function nodeForLine(maps: SyncMaps, line: number | null, side: CodeSide = "compiled"): string | null {
  return innermostSpan(sideTable(maps, side), line);
}

/**
 * Raw reverse resolution: a raw-code line → what it highlights, with zone↔node
 * mutual exclusion (the prototype `rawTargetForLine`):
 *   • a zone BOUNDARY line (== startLine or == endLine) selects the ZONE;
 *   • a line INSIDE the innermost containing zone's body selects its templated
 *     `nodeId` (or the ZONE itself when the loop is unnamed/has no nodeId);
 *   • otherwise the innermost raw NODE span containing the line;
 *   • a line in no zone and no node → null (never fabricates).
 * The innermost (smallest) zone wins on overlap.
 */
export function rawTargetForLine(maps: SyncMaps, line: number | null): RawTarget | null {
  // tracked: cute-dbt#517 — equivalent: with `line == null`, every `line >= …`
  // comparison below is `false`, so no zone/node matches and the fn returns `null`
  // even without this early return. A clarity short-circuit, not a branch.
  // Stryker disable next-line ConditionalExpression
  if (line == null) return null;
  // innermost containing zone (smallest span wins).
  let bestZone: ZoneSpan | null = null;
  let bestZoneSpan = Infinity;
  // tracked: cute-dbt#517 — equivalent: Stryker's array-fill sentinel `["Stryker
  // was here"]` carries no startLine/endLine, so `line >= undefined` is `false` →
  // the loop matches nothing, identical to the real empty-array fallback.
  // Stryker disable next-line ArrayDeclaration
  for (const z of maps.zones ?? []) {
    // skip a malformed zone (missing/non-numeric boundary or an inverted range) so
    // the resolver is TOTAL — it falls through to the raw node span instead.
    if (!validZone(z)) continue;
    if (line >= z.startLine && line <= z.endLine) {
      const span = z.endLine - z.startLine;
      if (span < bestZoneSpan) {
        bestZoneSpan = span;
        bestZone = z;
      }
    }
  }
  if (bestZone) {
    if (line === bestZone.startLine || line === bestZone.endLine) return { kind: "zone", id: bestZone.id };
    if (bestZone.nodeId) return { kind: "node", id: bestZone.nodeId };
    return { kind: "zone", id: bestZone.id };
  }
  // no zone → the innermost raw NODE span.
  const nodeId = innermostSpan(maps.rawNodeSpans ?? {}, line);
  return nodeId ? { kind: "node", id: nodeId } : null;
}

// ── (2) transitions — the anti-loop reducers (=== bail on a true no-op) ────────

/**
 * selectNode — forward DAG→state: select `id` (or clear with null), CLEARING any
 * zone (mutual exclusion). Returns the SAME reference when the node is already
 * selected AND no zone is set AND no scroll guard needs resetting — the
 * functional-setState bail (no render).
 *
 * CLEARING the node (`id === null`) also RESETS `lastScrolled` to null
 * (cute-dbt#496 finding 1): `lastScrolled` is the scroll-nonce anti-loop guard —
 * it freezes re-scrolls while the SAME node stays selected. If a deselect left it
 * pinned to the just-cleared node, a later RE-selection of that same node would be
 * (wrongly) treated as "already scrolled" and `syncForward` would NOT scroll back
 * to it. Resetting it on clear makes a re-selection count as fresh. Selecting a
 * real node leaves `lastScrolled` untouched (the forward sync owns the bump).
 */
export function selectNode(s: SyncState, id: string | null): SyncState {
  const nextLastScrolled = id === null ? null : s.lastScrolled;
  // true no-op → identity (nothing observable, including the scroll guard, changes)
  if (s.node === id && s.zone === null && s.lastScrolled === nextLastScrolled) return s;
  return { ...s, node: id, zone: null, lastScrolled: nextLastScrolled };
}

/**
 * selectZone — forward DAG→state: select a zone (or clear with null), CLEARING any
 * node (mutual exclusion). Same `===`-bail discipline as `selectNode`. Selecting a
 * zone forces the node to null, so it ALWAYS resets `lastScrolled` to null
 * (cute-dbt#496 finding 1) — a later re-selection of the previously-scrolled node
 * then counts as fresh and `syncForward` scrolls back to it.
 */
export function selectZone(s: SyncState, id: string | null): SyncState {
  // node→null on every zone selection ⇒ lastScrolled always resets.
  if (s.zone === id && s.node === null && s.lastScrolled === null) return s; // true no-op → identity
  return { ...s, zone: id, node: null, lastScrolled: null };
}

/**
 * inSpanCursor — the in-span cursor or null: the line if it lies within `sp`
 * (inclusive of both endpoints), else null (a null/absent cursor is never in span).
 * Extracted so the boundary contract is its own exhaustively-tested unit; the
 * caller (`syncForward`) snaps to the span start whenever this returns null.
 */
export function inSpanCursor(cursor: number | null, sp: LineSpan): number | null {
  // tracked: cute-dbt#517 — equivalent: with `cursor == null`, the comparison
  // `null >= start` is `false` in JS, so the ternary returns null even without this
  // guard. A clarity short-circuit (and the non-null assertion's witness), not a branch.
  // Stryker disable next-line ConditionalExpression
  if (cursor == null) return null;
  // skip a malformed span (missing/non-numeric endpoint) — an in-span check on a
  // span with no `.start`/`.end` would throw; this makes the helper TOTAL. The
  // inversion clause of validLineSpan also rejects an inverted span (no cursor can
  // be in-span when end < start), so the contract is "honest null on garbage".
  if (!validLineSpan(sp)) return null;
  // tracked: cute-dbt#517 — equivalent: `cursor >= start` → `> start` differs only
  // when `cursor === start`, where the caller's fallback snaps to `start` = the same
  // value. The `<= end` boundary is NOT suppressed — a real test (cursor past the
  // span end → null → snapped to start) kills its `< end` / `true` mutants.
  // Stryker disable next-line EqualityOperator
  return cursor >= sp.start.line && cursor <= sp.end.line ? cursor : null;
}

/**
 * forwardSnapTarget — the line `syncForward` snaps an out-of-span cursor to. The
 * obvious target is the selected node's span START, and that is what it returns
 * whenever the start reverse-resolves back to the SAME node (the common case).
 *
 * SHARED-START NESTING (cute-dbt#496 finding 2): when a NESTED (smaller) span shares
 * the selected node's start line — a wrapper/model span whose first line is also its
 * first CTE's start — `nodeForLine(start)` resolves to the INNER node (innermost-
 * span-wins). Snapping the cursor there would make the reverse sync flip the
 * selection to the inner node AND fire a second scroll, breaking the AC#3
 * fixed-point/zero-extra-render guarantee. So when the start is shadowed, scan the
 * span for the FIRST line that reverse-resolves to the selected node, keeping
 * forward→reverse a true fixed point. If NO line in the span resolves to the node
 * (it is wholly shadowed by nested spans — a degenerate/ambiguous shape), fall back
 * to the span start honestly rather than fabricate a position. Reverse resolution
 * (innermost-span-wins) is UNCHANGED — only forward's landing line is made
 * round-trip-stable.
 */
export function forwardSnapTarget(maps: SyncMaps, node: string, sp: LineSpan, side: CodeSide = "compiled"): number {
  // tracked: cute-dbt#517 — equivalent: this early return is a clarity/perf
  // short-circuit for the common case. The loop below starts at `sp.start.line`, so
  // when the start resolves to `node` the loop's FIRST iteration returns it too —
  // dropping this guard yields the identical result. Not an observable branch.
  // Stryker disable next-line ConditionalExpression
  if (nodeForLine(maps, sp.start.line, side) === node) return sp.start.line;
  for (let line = sp.start.line; line <= sp.end.line; line++) {
    if (nodeForLine(maps, line, side) === node) return line;
  }
  return sp.start.line; // wholly shadowed → honest fallback (genuine ambiguity)
}

/**
 * syncForward — the forward sync (DAG node → code cursor + scroll), the prototype's
 * guarded effect (topology.js §"forward sync: a CTE node was picked"). For the
 * CURRENTLY-selected node:
 *   • no node selected, or the node has no compiled span (incremental-only) → an
 *     honest NO-OP (same reference; never moves the cursor, never scrolls);
 *   • move the cursor INTO the node's span ONLY when it is outside it (the
 *     `c >= start && c <= end ? c : start` functional bail — an in-span cursor is
 *     preserved so a reverse-driven cursor isn't yanked to the span top); the snap
 *     target is the first line that reverse-resolves to the node (`forwardSnapTarget`),
 *     so a shared-start nested span can't flip the selection on the reverse half;
 *   • bump the scroll nonce ONLY when the node differs from `lastScrolled` (the
 *     `lastScrolledRef` guard) — re-running with the same node is idempotent.
 * The whole transition collapses to `===` when nothing changed (anti-loop).
 */
export function syncForward(s: SyncState, maps: SyncMaps, side: CodeSide = "compiled"): SyncState {
  // tracked: cute-dbt#517 — equivalent: with `s.node` null/empty,
  // `spanForNode(maps, null)` returns null and the next `if (!sp) return s` fires
  // → the same `s`. A clarity short-circuit, not an observable branch.
  // Stryker disable next-line ConditionalExpression
  if (!s.node) return s; // a zone (or nothing) is selected → forward sync is a no-op
  // resolve the span against the ACTIVE SIDE's table: compiled `nodeSpans` on the
  // compiled shelf, raw `rawNodeSpans` on the raw shelf — so a raw-only DAG node
  // (a `zone:N` collapsed {% for %} or the synthesized `(final select)`, both
  // ABSENT from `nodeSpans`) scrolls + ring-flashes the raw pane instead of being a
  // silent no-op (cute-dbt#497 finding 1).
  const sp = spanForNode(maps, s.node, side);
  if (!sp) return s; // no span on this side (incremental-only / raw-only mismatch) → honest no-op
  // The cursor stays put only when it is INSIDE the node's span; otherwise it snaps
  // to a span line that reverse-resolves to THIS node (the prototype's
  // `c >= start && c <= end ? c : start` bail, hardened against shared-start nesting).
  // `inSpanCursor` returns the in-span cursor or null, isolating the `<= end`
  // boundary mutant (killed by a real "cursor past end snaps back" test) from the
  // `>= start` equivalent it suppresses internally.
  const kept = inSpanCursor(s.cursor, sp);
  const nextCursor = kept ?? forwardSnapTarget(maps, s.node, sp, side);
  const fresh = s.lastScrolled !== s.node;
  if (nextCursor === s.cursor && !fresh) return s; // cursor unchanged + already scrolled → identity
  return {
    ...s,
    cursor: nextCursor,
    scrollNonce: fresh ? s.scrollNonce + 1 : s.scrollNonce,
    lastScrolled: fresh ? s.node : s.lastScrolled,
  };
}

/**
 * syncFromCursor — the reverse sync (code cursor → DAG node/zone), the prototype's
 * `onRawCursor` / compiled `moveLine` reverse path. Moves the cursor to `line` and
 * resolves the new selection by SIDE:
 *   • "compiled" → innermost node (`nodeForLine`); selecting a node clears the zone;
 *   • "raw" → `rawTargetForLine` (zone↔node mutual exclusion).
 * Reverse NEVER scrolls (the scroll nonce is untouched — only the forward sync
 * scrolls). When the cursor lands in a GAP (no target), the selection is left
 * UNTOUCHED (an honest no-op — a no-target line never clears a valid selection and
 * never fabricates one) AND the cursor does not move, so the whole transition is a
 * `===` no-op. When the resolved target equals the current selection AND the cursor
 * is unchanged, it is likewise a `===` no-op — the reverse half of the anti-loop.
 */
export function syncFromCursor(s: SyncState, maps: SyncMaps, line: number | null, side: CodeSide): SyncState {
  const target: RawTarget | null =
    side === "raw"
      ? rawTargetForLine(maps, line)
      : (() => {
          const id = nodeForLine(maps, line);
          return id ? ({ kind: "node", id } as RawTarget) : null;
        })();
  if (!target) return s; // a gap line → leave selection + cursor untouched (honest no-op)
  const nextNode = target.kind === "node" ? target.id : null;
  const nextZone = target.kind === "zone" ? target.id : null;
  if (s.cursor === line && s.node === nextNode && s.zone === nextZone) return s; // nothing changed → identity
  return { ...s, cursor: line, node: nextNode, zone: nextZone };
}
