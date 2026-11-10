// The view's logic against the mock core, end to end over JSON-RPC: what the mock serves
// (the shapes of `regulus-proto`, `crates/modules/code` and `delegation`) must read as the
// view expects. No socket: one mock session, asked as the shared client asks.
import { expect, test } from "vitest";
import type { Manifest, Profile } from "@/composition/model";
import { RpcError } from "@/core/connection";
import { operateApi, type Rpc } from "@/operate/client";
import { decisionFor, historyRecords, reduceConversation } from "@/operate/conversation";
import { nodeRows, unavailableOf } from "@/operate/logic";
import type { Execution } from "@/operate/model";
import { templatesApi } from "@/templates/client";
import { requireState } from "@/templates/logic";
import { createSession, createState, type MockState } from "../../mock/rpc.mjs";
import { codeApi, codeMethods } from "./client";
import { diffStats, parseDiff } from "./diff";
import {
  activityOf, changeKind, codeModuleIds, codeSessions, codeTargets, codeTemplates, directChildren, harnessParts, nodesAnswering, openPermissions,
  sessionProblems, splitVars, suggestProfileId, targetForRoot, workStamp,
} from "./logic";

function connect(state: MockState = createState()) {
  const session = createSession(state);
  let id = 0;
  const rpc: Rpc = {
    request: async <T,>(method: string, params: Record<string, unknown> = {}) => {
      const response = session(JSON.stringify({ jsonrpc: "2.0", id: ++id, method, params }))!;
      if (response.error) throw new RpcError(response.error.message, response.error.code);
      return response.result as T;
    },
  };
  return { state, rpc, code: codeApi(rpc), operate: operateApi(rpc), templating: templatesApi(rpc) };
}

const ROOT = "/home/ada/lighthouse";

/** Hello, then what the view loads before anything else. */
async function loaded(state?: MockState) {
  const line = connect(state);
  await line.rpc.request("hello", { client: "regulus-gui", proto: "1.0" });
  const [manifests, modules, templates] = await Promise.all([line.code.manifests(), line.operate.modules(), line.templating.list()]);
  return { ...line, manifests, modules, templates };
}

/** Sets a session up as the view does: the template applied on a folder, then started. */
async function started(root = ROOT) {
  const line = await loaded();
  const [template] = codeTemplates(line.templates, line.manifests, "regulus-code");
  const profiles = await line.operate.profiles();
  const applied = await line.templating.apply({ template: template.name, vars: { root }, id: suggestProfileId(root, profiles.map((profile) => profile.id), "code") });
  const execution = await line.operate.start(applied.profile_id, undefined, 1000);
  const [target] = codeTargets(await line.operate.profiles(), line.manifests);
  return { ...line, template, applied, execution, target };
}

/** Reads the agent's conversation on until `done`, one scripted step per read, as the panel's polling does. */
async function follow(line: Awaited<ReturnType<typeof started>>, done: (history: ReturnType<typeof historyRecords>) => boolean) {
  let records: ReturnType<typeof historyRecords> = [];
  for (let since = 0, rounds = 0; rounds < 40 && !done(records); rounds++) {
    const page = await line.operate.history(line.execution.id, line.target.agentIds[0], since, 200);
    records = [...records, ...historyRecords(page)];
    since = page.next;
  }
  return records;
}

test("setup: the coding module and its template are found by what they offer and require", async () => {
  const { manifests, modules, templates } = await loaded();
  expect(codeModuleIds(manifests)).toEqual(["regulus/code@0.1.0"]);
  const [template, ...others] = codeTemplates(templates, manifests, "regulus-code");
  expect(others).toEqual([]);
  expect(template.requires.map((require) => requireState(require, modules))).toEqual(Array(template.requires.length).fill("installed"));
  const vars = splitVars(template.vars);
  expect(vars.root?.name).toBe("root");
  expect(vars.provider.map((item) => item.name)).toEqual(["preset", "base_url", "credential", "model"]);
  expect(vars.more.length).toBeGreaterThan(0);
});

