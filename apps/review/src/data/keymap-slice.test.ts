// The keymap Zustand slice — sparse-override state + derived keymap/canonicalizer.
// The slice owns ONLY the override; every binding/predicate lives in the domain.
import { describe, it, expect } from "vitest";
import {
  createKeymapSlice,
  effectiveKeymap,
  canonicalizerFor,
  captureRebind,
  defaultKeymap,
  type KeymapSlice,
} from "./keymap-slice";
import { DEAD } from "../domain/keymap";

/** A tiny test harness mimicking zustand's set over the slice's owned shape. */
function makeSlice(): { slice: KeymapSlice; state: { keymapOverride: Record<string, string> } } {
  const state = { keymapOverride: {} as Record<string, string> };
  const set: Parameters<typeof createKeymapSlice>[0] = (partial) => {
    const next = typeof partial === "function" ? partial(state) : partial;
    Object.assign(state, next);
  };
  const slice = createKeymapSlice(set);
  // wire the slice's own override into the shared state mirror.
  state.keymapOverride = slice.keymapOverride;
  return { slice, state };
}

describe("createKeymapSlice", () => {
  it("starts with an empty override (all defaults)", () => {
    const { slice } = makeSlice();
    expect(slice.keymapOverride).toEqual({});
  });

  it("rebindAction records a sparse override", () => {
    const { slice, state } = makeSlice();
    slice.rebindAction("diff", "x");
    expect(state.keymapOverride).toEqual({ diff: "x" });
  });

  it("rebindAction refuses to bind onto a fixed/reserved key (deny-list)", () => {
    const { slice, state } = makeSlice();
    slice.rebindAction("diff", "Tab"); // Tab is fixed
    expect(state.keymapOverride).toEqual({}); // unchanged
    slice.rebindAction("diff", " "); // space is fixed
    expect(state.keymapOverride).toEqual({});
  });

  it("resetAction drops one override", () => {
    const { slice, state } = makeSlice();
    slice.rebindAction("diff", "x");
    slice.rebindAction("file", "y");
    slice.resetAction("diff");
    expect(state.keymapOverride).toEqual({ file: "y" });
  });

  it("resetKeymap clears all overrides", () => {
    const { slice, state } = makeSlice();
    slice.rebindAction("diff", "x");
    slice.resetKeymap();
    expect(state.keymapOverride).toEqual({});
  });
});

describe("effectiveKeymap + canonicalizerFor (domain re-exports)", () => {
  it("effectiveKeymap merges the override over defaults", () => {
    const km = effectiveKeymap({ diff: "x" });
    expect(km.diff).toBe("x");
    expect(km.file).toBe("f");
  });

  it("effectiveKeymap with no override equals the defaults", () => {
    expect(effectiveKeymap()).toEqual(defaultKeymap());
  });

  it("canonicalizerFor builds a live alias + DEAD-shadow for the override", () => {
    const canon = canonicalizerFor({ diff: "y" }); // "y" is free ("x" = mark verb)
    expect(canon("y")).toBe("d"); // physical y → canonical diff
    expect(canon("d")).toBe(DEAD); // the vacated default is shadowed
  });

  it("captureRebind mirrors the domain captureKey (deny-list + case-preserving)", () => {
    expect(captureRebind({ key: "Tab" })).toBeNull();
    // case is PRESERVED so a shift-layer letter (Shift+N → "N") stays rebindable.
    expect(captureRebind({ key: "D" })).toBe("D");
    expect(captureRebind({ key: "d" })).toBe("d");
  });
});
