import { test } from "node:test";
import assert from "node:assert/strict";
import { createSession, createState } from "./rpc.mjs";
import fixture from "./profile.json" with { type: "json" };
import manifests from "./manifests.json" with { type: "json" };
import code from "./code.json" with { type: "json" };
import operate from "./operate.json" with { type: "json" };
import settings from "./settings.json" with { type: "json" };

const request = (method, params = {}, id = 1) => JSON.stringify({ jsonrpc: "2.0", id, method, params });
const negotiatedSession = (state) => {
  const session = createSession(state);
  assert.equal(session(request("hello", { proto: "1.0" })).result.compatible, true);
  return session;
};

test("each socket must negotiate a compatible protocol before reading state", () => {
  const first = negotiatedSession();
  assert.equal(first(request("profile.list")).result.length, 1);
  const second = createSession();
  assert.equal(second(request("profile.list")).error.code, -32001);
  assert.equal(second(request("hello", { proto: "2.0" })).result.compatible, false);
  assert.equal(second(request("profile.list")).error.code, -32001);
});

test("invalid JSON, requests, params and unknown methods have RPC errors", () => {
  const session = negotiatedSession();
  assert.equal(session("{").error.code, -32700);
  assert.equal(session("[]").error.code, -32600);
  assert.equal(session(request("profile.get", [])).error.code, -32602);
  assert.equal(session(request("profile.get", { id: "missing" })).error.code, -32602);
  assert.equal(session(request("node.add")).error.code, -32601);
  assert.equal(session(JSON.stringify({ jsonrpc: "2.0", method: "profile.list" })), null);
});

test("profile state is separate from executions and module metadata", () => {
  const session = negotiatedSession();
  const [{ id }] = session(request("profile.list")).result;
  assert.deepEqual(session(request("profile.get", { id })).result, fixture.profiles[0]);
  assert.deepEqual(session(request("execution.list")).result, []);
  const listed = session(request("module.list")).result;
  // code.json describes `regulus/providers` in full: it stands in for the fixture's example of that name.
  const replaced = fixture.modules.filter((module) => code.manifests.some((manifest) => manifest.module.id.split("@")[0] === module.id.split("@")[0]));
  assert.deepEqual(replaced.map((module) => module.id), ["regulus/providers@0.1"]);
  assert.equal(listed.length, fixture.modules.length - replaced.length + manifests.length + operate.manifests.length + code.manifests.length + settings.modules.length);
  assert.equal(new Set(listed.map((module) => module.id.split("@")[0])).size, listed.length);
});

test("every wire joins existing compatible ports of the same kind", () => {
  for (const profile of fixture.profiles) {
    assert.ok(profile.workflows.some((workflow) => workflow.id === profile.main_workflow_id));
    for (const workflow of profile.workflows) {
      assert.equal(new Set(workflow.nodes.map((node) => node.id)).size, workflow.nodes.length);
      for (const wire of workflow.wires) {
        const from = workflow.nodes.find((node) => node.id === wire.from.node_id)?.ports.find((port) => port.id === wire.from.port_id);
        const to = workflow.nodes.find((node) => node.id === wire.to.node_id)?.ports.find((port) => port.id === wire.to.port_id);
        assert.ok(from && to);
        assert.equal(from.direction, "output");
        assert.equal(to.direction, "input");
        assert.equal(from.capability, to.capability);
        assert.equal(from.kind, wire.kind);
        assert.equal(to.kind, wire.kind);
      }
    }
  }
});

test("module spaces resolve by reference and credentials stay as references", () => {
  for (const profile of fixture.profiles) for (const workflow of profile.workflows) for (const node of workflow.nodes) {
    if (node.module_id) assert.ok(fixture.modules.some((module) => module.id === node.module_id));
    if (node.internal_workflow_id) assert.ok(profile.workflows.some((internal) => internal.id === node.internal_workflow_id));
    for (const [key, value] of Object.entries(node.config)) if (key === "credential") assert.match(value, /^secret:/);
  }
});

test("module.manifest gives one manifest by id, with or without version, or all of them", () => {
  const session = negotiatedSession();
  const flow = session(request("module.manifest", { id: "regulus/flow" })).result;
  assert.deepEqual(flow, manifests[0]);
  assert.equal(session(request("module.manifest", { id: "regulus/files@0.1.0" })).result.module.name, "Files");
  const call = flow.node.find((node) => node.type === "call");
  assert.ok(call.port.length && call.field.some((field) => field.type === "workflow"));
  const all = session(request("module.manifest")).result;
  assert.deepEqual(all.map((manifest) => manifest.module.id), session(request("module.list")).result.map((module) => module.id));
  assert.equal(session(request("module.manifest", { id: "nope/x" })).error.code, -32602);
});

test("profile.save writes new profiles, replaces only with force and refuses bad ones", () => {
  const state = createState();
  const session = negotiatedSession(state);
  const copy = { ...structuredClone(fixture.profiles[0]), id: "copy", name: "" };
  const saved = session(request("profile.save", { profile: copy })).result;
  assert.deepEqual(saved, { profile_id: "copy", path: "mock/profiles/copy.toml", diagnostics: [] });
  assert.equal(session(request("profile.get", { id: "copy" })).result.name, "copy");
  assert.equal(negotiatedSession(state)(request("profile.list")).result.length, 2);
  assert.equal(session(request("profile.save", { profile: { ...copy, name: "Again" } })).error.code, -32007);
  session(request("profile.save", { profile: { ...copy, name: "Again" }, force: true }));
  assert.equal(session(request("profile.get", { id: "copy" })).result.name, "Again");
  assert.equal(session(request("profile.save", { profile: { ...copy, main_workflow_id: "nowhere" }, force: true })).error.code, -32008);
  assert.equal(session(request("profile.save", { profile: { ...copy, id: "../x" } })).error.code, -32602);
  assert.equal(session(request("profile.save", { profile: { id: 1 } })).error.code, -32602);
  // A fresh state has none of it.
  assert.equal(negotiatedSession()(request("profile.list")).result.length, 1);
});

