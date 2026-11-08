// App updates (docs/06-stack.md §5.4): the shell reads `[update]` of `config.toml` and runs
// Tauri's updater; this file only asks it and decides what Settings says. The CLI does the
// same with `regulus update`.
import { create } from "zustand";

/** `update_settings`: what the shell read from `[update]`, before any network. */
export interface UpdateSettings {
  current: string;
  /** Empty: updates are off. */
  url: string;
  channel: string;
  check_on_start: boolean;
  has_key: boolean;
  config_path: string;
}

/** `update_check`: what the channel offers. */
export interface UpdateFound {
  current: string;
  available: string | null;
  notes: string | null;
  date: string | null;
}

export type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

/** The shell's commands, one per `regulus update` step. */
export const commands = { settings: "update_settings", check: "update_check", install: "update_install" } as const;

export function updateApi(invoke: Invoke) {
  return {
    settings: () => invoke<UpdateSettings>(commands.settings),
    check: () => invoke<UpdateFound>(commands.check),
    install: () => invoke<void>(commands.install),
  };
}

export type Phase = "idle" | "checking" | "installing";

export interface UpdateState {
  settings: UpdateSettings | null;
  found: UpdateFound | null;
  error: string | null;
  phase: Phase;
}

/** What the Updates row shows: one case each, decided only from the state. */
export type UpdateView =
  | { kind: "browser" }
  | { kind: "loading" }
  | { kind: "off"; current: string; configPath: string }
  | { kind: "noKey"; current: string; configPath: string }
  | { kind: "unchecked"; current: string; channel: string }
  | { kind: "checking"; current: string; channel: string }
  | { kind: "upToDate"; current: string; channel: string }
  | { kind: "available"; current: string; channel: string; version: string; notes: string | null }
  | { kind: "installing"; current: string; version: string }
  | { kind: "failed"; current: string; channel: string; reason: string };

export function updateView(inShell: boolean, state: UpdateState): UpdateView {
  if (!inShell) return { kind: "browser" };
  const { settings, found, error, phase } = state;
  if (!settings) return error ? { kind: "failed", current: "?", channel: "?", reason: error } : { kind: "loading" };
  const { current, channel } = settings;
  if (!settings.url) return { kind: "off", current, configPath: settings.config_path };
  if (!settings.has_key) return { kind: "noKey", current, configPath: settings.config_path };
  if (phase === "installing" && found?.available) return { kind: "installing", current, version: found.available };
  if (phase === "checking") return { kind: "checking", current, channel };
  if (error) return { kind: "failed", current, channel, reason: error };
  if (!found) return { kind: "unchecked", current, channel };
  if (found.available) return { kind: "available", current, channel, version: found.available, notes: found.notes };
  return { kind: "upToDate", current, channel };
}

/** Whether the app should check by itself now: `check_on_start`, with somewhere to look. */
export function shouldCheckOnStart(settings: UpdateSettings | null): boolean {
  return !!settings && settings.check_on_start && !!settings.url && settings.has_key;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

interface UpdateStore extends UpdateState {
  /** Reads `[update]`; with `onStart`, also checks when `check_on_start` says so. */
  load: (api: ReturnType<typeof updateApi>, onStart?: boolean) => Promise<void>;
  check: (api: ReturnType<typeof updateApi>) => Promise<void>;
  install: (api: ReturnType<typeof updateApi>) => Promise<void>;
}

/** One state for the app: a check made at start shows in Settings. */
export const useUpdates = create<UpdateStore>()((set, get) => ({
  settings: null,
  found: null,
  error: null,
  phase: "idle",
  load: async (api, onStart = false) => {
    try {
      const settings = await api.settings();
      set({ settings, error: null });
      if (onStart && shouldCheckOnStart(settings)) await get().check(api);
    } catch (error) { set({ error: message(error) }); }
  },
  check: async (api) => {
    if (get().phase !== "idle") return;
    set({ phase: "checking", error: null });
    try { set({ found: await api.check(), phase: "idle" }); }
    catch (error) { set({ error: message(error), phase: "idle" }); }
  },
  install: async (api) => {
    if (get().phase !== "idle" || !get().found?.available) return;
    set({ phase: "installing", error: null });
    // On success the shell restarts the app: nothing after this runs.
    try { await api.install(); }
    catch (error) { set({ error: message(error), phase: "idle" }); }
  },
}));
