// The persisted-store hydration sanitize — the security-relevant half of the
// keymap persistence. A stale or hand-edited localStorage blob must not be able
// to reintroduce a reserved binding the live `rebindAction` path refuses; the
// `merge` hook runs every persisted override through `sanitizeKeymapOverride`,
// which mirrors the `DENY_REBIND_KEYS` guard. These tests pin that the deny-list
// is enforced at hydration, not only at write time.
import { describe, it, expect } from "vitest";
import { sanitizeKeymapOverride } from "./store";
import { DENY_REBIND_KEYS } from "../domain/keymap";

describe("sanitizeKeymapOverride (persisted-override hydration guard)", () => {
  it("keeps valid (non-reserved) rebindings verbatim", () => {
    const out = sanitizeKeymapOverride({ diff: "y", palette: "z" });
    expect(out).toEqual({ diff: "y", palette: "z" });
  });

  it("preserves a shift-layer letter rebinding (Shift+N → 'N')", () => {
    // case-preserving capture means a stored shift-layer token survives hydration.
    const out = sanitizeKeymapOverride({ "some-action": "N" });
    expect(out).toEqual({ "some-action": "N" });
  });

  it("DROPS any binding onto a reserved token (deny-list bypass closed)", () => {
    // a stale blob trying to bind onto Tab / Space / Enter / an arrow / Escape —
    // each must be stripped, matching what rebindAction would have refused.
    const out = sanitizeKeymapOverride({
      diff: "y", // valid → kept
      a1: "Tab",
      a2: " ",
      a3: "Enter",
      a4: "ArrowLeft",
      a5: "Escape",
      a6: "Delete",
    });
    expect(out).toEqual({ diff: "y" });
    // exhaustively: every DENY_REBIND_KEYS token is stripped on hydration.
    DENY_REBIND_KEYS.forEach((token) => {
      expect(sanitizeKeymapOverride({ x: token })).toEqual({});
    });
  });

  it("drops malformed (non-string) entries and degrades non-object input", () => {
    // a deliberately malformed persisted blob (the param is `unknown`, so no cast).
    expect(sanitizeKeymapOverride({ diff: 123, file: null, ok: "y" })).toEqual({ ok: "y" });
    expect(sanitizeKeymapOverride(null)).toEqual({});
    expect(sanitizeKeymapOverride(undefined)).toEqual({});
    expect(sanitizeKeymapOverride("not-an-object")).toEqual({});
    expect(sanitizeKeymapOverride(42)).toEqual({});
  });
});
