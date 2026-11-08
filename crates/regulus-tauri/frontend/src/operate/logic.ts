// Pure logic of the Operate view: what a node is, which first-level state it is in,
// how bus events merge and how values print. No React, no socket: tested in logic.test.ts.
import type { CompositionNode, Profile, Workflow, Port } from "@/composition/model";
import type { AgentTurn, BusEvent, Execution, ExecutionState, ModuleSummary } from "./model";

/** A node is known by what it takes, never by its module (docs/04-client.md §7). */
export const capabilities = { agent: "agent.engine", channel: "participant" } as const;

/** JSON-RPC codes that mean "the module behind this method is not there". */
export const rpcCodes = { methodNotFound: -32601, moduleUnavailable: -32002 } as const;

export type NodeKind = "agent" | "channel" | "other";

const baseCapability = (capability: string) => capability.split("@")[0];
const baseModule = (id: string) => id.split("@")[0];

export function hasInput(node: Pick<CompositionNode, "ports">, capability: string): boolean {
  return node.ports.some((port) => port.direction === "input" && baseCapability(port.capability) === capability);
}

export function nodeKind(node: Pick<CompositionNode, "ports">): NodeKind {
  if (hasInput(node, capabilities.agent)) return "agent";
  if (hasInput(node, capabilities.channel)) return "channel";
  return "other";
}

export function isLive(state: ExecutionState): boolean {
  return state.status === "starting" || state.status === "running";
}

/** The module a node names: the exact id first, then the same module at any version. */
export function findModule(modules: ModuleSummary[], moduleId: string): ModuleSummary | undefined {
  return modules.find((module) => module.id === moduleId) ??
    modules.find((module) => baseModule(module.id) === baseModule(moduleId));
}

/** First-level states (docs/04-client.md §3), plus what a running execution adds. */
export type NodeState =
  | { kind: "resource" }
  | { kind: "not-installed" }
  | { kind: "disabled"; missing: string[] }
  | { kind: "broken"; reason: string }
  | { kind: "incompatible"; requires: string; have: string }
  | { kind: "unused" }
  | { kind: "running" }
  | { kind: "stopped" };

export function nodeState(node: Pick<CompositionNode, "id" | "module_id">, execution: Execution, modules: ModuleSummary[]): NodeState {
  if (!node.module_id) return { kind: "resource" };
  const module = findModule(modules, node.module_id);
  if (!module) return { kind: "not-installed" };
  const disabled = execution.disabled?.find((item) => item.node_id === node.id);
  if (disabled) return { kind: "disabled", missing: disabled.missing };
  // An older mock lists modules without a state: read it as installed and never used.
  const state: ModuleSummary["state"] = module.state ?? { status: "idle", last_used: null };
  // Blocked reads as broken: it does not run, and the reason says why.
  if (state.status === "broken" || state.status === "blocked") return { kind: "broken", reason: state.reason };
  if (state.status === "incompatible") return { kind: "incompatible", requires: state.requires, have: state.have };
  if (state.status === "disabled") return { kind: "disabled", missing: state.missing };
  if (!isLive(execution.state)) return { kind: "stopped" };
  if (state.status === "idle" && state.last_used === null) return { kind: "unused" };
  return { kind: "running" };
}

export interface NodeRow {
  id: string;
  name: string;
  type: string;
  kind: NodeKind;
  state: NodeState;
}

/** The workflow an execution runs, from the profile it names. */
export function workflowOf(profiles: Profile[], execution: Execution): Workflow | undefined {
  return profiles.find((profile) => profile.id === execution.profile_id)?.workflows.find((workflow) => workflow.id === execution.workflow_id);
}

export function nodeRows(workflow: Workflow | undefined, execution: Execution, modules: ModuleSummary[]): NodeRow[] {
  return (workflow?.nodes ?? []).map((node) => ({
    id: node.id, name: node.name || node.id, type: node.resource || node.type,
    kind: nodeKind(node), state: nodeState(node, execution, modules),
  }));
}

/** Prompts and messages only reach a node of a live execution whose module answers. */
export function canTalk(row: NodeRow): boolean {
  return row.kind !== "other" && (row.state.kind === "running" || row.state.kind === "unused");
}

