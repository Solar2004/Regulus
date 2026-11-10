// The canvas preferences: one object with defaults, kept on this device like the shell's
// density and start page. Nothing here configures the core.
import { create } from "zustand";
import { persist } from "zustand/middleware";
import { LAYOUT_DIRECTIONS, type LayoutDirection } from "./layout";
import type { Grouping } from "./palette";

export interface CanvasSettings {
  minZoom: number;
  maxZoom: number;
  fitPadding: number;
  snapToGrid: boolean;
  gridSize: number;
  /** Width of a node on the canvas, in pixels. */
  nodeWidth: number;
  /** Space between the workflow's own ports and its nodes, in pixels. */
  selfGap: number;
  paletteGrouping: Grouping;
  /** Which way wires run when the nodes are arranged. */
  layoutDirection: LayoutDirection;
  /** Space between one rank of an arrangement and the next, in pixels. */
  layoutLayerGap: number;
  /** Space between nodes of one rank, and between unconnected groups, in pixels. */
  layoutNodeGap: number;
  /** Height assumed for a node the canvas has not measured yet, in pixels. */
  nodeHeight: number;
  /** How far a node added by click moves away from one already there, in pixels. */
  cascadeStep: number;
  autosave: boolean;
  autosaveMs: number;
  /** Keys that delete the selection (`KeyboardEvent.key` names). */
  deleteKeys: string[];
  /** With Ctrl or Cmd, saves. */
  saveKey: string;
}

export const canvasDefaults: CanvasSettings = {
  minZoom: 0.15,
  maxZoom: 1.8,
  fitPadding: 0.12,
  snapToGrid: false,
  gridSize: 16,
  nodeWidth: 250,
  selfGap: 320,
  paletteGrouping: "module",
  layoutDirection: "right",
  layoutLayerGap: 120,
  layoutNodeGap: 48,
  nodeHeight: 160,
  cascadeStep: 40,
  autosave: false,
  autosaveMs: 30000,
  deleteKeys: ["Delete", "Backspace"],
  saveKey: "s",
};

/** Bounds a saved value must keep; outside them the default is used. */
export const canvasLimits = {
  minZoom: [0.05, 1],
  maxZoom: [1, 8],
  fitPadding: [0, 1],
  gridSize: [2, 200],
  nodeWidth: [120, 800],
  selfGap: [40, 2000],
  layoutLayerGap: [0, 1000],
  layoutNodeGap: [0, 1000],
  nodeHeight: [40, 1200],
  cascadeStep: [4, 400],
  autosaveMs: [2000, 3600000],
} as const satisfies Partial<Record<keyof CanvasSettings, readonly [number, number]>>;

const GROUPINGS: Grouping[] = ["module", "state", "none"];

/** Saved settings as far as they are valid, the defaults for the rest. */
export function sanitizeCanvasSettings(saved: unknown): CanvasSettings {
  const input = saved && typeof saved === "object" ? saved as Record<string, unknown> : {};
  const result: CanvasSettings = { ...canvasDefaults, deleteKeys: [...canvasDefaults.deleteKeys] };
  for (const [key, [low, high]] of Object.entries(canvasLimits) as [keyof typeof canvasLimits, readonly [number, number]][]) {
    const value = input[key];
    if (typeof value === "number" && Number.isFinite(value) && value >= low && value <= high) result[key] = value;
  }
  if (result.minZoom > result.maxZoom) { result.minZoom = canvasDefaults.minZoom; result.maxZoom = canvasDefaults.maxZoom; }
  for (const key of ["snapToGrid", "autosave"] as const) if (typeof input[key] === "boolean") result[key] = input[key] as boolean;
  if (GROUPINGS.includes(input.paletteGrouping as Grouping)) result.paletteGrouping = input.paletteGrouping as Grouping;
  if (LAYOUT_DIRECTIONS.includes(input.layoutDirection as LayoutDirection)) result.layoutDirection = input.layoutDirection as LayoutDirection;
  if (Array.isArray(input.deleteKeys) && input.deleteKeys.every((key) => typeof key === "string" && key)) result.deleteKeys = [...input.deleteKeys as string[]];
  if (typeof input.saveKey === "string" && input.saveKey.length === 1) result.saveKey = input.saveKey.toLowerCase();
  return result;
}

interface CanvasSettingsState extends CanvasSettings {
  set: (patch: Partial<CanvasSettings>) => void;
  reset: () => void;
}

export const useCanvasSettings = create<CanvasSettingsState>()(persist((set) => ({
  ...canvasDefaults,
  set: (patch) => set((state) => sanitizeCanvasSettings({ ...state, ...patch })),
  reset: () => set(sanitizeCanvasSettings({})),
}), {
  name: "regulus-canvas-settings",
  partialize: ({ set: _set, reset: _reset, ...settings }) => settings,
  merge: (saved, current) => ({ ...current, ...sanitizeCanvasSettings(saved) }),
}));
