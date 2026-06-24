// The SINGLE keydown dispatcher (S2) — the ONE capture-phase listener for the
// whole app (discovery risk #5: never N competing listeners; the prototype's
// R-ref mirror is DISSOLVED). It:
//   • registers ONCE on mount (an empty-dep effect — never re-subscribes, so the
//     listener identity is stable and there is exactly one),
//   • reads LIVE state via `useAppStore.getState()` inside the handler (the
//     R-ref dissolution — no stale closure, no re-register on every state change),
//   • canonicalizes the physical key through S1's `makeCanonicalizer`,
//   • routes through the PURE `routeKey` ladder (src/domain/dispatch.ts),
//   • applies the returned typed `DispatchAction` to the store.
//
// LAYER: data (may import domain; never view/chrome). The chrome mounts it once
// (the App root); it owns no JSX.

import { useEffect } from "react";
import { useAppStore } from "./store";
import { makeCanonicalizer } from "../domain/keymap";
import { routeKey, type DispatchAction, type KeyEventLike } from "../domain/dispatch";
import { deriveView } from "./nav-slice";
import { anyOverlayOpen } from "./ui-slice";
import { viewsFor } from "../domain/matrix";
import { isInsideShadowOrEditable } from "../domain/diff/shadow-guard";
import { dataSlice } from "./data-slice";
import { parsePatchNav } from "../domain/diff/patch-nav";
import { nextUnreviewed, prevUnreviewed } from "../domain/review/review-machine";
import type { AppState } from "./store";

// ── V1 flow-handler helpers (data layer; pure derivations off the live store) ──

/** The in-scope MODEL list (the review LOOP's scope) for the active context. */
export function reviewScope(st: AppState): string[] {
  // the PR-scope selectable MODELS the reviewer walks — prSelectableModels has
  // already dropped the seed/macro/non-model ids prSelectable carries (cute-dbt#495)
  // and falls back to every model when the PR scope is empty. The loop MUST walk
  // models only: a recordless seed/macro id would show the wrong model's diff while
  // marking the seed/macro reviewed (the never-a-false-claim violation).
  return dataSlice(st.activeSource).prSelectableModels;
}

/**
 * Advance the Models selection to a flow target (next/prev-unreviewed). Pure
 * routing of the review-machine result into the nav + ui state: select the
 * target model, switch to the code/diff surface (so the reviewer lands ON a
 * reviewable diff), and reset the hunk cursor for the fresh file. A null target
 * (loop complete / nothing to advance to) is an honest no-op.
 */
function advanceTo(st: AppState, target: string | null): void {
  if (target == null) return;
  st.setSel(target, "models");
  st.setView("code");
  st.setCodeMode("diff");
}

/**
 * Apply a typed dispatch action to the store. Pulled out (and exported) so it is
 * unit-testable without a DOM: feed it an action + assert the store mutation.
 * Every branch is a store call — no routing logic here (that's the pure ladder).
 */
export function applyDispatch(action: DispatchAction): void {
  const st = useAppStore.getState();
  switch (action.kind) {
    case "history-back":
      st.historyBack();
      return;
    case "history-forward":
      st.historyForward();
      return;
    case "toggle-overlay":
      st.toggleOverlay(action.overlay);
      return;
    case "open-overlay":
      st.openOverlay(action.overlay);
      return;
    case "set-entity":
      st.setEntity(action.entity);
      return;
    case "goto-pr":
      st.setEntity("pr");
      return;
    case "set-view": {
      // ⇧digit on the ALREADY-active view is a no-op here (S2 routes the view;
      // the surface-level "cycle the topology surface" affordance lands with S6).
      st.setView(action.view);
      return;
    }
    case "set-code-mode":
      // V1: the Models code-surface mode now lives in the store (the dispatcher
      // gates [ ] / ⇧R / the hunk cursor on it). Resets the hunk cursor.
      st.setCodeMode(action.mode);
      return;
    case "set-data-mode":
      // the Data-view mode lands with the Data surface (S7). Routed; no store field yet.
      return;
    case "toggle-panel":
      st.toggleOverlay("shelf");
      return;
    case "cycle-instance":
    case "mark-reviewed-advance":
    case "next-unreviewed":
    case "prev-unreviewed":
    case "resolve-from-keyboard":
    case "step-hunk":
      // the V1 review-flow verbs — delegated to applyReviewFlow so applyDispatch
      // stays a flat, low-complexity router (the dispatch CRAP ceiling).
      applyReviewFlow(st, action);
      return;
    case "context":
      // a surface-scoped context key (thread nav, …) — its handler lands with the
      // owning surface. Routed through the ONE dispatcher; no-op here.
      return;
  }
}

