// applyDispatch — the typed-action → store-mutation half of the single
// dispatcher. Tested against the REAL store (reset between cases) so every
// DispatchAction branch is exercised and its store effect asserted. The routing
// (which action a key produces) is covered by dispatch.test.ts; this file covers
// the APPLY side (no DOM needed — applyDispatch reads getState() directly).
import { describe, it, expect, beforeEach } from "vitest";
import { applyDispatch, keyTarget, reviewScope, activeAnchors } from "./use-keydown";
import { routeKey, type KeyEventLike, type DispatchInput } from "../domain/dispatch";
import { useAppStore } from "./store";
import { NAV_DEFAULTS, UI_DEFAULTS, REVIEW_DEFAULTS } from "./store";
import { dataSlice } from "./data-slice";
import { emptyReviewState } from "../domain/review/review-machine";

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
    codeMode: UI_DEFAULTS.codeMode,
    hunkCursor: { ...UI_DEFAULTS.hunkCursor },
    // a fresh empty review state per case (so reviewed/resolved don't bleed across).
    review: emptyReviewState(),
  });
  void REVIEW_DEFAULTS;
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

describe("applyDispatch — still-unwired intents are inert (no crash, no mutation)", () => {
  it.each([
    { kind: "set-data-mode", mode: "file" },
    { kind: "context", action: "next-hunk" },
  ] as const)("%o does not throw and leaves nav untouched", (action) => {
    const before = JSON.stringify(useAppStore.getState().sel);
    expect(() => applyDispatch(action)).not.toThrow();
    expect(JSON.stringify(useAppStore.getState().sel)).toBe(before);
  });
});

