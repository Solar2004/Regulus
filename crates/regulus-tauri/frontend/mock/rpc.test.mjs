import { test } from "node:test";
import assert from "node:assert/strict";
import { createSession, createState } from "./rpc.mjs";
import fixture from "./profile.json" with { type: "json" };
import manifests from "./manifests.json" with { type: "json" };

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
  assert.equal(session(request("module.list")).result.length, fixture.modules.length + manifests.length);
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