/** The V1 review-flow action subset applyReviewFlow handles. */
type ReviewFlowAction = Extract<
  DispatchAction,
  { kind: "cycle-instance" | "mark-reviewed-advance" | "next-unreviewed" | "prev-unreviewed" | "resolve-from-keyboard" | "step-hunk" }
>;

/**
 * Apply a V1 review-flow verb against the store. Split out of applyDispatch so
 * the top-level router stays low-complexity (the dispatch CRAP ceiling). Each
 * verb is Models-scoped (the reviewable entity in V1) and an honest no-op when
 * its precondition (a selected model / a scope / a focusable thread) isn't met.
 */
export function applyReviewFlow(st: AppState, action: ReviewFlowAction): void {
  switch (action.kind) {
    case "cycle-instance": {
      // the ordered instance cycle over the in-scope model list (Models only;
      // other entities' instance lists land with their slices). Wraps.
      if (st.entity !== "models") return;
      const scope = reviewScope(st);
      if (!scope.length) return;
      const cur = st.sel.models;
      const at = cur ? scope.indexOf(cur) : -1;
      const from = at >= 0 ? at : action.dir > 0 ? -1 : 0;
      const next = scope[(((from + action.dir) % scope.length) + scope.length) % scope.length];
      if (next) st.setSel(next, "models");
      return;
    }
    case "mark-reviewed-advance":
      // mark the current model reviewed AND advance to the next-unreviewed model.
      if (st.entity !== "models" || !st.sel.models) return;
      advanceTo(st, st.markReviewedAdvance(reviewScope(st), st.sel.models));
      return;
    case "next-unreviewed":
      if (st.entity !== "models" || !st.sel.models) return;
      advanceTo(st, nextUnreviewed(st.review, reviewScope(st), st.sel.models));
      return;
    case "prev-unreviewed":
      if (st.entity !== "models" || !st.sel.models) return;
      advanceTo(st, prevUnreviewed(st.review, reviewScope(st), st.sel.models));
      return;
    case "resolve-from-keyboard":
      if (st.entity !== "models") return;
      // the ⇧R keyboard-resolve verb: toggle the focused thread's resolved state.
      resolveFocusedThread(st);
      return;
    case "step-hunk":
      if (st.entity !== "models") return;
      // step the running hunk cursor over the active model's change-run anchors
      // (the S5 next/prev-hunk deferral V1 owns). Empty/absent patch → no-op.
      st.stepHunkCursor(activeAnchors(st), action.dir);
      return;
  }
}

/** The active model's change-run anchors (parsed from its committed patch). */
export function activeAnchors(st: AppState): ReturnType<typeof parsePatchNav>["starts"] {
  const model = st.sel.models;
  if (!model) return [];
  const rec = dataSlice(st.activeSource).D[model];
  if (!rec) return [];
  return parsePatchNav(rec.patch).starts;
}

/**
 * Resolve (toggle) the focused thread on the active model. The focus target is
 * the live comment at the running hunk-cursor anchor line; failing that, the
 * model's first live comment line. Honest no-op when the model has no live
 * thread. (V1: the keyboard-resolve verb the prototype's mouse-only resolve
 * lacked — FEATURE-GAP P2#6.)
 */
export function resolveFocusedThread(st: AppState): void {
  const model = st.sel.models;
  if (!model) return;
  const rec = dataSlice(st.activeSource).D[model];
  if (!rec || !rec.comments.length) return;
  const anchors = activeAnchors(st);
  const idx = st.hunkCursor.index;
  const anchorLine = idx >= 0 && idx < anchors.length ? anchors[idx]?.no : undefined;
  // prefer a comment AT the cursor's anchor line; else the first live comment.
  const focused =
    (anchorLine != null ? rec.comments.find((c) => c.line === anchorLine) : undefined) ?? rec.comments[0];
  const line = focused?.line;
  if (line == null) return;
  const currently = st.review.resolved[`${model}@${line}`] === true;
  st.setThreadResolved(model, line, !currently);
}