test("profile.delete removes a profile unless an execution of it is live", () => {
  const state = createState();
  const session = negotiatedSession(state);
  const [{ id }] = session(request("profile.list")).result;
  state.executions.push({ id: "exec-1", profile_id: id, workflow_id: "main", project_id: null, state: { status: "running" } });
  const busy = session(request("profile.delete", { id }));
  assert.equal(busy.error.code, -32009);
  assert.match(busy.error.message, /exec-1/);
  state.executions[0].state = { status: "stopped" };
  assert.deepEqual(session(request("profile.delete", { id })).result, { profile_id: id, path: `mock/profiles/${id}.toml` });
  assert.equal(session(request("profile.get", { id })).error.code, -32602);
  assert.equal(session(request("profile.delete", { id })).error.code, -32602);
});

test("template.list, template.get and template.apply mirror the core's shapes and errors", () => {
  const session = negotiatedSession();
  const list = session(request("template.list")).result;
  assert.deepEqual(list.map((template) => template.name), ["echo", "chat", "ralph", "broken", "regulus-code"]);
  assert.ok(list.every((template) => !("text" in template) && !("profile" in template) && Array.isArray(template.vars)));
  assert.match(list.find((template) => template.name === "broken").error, /TOML/);
  assert.deepEqual(Object.keys(session(request("template.get", { template: "echo" })).result), ["name", "path", "text"]);
  assert.equal(session(request("template.get", { template: "nope" })).error.code, -32004);
  const missing = session(request("template.apply", { template: "echo", vars: {} }));
  assert.equal(missing.error.code, -32602);
  assert.match(missing.error.message, /`who`/);
  const applied = session(request("template.apply", { template: "echo", vars: { who: "Ana" } })).result;
  assert.deepEqual(applied, { profile_id: "echo", path: "mock/profiles/echo.toml", diagnostics: [] });
  const profile = session(request("profile.get", { id: "echo" })).result;
  assert.equal(profile.name, "Echo for Ana");
  assert.equal(profile.workflows[0].nodes[0].config.path, "greeting.txt");
  assert.equal(session(request("template.apply", { template: "echo", vars: { who: "Bo" } })).error.code, -32007);
  assert.equal(session(request("template.apply", { template: "echo", vars: { who: "Bo" }, force: true })).result.profile_id, "echo");
  assert.equal(session(request("template.apply", { template: "echo", vars: { who: "Bo" }, id: "echo2" })).result.profile_id, "echo2");
  assert.equal(session(request("template.apply", { template: "ralph", vars: { workdir: "/x" } })).error.code, -32006);
  assert.equal(session(request("template.apply", { template: "broken" })).error.code, -32005);
  assert.equal(session(request("template.apply", { template: "nope" })).error.code, -32004);
});

test("execution.start and execution.stop run a profile and report disabled nodes", () => {
  const session = negotiatedSession();
  session(request("template.apply", { template: "chat", vars: {} }));
  const started = session(request("execution.start", { profile_id: "chat" })).result;
  assert.deepEqual(started, { id: "exec-1", profile_id: "chat", workflow_id: "main", project_id: null, state: { status: "running" }, disabled: [{ node_id: "lonely", missing: ["engine"] }] });
  assert.equal(session(request("execution.list")).result.length, 1);
  assert.equal(session(request("execution.start", { profile_id: "nope" })).error.code, -32602);
  assert.equal(session(request("execution.start", { profile_id: "chat", workflow_id: "nope" })).error.code, -32602);
  assert.deepEqual(session(request("execution.stop", { id: "exec-1" })).result.state, { status: "stopped" });
  assert.equal(session(request("execution.stop", { id: "exec-9" })).error.code, -32602);
});

test("workflow.send emits from self and event.list pages the bus with its cursor", () => {
  const session = negotiatedSession();
  session(request("template.apply", { template: "echo", vars: { who: "Ana" } }));
  const { id } = session(request("execution.start", { profile_id: "echo" })).result;
  assert.deepEqual(session(request("workflow.send", { execution_id: id, port: "go", value: "hi" })).result, { event: 1 });
  assert.equal(session(request("workflow.send", { execution_id: id, port: "done", value: "hi" })).error.code, -32602);
  const all = session(request("event.list", { execution_id: id })).result;
  assert.deepEqual(all.events.map((event) => [event.kind, `${event.from.node_id}.${event.from.port_id}`]), [["emitted", "self.go"], ["delivered", "self.go"], ["emitted", "self.done"]]);
  assert.deepEqual(all.events[1].to, { node_id: "note", port_id: "in" });
  assert.equal(all.events[2].value, "hi");
  assert.equal(all.pending, 0);
  const first = session(request("event.list", { execution_id: id, limit: 2 })).result;
  assert.equal(first.events.length, 2);
  const rest = session(request("event.list", { execution_id: id, since: first.next })).result;
  assert.deepEqual(rest.events.map((event) => event.seq), [3]);
  assert.deepEqual(session(request("event.list", { since: rest.next })).result, { events: [], next: rest.next, pending: 0 });
  session(request("execution.stop", { id }));
  assert.equal(session(request("workflow.send", { execution_id: id, port: "go", value: "hi" })).error.code, -32602);
});

test("agent.prompt answers an agent node of a live execution and refuses the rest", () => {
  const session = negotiatedSession();
  session(request("template.apply", { template: "chat", vars: {} }));
  const { id } = session(request("execution.start", { profile_id: "chat" })).result;
  assert.deepEqual(session(request("agent.prompt", { execution_id: id, node_id: "agent", text: "ping" })).result, { text: "(mock) ping", stop_reason: "end_turn" });
  assert.equal(session(request("agent.prompt", { execution_id: id, node_id: "lonely", text: "ping" })).error.code, -32602);
  assert.equal(session(request("agent.prompt", { execution_id: id, node_id: "room", text: "ping" })).error.code, -32602);
  assert.equal(session(request("agent.prompt", { execution_id: "exec-9", node_id: "agent", text: "ping" })).error.code, -32602);
});

