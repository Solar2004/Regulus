// What the Regulus Code view calls beyond Operate: `code.inspect` of the module that
// offers `code.workspace@1` and the delegate's `delegation.workers` / `delegation.close`,
// mirrored with canned data from code.json. Shapes and errors follow
// `crates/modules/code/src/contract.rs` and `crates/modules/delegation`; the mock adds no
// method of its own. The manifests in code.json keep the real ports and a subset of the
// real fields.
//
// The folder is one sample tree per root, in memory, with a `tracked` state (the last
// commit) and a `working` one: the coding agent's scripted turn reads it and, when a person
// allows it, edits NOTES.md, so status and diff change as they would. A root whose last
// folder is `no-git` has no repository.
import extra from "./code.json" with { type: "json" };
import { LIVE, advanceExecution, askPermission, baseId, conversationOf, failure, hasModule, liveNode, ok, queueTurn, workflowOf } from "./operate.mjs";

const CODE = "regulus/code";
const DELEGATION = "regulus/delegation";
const NO_GIT = "no-git";
const NOTES = "NOTES.md";
const offers = (node, capability) => (node?.ports ?? []).some((port) => port.direction === "output" && port.capability.split("@")[0] === capability);
const moduleOf = (node) => baseId(node?.module_id ?? "");
const lines = (text) => text.split("\n");
const bytes = (text) => new TextEncoder().encode(text).length;

/** What `createState` adds: the modules, the template, the folders and the workers. */
export function codeState() {
  return { codeManifests: structuredClone(extra.manifests), codeTemplates: structuredClone(extra.templates), folders: new Map(), workers: new Map(), scripts: [codeTurn] };
}

function folderOf(state, root) {
  if (!state.folders.has(root)) {
    state.folders.set(root, { tracked: new Map(Object.entries(extra.workspace.tracked)), working: new Map(Object.entries(extra.workspace.working)) });
  }
  return state.folders.get(root);
}
const hasGit = (root) => root.split("/").filter(Boolean).pop() !== NO_GIT;
const numberOf = (node, field, fallback) => typeof node.config?.[field] === "number" ? node.config[field] : fallback;

/** The workspace node of a workflow: the one that offers `code.workspace`. */
const workspaceOf = (workflow) => (workflow?.nodes ?? []).find((node) => offers(node, "code.workspace"));

// ── git, as the module reads it ────────────────────────────────────────────────────────

/** Porcelain v1 lines of the folder, by path. */
function changesOf(folder) {
  const paths = [...new Set([...folder.tracked.keys(), ...folder.working.keys()])].sort();
  return paths.flatMap((path) => {
    const before = folder.tracked.get(path);
    const after = folder.working.get(path);
    if (before === after) return [];
    return [{ path, code: before === undefined ? "??" : after === undefined ? " D" : " M" }];
  });
}

const CONTEXT = 3;

/** The lines of two texts as kept, removed and added ones, by their longest common subsequence. */
function lineChanges(old, now) {
  const common = Array.from({ length: old.length + 1 }, () => new Array(now.length + 1).fill(0));
  for (let i = old.length - 1; i >= 0; i--) for (let j = now.length - 1; j >= 0; j--) {
    common[i][j] = old[i] === now[j] ? common[i + 1][j + 1] + 1 : Math.max(common[i + 1][j], common[i][j + 1]);
  }
  const changes = [];
  let i = 0;
  let j = 0;
  while (i < old.length || j < now.length) {
    if (i < old.length && j < now.length && old[i] === now[j]) changes.push({ mark: " ", text: old[i], old: i++, now: j++ });
    else if (i < old.length && (j === now.length || common[i + 1][j] >= common[i][j + 1])) changes.push({ mark: "-", text: old[i], old: i++, now: j });
    else changes.push({ mark: "+", text: now[j], old: i, now: j++ });
  }
  return changes;
}

/** One file's unified diff as git prints it: hunks of what changed with three lines around. */
function fileDiff(path, before, after) {
  const head = [`diff --git a/${path} b/${path}`];
  if (after === undefined) head.push("deleted file mode 100644");
  head.push(`--- a/${path}`, after === undefined ? "+++ /dev/null" : `+++ b/${path}`);
  const changes = lineChanges(lines(before.replace(/\n$/, "")), after === undefined ? [] : lines(after.replace(/\n$/, "")));
  const span = (at, count) => count === 0 ? `${at},0` : count === 1 ? `${at + 1}` : `${at + 1},${count}`;
  const hunks = [];
  let at = 0;
  while (at < changes.length) {
    while (at < changes.length && changes[at].mark === " ") at++;
    if (at === changes.length) break;
    const from = Math.max(0, at - CONTEXT);
    // The hunk goes on while the next change is within two contexts of the last one.
    let last = at;
    for (let next = at; next < changes.length && next - last <= CONTEXT * 2; next++) if (changes[next].mark !== " ") last = next;
    const to = Math.min(changes.length, last + CONTEXT + 1);
    const body = changes.slice(from, to);
    const count = (side) => body.filter((change) => change.mark !== side).length;
    hunks.push(`@@ -${span(body[0].old, count("+"))} +${span(body[0].now, count("-"))} @@`, ...body.map((change) => `${change.mark}${change.text}`));
    at = to;
  }
  return [...head, ...hunks].join("\n");
}

