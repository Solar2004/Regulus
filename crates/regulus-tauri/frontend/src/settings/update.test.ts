import { describe, expect, test, vi } from "vitest";
import { commands, shouldCheckOnStart, updateApi, updateView, useUpdates, type UpdateSettings, type UpdateState } from "./update";

const settings: UpdateSettings = { current: "0.1.0", url: "https://releases.example/regulus", channel: "stable", check_on_start: false, has_key: true, config_path: "/home/a/.regulus/config.toml" };
const state = (over: Partial<UpdateState> = {}): UpdateState => ({ settings, found: null, error: null, phase: "idle", ...over });

describe("update view", () => {
  test("outside the desktop shell there is nothing to update", () => {
    expect(updateView(false, state()).kind).toBe("browser");
  });
  test("an empty url is off and says which file to edit", () => {
    expect(updateView(true, state({ settings: { ...settings, url: "" } }))).toEqual({ kind: "off", current: "0.1.0", configPath: settings.config_path });
  });
  test("without a release key nothing can be installed", () => {
    expect(updateView(true, state({ settings: { ...settings, has_key: false } })).kind).toBe("noKey");
  });
  test("shows current and available versions", () => {
    expect(updateView(true, state()).kind).toBe("unchecked");
    expect(updateView(true, state({ phase: "checking" })).kind).toBe("checking");
    expect(updateView(true, state({ found: { current: "0.1.0", available: null, notes: null, date: null } }))).toEqual({ kind: "upToDate", current: "0.1.0", channel: "stable" });
    expect(updateView(true, state({ found: { current: "0.1.0", available: "0.2.0", notes: "Faster", date: null } }))).toEqual({ kind: "available", current: "0.1.0", channel: "stable", version: "0.2.0", notes: "Faster" });
    expect(updateView(true, state({ phase: "installing", found: { current: "0.1.0", available: "0.2.0", notes: null, date: null } })).kind).toBe("installing");
  });
  test("a failed check keeps the reason the shell gave", () => {
    expect(updateView(true, state({ error: "signature mismatch" }))).toMatchObject({ kind: "failed", reason: "signature mismatch" });
    expect(updateView(true, state({ settings: null, error: "no config" }))).toMatchObject({ kind: "failed", reason: "no config" });
    expect(updateView(true, state({ settings: null })).kind).toBe("loading");
  });
});

describe("check on start", () => {
  test("only when asked and there is somewhere to look", () => {
    expect(shouldCheckOnStart(settings)).toBe(false);
    expect(shouldCheckOnStart({ ...settings, check_on_start: true })).toBe(true);
    expect(shouldCheckOnStart({ ...settings, check_on_start: true, url: "" })).toBe(false);
    expect(shouldCheckOnStart({ ...settings, check_on_start: true, has_key: false })).toBe(false);
    expect(shouldCheckOnStart(null)).toBe(false);
  });
  test("load with onStart checks through the shell's commands", async () => {
    const invoke = vi.fn(async (command: string) => command === commands.settings ? { ...settings, check_on_start: true } : { current: "0.1.0", available: "0.2.0", notes: null, date: null });
    const api = updateApi(invoke as never);
    await useUpdates.getState().load(api, true);
    expect(invoke.mock.calls.map((c) => c[0])).toEqual([commands.settings, commands.check]);
    expect(useUpdates.getState().found?.available).toBe("0.2.0");
    await useUpdates.getState().load(api);
    expect(invoke).toHaveBeenCalledTimes(3);
  });
  test("install asks the shell only when there is something newer", async () => {
    const invoke = vi.fn(async () => { throw new Error("download failed"); });
    useUpdates.setState({ found: { current: "0.1.0", available: null, notes: null, date: null }, phase: "idle", error: null });
    await useUpdates.getState().install(updateApi(invoke as never));
    expect(invoke).not.toHaveBeenCalled();
    useUpdates.setState({ found: { current: "0.1.0", available: "0.2.0", notes: null, date: null } });
    await useUpdates.getState().install(updateApi(invoke as never));
    expect(invoke).toHaveBeenCalledWith(commands.install);
    expect(useUpdates.getState()).toMatchObject({ phase: "idle", error: "download failed" });
  });
});
