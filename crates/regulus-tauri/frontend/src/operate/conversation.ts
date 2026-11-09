// Pure logic of a conversation in the Operate view (docs/04-client.md §8): what can be
// talked to, the binding a person chose for a workflow, and how an agent's history or a
// workflow's bus events reduce to turns with their activity cards. No React, no socket:
// tested in conversation.test.ts.
import type { Port, Workflow } from "@/composition/model";
import type { AgentHistory, BusEvent, PendingDecision, PermissionOption, SessionEvent } from "./model";
import { capabilities, hasInput } from "./logic";

/** Capabilities a conversation is read from, by name without version: never a module id. */
export const conversationCapabilities = { text: "text", activity: "agent.event" } as const;

const base = (capability: string) => capability.split("@")[0];
const isObject = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
const str = (value: unknown): string | null => typeof value === "string" ? value : null;
const num = (value: unknown): number => typeof value === "number" && Number.isFinite(value) ? value : 0;

// ── What can be talked to ──────────────────────────────────────────────────────────────

export interface ConversationPorts {
  /** The workflow's own then inputs of `text@`: where `workflow.send` puts a turn. */
  inputs: Port[];
  /** The workflow's own then outputs of `text@`: where replies come out as `self.<port>`. */
  outputs: Port[];
}

export function conversationPorts(workflow: Workflow | undefined): ConversationPorts {
  const text = (workflow?.ports ?? []).filter((port) => port.kind === "then" && base(port.capability) === conversationCapabilities.text);
  return { inputs: text.filter((port) => port.direction === "input"), outputs: text.filter((port) => port.direction === "output") };
}

/** A workflow with text going in and text coming out can be bound as a conversation; it is never bound on its own. */
export function isConversable(workflow: Workflow | undefined): boolean {
  const ports = conversationPorts(workflow);
  return ports.inputs.length > 0 && ports.outputs.length > 0;
}

/** Direct agents: nodes with a `uses` input `agent.engine`. Always talkable, no binding. */
export function agentNodes(workflow: Workflow | undefined) {
  return (workflow?.nodes ?? []).filter((node) => hasInput(node, capabilities.agent));
}

export function hasConversation(workflow: Workflow | undefined): boolean {
  return isConversable(workflow) || agentNodes(workflow).length > 0;
}

/** What the conversation panel talks to. */
export type Subject = { kind: "workflow" } | { kind: "agent"; nodeId: string };

/** Every subject of a workflow: the workflow itself when it is a candidate, then each direct agent. */
export function subjectsOf(workflow: Workflow | undefined): Subject[] {
  return [
    ...(isConversable(workflow) ? [{ kind: "workflow" as const }] : []),
    ...agentNodes(workflow).map((node) => ({ kind: "agent" as const, nodeId: node.id })),
  ];
}

export const sameSubject = (a: Subject | null, b: Subject | null) =>
  !!a && !!b && a.kind === b.kind && (a.kind === "workflow" || a.nodeId === (b as { nodeId: string }).nodeId);

/**
 * The subject shown: the one picked if the workflow still has it, else the selected node if
 * it is an agent, else the workflow when it is bound, else the first subject there is.
 */
export function pickSubject(picked: Subject | null, selectedNodeId: string | null, workflow: Workflow | undefined, bound: boolean): Subject | null {
  const all = subjectsOf(workflow);
  const has = (subject: Subject | null) => all.find((item) => sameSubject(item, subject)) ?? null;
  return has(picked)
    ?? has(selectedNodeId ? { kind: "agent", nodeId: selectedNodeId } : null)
    ?? (bound ? has({ kind: "workflow" }) : null)
    ?? all[0] ?? null;
}

// ── The binding a person chose ─────────────────────────────────────────────────────────

export interface ConversationBinding {
  input: string;
  output: string;
}

/** One entry of `operate.conversations` in `config.client`. */
export interface ConfiguredBinding extends ConversationBinding {
  profile: string;
  workflow: string;
}

export const bindingKey = (profileId: string, workflowId: string) => `${profileId}/${workflowId}`;

