import { describe, expect, test } from "vitest";
import { canvasDefaults, sanitizeCanvasSettings } from "./settings";

describe("canvas settings", () => {
  test("nothing saved gives the defaults", () => {
    expect(sanitizeCanvasSettings(undefined)).toEqual(canvasDefaults);
    expect(sanitizeCanvasSettings("junk")).toEqual(canvasDefaults);
  });
  test("valid values are kept", () => {
    const saved = { minZoom: 0.3, maxZoom: 3, snapToGrid: true, gridSize: 24, nodeWidth: 300, paletteGrouping: "state", autosave: true, autosaveMs: 5000, deleteKeys: ["Delete"], saveKey: "S" };
    expect(sanitizeCanvasSettings(saved)).toMatchObject({ ...saved, saveKey: "s" });
  });
  test("out of range, wrong type or unknown values fall back to the default", () => {
    const result = sanitizeCanvasSettings({ minZoom: -1, maxZoom: "big", gridSize: 1, autosaveMs: 10, paletteGrouping: "color", deleteKeys: [1], saveKey: "ctrl+s", snapToGrid: "yes" });
    expect(result).toEqual(canvasDefaults);
  });
  test("a minimum zoom above the maximum resets both", () => {
    expect(sanitizeCanvasSettings({ minZoom: 1, maxZoom: 1 })).toMatchObject({ minZoom: 1, maxZoom: 1 });
    const result = sanitizeCanvasSettings({ minZoom: 0.9, maxZoom: 1, fitPadding: 0.5 });
    expect(result).toMatchObject({ minZoom: 0.9, maxZoom: 1, fitPadding: 0.5 });
  });
  test("the arrange preferences are kept when valid and defaulted when not", () => {
    expect(sanitizeCanvasSettings({ layoutDirection: "down", layoutLayerGap: 200, layoutNodeGap: 0, nodeHeight: 220 }))
      .toMatchObject({ layoutDirection: "down", layoutLayerGap: 200, layoutNodeGap: 0, nodeHeight: 220 });
    expect(sanitizeCanvasSettings({ layoutDirection: "diagonal", layoutLayerGap: -5, layoutNodeGap: 5000, nodeHeight: 1 }))
      .toMatchObject({ layoutDirection: canvasDefaults.layoutDirection, layoutLayerGap: canvasDefaults.layoutLayerGap, layoutNodeGap: canvasDefaults.layoutNodeGap, nodeHeight: canvasDefaults.nodeHeight });
  });
});
