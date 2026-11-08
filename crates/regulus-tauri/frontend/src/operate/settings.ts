import { create } from "zustand";
import { persist } from "zustand/middleware";
import { clientValue, useClientConfig, type ClientSettings } from "@/core/clientConfig";

/** What a person can tune in the Operate view, with its limits. Persisted on this device. */
export const operateLimits = {
  /** How often executions, modules and events are asked for (the TUI's `client.refresh_ms`). */
  refreshMs: { default: 1000, min: 250, max: 60000 },
  /** `limit` of one `event.list` page. */
  eventPageSize: { default: 200, min: 10, max: 5000 },
  /** Events kept on screen per execution; older ones drop off. */
  eventKeep: { default: 1000, min: 50, max: 20000 },
  /** Characters of a value shown before "show more". */
  maxValueLength: { default: 240, min: 20, max: 20000 },
  /** How long `execution.start` may take: engines start their CLI before it answers. */
  startTimeoutMs: { default: 120000, min: 5000, max: 600000 },
  /** How long an `agent.prompt` turn may take. */
  agentTimeoutMs: { default: 600000, min: 5000, max: 3600000 },
} as const;

export type OperateSettingKey = keyof typeof operateLimits;
export type OperateSettings = Record<OperateSettingKey, number>;

export const operateDefaults = Object.fromEntries(
  Object.entries(operateLimits).map(([key, limit]) => [key, limit.default]),
) as OperateSettings;

/** Where `config.client` (`[client]` of config.toml) sets each setting's default. */
export const clientKeys: Partial<Record<OperateSettingKey, string>> = {
  refreshMs: "refresh_ms",
  eventPageSize: "tui.event_page",
  eventKeep: "tui.event_keep",
  maxValueLength: "tui.value_max",
};

/** One value inside its limits, or nothing if it is not a number. */
function within(key: OperateSettingKey, raw: unknown): number | undefined {
  if (raw === undefined || raw === null || raw === "" || typeof raw === "boolean") return undefined;
  const value = Number(raw);
  if (!Number.isFinite(value)) return undefined;
  const { min, max } = operateLimits[key];
  return Math.min(max, Math.max(min, Math.round(value)));
}

/** Saved values back inside their limits; anything unreadable takes the default. */
export function sanitizeSettings(saved: unknown): OperateSettings {
  return { ...operateDefaults, ...sanitizeOverrides(saved) };
}

/** The settings `saved` holds that are readable, inside their limits; nothing else. */
export function sanitizeOverrides(saved: unknown): Partial<OperateSettings> {
  const source = saved && typeof saved === "object" ? saved as Record<string, unknown> : {};
  const out: Partial<OperateSettings> = {};
  for (const key of Object.keys(operateLimits) as OperateSettingKey[]) {
    const value = within(key, source[key]);
    if (value !== undefined) out[key] = value;
  }
  return out;
}

/** What `config.client` sets, inside the limits, and the file each value came from. */
export function configuredSettings({ client, sources }: Pick<ClientSettings, "client" | "sources">) {
  const values: Partial<OperateSettings> = {};
  const from: Partial<Record<OperateSettingKey, string>> = {};
  for (const [key, path] of Object.entries(clientKeys) as [OperateSettingKey, string][]) {
    const value = within(key, clientValue(client, path));
    if (value === undefined) continue;
    values[key] = value;
    from[key] = sources[path] ?? "config.toml";
  }
  return { values, from };
}

/** What each setting is: the person's change on this device, else config.toml, else the default. */
export function effectiveSettings(overrides: Partial<OperateSettings>, configured: Partial<OperateSettings>): OperateSettings {
  return { ...operateDefaults, ...configured, ...overrides };
}

interface OperateSettingsState extends OperateSettings {
  /** What the person changed here. Persisted on this device; it wins over config.toml. */
  overrides: Partial<OperateSettings>;
  /** What `config.client` sets, and the file each value came from. */
  configured: Partial<OperateSettings>;
  configuredFrom: Partial<Record<OperateSettingKey, string>>;
  /** The project shown, client state (docs/04-client.md §5); null: all of them. */
  projectId: string | null;
  setSetting: (key: OperateSettingKey, value: number) => void;
  setProject: (projectId: string | null) => void;
  /** Drops the person's changes: config.toml or the defaults apply again. */
  reset: () => void;
  applyClient: (settings: Pick<ClientSettings, "client" | "sources">) => void;
}

export const useOperateSettings = create<OperateSettingsState>()(persist((set, get) => ({
  ...operateDefaults,
  overrides: {},
  configured: {},
  configuredFrom: {},
  projectId: null,
  setSetting: (key, value) => {
    const overrides = { ...get().overrides, ...sanitizeOverrides({ [key]: value }) };
    set({ overrides, ...effectiveSettings(overrides, get().configured) });
  },
  setProject: (projectId) => set({ projectId }),
  reset: () => set({ overrides: {}, ...effectiveSettings({}, get().configured) }),
  applyClient: (settings) => {
    const { values, from } = configuredSettings(settings);
    set({ configured: values, configuredFrom: from, ...effectiveSettings(get().overrides, values) });
  },
}), {
  name: "regulus-operate-settings",
  version: 1,
  partialize: (state) => ({ overrides: state.overrides, projectId: state.projectId }),
  merge: (saved, current): OperateSettingsState => {
    const stored = (saved ?? {}) as { overrides?: unknown; projectId?: unknown };
    // Before version 1 every setting was saved; only the ones away from the default were changes.
    const legacy = Object.fromEntries(Object.entries(sanitizeOverrides(saved)).filter(([key, value]) => operateDefaults[key as OperateSettingKey] !== value));
    const overrides = stored.overrides !== undefined ? sanitizeOverrides(stored.overrides) : legacy;
    const projectId = typeof stored.projectId === "string" ? stored.projectId : null;
    return { ...current, overrides, projectId, ...effectiveSettings(overrides, current.configured) };
  },
}));

// The defaults follow config.toml as the core serves it, whenever it answers.
const followClient = (settings: Pick<ClientSettings, "client" | "sources">) => useOperateSettings.getState().applyClient(settings);
followClient(useClientConfig.getState());
useClientConfig.subscribe(followClient);
