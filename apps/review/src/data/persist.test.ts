// The persistence contract tests (discovery risk #6): the migrate-MERGE rule (a
// v0 blob missing a NEW field → the default appears, the user's other settings
// survive) + FAIL-CLOSED load (a corrupt/partial blob → defaults, never a crash
// or a wholesale drop). These pin `hydrateMerge` + `mergeSettings` directly (the
// pure halves the persist `merge`/`storage` hooks call).
import { describe, it, expect } from "vitest";
import { hydrateMerge, migratePersisted, sanitizeKeymapOverride } from "./store";
import { mergeSettings, SETTINGS_DEFAULTS } from "./settings-slice";
import { NAV_DEFAULTS } from "./nav-slice";
import { UI_DEFAULTS } from "./ui-slice";

describe("mergeSettings — migrate-MERGE over defaults", () => {
  it("a v0 blob MISSING a new field gets that field at its DEFAULT (additive migrate)", () => {
    // simulate an old user whose persisted settings predate `diffEngine`/`prdag`.
    const v0 = { theme: "dracula", density: "compact" };
    const merged = mergeSettings(v0);
    expect(merged.theme).toBe("dracula"); // the user's value wins
    expect(merged.density).toBe("compact");
    // the NEW fields appear at their defaults (never dropped, never a wholesale replace):
    expect(merged.diffEngine).toBe(SETTINGS_DEFAULTS.diffEngine);
    expect(merged.prdag).toBe(SETTINGS_DEFAULTS.prdag);
    expect(merged.contextLines).toBe(SETTINGS_DEFAULTS.contextLines);
  });
  it("fail-closed on a non-object blob → pristine defaults", () => {
    expect(mergeSettings(null)).toEqual(SETTINGS_DEFAULTS);
    expect(mergeSettings("corrupt")).toEqual(SETTINGS_DEFAULTS);
    expect(mergeSettings(undefined)).toEqual(SETTINGS_DEFAULTS);
    expect(mergeSettings(42)).toEqual(SETTINGS_DEFAULTS);
  });
  it("fail-closed on an ARRAY blob → pristine defaults (typeof [] === 'object' trap)", () => {
    // An array passes `typeof === "object"`; without the Array.isArray guard its
    // numeric indices would spread into settings as `0`, `1`, … keys.
    expect(mergeSettings([])).toEqual(SETTINGS_DEFAULTS);
    expect(mergeSettings(["dracula", "compact"])).toEqual(SETTINGS_DEFAULTS);
    // crucially: no numeric keys leaked onto the merged object.
    expect(Object.keys(mergeSettings(["x"]))).toEqual(Object.keys(SETTINGS_DEFAULTS));
  });
  it("PER-FIELD fail-closed: WRONG-TYPED values fall back to the typed default, valid fields still apply", () => {
    // A hand-edited / stale blob where the right-named fields carry the WRONG
    // runtime type (a stringified boolean, a stringified number). Each wrong-typed
    // field must degrade to ITS typed default — never spread the string over the
    // default — while correctly-typed fields in the same blob still win.
    const malformed = {
      project: "false", // string over a boolean default
      contextLines: "3", // string over a number default
      coverage: 1, // number over a boolean default
      expandStep: true, // boolean over a number default
      theme: "dracula", // valid string — still applies
      density: "compact", // valid string — still applies
    };
    const merged = mergeSettings(malformed);
    // wrong-typed fields keep the TYPED default (fail-closed at field grain):
    expect(merged.project).toBe(SETTINGS_DEFAULTS.project); // boolean, not "false"
    expect(typeof merged.project).toBe("boolean");
    expect(merged.contextLines).toBe(SETTINGS_DEFAULTS.contextLines); // number, not "3"
    expect(typeof merged.contextLines).toBe("number");
    expect(merged.coverage).toBe(SETTINGS_DEFAULTS.coverage);
    expect(typeof merged.coverage).toBe("boolean");
    expect(merged.expandStep).toBe(SETTINGS_DEFAULTS.expandStep);
    expect(typeof merged.expandStep).toBe("number");
    // valid, correctly-typed fields still apply:
    expect(merged.theme).toBe("dracula");
    expect(merged.density).toBe("compact");
  });
});