// ── V1 flow handlers (live against the REAL dataset + review slice) ───────────
// The store is reset to Models + the dataset's default model before each case.
// reviewScope() reads the live dataset; these cases assert the real store effect.
describe("applyDispatch — V1 review-flow handlers", () => {
  function setupModels(): { scope: string[]; first: string } {
    const st = useAppStore.getState();
    st.setEntity("models");
    const scope = reviewScope(st);
    const first = scope[0]!;
    st.setSel(first, "models");
    return { scope, first };
  }

  it("mark-reviewed-advance marks the current model reviewed AND advances the selection", () => {
    const { scope, first } = setupModels();
    expect(scope.length).toBeGreaterThan(1); // the dogfood fixture has many models
    applyDispatch({ kind: "mark-reviewed-advance" });
    const st = useAppStore.getState();
    expect(st.review.reviewed[first]).toBe(true); // marked
    expect(st.sel.models).not.toBe(first); // advanced off the just-reviewed model
    expect(scope).toContain(st.sel.models); // …to an in-scope model
    expect(st.review.reviewed[st.sel.models!]).toBeUndefined(); // …that's unreviewed
    expect(st.viewMap.models).toBe("code"); // landed on the reviewable code surface
  });

  it("next-unreviewed / prev-unreviewed jump to an unreviewed model (skipping reviewed)", () => {
    const { scope, first } = setupModels();
    applyDispatch({ kind: "next-unreviewed" });
    const afterNext = useAppStore.getState().sel.models;
    expect(afterNext).not.toBe(first);
    expect(scope).toContain(afterNext);
    applyDispatch({ kind: "prev-unreviewed" });
    expect(useAppStore.getState().sel.models).toBe(first); // walked back
  });

  it("set-code-mode promotes the mode to the store + resets the hunk cursor", () => {
    setupModels();
    useAppStore.getState().stepHunkCursor([{ no: 3, side: "additions" }], 1); // advance cursor
    applyDispatch({ kind: "set-code-mode", mode: "file" });
    const st = useAppStore.getState();
    expect(st.codeMode).toBe("file");
    expect(st.hunkCursor).toEqual({ index: -1, nonce: 0 }); // reset on mode switch
  });

  it("step-hunk steps the running cursor over the active model's anchors", () => {
    setupModels();
    const anchors = activeAnchors(useAppStore.getState());
    applyDispatch({ kind: "step-hunk", dir: 1 });
    const st = useAppStore.getState();
    if (anchors.length) {
      expect(st.hunkCursor.index).toBe(0); // first forward step → index 0
      expect(st.hunkCursor.nonce).toBe(1);
    } else {
      expect(st.hunkCursor.index).toBe(-1); // no anchors → honest no-op
    }
  });

  it("resolve-from-keyboard toggles a focused thread's resolved state when one exists", () => {
    const st0 = useAppStore.getState();
    st0.setEntity("models");
    // find a model that carries at least one live comment in the dataset.
    const scope = reviewScope(st0);
    const ds = dataSlice(st0.activeSource);
    const withThread = scope.find((m) => (ds.D[m]?.comments.length ?? 0) > 0);
    if (!withThread) return; // dataset has no live thread → nothing to assert (honest skip)
    st0.setSel(withThread, "models");
    const line = ds.D[withThread]!.comments[0]!.line!;
    applyDispatch({ kind: "resolve-from-keyboard" });
    expect(useAppStore.getState().review.resolved[`${withThread}@${line}`]).toBe(true);
    applyDispatch({ kind: "resolve-from-keyboard" }); // toggle back
    expect(useAppStore.getState().review.resolved[`${withThread}@${line}`]).toBeUndefined();
  });

  it("cycle-instance walks the in-scope model list (wraps)", () => {
    const { scope, first } = setupModels();
    applyDispatch({ kind: "cycle-instance", dir: 1 });
    expect(useAppStore.getState().sel.models).toBe(scope[1]);
    // wrap back to the first from the last.
    useAppStore.getState().setSel(scope[scope.length - 1]!, "models");
    applyDispatch({ kind: "cycle-instance", dir: 1 });
    expect(useAppStore.getState().sel.models).toBe(first);
  });

  // ── cute-dbt#495 finding #1: the review scope is MODELS only ────────────────
  // prSelectable carries a seed (`raw_payments`) + a macro (`cents_to_dollars`)
  // in the dogfood fixture; the review LOOP must NOT walk them (they have no
  // record in D, so advancing onto one shows the WRONG model's diff while marking
  // the seed/macro reviewed). The E2E only presses `x` twice, never reaching the
  // seed/macro positions — these cases drive the loop to COMPLETION.
  it("the review scope excludes the seed + macro prSelectable carries", () => {
    const st = useAppStore.getState();
    st.setEntity("models");
    const scope = reviewScope(st);
    const ds = dataSlice(st.activeSource);
    // the contaminated source still carries the non-models …
    expect(ds.prSelectable).toContain("raw_payments");
    expect(ds.prSelectable).toContain("cents_to_dollars");
    // … but the LOOP scope drops them and contains ONLY real models.
    expect(scope).not.toContain("raw_payments");
    expect(scope).not.toContain("cents_to_dollars");
    scope.forEach((id) => expect(ds.D[id], `${id} must be a model with a record`).toBeDefined());
  });

  it("driving `x` to loop-completion NEVER selects or marks a non-model (seed/macro)", () => {
    const st0 = useAppStore.getState();
    st0.setEntity("models");
    const scope = reviewScope(st0);
    st0.setSel(scope[0]!, "models");
    // mark every model reviewed by repeatedly pressing `x` (one more than scope
    // length to prove it terminates without ever touching a non-model).
    const visited: string[] = [];
    for (let i = 0; i < scope.length + 2; i++) {
      const cur = useAppStore.getState().sel.models;
      if (cur) visited.push(cur);
      applyDispatch({ kind: "mark-reviewed-advance" });
    }
    const st = useAppStore.getState();
    const ds = dataSlice(st.activeSource);
    // every id the loop ever SELECTED is a real model (never a seed/macro).
    visited.forEach((id) => {
      expect(ds.D[id], `selected ${id} must be a model`).toBeDefined();
      expect(id).not.toBe("raw_payments");
      expect(id).not.toBe("cents_to_dollars");
    });
    // every id the loop ever MARKED reviewed is a real model (the seed/macro are
    // NEVER in the reviewed set — the never-a-false-claim contract).
    Object.keys(st.review.reviewed).forEach((id) => {
      expect(ds.D[id], `reviewed ${id} must be a model`).toBeDefined();
    });
    expect(st.review.reviewed["raw_payments"]).toBeUndefined();
    expect(st.review.reviewed["cents_to_dollars"]).toBeUndefined();
    // the loop completes: every in-scope MODEL is reviewed, nothing else.
    expect(Object.keys(st.review.reviewed).sort()).toEqual([...scope].sort());
  });
});