/** The workflow's own inputs: where `workflow.send` puts a value (protocol 1.7). */
export function inputPorts(workflow: Workflow | undefined): Port[] {
  return (workflow?.ports ?? []).filter((port) => port.direction === "input");
}

/** Workflows a profile can run: the main one first, then the extra ones. */
export function runnableWorkflows(profile: Profile): Workflow[] {
  const runnable = profile.workflows.filter((workflow) => workflow.kind !== "module");
  return [...runnable.filter((workflow) => workflow.id === profile.main_workflow_id), ...runnable.filter((workflow) => workflow.id !== profile.main_workflow_id)];
}

/** Project ids the profiles name, for the client-side filter (docs/04-client.md §5). */
export function projectsOf(profiles: { project_id: string | null }[]): string[] {
  return [...new Set(profiles.flatMap((profile) => profile.project_id ? [profile.project_id] : []))].sort();
}

/** Adds a page to what is shown: no duplicates, in `seq` order, the newest `keep` only. */
export function mergeEvents(current: BusEvent[], incoming: BusEvent[], keep: number): BusEvent[] {
  if (!incoming.length) return current;
  const bySeq = new Map<number, BusEvent>();
  for (const event of current) bySeq.set(event.seq, event);
  for (const event of incoming) bySeq.set(event.seq, event);
  const merged = [...bySeq.values()].sort((a, b) => a.seq - b.seq);
  return keep > 0 && merged.length > keep ? merged.slice(merged.length - keep) : merged;
}

/** Whether an event left from or went to a node. */
export function eventTouches(event: BusEvent, nodeId: string): boolean {
  if (event.from.node_id === nodeId) return true;
  return (event.kind === "delivered" || event.kind === "failed") && event.to.node_id === nodeId;
}

export const portName = (ref: { node_id: string; port_id: string }) => `${ref.node_id}.${ref.port_id}`;

export interface RenderedValue {
  /** One line, cut at the limit. */
  short: string;
  /** All of it: text as text, anything else as indented JSON. */
  full: string;
  /** The line was cut. */
  truncated: boolean;
  /** There is more to see than the line: cut, or JSON that indents. */
  expandable: boolean;
  json: boolean;
}

/** How a value on the bus prints: text as text, the rest as JSON, cut at `max` characters. */
export function renderValue(value: unknown, max: number): RenderedValue {
  const json = typeof value !== "string";
  const compact = json ? JSON.stringify(value) ?? String(value) : value;
  const full = json ? JSON.stringify(value, null, 2) ?? String(value) : value;
  const line = compact.replace(/\s*\n\s*/g, " ⏎ ");
  const truncated = max > 0 && line.length > max;
  return { short: truncated ? `${line.slice(0, max)}…` : line, full, truncated, expandable: truncated || full !== line, json };
}

export type SendValue = { ok: true; value: unknown } | { ok: false };

/** What `workflow.send` carries: a `text@` port takes the text, any other JSON. */
export function parseSendValue(capability: string, raw: string): SendValue {
  if (baseCapability(capability) === "text") return { ok: true, value: raw };
  try { return { ok: true, value: JSON.parse(raw) }; } catch { return { ok: false }; }
}

/** The agent's answer and, when the turn did not end normally, why (as the TUI shows it). */
export function agentAnswer(turn: AgentTurn): { text: string; stopReason: string | null } {
  const reason = turn.stop_reason && turn.stop_reason !== "end_turn" ? turn.stop_reason : null;
  return { text: turn.text ?? "", stopReason: reason };
}

export type Unavailable = "not-installed" | "unavailable";

/** A module method that failed because its module is absent or not usable, never a generic error. */
export function unavailableOf(error: unknown): Unavailable | null {
  const code = (error as { code?: unknown } | null)?.code;
  if (code === rpcCodes.methodNotFound) return "not-installed";
  if (code === rpcCodes.moduleUnavailable) return "unavailable";
  return null;
}

export const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);

/** Fills `{name}` holes of a translated text. */
export function fill(text: string, values: Record<string, string | number>): string {
  return text.replace(/\{(\w+)\}/g, (hole, name: string) => name in values ? String(values[name]) : hole);
}