/** Tracked changes against the last commit; untracked files are not in a diff. */
function diffOf(folder) {
  const parts = changesOf(folder).filter((change) => change.code !== "??").map((change) => fileDiff(change.path, folder.tracked.get(change.path), folder.working.get(change.path)));
  return parts.length ? `${parts.join("\n")}\n` : "";
}

// ── code.inspect ───────────────────────────────────────────────────────────────────────

/** A path as the module takes it: relative, inside the root, never `.git`. */
function cleanPath(raw) {
  if (raw === undefined || raw === null || raw === "") return { path: "" };
  if (typeof raw !== "string") return failure(-32602, "`path` must be a text");
  const parts = raw.split("/").filter((part) => part && part !== ".");
  if (raw.startsWith("/") || parts.includes("..")) return failure(-32602, `\`${raw}\` is outside the workspace`);
  if (parts.includes(".git")) return failure(-32602, `\`${raw}\` is protected`);
  return { path: parts.join("/") };
}

function inspect(node, folder, root, params) {
  const maxEntries = numberOf(node, "inspect_max_entries", 2000);
  const maxBytes = numberOf(node, "inspect_max_bytes", 1048576);
  const files = [...folder.working.keys()].sort();
  const isFolder = (path) => path === "" || files.some((file) => file.startsWith(`${path}/`));
  const git = hasGit(root);
  switch (params.action) {
    case "summary": {
      const tools = Array.isArray(node.config?.tools) ? node.config.tools : extra.workspace.tools;
      if (!git) return ok({ root, git: null, git_reason: "not a git repository", tools });
      const changes = changesOf(folder);
      return ok({
        root, tools,
        git: { branch: extra.workspace.branch, status: changes.map((change) => `${change.code} ${change.path}\n`).join(""), head: extra.workspace.head, changes: changes.slice(0, maxEntries), truncated: changes.length > maxEntries },
      });
    }
    case "list": {
      const at = cleanPath(params.path);
      if (at.error) return at;
      if (!isFolder(at.path)) return failure(-32602, folder.working.has(at.path) ? `\`${at.path}\` is a file` : `no folder \`${at.path}\` in the workspace`);
      const prefix = at.path ? `${at.path}/` : "";
      const children = new Map();
      for (const file of files) {
        if (!file.startsWith(prefix)) continue;
        const [name, ...deeper] = file.slice(prefix.length).split("/");
        const path = `${prefix}${name}`;
        children.set(path, deeper.length ? { path, kind: "directory" } : { path, kind: "file", size: bytes(folder.working.get(file)) });
      }
      // By path, byte for byte, as the module sorts: capitals first.
      const entries = [...children.values()].sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
      return ok({ root, path: at.path, entries: entries.slice(0, maxEntries), truncated: entries.length > maxEntries });
    }
    case "read": {
      if (typeof params.path !== "string" || !params.path) return failure(-32602, "`read` needs a `path`");
      const at = cleanPath(params.path);
      if (at.error) return at;
      if (isFolder(at.path)) return failure(-32602, `\`${at.path}\` is a folder`);
      const text = folder.working.get(at.path);
      if (text === undefined) return failure(-32602, `no file \`${at.path}\` in the workspace`);
      return ok({ root, path: at.path, text: text.slice(0, maxBytes), truncated: text.length > maxBytes, size: bytes(text) });
    }
    case "search": {
      if (typeof params.query !== "string" || !params.query) return failure(-32602, "`search` needs a `query`");
      const at = cleanPath(params.path);
      if (at.error) return at;
      const query = params.query.toLowerCase();
      const matches = files.filter((file) => !at.path || file === at.path || file.startsWith(`${at.path}/`)).flatMap((file) =>
        lines(folder.working.get(file)).flatMap((text, index) => text.toLowerCase().includes(query) ? [{ path: file, line: index + 1, text }] : []));
      return ok({ root, matches: matches.slice(0, maxEntries), truncated: matches.length > maxEntries });
    }
    case "diff": {
      if (!git) return ok({ root, text: "", truncated: false, git_reason: "not a git repository" });
      const text = diffOf(folder);
      return ok({ root, text: text.slice(0, maxBytes), truncated: text.length > maxBytes });
    }
    default: return failure(-32602, `\`${params.action}\` is not an action: summary, list, read, search or diff`);
  }
}

