// The core methods for writing node modules, each the RPC a CLI command also calls
// (protocol 1.17, RUN/API.md): `regulus module shape`, `regulus profile sync`,
// `regulus project reload`, `regulus source get|save|check`, `regulus node init`.
import { RpcError } from "@/core/connection";
import type { Requester } from "../editor/api";
import type { ConfigValue, ModuleInUse, NodeCreated, NodeShape, Profile, ProfileSynced, ProjectReloaded, ProjectSource, ProjectSummary, SourceCheck, SourceFile, SourceSaved } from "../model";

/** Error codes the authoring views tell apart; anything else is shown with the core's message. */
export const CODE = {
  /** The core is older than 1.17: the action is hidden or said to be unavailable. */
  methodNotFound: -32601,
  invalidParams: -32602,
  projectNotFound: -32010,
  moduleInUse: -32014,
  sourceConflict: -32015,
  sourceUnavailable: -32016,
} as const;

/** How long a call that starts or checks a process may take, in ms. */
export interface AuthoringTimeouts { reloadMs: number; checkMs: number }

export const moduleShape = (rpc: Requester, moduleId: string, type: string, config: Record<string, ConfigValue>) =>
  rpc.request<NodeShape>("module.shape", { module_id: moduleId, type, config });

/** A preview: the core returns the synced profile and what it would drop, and writes nothing. */
export const syncProfile = (rpc: Requester, profile: Profile, scope: { workflowId?: string; nodeIds?: string[] } = {}) =>
  rpc.request<ProfileSynced>("profile.sync", {
    profile, ...(scope.workflowId ? { workflow_id: scope.workflowId } : {}), ...(scope.nodeIds ? { node_ids: scope.nodeIds } : {}),
  });

export const reloadProject = (rpc: Requester, params: { id: string; module?: string; stopExecutions?: boolean }, timeouts: AuthoringTimeouts) =>
  rpc.request<ProjectReloaded>("project.reload", {
    id: params.id, ...(params.module ? { module: params.module } : {}), ...(params.stopExecutions ? { stop_executions: true } : {}),
  }, timeouts.reloadMs);

export const getSource = (rpc: Requester, projectId: string, module: string) =>
  rpc.request<SourceFile>("source.get", { project_id: projectId, module });

/** `revision` is what was last read; `null` expects no file. `force` writes over a newer file. */
export const saveSource = (rpc: Requester, params: { projectId: string; module: string; text: string; revision: string | null; force?: boolean }, timeouts: AuthoringTimeouts) =>
  rpc.request<SourceSaved>("source.save", {
    project_id: params.projectId, module: params.module, text: params.text, revision: params.revision, ...(params.force ? { force: true } : {}),
  }, timeouts.checkMs);

/** Checks unsaved text, or the file when `text` is left out. Nothing is imported or run. */
export const checkSource = (rpc: Requester, projectId: string, module: string, text: string | undefined, timeouts: AuthoringTimeouts) =>
  rpc.request<SourceCheck>("source.check", { project_id: projectId, module, ...(text === undefined ? {} : { text }) }, timeouts.checkMs);

export const initNode = (rpc: Requester, projectId: string, id: string, timeouts: AuthoringTimeouts) =>
  rpc.request<NodeCreated>("node.init", { project_id: projectId, id }, timeouts.reloadMs);

export const listProjects = (rpc: Requester) => rpc.request<ProjectSummary[]>("project.list", {});

/** Opens a project folder (`project.open`, the CLI's `regulus project open`), untrusted: nothing in it runs until it is trusted. */
export const openProject = (rpc: Requester, path: string) => rpc.request<ProjectSummary>("project.open", { path });

/** Where a project's local modules keep their editable files (`project.get` → `sources`); empty from a core before 1.17. */
export const projectSources = (rpc: Requester, projectId: string) =>
  rpc.request<{ sources?: ProjectSource[] }>("project.get", { id: projectId }).then((details) => details.sources ?? []);

/** The code of a failed call, if the core gave one. */
export const codeOf = (error: unknown) => error instanceof RpcError ? error.code : null;

/** The live executions in a `-32014` answer, or `null` for any other failure. */
export function inUseOf(error: unknown): ModuleInUse | null {
  if (!(error instanceof RpcError) || error.code !== CODE.moduleInUse) return null;
  const data = error.data as Partial<ModuleInUse> | undefined;
  return { executions: Array.isArray(data?.executions) ? data.executions : [] };
}

/** The file's current revision in a `-32015` answer (`null`: there is no file now), or `undefined` for any other failure. */
export function conflictOf(error: unknown): { revision: string | null; path: string | null } | undefined {
  if (!(error instanceof RpcError) || error.code !== CODE.sourceConflict) return undefined;
  const data = (error.data ?? {}) as { revision?: unknown; path?: unknown };
  return { revision: typeof data.revision === "string" ? data.revision : null, path: typeof data.path === "string" ? data.path : null };
}
