import { afterEach, beforeEach, expect, test, vi } from "vitest";

// What this device saved before: the stores are imported after `localStorage` holds it.
let saved: Map<string, string>;

beforeEach(() => {
  saved = new Map();
  const localStorage = {
    getItem: (key: string) => saved.get(key) ?? null,
    setItem: (key: string, value: string) => { saved.set(key, value); },
    removeItem: (key: string) => { saved.delete(key); },
  };
  // zustand's persist reads `window.localStorage`; tests run without a browser.
  vi.stubGlobal("window", { localStorage });
  vi.resetModules();
});

afterEach(() => { vi.unstubAllGlobals(); });

test("settings saved before overrides existed are kept; only values away from the default become changes", async () => {
  saved.set("regulus-operate-settings", JSON.stringify({
    state: { refreshMs: 2000, eventPageSize: 200, eventKeep: 1000, maxValueLength: 240, startTimeoutMs: 120000, agentTimeoutMs: 600000, projectId: "demo" },
    version: 0,
  }));
  const { useOperateSettings } = await import("./settings");
  const { useClientConfig } = await import("@/core/clientConfig");
  expect(useOperateSettings.getState().projectId).toBe("demo");
  expect(useOperateSettings.getState().overrides).toEqual({ refreshMs: 2000 });
  // The person's change > config.toml > the default.
  useClientConfig.getState().set({ client: { refresh_ms: 250, tui: { event_page: 50 } }, sources: {}, warnings: [] });
  expect(useOperateSettings.getState().refreshMs).toBe(2000);
  expect(useOperateSettings.getState().eventPageSize).toBe(50);
  expect(useOperateSettings.getState().eventKeep).toBe(1000);
});

test("settings saved with overrides come back as they were", async () => {
  saved.set("regulus-operate-settings", JSON.stringify({ state: { overrides: { eventKeep: 300 }, projectId: null }, version: 1 }));
  const { useOperateSettings } = await import("./settings");
  expect(useOperateSettings.getState().overrides).toEqual({ eventKeep: 300 });
  expect(useOperateSettings.getState().eventKeep).toBe(300);
});

test("a non-default start page saved before startPageChosen existed counts as chosen, so start_view does not move it", async () => {
  saved.set("regulus-shell-preferences", JSON.stringify({ state: { startPage: "composition", density: "comfortable" }, version: 0 }));
  const { useRegulusUi } = await import("@/store/useRegulusUi");
  expect(useRegulusUi.getState().startPageChosen).toBe(true);
  useRegulusUi.getState().applyStartView("operate");
  expect(useRegulusUi.getState().view).toBe("composition");
});

test("the default start page saved before startPageChosen existed does not count as chosen", async () => {
  saved.set("regulus-shell-preferences", JSON.stringify({ state: { startPage: "home", density: "comfortable" }, version: 0 }));
  const { useRegulusUi } = await import("@/store/useRegulusUi");
  expect(useRegulusUi.getState().startPageChosen).toBe(false);
  useRegulusUi.getState().applyStartView("operate");
  expect(useRegulusUi.getState().view).toBe("operate");
});

test("with nothing saved, start_view opens the app", async () => {
  const { useRegulusUi } = await import("@/store/useRegulusUi");
  expect(useRegulusUi.getState().startPageChosen).toBe(false);
  useRegulusUi.getState().applyStartView("operate");
  expect(useRegulusUi.getState().view).toBe("operate");
});

test("the panel layout: the person's saved one wins over config.toml's, which wins over the default", async () => {
  saved.set("regulus-operate-settings", JSON.stringify({ state: { overrides: {}, projectId: null, ownPanels: [{ id: "events", place: "aside", hidden: false }, "bogus"] }, version: 1 }));
  const { useOperateSettings } = await import("./settings");
  const { useClientConfig } = await import("@/core/clientConfig");
  const state = () => useOperateSettings.getState();
  // Saved before the other panels existed: they come back, shown.
  expect(state().panels[0]).toEqual({ id: "events", place: "aside", hidden: false });
  expect(state().panels.every((entry) => !entry.hidden)).toBe(true);
  useClientConfig.getState().set({ client: { operate: { panels: ["tree", "events"] } }, sources: { "operate.panels": "/p/regulus.toml" }, warnings: [] });
  expect(state().configuredPanels?.filter((entry) => !entry.hidden).map((entry) => entry.id)).toEqual(["tree", "events"]);
  expect(state().panelsFrom).toBe("/p/regulus.toml");
  expect(state().panels[0].id).toBe("events");
  state().reset();
  expect(state().panels.filter((entry) => !entry.hidden).map((entry) => entry.id)).toEqual(["tree", "events"]);
  expect(JSON.parse(saved.get("regulus-operate-settings")!).state.ownPanels).toBeNull();
  useClientConfig.getState().set({ client: {}, sources: {}, warnings: [] });
  expect(state().panels.map((entry) => entry.id)).toEqual(["nodes", "send", "events", "tree", "node", "decisions"]);
});
