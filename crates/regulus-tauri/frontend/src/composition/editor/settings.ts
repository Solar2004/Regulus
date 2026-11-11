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
  /** A code field writes its text to the profile this long after the last keystroke (and at once on blur or save), in ms. */
  codeCommitMs: number;
  /** The code editor shows syntax problems this long after the last change, in ms. */
  codeLintMs: number;
  /** Lines a code field shows before it scrolls, and at least. */
  codeMaxLines: number;
  codeMinLines: number;
  /** A module source is checked by the core this long after typing pauses, in ms; 0 checks only on save or on request. */
  sourceCheckMs: number;
  /** A node's shape is asked for again this long after a field it depends on changes, in ms. */
  shapeDelayMs: number;
  /** How long a reload or a new node module may take to start, in ms. */
  reloadTimeoutMs: number;
  /** How long a source check or save may take, in ms. */
  checkTimeoutMs: number;
  /** Spaces Tab inserts in a module source. */
  sourceIndent: number;
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
  codeCommitMs: 400,
  codeLintMs: 300,
  codeMaxLines: 18,
  codeMinLines: 4,
  sourceCheckMs: 1500,
  shapeDelayMs: 250,
  reloadTimeoutMs: 60000,
  checkTimeoutMs: 15000,
  sourceIndent: 4,
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
  codeCommitMs: [0, 10000],
  codeLintMs: [0, 10000],
  codeMaxLines: [4, 200],
  codeMinLines: [1, 50],
  sourceCheckMs: [0, 60000],
  shapeDelayMs: [0, 5000],
  reloadTimeoutMs: [1000, 600000],
  checkTimeoutMs: [1000, 120000],
  sourceIndent: [1, 8],
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
  if (result.codeMinLines > result.codeMaxLines) { result.codeMinLines = canvasDefaults.codeMinLines; result.codeMaxLines = canvasDefaults.codeMaxLines; }
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