test("agent.send queues a turn; agent.history streams it step by step with one cursor; a decider's question waits in human.list", () => {
  const session = negotiatedSession();
  session(request("template.apply", { template: "chat", vars: {} }));
  const { id } = session(request("execution.start", { profile_id: "chat" })).result;
  const at = { execution_id: id, node_id: "agent" };
  assert.deepEqual(session(request("agent.send", { ...at, text: "hello" })).result, { turn: "t-1" });
  assert.deepEqual(session(request("agent.send", { ...at, text: "again" })).result, { turn: "t-2" });
  assert.equal(session(request("agent.send", { execution_id: id, node_id: "room", text: "x" })).error.code, -32602);
  assert.equal(session(request("agent.send", { execution_id: id, node_id: "lonely", text: "x" })).error.code, -32602);
  // Each read moves the turn one step; the cursor pages entries and activity together.
  let cursor = 0;
  const seen = [];
  const read = () => {
    const page = session(request("agent.history", { ...at, since: cursor })).result;
    for (const item of [...page.entries, ...page.activity]) { assert.ok(item.seq > cursor); seen.push(item); }
    cursor = page.next;
    return page;
  };
  assert.deepEqual(read().session, { engine_session_id: `mock-${id}-agent`, native_resume: false, state: "busy" });
  for (let step = 0; step < 8; step++) read();
  const kinds = () => seen.filter((item) => item.event).map((item) => item.event.kind);
  assert.deepEqual(kinds(), ["turn_started", "state", "thought", "text", "tool_call", "tool_update", "plan", "permission_request"]);
  assert.equal(seen.find((item) => item.event?.kind === "tool_call").event.tool_kind, "read");
  // The permission request waits for a person: the turn does not move.
  const [decision] = session(request("human.list", { execution_id: id })).result;
  assert.equal(decision.kind, "decision");
  assert.equal(decision.node_id, "decider");
  assert.deepEqual(decision.value.context, { tool: "edit", title: "Edit notes.md", execution_id: id, node_id: "agent", request_id: "t-1-edit" });
  assert.deepEqual(decision.form.properties.option.enum, ["allow", "reject"]);
  assert.equal(read().entries.length + read().activity.length, 0);
  assert.equal(session(request("human.answer", { id: decision.id, decision: "approve", fields: { option: "maybe" } })).error.code, -32602);
  assert.equal(session(request("human.answer", { id: decision.id, decision: "approve", fields: { option: "allow" } })).result.port, "decisions");
  assert.deepEqual(session(request("human.list", {})).result, []);
  for (let step = 0; step < 2; step++) read();
  assert.deepEqual(kinds().slice(8), ["permission_resolved", "text", "turn_ended", "state"]);
  assert.equal(seen.find((item) => item.event?.kind === "permission_resolved").event.option, "allow");
  assert.deepEqual(seen.filter((item) => item.role).map((item) => [item.role, item.text, item.turn]), [["user", "hello", "t-1"], ["agent", "(mock) You said: hello", "t-1"]]);
  // A page read again from 0 gives the same records: nothing is lost or repeated.
  const all = session(request("agent.history", { ...at, since: 0, limit: 5 })).result;
  assert.equal(all.entries.length + all.activity.length, 5);
  // The next turn starts; cancelling it cancels the waiting permission and ends it as cancelled.
  for (let step = 0; step < 7; step++) read();
  assert.equal(session(request("human.list", {})).result.length, 1);
  assert.equal(session(request("agent.cancel", at)).result, null);
  assert.deepEqual(session(request("human.list", {})).result, []);
  const last = read();
  // The cancelled turn's answer is what it had written.
  assert.deepEqual(last.entries.map((entry) => [entry.role, entry.text, entry.turn]), [["agent", "(mock) You said: ", "t-2"]]);
  const tail = [...seen].sort((a, b) => a.seq - b.seq).slice(-4).map((item) => item.event?.kind ?? item.role);
  assert.deepEqual(tail, ["permission_resolved", "turn_ended", "agent", "state"]);
  assert.equal(seen.find((item) => item.event?.kind === "turn_ended" && item.turn === "t-2").event.stop_reason, "cancelled");
  // Stopped, the history still reads, as `stopped`; send and cancel do not.
  session(request("execution.stop", { id }));
  assert.equal(session(request("agent.history", at)).result.session.state, "stopped");
  assert.equal(session(request("agent.send", { ...at, text: "x" })).error.code, -32602);
  assert.equal(session(request("agent.cancel", at)).error.code, -32602);
});

test("workflow.send into an agent's prompt is its turn: activity and the reply come on the bus as event.list reads it", () => {
  const session = negotiatedSession();
  session(request("template.apply", { template: "chat", vars: {} }));
  const { id } = session(request("execution.start", { profile_id: "chat" })).result;
  session(request("workflow.send", { execution_id: id, port: "go", value: "hi" }));
  let cursor = 0;
  const events = [];
  for (let read = 0; read < 12; read++) {
    const page = session(request("event.list", { execution_id: id, since: cursor })).result;
    events.push(...page.events);
    cursor = page.next;
    const [decision] = session(request("human.list", { execution_id: id })).result;
    if (decision) session(request("human.answer", { id: decision.id, decision: "reject" }));
  }
  const emitted = events.filter((event) => event.kind === "emitted").map((event) => `${event.from.node_id}.${event.from.port_id}`);
  assert.equal(emitted[0], "self.go");
  assert.ok(emitted.includes("agent.activity"));
  assert.deepEqual(emitted.slice(-2), ["agent.reply", "self.done"]);
  assert.equal(events.at(-1).value, "(mock) You said: hi");
  const resolved = events.find((event) => event.capability === "agent.event@1" && event.value.kind === "permission_resolved");
  assert.equal(resolved.value.option, "reject");
});

