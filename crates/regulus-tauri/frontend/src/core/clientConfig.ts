import { useEffect } from "react";
import { create } from "zustand";
import { useCore } from "./connection";

/** `config.client` (docs/04-client.md §11): `[client]` of `config.toml` with the project's own on top. */
export interface ClientSettings {
  /** Only what is written; anything missing takes the app's own default. */
  client: Record<string, unknown>;
  /** The file each value came from, by `key` or `table.key`. */
  sources: Record<string, string>;
  warnings: string[];
}

interface ClientConfigState extends ClientSettings {
  /** How many answers arrived since the app started; 0 until the first one. */
  answers: number;
  set: (settings: ClientSettings) => void;
}

const empty: ClientSettings = { client: {}, sources: {}, warnings: [] };

/** The last answer of `config.client`, for every view. Not persisted: config.toml is the source. */
export const useClientConfig = create<ClientConfigState>()((set) => ({
  ...empty,
  answers: 0,
  set: (settings) => set((state) => ({ ...settings, answers: state.answers + 1 })),
}));

/** A `config.client` answer as the app reads it; anything unreadable counts as empty. */
export function readClientSettings(answer: unknown): ClientSettings {
  const source = answer && typeof answer === "object" ? answer as Record<string, unknown> : {};
  const object = (value: unknown) => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
  const sources = Object.fromEntries(Object.entries(object(source.sources)).filter(([, file]) => typeof file === "string")) as Record<string, string>;
  const warnings = Array.isArray(source.warnings) ? source.warnings.filter((w): w is string => typeof w === "string") : [];
  return { client: object(source.client), sources, warnings };
}

/** `name` or `table.name` of a `[client]` table, if it is there. */
export function clientValue(client: Record<string, unknown>, path: string): unknown {
  return path.split(".").reduce<unknown>((at, part) => at && typeof at === "object" ? (at as Record<string, unknown>)[part] : undefined, client);
}

/** Asks `config.client` once connected and again when `projectId` changes. A project the
 * core does not have open falls back to the global table; a core without the method (older
 * than protocol 1.13) leaves it empty: the app's defaults apply. */
export function useClientConfigSync(projectId: string | null) {
  const { client, status } = useCore();
  useEffect(() => {
    if (!client || status !== "ready") return;
    let disposed = false;
    const global = () => client.request("config.client", {});
    (projectId ? client.request("config.client", { project_id: projectId }).catch(global) : global())
      .then((answer) => readClientSettings(answer), () => empty)
      .then((settings) => { if (!disposed) useClientConfig.getState().set(settings); });
    return () => { disposed = true; };
  }, [client, status, projectId]);
}
