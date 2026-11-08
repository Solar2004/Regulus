import { describe, expect, test } from "vitest";
import type { CompositionNode, Port, Profile, Workflow } from "@/composition/model";
import type { BusEvent, Execution, ModuleSummary } from "./model";
import {
  agentAnswer, canTalk, eventTouches, fill, findModule, inputPorts, mergeEvents, nodeKind, nodeRows, nodeState,
  parseSendValue, projectsOf, renderValue, runnableWorkflows, unavailableOf, workflowOf,
} from "./logic";

const port = (id: string, capability: string, direction: Port["direction"] = "input", kind: Port["kind"] = "then"): Port =>
  ({ id, name: id, capability, direction, kind, required: false });
const node = (id: string, module_id: string | null, ports: Port[] = []): CompositionNode => ({
  id, type: "t", name: id, module_id, resource: module_id ? null : "file", ports, config: {},
  position: { x: 0, y: 0 }, internal_workflow_id: null, exposed_parameters: [],
});
const module = (id: string, state: ModuleSummary["state"] = { status: "active", since: 1 }): ModuleSummary =>
  ({ id, name: id, node_types: [], workflow_id: null, state });
const execution = (over: Partial<Execution> = {}): Execution =>
  ({ id: "exec-1", profile_id: "p", workflow_id: "main", project_id: null, state: { status: "running" }, ...over });
const workflow = (nodes: CompositionNode[], ports: Port[] = [], id = "main", kind: Workflow["kind"] = "main"): Workflow =>
  ({ id, name: id, kind, module_id: null, ports, nodes, wires: [], layout: { x: 0, y: 0, width: 0, height: 0 } });
const emitted = (seq: number, from = "a", value: unknown = "v"): BusEvent =>
  ({ seq, at: 0, execution_id: "exec-1", project_id: null, event: seq, from: { node_id: from, port_id: "out" }, capability: "text@1", depth: 0, kind: "emitted", value });

describe("node kind by capability", () => {
  test("an agent.engine input makes an agent, a participant input a channel", () => {
    expect(nodeKind(node("a", "x", [port("engine", "agent.engine@1", "input", "uses")]))).toBe("agent");
    expect(nodeKind(node("c", "x", [port("p", "participant@1", "input", "uses")]))).toBe("channel");
    expect(nodeKind(node("o", "x", [port("engine", "agent.engine@1", "output", "uses")]))).toBe("other");
  });
});

describe("first-level node state", () => {
  const modules = [module("regulus/agent@0.1.0"), module("regulus/files@0.1.0", { status: "idle", last_used: null }),
    module("regulus/bad@0.1.0", { status: "broken", reason: "panicked in init" }),
    module("regulus/old@0.1.0", { status: "incompatible", requires: "2.0", have: "1.11" })];

  test("resource, not installed, disabled with the missing input, broken, incompatible", () => {
    expect(nodeState(node("r", null), execution(), modules)).toEqual({ kind: "resource" });
    expect(nodeState(node("n", "regulus/gone@0.1.0"), execution(), modules)).toEqual({ kind: "not-installed" });
    const disabled = execution({ disabled: [{ node_id: "a", missing: ["engine"] }] });
    expect(nodeState(node("a", "regulus/agent@0.1.0"), disabled, modules)).toEqual({ kind: "disabled", missing: ["engine"] });
    expect(nodeState(node("b", "regulus/bad@0.1.0"), execution(), modules)).toEqual({ kind: "broken", reason: "panicked in init" });
    expect(nodeState(node("o", "regulus/old@0.1.0"), execution(), modules)).toEqual({ kind: "incompatible", requires: "2.0", have: "1.11" });
  });

  test("not installed wins over disabled: a missing module is never an error", () => {
    const disabled = execution({ disabled: [{ node_id: "n", missing: ["engine"] }] });
    expect(nodeState(node("n", "regulus/gone@0.1.0"), disabled, modules).kind).toBe("not-installed");
  });

  test("running, unused while never used, stopped when the execution is not live", () => {
    expect(nodeState(node("a", "regulus/agent@0.1.0"), execution(), modules)).toEqual({ kind: "running" });
    expect(nodeState(node("f", "regulus/files@0.1.0"), execution(), modules)).toEqual({ kind: "unused" });
    expect(nodeState(node("a", "regulus/agent@0.1.0"), execution({ state: { status: "stopped" } }), modules)).toEqual({ kind: "stopped" });
    expect(nodeState(node("a", "regulus/agent@0.1.0"), execution({ state: { status: "failed", reason: "x" } }), modules)).toEqual({ kind: "stopped" });
  });

  test("a module is found by exact id, then at any version", () => {
    expect(findModule(modules, "regulus/agent@0.2.0")?.id).toBe("regulus/agent@0.1.0");
    expect(findModule(modules, "regulus/agent")?.id).toBe("regulus/agent@0.1.0");
    expect(findModule(modules, "regulus/none")).toBeUndefined();
  });

  test("rows cross the execution's workflow with module states; only live agent and channel rows talk", () => {
    const flow = workflow([node("agent", "regulus/agent@0.1.0", [port("engine", "agent.engine@1", "input", "uses")]), node("doc", null)]);
    const profile: Profile = { id: "p", name: "p", project_id: null, extends: null, main_workflow_id: "main", workflows: [flow] };
    const rows = nodeRows(workflowOf([profile], execution()), execution(), modules);
    expect(rows.map((row) => [row.id, row.kind, row.state.kind])).toEqual([["agent", "agent", "running"], ["doc", "other", "resource"]]);
    expect(rows.map(canTalk)).toEqual([true, false]);
    expect(nodeRows(workflowOf([profile], execution({ profile_id: "gone" })), execution(), modules)).toEqual([]);
  });
});

