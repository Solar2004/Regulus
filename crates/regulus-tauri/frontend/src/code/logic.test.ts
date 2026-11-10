import { describe, expect, test } from "vitest";
import { sample } from "@/composition/editor/fixtures.test-util";
import { historyRecords, reduceConversation } from "@/operate/conversation";
import type { NodeRow } from "@/operate/logic";
import type { Execution } from "@/operate/model";
import type { TemplateSummary } from "@/templates/model";
import { codeProfile, codeWorkflow, manifests, workerWorkflow } from "./fixtures.test-util";
import {
  activityOf, ancestorsOf, changeKind, codeAgents, codeModuleIds, codeSessions, codeTargets, codeTemplates, credentialState, directChildren, fileLines,
  folderName, harnessParts, isAbsolutePath, isCodeWorkflow, nodesAnswering, normalizeRoot, openPermissions, parseStatus, readDiff, readFile,
  readListing, readSearch, readSummary, readWorkers, rootOf, sameRoot, secretPrefix, sessionProblems, slugOf, splitVars, suggestProfileId, targetForRoot,
  treeRows, workspaceNodes, workStamp,
} from "./logic";

const execution = (id: string, over: Partial<Execution> = {}): Execution =>
  ({ id, profile_id: "code-app", workflow_id: "code", project_id: null, state: { status: "running" }, ...over });
const template = (name: string, requires: string[], over: Partial<TemplateSummary> = {}): TemplateSummary =>
  ({ name, description: "", path: `templates/${name}.toml`, vars: [], requires: requires.map((module) => ({ module, version: ">=0.1" })), ...over });

describe("what is a coding session", () => {
  test("a coding workflow has a node offering code.workspace and a direct agent, whatever their modules", () => {
    expect(isCodeWorkflow(codeWorkflow(), manifests())).toBe(true);
    expect(workspaceNodes(codeWorkflow()).map((node) => node.id)).toEqual(["workspace"]);
    // A worker has an agent and no workspace; a plain profile has neither.
    expect(isCodeWorkflow(workerWorkflow(), manifests())).toBe(false);
    expect(sample().workflows.some((workflow) => isCodeWorkflow(workflow, manifests()))).toBe(false);
    const renamed = codeWorkflow();
    renamed.nodes = renamed.nodes.map((node) => node.id === "workspace" ? { ...node, id: "repo", type: "anything", module_id: "someone/else@2" } : node);
    expect(workspaceNodes(renamed).map((node) => node.id)).toEqual(["repo"]);
  });

  test("a delegate takes agent.engine too, and is not an agent to talk to", () => {
    // By the manifests: only the module that declares agent.history holds a conversation.
    expect(codeAgents(codeWorkflow(), manifests()).map((node) => node.id)).toEqual(["agent"]);
    // Without manifests (modules not installed): the node that requires its engine.
    expect(codeAgents(codeWorkflow(), []).map((node) => node.id)).toEqual(["agent"]);
    expect(codeAgents(undefined, manifests())).toEqual([]);
  });

  test("targets come from saved profiles, with the folder the workspace node is set to", () => {
    const targets = codeTargets([sample(), codeProfile(), codeProfile("code-lib", null)], manifests());
    expect(targets.map((target) => [target.profileId, target.workflowId, target.workspaceId, target.root])).toEqual([
      ["code-app", "code", "workspace", "/home/ada/app"], ["code-lib", "code", "workspace", null],
    ]);
    expect(targets[0].agentIds).toEqual(["agent"]);
    expect(rootOf({ config: { root: "  " } })).toBeNull();
    expect(targetForRoot(targets, "/home/ada/app/")?.profileId).toBe("code-app");
    expect(targetForRoot(targets, "/home/ada/other")).toBeUndefined();
  });

  test("sessions are the executions of a target that nobody started as a child: live first, then the newest", () => {
    const targets = codeTargets([codeProfile()], manifests());
    const sessions = codeSessions([
      execution("exec-1", { state: { status: "stopped" } }), execution("exec-2"),
      execution("exec-3", { workflow_id: "worker", parent: "exec-2" }), execution("exec-4", { profile_id: "other" }),
      execution("exec-5", { state: { status: "failed", reason: "boom" } }), execution("exec-6", { parent: "exec-2" }),
    ], targets);
    expect(sessions.map((session) => [session.execution.id, session.live])).toEqual([["exec-2", true], ["exec-5", false], ["exec-1", false]]);
  });

  test("the problems of a session are the nodes that will not work", () => {
    const row = (id: string, state: NodeRow["state"]): NodeRow => ({ id, name: id, type: "t", kind: "other", state });
    const rows = [row("agent", { kind: "running" }), row("provider", { kind: "not-installed" }), row("native", { kind: "disabled", missing: ["model"] }), row("notes", { kind: "resource" }), row("idle", { kind: "unused" })];
    expect(sessionProblems(rows).map((item) => item.id)).toEqual(["provider", "native"]);
  });
});

