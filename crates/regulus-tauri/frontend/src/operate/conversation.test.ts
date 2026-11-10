import { describe, expect, test } from "vitest";
import type { CompositionNode, Port, Workflow } from "@/composition/model";
import type { BusEvent, PendingDecision } from "./model";
import {
  agentNodes, asksPermission, bindingFor, busRecords, conversationPorts, decisionFor, groupTurns, hasConversation, historyRecords,
  isConversable, mergeRecords, pickSubject, readConfiguredBindings, readOwnBindings, readSessionEvent, reduceConversation,
  stillPending, subjectsOf, type ConversationRecord,
} from "./conversation";

const port = (id: string, direction: Port["direction"], capability = "text@1", kind: Port["kind"] = "then"): Port =>
  ({ id, name: id, capability, direction, kind, required: false });
const node = (id: string, ports: Port[]): CompositionNode => ({
  id, name: id, type: id, module_id: "any/module", resource: null, ports, config: {}, position: { x: 0, y: 0 }, internal_workflow_id: null, exposed_parameters: [],
} as CompositionNode);
const workflow = (ports: Port[], nodes: CompositionNode[] = []): Workflow => ({ id: "main", name: "main", ports, nodes, wires: [] } as unknown as Workflow);
const agent = node("agent", [port("engine", "input", "agent.engine@1", "uses"), port("prompt", "input")]);

describe("what can be talked to", () => {
  test("candidates are the workflow's own then ports of text, by capability", () => {
    const flow = workflow([port("go", "input"), port("data", "input", "json@1"), port("ask", "input", "text@1", "uses"), port("done", "output"), port("log", "output", "json@1")]);
    expect(conversationPorts(flow).inputs.map((item) => item.id)).toEqual(["go"]);
    expect(conversationPorts(flow).outputs.map((item) => item.id)).toEqual(["done"]);
    expect(isConversable(flow)).toBe(true);
    expect(isConversable(workflow([port("go", "input")]))).toBe(false);
    expect(isConversable(undefined)).toBe(false);
  });

  test("a direct agent is a node that uses agent.engine, whatever its module", () => {
    const flow = workflow([], [agent, node("writer", [port("in", "input")])]);
    expect(agentNodes(flow).map((item) => item.id)).toEqual(["agent"]);
    expect(hasConversation(flow)).toBe(true);
    expect(hasConversation(workflow([port("go", "input")]))).toBe(false);
  });

  test("a delegate's lazy engine for workers is not another conversation", () => {
    const workers = node("workers", [{ ...port("engine", "input", "agent.engine@1", "uses"), lazy: true }]);
    const flow = workflow([], [agent, workers]);
    expect(agentNodes(flow).map((item) => item.id)).toEqual(["agent"]);
    expect(subjectsOf(flow)).toEqual([{ kind: "agent", nodeId: "agent" }]);
  });

  test("the subject: the one picked, else the selected agent, else the bound workflow, else the first", () => {
    const flow = workflow([port("go", "input"), port("done", "output")], [agent, node("other", [port("engine", "input", "agent.engine@1", "uses")])]);
    expect(subjectsOf(flow)).toEqual([{ kind: "workflow" }, { kind: "agent", nodeId: "agent" }, { kind: "agent", nodeId: "other" }]);
    expect(pickSubject({ kind: "agent", nodeId: "other" }, "agent", flow, true)).toEqual({ kind: "agent", nodeId: "other" });
    expect(pickSubject(null, "agent", flow, true)).toEqual({ kind: "agent", nodeId: "agent" });
    expect(pickSubject(null, "nope", flow, true)).toEqual({ kind: "workflow" });
    expect(pickSubject({ kind: "agent", nodeId: "gone" }, null, flow, false)).toEqual({ kind: "workflow" });
    expect(pickSubject(null, null, workflow([], [agent]), false)).toEqual({ kind: "agent", nodeId: "agent" });
    expect(pickSubject(null, null, workflow([]), false)).toBeNull();
  });
});

