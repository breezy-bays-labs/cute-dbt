// The navigation Zustand slice (S2) — one of the 5 store slices (data/nav/keymap/
// review/ui). It owns the dispatch-state spine the prototype's ~40 useStates
// flatten into: `entity`, the PER-ENTITY `viewMap` (view is DERIVED), the
// PER-ENTITY `sel`, the UNCONSTRAINED `prNode` (split from sel.models), and the
// history ring buffer (cap 100, push-unless-equal-top, truncate-forward).
//
// LAYER: data (may import domain; never view/chrome). The slice is composed into
// the app store (src/data/store.ts); React reads it the same way as the S0/S1
// slices. Immer powers the nested-map updates (viewMap/sel).
//
// THE prNode ⇆ sel.models SPLIT (load-bearing, never collapse — discovery §nav):
//   • `sel.models` is the Models-entity selected INSTANCE (the model you review).
//   • `prNode` is the PR-lineage cursor — UNCONSTRAINED: it can be a model, a
//     seed, a macro, or a DELETED node that is not in `modelsList` at all.
// Clicking a node on the PR DAG moves `prNode` and updates the shelf WITHOUT
// changing which model the Models entity reviews. Collapsing them would make the
// PR cursor unable to land on a seed/macro/deleted node (it would be clamped to
// the model set) — the exact bug the split exists to prevent.

import { produce } from "immer";
import { defaultViewFor, viewsFor, type View } from "../domain/matrix";
import type { Entity } from "../domain/keymap";

/** A point in navigation history — the (entity, view, sel) tuple, JSON-snapshotted. */
export type HistorySnapshot = string;

/** The history ring buffer (cap 100). `idx` is the current position in `stack`. */
export interface HistoryState {
  stack: HistorySnapshot[];
  idx: number;
}

export const HISTORY_CAP = 100;

/** The nav slice's state + actions (composed into the app store). */
export interface NavSlice {
  /** the active entity (number-row). */
  entity: Entity;
  /** per-entity last-view memory; the active `view` is DERIVED, never stored bare. */
  viewMap: Record<Entity, View>;
  /** per-entity selected instance (the model/macro/seed/file under review). */
  sel: Record<Entity, string | null>;
  /** the PR-lineage cursor — UNCONSTRAINED, SPLIT from sel.models (never collapse). */
  prNode: string | null;
  /** the navigation history ring buffer. */
  history: HistoryState;

  /** switch the active entity (does NOT clear per-entity view/sel memory). */
  setEntity: (entity: Entity) => void;
  /** set the active entity's view (writes into viewMap[entity]). */
  setView: (view: View) => void;
  /** set the selected instance for an entity (defaults to the active entity). */
  setSel: (id: string | null, entity?: Entity) => void;
  /** set the UNCONSTRAINED PR-lineage cursor (any resource; not clamped to models). */
  setPrNode: (id: string | null) => void;
  /** push the CURRENT (entity, view, sel) onto history (push-unless-equal-top). */
  pushHistory: () => void;
  /** step back through history (applies the prior snapshot). */
  historyBack: () => void;
  /** step forward through history (applies the next snapshot). */
  historyForward: () => void;
}

/**
 * The DERIVED active view for an entity: its remembered view if available, else
 * the entity's first matrix view. This is the `view = viewMap[entity] ||
 * AVAIL[entity][0]` rule from the prototype, made a pure selector so no consumer
 * stores a bare `view`.
 */
export function deriveView(viewMap: Record<Entity, View>, entity: Entity): View {
  const remembered = viewMap[entity];
  if (remembered && viewsFor(entity).includes(remembered)) return remembered;
  return defaultViewFor(entity);
}

/** The JSON snapshot of a navigable position (the history element). */
export function snapshotOf(entity: Entity, view: View, sel: Record<Entity, string | null>): HistorySnapshot {
  return JSON.stringify({ entity, view, sel });
}

/** Zustand StateCreator-shaped setter/getter pair, narrowed to this slice's shape. */
export type NavSliceSet = (
  partial: NavSlice | Partial<NavSlice> | ((state: NavSlice) => NavSlice | Partial<NavSlice>),
) => void;
export type NavSliceGet = () => NavSlice;

/** The persisted defaults — exact prototype seeds. */
export const NAV_DEFAULTS = {
  entity: "models" as Entity,
  viewMap: {
    pr: "overview",
    models: "topology",
    macros: "review",
    seeds: "review",
    else: "review",
  } as Record<Entity, View>,
  sel: {
    pr: null,
    models: "customers",
    macros: "cents_to_dollars",
    seeds: "raw_payments",
    else: "README.md",
  } as Record<Entity, string | null>,
};

/**
 * Push the current position onto a history ring buffer with the prototype's
 * exact discipline (extracted PURE so the ring semantics are unit-tested without
 * a store): push-unless-equal-top (no-op when already at the snapshot — the
 * back/forward case), truncate-forward (a new push after a back drops the
 * forward tail), cap at HISTORY_CAP (shift the oldest, decrement idx).
 */
export function pushSnapshot(h: HistoryState, snap: HistorySnapshot): HistoryState {
  if (h.stack[h.idx] === snap) return h; // already here (from back/forward)
  let stack = h.stack.slice(0, h.idx + 1); // truncate any forward tail
  stack.push(snap);
  let idx = stack.length - 1;
  if (stack.length > HISTORY_CAP) {
    stack = stack.slice(1); // drop the oldest
    idx -= 1;
  }
  return { stack, idx };
}

/** Build the nav slice. `set`/`get` are the store's setter/getter. */
export function createNavSlice(set: NavSliceSet, get: NavSliceGet): NavSlice {
  // Apply a history snapshot back into entity/viewMap/sel (the back/forward body).
  const applySnap = (snap: HistorySnapshot): void => {
    const o = JSON.parse(snap) as { entity: Entity; view: View; sel: Record<Entity, string | null> };
    set(
      produce((s: NavSlice) => {
        s.entity = o.entity;
        s.viewMap[o.entity] = o.view;
        s.sel = o.sel;
      }),
    );
  };

  return {
    entity: NAV_DEFAULTS.entity,
    viewMap: { ...NAV_DEFAULTS.viewMap },
    sel: { ...NAV_DEFAULTS.sel },
    prNode: null,
    history: { stack: [], idx: -1 },

    setEntity: (entity) =>
      set(
        produce((s: NavSlice) => {
          s.entity = entity;
        }),
      ),

    setView: (view) =>
      set(
        produce((s: NavSlice) => {
          s.viewMap[s.entity] = view;
        }),
      ),

    setSel: (id, entity) =>
      set(
        produce((s: NavSlice) => {
          s.sel[entity ?? s.entity] = id;
        }),
      ),

    setPrNode: (id) =>
      set(
        produce((s: NavSlice) => {
          s.prNode = id;
        }),
      ),

    pushHistory: () => {
      const s = get();
      const snap = snapshotOf(s.entity, deriveView(s.viewMap, s.entity), s.sel);
      const next = pushSnapshot(s.history, snap);
      if (next !== s.history) set({ history: next });
    },

    historyBack: () => {
      const h = get().history;
      if (h.idx > 0) {
        const idx = h.idx - 1;
        const snap = h.stack[idx];
        if (snap === undefined) return;
        set({ history: { ...h, idx } });
        applySnap(snap);
      }
    },

    historyForward: () => {
      const h = get().history;
      if (h.idx < h.stack.length - 1) {
        const idx = h.idx + 1;
        const snap = h.stack[idx];
        if (snap === undefined) return;
        set({ history: { ...h, idx } });
        applySnap(snap);
      }
    },
  };
}
