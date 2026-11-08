import { create } from "zustand";
import { persist } from "zustand/middleware";

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

/** Saved values back inside their limits; anything unreadable takes the default. */
export function sanitizeSettings(saved: unknown): OperateSettings {
  const source = saved && typeof saved === "object" ? saved as Record<string, unknown> : {};
  const out = { ...operateDefaults };
  for (const key of Object.keys(operateLimits) as OperateSettingKey[]) {
    const value = Number(source[key]);
    const { min, max } = operateLimits[key];
    if (source[key] !== undefined && source[key] !== null && source[key] !== "" && Number.isFinite(value)) {
      out[key] = Math.min(max, Math.max(min, Math.round(value)));
    }
  }
  return out;
}

interface OperateSettingsState extends OperateSettings {
  /** The project shown, client state (docs/04-client.md §5); null: all of them. */
  projectId: string | null;
  setSetting: (key: OperateSettingKey, value: number) => void;
  setProject: (projectId: string | null) => void;
  reset: () => void;
}

export const useOperateSettings = create<OperateSettingsState>()(persist((set, get) => ({
  ...operateDefaults,
  projectId: null,
  setSetting: (key, value) => set(sanitizeSettings({ ...get(), [key]: value })),
  setProject: (projectId) => set({ projectId }),
  reset: () => set({ ...operateDefaults }),
}), {
  name: "regulus-operate-settings",
  partialize: (state) => ({ ...Object.fromEntries((Object.keys(operateLimits) as OperateSettingKey[]).map((key) => [key, state[key]])), projectId: state.projectId }),
  merge: (saved, current): OperateSettingsState => {
    const projectId = (saved as { projectId?: unknown } | undefined)?.projectId;
    return { ...current, ...sanitizeSettings(saved), projectId: typeof projectId === "string" ? projectId : null };
  },
}));
