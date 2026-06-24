// The Zustand store (S0 slice — selection only). Persists the selected model +
// theme under a versioned `cute-dbt:` localStorage key. Later slices grow this
// into the 5-slice store (data/nav/keymap/review/ui); S0 proves the persist wire.
import { create } from "zustand";
import { persist, createJSONStorage } from "zustand/middleware";
import type { AppTheme } from "../domain/highlighter";
import { createKeymapSlice, type KeymapSlice } from "./keymap-slice";

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
      // fail-closed merge: defaults win for any key the persisted blob lacks.
      merge: (persisted, current) => ({ ...current, ...(persisted as Partial<AppState>) }),
      // persist the selection, theme, AND the keymap override (rebindings stick).
      partialize: (s) => ({ selectedModel: s.selectedModel, theme: s.theme, keymapOverride: s.keymapOverride }),
    },
  ),
);
