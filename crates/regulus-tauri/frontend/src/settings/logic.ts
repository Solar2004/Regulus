// Pure logic of Settings: secret names, key scopes and policies, the service log and the
// first-level state of each module. No React, no socket: tested in logic.test.ts.
import type { ModuleDefinition } from "@/composition/model";
import type { KeyRecord, LogEntry, Policy } from "./model";

// --- secrets --------------------------------------------------------------------------

/** The names the core accepts (`regulus_core::secrets::valid_name`). */
const secretName = /^[A-Za-z0-9._-]+$/;

export type SecretNameProblem = "empty" | "invalid";

export function secretNameProblem(name: string): SecretNameProblem | null {
  if (!name) return "empty";
  return secretName.test(name) ? null : "invalid";
}

/** Names in a stable order, whatever order the store keeps them in. */
export const sortNames = (names: string[]) => [...names].sort((a, b) => a.localeCompare(b));

// --- keys -----------------------------------------------------------------------------

/** Scope entries the `service` module accepts: `<project>[/<workflow>]`, `*` for any. */
export function scopeEntryValid(entry: string): boolean {
  const [project, workflow, ...rest] = entry.split("/");
  if (rest.length) return false;
  return project !== "" && (workflow === undefined || workflow !== "");
}

export type ScopeParse = { ok: true; scope: string[] } | { ok: false; problem: "empty" } | { ok: false; problem: "invalid"; bad: string[] };

/** A scope typed as one line: entries split by commas or spaces, each once. */
export function parseScope(text: string): ScopeParse {
  const scope = [...new Set(text.split(/[\s,]+/).filter(Boolean))];
  if (!scope.length) return { ok: false, problem: "empty" };
  const bad = scope.filter((entry) => !scopeEntryValid(entry));
  return bad.length ? { ok: false, problem: "invalid", bad } : { ok: true, scope };
}

/** The limits of a policy, in the order the CLI names them. */
export const policyFields = ["cooldown_ms", "requests", "window_ms", "concurrency", "budget_requests", "budget_ms"] as const satisfies readonly (keyof Policy)[];
export type PolicyField = (typeof policyFields)[number];

export const emptyPolicy: Policy = { cooldown_ms: 0, requests: 0, window_ms: 0, concurrency: 0, budget_requests: 0, budget_ms: 0 };

export type PolicyParse = { ok: true; policy: Policy } | { ok: false; problem: "number"; field: PolicyField } | { ok: false; problem: "window" };

/**
 * A policy from its form: empty is 0 (no limit), anything else a whole number ≥ 0.
 * `requests` counts per `window_ms`: one without the other limits nothing, so it is refused.
 */
export function parsePolicy(raw: Partial<Record<PolicyField, string>>): PolicyParse {
  const policy = { ...emptyPolicy };
  for (const field of policyFields) {
    const text = (raw[field] ?? "").trim();
    if (!text) continue;
    if (!/^\d+$/.test(text) || !Number.isSafeInteger(Number(text))) return { ok: false, problem: "number", field };
    policy[field] = Number(text);
  }
  if ((policy.requests > 0) !== (policy.window_ms > 0)) return { ok: false, problem: "window" };
  return { ok: true, policy };
}

/** The limits a policy sets, without the zeros. */
export function policyLimits(policy: Partial<Policy> | undefined): { field: PolicyField; value: number }[] {
  return policyFields.flatMap((field) => policy?.[field] ? [{ field, value: policy[field]! }] : []);
}

export const isRevoked = (key: Pick<KeyRecord, "revoked_ms">) => key.revoked_ms !== undefined && key.revoked_ms !== null;

/** Live keys first, then revoked; the newest first within each. */
export function sortKeys(keys: KeyRecord[]): KeyRecord[] {
  return [...keys].sort((a, b) => Number(isRevoked(a)) - Number(isRevoked(b)) || b.created_ms - a.created_ms || a.id.localeCompare(b.id));
}

