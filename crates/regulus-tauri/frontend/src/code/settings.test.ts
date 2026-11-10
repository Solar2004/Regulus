import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { codeDefaults, configuredCode, effectiveCode, rememberRootIn, rememberableVars, sanitizeCodeOverrides } from "./settings";

describe("values and their layers", () => {
  test("saved values come back inside their limits; anything unreadable drops", () => {
    expect(sanitizeCodeOverrides({ inspectRefreshMs: 10, workspaceWidth: "60", searchMinChars: true, label: "  Leo  ", template: 7, junk: 1 }))
      .toEqual({ inspectRefreshMs: 1000, workspaceWidth: 60, label: "Leo" });
    expect(sanitizeCodeOverrides(null)).toEqual({});
    expect(sanitizeCodeOverrides({ label: "x".repeat(200) }).label).toHaveLength(40);
  });

  test("config.client sets the defaults from [client.code], and says the file", () => {
    const { values, from } = configuredCode({
      client: { code: { inspect_refresh_ms: 9000, label: "Leo", template: "my-code", workspace_width: 200 }, refresh_ms: 500 },
      sources: { "code.label": "regulus.toml", "code.template": "config.toml" },
    });
    expect(values).toEqual({ inspectRefreshMs: 9000, workspaceWidth: 75, label: "Leo", template: "my-code" });
    expect(from).toEqual({ inspectRefreshMs: "config.toml", workspaceWidth: "config.toml", label: "regulus.toml", template: "config.toml" });
  });

  test("the person's change wins over config.toml, which wins over the default", () => {
    const values = effectiveCode({ template: "mine" }, { template: "theirs", profilePrefix: "repo" });
    expect(values.template).toBe("mine");
    expect(values.profilePrefix).toBe("repo");
    expect(values.inspectRefreshMs).toBe(codeDefaults.inspectRefreshMs);
    expect(codeDefaults).toMatchObject({ template: "regulus-code", profilePrefix: "code", label: "" });
  });
});

describe("what the view remembers", () => {
  test("a folder goes first, once, and the list keeps its length", () => {
    expect(rememberRootIn(["/a", "/b", "/c"], "/b/", 3)).toEqual(["/b", "/a", "/c"]);
    expect(rememberRootIn(["/a", "/b", "/c"], "/d", 3)).toEqual(["/d", "/a", "/b"]);
    expect(rememberRootIn(["/a"], "/b", 0)).toEqual([]);
    expect(rememberRootIn(["/a"], "  ", 3)).toEqual(["/a"]);
  });

  test("only how the provider was set is kept, and a credential only as a reference", () => {
    expect(rememberableVars({ root: "/a", preset: " openrouter ", base_url: "", credential: "secret:openrouter", model: "glm", system: "be brief" }))
      .toEqual({ preset: "openrouter", credential: "secret:openrouter", model: "glm" });
    // Something typed that is not a reference is never written to this device.
    expect(rememberableVars({ preset: "ollama", credential: "sk-live-0000" })).toEqual({ preset: "ollama" });
  });
});

describe("on this device", () => {
  let saved: Map<string, string>;
  beforeEach(() => {
    saved = new Map();
    const localStorage = {
      getItem: (key: string) => saved.get(key) ?? null,
      setItem: (key: string, value: string) => { saved.set(key, value); },
      removeItem: (key: string) => { saved.delete(key); },
    };
    vi.stubGlobal("window", { localStorage });
    vi.resetModules();
  });
  afterEach(() => { vi.unstubAllGlobals(); });

  test("what was saved comes back cleaned, and config.toml only fills what the person left alone", async () => {
    saved.set("regulus-code-settings", JSON.stringify({ version: 1, state: {
      overrides: { workspaceWidth: 40, template: "mine", junk: true }, ownPanels: ["changes", "files"], recent: ["/a", 3, "/b"],
      lastVars: { preset: "ollama", credential: "sk-live-0000", system: "x" }, executionId: "exec-7", panel: "changes", workspaceShown: false,
    } }));
    const { useCodeSettings } = await import("./settings");
    const { useClientConfig } = await import("@/core/clientConfig");
    const state = useCodeSettings.getState();
    expect(state.overrides).toEqual({ workspaceWidth: 40, template: "mine" });
    expect(state.panels.map((entry) => entry.id)).toEqual(["changes", "files", "activity", "workers"]);
    expect(state.recent).toEqual(["/a", "/b"]);
    expect(state.lastVars).toEqual({ preset: "ollama" });
    expect([state.executionId, state.panel, state.workspaceShown]).toEqual(["exec-7", "changes", false]);
    useClientConfig.getState().set({ client: { code: { template: "theirs", label: "Leo", panels: ["activity"] } }, sources: { "code.label": "regulus.toml" }, warnings: [] });
    expect(useCodeSettings.getState().template).toBe("mine");
    expect(useCodeSettings.getState().label).toBe("Leo");
    expect(useCodeSettings.getState().configuredFrom.label).toBe("regulus.toml");
    // The person's own panel order stays; reset goes back to the file's.
    expect(useCodeSettings.getState().panels[0].id).toBe("changes");
    useCodeSettings.getState().reset();
    expect(useCodeSettings.getState().template).toBe("theirs");
    expect(useCodeSettings.getState().panels.map((entry) => [entry.id, entry.hidden])).toEqual([["activity", false], ["files", true], ["changes", true], ["workers", true]]);
    expect(useCodeSettings.getState().recent).toEqual(["/a", "/b"]);
  });

  test("starting a session remembers its folder and its provider; a changed setting stays inside its limits", async () => {
    const { useCodeSettings } = await import("./settings");
    useCodeSettings.getState().rememberSetup("/home/ada/app/", { preset: "ollama", model: "qwen3:4b", root: "/home/ada/app/" });
    expect(useCodeSettings.getState().recent).toEqual(["/home/ada/app"]);
    expect(useCodeSettings.getState().lastVars).toEqual({ preset: "ollama", model: "qwen3:4b" });
    useCodeSettings.getState().setSetting("workspaceWidth", 5);
    expect(useCodeSettings.getState().workspaceWidth).toBe(25);
    expect(JSON.parse(saved.get("regulus-code-settings")!).state.overrides).toEqual({ workspaceWidth: 25 });
  });
});