describe("hydrateMerge — the full persisted-blob → state merge", () => {
  it("MERGES each sub-blob over its slice defaults (new field appears for an old user)", () => {
    // an old persisted blob: a custom entity + a partial viewByEntity + partial sel
    // + settings missing a new field. Every default must backfill.
    const v0 = {
      entity: "pr",
      viewByEntity: { models: "data" }, // only one entity remembered
      sel: { models: "orders" }, // only one entity selected
      sidebar: true,
      settings: { theme: "gruvbox" }, // missing all the newer settings fields
      keymapOverride: { diff: "y" },
    };
    const out = hydrateMerge(v0);
    expect(out.entity).toBe("pr");
    // viewMap: the remembered models view wins; the other entities backfill defaults.
    expect(out.viewMap?.models).toBe("data");
    expect(out.viewMap?.pr).toBe(NAV_DEFAULTS.viewMap.pr);
    expect(out.viewMap?.macros).toBe(NAV_DEFAULTS.viewMap.macros);
    // sel: same — remembered wins, others backfill.
    expect(out.sel?.models).toBe("orders");
    expect(out.sel?.seeds).toBe(NAV_DEFAULTS.sel.seeds);
    // settings: the migrate-MERGE — theme wins, new fields appear.
    expect(out.settings?.theme).toBe("gruvbox");
    expect(out.settings?.diffEngine).toBe(SETTINGS_DEFAULTS.diffEngine);
    // sidebar flag lands in the overlays.
    expect(out.overlays?.sidebar).toBe(true);
    expect(out.overlays?.palette).toBe(UI_DEFAULTS.overlays.palette);
    // keymap survives sanitized.
    expect(out.keymapOverride).toEqual({ diff: "y" });
  });

  it("FAIL-CLOSED: a fully corrupt blob → empty patch (defaults win downstream)", () => {
    expect(hydrateMerge(null)).toEqual({});
    expect(hydrateMerge("corrupt-string")).toEqual({});
    expect(hydrateMerge(42)).toEqual({});
  });

  it("FAIL-CLOSED on a PARTIAL/malformed blob: each bad field degrades in place", () => {
    // entity wrong type, viewByEntity not an object, sel missing, settings garbage,
    // sidebar non-boolean — every one degrades to its default WITHOUT crashing.
    const bad = {
      entity: 123,
      viewByEntity: "not-an-object",
      sel: null,
      sidebar: "yes",
      settings: 99,
      keymapOverride: { a: "Tab" }, // reserved → stripped
    };
    const out = hydrateMerge(bad);
    expect(out.entity).toBeUndefined(); // bad type → not applied (current wins)
    expect(out.viewMap).toEqual(NAV_DEFAULTS.viewMap); // degraded to defaults
    expect(out.sel).toEqual(NAV_DEFAULTS.sel);
    expect(out.settings).toEqual(SETTINGS_DEFAULTS);
    expect(out.overlays?.sidebar).toBe(UI_DEFAULTS.overlays.sidebar); // non-bool → default
    expect(out.keymapOverride).toEqual({}); // reserved binding stripped
  });
});

describe("migratePersisted — the v1 → v2 shape migration (the ONE real prior on-disk shape)", () => {
  it("carries the v1 `selectedModel` into `sel.models` and `theme` into `settings.theme`", () => {
    // the EXACT v1 (S0/S1) persisted shape under `cute-dbt:review`:
    const v1 = { selectedModel: "orders", theme: "gruvbox", keymapOverride: { diff: "y" } };
    const out = migratePersisted(v1, 1);
    // run the migrated blob through hydrateMerge as the persist pipeline does:
    const hydrated = hydrateMerge(out);
    expect(hydrated.sel?.models).toBe("orders"); // selection survives the upgrade
    expect(hydrated.settings?.theme).toBe("gruvbox"); // theme survives the upgrade
    // the keymap override carries forward (same key + meaning across v1→v2):
    expect(hydrated.keymapOverride).toEqual({ diff: "y" });
    // un-remembered nav fields backfill their defaults (v1 had no per-entity nav):
    expect(hydrated.entity).toBe(NAV_DEFAULTS.entity);
    expect(hydrated.sel?.seeds).toBe(NAV_DEFAULTS.sel.seeds);
    // and the new settings fields appear at their defaults (additive migrate):
    expect(hydrated.settings?.diffEngine).toBe(SETTINGS_DEFAULTS.diffEngine);
  });
  it("a wrong-typed legacy `selectedModel`/`theme` degrades to defaults (fail-closed)", () => {
    const v1bad = { selectedModel: 123, theme: 99 };
    const out = migratePersisted(v1bad, 1);
    const hydrated = hydrateMerge(out);
    expect(hydrated.sel?.models).toBe(NAV_DEFAULTS.sel.models); // non-string → default
    expect(hydrated.settings?.theme).toBe(SETTINGS_DEFAULTS.theme); // non-string → default
  });
  it("a v2+ blob passes straight through (hydrateMerge owns the same-shape merge)", () => {
    const v2 = { entity: "pr", settings: { theme: "tokyo" } };
    expect(migratePersisted(v2, 2)).toBe(v2); // unchanged identity — no remap
  });
  it("a non-object legacy blob passes through (hydrateMerge fail-closes it)", () => {
    expect(migratePersisted(null, 1)).toBeNull();
    expect(hydrateMerge(migratePersisted(null, 1))).toEqual({});
  });
});

describe("sanitizeKeymapOverride — the deny-list hydration guard (unchanged from S1)", () => {
  it("keeps valid rebindings, strips reserved tokens, drops malformed", () => {
    expect(sanitizeKeymapOverride({ diff: "y", a: "Tab", b: 1 })).toEqual({ diff: "y" });
    expect(sanitizeKeymapOverride(null)).toEqual({});
  });
});