describe("workflows and projects", () => {
  test("workflow inputs are its own input ports", () => {
    expect(inputPorts(workflow([], [port("go", "text@1"), port("done", "text@1", "output")])).map((p) => p.id)).toEqual(["go"]);
    expect(inputPorts(undefined)).toEqual([]);
  });

  test("runnable workflows put the main one first and leave module workflows out", () => {
    const profile: Profile = { id: "p", name: "p", project_id: null, extends: null, main_workflow_id: "main",
      workflows: [workflow([], [], "extra", "extra"), workflow([], [], "inner", "module"), workflow([], [], "main")] };
    expect(runnableWorkflows(profile).map((w) => w.id)).toEqual(["main", "extra"]);
  });

  test("projects are the distinct non-null ids, sorted", () => {
    expect(projectsOf([{ project_id: "b" }, { project_id: null }, { project_id: "a" }, { project_id: "b" }])).toEqual(["a", "b"]);
  });
});

describe("events", () => {
  test("merging pages dedupes by seq, keeps order and the newest only", () => {
    const merged = mergeEvents([emitted(1), emitted(2)], [emitted(2), emitted(4), emitted(3)], 0);
    expect(merged.map((e) => e.seq)).toEqual([1, 2, 3, 4]);
    expect(mergeEvents(merged, [emitted(5)], 3).map((e) => e.seq)).toEqual([3, 4, 5]);
    const same = [emitted(1)];
    expect(mergeEvents(same, [], 10)).toBe(same);
  });

  test("an event touches the node it left from or went to", () => {
    const delivered: BusEvent = { ...emitted(2), kind: "delivered", to: { node_id: "b", port_id: "in" } } as BusEvent;
    expect(eventTouches(emitted(1, "a"), "a")).toBe(true);
    expect(eventTouches(emitted(1, "a"), "b")).toBe(false);
    expect(eventTouches(delivered, "b")).toBe(true);
  });

  test("text prints as text, JSON compact with a pretty full view, long values are cut", () => {
    expect(renderValue("hello", 10)).toEqual({ short: "hello", full: "hello", truncated: false, expandable: false, json: false });
    const long = renderValue("x".repeat(30), 10);
    expect(long.short).toBe(`${"x".repeat(10)}…`);
    expect(long.truncated && long.expandable).toBe(true);
    const json = renderValue({ code: 0 }, 100);
    expect(json).toMatchObject({ short: '{"code":0}', truncated: false, expandable: true, json: true });
    expect(json.full).toBe('{\n  "code": 0\n}');
    expect(renderValue("a\nb", 100).short).toBe("a ⏎ b");
    expect(renderValue(null, 100).short).toBe("null");
  });
});

describe("sending and answers", () => {
  test("a text port takes the text, any other port JSON", () => {
    expect(parseSendValue("text@1", "{not json")).toEqual({ ok: true, value: "{not json" });
    expect(parseSendValue("json@1", '{"a":1}')).toEqual({ ok: true, value: { a: 1 } });
    expect(parseSendValue("json@1", "{nope")).toEqual({ ok: false });
  });

  test("an agent turn that did not end normally says why", () => {
    expect(agentAnswer({ text: "pong", stop_reason: "end_turn" })).toEqual({ text: "pong", stopReason: null });
    expect(agentAnswer({ text: "", stop_reason: "max_tokens" })).toEqual({ text: "", stopReason: "max_tokens" });
  });

  test("a missing or unusable module is its own state, never a generic error", () => {
    expect(unavailableOf({ code: -32601 })).toBe("not-installed");
    expect(unavailableOf({ code: -32002 })).toBe("unavailable");
    expect(unavailableOf({ code: -32003 })).toBeNull();
    expect(unavailableOf(new Error("x"))).toBeNull();
  });

  test("fill puts values in the holes it knows", () => {
    expect(fill("missing {input} in {node}", { input: "engine" })).toBe("missing engine in {node}");
  });
});
