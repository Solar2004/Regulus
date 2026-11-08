// The Settings calls, over any JSON-RPC line with `request`. Each is the call a CLI
// command makes: `regulus secret …` and `regulus service …`.
import type { Rpc } from "@/operate/client";
import type { ProjectSummary } from "@/composition/model";
import type { KeyRecord, LogEntry, LogQuery, NewKey, Policy, ServiceEndpoint } from "./model";

/** Method names: the core's (`secret.*`) and the `service` module's. */
export const methods = {
  secretList: "secret.list",
  secretSet: "secret.set",
  secretDelete: "secret.delete",
  keyList: "service.key.list",
  keyNew: "service.key.new",
  keyRevoke: "service.key.revoke",
  log: "service.log",
  endpoints: "service.endpoints",
  projectList: "project.list",
  projectTrust: "project.trust",
} as const;

export function settingsApi(rpc: Rpc) {
  return {
    /** `regulus secret ls`: names only, the core never hands out a value. */
    secrets: () => rpc.request<string[]>(methods.secretList),
    /** `regulus secret set <name>`. */
    setSecret: (name: string, value: string) => rpc.request<null>(methods.secretSet, { name, value }),
    /** `regulus secret rm <name>`. */
    deleteSecret: (name: string) => rpc.request<null>(methods.secretDelete, { name }),
    /** `regulus service key list`. */
    keys: () => rpc.request<KeyRecord[]>(methods.keyList),
    /** `regulus service key new`: the answer carries the key in clear, once. */
    newKey: (name: string, scope: string[], policy: Policy) => rpc.request<NewKey>(methods.keyNew, { name, scope, policy }),
    /** `regulus service key revoke <id>`. */
    revokeKey: (id: string) => rpc.request<KeyRecord>(methods.keyRevoke, { id }),
    /** `regulus service log [--last] [--key] [--workflow]`: oldest first. */
    log: (query: LogQuery) => rpc.request<LogEntry[]>(methods.log, { ...query }),
    /** `regulus service endpoints`. */
    endpoints: () => rpc.request<ServiceEndpoint[]>(methods.endpoints),
    /** `regulus project ls`: the open projects, with the folder `project.trust` takes. */
    projects: () => rpc.request<ProjectSummary[]>(methods.projectList),
    /** `regulus project trust <path>`: its local modules start right away (proto 1.14). */
    trustProject: (path: string) => rpc.request<{ path: string; trusted: boolean }>(methods.projectTrust, { path }),
  };
}

export type SettingsApi = ReturnType<typeof settingsApi>;