test("setup without the coding module says what is missing instead of offering a session", async () => {
  const state = createState();
  state.modules = (state.modules as { id: string }[]).filter((module) => !module.id.startsWith("regulus/code@"));
  state.manifests = (state.manifests as Manifest[]).filter((manifest) => !manifest.module.id.startsWith("regulus/code@"));
  const { manifests, modules, templates, templating } = await loaded(state);
  expect(codeModuleIds(manifests)).toEqual([]);
  // The template is still found by its name, so its missing module can be named.
  const [template] = codeTemplates(templates, manifests, "regulus-code");
  expect(template.requires.filter((require) => requireState(require, modules) === "missing").map((require) => require.module)).toEqual(["regulus/code"]);
  await expect(templating.apply({ template: template.name, vars: { root: ROOT } })).rejects.toMatchObject({ code: -32006 });
});

test("a session: the profile of the folder, its execution, and the boxes it is made of", async () => {
  const line = await started();
  const { target, execution, manifests, modules, operate } = line;
  expect(target).toMatchObject({ profileId: line.applied.profile_id, workflowId: "code", workspaceId: "workspace", agentIds: ["agent"], root: ROOT });
  expect(line.applied.profile_id).toBe(suggestProfileId(ROOT, [], "code"));
  const profiles = await operate.profiles();
  expect(targetForRoot(codeTargets(profiles, manifests), `${ROOT}/`)?.profileId).toBe(target.profileId);
  const executions = await operate.executions(null);
  expect(codeSessions(executions, [target]).map((session) => [session.execution.id, session.live])).toEqual([[execution.id, true]]);
  const workflow = profiles.find((profile: Profile) => profile.id === target.profileId)!.workflows.find((item) => item.id === target.workflowId)!;
  const workers = nodesAnswering(workflow, manifests, codeMethods.workers).map((node) => node.id);
  expect(harnessParts(workflow, "agent", workers).map((part) => `${part.role}:${part.nodeId}`)).toEqual([
    "agent:agent", "engine:native", "model:provider", "workspace:workspace", "decider:decider", "workers:workers",
  ]);
  expect(sessionProblems(nodeRows(workflow, execution, modules))).toEqual([]);
});

test("the workspace: summary, tree, file, search and the diff split into files", async () => {
  const { code, execution, target } = await started();
  const summary = await code.summary(execution.id, target.workspaceId);
  expect(summary.root).toBe(ROOT);
  expect(summary.git).toMatchObject({ available: true, branch: "main", reason: null });
  expect(summary.git.changes.map((change) => [change.path, changeKind(change)])).toEqual([["scratch/todo.txt", "untracked"], ["src/lib/port.ts", "modified"]]);
  const listing = await code.list(execution.id, target.workspaceId, "");
  expect(directChildren("", listing.entries).map((entry) => entry.path)).toEqual(["docs", "scratch", "src", "AGENTS.md", "NOTES.md", "package.json", "README.md"]);
  expect((await code.read(execution.id, target.workspaceId, "src/lib/port.ts")).text).toContain("fallback = 8080");
  expect((await code.search(execution.id, target.workspaceId, "FALLBACK", "src")).matches.map((match) => `${match.path}:${match.line}`)).toEqual(["src/lib/port.ts:1", "src/lib/port.ts:3", "src/lib/port.ts:6"]);
  const files = parseDiff((await code.diff(execution.id, target.workspaceId)).text);
  expect(files.map((file) => [file.path, file.status, file.additions, file.deletions])).toEqual([["src/lib/port.ts", "modified", 3, 3]]);
  expect(diffStats(files)).toEqual({ files: 1, additions: 3, deletions: 3 });
});

test("a folder without a repository is a state with its reason, and its files still read", async () => {
  const { code, execution, target } = await started("/home/ada/no-git");
  const summary = await code.summary(execution.id, target.workspaceId);
  expect(summary.git).toMatchObject({ available: false, reason: "not a git repository", changes: [] });
  const diff = await code.diff(execution.id, target.workspaceId);
  expect([diff.text, diff.git_reason]).toEqual(["", "not a git repository"]);
  expect((await code.list(execution.id, target.workspaceId, "")).entries.length).toBeGreaterThan(0);
});

