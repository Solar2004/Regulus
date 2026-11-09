import { expect, test } from "vitest";
import { configuredSettings, effectiveSettings, operateDefaults, operateLimits, sanitizeSettings, useOperateSettings } from "./settings";
import { useClientConfig } from "@/core/clientConfig";

test("defaults match the TUI refresh and the documented values", () => {
  expect(operateDefaults).toEqual({ refreshMs: 1000, eventPageSize: 200, eventKeep: 1000, maxValueLength: 240, startTimeoutMs: 120000, agentTimeoutMs: 600000, conversationKeep: 5000 });
});

test("saved values are clamped, rounded, and unreadable ones take the default", () => {
  expect(sanitizeSettings(undefined)).toEqual(operateDefaults);
  expect(sanitizeSettings({ refreshMs: 1, eventPageSize: "50.6", maxValueLength: "x", eventKeep: null })).toEqual({
    ...operateDefaults, refreshMs: operateLimits.refreshMs.min, eventPageSize: 51,
  });
  expect(sanitizeSettings({ agentTimeoutMs: 1e12 }).agentTimeoutMs).toBe(operateLimits.agentTimeoutMs.max);
});

test("config.client sets defaults from refresh_ms and tui.*, inside the limits, with their file", () => {
  const { values, from } = configuredSettings({
    client: { refresh_ms: 250, tui: { event_page: 50, value_max: 1, event_keep: "x" }, start_view: "operate" },
    sources: { refresh_ms: "regulus.toml", "tui.event_page": "config.toml" },
  });
  expect(values).toEqual({ refreshMs: 250, eventPageSize: 50, maxValueLength: operateLimits.maxValueLength.min });
  expect(from).toEqual({ refreshMs: "regulus.toml", eventPageSize: "config.toml", maxValueLength: "config.toml" });
});

test("a change made in the app wins over config.toml, which wins over the defaults", () => {
  expect(effectiveSettings({ refreshMs: 2000 }, { refreshMs: 250, eventPageSize: 50 })).toEqual({ ...operateDefaults, refreshMs: 2000, eventPageSize: 50 });
});

test("the store follows config.client until the person changes a value, and reset goes back to it", () => {
  useClientConfig.getState().set({ client: { refresh_ms: 250, tui: { event_page: 50 } }, sources: { refresh_ms: "config.toml", "tui.event_page": "config.toml" }, warnings: [] });
  expect(useOperateSettings.getState().refreshMs).toBe(250);
  expect(useOperateSettings.getState().configuredFrom.refreshMs).toBe("config.toml");
  useOperateSettings.getState().setSetting("refreshMs", 3000);
  expect(useOperateSettings.getState().refreshMs).toBe(3000);
  expect(useOperateSettings.getState().eventPageSize).toBe(50);
  useOperateSettings.getState().reset();
  expect(useOperateSettings.getState().refreshMs).toBe(250);
  useClientConfig.getState().set({ client: {}, sources: {}, warnings: [] });
  expect(useOperateSettings.getState().refreshMs).toBe(operateDefaults.refreshMs);
});