test("comm.history, comm.participants and comm.send serve a channel node", () => {
  const session = negotiatedSession();
  session(request("template.apply", { template: "chat", vars: {} }));
  const { id } = session(request("execution.start", { profile_id: "chat" })).result;
  const at = { execution_id: id, node_id: "room" };
  assert.deepEqual(session(request("comm.history", at)).result, []);
  assert.deepEqual(session(request("comm.participants", at)).result, [{ name: "agent", node: "agent", kind: "agent", state: "idle", queued: 0 }]);
  const sent = session(request("comm.send", { ...at, from: "user", text: "hello" })).result;
  assert.deepEqual({ ...sent, at: 0 }, { id: 1, at: 0, from: "user", to: ["agent"], text: "hello" });
  assert.equal(session(request("comm.history", at)).result.length, 1);
  assert.equal(session(request("comm.send", { ...at, from: "a user", text: "x" })).error.code, -32602);
  assert.equal(session(request("comm.send", { ...at, from: "user", text: " " })).error.code, -32602);
  assert.equal(session(request("comm.history", { execution_id: id, node_id: "agent" })).error.code, -32602);
});

test("human methods list nothing pending; module methods without their module are not found", () => {
  const state = createState();
  const session = negotiatedSession(state);
  assert.deepEqual(session(request("human.list", {})).result, []);
  assert.equal(session(request("human.answer", { id: "h-1", decision: "approve" })).error.code, -32602);
  state.modules = state.modules.filter((module) => !module.id.startsWith("regulus/agent@"));
  assert.equal(session(request("agent.prompt", { execution_id: "exec-1", node_id: "agent", text: "x" })).error.code, -32601);
  for (const method of ["agent.send", "agent.history", "agent.cancel"]) {
    assert.equal(session(request(method, { execution_id: "exec-1", node_id: "agent", text: "x" })).error.code, -32601);
  }
});

// A profile with a greeter wired "then" to a writer, plus an agent box whose required
// "uses" input has no wire: what the canvas builds and saves.
const edited = () => ({
  id: "edited", name: "Edited", project_id: null, extends: null, main_workflow_id: "main",
  workflows: [{
    id: "main", name: "Main", kind: "main", module_id: null, layout: { x: 0, y: 0, width: 0, height: 0 },
    nodes: [
      { id: "write", type: "write", name: "Write", module_id: "regulus/files@0.1.0", resource: null, config: { path: "out.md" }, position: { x: 300, y: 0 }, internal_workflow_id: null, exposed_parameters: [],
        ports: manifests[1].node.find((node) => node.type === "write").port },
      { id: "agent", type: "agent", name: "Agent", module_id: "regulus/agent@0.1.0", resource: null, config: {}, position: { x: 0, y: 200 }, internal_workflow_id: null, exposed_parameters: [],
        ports: [{ id: "engine", name: "engine", capability: "agent.engine@1", direction: "input", kind: "uses", required: true }] },
      { id: "read", type: "read", name: "Read", module_id: "regulus/files@0.1.0", resource: null, config: {}, position: { x: 0, y: 0 }, internal_workflow_id: null, exposed_parameters: [],
        ports: manifests[1].node.find((node) => node.type === "read").port },
    ],
    wires: [{ id: "read-write", kind: "then", from: { node_id: "read", port_id: "out" }, to: { node_id: "write", port_id: "in" } }],
  }],
});

test("profile.check reports what the core would: missing module, unwired required input", () => {
  const state = createState();
  const session = negotiatedSession(state);
  assert.equal(session(request("profile.check", { id: "missing" })).error.code, -32602);
  const [{ id }] = session(request("profile.list")).result;
  assert.deepEqual(session(request("profile.check", { id })).result, []);
  const profile = edited();
  profile.workflows[0].nodes.push({ id: "gone", type: "x", name: "Gone", module_id: "regulus/nowhere@0.1.0", resource: null, config: {}, position: { x: 0, y: 400 }, internal_workflow_id: null, exposed_parameters: [], ports: [] });
  const saved = session(request("profile.save", { profile })).result;
  const codes = saved.diagnostics.map((item) => [item.severity, item.code, item.node_id]);
  assert.deepEqual(codes, [["info", "node.module_not_installed", "gone"], ["warning", "port.required_unwired", "agent"]]);
  assert.deepEqual(session(request("profile.check", { id: "edited" })).result, saved.diagnostics);
  for (const item of saved.diagnostics) assert.deepEqual(Object.keys(item).sort(), ["code", "message", "node_id", "severity", "wire_id", "workflow_id"]);
});

test("profile.save refuses a profile with errors, with the core's message", () => {
  const session = negotiatedSession();
  const profile = edited();
  profile.workflows[0].wires.push({ id: "bad", kind: "then", from: { node_id: "write", port_id: "written" }, to: { node_id: "read", port_id: "in" } });
  const refused = session(request("profile.save", { profile }));
  assert.equal(refused.error.code, -32008);
  assert.match(refused.error.message, /^profile `edited` has errors: `path@1` does not fit into `text@1` \(wire\.capability_mismatch\)$/);
  profile.workflows[0].wires.push({ id: "bad2", kind: "then", from: { node_id: "read", port_id: "out" }, to: { node_id: "write", port_id: "nope" } });
  assert.match(session(request("profile.save", { profile })).error.message, /wire\.endpoint_missing/);
});

test("execution.start records an execution of the main workflow with its disabled nodes", () => {
  const state = createState();
  const session = negotiatedSession(state);
  session(request("profile.save", { profile: edited() }));
  assert.equal(session(request("execution.start", { profile_id: "nope" })).error.code, -32602);
  assert.equal(session(request("execution.start", { profile_id: "edited", workflow_id: "nope" })).error.code, -32602);
  const started = session(request("execution.start", { profile_id: "edited" })).result;
  assert.deepEqual(started, { id: "exec-1", profile_id: "edited", workflow_id: "main", project_id: null, state: { status: "running" }, disabled: [{ node_id: "agent", missing: ["engine"] }] });
  assert.deepEqual(session(request("execution.list")).result, [started]);
  assert.equal(session(request("profile.delete", { id: "edited" })).error.code, -32009);
});

