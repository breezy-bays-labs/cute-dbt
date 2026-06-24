// data-slice tests — the activeSource field (the module-global replacement) +
// the source-keyed dataset accessor.
import { describe, expect, it } from "vitest";
import {
  createDataSlice, DATA_DEFAULTS, DATA_SOURCE_LABELS, DATA_SOURCES, dataSlice, isDataSource,
} from "./data-slice";

describe("createDataSlice — the activeSource field", () => {
  it("defaults to context.440 (the prototype's _activeSrc = pr440)", () => {
    let state = { activeSource: DATA_DEFAULTS.activeSource };
    const slice = createDataSlice((p) => { state = { ...state, ...(typeof p === "function" ? p(state) : p) }; });
    expect(slice.activeSource).toBe("context.440");
  });
  it("setActiveSource accepts a known source, ignores an unknown one (fail-closed)", () => {
    let state = { activeSource: DATA_DEFAULTS.activeSource };
    const slice = createDataSlice((p) => { state = { ...state, ...(typeof p === "function" ? p(state) : p) }; });
    slice.setActiveSource("context.sample");
    expect(state.activeSource).toBe("context.sample");
    slice.setActiveSource("not-a-source");
    expect(state.activeSource).toBe("context.sample"); // unchanged
    slice.setActiveSource(42);
    expect(state.activeSource).toBe("context.sample"); // unchanged
  });
});

describe("isDataSource + the catalog", () => {
  it("recognizes every catalogued source + rejects others", () => {
    DATA_SOURCES.forEach((s) => expect(isDataSource(s)).toBe(true));
    expect(isDataSource("nope")).toBe(false);
    expect(isDataSource(undefined)).toBe(false);
  });
  it("every source has a label", () => {
    DATA_SOURCES.forEach((s) => expect(DATA_SOURCE_LABELS[s]).toBeTruthy());
  });
});

describe("dataSlice — the source-keyed dataset accessor", () => {
  it("loads + validates + builds a dataset for each source", () => {
    DATA_SOURCES.forEach((s) => {
      const ds = dataSlice(s);
      expect(ds.MODELS.length).toBeGreaterThan(0);
      ds.MODELS.forEach((n) => expect(ds.D[n]).toBeDefined());
    });
  });
  it("is memoized: two calls for the same source return the SAME dataset (WeakMap on the fixture identity)", () => {
    // loadFixture returns a stable object per id (the imported JSON module), so the
    // WeakMap memo holds across calls.
    expect(dataSlice("context.440")).toBe(dataSlice("context.440"));
  });
});