describe("folders and profile ids", () => {
  test("a folder is compared without its trailing separators", () => {
    expect(normalizeRoot(" /home/ada/app// ")).toBe("/home/ada/app");
    expect(normalizeRoot("/")).toBe("/");
    expect(normalizeRoot("C:\\work\\app\\")).toBe("C:\\work\\app");
    expect(sameRoot("/a/b", "/a/b/")).toBe(true);
    expect(sameRoot("/a/b", "/a/B")).toBe(false);
  });

  test("only an absolute folder is offered to the core", () => {
    expect(["/srv/app", "C:\\work", "c:/work", "\\\\host\\share"].map(isAbsolutePath)).toEqual([true, true, true, true]);
    expect(["app", "./app", "~/app", ""].map(isAbsolutePath)).toEqual([false, false, false, false]);
  });

  test("the profile id comes from the folder's name and never repeats one that is taken", () => {
    expect(folderName("/home/ada/My App/")).toBe("My App");
    expect(folderName("C:\\work\\api")).toBe("api");
    expect(slugOf("My App (v2)")).toBe("my-app-v2");
    expect(slugOf("..hidden..")).toBe("hidden");
    expect(suggestProfileId("/home/ada/My App", [], "code")).toBe("code-my-app");
    expect(suggestProfileId("/home/ada/app", ["code-app", "code-app-2"], "code")).toBe("code-app-3");
    // A folder whose name leaves nothing still gets an id.
    expect(suggestProfileId("/", [], "code")).toBe("code");
    expect(suggestProfileId("/日本", [], "")).toBe("workspace");
  });
});

describe("the setup form", () => {
  test("the coding module is the one whose manifest offers code.workspace", () => {
    expect(codeModuleIds(manifests())).toEqual(["regulus/code@0.1.0"]);
    expect(codeModuleIds([])).toEqual([]);
  });

  test("templates: the one named first, then any that requires the coding module", () => {
    const all = [template("ralph", ["regulus/process"]), template("my-code", ["regulus/code", "regulus/agent"]), template("regulus-code", ["regulus/code"])];
    expect(codeTemplates(all, manifests(), "regulus-code").map((item) => item.name)).toEqual(["regulus-code", "my-code"]);
    // Without the module the named template still shows, so its missing modules can be said.
    expect(codeTemplates(all, [], "regulus-code").map((item) => item.name)).toEqual(["regulus-code"]);
    expect(codeTemplates(all, [], "nope")).toEqual([]);
  });

  test("the form gives the folder and the provider a place; every other var stays reachable", () => {
    const vars = ["model", "system", "root", "credential", "max_workers", "preset", "base_url"].map((name) => ({ name, description: "" }));
    const split = splitVars(vars);
    expect(split.root?.name).toBe("root");
    expect(split.provider.map((item) => item.name)).toEqual(["preset", "base_url", "credential", "model"]);
    expect(split.more.map((item) => item.name)).toEqual(["system", "max_workers"]);
    expect(splitVars([{ name: "folder", description: "" }])).toEqual({ root: null, provider: [], more: [{ name: "folder", description: "" }] });
  });

  test("a credential is a reference to a secret the core has, has not, or not a reference", () => {
    expect(credentialState("", ["k"])).toEqual({ kind: "none" });
    expect(credentialState("secret:openrouter", ["openrouter"])).toEqual({ kind: "stored", name: "openrouter" });
    expect(credentialState(" secret:gone ", ["openrouter"])).toEqual({ kind: "missing", name: "gone" });
    expect(credentialState("secret:any", null)).toEqual({ kind: "unknown", name: "any" });
    expect(credentialState("sk-live-1234", [])).toEqual({ kind: "malformed" });
    // The prefix alone names no secret.
    expect(credentialState(secretPrefix, [])).toEqual({ kind: "malformed" });
  });
});