test("secret.* lists names only, sets, replaces and deletes, refusing bad names", () => {
  const state = createState();
  const session = negotiatedSession(state);
  const names = () => session(request("secret.list")).result;
  assert.deepEqual(names(), Object.keys(settings.secrets));
  assert.equal(session(request("secret.set", { name: "demo", value: "x" })).result, null);
  assert.equal(session(request("secret.set", { name: "demo", value: "y" })).result, null);
  assert.deepEqual(names(), [...Object.keys(settings.secrets), "demo"]);
  assert.ok(!JSON.stringify(session(request("secret.list"))).includes("mock-not-a-real"));
  assert.equal(session(request("secret.set", { name: "has space", value: "x" })).error.code, -32602);
  assert.equal(session(request("secret.delete", { name: "demo" })).result, null);
  assert.equal(session(request("secret.delete", { name: "demo" })).error.code, -32602);
  assert.ok(!names().includes("demo"));
});

test("service keys: new shows the key once, list never does, revoke stamps the time", () => {
  const state = createState();
  const session = negotiatedSession(state);
  const policy = { cooldown_ms: 0, requests: 5, window_ms: 1000, concurrency: 0, budget_requests: 0, budget_ms: 0 };
  const created = session(request("service.key.new", { name: "demo", scope: ["*"], policy })).result;
  assert.match(created.key, /^rgk_[0-9a-f]{64}$/);
  assert.equal(created.hint, created.key.slice(0, 10));
  assert.deepEqual(created.policy, policy);
  const listed = session(request("service.key.list")).result;
  assert.equal(listed.length, settings.keys.length + 1);
  assert.ok(listed.every((key) => key.key === undefined && key.hash === undefined));
  const plain = session(request("service.key.new", { name: "", scope: ["global"], policy: { ...policy, requests: 0, window_ms: 0 } })).result;
  assert.equal(plain.policy, undefined);
  assert.equal(session(request("service.key.new", { scope: [] })).error.code, -32602);
  assert.equal(session(request("service.key.new", { scope: ["a/b/c"] })).error.code, -32602);
  assert.equal(session(request("service.key.new", { scope: ["*"], policy: { nope: 1 } })).error.code, -32602);
  const revoked = session(request("service.key.revoke", { id: created.id })).result;
  assert.equal(typeof revoked.revoked_ms, "number");
  assert.equal(session(request("service.key.revoke", { id: created.id })).result.revoked_ms, revoked.revoked_ms);
  assert.equal(session(request("service.key.revoke", { id: "k-none" })).error.code, -32602);
});

test("service.log filters by key and workflow and keeps the last; endpoints list where engines listen", () => {
  const session = negotiatedSession();
  assert.equal(session(request("service.log")).result.length, settings.log.length);
  assert.ok(session(request("service.log", { workflow: "translate" })).result.every((entry) => entry.workflow === "translate"));
  assert.equal(session(request("service.log", { key: "k-none" })).result.length, 0);
  assert.deepEqual(session(request("service.log", { last: 1 })).result, settings.log.slice(-1));
  assert.deepEqual(session(request("service.endpoints")).result, settings.endpoints);
});

test("without the service module its methods are not found; unusable, they are unavailable with the reason", () => {
  const state = createState();
  const session = negotiatedSession(state);
  const service = state.modules.find((module) => module.id.startsWith("regulus/service@"));
  service.state = { status: "broken", reason: "init failed" };
  const unavailable = session(request("service.key.list")).error;
  assert.equal(unavailable.code, -32002);
  assert.match(unavailable.message, /init failed/);
  state.modules = state.modules.filter((module) => module !== service);
  assert.equal(session(request("service.key.list")).error.code, -32601);
  assert.equal(session(request("secret.list")).result.length, Object.keys(settings.secrets).length);
});

test("module.list carries the states Settings shows, each with its reason", () => {
  const listed = negotiatedSession()(request("module.list")).result;
  const status = (id) => listed.find((module) => module.id === id).state;
  assert.deepEqual(status("regulus/web@0.1.0"), { status: "broken", reason: settings.modules[1].state.reason });
  assert.deepEqual(status("regulus/memory@0.1.0"), { status: "disabled", missing: ["embedder"] });
  assert.equal(status("regulus/judge@0.1.0").status, "incompatible");
  assert.equal(status("regulus/service@0.1.0").status, "active");
  assert.deepEqual(status("demo/echo@0.1.0"), { status: "blocked", reason: "untrusted project: reopen with --trust" });
  assert.equal(listed.find((module) => module.id === "demo/echo@0.1.0").project, "demo");
});

test("project.trust starts a blocked local module; --remove blocks it again", () => {
  const session = negotiatedSession();
  const [project] = session(request("project.list")).result;
  assert.equal(project.id, "demo");
  const echo = () => session(request("module.list")).result.find((module) => module.id === "demo/echo@0.1.0").state;
  assert.equal(echo().status, "blocked");
  assert.deepEqual(session(request("project.trust", { path: project.path })).result, { path: project.path, trusted: true });
  assert.deepEqual(echo(), { status: "idle", last_used: null });
  assert.deepEqual(session(request("project.trust", { path: project.path, remove: true })).result, { path: project.path, trusted: false });
  assert.equal(echo().status, "blocked");
  assert.equal(session(request("project.trust", { path: "/nowhere" })).error.code, -32602);
});

test("config.client gives [client] as written, the project's on top key by key, with sources", () => {
  const session = negotiatedSession();
  const global = session(request("config.client")).result;
  assert.deepEqual(global, {
    client: settings.client.global,
    sources: { refresh_ms: "config.toml", "tui.event_page": "config.toml" },
    warnings: [],
  });
  const demo = session(request("config.client", { project_id: "demo" })).result;
  assert.deepEqual(demo.client, { refresh_ms: 500, tui: { event_page: 100, value_max: 120 }, start_view: "operate" });
  assert.equal(demo.sources.refresh_ms, "regulus.toml");
  assert.equal(demo.sources["tui.event_page"], "config.toml");
  assert.equal(demo.sources["tui.value_max"], "regulus.toml");
  assert.equal(session(request("config.client", { project_id: "nope" })).error.code, -32010);
});

