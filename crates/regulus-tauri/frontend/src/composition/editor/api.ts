// The core methods the canvas calls, each the same RPC a CLI command uses
// (`regulus profile save`, `regulus check`, `regulus run`, `regulus profile rm`).
import type { Diagnostic, Execution, Manifest, Profile, ProfileSaved } from "../model";

/** Anything that sends one JSON-RPC request: the shared `RpcClient`, or a test double. */
export interface Requester {
  request<T>(method: string, params?: Record<string, unknown>): Promise<T>;
}

export const loadManifests = (rpc: Requester) => rpc.request<Manifest[]>("module.manifest", {});
export const getProfile = (rpc: Requester, id: string) => rpc.request<Profile>("profile.get", { id });
/** A new profile is written without `force`, so it never replaces one that exists. */
export const saveProfile = (rpc: Requester, profile: Profile, isNew: boolean) =>
  rpc.request<ProfileSaved>("profile.save", isNew ? { profile } : { profile, force: true });
export const checkProfile = (rpc: Requester, id: string) => rpc.request<Diagnostic[]>("profile.check", { id });
export const startExecution = (rpc: Requester, profileId: string, workflowId?: string) =>
  rpc.request<Execution>("execution.start", workflowId ? { profile_id: profileId, workflow_id: workflowId } : { profile_id: profileId });
export const deleteProfile = (rpc: Requester, id: string) => rpc.request<{ profile_id: string; path: string }>("profile.delete", { id });

/** The message as the core sent it. */
export const messageOf = (error: unknown) => error instanceof Error ? error.message : String(error);

/** Diagnostics worst first. */
const RANK = { error: 0, warning: 1, info: 2 } as const;
export const bySeverity = (diagnostics: Diagnostic[]) => [...diagnostics].sort((a, b) => RANK[a.severity] - RANK[b.severity]);
