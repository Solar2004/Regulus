import { test } from "node:test";
import assert from "node:assert/strict";
import { createSession } from "./rpc.mjs";
import fixture from "./profile.json" with { type: "json" };

const request = (method, params = {}, id = 1) => JSON.stringify({ jsonrpc: "2.0", id, method, params });
const negotiatedSession = () => {
  const session = createSession();
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
  assert.equal(session(request("module.list")).result.length, fixture.modules.length);
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