test("template.save writes the saved profile or one workflow of it, refusing an existing name without force", () => {
  const session = negotiatedSession();
  const [{ id }] = session(request("profile.list")).result;
  const saved = session(request("template.save", { name: "mine", profile_id: id, description: "d", vars: { who: { description: "who", default: "me" }, model: { description: "m" } } }));
  assert.deepEqual(saved.result, { path: "mock/templates/mine.toml" });
  const listed = session(request("template.list")).result.find((item) => item.name === "mine");
  assert.deepEqual(listed.vars, [{ name: "who", description: "who", default: "me" }, { name: "model", description: "m" }]);
  assert.equal(session(request("template.save", { name: "mine", profile_id: id })).error.code, -32007);
  assert.deepEqual(session(request("template.save", { name: "mine", profile_id: id, force: true, path: "/tmp/x.toml" })).result, { path: "/tmp/x.toml" });
  // One workflow: it becomes the main one, with the workflows its boxes place (at any depth).
  const box = (nodeId, inner) => ({ id: nodeId, type: "workflow", name: nodeId, module_id: null, resource: null, ports: [], config: {}, position: { x: 0, y: 0 }, internal_workflow_id: inner, exposed_parameters: [] });
  const workflow = (wid, kind, nodes = []) => ({ id: wid, name: wid, kind, module_id: null, nodes, wires: [], layout: { x: 0, y: 0, width: 0, height: 0 } });
  const boxes = { id: "boxes", name: "boxes", project_id: null, extends: null, main_workflow_id: "main",
    workflows: [workflow("main", "main", [box("b", "inner")]), workflow("inner", "extra", [box("d", "deep")]), workflow("deep", "extra"), workflow("unused", "extra")] };
  assert.ok(session(request("profile.save", { profile: boxes })).result);
  session(request("template.save", { name: "part", profile_id: "boxes", workflow_id: "inner" }));
  const applied = session(request("template.apply", { template: "part", vars: {}, id: "part-copy" })).result;
  const copy = session(request("profile.get", { id: applied.profile_id })).result;
  assert.equal(copy.main_workflow_id, "inner");
  assert.deepEqual(copy.workflows.map((item) => item.id), ["inner", "deep"]);
  assert.equal(session(request("template.save", { name: "", profile_id: id })).error.code, -32005);
  assert.equal(session(request("template.save", { name: "x", profile_id: id, vars: { "2bad": { description: "" } } })).error.code, -32005);
  assert.equal(session(request("template.save", { name: "x", profile_id: "nope" })).error.code, -32602);
  assert.equal(session(request("template.save", { name: "x", profile_id: id, workflow_id: "nope" })).error.code, -32602);
});

test("execution.tree nests the children a calling node starts; stopping the parent stops them", () => {
  const session = negotiatedSession();
  const caller = { id: "c", type: "call", name: "c", module_id: "regulus/flow@0.1.0", resource: null, ports: [], config: { workflow: "worker" }, position: { x: 0, y: 0 }, internal_workflow_id: null, exposed_parameters: [] };
  const workflow = (id, kind, nodes = []) => ({ id, name: id, kind, module_id: null, nodes, wires: [], layout: { x: 0, y: 0, width: 0, height: 0 } });
  const profile = { id: "calls", name: "calls", project_id: null, extends: null, main_workflow_id: "main", workflows: [workflow("main", "main", [caller]), workflow("worker", "extra")] };
  assert.ok(session(request("profile.save", { profile })).result);
  const root = session(request("execution.start", { profile_id: "calls" })).result;
  const listed = session(request("execution.list")).result;
  const child = listed.find((item) => item.parent === root.id);
  assert.deepEqual({ workflow: child.workflow_id, caller: child.caller, tag: child.tag }, { workflow: "worker", caller: "c", tag: "c/0" });
  const tree = session(request("execution.tree", { execution_id: root.id })).result;
  assert.equal(tree.execution.id, root.id);
  assert.deepEqual(tree.children.map((item) => [item.execution.id, item.children.length]), [[child.id, 0]]);
  assert.deepEqual(session(request("execution.tree", { execution_id: child.id })).result.children, []);
  assert.equal(session(request("execution.tree", { execution_id: "nope" })).error.code, -32602);
  assert.equal(session(request("execution.tree", {})).error.code, -32602);
  session(request("execution.stop", { id: root.id }));
  assert.deepEqual(session(request("execution.list")).result.find((item) => item.id === child.id).state, { status: "stopped" });
});

// ── Regulus Code: the `regulus-code` template, `code.inspect`, the coding turn and workers ──

/** Applies `regulus-code` on a folder and starts it: the session, with helpers that read it. */
function codeSession(root = "/home/ada/lighthouse", state = createState()) {
  const session = negotiatedSession(state);
  const applied = session(request("template.apply", { template: "regulus-code", vars: { root }, id: "code-lighthouse" })).result;
  const execution = session(request("execution.start", { profile_id: applied.profile_id })).result;
  const on = (node) => ({ execution_id: execution.id, node_id: node });
  const inspect = (action, extra = {}) => session(request("code.inspect", { ...on("workspace"), action, ...extra }));
  const cursors = new Map();
  /** Reads on in an agent's history until `done` says so, one scripted step per read: what came since the last call. */
  const follow = (done, node = "agent", from = execution.id) => {
    const seen = { entries: [], activity: [] };
    const key = `${from}/${node}`;
    for (let rounds = 0; rounds < 40 && !done(seen); rounds++) {
      const page = session(request("agent.history", { execution_id: from, node_id: node, since: cursors.get(key) ?? 0 })).result;
      seen.entries.push(...page.entries);
      seen.activity.push(...page.activity);
      cursors.set(key, page.next);
    }
    return seen;
  };
  return { state, session, applied, execution, on, inspect, follow };
}
const kinds = (seen) => seen.activity.map((item) => item.event.kind);
const ended = (seen) => kinds(seen).includes("turn_ended");
const asking = (seen) => kinds(seen).includes("permission_request");

