// applyDispatch — the typed-action → store-mutation half of the single
// dispatcher. Tested against the REAL store (reset between cases) so every
// DispatchAction branch is exercised and its store effect asserted. The routing
// (which action a key produces) is covered by dispatch.test.ts; this file covers
// the APPLY side (no DOM needed — applyDispatch reads getState() directly).
import { describe, it, expect, beforeEach } from "vitest";
import { applyDispatch, keyTarget } from "./use-keydown";
import { routeKey, type KeyEventLike, type DispatchInput } from "../domain/dispatch";
import { useAppStore } from "./store";
import { NAV_DEFAULTS, UI_DEFAULTS } from "./store";

beforeEach(() => {
  // reset the store to a known baseline (defaults) before each case.
  useAppStore.setState({
    entity: NAV_DEFAULTS.entity,
    viewMap: { ...NAV_DEFAULTS.viewMap },
    sel: { ...NAV_DEFAULTS.sel },
    prNode: null,
    history: { stack: [], idx: -1 },
    overlays: { ...UI_DEFAULTS.overlays },
    codeAnchor: null,
    anchorNonce: 0,
  });
});

describe("applyDispatch — overlay actions", () => {
  it("toggle-overlay flips a flag", () => {
    applyDispatch({ kind: "toggle-overlay", overlay: "settings" });
    expect(useAppStore.getState().overlays.settings).toBe(true);
    applyDispatch({ kind: "toggle-overlay", overlay: "settings" });
    expect(useAppStore.getState().overlays.settings).toBe(false);
  });
  it("open-overlay opens a flag", () => {
    applyDispatch({ kind: "open-overlay", overlay: "palette" });
    expect(useAppStore.getState().overlays.palette).toBe(true);
  });
  it("toggle-panel toggles the shelf overlay", () => {
    applyDispatch({ kind: "toggle-panel" });
    expect(useAppStore.getState().overlays.shelf).toBe(true);
  });
});

describe("applyDispatch — nav actions", () => {
  it("set-entity changes the entity", () => {
    applyDispatch({ kind: "set-entity", entity: "macros" });
    expect(useAppStore.getState().entity).toBe("macros");
  });
  it("goto-pr jumps to the PR entity", () => {
    applyDispatch({ kind: "goto-pr" });
    expect(useAppStore.getState().entity).toBe("pr");
  });
  it("set-view writes the active entity's viewMap slot", () => {
    useAppStore.getState().setEntity("models");
    applyDispatch({ kind: "set-view", view: "data" });
    expect(useAppStore.getState().viewMap.models).toBe("data");
  });
  it("history-back / history-forward drive the ring", () => {
    const st = useAppStore.getState();
    st.pushHistory(); // models/topology
    st.setEntity("pr");
    st.pushHistory(); // pr/overview
    applyDispatch({ kind: "history-back" });
    expect(useAppStore.getState().entity).toBe("models");
    applyDispatch({ kind: "history-forward" });
    expect(useAppStore.getState().entity).toBe("pr");
  });
});

describe("keyTarget — pierces the shadow DOM (discovery risk #5)", () => {
  // A keydown captured on `window` is RETARGETED to the shadow host; the real
  // focused leaf is `composedPath()[0]`. Synthetic objects stand in for the DOM
  // (vitest env is `node`, no real shadow DOM) — exactly the DOM-independent
  // KeyEventLike pattern the ladder already uses.
  const fakeEl = (tagName: string, isContentEditable = false): HTMLElement =>
    ({ tagName, isContentEditable }) as unknown as HTMLElement;

  it("returns the deepest leaf (the INPUT inside the shadow root), NOT the host", () => {
    const input = fakeEl("INPUT");
    const shadowHost = fakeEl("DIFFS-CONTAINER");
    const e = {
      target: shadowHost, // RETARGETED to the host (the wrong answer)
      composedPath: () => [input, shadowHost, globalThis as unknown as EventTarget],
    };
    expect(keyTarget(e)).toBe(input);
  });

  it("the input-guard rung short-circuits for a shadow-DOM input (hotkeys SUPPRESSED)", () => {
    // Build the KeyEventLike the hook builds — but from the PIERCED leaf, not
    // the retargeted host. A bare '2' (an entity hotkey) must NOT fire.
    const input = fakeEl("INPUT");
    const shadowHost = fakeEl("DIFFS-CONTAINER");
    const e = {
      target: shadowHost,
      composedPath: () => [input, shadowHost, globalThis as unknown as EventTarget],
    };
    const target = keyTarget(e);
    const ev: KeyEventLike = {
      key: "2",
      targetTag: target?.tagName,
      targetEditable: target?.isContentEditable ?? false,
    };
    const st: DispatchInput = { entity: "models", view: "topology", modal: false };
    const r = routeKey(ev, st);
    expect(r.action).toBeNull(); // input-guard won — no set-entity leaked
    expect(r.preventDefault).toBe(false);
  });

  it("a contenteditable leaf inside a shadow host is also guarded", () => {
    const editable = fakeEl("DIV", /* isContentEditable */ true);
    const shadowHost = fakeEl("DIFFS-CONTAINER");
    const e = {
      target: shadowHost,
      composedPath: () => [editable, shadowHost, globalThis as unknown as EventTarget],
    };
    const target = keyTarget(e);
    const ev: KeyEventLike = {
      key: "2",
      targetTag: target?.tagName,
      targetEditable: target?.isContentEditable ?? false,
    };
    const r = routeKey(ev, { entity: "models", view: "topology", modal: false });
    expect(r.action).toBeNull();
  });

  it("falls back to e.target when composedPath is absent or empty", () => {
    const tgt = fakeEl("BUTTON");
    expect(keyTarget({ target: tgt })).toBe(tgt); // no composedPath at all
    expect(keyTarget({ target: tgt, composedPath: () => [] })).toBe(tgt); // empty path
  });

  it("a real (light-DOM) hotkey still fires — guard does NOT over-suppress", () => {
    // composedPath[0] is the document body (not a form control) → '2' fires.
    const body = fakeEl("BODY");
    const e = { target: body, composedPath: () => [body, globalThis as unknown as EventTarget] };
    const target = keyTarget(e);
    const ev: KeyEventLike = { key: "2", targetTag: target?.tagName, targetEditable: false };
    const r = routeKey(ev, { entity: "models", view: "topology", modal: false });
    expect(r.action).toEqual({ kind: "set-entity", entity: "models" });
  });
});

describe("applyDispatch — claimed-but-unwired intents are inert (no crash, no mutation)", () => {
  it.each([
    { kind: "cycle-instance", dir: 1 },
    { kind: "set-code-mode", mode: "diff" },
    { kind: "set-data-mode", mode: "file" },
    { kind: "mark-reviewed-advance" },
    { kind: "context", action: "next-hunk" },
  ] as const)("%o does not throw and leaves nav untouched", (action) => {
    const before = JSON.stringify(useAppStore.getState().sel);
    expect(() => applyDispatch(action)).not.toThrow();
    expect(JSON.stringify(useAppStore.getState().sel)).toBe(before);
  });
});
