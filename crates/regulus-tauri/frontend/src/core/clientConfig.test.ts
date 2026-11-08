import { expect, test } from "vitest";
import { clientValue, fetchClientSettings, readClientSettings } from "./clientConfig";
import { RpcError } from "./connection";
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

const answer = (client: Record<string, unknown>) => ({ client, sources: {}, warnings: [] });

test("a project the core has not open falls back to the global table", async () => {
  const asked: unknown[] = [];
  const request = async (_: string, params: Record<string, unknown>) => {
    asked.push(params);
    if (params.project_id) throw new RpcError("no open project `gone`", -32010);
    return answer({ refresh_ms: 250 });
  };
  expect(await fetchClientSettings(request, "gone")).toEqual(answer({ refresh_ms: 250 }));
  expect(asked).toEqual([{ project_id: "gone" }, {}]);
});

test("any other failure keeps what there was; a core without the method gives the defaults", async () => {
  const failing = async () => { throw new Error("Connection closed"); };
  expect(await fetchClientSettings(failing, "demo")).toBeNull();
  expect(await fetchClientSettings(failing, null)).toBeNull();
  const old = async () => { throw new RpcError("Method not found", -32601); };
  expect(await fetchClientSettings(old, "demo")).toEqual({ client: {}, sources: {}, warnings: [] });
  expect(await fetchClientSettings(old, null)).toEqual({ client: {}, sources: {}, warnings: [] });
});