const readBinding = (value: unknown): ConversationBinding | null => {
  if (!isObject(value)) return null;
  const input = str(value.input);
  const output = str(value.output);
  return input && output ? { input, output } : null;
};

/** `operate.conversations` as written: `[{ profile, workflow, input, output }]`; anything else drops. */
export function readConfiguredBindings(raw: unknown): ConfiguredBinding[] {
  if (!Array.isArray(raw)) return [];
  return raw.flatMap((item) => {
    const binding = readBinding(item);
    const profile = isObject(item) ? str(item.profile) : null;
    const workflow = isObject(item) ? str(item.workflow) : null;
    return binding && profile && workflow ? [{ profile, workflow, ...binding }] : [];
  });
}

/** The bindings this device saved, `{ "<profile>/<workflow>": { input, output } }`; anything else drops. */
export function readOwnBindings(raw: unknown): Record<string, ConversationBinding> {
  if (!isObject(raw)) return {};
  return Object.fromEntries(Object.entries(raw).flatMap(([key, value]) => {
    const binding = readBinding(value);
    return binding ? [[key, binding]] : [];
  }));
}

export type BindingSource = "own" | "config";

/** Whether both ports of a binding are still candidates of the workflow. */
export function fits(binding: ConversationBinding, workflow: Workflow | undefined): boolean {
  const ports = conversationPorts(workflow);
  return ports.inputs.some((port) => port.id === binding.input) && ports.outputs.some((port) => port.id === binding.output);
}

/** The binding in effect: the person's on this device, else config.toml's; one whose ports are gone does not count. */
export function bindingFor(
  own: Record<string, ConversationBinding>, configured: ConfiguredBinding[], profileId: string, workflow: Workflow | undefined,
): { binding: ConversationBinding; source: BindingSource } | null {
  if (!workflow) return null;
  const mine = own[bindingKey(profileId, workflow.id)];
  if (mine && fits(mine, workflow)) return { binding: mine, source: "own" };
  const fromFile = configured.find((item) => item.profile === profileId && item.workflow === workflow.id);
  if (fromFile && fits(fromFile, workflow)) return { binding: { input: fromFile.input, output: fromFile.output }, source: "config" };
  return null;
}

// ── Records: history and bus events in one shape ───────────────────────────────────────

const knownKinds = new Set(["turn_started", "text", "thought", "tool_call", "tool_update", "plan", "permission_request", "permission_resolved", "turn_ended", "state"]);

const readOptions = (value: unknown): PermissionOption[] | null => Array.isArray(value)
  ? value.flatMap((option) => isObject(option) && str(option.id) !== null ? [{ id: option.id as string, kind: str(option.kind) ?? "", name: str(option.name) ?? (option.id as string) }] : [])
  : null;

/** A `SessionEvent` as the app reads it; a kind it does not know, or one missing a field, reads as `other`. */
export function readSessionEvent(value: unknown): SessionEvent | null {
  if (!isObject(value) || typeof value.kind !== "string") return null;
  const head = { seq: num(value.seq), at: num(value.at), ...(typeof value.turn === "number" ? { turn: value.turn } : {}) };
  const other = (): SessionEvent => ({ ...head, kind: "other", name: value.kind as string, raw: value });
  if (!knownKinds.has(value.kind)) return other();
  switch (value.kind) {
    case "turn_started": return { ...head, kind: "turn_started" };
    case "text": case "thought": {
      const text = str(value.text);
      if (text === null) return other();
      return value.kind === "text" ? { ...head, kind: "text", text } : { ...head, kind: "thought", text };
    }
    case "tool_call": {
      const id = str(value.id);
      if (id === null) return other();
      return { ...head, kind: "tool_call", id, title: str(value.title) ?? id, status: str(value.status) ?? "", ...(str(value.tool_kind) ? { tool_kind: value.tool_kind as string } : {}) };
    }
    case "tool_update": {
      const id = str(value.id);
      if (id === null) return other();
      return { ...head, kind: "tool_update", id, status: str(value.status) ?? "", ...(value.content !== undefined && value.content !== null ? { content: value.content } : {}) };
    }
    case "plan": return { ...head, kind: "plan", entries: value.entries ?? null };
    case "permission_request": {
      const id = str(value.id);
      const options = readOptions(value.options);
      if (id === null || !options) return other();
      return { ...head, kind: "permission_request", id, title: str(value.title) ?? id, options, ...(str(value.tool) ? { tool: value.tool as string } : {}) };
    }
    case "permission_resolved": {
      const id = str(value.id);
      return id === null ? other() : { ...head, kind: "permission_resolved", id, option: str(value.option) };
    }
    case "turn_ended": return { ...head, kind: "turn_ended", stop_reason: str(value.stop_reason) ?? "" };
    default: {
      const state = str(value.state);
      return state === null ? other() : { ...head, kind: "state", state };
    }
  }
}