describe("the parts of a harness", () => {
  test("found by the wires: agent, engine, model, workspace, decider, workers", () => {
    const flow = codeWorkflow();
    const workers = nodesAnswering(flow, manifests(), "delegation.workers").map((node) => node.id);
    expect(workers).toEqual(["workers"]);
    expect(harnessParts(flow, "agent", workers).map((part) => [part.role, part.nodeId])).toEqual([
      ["agent", "agent"], ["engine", "native"], ["model", "provider"], ["workspace", "workspace"], ["decider", "decider"], ["workers", "workers"],
    ]);
    expect(harnessParts(flow, "nobody", workers)).toEqual([]);
    expect(harnessParts(undefined, "agent", [])).toEqual([]);
  });

  test("a harness with pieces unwired lists what is there", () => {
    const flow = codeWorkflow();
    flow.wires = flow.wires.filter((wire) => wire.id !== "decider-agent" && wire.id !== "provider-native");
    expect(harnessParts(flow, "agent", []).map((part) => part.role)).toEqual(["agent", "engine", "workspace"]);
    // Without the manifest nobody is known to answer the workers method.
    expect(nodesAnswering(flow, [], "delegation.workers")).toEqual([]);
  });
});

describe("code.inspect answers", () => {
  test("porcelain status reads as changes, renames with where they came from", () => {
    const changes = parseStatus(" M src/app.ts\nA  src/new.ts\n?? notes.md\nR  old.ts -> src/moved.ts\nUU both.ts\n D gone.ts\n?? \"with space.md\"\n");
    expect(changes.map((change) => [change.path, changeKind(change)])).toEqual([
      ["src/app.ts", "modified"], ["src/new.ts", "added"], ["notes.md", "untracked"], ["src/moved.ts", "renamed"], ["both.ts", "conflicted"],
      ["gone.ts", "deleted"], ["with space.md", "untracked"],
    ]);
    expect(changes[3].from).toBe("old.ts");
    // A file whose name holds an arrow is not a rename.
    expect(parseStatus("?? a -> b.txt")[0]).toMatchObject({ path: "a -> b.txt", from: null });
  });

  test("status also reads as lines or as objects", () => {
    expect(parseStatus(["?? a.txt", " M b.txt"]).map((change) => change.path)).toEqual(["a.txt", "b.txt"]);
    expect(parseStatus(" M a.txt\0?? b.txt\0").map((change) => change.path)).toEqual(["a.txt", "b.txt"]);
    expect(parseStatus([{ path: "a.txt", status: "??" }, { path: "b.txt", index: "M", worktree: " " }, { status: "M" }]).map((change) => [change.path, changeKind(change)]))
      .toEqual([["a.txt", "untracked"], ["b.txt", "modified"]]);
    expect(parseStatus(null)).toEqual([]);
  });

  test("a summary without git is a folder without git, not a failure", () => {
    const repo = readSummary({ root: "/home/ada/app", git: { branch: "main", status: " M a.ts\n" }, tools: ["code_read_file", 3, "code_search"] });
    expect(repo).toMatchObject({ root: "/home/ada/app", tools: ["code_read_file", "code_search"], git: { available: true, branch: "main", reason: null } });
    expect(repo?.git.changes).toHaveLength(1);
    expect(readSummary({ root: "/tmp/x", git: null, tools: [] })?.git).toEqual({ available: false, branch: null, changes: [], reason: null });
    expect(readSummary({ root: "/tmp/x" })?.git.available).toBe(false);
    expect(readSummary({ root: "/tmp/x", git: { reason: "not a git repository" } })?.git).toEqual({ available: false, branch: null, changes: [], reason: "not a git repository" });
    // A repository with no commit yet: no branch, still git.
    expect(readSummary({ root: "/tmp/x", git: { branch: null, status: "" } })?.git).toMatchObject({ available: true, branch: null, changes: [] });
    expect(readSummary({ git: null })).toBeNull();
    expect(readSummary("nope")).toBeNull();
  });

  test("a listing keeps what is an entry and reads the folder kinds it knows", () => {
    const listing = readListing({ root: "/r", path: "src/", entries: [{ path: "src/a.ts", kind: "file" }, { path: "src/lib/", kind: "directory" }, { path: "src/x", kind: "symlink" }, { kind: "file" }, "junk"], truncated: true });
    expect(listing).toEqual({ root: "/r", path: "src", truncated: true, entries: [{ path: "src/a.ts", kind: "file" }, { path: "src/lib", kind: "directory" }, { path: "src/x", kind: "file" }] });
    expect(readListing({ root: "/r", entries: [] })).toMatchObject({ path: "", truncated: false });
    expect(readListing({ root: "/r" })).toBeNull();
  });

  test("read, search and diff answers", () => {
    expect(readFile({ root: "/r", path: "a.ts", text: "x", truncated: false })).toEqual({ root: "/r", path: "a.ts", text: "x", truncated: false });
    expect(readFile({ root: "/r", path: "a.ts" })).toBeNull();
    expect(readSearch({ root: "/r", matches: [{ path: "a.ts", line: 3, text: "let x" }, { path: "b.ts", line: 0 }, { line: 2 }], truncated: true }))
      .toEqual({ root: "/r", truncated: true, matches: [{ path: "a.ts", line: 3, text: "let x" }, { path: "b.ts", line: 1, text: "" }] });
    expect(readSearch({})).toBeNull();
    expect(readDiff({ root: "/r", text: "", truncated: false })).toEqual({ root: "/r", text: "", truncated: false });
    expect(readDiff({ root: "/r" })).toBeNull();
  });
});