test("regulus-code applies with the folder, passes profile.check and starts with nothing disabled", () => {
  const { session, applied, execution } = codeSession();
  assert.deepEqual(applied, { profile_id: "code-lighthouse", path: "mock/profiles/code-lighthouse.toml", diagnostics: [] });
  const profile = session(request("profile.get", { id: "code-lighthouse" })).result;
  const node = (id) => profile.workflows[0].nodes.find((item) => item.id === id);
  assert.equal(profile.main_workflow_id, "code");
  assert.equal(node("workspace").config.root, "/home/ada/lighthouse");
  assert.deepEqual(node("provider").config, { preset: "ollama", base_url: "", credential: "", model: "qwen3:4b" });
  // A hole the manifest types as a number is a number, as in the core's TOML.
  assert.equal(node("workers").config.max_workers, 2);
  assert.equal(node("native").config.max_steps, 16);
  assert.deepEqual(session(request("profile.check", { id: "code-lighthouse" })).result, []);
  assert.equal(execution.state.status, "running");
  assert.equal(execution.disabled, undefined);
  assert.equal(session(request("template.apply", { template: "regulus-code", vars: {} })).error.code, -32602);
});

test("the workspace node is the one offering code.workspace@1, and the module declares code.inspect", () => {
  const manifest = negotiatedSession()(request("module.manifest", { id: "regulus/code" })).result;
  assert.deepEqual(manifest.module.methods, ["code.inspect"]);
  const offering = manifest.node.filter((type) => type.port.some((port) => port.direction === "output" && port.capability === "code.workspace@1"));
  assert.deepEqual(offering.map((type) => type.type), ["workspace"]);
  assert.ok(offering[0].port.some((port) => port.direction === "output" && port.capability === "tool@1"));
});

test("code.inspect summary: the folder, porcelain status with its parsed lines, and the tools", () => {
  const { inspect } = codeSession();
  const summary = inspect("summary").result;
  assert.equal(summary.root, "/home/ada/lighthouse");
  assert.equal(summary.git.branch, "main");
  assert.equal(summary.git.status, "?? scratch/todo.txt\n M src/lib/port.ts\n");
  assert.deepEqual(summary.git.changes, [{ path: "scratch/todo.txt", code: "??" }, { path: "src/lib/port.ts", code: " M" }]);
  assert.equal(summary.git.truncated, false);
  assert.equal(summary.git_reason, undefined);
  assert.ok(summary.tools.includes("code_edit_file") && summary.tools.includes("code_run_command"));
});

test("code.inspect list gives direct children with root-relative paths; read and search stay inside the folder", () => {
  const { inspect } = codeSession();
  const top = inspect("list").result;
  assert.equal(top.path, "");
  assert.deepEqual(top.entries.map((entry) => `${entry.kind}:${entry.path}`), [
    "file:AGENTS.md", "file:NOTES.md", "file:README.md", "directory:docs", "file:package.json", "directory:scratch", "directory:src",
  ]);
  assert.ok(top.entries.every((entry) => entry.kind === "directory" ? entry.size === undefined : entry.size > 0));
  assert.deepEqual(inspect("list", { path: "src" }).result.entries.map((entry) => entry.path), ["src/app.ts", "src/lib", "src/routes"]);
  const file = inspect("read", { path: "src/lib/port.ts" }).result;
  assert.match(file.text, /fallback = 8080/);
  assert.deepEqual([file.path, file.truncated, file.size], ["src/lib/port.ts", false, new TextEncoder().encode(file.text).length]);
  // The query is literal and case-insensitive; `path` scopes it.
  const found = inspect("search", { query: "PORT" }).result;
  assert.ok(found.matches.length > 3 && found.matches.every((match) => /port/i.test(match.text) && match.line >= 1));
  assert.deepEqual(inspect("search", { query: "fallback", path: "docs" }).result, { root: "/home/ada/lighthouse", matches: [], truncated: false });
  // A substring counts: "reports" holds "port".
  assert.deepEqual(inspect("search", { query: "port", path: "docs" }).result.matches.map((match) => [match.path, match.line]), [["docs/guide.md", 3]]);
  assert.deepEqual([...new Set(inspect("search", { query: "port", path: "src/lib" }).result.matches.map((match) => match.path))], ["src/lib/port.ts"]);
});

test("code.inspect diff is tracked changes against the last commit; untracked files stay in the status", () => {
  const { inspect } = codeSession();
  const diff = inspect("diff").result;
  assert.equal(diff.truncated, false);
  assert.match(diff.text, /^diff --git a\/src\/lib\/port\.ts b\/src\/lib\/port\.ts\n--- a\/src\/lib\/port\.ts\n\+\+\+ b\/src\/lib\/port\.ts\n@@ -1,7 \+1,7 @@\n-const fallback = 3000;\n\+const fallback = 8080;\n/);
  assert.ok(!diff.text.includes("scratch/todo.txt"));
});

test("code.inspect refuses what is not a look at a live workspace node, by code", () => {
  const { state, session, execution, on, inspect } = codeSession();
  const code = (answer) => answer.error?.code;
  assert.equal(code(inspect("write")), -32602);
  assert.equal(code(inspect("read")), -32602);
  assert.equal(code(inspect("search")), -32602);
  assert.equal(code(inspect("read", { path: "../etc/passwd" })), -32602);
  assert.equal(code(inspect("read", { path: "/etc/passwd" })), -32602);
  assert.equal(code(inspect("list", { path: ".git" })), -32602);
  assert.equal(code(inspect("read", { path: "src" })), -32602);
  assert.equal(code(inspect("list", { path: "README.md" })), -32602);
  assert.equal(code(inspect("read", { path: "nope.txt" })), -32602);
  assert.equal(code(session(request("code.inspect", { ...on("agent"), action: "summary" }))), -32602);
  assert.equal(code(session(request("code.inspect", { execution_id: "exec-none", node_id: "workspace", action: "summary" }))), -32602);
  session(request("execution.stop", { id: execution.id }));
  assert.equal(code(inspect("summary")), -32602);
  // Without the module the method is not there at all.
  state.modules = state.modules.filter((module) => !module.id.startsWith("regulus/code@"));
  assert.equal(code(inspect("summary")), -32601);
});

