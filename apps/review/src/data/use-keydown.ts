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
    case "cycle-instance":
      // S2 has no instance list yet (that's the data slice / reshapers, S3). The
      // intent is routed + claimed; the concrete cycle wires in when the ordered
      // instance list exists. No-op store-side for now (honest: the key is live,
      // the motion lands with its data).
      return;
    case "set-code-mode":
    case "set-data-mode":
      // code/data mode is a per-surface ui concern that lands with the Code/Data
      // surfaces (S5/S7). Routed + claimed here; no store field yet.
      return;
    case "toggle-panel":
      st.toggleOverlay("shelf");
      return;
    case "mark-reviewed-advance":
      // the review FLOW verb — its store wiring lands with the review slice (V1).
      return;
    case "context":
      // a surface-scoped context key (hunk/thread nav, …) — its handler lands
      // with the owning surface. Routed through the ONE dispatcher; no-op here.
      return;
  }
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
