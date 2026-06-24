// The precedence-ladder unit tests — each RUNG asserted in isolation, plus the
// ordering guarantees between rungs (input-guard before everything; alt-arrow
// BEFORE the modal gate; the modal gate suppressing app/entity/view/context). The
// ladder is a PURE function, so these are DOM-free (KeyEventLike is a plain
// object — the S1 captureKey pattern).
import { describe, it, expect } from "vitest";
import { routeKey, shiftDigit, type DispatchInput, type KeyEventLike } from "./dispatch";

const base: DispatchInput = { entity: "models", view: "topology", modal: false };

/** A canonicalized key event (the hook canonicalizes BEFORE routeKey; here we
 *  pass the canonical token as `key` and mirror it in `rawKey` unless noted). */
function ev(over: Partial<KeyEventLike> & { key: string }): KeyEventLike {
  return { rawKey: over.key, ...over };
}

describe("routeKey — rung 1: input-guard", () => {
  it.each(["INPUT", "SELECT", "TEXTAREA"])("ignores keys from a %s target", (tag) => {
    const r = routeKey(ev({ key: "2", targetTag: tag }), base);
    expect(r.action).toBeNull();
    expect(r.preventDefault).toBe(false);
  });
  it("ignores keys from a contenteditable target", () => {
    const r = routeKey(ev({ key: "2", targetEditable: true }), base);
    expect(r.action).toBeNull();
  });
  it("the guard wins even over an alt-arrow history chord", () => {
    const r = routeKey(ev({ key: "ArrowLeft", altKey: true, targetTag: "INPUT" }), base);
    expect(r.action).toBeNull();
  });
});

describe("routeKey — rung 2: bare-modifier", () => {
  it.each(["Shift", "Control", "Alt", "Meta", "CapsLock"])("a lone %s never fires", (mod) => {
    const r = routeKey(ev({ key: mod }), base);
    expect(r.action).toBeNull();
    expect(r.preventDefault).toBe(false);
  });
});

describe("routeKey — rung 3: alt-arrow history (before the modal gate)", () => {
  it("⌥← → history-back", () => {
    expect(routeKey(ev({ key: "ArrowLeft", altKey: true }), base).action).toEqual({ kind: "history-back" });
  });
  it("⌥→ → history-forward", () => {
    expect(routeKey(ev({ key: "ArrowRight", altKey: true }), base).action).toEqual({ kind: "history-forward" });
  });
  it("FIRES even when an overlay is open (history precedes the modal gate)", () => {
    const r = routeKey(ev({ key: "ArrowLeft", altKey: true }), { ...base, modal: true });
    expect(r.action).toEqual({ kind: "history-back" });
  });
});

describe("routeKey — rung 4: modal-gate", () => {
  it("an open overlay OWNS the keyboard: app/entity/view/context all suppressed", () => {
    const m = { ...base, modal: true };
    for (const k of ["?", "/", ",", "s", "w", "p", "2", "n", "v", "d"]) {
      expect(routeKey(ev({ key: k }), m).action, `key ${k} under modal`).toBeNull();
    }
    // ⇧digit too
    expect(routeKey(ev({ key: "@", code: "Digit2", shiftKey: true }), m).action).toBeNull();
  });
});

describe("routeKey — rung 5: app keys", () => {
  it.each([
    ["?", { kind: "toggle-overlay", overlay: "kbDrawer" }],
    ["/", { kind: "open-overlay", overlay: "palette" }],
    [",", { kind: "toggle-overlay", overlay: "settings" }],
    ["s", { kind: "toggle-overlay", overlay: "sidebar" }],
    ["w", { kind: "open-overlay", overlay: "review" }],
    ["p", { kind: "goto-pr" }],
  ] as const)("%s → %o", (key, action) => {
    expect(routeKey(ev({ key }), base).action).toEqual(action);
  });
  it("a BARE app key claims + preventDefaults (the happy path)", () => {
    const r = routeKey(ev({ key: "w" }), base);
    expect(r.action).toEqual({ kind: "open-overlay", overlay: "review" });
    expect(r.preventDefault).toBe(true);
  });
  it.each([
    ["w", "metaKey"], // ⌘W (close tab)
    ["w", "ctrlKey"], // ⌃W
    ["p", "metaKey"], // ⌘P (print)
    ["p", "ctrlKey"],
    ["s", "metaKey"], // ⌘S (save)
    ["s", "ctrlKey"],
  ] as const)("a %s chord with %s is NOT claimed (browser shortcut left alone)", (key, mod) => {
    // the canonicalizer normalizes only the bare key, so a ⌘/⌃+W press arrives
    // here as key="w"; rungAppKeys must NOT claim it or call preventDefault.
    const r = routeKey(ev({ key, [mod]: true }), base);
    expect(r.action).toBeNull();
    expect(r.preventDefault).toBe(false);
  });
  it("an alt-modified app key is also left alone (no hijack)", () => {
    const r = routeKey(ev({ key: "s", altKey: true }), base);
    expect(r.action).toBeNull();
    expect(r.preventDefault).toBe(false);
  });
});

describe("routeKey — rung 6: entity keys (number row)", () => {
  it.each([
    ["1", "pr"],
    ["2", "models"],
    ["3", "macros"],
    ["4", "seeds"],
    ["5", "else"],
  ] as const)("%s → set-entity %s", (key, entity) => {
    expect(routeKey(ev({ key }), base).action).toEqual({ kind: "set-entity", entity });
  });
  it("a number with a modifier is NOT an entity key (leaves the rung to ⇧digit/context)", () => {
    // shift+2 is a VIEW key, not entity. With shift, the entity rung is skipped.
    const r = routeKey(ev({ key: "@", rawKey: "@", code: "Digit2", shiftKey: true }), base);
    expect(r.action).toEqual({ kind: "set-view", view: "node" }); // models[1]
  });
});

