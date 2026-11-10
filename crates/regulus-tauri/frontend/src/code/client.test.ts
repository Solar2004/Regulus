import { expect, test } from "vitest";
import type { Rpc } from "@/operate/client";
import { codeApi, codeMethods, UnreadableAnswer } from "./client";

function recorder(answers: Record<string, unknown>) {
  const calls: [string, Record<string, unknown>][] = [];
  const rpc: Rpc = { request: async <T,>(method: string, params: Record<string, unknown> = {}) => { calls.push([method, params]); return answers[String(params.action ?? method)] as T; } };
  return { calls, api: codeApi(rpc) };
}

test("every inspect action is one code.inspect call on the session's workspace node", async () => {
  const { calls, api } = recorder({
    summary: { root: "/r", git: null, tools: [] }, list: { root: "/r", path: "", entries: [], truncated: false }, read: { root: "/r", path: "a", text: "", truncated: false },
    search: { root: "/r", matches: [], truncated: false }, diff: { root: "/r", text: "", truncated: false },
  });
  await api.summary("exec-1", "workspace");
  await api.list("exec-1", "workspace", "");
  await api.list("exec-1", "workspace", "src");
  await api.read("exec-1", "workspace", "src/a.ts");
  await api.search("exec-1", "workspace", "port");
  await api.search("exec-1", "workspace", "port", "src");
  await api.diff("exec-1", "workspace");
  const on = { execution_id: "exec-1", node_id: "workspace" };
  expect(calls).toEqual([
    [codeMethods.inspect, { ...on, action: "summary" }], [codeMethods.inspect, { ...on, action: "list" }], [codeMethods.inspect, { ...on, action: "list", path: "src" }],
    [codeMethods.inspect, { ...on, action: "read", path: "src/a.ts" }], [codeMethods.inspect, { ...on, action: "search", query: "port" }],
    [codeMethods.inspect, { ...on, action: "search", query: "port", path: "src" }], [codeMethods.inspect, { ...on, action: "diff" }],
  ]);
});

test("an answer that is not the action's shape is said, not drawn as empty", async () => {
  const { api } = recorder({ summary: { tools: [] }, diff: null });
  await expect(api.summary("e", "w")).rejects.toBeInstanceOf(UnreadableAnswer);
  await expect(api.diff("e", "w")).rejects.toMatchObject({ action: "diff" });
});

test("workers are the delegate's own methods, on its node", async () => {
  const { calls, api } = recorder({ [codeMethods.workers]: [{ worker: "w-1", state: "running" }], [codeMethods.workerClose]: { worker: "w-1", state: "closed" } });
  expect((await api.workers("exec-1", "workers")).map((worker) => worker.worker)).toEqual(["w-1"]);
  await api.closeWorker("exec-1", "workers", "w-1");
  expect(calls).toEqual([
    [codeMethods.workers, { execution_id: "exec-1", node_id: "workers" }],
    [codeMethods.workerClose, { execution_id: "exec-1", node_id: "workers", worker: "w-1" }],
  ]);
});