// --- log ------------------------------------------------------------------------------

export type StatusClass = "ok" | "refused" | "failed";
export type StatusFilter = "all" | StatusClass;

/** 2xx answered, 4xx refused at the door, 5xx failed behind it. */
export function statusClass(status: number): StatusClass {
  if (status >= 500) return "failed";
  if (status >= 400) return "refused";
  return "ok";
}

/** What the table shows: newest first, of the chosen class. */
export function logRows(entries: LogEntry[], filter: StatusFilter): LogEntry[] {
  return entries.filter((entry) => filter === "all" || statusClass(entry.status) === filter).reverse();
}

/** `last` of the log form: empty is all, anything else must be a whole number above 0. */
export function lastValid(text: string): boolean {
  const trimmed = text.trim();
  return !trimmed || (/^\d+$/.test(trimmed) && Number.isSafeInteger(Number(trimmed)) && Number(trimmed) > 0);
}

/** `service.log` filters from the form: blanks are no filter, `last` a whole number > 0. */
export function logQuery(form: { last: string; key: string; workflow: string }): { last?: number; key?: string; workflow?: string } {
  const query: { last?: number; key?: string; workflow?: string } = {};
  const last = Number(form.last.trim());
  if (form.last.trim() && Number.isSafeInteger(last) && last > 0) query.last = last;
  if (form.key.trim()) query.key = form.key.trim();
  if (form.workflow.trim()) query.workflow = form.workflow.trim();
  return query;
}

// --- modules --------------------------------------------------------------------------

/** First-level state of a module (docs/04-client.md §3), with what explains it. */
export type ModuleStatus =
  | { kind: "active"; since: number }
  | { kind: "unused" }
  | { kind: "idle"; lastUsed: number }
  | { kind: "disabled"; missing: string[] }
  | { kind: "broken"; reason: string }
  | { kind: "incompatible"; requires: string; have: string };

/** A module with no state (an older mock) reads as installed and never used. */
export function moduleStatus(module: ModuleDefinition): ModuleStatus {
  const state = module.state ?? { status: "idle", last_used: null };
  switch (state.status) {
    case "active": return { kind: "active", since: state.since };
    case "idle": return state.last_used === null || state.last_used === undefined ? { kind: "unused" } : { kind: "idle", lastUsed: state.last_used };
    case "disabled": return { kind: "disabled", missing: state.missing };
    case "broken":
    case "blocked": return { kind: "broken", reason: state.reason };
    case "incompatible": return { kind: "incompatible", requires: state.requires, have: state.have };
    default: return { kind: "broken", reason: `unknown state \`${(state as { status: string }).status}\`` };
  }
}

/** Order of attention: what does not work first, then what was never used, then the rest. */
const attention: ModuleStatus["kind"][] = ["broken", "incompatible", "disabled", "unused", "idle", "active"];

export interface ModuleRow {
  id: string;
  name: string;
  nodeTypes: string[];
  status: ModuleStatus;
}

export function moduleRows(modules: ModuleDefinition[]): ModuleRow[] {
  return modules
    .map((module) => ({ id: module.id, name: module.name || module.id, nodeTypes: module.node_types, status: moduleStatus(module) }))
    .sort((a, b) => attention.indexOf(a.status.kind) - attention.indexOf(b.status.kind) || a.name.localeCompare(b.name));
}

/** How many modules are in each state, for the line above the list. */
export function statusCounts(rows: ModuleRow[]): Partial<Record<ModuleStatus["kind"], number>> {
  const counts: Partial<Record<ModuleStatus["kind"], number>> = {};
  for (const row of rows) counts[row.status.kind] = (counts[row.status.kind] ?? 0) + 1;
  return counts;
}

/** Milliseconds since the epoch as this device writes a date and time. */
export const formatTime = (ms: number) => new Date(ms).toLocaleString();