describe("the binding", () => {
  const flow = workflow([port("go", "input"), port("chat", "input"), port("done", "output")]);

  test("config.toml's list and the device's map are read; anything else drops", () => {
    expect(readConfiguredBindings([{ profile: "p", workflow: "main", input: "go", output: "done" }, { profile: "p", input: "go" }, "x"]))
      .toEqual([{ profile: "p", workflow: "main", input: "go", output: "done" }]);
    expect(readConfiguredBindings({ profile: "p" })).toEqual([]);
    expect(readOwnBindings({ "p/main": { input: "go", output: "done" }, bad: { input: 1 } })).toEqual({ "p/main": { input: "go", output: "done" } });
    expect(readOwnBindings(null)).toEqual({});
  });

  test("never by itself: only what the person chose or the file says, the person's first, if its ports still fit", () => {
    expect(bindingFor({}, [], "p", flow)).toBeNull();
    const configured = [{ profile: "p", workflow: "main", input: "chat", output: "done" }];
    expect(bindingFor({}, configured, "p", flow)).toEqual({ binding: { input: "chat", output: "done" }, source: "config" });
    expect(bindingFor({ "p/main": { input: "go", output: "done" } }, configured, "p", flow)).toEqual({ binding: { input: "go", output: "done" }, source: "own" });
    // A port renamed since: the saved choice no longer counts.
    expect(bindingFor({ "p/main": { input: "gone", output: "done" } }, configured, "p", flow)?.source).toBe("config");
    expect(bindingFor({}, configured, "other", flow)).toBeNull();
    expect(bindingFor({}, configured, "p", undefined)).toBeNull();
  });
});

describe("session events", () => {
  test("known kinds read with their fields; tool_kind is not the tag; the unknown reads as other", () => {
    expect(readSessionEvent({ seq: 3, at: 1, turn: 1, kind: "tool_call", id: "t1", title: "Read", tool_kind: "read", status: "pending" }))
      .toEqual({ seq: 3, at: 1, turn: 1, kind: "tool_call", id: "t1", title: "Read", tool_kind: "read", status: "pending" });
    expect(readSessionEvent({ seq: 1, at: 0, kind: "permission_resolved", id: "r", option: null })).toMatchObject({ kind: "permission_resolved", option: null });
    expect(readSessionEvent({ seq: 1, at: 0, kind: "wobble", x: 1 })).toMatchObject({ kind: "other", name: "wobble" });
    expect(readSessionEvent({ seq: 1, at: 0, kind: "text" })).toMatchObject({ kind: "other", name: "text" });
    expect(readSessionEvent("text")).toBeNull();
  });
});

// An agent's history: a turn that streams, calls a tool, asks permission and ends.
const history = {
  entries: [
    { seq: 1, at: 1, role: "user", text: "hi", turn: "t-1", execution_id: "e" },
    { seq: 12, at: 12, role: "agent", text: "Hello there", turn: "t-1", execution_id: "e" },
    { seq: 13, at: 13, role: "system", text: "no decider", execution_id: "e" },
  ],
  activity: [
    { seq: 2, at: 2, turn: "t-1", event: { seq: 1, at: 2, turn: 1, kind: "turn_started" } },
    { seq: 3, at: 3, turn: "t-1", event: { seq: 2, at: 3, kind: "thought", text: "Let me " } },
    { seq: 4, at: 4, turn: "t-1", event: { seq: 3, at: 4, kind: "thought", text: "think" } },
    { seq: 5, at: 5, turn: "t-1", event: { seq: 4, at: 5, kind: "text", text: "Hello " } },
    { seq: 6, at: 6, turn: "t-1", event: { seq: 5, at: 6, kind: "tool_call", id: "c1", title: "Read a", tool_kind: "read", status: "pending" } },
    { seq: 7, at: 7, turn: "t-1", event: { seq: 6, at: 7, kind: "tool_update", id: "c1", status: "completed", content: [{ type: "content" }] } },
    { seq: 8, at: 8, turn: "t-1", event: { seq: 7, at: 8, kind: "permission_request", id: "r1", tool: "edit", title: "Edit a", options: [{ id: "ok", kind: "allow_once", name: "Allow" }] } },
    { seq: 9, at: 9, turn: "t-1", event: { seq: 8, at: 9, kind: "permission_resolved", id: "r1", option: "ok" } },
    { seq: 10, at: 10, turn: "t-1", event: { seq: 9, at: 10, kind: "text", text: "there" } },
    { seq: 11, at: 11, turn: "t-1", event: { seq: 10, at: 11, kind: "turn_ended", stop_reason: "end_turn" } },
  ],
};

