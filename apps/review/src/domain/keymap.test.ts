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
  DENY_REBIND_KEYS,
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

/** Build a KbContext for every (entity, view, codeMode?) the app can be in. */
function buildCube(): KbContext[] {
  const out: KbContext[] = [];
  (Object.keys(AVAIL) as Entity[]).forEach((entity) => {
    const views = AVAIL[entity];
    views.forEach((view) => {
      const viewCount = views.length;
      // code surfaces additionally vary codeMode (diff | file)
      if (view === "code") {
        (["diff", "file"] as const).forEach((codeMode) => {
          out.push({ entity, view, codeMode, viewCount });
        });
      } else {
        out.push({ entity, view, viewCount });
      }
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

  it("isReviewable is true everywhere except pr (unless pr·files)", () => {
    CUBE.forEach((c) => {
      expect(isReviewable(c)).toBe(c.entity !== "pr" || c.view === "files");
    });
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
    // Without an activeSet, cross-context reuse (R/resolve vs resolve-from-keyboard,
    // x/mark, etc.) IS counted as a global key share. The only deliberate global
    // share is the ⇧R chord (resolve / resolve-from-keyboard), which is allowed
    // because they are never both the focused handler in the same context.
    const conflicts = findConflicts(defaultKeymap());
    // every reported conflict must be a context-exclusive pair (never co-active).
    conflicts.forEach((c) => {
      const ids = c.actions.map((a) => a.id);
      // assert no cube context activates all of them together.
      const coActive = CUBE.some((ctx) => {
        const active = activeActionIds(ctx);
        return ids.every((id) => active.has(id));
      });
      expect(coActive).toBe(false);
    });
  });

  it("no ACTIVE-context conflict exists in ANY cube cell (the real gate)", () => {
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
});

describe("Council MUST-FIX D — flow actions registered first-class without conflict", () => {
  const FLOW_IDS = [
    "mark-reviewed-advance",
    "next-unreviewed",
    "prev-unreviewed",
    "resolve-from-keyboard",
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

  it("resolve-from-keyboard suggests ⇧R", () => {
    const a = ALL_ACTIONS.find((x) => x.id === "resolve-from-keyboard");
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
    const noUnreviewed: KbContext = { entity: "models", view: "topology", viewCount: 3, hasUnreviewed: false };
    expect(hasUnreviewedTarget(noUnreviewed)).toBe(false);
    expect(activeActionIds(noUnreviewed).has("next-unreviewed")).toBe(false);
    const withUnreviewed: KbContext = { ...noUnreviewed, hasUnreviewed: true };
    expect(hasUnreviewedTarget(withUnreviewed)).toBe(true);
    expect(activeActionIds(withUnreviewed).has("next-unreviewed")).toBe(true);
  });

  it("canResolveThread gates resolve-from-keyboard honestly", () => {
    const noThread: KbContext = { entity: "models", view: "topology", viewCount: 3 };
    expect(canResolveThread(noThread)).toBe(false);
    const withThread: KbContext = { ...noThread, hasOpenThread: true };
    expect(canResolveThread(withThread)).toBe(true);
    expect(activeActionIds(withThread).has("resolve-from-keyboard")).toBe(true);
  });

  it("mark-reviewed-advance is the canonical mark verb (x, reviewable surfaces)", () => {
    const a = ALL_ACTIONS.find((x) => x.id === "mark-reviewed-advance");
    expect(a?.def).toBe("x");
    expect(activeActionIds({ entity: "models", view: "code", codeMode: "diff", viewCount: 3 }).has("mark-reviewed-advance")).toBe(true);
    expect(activeActionIds({ entity: "pr", view: "overview", viewCount: 4 }).has("mark-reviewed-advance")).toBe(false);
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
    // case-insensitive single chars
    expect(canon("D")).toBe("d");
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

  it("unmapped physical keys pass through unchanged", () => {
    const canon = makeCanonicalizer({});
    expect(canon("z")).toBe("z");
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

  it("accepts a single printable char (letters lowercased)", () => {
    expect(captureKey({ key: "D" })).toBe("d");
    expect(captureKey({ key: "?" })).toBe("?");
  });

  it("DENY_REBIND_KEYS contains every fixed:true action's key", () => {
    const fixedDefs = ALL_ACTIONS.filter((a) => a.fixed).map((a) => a.def);
    fixedDefs.forEach((def) => expect(DENY_REBIND_KEYS.has(def)).toBe(true));
    // sanity: Tab/Enter/Space are the fixed keys.
    expect(DENY_REBIND_KEYS.has("Tab")).toBe(true);
    expect(DENY_REBIND_KEYS.has("Enter")).toBe(true);
    expect(DENY_REBIND_KEYS.has(" ")).toBe(true);
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
    const chips = footerHints({ entity: "models", view: "topology", viewCount: 3, hasUnreviewed: false });
    const unreviewed = chips.find((c) => c.label === "unreviewed");
    expect(unreviewed).toBeDefined();
    expect(unreviewed?.active).toBe(false);
    // with an unreviewed target it becomes active.
    const active = footerHints({ entity: "models", view: "topology", viewCount: 3, hasUnreviewed: true });
    expect(active.find((c) => c.label === "unreviewed")?.active).toBe(true);
  });

  it("the resolve-thread flow chip appears greyed in-threads with no open thread", () => {
    const chips = footerHints({ entity: "models", view: "topology", viewCount: 3 });
    const resolve = chips.find((c) => c.label === "resolve thread");
    expect(resolve).toBeDefined();
    expect(resolve?.active).toBe(false);
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