/** One thing of a conversation, in the order its `seq` gives. */
export type ConversationRecord =
  | { type: "entry"; seq: number; at: number; role: string; text: string; turn: string | null; port?: string }
  | { type: "activity"; seq: number; at: number; turn: string | null; node: string | null; event: SessionEvent };

/** An `agent.history` page as records: entries and activity, one `seq` for both. */
export function historyRecords(history: Pick<AgentHistory, "entries" | "activity">): ConversationRecord[] {
  const records: ConversationRecord[] = [];
  for (const entry of history.entries ?? []) {
    records.push({ type: "entry", seq: entry.seq, at: entry.at, role: entry.role, text: entry.text ?? "", turn: entry.turn ?? null });
  }
  for (const item of history.activity ?? []) {
    const event = readSessionEvent(item.event);
    if (event) records.push({ type: "activity", seq: item.seq, at: item.at, turn: item.turn ?? null, node: null, event });
  }
  return records.sort((a, b) => a.seq - b.seq);
}

/** A bus value as text: text as it is, anything else as JSON. */
export const valueText = (value: unknown) => typeof value === "string" ? value : JSON.stringify(value) ?? String(value);

/**
 * The bus events of a bound workflow as records: what was emitted from `self.<input>` is
 * a person's turn (what `workflow.send` records), from `self.<output>` a reply, and an
 * emitted `agent.event` (an agent box's `activity`) is activity of the node it left.
 */
export function busRecords(events: BusEvent[], binding: ConversationBinding): ConversationRecord[] {
  const records: ConversationRecord[] = [];
  for (const event of events) {
    if (event.kind !== "emitted") continue;
    const { from } = event;
    if (from.node_id === "self" && from.port_id === binding.input) {
      records.push({ type: "entry", seq: event.seq, at: event.at, role: "user", text: valueText(event.value), turn: null, port: from.port_id });
    } else if (from.node_id === "self" && from.port_id === binding.output) {
      records.push({ type: "entry", seq: event.seq, at: event.at, role: "agent", text: valueText(event.value), turn: null, port: from.port_id });
    } else if (base(event.capability) === conversationCapabilities.activity) {
      const activity = readSessionEvent(event.value);
      if (activity) records.push({ type: "activity", seq: event.seq, at: event.at, turn: null, node: from.node_id, event: activity });
    }
  }
  return records;
}

/** Adds a page to what is kept: no duplicates, in `seq` order, the newest `keep` only. */
export function mergeRecords(current: ConversationRecord[], incoming: ConversationRecord[], keep: number): ConversationRecord[] {
  if (!incoming.length) return current;
  const bySeq = new Map<number, ConversationRecord>();
  for (const record of current) bySeq.set(record.seq, record);
  for (const record of incoming) bySeq.set(record.seq, record);
  const merged = [...bySeq.values()].sort((a, b) => a.seq - b.seq);
  return keep > 0 && merged.length > keep ? merged.slice(merged.length - keep) : merged;
}

// ── Turns ──────────────────────────────────────────────────────────────────────────────

/**
 * How records fall into turns. `turn`: by the box's turn id (`agent.history`); a record
 * without one stands alone. `sequence`: each person's turn opens one and what follows
 * joins it until the next (a workflow's bus has no turn ids).
 */
export type Grouping = "turn" | "sequence";