describe("an agent's conversation", () => {
  test("records share one seq; turns group by the box's turn id", () => {
    const records = historyRecords(history);
    expect(records.map((record) => record.seq)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]);
    expect(groupTurns(records, "turn").map((group) => [group.key, group.records.length])).toEqual([["t-1", 12], ["seq-13", 1]]);
  });

  test("a turn reduces to cards: joined thoughts, one tool card, a resolved permission, the answer instead of its stream", () => {
    const [turn, note] = reduceConversation(historyRecords(history), "turn");
    expect(turn.items.map((item) => item.type)).toEqual(["entry", "thought", "tool", "permission", "ended", "entry"]);
    expect(turn.items[1]).toMatchObject({ type: "thought", text: "Let me think" });
    expect(turn.items[2]).toMatchObject({ type: "tool", title: "Read a", toolKind: "read", status: "completed", content: [{ type: "content" }] });
    expect(turn.items[3]).toMatchObject({ type: "permission", tool: "edit", resolved: { option: "ok" } });
    expect(turn.items[5]).toMatchObject({ type: "entry", role: "agent", text: "Hello there" });
    expect(turn.open).toBe(false);
    expect(note.items).toMatchObject([{ type: "entry", role: "system" }]);
  });

  test("while the turn runs, text chunks join into one live bubble and the turn is open", () => {
    const partial = historyRecords({ entries: history.entries.slice(0, 1), activity: history.activity.slice(0, 9) });
    const [turn] = reduceConversation(partial, "turn");
    expect(turn.open).toBe(true);
    expect(turn.items.filter((item) => item.type === "live")).toMatchObject([{ text: "Hello there" }]);
    expect(asksPermission([turn])).toBe(false);
    const asking = reduceConversation(historyRecords({ entries: history.entries.slice(0, 1), activity: history.activity.slice(0, 7) }), "turn");
    expect(asksPermission(asking)).toBe(true);
  });

  test("a page polled twice adds nothing; the oldest go past keep", () => {
    const records = historyRecords(history);
    const once = mergeRecords([], records, 0);
    expect(mergeRecords(once, records.slice(5), 0)).toEqual(once);
    expect(mergeRecords(once, [], 0)).toBe(once);
    expect(mergeRecords(once, records, 4).map((record) => record.seq)).toEqual([10, 11, 12, 13]);
  });

  test("a sent turn waits until its person entry is in the record", () => {
    const records = historyRecords(history);
    expect(stillPending([{ text: "hi", at: 0, turn: "t-1" }, { text: "again", at: 0, turn: "t-2" }], records, 13)).toEqual([{ text: "again", at: 0, turn: "t-2" }]);
  });
});

const emitted = (seq: number, node: string, portId: string, capability: string, value: unknown): BusEvent =>
  ({ seq, at: seq, execution_id: "e", project_id: null, event: seq, from: { node_id: node, port_id: portId }, capability, depth: 0, kind: "emitted", value });
const delivered = (seq: number): BusEvent =>
  ({ seq, at: seq, execution_id: "e", project_id: null, event: 1, from: { node_id: "self", port_id: "go" }, capability: "text@1", depth: 0, kind: "delivered", to: { node_id: "agent", port_id: "prompt" } });