// ── The coding agent's turn ────────────────────────────────────────────────────────────

/** As the native engine asks (docs/12-agents.md §8): once, always, or not. */
const EDIT_OPTIONS = [
  { id: "allow_once", kind: "allow_once", name: "Allow once" },
  { id: "allow_always", kind: "allow_always", name: "Always allow" },
  { id: "reject_once", kind: "reject_once", name: "Reject" },
];
const said = (text) => [{ type: "content", content: { type: "text", text } }];
const delegateOf = (workflow) => (workflow?.nodes ?? []).find((node) => moduleOf(node) === DELEGATION);

/**
 * One step of a turn of an agent whose workflow has a code workspace: it lists the folder,
 * reads the README, asks to edit NOTES.md (a decider wired: a person answers; none: it is
 * refused, as the box does), writes the line if allowed, starts a worker when the message
 * asks for one, and answers. `true`: this script took the step.
 */
function codeTurn({ state, execution, node, conversation, current, activity, write, end }) {
  const workflow = workflowOf(state, execution);
  const workspace = workspaceOf(workflow);
  const root = workspace?.config?.root;
  if (!workspace || typeof root !== "string" || !hasModule(state, CODE)) return false;
  const folder = folderOf(state, root);
  const { turn, text } = current;
  const call = (id, title, kind) => activity({ kind: "tool_call", id: `${turn}-${id}`, title, tool_kind: kind, status: "in_progress" });
  const done = (id, content, status = "completed") => activity({ kind: "tool_update", id: `${turn}-${id}`, status, content: said(content) });
  switch (current.step) {
    case 1: activity({ kind: "thought", text: "(mock) Reading the request, then the folder." }); break;
    case 2: call("list", "code_list_files", "read"); break;
    case 3: done("list", [...new Set([...folder.working.keys()].map((path) => path.split("/")[0]))].sort().join("\n")); break;
    case 4: call("read", "code_read_file README.md", "read"); break;
    case 5: done("read", folder.working.get("README.md") ?? "no file `README.md` in the workspace", folder.working.has("README.md") ? "completed" : "failed"); break;
    case 6: write(`(mock) You said: ${text}. `); break;
    case 7:
      // "Always allow" from an earlier turn of this conversation needs no question.
      if (conversation.allowEdits) current.decided = "allow_always";
      else if (!askPermission(state, execution, node, conversation, { request: `${turn}-edit`, tool: "code_edit_file", title: `code_edit_file ${NOTES}`, options: EDIT_OPTIONS })) current.decided = null;
      break;
    case 8: call("edit", `code_edit_file ${NOTES}`, "edit"); break;
    case 9: {
      const allowed = current.decided === "allow_once" || current.decided === "allow_always";
      if (current.decided === "allow_always") conversation.allowEdits = true;
      if (allowed) folder.working.set(NOTES, `${folder.working.get(NOTES) ?? "# Notes\n\n"}- ${text}\n`);
      done("edit", allowed ? `edited ${NOTES}: 1 line added` : current.decided === null ? "refused: nobody is wired to answer the permission request" : "refused: the person rejected it", allowed ? "completed" : "failed");
      current.edited = allowed;
      break;
    }
    case 10: {
      const delegate = delegateOf(workflow);
      if (!delegate || !/\b(worker|delegate)/i.test(text)) { current.step += 2; return codeTurn({ state, execution, node, conversation, current, activity, write, end }); }
      call("worker", "worker_create", "other");
      break;
    }
    case 11: {
      const made = createWorker(state, execution, delegateOf(workflow), `Look into: ${text}`);
      done("worker", made.error ? made.error : JSON.stringify(made.worker), made.error ? "failed" : "completed");
      break;
    }
    case 12: write(current.edited ? `I added it to ${NOTES}.` : `I left ${NOTES} as it was.`); break;
    default:
      current.answer = current.written;
      end("end_turn");
  }
  return true;
}

// ── Workers (module `delegation`) ──────────────────────────────────────────────────────

const groupKey = (execution, node) => `${execution.id}/${node.id}`;
function groupOf(state, execution, node) {
  const key = groupKey(execution, node);
  if (!state.workers.has(key)) state.workers.set(key, { next: 0, list: [] });
  return state.workers.get(key);
}