export interface TurnGroup {
  key: string;
  records: ConversationRecord[];
}

export function groupTurns(records: ConversationRecord[], grouping: Grouping): TurnGroup[] {
  const sorted = [...records].sort((a, b) => a.seq - b.seq);
  const groups: TurnGroup[] = [];
  const byKey = new Map<string, TurnGroup>();
  const add = (key: string, record: ConversationRecord) => {
    let group = byKey.get(key);
    if (!group) { group = { key, records: [] }; byKey.set(key, group); groups.push(group); }
    group.records.push(record);
  };
  let current = "start";
  for (const record of sorted) {
    if (grouping === "turn") add(record.turn ?? `seq-${record.seq}`, record);
    else {
      if (record.type === "entry" && record.role === "user") current = `u-${record.seq}`;
      add(current, record);
    }
  }
  return groups;
}

/** One card or bubble of a turn. `node` is the node the activity came from (null: the agent talked to). */
export type TurnItem =
  | { type: "entry"; key: string; seq: number; at: number; role: string; text: string; port?: string }
  | { type: "live"; key: string; seq: number; node: string | null; text: string }
  | { type: "thought"; key: string; seq: number; node: string | null; text: string }
  | { type: "tool"; key: string; seq: number; node: string | null; id: string; title: string; toolKind: string | null; status: string; content: unknown }
  | { type: "plan"; key: string; seq: number; node: string | null; entries: unknown }
  | { type: "permission"; key: string; seq: number; node: string | null; id: string; tool: string | null; title: string; options: PermissionOption[]; resolved: { option: string | null } | null }
  | { type: "ended"; key: string; seq: number; node: string | null; stopReason: string }
  | { type: "state"; key: string; seq: number; node: string | null; state: string }
  | { type: "other"; key: string; seq: number; node: string | null; name: string; raw: unknown };

export interface TurnView {
  key: string;
  items: TurnItem[];
  /** A person's turn with no answer yet (no agent or system entry, no reply). */
  open: boolean;
}

const nodeKey = (node: string | null) => node ?? "";

/**
 * A turn's records as cards: text chunks join into one live bubble per node until an
 * answer arrives (then the answer is the text), consecutive thoughts join, a tool call
 * and its updates are one card, the latest plan replaces the earlier, a permission
 * request shows how it was resolved.
 */
