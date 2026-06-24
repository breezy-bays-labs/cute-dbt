// The keymap Zustand slice (S1) — one of the 5 store slices (data/nav/keymap/
// review/ui). It holds the SPARSE override the user has rebound (settings.keymap:
// Record<actionId, token>) and exposes the merged keymap + the live canonicalizer,
// both derived from the pure domain registry (src/domain/keymap.ts). No keyboard
// logic lives here — this slice only owns the override state; every binding,
// predicate, and selector is imported from the domain.
//
// LAYER: data (may import domain; never view/chrome). The slice is wired into the
// app store (src/data/store.ts) so React reads it the same way as the S0 slice.
import {
  defaultKeymap,
  mergeKeymap,
  makeCanonicalizer,
  captureKey,
  DENY_REBIND_KEYS,
  type Keymap,
  type CapturableKey,
} from "../domain/keymap";

/** The keymap slice's state + actions (composed into the app store). */
export interface KeymapSlice {
  /** the sparse override (only the rebound action ids); empty = all defaults. */
  keymapOverride: Keymap;
  /** rebind one action to a token (validated against the capture deny-list). */
  rebindAction: (id: string, token: string) => void;
  /** reset one action to its default (drops it from the override). */
  resetAction: (id: string) => void;
  /** reset all rebindings (clears the override). */
  resetKeymap: () => void;
}

/** Zustand slice creator (StateCreator-shaped without importing zustand types here). */
export type SliceSet = (
  partial:
    | Partial<{ keymapOverride: Keymap }>
    | ((state: { keymapOverride: Keymap }) => Partial<{ keymapOverride: Keymap }>),
) => void;

/**
 * Build the keymap slice. The `set` is the store's setter; we keep it minimal so
 * the slice composes into the full store in S2 without coupling to its shape.
 */
export function createKeymapSlice(set: SliceSet): KeymapSlice {
  return {
    keymapOverride: {},
    rebindAction: (id, token) => {
      // refuse to rebind onto a fixed/reserved key (the captureKey deny-list).
      if (DENY_REBIND_KEYS.has(token)) return;
      set((s) => ({ keymapOverride: { ...s.keymapOverride, [id]: token } }));
    },
    resetAction: (id) =>
      set((s) => {
        const next = { ...s.keymapOverride };
        delete next[id];
        return { keymapOverride: next };
      }),
    resetKeymap: () => set({ keymapOverride: {} }),
  };
}

/** The merged, effective keymap for a given override (defaults + sparse merge). */
export function effectiveKeymap(override?: Keymap | null): Keymap {
  return mergeKeymap(override);
}

/** The live canonicalizer for a given override (physical → canonical + DEAD-shadow). */
export function canonicalizerFor(override?: Keymap | null): (eventKey: string) => string {
  return makeCanonicalizer(override);
}

/** Re-export the capture helper so the (future) remap UI imports from the slice. */
export function captureRebind(e: CapturableKey): string | null {
  return captureKey(e);
}

/** The default keymap (for the reset-to-defaults affordance + tests). */
export { defaultKeymap };
