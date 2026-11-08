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

/** JSON-RPC codes `fetchClientSettings` tells apart. */
const NO_PROJECT = -32010;
const NO_METHOD = -32601;

const codeOf = (error: unknown) => error && typeof error === "object" && "code" in error ? (error as { code: unknown }).code : undefined;

type Request = (method: string, params: Record<string, unknown>) => Promise<unknown>;

/** One `config.client` round. A project the core does not have open (`-32010`) falls back
 * to the global table; a core without the method (`-32601`, older than protocol 1.13)
 * gives nothing, so the app's defaults apply; any other failure gives `null`: keep what
 * there was, the next connection asks again. */
export async function fetchClientSettings(request: Request, projectId: string | null): Promise<ClientSettings | null> {
  const ask = async (params: Record<string, unknown>) => readClientSettings(await request("config.client", params));
  try {
    if (projectId === null) return await ask({});
    try {
      return await ask({ project_id: projectId });
    } catch (error) {
      if (codeOf(error) !== NO_PROJECT) throw error;
      return await ask({});
    }
  } catch (error) {
    return codeOf(error) === NO_METHOD ? { ...empty } : null;
  }
}

/** Asks `config.client` once connected, again on every new connection (the core may have
 * restarted with another config.toml) and when `projectId` changes. */
export function useClientConfigSync(projectId: string | null) {
  const { client, status } = useCore();
  useEffect(() => {
    if (!client || status !== "ready") return;
    let disposed = false;
    fetchClientSettings((method, params) => client.request(method, params), projectId)
      .then((settings) => { if (settings && !disposed) useClientConfig.getState().set(settings); });
    return () => { disposed = true; };
  }, [client, status, projectId]);
}
