// The ui-slice unit tests: the modal gate (which overlays own the keyboard), the
// overlay toggle/open/close, and the codeAnchor nonce discipline (a re-anchor on
// repeat; deterministic monotonic nonce, NOT Date.now).
import { describe, it, expect } from "vitest";
import {
  createUiSlice,
  anyOverlayOpen,
  MODAL_OVERLAYS,
  UI_DEFAULTS,
  type UiSlice,
  type OverlayFlags,
} from "./ui-slice";

function harness() {
  let state: UiSlice;
  const set: (p: UiSlice | Partial<UiSlice> | ((s: UiSlice) => UiSlice | Partial<UiSlice>)) => void = (p) => {
    const patch = typeof p === "function" ? p(state) : p;
    state = { ...state, ...patch };
  };
  const get = () => state;
  state = createUiSlice(set, get);
  return { get };
}

describe("anyOverlayOpen — the modal gate membership", () => {
  it("the keyboard-owning overlays fire the gate", () => {
    for (const name of MODAL_OVERLAYS) {
      const flags = { ...UI_DEFAULTS.overlays, [name]: true } as OverlayFlags;
      expect(anyOverlayOpen(flags), `${name} should gate`).toBe(true);
    }
  });
  it("the persistent PANELS (sidebar, shelf) do NOT own the keyboard", () => {
    expect(anyOverlayOpen({ ...UI_DEFAULTS.overlays, sidebar: true })).toBe(false);
    expect(anyOverlayOpen({ ...UI_DEFAULTS.overlays, shelf: true })).toBe(false);
  });
  it("all-closed → no gate", () => {
    expect(anyOverlayOpen(UI_DEFAULTS.overlays)).toBe(false);
  });
});

describe("ui slice — overlay flag actions", () => {
  it("toggle/open/close one flag", () => {
    const { get } = harness();
    get().toggleOverlay("palette");
    expect(get().overlays.palette).toBe(true);
    get().closeOverlay("palette");
    expect(get().overlays.palette).toBe(false);
    get().openOverlay("settings");
    expect(get().overlays.settings).toBe(true);
  });
  it("closeAllOverlays clears every flag", () => {
    const { get } = harness();
    get().openOverlay("palette");
    get().openOverlay("settings");
    get().openOverlay("sidebar");
    get().closeAllOverlays();
    expect(Object.values(get().overlays).every((v) => v === false)).toBe(true);
  });
});

describe("ui slice — codeAnchor nonce discipline", () => {
  it("setCodeAnchor bumps a MONOTONIC nonce (deterministic, not Date.now)", () => {
    const { get } = harness();
    get().setCodeAnchor({ id: "customers", line: 10, side: "new" });
    const n1 = get().codeAnchor?.nonce;
    expect(n1).toBe(1);
    // re-anchoring the SAME (id,line,side) still bumps the nonce → forces a re-scroll.
    get().setCodeAnchor({ id: "customers", line: 10, side: "new" });
    expect(get().codeAnchor?.nonce).toBe(2);
  });
  it("setCodeAnchor(null) clears the anchor", () => {
    const { get } = harness();
    get().setCodeAnchor({ id: "x", line: 1, side: "old" });
    get().setCodeAnchor(null);
    expect(get().codeAnchor).toBeNull();
  });
});

describe("ui slice — code-mode + the running hunk cursor (V1)", () => {
  const anchors = [
    { no: 3, side: "additions" as const },
    { no: 8, side: "deletions" as const },
  ];

  it("defaults to diff mode + an unset hunk cursor", () => {
    const { get } = harness();
    expect(get().codeMode).toBe("diff");
    expect(get().hunkCursor).toEqual({ index: -1, nonce: 0 });
  });

  it("setCodeMode switches the mode AND resets the hunk cursor (fresh diff surface)", () => {
    const { get } = harness();
    get().stepHunkCursor(anchors, 1); // advance the cursor first
    expect(get().hunkCursor.index).toBe(0);
    get().setCodeMode("file");
    expect(get().codeMode).toBe("file");
    expect(get().hunkCursor).toEqual({ index: -1, nonce: 0 }); // reset
  });

  it("stepHunkCursor steps the cursor + bumps the nonce on every step", () => {
    const { get } = harness();
    get().stepHunkCursor(anchors, 1);
    expect(get().hunkCursor.index).toBe(0);
    expect(get().hunkCursor.nonce).toBe(1);
    get().stepHunkCursor(anchors, 1);
    expect(get().hunkCursor.index).toBe(1);
    expect(get().hunkCursor.nonce).toBe(2);
    get().stepHunkCursor(anchors, 1); // wrap
    expect(get().hunkCursor.index).toBe(0);
    expect(get().hunkCursor.nonce).toBe(3);
  });

  it("stepHunkCursor on an EMPTY anchor list is a no-op index (-1) but still bumps the nonce", () => {
    const { get } = harness();
    get().stepHunkCursor([], 1);
    expect(get().hunkCursor.index).toBe(-1);
    expect(get().hunkCursor.nonce).toBe(1);
  });

  it("resetHunkCursor clears the cursor", () => {
    const { get } = harness();
    get().stepHunkCursor(anchors, 1);
    get().resetHunkCursor();
    expect(get().hunkCursor).toEqual({ index: -1, nonce: 0 });
  });
});
