import { expect, test } from "vitest";
import { operateDefaults, operateLimits, sanitizeSettings } from "./settings";

test("defaults match the TUI refresh and the documented values", () => {
  expect(operateDefaults).toEqual({ refreshMs: 1000, eventPageSize: 200, eventKeep: 1000, maxValueLength: 240, startTimeoutMs: 120000, agentTimeoutMs: 600000 });
});

test("saved values are clamped, rounded, and unreadable ones take the default", () => {
  expect(sanitizeSettings(undefined)).toEqual(operateDefaults);
  expect(sanitizeSettings({ refreshMs: 1, eventPageSize: "50.6", maxValueLength: "x", eventKeep: null })).toEqual({
    ...operateDefaults, refreshMs: operateLimits.refreshMs.min, eventPageSize: 51,
  });
  expect(sanitizeSettings({ agentTimeoutMs: 1e12 }).agentTimeoutMs).toBe(operateLimits.agentTimeoutMs.max);
});