describe("the file tree", () => {
  test("a folder's children: folders first, then by name; a deeper entry counts as its top folder", () => {
    const entries = [{ path: "b.ts", kind: "file" as const }, { path: "src", kind: "directory" as const }, { path: "a.ts", kind: "file" as const }, { path: "docs/guide/intro.md", kind: "file" as const }];
    expect(directChildren("", entries)).toEqual([{ path: "docs", kind: "directory" }, { path: "src", kind: "directory" }, { path: "a.ts", kind: "file" }, { path: "b.ts", kind: "file" }]);
    expect(directChildren("docs", entries)).toEqual([{ path: "docs/guide", kind: "directory" }]);
    expect(directChildren("src", [{ path: "src", kind: "directory" }, { path: "srcx/a", kind: "file" }])).toEqual([]);
  });

  test("the rows on screen follow what is open and say what is loaded", () => {
    const listings = { "": [{ path: "src", kind: "directory" as const }, { path: "docs", kind: "directory" as const }, { path: "a.ts", kind: "file" as const }], src: [{ path: "src/lib", kind: "directory" as const }, { path: "src/b.ts", kind: "file" as const }] };
    expect(treeRows(listings, new Set()).map((row) => row.path)).toEqual(["src", "docs", "a.ts"]);
    const rows = treeRows(listings, new Set(["src", "docs", "src/lib"]));
    expect(rows.map((row) => [row.path, row.depth, row.open, row.loaded])).toEqual([
      ["src", 0, true, true], ["src/lib", 1, true, false], ["src/b.ts", 1, false, false], ["docs", 0, true, false], ["a.ts", 0, false, false],
    ]);
    expect(ancestorsOf("src/lib/x.ts")).toEqual(["src", "src/lib"]);
    expect(ancestorsOf("x.ts")).toEqual([]);
  });

  test("a file's lines drop the empty one after a final newline", () => {
    expect(fileLines("a\nb\n")).toEqual(["a", "b"]);
    expect(fileLines("a\r\n\r\nb")).toEqual(["a", "", "b"]);
    expect(fileLines("")).toEqual([""]);
  });
});

