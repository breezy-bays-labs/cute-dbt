// The keyboard action registry — the SSOT — under exhaustive unit test. The
// registry is the single source of truth every downstream interaction derives
// from, so coverage must be high. We table-test over the realistic entity×view×
// codeMode cube (the same AVAIL shape the app exposes), assert when-predicate
// context-scoping, conflict detection (same key in two ACTIVE contexts =
// conflict; cross-screen reuse allowed), the canonicalizer alias + DEAD-shadow,
// and that the Council MUST-FIX D flow actions are present + keyed without
// conflict.
import { describe, it, expect } from "vitest";
import {
  KEY_GROUPS,
  ALL_ACTIONS,
  CATEGORIES,
  FIXED_KEYS,
  MOTION_HINTS,
  defaultKeymap,
  mergeKeymap,
  activeActionIds,
  findConflicts,
  makeCanonicalizer,
  captureKey,
  displayKey,
  footerHints,
  actionMeta,
  actionLabel,
  parseToken,
  layerBindings,
  canonicalKey,
  DENY_REBIND_KEYS,
  RESERVED_KEYS,
  DEAD,
  isTopoShelf,
  isCodeDiff,
  hasDiffPane,
  inThreads,
  isReviewable,
  hasUnreviewedTarget,
  canResolveThread,
  type KbContext,
  type Entity,
} from "./keymap";

// ── the realistic entity → view list (the app's AVAIL shape) ─────────────────
// viewCount is derived from this so the positional ⇧1..⇧4 view actions light up
// exactly as the app exposes them.
const AVAIL: Record<Entity, string[]> = {
  pr: ["overview", "lineage", "files", "timeline"],
  models: ["topology", "code", "data"],
  macros: ["review"],
  seeds: ["review"],
  else: ["review"],
};

/**
 * Build a KbContext for every (entity, view, codeMode?) the app can be in,
 * EXHAUSTIVELY enumerated over the flow-signal boolean axes the flow actions
 * gate on. The earlier cube fixed those booleans at undefined, so it never
 * visited the cells where the flow actions (mark-reviewed-advance,
 * next/prev-unreviewed, resolve) become active — the very cells where a key
 * conflict on a flow action would show up. Enumerating
 * `inReview` × `hasOpenThread` × `hasUnreviewed` (the three flow-signal booleans
 * the when-predicates read) makes the cube genuinely cover every active-context
 * cell, so the conflict-free guarantee (findConflicts is empty for every cell,
 * AND no two active actions share a CANONICAL key) is sound, not vacuously true.
 * `inReview` is load-bearing: `isReviewable` now gates on it, so without the
 * `inReview:true` cells the reviewable flow actions would never light up and the
 * canonical-key guarantee would never exercise the `n`/`N` (shift-layer)
 * distinction.
 */
function buildCube(): KbContext[] {
  const out: KbContext[] = [];
  const BOOLS = [undefined, false, true] as const;
  (Object.keys(AVAIL) as Entity[]).forEach((entity) => {
    const views = AVAIL[entity];
    views.forEach((view) => {
      const viewCount = views.length;
      // code surfaces additionally vary codeMode (diff | file)
      const codeModes = view === "code" ? (["diff", "file"] as const) : ([undefined] as const);
      codeModes.forEach((codeMode) => {
        // flow-signal axes: inReview (gates isReviewable → mark-reviewed-advance +
        // next/prev-unreviewed), hasOpenThread (canResolveThread handler signal),
        // hasUnreviewed (next/prev-unreviewed's hasUnreviewedTarget). undefined |
        // false | true each visited so the active set spans the full flow space.
        BOOLS.forEach((inReview) => {
          BOOLS.forEach((hasOpenThread) => {
            BOOLS.forEach((hasUnreviewed) => {
              out.push({ entity, view, codeMode, viewCount, inReview, hasOpenThread, hasUnreviewed });
            });
          });
        });
      });
    });
  });
  return out;
}

const CUBE = buildCube();

