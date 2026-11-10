// The Regulus Code view's calls beyond those of Operate and Templates, over the shared
// JSON-RPC line. `code.inspect` is the read-only method of the module that offers
// `code.workspace@1` (the same call `regulus code inspect` makes); the workers are the
// delegate's own methods (`regulus workers ls|close`); the rest belongs to the core.
import type { Manifest, ProjectSummary } from "@/composition/model";
import type { Rpc } from "@/operate/client";
import { errorText, unavailableOf } from "@/operate/logic";
import { readDiff, readFile, readListing, readSearch, readSummary, readWorkers } from "./logic";
import type { InspectAction } from "./model";

export const codeMethods = {
  inspect: "code.inspect",
  workers: "delegation.workers",
  workerClose: "delegation.close",
  manifests: "module.manifest",
  secretList: "secret.list",
  projectList: "project.list",
} as const;

/** The module answered, with something this app cannot read as the answer of that action. */
export class UnreadableAnswer extends Error {
  constructor(readonly action: string) { super(`unreadable answer to ${action}`); }
}

/**
 * Why a call of the view failed, as the view says it: the module absent or unusable (a
 * first-level state, never a generic error), an answer it cannot read, or the core's words.
 */
export type CallFailure =
  | { kind: "not-installed" | "unavailable"; reason: string }
  | { kind: "unreadable"; action: string }
  | { kind: "refused"; reason: string };

export function callFailure(error: unknown): CallFailure {
  const missing = unavailableOf(error);
  if (missing) return { kind: missing, reason: errorText(error) };
  if (error instanceof UnreadableAnswer) return { kind: "unreadable", action: error.action };
  return { kind: "refused", reason: errorText(error) };
}

function expectAnswer<T>(action: string, answer: T | null): T {
  if (answer === null) throw new UnreadableAnswer(action);
  return answer;
}

export function codeApi(rpc: Rpc) {
  const inspect = (executionId: string, nodeId: string, action: InspectAction, extra: Record<string, unknown> = {}) =>
    rpc.request<unknown>(codeMethods.inspect, { execution_id: executionId, node_id: nodeId, action, ...extra });
  return {
    /** The folder, its git state and the tools the node offers. */
    summary: async (executionId: string, nodeId: string) => expectAnswer("summary", readSummary(await inspect(executionId, nodeId, "summary"))),
    /** What sits in a folder of the workspace; empty `path` is the root. */
    list: async (executionId: string, nodeId: string, path: string) => expectAnswer("list", readListing(await inspect(executionId, nodeId, "list", path ? { path } : {}))),
    read: async (executionId: string, nodeId: string, path: string) => expectAnswer("read", readFile(await inspect(executionId, nodeId, "read", { path }))),
    /** Lines that hold `query`, under `path` when given. */
    search: async (executionId: string, nodeId: string, query: string, path = "") =>
      expectAnswer("search", readSearch(await inspect(executionId, nodeId, "search", path ? { query, path } : { query }))),
    /** The working tree against its last commit, as one unified diff. */
    diff: async (executionId: string, nodeId: string) => expectAnswer("diff", readDiff(await inspect(executionId, nodeId, "diff"))),
    /** `regulus workers ls <execution> <node>`. */
    workers: async (executionId: string, nodeId: string) => readWorkers(await rpc.request<unknown>(codeMethods.workers, { execution_id: executionId, node_id: nodeId })),
    /** `regulus workers close <execution> <node> <worker>`: stops the worker's execution. */
    closeWorker: (executionId: string, nodeId: string, worker: string) =>
      rpc.request<unknown>(codeMethods.workerClose, { execution_id: executionId, node_id: nodeId, worker }),
    /** Every manifest: who offers what, and which methods each module declares. */
    manifests: () => rpc.request<Manifest[]>(codeMethods.manifests, {}),
    /** `regulus secret ls`: names only. */
    secrets: () => rpc.request<string[]>(codeMethods.secretList),
    /** `regulus project ls`: the open projects, whose folders the setup form offers. */
    projects: () => rpc.request<ProjectSummary[]>(codeMethods.projectList),
  };
}

export type CodeApi = ReturnType<typeof codeApi>;