describe("workers", () => {
  test("delegation.workers reads as the module's view; a row without an id drops", () => {
    expect(readWorkers([
      { worker: "w-1", kind: "worker", name: "tests", execution: "exec-9", state: "running", pending_results: 2, last_output_at: 1700, engine: "native" },
      { worker: "w-2", kind: "worker", name: "", execution: "", state: "starting", pending_results: 0, last_output_at: null }, { name: "ghost" },
    ])).toEqual([
      { worker: "w-1", kind: "worker", name: "tests", execution: "exec-9", state: "running", pending_results: 2, last_output_at: 1700, engine: "native" },
      { worker: "w-2", kind: "worker", name: "", execution: "", state: "starting", pending_results: 0, last_output_at: null },
    ]);
    expect(readWorkers({ workers: [] })).toEqual([]);
  });
});

describe("activity", () => {
  const event = (seq: number, turn: string, fields: Record<string, unknown>) => ({ seq, at: seq * 10, turn, event: { seq, at: seq * 10, ...fields } });
  const records = historyRecords({
    entries: [
      { seq: 1, at: 10, role: "user", text: "fix it", turn: "t-1", execution_id: "e" },
      { seq: 9, at: 90, role: "agent", text: "done", turn: "t-1", execution_id: "e" },
      { seq: 10, at: 100, role: "system", text: "provider unreachable", turn: "t-2", execution_id: "e" },
    ],
    activity: [
      event(2, "t-1", { kind: "thought", text: "hm" }),
      event(3, "t-1", { kind: "tool_call", id: "r", title: "code_read_file src/a.ts", tool_kind: "read", status: "in_progress" }),
      event(4, "t-1", { kind: "tool_update", id: "r", status: "completed" }),
      event(5, "t-1", { kind: "permission_request", id: "p", title: "code_edit_file src/a.ts", options: [{ id: "allow_once", kind: "allow_once", name: "Allow once" }] }),
      event(6, "t-1", { kind: "permission_resolved", id: "p", option: "allow_once" }),
      event(7, "t-1", { kind: "permission_request", id: "q", title: "code_run_command", options: [] }),
      event(8, "t-1", { kind: "turn_ended", stop_reason: "end_turn" }),
    ],
  });

  test("what the agent did, newest first, without what was said", () => {
    expect(activityOf(records).map((item) => [item.kind, item.title, item.note, item.status, item.at])).toEqual([
      ["system", "provider unreachable", null, null, 100],
      ["ended", "", "end_turn", null, 80],
      ["permission", "code_run_command", null, "waiting", 70],
      ["permission", "code_edit_file src/a.ts", "Allow once", "answered", 50],
      ["tool", "code_read_file src/a.ts", "read", "completed", 30],
    ]);
  });

  test("open permissions are the ones nobody answered", () => {
    expect(openPermissions(reduceConversation(records, "turn")).map((item) => item.id)).toEqual(["q"]);
  });

  test("the work stamp moves when a tool finishes or a turn ends", () => {
    expect(workStamp(records)).toBe(8);
    expect(workStamp(records.filter((record) => record.seq < 4))).toBe(0);
    expect(workStamp(records.filter((record) => record.seq < 5))).toBe(4);
  });
});