test("without the module, or once the session stops, the workspace says so by code", async () => {
  const line = await started();
  await line.operate.stop(line.execution.id);
  const stopped = await line.code.summary(line.execution.id, line.target.workspaceId).catch((error: unknown) => error);
  expect(stopped).toMatchObject({ code: -32602 });
  expect(unavailableOf(stopped)).toBeNull();
  line.state.modules = (line.state.modules as { id: string }[]).filter((module) => !module.id.startsWith("regulus/code@"));
  const absent = await line.code.summary(line.execution.id, line.target.workspaceId).catch((error: unknown) => error);
  expect(unavailableOf(absent)).toBe("not-installed");
});

test("a turn: the permission card finds its decision, the answer lets the edit through, and the workspace is asked again", async () => {
  const line = await started();
  const { operate, code, execution, target } = line;
  const agent = target.agentIds[0];
  await operate.agentSend(execution.id, agent, "note the port range");
  const asking = await follow(line, (records) => openPermissions(reduceConversation(records, "turn")).length > 0);
  const [request] = openPermissions(reduceConversation(asking, "turn"));
  expect(request).toMatchObject({ tool: "code_edit_file", title: "code_edit_file NOTES.md" });
  const decision = decisionFor(await operate.decisions(), execution.id, agent, request.id)!;
  expect(decision.kind).toBe("decision");
  const before = workStamp(asking);
  await operate.answer(decision.id, "approve", { option: "allow_once" }, null);
  const done = await follow(line, (records) => reduceConversation(records, "turn").some((turn) => !turn.open && turn.items.some((item) => item.type === "ended")));
  // A tool finished and the turn ended since: the view looks at the workspace again.
  expect(workStamp(done)).toBeGreaterThan(before);
  expect(openPermissions(reduceConversation(done, "turn"))).toEqual([]);
  expect(activityOf(done).filter((item) => item.kind !== "ended").map((item) => [item.kind, item.title, item.status])).toEqual([
    ["tool", "code_edit_file NOTES.md", "completed"], ["permission", "code_edit_file NOTES.md", "answered"],
    ["tool", "code_read_file README.md", "completed"], ["tool", "code_list_files", "completed"],
  ]);
  const summary = await code.summary(execution.id, target.workspaceId);
  expect(summary.git.changes.map((change) => change.path)).toEqual(["NOTES.md", "scratch/todo.txt", "src/lib/port.ts"]);
  expect(parseDiff((await code.diff(execution.id, target.workspaceId)).text).map((file) => [file.path, file.additions])).toEqual([["NOTES.md", 1], ["src/lib/port.ts", 3]]);
});

test("workers: listed from the delegate node, each a child execution with its own conversation", async () => {
  const line = await started();
  const { operate, code, execution, target } = line;
  await operate.agentSend(execution.id, target.agentIds[0], "have a worker read the docs");
  const asking = await follow(line, (records) => openPermissions(reduceConversation(records, "turn")).length > 0);
  const [request] = openPermissions(reduceConversation(asking, "turn"));
  await operate.answer(decisionFor(await operate.decisions(), execution.id, target.agentIds[0], request.id)!.id, "reject", {}, null);
  await follow(line, (records) => reduceConversation(records, "turn").some((turn) => turn.items.some((item) => item.type === "ended")));
  const [worker] = await code.workers(execution.id, "workers");
  expect(worker).toMatchObject({ worker: "w-1", kind: "worker", state: "running" });
  const executions = await operate.executions(null);
  const child = executions.find((item: Execution) => item.id === worker.execution)!;
  expect([child.parent, child.caller, child.tag]).toEqual([execution.id, "workers", "w-1"]);
  // A worker is not a session of its own: it shows under the one that started it.
  expect(codeSessions(executions, [target]).map((session) => session.execution.id)).toEqual([execution.id]);
  await code.closeWorker(execution.id, "workers", "w-1");
  expect((await code.workers(execution.id, "workers"))[0].state).toBe("closed");
});
