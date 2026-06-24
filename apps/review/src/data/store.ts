// The Zustand store (S0 slice — selection only). Persists the selected model +
// theme under a versioned `cute-dbt:` localStorage key. Later slices grow this
// into the 5-slice store (data/nav/keymap/review/ui); S0 proves the persist wire.
import { create } from "zustand";
import { persist, createJSONStorage } from "zustand/middleware";
import type { AppTheme } from "../domain/highlighter";

export const PERSIST_KEY = "cute-dbt:review";
export const PERSIST_VERSION = 1;

export interface AppState {
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
    }),
    {
      name: PERSIST_KEY,
      version: PERSIST_VERSION,
      storage: createJSONStorage(() => localStorage),
      // fail-closed merge: defaults win for any key the persisted blob lacks.
      merge: (persisted, current) => ({ ...current, ...(persisted as Partial<AppState>) }),
      partialize: (s) => ({ selectedModel: s.selectedModel, theme: s.theme }),
    },
  ),
);
