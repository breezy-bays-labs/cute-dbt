// applyDispatch — the typed-action → store-mutation half of the single
// dispatcher. Tested against the REAL store (reset between cases) so every
// DispatchAction branch is exercised and its store effect asserted. The routing
// (which action a key produces) is covered by dispatch.test.ts; this file covers
// the APPLY side (no DOM needed — applyDispatch reads getState() directly).
import { describe, it, expect, beforeEach } from "vitest";
import { applyDispatch } from "./use-keydown";
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