describe("a workflow's conversation", () => {
  const binding = { input: "go", output: "done" };
  const events: BusEvent[] = [
    emitted(1, "self", "go", "text@1", "first"),
    delivered(2),
    emitted(3, "agent", "activity", "agent.event@1", { seq: 1, at: 3, kind: "text", text: "thin" }),
    emitted(4, "agent", "activity", "agent.event@1", { seq: 2, at: 4, kind: "tool_call", id: "c", title: "Run", status: "in_progress" }),
    emitted(5, "agent", "reply", "text@1", "not a reply of the workflow"),
    emitted(6, "self", "done", "text@1", "answer one"),
    emitted(7, "self", "go", "text@1", { not: "text" }),
    emitted(8, "other", "activity", "agent.event@1", { seq: 1, at: 8, kind: "permission_request", id: "r", title: "Edit", options: [] }),
  ];

  test("self.<input> is a person's turn, self.<output> a reply, agent.event activity; the rest is not the conversation", () => {
    const records = busRecords(events, binding);
    expect(records.map((record) => [record.seq, record.type, record.type === "entry" ? record.role : record.node])).toEqual([
      [1, "entry", "user"], [3, "activity", "agent"], [4, "activity", "agent"], [6, "entry", "agent"], [7, "entry", "user"], [8, "activity", "other"],
    ]);
    expect(records[4]).toMatchObject({ text: "{\"not\":\"text\"}" });
  });

  test("activity groups under the person's turn it follows, by seq", () => {
    const turns = reduceConversation(busRecords(events, binding), "sequence");
    expect(turns.map((turn) => [turn.key, turn.open])).toEqual([["u-1", false], ["u-7", true]]);
    expect(turns[0].items.map((item) => item.type)).toEqual(["entry", "tool", "entry"]);
    expect(turns[1].items.map((item) => item.type)).toEqual(["entry", "permission"]);
    expect(asksPermission(turns)).toBe(true);
    // Activity before any turn of a person has a group of its own.
    expect(groupTurns(busRecords(events.slice(2), binding), "sequence")[0].key).toBe("start");
  });

  test("re-polling the same events never duplicates a turn", () => {
    const first = mergeRecords([], busRecords(events.slice(0, 4), binding), 0);
    const again = mergeRecords(first, busRecords(events, binding), 0);
    expect(mergeRecords(again, busRecords(events, binding), 0)).toEqual(again);
    expect(reduceConversation(again, "sequence").length).toBe(2);
  });

  test("a sent value waits until its bus event is in the record or behind the cursor", () => {
    const records: ConversationRecord[] = busRecords(events, binding);
    expect(stillPending([{ text: "a", at: 0, event: 7 }, { text: "b", at: 0, event: 9 }, { text: "c", at: 0, event: 2 }], records, 8)).toEqual([{ text: "b", at: 0, event: 9 }]);
  });
});

describe("decisions behind permission cards", () => {
  const decision = (id: string, context: Record<string, unknown>): PendingDecision => ({
    id, kind: "decision", execution_id: "e", node_id: "decider", prompt: "asks", created_ms: 0,
    value: { options: [{ id: "ok", name: "Allow", kind: "allow_once" }], context },
  });

  test("a request is matched by its execution, node and request id in the decision's context", () => {
    const listed = [
      decision("h-1", { execution_id: "e", node_id: "agent", request_id: "r1" }),
      decision("h-2", { execution_id: "e", node_id: "other", request_id: "r1" }),
      { ...decision("h-3", { execution_id: "e", node_id: "agent", request_id: "r2" }), kind: "approval" },
    ];
    expect(decisionFor(listed, "e", "agent", "r1")?.id).toBe("h-1");
    expect(decisionFor(listed, "e", "other", "r1")?.id).toBe("h-2");
    expect(decisionFor(listed, "e", "agent", "r2")).toBeUndefined();
    expect(decisionFor(listed, "x", "agent", "r1")).toBeUndefined();
  });
});