describe("routeKey — rung 7: view keys (⇧digit positional over AVAIL)", () => {
  it("⇧1 selects the entity's FIRST view (Models → topology)", () => {
    const r = routeKey(ev({ key: "!", code: "Digit1", shiftKey: true }), { ...base, entity: "models" });
    expect(r.action).toEqual({ kind: "set-view", view: "topology" });
  });
  it("⇧3 selects the THIRD view (Models → data)", () => {
    const r = routeKey(ev({ key: "#", code: "Digit3", shiftKey: true }), { ...base, entity: "models" });
    expect(r.action).toEqual({ kind: "set-view", view: "data" });
  });
  it("positions are entity-relative: ⇧2 on PR → lineage, on Models → node", () => {
    expect(
      routeKey(ev({ key: "@", code: "Digit2", shiftKey: true }), { ...base, entity: "pr" }).action,
    ).toEqual({ kind: "set-view", view: "lineage" });
    expect(
      routeKey(ev({ key: "@", code: "Digit2", shiftKey: true }), { ...base, entity: "models" }).action,
    ).toEqual({ kind: "set-view", view: "node" });
  });
  it("a ⇧digit BEYOND the entity's view count is claimed-but-no-op (does NOT fall to context)", () => {
    // Macros has ONE view; ⇧2 is out of range. It must not become a context key.
    const r = routeKey(ev({ key: "@", code: "Digit2", shiftKey: true }), { ...base, entity: "macros" });
    expect(r.action).toBeNull();
  });
  it("derives the digit from e.code (layout-stable) even when the glyph map misses", () => {
    // a non-US-layout shift+3 may yield a glyph that is NOT in SHIFT_DIGIT (e.g.
    // "£"); e.code === "Digit3" is the layout-stable fallback. (We pick a glyph
    // that is neither an app key nor in the glyph map, isolating the code path.)
    const r = routeKey(ev({ key: "£", rawKey: "£", code: "Digit3", shiftKey: true }), { ...base, entity: "models" });
    expect(r.action).toEqual({ kind: "set-view", view: "data" });
  });
});

describe("shiftDigit — positional source of truth", () => {
  it("prefers e.code", () => {
    expect(shiftDigit({ key: "x", code: "Digit4" })).toBe(4);
  });
  it("falls back to the shifted glyph when no code", () => {
    expect(shiftDigit({ key: "$", rawKey: "$" })).toBe(4);
  });
  it("returns 0 for a non-digit", () => {
    expect(shiftDigit({ key: "a" })).toBe(0);
  });
});

describe("routeKey — rung 8: context keys", () => {
  it("n / b cycle the instance (not on PR)", () => {
    expect(routeKey(ev({ key: "n" }), base).action).toEqual({ kind: "cycle-instance", dir: 1 });
    expect(routeKey(ev({ key: "b" }), base).action).toEqual({ kind: "cycle-instance", dir: -1 });
  });
  it("N / B (shift layer) cycle the OTHER direction's unreviewed jump (claimed)", () => {
    // canonical token "N" is distinct from "n"; both route to cycle here (S2 spine).
    expect(routeKey(ev({ key: "N", rawKey: "N", shiftKey: true }), base).action).toEqual({
      kind: "cycle-instance",
      dir: -1,
    });
  });
  it("n / b are inert on PR", () => {
    expect(routeKey(ev({ key: "n" }), { ...base, entity: "pr", view: "overview" }).action).toBeNull();
  });
  it("x marks-reviewed-advance (not on PR)", () => {
    expect(routeKey(ev({ key: "x" }), base).action).toEqual({ kind: "mark-reviewed-advance" });
    expect(routeKey(ev({ key: "x" }), { ...base, entity: "pr", view: "overview" }).action).toBeNull();
  });
  it("d / f set code-mode in the Code view, data-mode in the Data view", () => {
    expect(routeKey(ev({ key: "d" }), { ...base, view: "code" }).action).toEqual({ kind: "set-code-mode", mode: "diff" });
    expect(routeKey(ev({ key: "f" }), { ...base, view: "code" }).action).toEqual({ kind: "set-code-mode", mode: "file" });
    expect(routeKey(ev({ key: "d" }), { ...base, view: "data" }).action).toEqual({ kind: "set-data-mode", mode: "diff" });
  });
  it("v toggles the panel", () => {
    expect(routeKey(ev({ key: "v" }), base).action).toEqual({ kind: "toggle-panel" });
  });
  it("an unclaimed key passes through", () => {
    expect(routeKey(ev({ key: "z" }), base).action).toBeNull();
  });
});

describe("routeKey — precedence ordering between rungs", () => {
  it("the entity rung wins over the context rung for a bare digit", () => {
    // '2' is an entity key, never a context key — even though context keys exist.
    expect(routeKey(ev({ key: "2" }), base).action).toEqual({ kind: "set-entity", entity: "models" });
  });
  it("app keys win over entity/context (e.g. 's' is sidebar, never an instance)", () => {
    expect(routeKey(ev({ key: "s" }), base).action).toEqual({ kind: "toggle-overlay", overlay: "sidebar" });
  });
});
