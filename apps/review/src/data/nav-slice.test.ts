// The nav-slice unit tests: the history ring buffer (push-unless-equal-top,
// truncate-forward, cap), the derived-view rule, and the prNode ⇆ sel.models
// SPLIT (the load-bearing independence). The pure ring helper (`pushSnapshot`)
// is tested directly; the slice behaviors are tested through a minimal in-memory
// store harness (no zustand, no React — exercise the creator with a plain set/get).
import { describe, it, expect } from "vitest";
import {
  pushSnapshot,
  deriveView,
  snapshotOf,
  applySnap,
  createNavSlice,
  HISTORY_CAP,
  NAV_DEFAULTS,
  type NavSlice,
  type HistoryState,
} from "./nav-slice";
import type { Entity } from "../domain/keymap";
import type { View } from "../domain/matrix";

const empty: HistoryState = { stack: [], idx: -1 };

describe("pushSnapshot — the history ring discipline", () => {
  it("pushes onto an empty ring", () => {
    const h = pushSnapshot(empty, "A");
    expect(h).toEqual({ stack: ["A"], idx: 0 });
  });
  it("push-unless-equal-top: re-pushing the current snapshot is a no-op (===)", () => {
    const h1 = pushSnapshot(empty, "A");
    const h2 = pushSnapshot(h1, "A");
    expect(h2).toBe(h1); // identity preserved (the back/forward no-op case)
  });
  it("truncate-forward: a push after a back drops the forward tail", () => {
    let h = pushSnapshot(empty, "A");
    h = pushSnapshot(h, "B");
    h = pushSnapshot(h, "C"); // stack A,B,C idx 2
    h = { ...h, idx: 0 }; // simulate two backs to A
    h = pushSnapshot(h, "D"); // a new branch from A
    expect(h.stack).toEqual(["A", "D"]);
    expect(h.idx).toBe(1);
  });
  it("caps at HISTORY_CAP by dropping the oldest + decrementing idx", () => {
    let h = empty;
    for (let i = 0; i < HISTORY_CAP + 5; i++) h = pushSnapshot(h, "s" + i);
    expect(h.stack.length).toBe(HISTORY_CAP);
    expect(h.stack[0]).toBe("s5"); // the first 5 were shifted off
    expect(h.idx).toBe(HISTORY_CAP - 1);
  });
});

describe("deriveView — view = viewMap[entity] || AVAIL[entity][0]", () => {
  it("uses the remembered view when it is available for the entity", () => {
    const vm = { ...NAV_DEFAULTS.viewMap, models: "data" as View };
    expect(deriveView(vm, "models")).toBe("data");
  });
  it("falls back to the first matrix view when the remembered view is unavailable", () => {
    const vm = { ...NAV_DEFAULTS.viewMap, models: "files" as View }; // not a Models view
    expect(deriveView(vm, "models")).toBe("topology");
  });
  it("falls back when the entity has no remembered view", () => {
    const vm = { ...NAV_DEFAULTS.viewMap } as Record<Entity, View>;
    delete (vm as Record<string, View>)["pr"];
    expect(deriveView(vm, "pr")).toBe("overview");
  });
});

/** A minimal in-memory store harness — drives the slice creator without zustand. */
function harness() {
  let state: NavSlice;
  const set: (p: NavSlice | Partial<NavSlice> | ((s: NavSlice) => NavSlice | Partial<NavSlice>)) => void = (p) => {
    const patch = typeof p === "function" ? p(state) : p;
    state = { ...state, ...patch };
  };
  const get = () => state;
  state = createNavSlice(set, get);
  return { get, set };
}

describe("nav slice — prNode ⇆ sel.models SPLIT (never collapse)", () => {
  it("setPrNode does NOT change sel.models, and vice-versa", () => {
    const { get } = harness();
    get().setSel("customers", "models");
    get().setPrNode("a_deleted_seed"); // an UNCONSTRAINED node not in any model list
    expect(get().sel.models).toBe("customers");
    expect(get().prNode).toBe("a_deleted_seed");

    get().setSel("orders", "models");
    expect(get().prNode).toBe("a_deleted_seed"); // prNode untouched by a model selection
    expect(get().sel.models).toBe("orders");
  });
  it("prNode is unconstrained — accepts a value that is not a model/seed/macro at all", () => {
    const { get } = harness();
    get().setPrNode("some::deleted::node");
    expect(get().prNode).toBe("some::deleted::node");
  });
  it("setSel targets the ACTIVE entity by default but can be entity-scoped", () => {
    const { get } = harness();
    get().setEntity("macros");
    get().setSel("my_macro"); // defaults to the active entity
    expect(get().sel.macros).toBe("my_macro");
    get().setSel("a_seed", "seeds"); // explicit entity
    expect(get().sel.seeds).toBe("a_seed");
    expect(get().sel.macros).toBe("my_macro"); // unchanged
  });
});

describe("nav slice — per-entity view memory + history back/forward", () => {
  it("setView writes into the ACTIVE entity's viewMap slot only", () => {
    const { get } = harness();
    get().setEntity("models");
    get().setView("data");
    expect(get().viewMap.models).toBe("data");
    expect(get().viewMap.pr).toBe(NAV_DEFAULTS.viewMap.pr); // untouched
  });
  it("pushHistory + back/forward round-trip the (entity, view, sel)", () => {
    const { get } = harness();
    // position 1: models/topology
    get().pushHistory();
    // move to pr/overview
    get().setEntity("pr");
    get().pushHistory();
    // move to models/data
    get().setEntity("models");
    get().setView("data");
    get().pushHistory();

    expect(get().history.stack.length).toBe(3);
    get().historyBack(); // → pr/overview
    expect(get().entity).toBe("pr");
    get().historyBack(); // → models/topology
    expect(get().entity).toBe("models");
    expect(deriveView(get().viewMap, "models")).toBe("topology");
    get().historyForward(); // → pr/overview
    expect(get().entity).toBe("pr");
  });
  it("snapshotOf is a stable JSON of (entity, view, sel)", () => {
    const sel = { ...NAV_DEFAULTS.sel };
    expect(snapshotOf("models", "topology", sel)).toBe(
      JSON.stringify({ entity: "models", view: "topology", sel }),
    );
  });
  it("applySnap mutates the passed draft directly (no internal set)", () => {
    // The atomicity refactor: applySnap is a pure draft-mutator. Restoring a
    // snapshot writes entity/viewMap[entity]/sel onto the SAME object.
    const draft = createNavSlice(
      () => {},
      () => draft,
    );
    const snap = snapshotOf("pr", "lineage", { ...NAV_DEFAULTS.sel, pr: "x" });
    applySnap(draft, snap);
    expect(draft.entity).toBe("pr");
    expect(draft.viewMap.pr).toBe("lineage");
    expect(draft.sel.pr).toBe("x");
  });
  it("historyBack restores index AND nav state together (atomic — never a torn state)", () => {
    const { get } = harness();
    get().pushHistory(); // pos 0: models/topology
    get().setEntity("pr");
    get().setView("lineage");
    get().pushHistory(); // pos 1: pr/lineage
    expect(get().history.idx).toBe(1);

    get().historyBack();
    // After the single atomic set the index and the restored snapshot are
    // mutually consistent: idx 0 ⇔ models/topology (never idx 0 with pr still set).
    expect(get().history.idx).toBe(0);
    expect(get().entity).toBe("models");
    expect(deriveView(get().viewMap, "models")).toBe("topology");
  });
});