export function reduceTurn(group: TurnGroup): TurnView {
  const items: TurnItem[] = [];
  const find = <T extends TurnItem["type"]>(type: T, node: string | null, id?: string) =>
    items.find((item) => item.type === type && nodeKey((item as { node?: string | null }).node ?? null) === nodeKey(node) && (id === undefined || (item as { id?: string }).id === id)) as Extract<TurnItem, { type: T }> | undefined;
  let asked = false;
  let answered = false;
  for (const record of [...group.records].sort((a, b) => a.seq - b.seq)) {
    if (record.type === "entry") {
      items.push({ type: "entry", key: `entry-${record.seq}`, seq: record.seq, at: record.at, role: record.role, text: record.text, ...(record.port ? { port: record.port } : {}) });
      if (record.role === "user") asked = true; else answered = true;
      continue;
    }
    const { event, node, seq } = record;
    const tag = `${nodeKey(node)}-${seq}`;
    switch (event.kind) {
      case "turn_started": break;
      case "text": {
        const live = find("live", node);
        if (live) live.text += event.text;
        else items.push({ type: "live", key: `live-${tag}`, seq, node, text: event.text });
        break;
      }
      case "thought": {
        const last = [...items].reverse().find((item) => item.type !== "live");
        if (last?.type === "thought" && nodeKey(last.node) === nodeKey(node)) last.text += event.text;
        else items.push({ type: "thought", key: `thought-${tag}`, seq, node, text: event.text });
        break;
      }
      case "tool_call": {
        const tool = find("tool", node, event.id);
        if (tool) { tool.title = event.title; tool.toolKind = event.tool_kind ?? tool.toolKind; tool.status = event.status; }
        else items.push({ type: "tool", key: `tool-${tag}`, seq, node, id: event.id, title: event.title, toolKind: event.tool_kind ?? null, status: event.status, content: null });
        break;
      }
      case "tool_update": {
        const tool = find("tool", node, event.id);
        if (tool) { tool.status = event.status || tool.status; if (event.content !== undefined) tool.content = event.content; }
        else items.push({ type: "tool", key: `tool-${tag}`, seq, node, id: event.id, title: event.id, toolKind: null, status: event.status, content: event.content ?? null });
        break;
      }
      case "plan": {
        const plan = find("plan", node);
        if (plan) plan.entries = event.entries;
        else items.push({ type: "plan", key: `plan-${tag}`, seq, node, entries: event.entries });
        break;
      }
      case "permission_request": {
        const asking = find("permission", node, event.id);
        if (asking) { asking.title = event.title; asking.options = event.options; asking.tool = event.tool ?? asking.tool; }
        else items.push({ type: "permission", key: `permission-${tag}`, seq, node, id: event.id, tool: event.tool ?? null, title: event.title, options: event.options, resolved: null });
        break;
      }
      case "permission_resolved": {
        const asking = find("permission", node, event.id);
        if (asking) asking.resolved = { option: event.option };
        else items.push({ type: "permission", key: `permission-${tag}`, seq, node, id: event.id, tool: null, title: event.id, options: [], resolved: { option: event.option } });
        break;
      }
      case "turn_ended": items.push({ type: "ended", key: `ended-${tag}`, seq, node, stopReason: event.stop_reason }); break;
      case "state": items.push({ type: "state", key: `state-${tag}`, seq, node, state: event.state }); break;
      case "other": items.push({ type: "other", key: `other-${tag}`, seq, node, name: event.name, raw: event.raw }); break;
    }
  }
  // The answer is the text: the stream it came from goes once it is there.
  const shown = answered ? items.filter((item) => item.type !== "live") : items;
  return { key: group.key, items: shown.sort((a, b) => a.seq - b.seq), open: asked && !answered };
}

export const reduceConversation = (records: ConversationRecord[], grouping: Grouping): TurnView[] =>
  groupTurns(records, grouping).map(reduceTurn);

// ── Turns sent and not seen yet ────────────────────────────────────────────────────────

/** A person's turn sent and not in the record yet: `turn` from `agent.send`, `event` from `workflow.send`. */
export interface PendingSend {
  text: string;
  at: number;
  turn?: string;
  event?: number;
}

/** The sends still waiting: one goes once its turn's person entry, or its bus event, is in the record or behind the cursor. */
export function stillPending(pending: PendingSend[], records: ConversationRecord[], cursor: number): PendingSend[] {
  return pending.filter((send) => {
    if (send.turn !== undefined) return !records.some((record) => record.type === "entry" && record.role === "user" && record.turn === send.turn);
    if (send.event !== undefined) return send.event > cursor && !records.some((record) => record.seq === send.event);
    return true;
  });
}

// ── Decisions behind permission cards ──────────────────────────────────────────────────

/** What a decider's question says it is about (`human.list` row of kind `decision`, `value.context`). */
export interface DecisionContext {
  execution_id: string | null;
  node_id: string | null;
  request_id: string | null;
}

export function decisionContext(pending: PendingDecision): DecisionContext | null {
  if (pending.kind !== "decision" || !isObject(pending.value) || !isObject(pending.value.context)) return null;
  const { context } = pending.value;
  return { execution_id: str(context.execution_id), node_id: str(context.node_id), request_id: str(context.request_id) };
}

/** The pending decision a permission request of `nodeId` in `executionId` waits on, if a decider asked a person. */
export function decisionFor(decisions: PendingDecision[], executionId: string, nodeId: string, requestId: string): PendingDecision | undefined {
  return decisions.find((pending) => {
    const context = decisionContext(pending);
    return !!context && context.execution_id === executionId && context.node_id === nodeId && context.request_id === requestId;
  });
}

/** Whether any permission card of these turns still waits for an answer. */
export const asksPermission = (turns: TurnView[]) => turns.some((turn) => turn.items.some((item) => item.type === "permission" && !item.resolved));
