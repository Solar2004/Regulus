import { create } from "zustand";
import { persist } from "zustand/middleware";

export type RegulusView = "home" | "composition" | "settings" | "operate" | "templates";

interface RegulusUiState {
  view: RegulusView;
  startPage: "home" | "composition";
  density: "comfortable" | "compact";
  setStartPage: (page: "home" | "composition") => void;
  setDensity: (density: "comfortable" | "compact") => void;
  applyPreset: (preset: "default" | "focus" | "compact") => void;
  setView: (view: RegulusView) => void;
  /** The profile the canvas should show next, set from another view; null: its own choice. */
  canvasProfileId: string | null;
  openInCanvas: (profileId: string) => void;
}

// Persist client preferences only; runtime state belongs to the core.
export const useRegulusUi = create<RegulusUiState>()(persist((set) => ({
  view: "home",
  startPage: "home",
  density: "comfortable",
  setStartPage: (startPage) => set({ startPage }),
  setDensity: (density) => set({ density }),
  applyPreset: (preset) => set({ density: preset === "compact" ? "compact" : "comfortable", startPage: preset === "focus" ? "composition" : "home" }),
  setView: (view) => set({ view }),
  canvasProfileId: null,
  openInCanvas: (profileId) => set({ view: "composition", canvasProfileId: profileId }),
}), {
  name: "regulus-shell-preferences",
  partialize: (state) => ({ startPage: state.startPage, density: state.density }),
  merge: (saved, current) => {
    const preferences = saved as Partial<RegulusUiState> | undefined;
    const startPage = preferences?.startPage === "composition" ? "composition" : "home";
    const density = preferences?.density === "compact" ? "compact" : "comfortable";
    return { ...current, startPage, density, view: startPage };
  },
}));