/**
 * The TRUE focused leaf for a captured keydown — pierces the shadow DOM.
 *
 * Discovery risk #5 (the whole reason for capture-phase listening): when a
 * keydown is captured on `window`, the browser RETARGETS `e.target` to the
 * shadow HOST (e.g. Pierre's `<diffs-container>`), NOT the `<input>`/`<textarea>`
 * leaf the user is actually typing in inside the shadow root. Reading `e.target`
 * therefore mis-reports the host's tag and the input-guard rung never fires — so
 * hotkeys leak into a comment composer inside the diff.
 *
 * `e.composedPath()[0]` is the deepest (real) target, pierced across every shadow
 * boundary; the input-guard runs against THAT. Falls back to `e.target` when
 * `composedPath` is unavailable or returns an empty path (defensive — some
 * synthetic/legacy events).
 */
export function keyTarget(e: Pick<KeyboardEvent, "target"> & { composedPath?: () => EventTarget[] }): HTMLElement | null {
  const path = e.composedPath?.();
  const leaf = path && path.length > 0 ? path[0] : e.target;
  return (leaf as HTMLElement | null) ?? null;
}

/**
 * Mount the single capture-phase keydown dispatcher. Call ONCE at the App root.
 */
export function useKeydown(): void {
  useEffect(() => {
    function onKey(e: KeyboardEvent): void {
      const st = useAppStore.getState();
      // RISK#2 (S5): a keystroke whose composedPath crosses the Pierre shadow
      // root (`diffs-container`) — or any editable field — belongs to THAT
      // surface; the app dispatcher must not hijack it. This guard sits ABOVE
      // the pure input-guard rung: it short-circuits BEFORE canonicalization, so
      // Pierre's own in-shadow key handling (and the Composer textarea) is never
      // intercepted. (This guard — not stopPropagation — is what keeps the
      // Composer's ⌘↵/esc working: the Composer only preventDefaults; the
      // TEXTAREA in the composedPath trips isInsideShadowOrEditable, so the app
      // dispatcher returns early and never sees those keys.)
      const path = e.composedPath?.();
      if (isInsideShadowOrEditable(path)) return;
      // Canonicalize the physical key through the live (possibly rebound) keymap.
      const canon = makeCanonicalizer(st.keymapOverride);
      // Pierce the shadow DOM: `e.target` is retargeted to the shadow host under
      // capture; `composedPath()[0]` is the real focused leaf (discovery risk #5).
      const target = keyTarget(e);
      const ev: KeyEventLike = {
        key: canon(e.key),
        code: e.code,
        rawKey: e.key,
        shiftKey: e.shiftKey,
        metaKey: e.metaKey,
        altKey: e.altKey,
        ctrlKey: e.ctrlKey,
        targetTag: target?.tagName,
        targetEditable: target?.isContentEditable ?? false,
      };
      const view = deriveView(st.viewMap, st.entity);
      const result = routeKey(ev, {
        entity: st.entity,
        view,
        modal: anyOverlayOpen(st.overlays),
        // the active code-surface mode — gates the diff/thread surface keys
        // ([ ] / ⇧R / the hunk cursor) on the Models code diff (V1).
        codeMode: st.codeMode,
      });
      if (result.preventDefault) e.preventDefault();
      if (result.action) applyDispatch(result.action);
    }
    // capture: true — the single dispatcher sees the key before any surface
    // (the prototype used a window listener; capture phase guarantees ONE
    // authoritative entry point even with focus inside a Pierre shadow root).
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, []); // register ONCE — getState() supplies live state (R-ref dissolved).
}

/** The active view for the current store state (selector for the chrome). */
export function useActiveView() {
  const viewMap = useAppStore((s) => s.viewMap);
  const entity = useAppStore((s) => s.entity);
  return deriveView(viewMap, entity);
}

/** Convenience: the available views for the active entity. */
export function useEntityViews() {
  const entity = useAppStore((s) => s.entity);
  return viewsFor(entity);
}
