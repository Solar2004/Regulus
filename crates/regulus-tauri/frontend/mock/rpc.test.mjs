import { test } from "node:test";
import assert from "node:assert/strict";
import { createSession, createState } from "./rpc.mjs";
import fixture from "./profile.json" with { type: "json" };
import manifests from "./manifests.json" with { type: "json" };
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
  assert.equal(session(request("module.list")).result.length, fixture.modules.length + manifests.length + operate.manifests.length + settings.modules.length);
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
  assert.deepEqual(list.map((template) => template.name), ["echo", "chat", "ralph", "broken"]);
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
