import { expect, test } from "vitest";
import { clientValue, readClientSettings } from "./clientConfig";
import { startViewOf, useRegulusUi } from "@/store/useRegulusUi";

test("an answer of config.client is read defensively", () => {
  expect(readClientSettings(null)).toEqual({ client: {}, sources: {}, warnings: [] });
  expect(readClientSettings({ client: { refresh_ms: 250 }, sources: { refresh_ms: "config.toml", bad: 1 }, warnings: ["w", 2] }))
    .toEqual({ client: { refresh_ms: 250 }, sources: { refresh_ms: "config.toml" }, warnings: ["w"] });
});

test("values are found by key or table.key", () => {
  const client = { refresh_ms: 250, tui: { event_page: 50 } };
  expect(clientValue(client, "refresh_ms")).toBe(250);
  expect(clientValue(client, "tui.event_page")).toBe(50);
  expect(clientValue(client, "tui.value_max")).toBeUndefined();
  expect(clientValue(client, "refresh_ms.x")).toBeUndefined();
});

test("start_view names a view; anything else keeps the app's start page", () => {
  expect(startViewOf("canvas")).toBe("composition");
  expect(startViewOf("operate")).toBe("operate");
  expect(startViewOf("toString")).toBeNull();
  expect(startViewOf("nowhere")).toBeNull();
  expect(startViewOf(3)).toBeNull();
});

test("start_view opens the app once, unless the person chose a start page or moved", () => {
  const ui = useRegulusUi;
  ui.setState({ view: "home", startPage: "home", startPageChosen: false });
  ui.getState().applyStartView("nowhere");
  expect(ui.getState().view).toBe("home");
  ui.getState().applyStartView("operate");
  expect(ui.getState().view).toBe("operate");
  ui.setState({ view: "home", startPage: "home", startPageChosen: true });
  ui.getState().applyStartView("operate");
  expect(ui.getState().view).toBe("home");
  ui.setState({ view: "settings", startPage: "home", startPageChosen: false });
  ui.getState().applyStartView("operate");
  expect(ui.getState().view).toBe("settings");
});