describe("registry shape", () => {
  it("has exactly 6 groups with the canonical ids", () => {
    expect(KEY_GROUPS.map((g) => g.id)).toEqual(["app", "goto", "view", "move", "code", "review"]);
  });

  it("every action belongs to exactly one group (unique ids)", () => {
    const ids = ALL_ACTIONS.map((a) => a.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("every group carries a distinct color and CATEGORIES mirrors it", () => {
    expect(CATEGORIES.map((c) => c.id)).toEqual(KEY_GROUPS.map((g) => g.id));
    KEY_GROUPS.forEach((g, i) => {
      expect(CATEGORIES[i]?.color).toBe(g.color);
      expect(g.color).toMatch(/^var\(--legend-\d\)$/);
    });
  });

  it("actionMeta + actionLabel resolve every action; unknown ids degrade", () => {
    ALL_ACTIONS.forEach((a) => {
      const meta = actionMeta(a.id);
      expect(meta).not.toBeNull();
      expect(meta?.def).toBe(a.def);
      expect(actionLabel(a.id)).toBe(a.label);
    });
    expect(actionMeta("nope")).toBeNull();
    expect(actionLabel("nope")).toBe("nope");
  });

  it("FIXED_KEYS + MOTION_HINTS are non-empty registry data", () => {
    expect(FIXED_KEYS.length).toBeGreaterThan(0);
    expect(MOTION_HINTS.length).toBeGreaterThan(0);
  });
});

describe("when-predicate context-scoping", () => {
  // isTopoShelf: only models·topology or pr·lineage
  it("isTopoShelf is true ONLY on models·topology and pr·lineage", () => {
    CUBE.forEach((c) => {
      const expected =
        (c.entity === "models" && c.view === "topology") ||
        (c.entity === "pr" && c.view === "lineage");
      expect(isTopoShelf(c)).toBe(expected);
    });
  });

  it("isCodeDiff is true ONLY on models·code·diff", () => {
    CUBE.forEach((c) => {
      const expected = c.entity === "models" && c.view === "code" && c.codeMode === "diff";
      expect(isCodeDiff(c)).toBe(expected);
    });
  });

  it("hasDiffPane covers topo-shelf, any code view, and models/seeds data", () => {
    CUBE.forEach((c) => {
      const expected =
        isTopoShelf(c) ||
        c.view === "code" ||
        (c.view === "data" && (c.entity === "models" || c.entity === "seeds"));
      expect(hasDiffPane(c)).toBe(expected);
    });
  });

  it("inThreads is topo-shelf OR code-diff", () => {
    CUBE.forEach((c) => {
      expect(inThreads(c)).toBe(isTopoShelf(c) || isCodeDiff(c));
    });
  });

  it("isReviewable requires inReview AND the right surface family", () => {
    CUBE.forEach((c) => {
      const surfaceOk = c.entity !== "pr" || c.view === "files";
      expect(isReviewable(c)).toBe(surfaceOk && c.inReview === true);
    });
  });

  it("isReviewable is false on the right surface when the review flow is not mounted", () => {
    // surface family is reviewable, but inReview unset/false → inactive (honest degrade).
    expect(isReviewable({ entity: "models", view: "topology", viewCount: 3 })).toBe(false);
    expect(isReviewable({ entity: "models", view: "topology", viewCount: 3, inReview: false })).toBe(false);
    expect(isReviewable({ entity: "models", view: "topology", viewCount: 3, inReview: true })).toBe(true);
  });
});

describe("activeActionIds over the entity×view×codeMode cube", () => {
  // always-on actions (no `when`) must appear in EVERY context.
  const alwaysOnIds = ALL_ACTIONS.filter((a) => !a.when).map((a) => a.id);

  it("always-on actions are active in every context", () => {
    CUBE.forEach((c) => {
      const active = activeActionIds(c);
      alwaysOnIds.forEach((id) => expect(active.has(id)).toBe(true));
    });
  });

  it("each action is active in a context iff its when-predicate holds", () => {
    CUBE.forEach((c) => {
      const active = activeActionIds(c);
      ALL_ACTIONS.forEach((a) => {
        const expected = !a.when || a.when(c);
        expect(active.has(a.id)).toBe(expected);
      });
    });
  });

  // spot-check specific cube cells for the load-bearing surfaces.
  it("models·topology lights the shelf actions", () => {
    const active = activeActionIds({ entity: "models", view: "topology", viewCount: 3 });
    ["compiled", "comments-only", "next-hunk", "next-thread", "diff", "file", "panel"].forEach((id) =>
      expect(active.has(id)).toBe(true),
    );
  });

  it("pr·overview does NOT light diff/thread actions", () => {
    const active = activeActionIds({ entity: "pr", view: "overview", viewCount: 4 });
    ["compiled", "next-hunk", "next-thread", "diff", "comments-only"].forEach((id) =>
      expect(active.has(id)).toBe(false),
    );
  });

  it("positional view-N actions light by viewCount", () => {
    const one = activeActionIds({ entity: "macros", view: "review", viewCount: 1 });
    expect(one.has("view-1")).toBe(true);
    expect(one.has("view-2")).toBe(false);
    const four = activeActionIds({ entity: "pr", view: "overview", viewCount: 4 });
    ["view-1", "view-2", "view-3", "view-4"].forEach((id) => expect(four.has(id)).toBe(true));
  });
});

describe("findConflicts — same key in two ACTIVE contexts; cross-screen reuse allowed", () => {
  it("the default keymap is globally conflict-free in cross-context (no activeSet)", () => {
    // After the dedup there is NO global key share at all: every action has a
    // distinct (layer, base). The earlier draft kept two actions on ⇧R (resolve
    // + resolve-from-keyboard); collapsing to one removes the only global share,
    // so findConflicts with no activeSet is empty.
    const conflicts = findConflicts(defaultKeymap());
    expect(conflicts).toEqual([]);
    // and (defensively) any conflict that ever WERE reported must be a
    // context-exclusive pair — vacuously satisfied here.
    conflicts.forEach((c) => {
      const ids = c.actions.map((a) => a.id);
      const coActive = CUBE.some((ctx) => {
        const active = activeActionIds(ctx);
        return ids.every((id) => active.has(id));
      });
      expect(coActive).toBe(false);
    });
  });

  it("no ACTIVE-context conflict exists in ANY cube cell (the SSOT guarantee)", () => {
    // The mechanical guarantee: two distinct actions never share a key in the
    // same active context. Asserted over the FULL exhaustive cube — every
    // (entity, view, codeMode) × (hasOpenThread × hasUnreviewed) cell, so it
    // visits the cells where each flow action becomes active (the cells the
    // earlier cube omitted, which hid the resolve/resolve-from-keyboard ⇧R
    // conflict). It now genuinely holds because the dedup left exactly one
    // action on ⇧R.
    CUBE.forEach((ctx) => {
      const active = activeActionIds(ctx);
      const conflicts = findConflicts(defaultKeymap(), active);
      expect(conflicts).toEqual([]);
    });
  });

  it("a rebind that collides within an active context IS reported", () => {
    // rebind palette ("/") onto "d" — on models·topology both palette + diff are
    // active and now share "d" → a real conflict.
    const km = { ...defaultKeymap(), palette: "d" };
    const active = activeActionIds({ entity: "models", view: "topology", viewCount: 3 });
    const conflicts = findConflicts(km, active);
    const ids = conflicts.flatMap((c) => c.actions.map((a) => a.id));
    expect(ids).toContain("palette");
    expect(ids).toContain("diff");
  });

  it("cross-screen reuse is NOT a conflict (same key, never co-active)", () => {
    // bind help ("?") onto "g" (compiled). On pr·overview compiled is inactive,
    // so in that context there is no conflict even though they share "g" globally.
    const km = { ...defaultKeymap(), help: "g" };
    const prActive = activeActionIds({ entity: "pr", view: "overview", viewCount: 4 });
    const prConflicts = findConflicts(km, prActive);
    expect(prConflicts.flatMap((c) => c.actions.map((a) => a.id))).not.toContain("compiled");
  });

  // ── canonical-key conflict identity (the SSOT soundness fix) ────────────────
  // findConflicts must compare CANONICAL keys (layer|base, shift-aware), NOT raw
  // tokens. `n` and the shifted `N` are DISTINCT canonical keys; two tokens that
  // fold to the SAME canonical key ARE a conflict even though the raw tokens
  // (e.g. "d" vs the alias of a rebind) differ.
  it("a bare letter and its shift-layer twin are DISTINCT canonical keys (no false conflict)", () => {
    // inst-next ("n") and next-unreviewed ("N") are co-active on a reviewable
    // topo shelf with an unreviewed target — but they sit on different layers
    // (base vs shift), so they are NOT a conflict.
    expect(canonicalKey("n")).not.toBe(canonicalKey("N"));
    expect(canonicalKey("n")).toBe("|n");
    expect(canonicalKey("N")).toBe("shift|n");
    const active = activeActionIds({
      entity: "models",
      view: "topology",
      viewCount: 3,
      inReview: true,
      hasUnreviewed: true,
    });
    expect(active.has("inst-next")).toBe(true);
    expect(active.has("next-unreviewed")).toBe(true);
    const conflicts = findConflicts(defaultKeymap(), active);
    const ids = conflicts.flatMap((c) => c.actions.map((a) => a.id));
    expect(ids).not.toContain("inst-next");
    expect(ids).not.toContain("next-unreviewed");
  });

  it("two bindings that resolve to the SAME canonical key in one active context ARE reported", () => {
    // rebind palette onto "D" (Shift+d) and resolve onto "D" too — both parse to
    // canonical "shift|d". Make them co-active and assert the conflict surfaces.
    // (We use the cross-context-safe global form: bind two ALWAYS-on actions.)
    const km = { ...defaultKeymap(), palette: "D", help: "D" };
    // palette + help are always-on (no `when`) → co-active everywhere.
    const active = activeActionIds({ entity: "models", view: "topology", viewCount: 3 });
    const conflicts = findConflicts(km, active);
    const ids = conflicts.flatMap((c) => c.actions.map((a) => a.id));
    expect(ids).toContain("palette");
    expect(ids).toContain("help");
    // and they collide on the shift|d canonical key specifically.
    expect(conflicts.some((c) => c.layer === "shift" && c.base === "d")).toBe(true);
  });

  it("no two distinct ACTIVE actions share a CANONICAL key in ANY cube cell (canonicalizer-sound)", () => {
    // The teeth of the soundness guarantee: for every active cube cell, group the
    // active actions' default tokens by canonical key and assert no group has 2+.
    // This is what findConflicts asserts via layerBindings — restated directly in
    // canonical-key terms so the n/N (shift-layer) distinction is exercised in
    // exactly the cells where both are active (inReview && hasUnreviewed).
    CUBE.forEach((ctx) => {
      const active = activeActionIds(ctx);
      const byCanon = new Map<string, string[]>();
      ALL_ACTIONS.forEach((a) => {
        if (!active.has(a.id)) return;
        const ck = canonicalKey(a.def);
        const arr = byCanon.get(ck) ?? [];
        arr.push(a.id);
        byCanon.set(ck, arr);
      });
      byCanon.forEach((ids, ck) => {
        expect(ids.length, `canonical key ${ck} shared by ${ids.join(", ")} in ${JSON.stringify(ctx)}`).toBe(1);
      });
    });
  });

  it("the canonicalizer agrees with findConflicts on key identity (n/N exercised live)", () => {
    // The two notions of canonical identity must be the same one: makeCanonicalizer
    // resolves co-active shift/base twins to DISTINCT canonical tokens, matching
    // findConflicts' layer split. Pick the cell where both inst-next (n) and
    // next-unreviewed (N) are active and assert the canonicalizer separates them.
    const canon = makeCanonicalizer({});
    expect(canon("n")).toBe("n");
    expect(canon("N")).toBe("N");
    // findConflicts sees no conflict between them in that very cell:
    const active = activeActionIds({
      entity: "models",
      view: "topology",
      viewCount: 3,
      inReview: true,
      hasUnreviewed: true,
    });
    expect(findConflicts(defaultKeymap(), active)).toEqual([]);
  });
});

describe("Council MUST-FIX D — flow actions registered first-class without conflict", () => {
  // The council-D flow set. `resolve` (the prototype's pre-existing R/inThreads
  // action) IS the keyboard-resolve flow verb — there is NO separate
  // `resolve-from-keyboard` (it was a redundant subset-`when` twin on ⇧R and was
  // deduped). So the set is: mark-reviewed-advance, next/prev-unreviewed,
  // resolve, command-mode.
  const FLOW_IDS = [
    "mark-reviewed-advance",
    "next-unreviewed",
    "prev-unreviewed",
    "resolve",
    "command-mode",
  ];

  it("all five flow actions exist in the registry with labels + keys", () => {
    FLOW_IDS.forEach((id) => {
      const a = ALL_ACTIONS.find((x) => x.id === id);
      expect(a, `flow action ${id} must be registered`).toBeDefined();
      expect(a?.def).toBeTruthy();
      expect(a?.label).toBeTruthy();
    });
  });

  it("there is exactly ONE keyboard-resolve action and it is `resolve` on ⇧R", () => {
    // the dedup invariant: no `resolve-from-keyboard`, and `resolve` is the
    // single ⇧R keyboard-resolve verb (council-D's keyboard-resolve flow).
    expect(ALL_ACTIONS.find((x) => x.id === "resolve-from-keyboard")).toBeUndefined();
    const resolveActions = ALL_ACTIONS.filter((a) => a.def === "R");
    expect(resolveActions.map((a) => a.id)).toEqual(["resolve"]);
    const a = ALL_ACTIONS.find((x) => x.id === "resolve");
    expect(a?.def).toBe("R");
    expect(displayKey(a?.def)).toBe("⇧R");
  });

  it("command-mode uses the > token", () => {
    const a = ALL_ACTIONS.find((x) => x.id === "command-mode");
    expect(a?.def).toBe(">");
  });

  it("the flow actions introduce NO active-context conflict in any cube cell", () => {
    CUBE.forEach((ctx) => {
      const active = activeActionIds(ctx);
      const conflicts = findConflicts(defaultKeymap(), active);
      // none of the reported (there should be none) conflicts may involve a flow id.
      conflicts.forEach((c) => {
        expect(c.actions.every((a) => !FLOW_IDS.includes(a.id))).toBe(true);
      });
    });
  });

  it("hasUnreviewedTarget gates next/prev-unreviewed honestly", () => {
    // inReview is required (isReviewable now gates on it) AND an unreviewed target.
    const noUnreviewed: KbContext = { entity: "models", view: "topology", viewCount: 3, inReview: true, hasUnreviewed: false };
    expect(hasUnreviewedTarget(noUnreviewed)).toBe(false);
    expect(activeActionIds(noUnreviewed).has("next-unreviewed")).toBe(false);
    const withUnreviewed: KbContext = { ...noUnreviewed, hasUnreviewed: true };
    expect(hasUnreviewedTarget(withUnreviewed)).toBe(true);
    expect(activeActionIds(withUnreviewed).has("next-unreviewed")).toBe(true);
    // without inReview, even an unreviewed target stays inactive (flow not mounted).
    const notInReview: KbContext = { entity: "models", view: "topology", viewCount: 3, hasUnreviewed: true };
    expect(hasUnreviewedTarget(notInReview)).toBe(false);
    expect(activeActionIds(notInReview).has("next-unreviewed")).toBe(false);
  });

  it("canResolveThread is the HANDLER signal, not a key-visibility gate", () => {
    // `resolve` (⇧R) is keyed whenever inThreads — the prototype's behavior — so
    // the key/footer chip is visible across the whole thread surface, NOT gated
    // on hasOpenThread (that would be the subset-`when` that conflicted). The
    // canResolveThread helper is the separate HANDLER-time signal the S2/V1
    // resolve handler reads to decide whether a keypress actually resolves.
    const noThread: KbContext = { entity: "models", view: "topology", viewCount: 3 };
    expect(canResolveThread(noThread)).toBe(false);
    // resolve stays keyed in-threads regardless of hasOpenThread.
    expect(activeActionIds(noThread).has("resolve")).toBe(true);
    const withThread: KbContext = { ...noThread, hasOpenThread: true };
    expect(canResolveThread(withThread)).toBe(true);
    expect(activeActionIds(withThread).has("resolve")).toBe(true);
    // and there is no `resolve-from-keyboard` action to activate.
    expect(activeActionIds(withThread).has("resolve-from-keyboard")).toBe(false);
  });

  it("mark-reviewed-advance is the canonical mark verb (x, reviewable surfaces, in review)", () => {
    const a = ALL_ACTIONS.find((x) => x.id === "mark-reviewed-advance");
    expect(a?.def).toBe("x");
    // active on a reviewable surface ONLY once the review flow is mounted.
    expect(activeActionIds({ entity: "models", view: "code", codeMode: "diff", viewCount: 3, inReview: true }).has("mark-reviewed-advance")).toBe(true);
    expect(activeActionIds({ entity: "models", view: "code", codeMode: "diff", viewCount: 3 }).has("mark-reviewed-advance")).toBe(false);
    expect(activeActionIds({ entity: "pr", view: "overview", viewCount: 4, inReview: true }).has("mark-reviewed-advance")).toBe(false);
  });
});

describe("defaultKeymap + sparse-override merge", () => {
  it("defaultKeymap maps every action id to its def", () => {
    const km = defaultKeymap();
    expect(Object.keys(km).sort()).toEqual(ALL_ACTIONS.map((a) => a.id).sort());
    ALL_ACTIONS.forEach((a) => expect(km[a.id]).toBe(a.def));
  });

  it("mergeKeymap overlays a sparse override and keeps the rest default", () => {
    const merged = mergeKeymap({ diff: "x" });
    expect(merged.diff).toBe("x");
    expect(merged.file).toBe("f"); // untouched default
    expect(Object.keys(merged).length).toBe(ALL_ACTIONS.length);
  });

  it("mergeKeymap with null/undefined returns the bare defaults", () => {
    expect(mergeKeymap(null)).toEqual(defaultKeymap());
    expect(mergeKeymap(undefined)).toEqual(defaultKeymap());
  });
});

describe("makeCanonicalizer — alias + DEAD-shadowing", () => {
  it("identity-maps the default keymap (every default → its own canonical)", () => {
    const canon = makeCanonicalizer({});
    expect(canon("d")).toBe("d");
    expect(canon("f")).toBe("f");
  });

  it("shift-layer letters are CASE-SENSITIVE (n vs N never collide)", () => {
    // THE soundness fix: a physical bare `n` → inst-next ("n"); a physical
    // `Shift+N` (eventKey "N") → next-unreviewed ("N"). A case-folding norm
    // collapsed both onto "n", silently overwriting the alias slot.
    const canon = makeCanonicalizer({});
    expect(canon("n")).toBe("n"); // inst-next
    expect(canon("N")).toBe("N"); // next-unreviewed (distinct!)
    expect(canon("b")).toBe("b"); // inst-prev
    expect(canon("B")).toBe("B"); // prev-unreviewed
    expect(canon("c")).toBe("c"); // comments-only
    expect(canon("C")).toBe("C"); // comments-hidden
    expect(canon("o")).toBe("o"); // next-open-thread
    expect(canon("O")).toBe("O"); // prev-open-thread
    expect(canon("R")).toBe("R"); // resolve (shift-layer, no lowercase twin)
    // a shifted letter with no bound shift-layer action passes through unchanged
    // (D is not bound; it does NOT fold onto d).
    expect(canon("D")).toBe("D");
    // and the bare `d` is still its own canonical key, untouched by the above.
    expect(canon("d")).toBe("d");
  });

  it("a rebound key translates back to the action's canonical default", () => {
    // move `diff` from "d" to a FREE key "y"; pressing physical "y" yields "d".
    // (we avoid "x" — it's mark-reviewed-advance's default, a real collision.)
    const canon = makeCanonicalizer({ diff: "y" });
    expect(canon("y")).toBe("d");
  });

  it("DEAD-shadows a default whose action moved away (it stops firing)", () => {
    // diff moved off "d" → physical "d" is no longer bound to anything → DEAD.
    const canon = makeCanonicalizer({ diff: "y" });
    expect(canon("d")).toBe(DEAD);
  });

  it("a default still bound is NOT shadowed", () => {
    const canon = makeCanonicalizer({ diff: "y" });
    // file is untouched → still canonical "f".
    expect(canon("f")).toBe("f");
  });

  it("shifted-GLYPH tokens (view-N on !@#$) resolve through the shift layer", () => {
    // Shift+1 → e.key "!" → canonical "shift|1" → view-1's token "!".
    const canon = makeCanonicalizer({});
    expect(canon("!")).toBe("!"); // view-1
    expect(canon("@")).toBe("@"); // view-2
  });

  it("unmapped physical keys pass through unchanged", () => {
    const canon = makeCanonicalizer({});
    expect(canon("z")).toBe("z");
    // an unbound shift-layer letter passes through (does not fold onto its base).
    expect(canon("Z")).toBe("Z");
  });
});

describe("captureKey deny-list (fixed:true + reserved + chords)", () => {
  it("rejects reserved keys (space/enter/tab/esc/backspace/delete)", () => {
    [" ", "Spacebar", "Enter", "Tab", "Escape", "Backspace", "Delete"].forEach((key) =>
      expect(captureKey({ key })).toBeNull(),
    );
  });

  it("rejects modifier-only + arrow keys", () => {
    ["Shift", "Control", "Alt", "Meta", "CapsLock", "ArrowLeft", "ArrowUp"].forEach((key) =>
      expect(captureKey({ key })).toBeNull(),
    );
  });

  it("rejects modifier chords (not bindable yet)", () => {
    expect(captureKey({ key: "d", metaKey: true })).toBeNull();
    expect(captureKey({ key: "d", ctrlKey: true })).toBeNull();
    expect(captureKey({ key: "d", altKey: true })).toBeNull();
  });

  it("accepts a single printable char, PRESERVING letter case", () => {
    // case-preserving so a user can rebind onto a shift-layer letter (Shift+N → "N").
    expect(captureKey({ key: "d" })).toBe("d");
    expect(captureKey({ key: "D" })).toBe("D");
    expect(captureKey({ key: "N" })).toBe("N");
    expect(captureKey({ key: "?" })).toBe("?");
  });

  it("DENY_REBIND_KEYS unions fixed:true defaults AND the reserved non-action keys", () => {
    const fixedDefs = ALL_ACTIONS.filter((a) => a.fixed).map((a) => a.def);
    fixedDefs.forEach((def) => expect(DENY_REBIND_KEYS.has(def)).toBe(true));
    // sanity: Tab/Enter/Space are the fixed keys.
    expect(DENY_REBIND_KEYS.has("Tab")).toBe(true);
    expect(DENY_REBIND_KEYS.has("Enter")).toBe(true);
    expect(DENY_REBIND_KEYS.has(" ")).toBe(true);
    // the alignment fix: reserved NON-action keys are also denied (so a
    // programmatic/persisted override can't bypass captureKey's restrictions).
    ["Escape", "Backspace", "Delete", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].forEach((key) =>
      expect(DENY_REBIND_KEYS.has(key)).toBe(true),
    );
    // every RESERVED_KEY (the single source captureKey + the deny-list share) is
    // rejected by capture AND present in the deny-list — no drift between the two.
    RESERVED_KEYS.forEach((key) => {
      expect(captureKey({ key })).toBeNull();
      expect(DENY_REBIND_KEYS.has(key)).toBe(true);
    });
  });
});

describe("displayKey + parseToken + layerBindings", () => {
  it("pretty-prints reserved tokens", () => {
    expect(displayKey(" ")).toBe("Space");
    expect(displayKey("Enter")).toBe("↵");
    expect(displayKey("Tab")).toBe("Tab");
    expect(displayKey("")).toBe("—");
    expect(displayKey(null)).toBe("—");
  });

  it("renders shift-letter + shift-glyph tokens", () => {
    expect(displayKey("R")).toBe("⇧R");
    expect(displayKey("?")).toBe("?"); // glyph passes through
  });

  it("parseToken derives the shift layer for shifted glyphs + capitals", () => {
    expect(parseToken("?").mods.has("shift")).toBe(true);
    expect(parseToken("?").base).toBe("/");
    expect(parseToken("R").mods.has("shift")).toBe(true);
    expect(parseToken("R").base).toBe("r");
    expect(parseToken("d").mods.size).toBe(0);
  });

  it("layerBindings splits base vs shift layers", () => {
    const byLayer = layerBindings(defaultKeymap());
    // "d" (diff) lives on the base layer; "R" (resolve) on the shift layer.
    expect(byLayer[""]?.["d"]?.some((b) => b.id === "diff")).toBe(true);
    expect(byLayer["shift"]?.["r"]?.some((b) => b.id === "resolve")).toBe(true);
  });
});

describe("footerHints — registry-derived chips, honest flow degrade", () => {
  it("emits the instance-cycle chip with the context noun on a model surface", () => {
    const chips = footerHints({ entity: "models", view: "code", codeMode: "diff", viewCount: 3, noun: "model" });
    const inst = chips.find((c) => c.label === "model");
    expect(inst).toBeDefined();
    expect(inst?.active).toBe(true);
  });

  it("always emits the command chip (command-mode is app-wide)", () => {
    CUBE.forEach((ctx) => {
      const chips = footerHints(ctx);
      expect(chips.some((c) => c.label === "command")).toBe(true);
    });
  });

  it("the flow chips degrade honestly: greyed when their when-context is unmet", () => {
    // models·topology with NO unreviewed target → the unreviewed chip is greyed.
    const chips = footerHints({ entity: "models", view: "topology", viewCount: 3, inReview: true, hasUnreviewed: false });
    const unreviewed = chips.find((c) => c.label === "unreviewed");
    expect(unreviewed).toBeDefined();
    expect(unreviewed?.active).toBe(false);
    // with the flow mounted AND an unreviewed target it becomes active.
    const active = footerHints({ entity: "models", view: "topology", viewCount: 3, inReview: true, hasUnreviewed: true });
    expect(active.find((c) => c.label === "unreviewed")?.active).toBe(true);
    // without inReview the chip is still emitted (surface family) but greyed.
    const notMounted = footerHints({ entity: "models", view: "topology", viewCount: 3, hasUnreviewed: true });
    expect(notMounted.find((c) => c.label === "unreviewed")?.active).toBe(false);
  });

  it("the resolve chip IS the keyboard-resolve flow chip (⇧R, active in-threads)", () => {
    // after the dedup there is one resolve chip — `resolve` (⇧R, inThreads) — and
    // NO separate greyed "resolve thread" chip (that was the deduped twin).
    const chips = footerHints({ entity: "models", view: "topology", viewCount: 3 });
    expect(chips.some((c) => c.label === "resolve thread")).toBe(false);
    const resolve = chips.find((c) => c.label === "resolve");
    expect(resolve).toBeDefined();
    expect(resolve?.active).toBe(true);
    expect(resolve?.keys).toContain("⇧R");
  });

  it("chip keys reflect a rebinding (footer never drifts from the keymap)", () => {
    const chips = footerHints({ entity: "models", view: "topology", viewCount: 3 }, { compiled: "y" });
    const dfc = chips.find((c) => c.label === "diff/file/compiled");
    expect(dfc?.keys).toContain("y");
  });

  it("folds the dirHint motions into typed chips on a topo shelf", () => {
    const chips = footerHints({ entity: "models", view: "topology", viewCount: 3 });
    expect(chips.some((c) => c.label === "graph node")).toBe(true);
    expect(chips.some((c) => c.label === "code")).toBe(true);
  });
});
