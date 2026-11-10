// What a person can tune in the Regulus Code view, and what the view remembers on this
// device. Same layers as Operate (docs/04-client.md §11): the person's change here, else
// `[client.code]` of config.toml as the core serves it, else the default. How often the
// conversation is asked for, page sizes and timeouts are Operate's settings: this view
// reuses them, it does not copy them.
import { create } from "zustand";
import { persist } from "zustand/middleware";
import { clientValue, useClientConfig, type ClientSettings } from "@/core/clientConfig";
import { normalizeRoot, secretPrefix, setupVars } from "./logic";
import { defaultCodeLayout, readCodeLayout, type CodePanelEntry } from "./panels";

/** Numbers of the view, with their limits. */
export const codeLimits = {
  /** How often the workspace is asked for while nothing happens; it is also asked when the agent finishes a tool or a turn. */
  inspectRefreshMs: { default: 4000, min: 1000, max: 120000 },
  /** Share of the session's width the workspace pane takes, in percent. */
  workspaceWidth: { default: 50, min: 25, max: 75 },
  /** Folders the setup form remembers. */
  recentRoots: { default: 6, min: 0, max: 30 },
  /** Characters a search needs before it is sent. */
  searchMinChars: { default: 2, min: 1, max: 20 },
} as const;

/** Texts of the view, with the longest each may be. */
export const codeTexts = {
  /** What the view is called in the app. Empty: the name in the translation file. */
  label: { default: "", max: 40 },
  /** The template a new session is made from. */
  template: { default: "regulus-code", max: 120 },
  /** What a new profile id starts with, before the folder's name. */
  profilePrefix: { default: "code", max: 40 },
} as const;

export type CodeNumberKey = keyof typeof codeLimits;
export type CodeTextKey = keyof typeof codeTexts;
export type CodeNumbers = Record<CodeNumberKey, number>;
export type CodeTexts = Record<CodeTextKey, string>;
export type CodeValues = CodeNumbers & CodeTexts;
export type CodeSettingKey = CodeNumberKey | CodeTextKey;

const numberKeys = Object.keys(codeLimits) as CodeNumberKey[];
const textKeys = Object.keys(codeTexts) as CodeTextKey[];

export const codeDefaults = {
  ...Object.fromEntries(numberKeys.map((key) => [key, codeLimits[key].default])),
  ...Object.fromEntries(textKeys.map((key) => [key, codeTexts[key].default])),
} as CodeValues;

/** Where `config.client` sets each value: `[client.code]` of config.toml or of a project's regulus.toml. */
export const codeClientKeys: Record<CodeSettingKey, string> = {
  inspectRefreshMs: "code.inspect_refresh_ms",
  workspaceWidth: "code.workspace_width",
  recentRoots: "code.recent_roots",
  searchMinChars: "code.search_min_chars",
  label: "code.label",
  template: "code.template",
  profilePrefix: "code.profile_prefix",
};
/** Where `config.client` sets the order and visibility of the workspace panels. */
export const codePanelsKey = "code.panels";

function numberWithin(key: CodeNumberKey, raw: unknown): number | undefined {
  if (raw === undefined || raw === null || raw === "" || typeof raw === "boolean") return undefined;
  const value = Number(raw);
  if (!Number.isFinite(value)) return undefined;
  const { min, max } = codeLimits[key];
  return Math.min(max, Math.max(min, Math.round(value)));
}

const textWithin = (key: CodeTextKey, raw: unknown): string | undefined =>
  typeof raw === "string" ? raw.trim().slice(0, codeTexts[key].max) : undefined;

/** The values `saved` holds that are readable, inside their limits; nothing else. */
export function sanitizeCodeOverrides(saved: unknown): Partial<CodeValues> {
  const source = saved && typeof saved === "object" ? saved as Record<string, unknown> : {};
  const out: Partial<CodeValues> = {};
  for (const key of numberKeys) { const value = numberWithin(key, source[key]); if (value !== undefined) out[key] = value; }
  for (const key of textKeys) { const value = textWithin(key, source[key]); if (value !== undefined) out[key] = value; }
  return out;
}

/** What `config.client` sets, inside the limits, and the file each value came from. */
export function configuredCode({ client, sources }: Pick<ClientSettings, "client" | "sources">) {
  const raw = Object.fromEntries((Object.entries(codeClientKeys) as [CodeSettingKey, string][]).map(([key, path]) => [key, clientValue(client, path)]));
  const values = sanitizeCodeOverrides(raw);
  const from = Object.fromEntries((Object.keys(values) as CodeSettingKey[]).map((key) => [key, sources[codeClientKeys[key]] ?? "config.toml"])) as Partial<Record<CodeSettingKey, string>>;
  return { values, from };
}

/** What each value is: the person's change on this device, else config.toml, else the default. */
export const effectiveCode = (overrides: Partial<CodeValues>, configured: Partial<CodeValues>): CodeValues =>
  ({ ...codeDefaults, ...configured, ...overrides });

export const effectiveCodePanels = (own: CodePanelEntry[] | null, configured: CodePanelEntry[] | null): CodePanelEntry[] => own ?? configured ?? defaultCodeLayout();

/** `root` first in the remembered folders, without a repeat, at most `keep` of them. */
export function rememberRootIn(recent: string[], root: string, keep: number): string[] {
  const folder = normalizeRoot(root);
  if (!folder) return recent.slice(0, keep);
  return [folder, ...recent.filter((item) => normalizeRoot(item) !== folder)].slice(0, keep);
}

