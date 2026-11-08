import { create } from "zustand";
import { persist } from "zustand/middleware";

export type RegulusView = "home" | "composition" | "settings" | "operate" | "templates";

/** `start_view` of `[client]` (docs/04-client.md §11) → the view it names. */
const startViews: Record<string, RegulusView> = {
  home: "home",
  canvas: "composition",
  operate: "operate",
  templates: "templates",
  settings: "settings",
};

/** The view `start_view` names; null if it names none, so the app's own start page stays. */
export function startViewOf(value: unknown): RegulusView | null {
  return typeof value === "string" && Object.prototype.hasOwnProperty.call(startViews, value) ? startViews[value] : null;
}

interface RegulusUiState {
  view: RegulusView;
  startPage: "home" | "composition";
  /** The person chose the start page here: it wins over `start_view` of config.toml. */
  startPageChosen: boolean;
  density: "comfortable" | "compact";
  setStartPage: (page: "home" | "composition") => void;
  setDensity: (density: "comfortable" | "compact") => void;
  applyPreset: (preset: "default" | "focus" | "compact") => void;
  setView: (view: RegulusView) => void;
  /** Opens on `start_view` once, if the person chose no start page and has not moved yet. */
  applyStartView: (value: unknown) => void;
  /** The profile the canvas should show next, set from another view; null: its own choice. */
  canvasProfileId: string | null;
  openInCanvas: (profileId: string) => void;
}

// Persist client preferences only; runtime state belongs to the core.
export const useRegulusUi = create<RegulusUiState>()(persist((set, get) => ({
  view: "home",
  startPage: "home",
  startPageChosen: false,
  density: "comfortable",
  setStartPage: (startPage) => set({ startPage, startPageChosen: true }),
  setDensity: (density) => set({ density }),
  applyPreset: (preset) => set({ density: preset === "compact" ? "compact" : "comfortable", startPage: preset === "focus" ? "composition" : "home", startPageChosen: true }),
  setView: (view) => set({ view }),
  applyStartView: (value) => {
    const view = startViewOf(value);
    if (view && !get().startPageChosen && get().view === get().startPage) set({ view });
  },
  canvasProfileId: null,
  openInCanvas: (profileId) => set({ view: "composition", canvasProfileId: profileId }),
}), {
  name: "regulus-shell-preferences",
  partialize: (state) => ({ startPage: state.startPage, startPageChosen: state.startPageChosen, density: state.density }),
  merge: (saved, current) => {
    const preferences = saved as Partial<RegulusUiState> | undefined;
    const startPage = preferences?.startPage === "composition" ? "composition" : "home";
    const density = preferences?.density === "compact" ? "compact" : "comfortable";
    const startPageChosen = preferences?.startPageChosen === true;
    return { ...current, startPage, startPageChosen, density, view: startPage };
  },
}));
