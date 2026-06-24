// The Zustand store (S0 slice — selection only). Persists the selected model +
// theme under a versioned `cute-dbt:` localStorage key. Later slices grow this
// into the 5-slice store (data/nav/keymap/review/ui); S0 proves the persist wire.
import { create } from "zustand";
import { persist, createJSONStorage } from "zustand/middleware";
import type { AppTheme } from "../domain/highlighter";
import { createKeymapSlice, type KeymapSlice } from "./keymap-slice";
import { DENY_REBIND_KEYS, type Keymap } from "../domain/keymap";

/**
 * Sanitize a persisted keymap override on hydration: drop any binding onto a
 * reserved/fixed token (the same `DENY_REBIND_KEYS` guard `rebindAction`
 * enforces at write time). Without this, a stale or hand-edited localStorage
 * blob could reintroduce a reserved binding (`Tab`, `Space`, an arrow, …) that
 * the live rebind path refuses — letting persisted state bypass the deny-list.
 * Non-object/missing input degrades to an empty override (fail-closed).
 */
export function sanitizeKeymapOverride(raw: unknown): Keymap {
  if (!raw || typeof raw !== "object") return {};
  const out: Keymap = {};
  for (const [id, token] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof token !== "string") continue; // drop malformed entries
    if (DENY_REBIND_KEYS.has(token)) continue; // drop reserved bindings
    out[id] = token;
  }
  return out;
}

export const PERSIST_KEY = "cute-dbt:review";
export const PERSIST_VERSION = 1;

// The S0 selection slice + the S1 keymap slice. Later slices (nav/review/ui)
// compose in the same way. The keymap slice owns only the sparse rebind override;
// every binding/predicate lives in the pure domain registry.
export interface AppState extends KeymapSlice {
  selectedModel: string | null;
  theme: AppTheme;
  setSelectedModel: (name: string) => void;
  setTheme: (theme: AppTheme) => void;
}

export const useAppStore = create<AppState>()(
  persist(
    (set) => ({
      selectedModel: null,
      theme: "tokyo",
      setSelectedModel: (name) => set({ selectedModel: name }),
      setTheme: (theme) => set({ theme }),
      // keymap slice — its `set` is the store setter, narrowed to the slice's shape.
      ...createKeymapSlice((partial) =>
        set(partial as Partial<AppState> | ((s: AppState) => Partial<AppState>)),
      ),
    }),
    {
      name: PERSIST_KEY,
      version: PERSIST_VERSION,
      storage: createJSONStorage(() => localStorage),
      // fail-closed merge: defaults win for any key the persisted blob lacks,
      // AND the persisted keymap override is sanitized through the same deny-list
      // `rebindAction` enforces (reserved tokens can't survive a stale blob).
      merge: (persisted, current) => {
        const p = (persisted ?? {}) as Partial<AppState>;
        return {
          ...current,
          ...p,
          keymapOverride: sanitizeKeymapOverride(p.keymapOverride),
        };
      },
      // persist the selection, theme, AND the keymap override (rebindings stick).
      partialize: (s) => ({ selectedModel: s.selectedModel, theme: s.theme, keymapOverride: s.keymapOverride }),
    },
  ),
);