/**
 * The form values worth keeping for the next session: how the provider was set. A
 * credential is kept only as a reference to a secret, never as whatever was typed.
 */
export function rememberableVars(values: Record<string, string>): Record<string, string> {
  return Object.fromEntries(setupVars.provider.flatMap((name) => {
    const value = (values[name] ?? "").trim();
    if (!value || (name === "credential" && !value.startsWith(secretPrefix))) return [];
    return [[name, value]];
  }));
}

const readStrings = (raw: unknown): string[] => Array.isArray(raw) ? raw.filter((item): item is string => typeof item === "string") : [];
const readTexts = (raw: unknown): Record<string, string> => raw && typeof raw === "object" && !Array.isArray(raw)
  ? Object.fromEntries(Object.entries(raw).filter((entry): entry is [string, string] => typeof entry[1] === "string")) : {};

interface CodeSettingsState extends CodeValues {
  /** What the person changed here. Persisted on this device; it wins over config.toml. */
  overrides: Partial<CodeValues>;
  /** What `config.client` sets, and the file each value came from. */
  configured: Partial<CodeValues>;
  configuredFrom: Partial<Record<CodeSettingKey, string>>;
  /** The panel order the person arranged here; null: config.toml's or the default. Persisted. */
  ownPanels: CodePanelEntry[] | null;
  configuredPanels: CodePanelEntry[] | null;
  panelsFrom: string | null;
  /** What applies now (`effectiveCodePanels`). */
  panels: CodePanelEntry[];
  setPanels: (panels: CodePanelEntry[]) => void;
  /** Folders used before, newest first. Persisted. */
  recent: string[];
  /** How the provider was set last time, to fill the next form. Persisted; never a secret's value. */
  lastVars: Record<string, string>;
  /** A session started from a folder: remembers the folder and how its provider was set. */
  rememberSetup: (root: string, values: Record<string, string>) => void;
  /** The session on screen, client state: which execution this device looks at. Persisted. */
  executionId: string | null;
  openExecution: (id: string | null) => void;
  /** The workspace panel on screen and whether the pane shows. Persisted. */
  panel: string | null;
  showPanel: (id: string) => void;
  workspaceShown: boolean;
  setWorkspaceShown: (shown: boolean) => void;
  setSetting: (key: CodeSettingKey, value: number | string) => void;
  /** Drops the person's changes: config.toml or the defaults apply again. What is remembered stays. */
  reset: () => void;
  applyClient: (settings: Pick<ClientSettings, "client" | "sources">) => void;
}

export const useCodeSettings = create<CodeSettingsState>()(persist((set, get) => ({
  ...codeDefaults,
  overrides: {},
  configured: {},
  configuredFrom: {},
  ownPanels: null,
  configuredPanels: null,
  panelsFrom: null,
  panels: defaultCodeLayout(),
  setPanels: (panels) => set({ ownPanels: panels, panels }),
  recent: [],
  lastVars: {},
  rememberSetup: (root, values) => set({ recent: rememberRootIn(get().recent, root, get().recentRoots), lastVars: rememberableVars(values) }),
  executionId: null,
  openExecution: (executionId) => set({ executionId }),
  panel: null,
  showPanel: (panel) => set({ panel }),
  workspaceShown: true,
  setWorkspaceShown: (workspaceShown) => set({ workspaceShown }),
  setSetting: (key, value) => {
    const overrides = { ...get().overrides, ...sanitizeCodeOverrides({ [key]: value }) };
    set({ overrides, ...effectiveCode(overrides, get().configured) });
  },
  reset: () => set({ overrides: {}, ...effectiveCode({}, get().configured), ownPanels: null, panels: effectiveCodePanels(null, get().configuredPanels) }),
  applyClient: (settings) => {
    const { values, from } = configuredCode(settings);
    const configuredPanels = readCodeLayout(clientValue(settings.client, codePanelsKey), "hidden");
    set({
      configured: values, configuredFrom: from, ...effectiveCode(get().overrides, values),
      configuredPanels, panelsFrom: configuredPanels ? settings.sources[codePanelsKey] ?? "config.toml" : null, panels: effectiveCodePanels(get().ownPanels, configuredPanels),
    });
  },
}), {
  name: "regulus-code-settings",
  version: 1,
  partialize: (state) => ({
    overrides: state.overrides, ownPanels: state.ownPanels, recent: state.recent, lastVars: state.lastVars,
    executionId: state.executionId, panel: state.panel, workspaceShown: state.workspaceShown,
  }),
  merge: (saved, current): CodeSettingsState => {
    const stored = (saved ?? {}) as Record<string, unknown>;
    const overrides = sanitizeCodeOverrides(stored.overrides);
    // A saved layout gets the panels added since, shown.
    const ownPanels = readCodeLayout(stored.ownPanels, "shown");
    const values = effectiveCode(overrides, current.configured);
    return {
      ...current, overrides, ...values, ownPanels, panels: effectiveCodePanels(ownPanels, current.configuredPanels),
      recent: readStrings(stored.recent).slice(0, values.recentRoots), lastVars: rememberableVars(readTexts(stored.lastVars)),
      executionId: typeof stored.executionId === "string" ? stored.executionId : null,
      panel: typeof stored.panel === "string" ? stored.panel : null,
      workspaceShown: stored.workspaceShown !== false,
    };
  },
}));

// The defaults follow config.toml as the core serves it, whenever it answers.
const followClient = (settings: Pick<ClientSettings, "client" | "sources">) => useCodeSettings.getState().applyClient(settings);
followClient(useClientConfig.getState());
useClientConfig.subscribe(followClient);