/** A worker as `delegation.workers` shows it: its results are what its agent answered and nobody took. */
function viewOf(state, worker) {
  const child = state.executions.find((item) => item.id === worker.execution);
  const live = !!child && LIVE.has(child.state.status);
  const agent = child && workflowOf(state, child)?.nodes.find((node) => (node.ports ?? []).some((port) => port.direction === "input" && port.capability.split("@")[0] === "agent.engine"));
  const answers = child && agent ? conversationOf(state, child, agent).entries.filter((entry) => entry.role === "agent" && entry.execution_id === child.id) : [];
  return {
    worker: worker.id, kind: worker.kind, name: worker.name, execution: worker.execution,
    state: worker.closed ? "closed" : live ? "running" : child?.state.status ?? "stopped",
    pending_results: answers.length, last_output_at: answers.length ? answers[answers.length - 1].at : null,
  };
}

/** `worker_create`: a child execution of the workflow the delegate's `workers` map names, kept alive with its brief queued. */
function createWorker(state, execution, delegate, brief) {
  const kinds = Object.entries(delegate.config?.workers ?? {});
  if (!kinds.length) return { error: "the delegate has no kind of worker (`workers` is empty)" };
  const [kind, workflowId] = kinds[0];
  const profile = state.profiles.find((item) => item.id === execution.profile_id);
  const workflow = profile?.workflows.find((item) => item.id === workflowId);
  if (!workflow) return { error: `kind \`${kind}\` names the workflow \`${workflowId}\`, which the profile does not have` };
  const group = groupOf(state, execution, delegate);
  const max = numberOf(delegate, "max_workers", 4);
  const live = group.list.filter((worker) => viewOf(state, worker).state === "running").length;
  if (max > 0 && live >= max) return { error: `${live} workers are live already (\`max_workers\` ${max}): close one first` };
  const id = `w-${++group.next}`;
  const child = { id: `exec-${++state.runs}`, profile_id: execution.profile_id, workflow_id: workflow.id, project_id: execution.project_id, state: { status: "running" }, parent: execution.id, caller: delegate.id, tag: id };
  state.executions.push(child);
  const worker = { id, kind, name: id, execution: child.id, closed: false };
  group.list.push(worker);
  const agent = workflow.nodes.find((node) => (node.ports ?? []).some((port) => port.direction === "input" && port.capability.split("@")[0] === "agent.engine"));
  if (agent) queueTurn(conversationOf(state, child, agent), brief);
  return { worker: { worker: id, execution: child.id, kind, name: id } };
}

/** A running delegate node of a live execution, as `delegation.*` take it. */
function delegateNode(state, params) {
  const found = liveNode(state, params);
  if (found.error || moduleOf(found.node) !== DELEGATION) return failure(-32602, `delegate \`${params.node_id}\` is not running`);
  return found;
}

/** Answers `code.inspect` and the workers methods; `null` for any other method or an absent module. */
export function codeMethod(state, method, params) {
  switch (method) {
    case "code.inspect": {
      if (!hasModule(state, CODE)) return null;
      const found = liveNode(state, params);
      if (found.error || moduleOf(found.node) !== CODE || !offers(found.node, "code.workspace")) {
        return failure(-32602, `no live workspace node \`${params.node_id}\` in execution \`${params.execution_id}\``);
      }
      const root = found.node.config?.root;
      if (typeof root !== "string" || !root) return failure(-32003, `workspace \`${found.node.id}\` has no root`);
      return inspect(found.node, folderOf(state, root), root, params);
    }
    case "delegation.workers": {
      if (!hasModule(state, DELEGATION)) return null;
      const found = delegateNode(state, params);
      if (found.error) return found;
      const { list } = groupOf(state, found.execution, found.node);
      // Workers work while nobody watches their own conversation: each listing moves them a step.
      for (const worker of list) {
        const child = state.executions.find((item) => item.id === worker.execution);
        if (child) advanceExecution(state, child);
      }
      return ok(list.map((worker) => viewOf(state, worker)));
    }
    case "delegation.close": {
      if (!hasModule(state, DELEGATION)) return null;
      const found = delegateNode(state, params);
      if (found.error) return found;
      const worker = groupOf(state, found.execution, found.node).list.find((item) => item.id === params.worker || item.name === params.worker);
      if (!worker) return failure(-32003, `no worker \`${params.worker}\``);
      const child = state.executions.find((item) => item.id === worker.execution);
      if (child && LIVE.has(child.state.status)) { child.state = { status: "stopped" }; worker.closed = true; }
      return ok(viewOf(state, worker));
    }
    default: return null;
  }
}
