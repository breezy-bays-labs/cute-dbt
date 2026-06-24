// shadow-guard — RISK#2: a keystroke whose composedPath crosses the Pierre
// shadow root (or an editable field) belongs to that surface, NOT the app
// dispatcher. `isInsideShadowOrEditable` is the pure predicate the capture-phase
// listener consults so it does not hijack typing inside the diff's shadow DOM.
import { describe, it, expect } from "vitest";
import { isInsideShadowOrEditable } from "./shadow-guard";

// minimal element-like stubs for the composedPath array.
const el = (tag: string, extra: Record<string, unknown> = {}): unknown => ({
  tagName: tag.toUpperCase(),
  nodeName: tag.toUpperCase(),
  isContentEditable: false,
  ...extra,
});
const shadowHost = (): unknown => ({ tagName: "DIFFS-CONTAINER", nodeName: "DIFFS-CONTAINER" });

describe("isInsideShadowOrEditable", () => {
  it("true when the path contains a diffs-container (Pierre shadow host)", () => {
    expect(isInsideShadowOrEditable([el("span"), shadowHost(), el("div")])).toBe(true);
  });

  it("true when the path contains a TEXTAREA (composer)", () => {
    expect(isInsideShadowOrEditable([el("textarea"), el("div")])).toBe(true);
  });

  it("true when the path contains an INPUT", () => {
    expect(isInsideShadowOrEditable([el("input")])).toBe(true);
  });

  it("true for a contentEditable element", () => {
    expect(isInsideShadowOrEditable([el("div", { isContentEditable: true })])).toBe(true);
  });

  it("false for a plain app path (buttons, divs)", () => {
    expect(isInsideShadowOrEditable([el("button"), el("div"), el("main")])).toBe(false);
  });

  it("false / safe on an empty or undefined path", () => {
    expect(isInsideShadowOrEditable([])).toBe(false);
    expect(isInsideShadowOrEditable(undefined)).toBe(false);
  });

  it("matches a custom shadow-host tag passed explicitly", () => {
    expect(isInsideShadowOrEditable([el("my-host")], ["my-host"])).toBe(true);
    expect(isInsideShadowOrEditable([el("my-host")])).toBe(false); // default list excludes it
  });
});