test("a folder without a repository has no git, with the reason, and an empty diff", () => {
  const { inspect } = codeSession("/home/ada/no-git");
  const summary = inspect("summary").result;
  assert.equal(summary.git, null);
  assert.equal(summary.git_reason, "not a git repository");
  assert.deepEqual(inspect("diff").result, { root: "/home/ada/no-git", text: "", truncated: false, git_reason: "not a git repository" });
  assert.equal(inspect("list").result.entries.length, 7);
});

test("a coding turn reads, asks to edit, and the edit shows in the file, the status and the diff once allowed", () => {
  const { session, execution, on, inspect, follow } = codeSession();
  const { turn } = session(request("agent.send", { ...on("agent"), text: "note the port range" })).result;
  const before = follow(asking);
  assert.deepEqual(before.activity.filter((item) => item.event.kind === "tool_call").map((item) => item.event.title), ["code_list_files", "code_read_file README.md"]);
  const asked = before.activity.find((item) => item.event.kind === "permission_request").event;
  assert.deepEqual([asked.tool, asked.title, asked.options.map((option) => option.id)], ["code_edit_file", "code_edit_file NOTES.md", ["allow_once", "allow_always", "reject_once"]]);
  // Nothing is written while the question waits.
  assert.ok(!inspect("read", { path: "NOTES.md" }).result.text.includes("note the port range"));
  const [pending] = session(request("human.list")).result;
  assert.deepEqual(pending.value.context, { tool: "code_edit_file", title: "code_edit_file NOTES.md", execution_id: execution.id, node_id: "agent", request_id: asked.id });
  assert.equal(pending.node_id, "decider");
  session(request("human.answer", { id: pending.id, decision: "approve", fields: { option: "allow_once" } }));
  const after = follow(ended);
  assert.equal(after.activity.find((item) => item.event.kind === "permission_resolved").event.option, "allow_once");
  assert.equal(after.activity.filter((item) => item.event.kind === "tool_update").pop().event.status, "completed");
  assert.match(inspect("read", { path: "NOTES.md" }).result.text, /- note the port range\n$/);
  assert.ok(inspect("summary").result.git.status.includes(" M NOTES.md\n"));
  assert.match(inspect("diff").result.text, /diff --git a\/NOTES\.md b\/NOTES\.md[\s\S]*\n\+- note the port range\n/);
  const reply = after.entries.find((entry) => entry.role === "agent");
  assert.equal(reply.turn, turn);
  assert.match(reply.text, /I added it to NOTES\.md\.$/);
});

test("a rejected edit changes nothing, and a cancelled turn takes its question with it", () => {
  const { session, on, inspect, follow } = codeSession();
  session(request("agent.send", { ...on("agent"), text: "first" }));
  follow(asking);
  const [pending] = session(request("human.list")).result;
  session(request("human.answer", { id: pending.id, decision: "reject", fields: {} }));
  const rejected = follow(ended);
  const update = rejected.activity.filter((item) => item.event.kind === "tool_update").pop().event;
  assert.equal(update.status, "failed");
  assert.match(update.content[0].content.text, /^refused: the person rejected it/);
  assert.ok(!inspect("summary").result.git.status.includes("NOTES.md"));
  assert.match(rejected.entries.find((entry) => entry.role === "agent").text, /I left NOTES\.md as it was\.$/);
  session(request("agent.send", { ...on("agent"), text: "second" }));
  follow(asking);
  assert.equal(session(request("human.list")).result.length, 1);
  assert.equal(session(request("agent.cancel", on("agent"))).result, null);
  assert.equal(session(request("human.list")).result.length, 0);
  assert.ok(!inspect("summary").result.git.status.includes("NOTES.md"));
});

test("a worker the agent starts is a child execution the delegate lists and closes", () => {
  const { state, session, execution, on, follow } = codeSession();
  assert.deepEqual(session(request("delegation.workers", on("workers"))).result, []);
  session(request("agent.send", { ...on("agent"), text: "ask a worker to read the docs" }));
  follow(asking);
  session(request("human.answer", { id: session(request("human.list")).result[0].id, decision: "approve", fields: { option: "allow_always" } }));
  const turn = follow(ended);
  assert.ok(turn.activity.some((item) => item.event.kind === "tool_call" && item.event.title === "worker_create"));
  const [worker] = session(request("delegation.workers", on("workers"))).result;
  assert.deepEqual(Object.keys(worker), ["worker", "kind", "name", "execution", "state", "pending_results", "last_output_at"]);
  assert.deepEqual([worker.worker, worker.kind, worker.state], ["w-1", "worker", "running"]);
  const tree = session(request("execution.tree", { execution_id: execution.id })).result;
  assert.deepEqual(tree.children.map((child) => [child.execution.id, child.execution.workflow_id, child.execution.caller, child.execution.tag]), [[worker.execution, "worker", "workers", "w-1"]]);
  // The worker's own conversation holds its brief, and its answer counts as a result waiting.
  const own = follow(ended, "worker", worker.execution);
  assert.match(own.entries.find((entry) => entry.role === "user").text, /^Look into: ask a worker/);
  assert.equal(session(request("delegation.workers", on("workers"))).result[0].pending_results, 1);
  // "Always allow" holds for the next turn: no second question.
  session(request("agent.send", { ...on("agent"), text: "and again" }));
  assert.ok(!asking(follow(ended)));
  const closed = session(request("delegation.close", { ...on("workers"), worker: "w-1" })).result;
  assert.equal(closed.state, "closed");
  assert.equal(state.executions.find((item) => item.id === worker.execution).state.status, "stopped");
  assert.equal(session(request("delegation.workers", on("agent"))).error.code, -32602);
  assert.equal(session(request("delegation.close", { ...on("workers"), worker: "w-9" })).error.code, -32003);
  state.modules = state.modules.filter((module) => !module.id.startsWith("regulus/delegation@"));
  assert.equal(session(request("delegation.workers", on("workers"))).error.code, -32601);
});
