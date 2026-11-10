// Pure logic of the Regulus Code view: what makes a profile or an execution a coding
// session, how a folder becomes a profile id, what the setup form asks, and how the answers
// of `code.inspect` and `delegation.workers` are read. A node is known by what it offers or
// takes and a module by the methods its manifest declares, never by an id. No React, no
// socket: tested in logic.test.ts.
import type { CompositionNode, Manifest, Profile, Workflow } from "@/composition/model";
import { methods } from "@/operate/client";
import { agentNodes, reduceConversation, type ConversationRecord, type TurnItem, type TurnView } from "@/operate/conversation";
import { isLive, runnableWorkflows, type NodeRow } from "@/operate/logic";
import type { Execution } from "@/operate/model";
import type { TemplateSummary, TemplateVar } from "@/templates/model";
import type { SearchMatch, Worker, WorkspaceDiff, WorkspaceEntry, WorkspaceFile, WorkspaceListing, WorkspaceSearch } from "./model";

/** Capabilities the view goes by, by name without version. */
export const codeCapabilities = { workspace: "code.workspace", engine: "agent.engine", endpoint: "llm.endpoint", decision: "decision" } as const;

const base = (id: string) => id.split("@")[0];
const isObject = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
const str = (value: unknown): string | null => typeof value === "string" ? value : null;

// ── What is a coding session ───────────────────────────────────────────────────────────

export function offers(node: Pick<CompositionNode, "ports">, capability: string): boolean {
  return node.ports.some((port) => port.direction === "output" && base(port.capability) === capability);
}

/** The nodes of a workflow that offer `code.workspace`: where `code.inspect` looks. */
export function workspaceNodes(workflow: Workflow | undefined): CompositionNode[] {
  return (workflow?.nodes ?? []).filter((node) => offers(node, codeCapabilities.workspace));
}

const manifestOf = (manifests: Manifest[], moduleId: string | null) =>
  moduleId === null ? undefined : manifests.find((manifest) => base(manifest.module.id) === base(moduleId));

/**
 * The agents of a workflow one can talk to. A node with an `agent.engine` input is a
 * candidate, but a delegate has that input too (the engines of its workers, read lazily):
 * among the candidates, an agent is one whose module declares `agent.history`. A node
 * whose module has no manifest here (not installed) counts unless that input is lazy.
 */
export function codeAgents(workflow: Workflow | undefined, manifests: Manifest[]): CompositionNode[] {
  return agentNodes(workflow).filter((node) => {
    const manifest = manifestOf(manifests, node.module_id);
    if (manifest) return (manifest.module.methods ?? []).includes(methods.agentHistory);
    return node.ports.some((port) => port.direction === "input" && base(port.capability) === codeCapabilities.engine && !port.lazy);
  });
}

/** A workflow one can code with: a workspace to look into and an agent to talk to. */
export function isCodeWorkflow(workflow: Workflow | undefined, manifests: Manifest[]): boolean {
  return workspaceNodes(workflow).length > 0 && codeAgents(workflow, manifests).length > 0;
}

/** The folder a workspace node is set to, as its profile writes it; `null` if it says none. */
export function rootOf(node: Pick<CompositionNode, "config"> | undefined): string | null {
  const root = node?.config.root;
  return typeof root === "string" && root.trim() ? root.trim() : null;
}

/** A workflow of a saved profile that can run as a coding session. */
export interface CodeTarget {
  profileId: string;
  profileName: string;
  projectId: string | null;
  workflowId: string;
  workflowName: string;
  /** Direct agents of the workflow, in its order: the first is talked to unless another is picked. */
  agentIds: string[];
  /** The workspace node `code.inspect` reads. */
  workspaceId: string;
  root: string | null;
}

/** Every runnable workflow of these profiles that is a coding workflow, the main one of each profile first. */
export function codeTargets(profiles: Profile[], manifests: Manifest[]): CodeTarget[] {
  return profiles.flatMap((profile) => runnableWorkflows(profile).filter((workflow) => isCodeWorkflow(workflow, manifests)).map((workflow) => {
    const workspace = workspaceNodes(workflow)[0];
    return {
      profileId: profile.id, profileName: profile.name || profile.id, projectId: profile.project_id,
      workflowId: workflow.id, workflowName: workflow.name || workflow.id,
      agentIds: codeAgents(workflow, manifests).map((node) => node.id), workspaceId: workspace.id, root: rootOf(workspace),
    };
  }));
}

export const targetOf = (targets: CodeTarget[], profileId: string, workflowId: string) =>
  targets.find((target) => target.profileId === profileId && target.workflowId === workflowId);

export interface CodeSession {
  execution: Execution;
  target: CodeTarget;
  live: boolean;
}

/**
 * The executions that are coding sessions: those of a coding workflow that nobody started
 * as a child (a worker is seen under its session). Live ones first, then the newest.
 */
export function codeSessions(executions: Execution[], targets: CodeTarget[]): CodeSession[] {
  const sessions = executions.flatMap((execution, order) => {
    const target = execution.parent ? undefined : targetOf(targets, execution.profile_id, execution.workflow_id);
    return target ? [{ execution, target, live: isLive(execution.state), order }] : [];
  });
  return sessions.sort((a, b) => Number(b.live) - Number(a.live) || b.order - a.order).map(({ order: _order, ...session }) => session);
}

/** One folder set up for coding: its saved harness, the session running in it, and the ones before. */
export interface FolderRow {
  target: CodeTarget;
  live: CodeSession | null;
  earlier: CodeSession[];
}

/** The folders set up, each with its sessions: those with one running first, then by name. */
export function folderRows(targets: CodeTarget[], sessions: CodeSession[]): FolderRow[] {
  const rows = targets.map((target) => {
    const own = sessions.filter((session) => session.target.profileId === target.profileId && session.target.workflowId === target.workflowId);
    return { target, live: own.find((session) => session.live) ?? null, earlier: own.filter((session) => !session.live) };
  });
  const name = (row: FolderRow) => row.target.root ? folderName(row.target.root) : row.target.profileName;
  return rows.sort((a, b) => Number(!!b.live) - Number(!!a.live) || name(a).localeCompare(name(b)));
}

/** How a session stands, for the mascot and the words next to it: the first that holds. */
export type SessionMood = "failed" | "stopped" | "waiting" | "working" | "idle";

export function sessionMood(execution: Pick<Execution, "state">, turn: { waiting: boolean; working: boolean }): SessionMood {
  if (execution.state.status === "failed" || execution.state.status === "interrupted") return "failed";
  if (!isLive(execution.state)) return "stopped";
  if (turn.waiting) return "waiting";
  return turn.working ? "working" : "idle";
}

/** Nodes of a session that will not work, with the reason their state gives. */
export function sessionProblems(rows: NodeRow[]): NodeRow[] {
  return rows.filter((row) => row.state.kind === "not-installed" || row.state.kind === "disabled" || row.state.kind === "broken" || row.state.kind === "incompatible");
}

// ── Folders and profile ids ────────────────────────────────────────────────────────────

/** A folder as typed, without the trailing separators (the root `/` stays). */
export function normalizeRoot(root: string): string {
  const trimmed = root.trim();
  return trimmed.replace(/[\\/]+$/, "") || trimmed.slice(0, 1);
}

/** Same folder as text. The core resolves links; here two spellings of one folder differ. */
export const sameRoot = (a: string, b: string) => normalizeRoot(a) === normalizeRoot(b);

/** The core takes an absolute folder: POSIX, a drive letter or a UNC share. */
export function isAbsolutePath(path: string): boolean {
  const trimmed = path.trim();
  return trimmed.startsWith("/") || /^[A-Za-z]:[\\/]/.test(trimmed) || trimmed.startsWith("\\\\");
}

/** The last segment of a folder: what a person calls it. */
export function folderName(root: string): string {
  const parts = normalizeRoot(root).split(/[\\/]+/).filter(Boolean);
  return parts[parts.length - 1] ?? "";
}

/**
 * Text as part of a profile id (a file name in the core): ASCII letters lowered, digits,
 * dot, dash and underscore stay; any run of other characters is one dash. The same slug
 * `regulus code` makes (`tui/document.rs` `slugify`), character by character, so the two
 * clients name a folder's profile alike.
 */
export function slugOf(text: string): string {
  let out = "";
  let gap = false;
  for (const char of text.trim().toLowerCase()) {
    if (/^[a-z0-9._-]$/.test(char)) {
      if (gap && out) out += "-";
      gap = false;
      out += char;
    } else gap = true;
  }
  return out.replace(/^[-.]+/, "").replace(/-+$/, "");
}

/** FNV-1a, 64 bits, of the text's UTF-8 bytes, as sixteen hex digits. */
export function fnv1a64(text: string): string {
  let hash = 0xcbf29ce484222325n;
  for (const byte of new TextEncoder().encode(text)) hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * 0x100000001b3n);
  return hash.toString(16).padStart(16, "0");
}

/** Hex digits of the folder's hash a profile id carries. */
const HASH_DIGITS = 6;

/**
 * The profile id proposed for a folder: `<prefix>-<folder name>-<hash>`, the hash being
 * the first digits of the FNV-1a 64 of the folder's path, so two folders with one name do
 * not share a profile. `regulus code` writes the same id for the same path. If another
 * profile already has it, the next free number follows.
 */
export function suggestProfileId(root: string, taken: Iterable<string>, prefix: string): string {
  const used = new Set(taken);
  const folder = normalizeRoot(root);
  const stem = [slugOf(prefix), slugOf(folderName(folder)) || "folder", fnv1a64(folder).slice(0, HASH_DIGITS)].filter(Boolean).join("-");
  if (!used.has(stem)) return stem;
  for (let n = 2; ; n++) if (!used.has(`${stem}-${n}`)) return `${stem}-${n}`;
}

/** Why an id cannot be a new profile's: the core writes it as a file name and never replaces a profile by itself. */
export function profileIdProblem(id: string, taken: string[]): "empty" | "invalid" | "taken" | null {
  if (!id) return "empty";
  if (slugOf(id) !== id) return "invalid";
  return taken.includes(id) ? "taken" : null;
}

/** The saved coding profile of a folder, if there is one: found by its folder, whatever its id. */
export const targetForRoot = (targets: CodeTarget[], root: string) =>
  targets.find((target) => target.root !== null && sameRoot(target.root, root));

// ── The setup form ─────────────────────────────────────────────────────────────────────

/** Modules whose manifest has a node type offering `code.workspace`. */
export function codeModuleIds(manifests: Manifest[]): string[] {
  return manifests.filter((manifest) => (manifest.node ?? []).some((node) =>
    (node.port ?? []).some((port) => port.direction === "output" && base(port.capability) === codeCapabilities.workspace),
  )).map((manifest) => manifest.module.id);
}

/**
 * The templates a coding session can start from: the one named in the settings, and any
 * that requires a module offering `code.workspace`. The named one first; a template the
 * core could not read stays, so its error shows.
 */
export function codeTemplates(templates: TemplateSummary[], manifests: Manifest[], preferred: string): TemplateSummary[] {
  const modules = new Set(codeModuleIds(manifests).map(base));
  const fits = (template: TemplateSummary) => template.name === preferred || template.requires.some((require) => modules.has(base(require.module)));
  return templates.filter(fits).sort((a, b) => Number(b.name === preferred) - Number(a.name === preferred));
}

/** Template vars the form gives a place of its own; any other var shows under "more settings". */
export const setupVars = { root: "root", provider: ["preset", "base_url", "credential", "model"] } as const;

export interface SetupVars {
  root: TemplateVar | null;
  provider: TemplateVar[];
  more: TemplateVar[];
}

export function splitVars(vars: TemplateVar[]): SetupVars {
  const provider = setupVars.provider.flatMap((name) => vars.filter((variable) => variable.name === name));
  const known = new Set<string>([setupVars.root, ...setupVars.provider]);
  return { root: vars.find((variable) => variable.name === setupVars.root) ?? null, provider, more: vars.filter((variable) => !known.has(variable.name)) };
}

/** How a node config names a secret (docs/11-composition.md): the value never leaves the core. */
export const secretPrefix = "secret:";

export type CredentialState =
  | { kind: "none" }
  | { kind: "stored"; name: string }
  | { kind: "missing"; name: string }
  /** The core has not listed its secrets yet, or could not. */
  | { kind: "unknown"; name: string }
  | { kind: "malformed" };

/** What a `credential` value points at: nothing, a secret the core has, one it has not, or not a reference at all. */
export function credentialState(value: string, secrets: string[] | null): CredentialState {
  const text = value.trim();
  if (!text) return { kind: "none" };
  if (!text.startsWith(secretPrefix) || text.length === secretPrefix.length) return { kind: "malformed" };
  const name = text.slice(secretPrefix.length);
  if (secrets === null) return { kind: "unknown", name };
  return secrets.includes(name) ? { kind: "stored", name } : { kind: "missing", name };
}

// ── The parts of a harness, for "edit in canvas" ───────────────────────────────────────

export type HarnessRole = "agent" | "engine" | "model" | "workspace" | "decider" | "workers";

export interface HarnessPart {
  role: HarnessRole;
  nodeId: string;
  name: string;
}

/** The nodes wired into the `uses` inputs of `node` that take `capability`. */
function sourcesOf(workflow: Workflow, node: CompositionNode, capability: string): CompositionNode[] {
  const inputs = new Set(node.ports.filter((port) => port.direction === "input" && base(port.capability) === capability).map((port) => port.id));
  return workflow.wires
    .filter((wire) => wire.kind === "uses" && wire.to.node_id === node.id && inputs.has(wire.to.port_id))
    .flatMap((wire) => workflow.nodes.filter((item) => item.id === wire.from.node_id));
}

/**
 * The boxes a coding session is made of, found by its wires: the agent, the engine that
 * runs it, the model behind the engine, the workspace, who answers its permission requests
 * and who keeps its workers. Each can be opened and changed in the canvas.
 */
export function harnessParts(workflow: Workflow | undefined, agentId: string | null, workerNodeIds: string[]): HarnessPart[] {
  const agent = workflow?.nodes.find((node) => node.id === agentId);
  if (!workflow || !agent) return [];
  const engines = sourcesOf(workflow, agent, codeCapabilities.engine);
  const models = [...engines.flatMap((engine) => sourcesOf(workflow, engine, codeCapabilities.endpoint)), ...sourcesOf(workflow, agent, codeCapabilities.endpoint)];
  const parts: [HarnessRole, CompositionNode[]][] = [
    ["agent", [agent]], ["engine", engines], ["model", models], ["workspace", workspaceNodes(workflow)],
    ["decider", sourcesOf(workflow, agent, codeCapabilities.decision)],
    ["workers", workflow.nodes.filter((node) => workerNodeIds.includes(node.id))],
  ];
  const seen = new Set<string>();
  return parts.flatMap(([role, nodes]) => nodes.flatMap((node) => {
    if (seen.has(node.id)) return [];
    seen.add(node.id);
    return [{ role, nodeId: node.id, name: node.name || node.id }];
  }));
}

/** The nodes of a workflow whose module declares `method` in its manifest. */
export function nodesAnswering(workflow: Workflow | undefined, manifests: Manifest[], method: string): CompositionNode[] {
  return (workflow?.nodes ?? []).filter((node) => {
    const manifest = manifestOf(manifests, node.module_id);
    return !!manifest && (manifest.module.methods ?? []).includes(method) && (manifest.node ?? []).some((type) => type.type === node.type);
  });
}

// ── `code.inspect` answers ─────────────────────────────────────────────────────────────

/** One changed path of the working tree, from porcelain status. */
export interface GitChange {
  path: string;
  /** Where a renamed or copied file came from. */
  from: string | null;
  /** The two status letters: staged, then working tree. */
  index: string;
  worktree: string;
}

export interface GitState {
  available: boolean;
  /** `null` on a detached HEAD, and when git is unavailable. */
  branch: string | null;
  changes: GitChange[];
  /** Why git gave nothing, when the module says. */
  reason: string | null;
  /** Commits ahead of and behind the upstream branch, when the module counts them. */
  ahead: number;
  behind: number;
  /** The status had more lines than the module gives. */
  truncated: boolean;
}

const count = (value: unknown) => typeof value === "number" && value > 0 ? Math.floor(value) : 0;

export type ChangeKind = "untracked" | "ignored" | "conflicted" | "renamed" | "added" | "deleted" | "modified";

export function changeKind({ index, worktree }: GitChange): ChangeKind {
  const code = `${index}${worktree}`;
  if (code === "??") return "untracked";
  if (code === "!!") return "ignored";
  if (code.includes("U") || code === "AA" || code === "DD") return "conflicted";
  if (code.includes("R") || code.includes("C")) return "renamed";
  if (code.includes("A")) return "added";
  if (code.includes("D")) return "deleted";
  return "modified";
}

const unquote = (path: string) => path.length > 1 && path.startsWith('"') && path.endsWith('"') ? path.slice(1, -1) : path;

function statusLine(line: string): GitChange | null {
  if (line.length < 4) return null;
  const rest = line.slice(3);
  const arrow = rest.indexOf(" -> ");
  const renamed = arrow >= 0 && /[RC]/.test(line.slice(0, 2));
  return {
    index: line[0], worktree: line[1],
    path: unquote(renamed ? rest.slice(arrow + 4) : rest), from: renamed ? unquote(rest.slice(0, arrow)) : null,
  };
}

/**
 * Porcelain status as changes. Reads the text (`XY path` per line, or NUL separated), a
 * list of such lines, or a list of `{ path, code, from? }` / `{ path, index, worktree }` objects.
 */
export function parseStatus(status: unknown): GitChange[] {
  if (typeof status === "string") return status.split(/\r?\n|\0/).flatMap((line) => statusLine(line) ?? []);
  if (!Array.isArray(status)) return [];
  return status.flatMap((item): GitChange[] => {
    if (typeof item === "string") {
      const change = statusLine(item);
      return change ? [change] : [];
    }
    const path = isObject(item) ? str(item.path) : null;
    if (!isObject(item) || !path) return [];
    const code = (str(item.code) ?? str(item.status) ?? "").padEnd(2, " ");
    return [{ path, from: str(item.from), index: str(item.index) ?? code[0], worktree: str(item.worktree) ?? code[1] }];
  });
}

export interface SummaryView {
  root: string;
  git: GitState;
  tools: string[];
}

/**
 * A `summary` answer; `null` when it is not one. A folder without git reads as git
 * unavailable, never as a failure: `git` is `null` and `git_reason` says why. The status
 * text is what counts; `changes` is read only when there is no text.
 */
export function readSummary(answer: unknown): SummaryView | null {
  if (!isObject(answer) || typeof answer.root !== "string") return null;
  const raw = isObject(answer.git) ? answer.git : null;
  const status = raw && (typeof raw.status === "string" || Array.isArray(raw.status)) ? raw.status : raw && Array.isArray(raw.changes) ? raw.changes : null;
  const available = raw !== null && raw.available !== false && status !== null;
  const reason = str(answer.git_reason) ?? (raw ? str(raw.reason) ?? str(raw.error) : null);
  return {
    root: answer.root,
    git: {
      available, branch: available ? str(raw.branch) : null, changes: available ? parseStatus(status) : [], reason: available ? null : reason,
      ahead: available ? count(raw.ahead) : 0, behind: available ? count(raw.behind) : 0, truncated: available && raw.truncated === true,
    },
    tools: Array.isArray(answer.tools) ? answer.tools.filter((tool): tool is string => typeof tool === "string") : [],
  };
}

const size = (value: unknown) => typeof value === "number" && value >= 0 ? { size: value } : {};

const readEntry = (item: unknown): WorkspaceEntry | null => {
  const path = isObject(item) ? str(item.path) : null;
  if (!isObject(item) || !path) return null;
  const kind = str(item.kind);
  const folder = kind === "directory" || kind === "dir" || kind === "folder";
  return { path: path.replace(/\/+$/, ""), kind: folder ? "directory" : "file", ...(folder ? {} : size(item.size)) };
};

export function readListing(answer: unknown): WorkspaceListing | null {
  if (!isObject(answer) || !Array.isArray(answer.entries)) return null;
  return {
    root: str(answer.root) ?? "", path: (str(answer.path) ?? "").replace(/^\.?\/*|\/+$/g, ""),
    entries: answer.entries.flatMap((item) => readEntry(item) ?? []), truncated: answer.truncated === true,
  };
}

export function readFile(answer: unknown): WorkspaceFile | null {
  if (!isObject(answer) || typeof answer.text !== "string") return null;
  return { root: str(answer.root) ?? "", path: str(answer.path) ?? "", text: answer.text, truncated: answer.truncated === true, ...size(answer.size) };
}

export function readSearch(answer: unknown): WorkspaceSearch | null {
  if (!isObject(answer) || !Array.isArray(answer.matches)) return null;
  const matches = answer.matches.flatMap((item): SearchMatch[] => {
    const path = isObject(item) ? str(item.path) : null;
    if (!isObject(item) || !path) return [];
    return [{ path, line: typeof item.line === "number" && item.line > 0 ? Math.floor(item.line) : 1, text: str(item.text) ?? "" }];
  });
  return { root: str(answer.root) ?? "", matches, truncated: answer.truncated === true };
}

/** A `diff` answer. Outside a repository the text is empty and `git_reason` says why. */
export function readDiff(answer: unknown): WorkspaceDiff | null {
  if (!isObject(answer) || typeof answer.text !== "string") return null;
  const reason = str(answer.git_reason);
  return { root: str(answer.root) ?? "", text: answer.text, truncated: answer.truncated === true, ...(reason ? { git_reason: reason } : {}) };
}

// ── The file tree ──────────────────────────────────────────────────────────────────────

export const joinPath = (folder: string, name: string) => folder ? `${folder}/${name}` : name;
export const parentPath = (path: string) => path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "";
export const baseName = (path: string) => path.slice(path.lastIndexOf("/") + 1);

/** The folders above a path, outermost first: what must be open for it to show. */
export function ancestorsOf(path: string): string[] {
  const parts = path.split("/").filter(Boolean).slice(0, -1);
  return parts.map((_, index) => parts.slice(0, index + 1).join("/"));
}

/**
 * What sits directly in `folder`: the entries given for it, folders first, then by name.
 * An entry deeper than one level (a listing that recurses) counts as its top folder.
 */
export function directChildren(folder: string, entries: WorkspaceEntry[]): WorkspaceEntry[] {
  const prefix = folder ? `${folder}/` : "";
  const children = new Map<string, WorkspaceEntry>();
  for (const entry of entries) {
    if (!entry.path.startsWith(prefix) || entry.path === folder) continue;
    const [first, ...deeper] = entry.path.slice(prefix.length).split("/");
    if (!first) continue;
    const path = joinPath(folder, first);
    const kind = deeper.length > 0 ? "directory" : entry.kind;
    if (children.get(path)?.kind !== "directory") children.set(path, kind === "directory" ? { path, kind } : { ...entry, path });
  }
  return [...children.values()].sort((a, b) => Number(b.kind === "directory") - Number(a.kind === "directory") || baseName(a.path).localeCompare(baseName(b.path)));
}

export interface TreeRow {
  path: string;
  name: string;
  kind: "file" | "directory";
  depth: number;
  open: boolean;
  /** A folder whose listing has arrived. */
  loaded: boolean;
}

/** The rows of the tree on screen: each listed folder's children, an open folder followed by its own. */
export function treeRows(listings: Record<string, WorkspaceEntry[]>, open: ReadonlySet<string>, folder = "", depth = 0): TreeRow[] {
  return (listings[folder] ?? []).flatMap((entry): TreeRow[] => {
    const isOpen = entry.kind === "directory" && open.has(entry.path);
    const row: TreeRow = { path: entry.path, name: baseName(entry.path), kind: entry.kind, depth, open: isOpen, loaded: entry.path in listings };
    return isOpen ? [row, ...treeRows(listings, open, entry.path, depth + 1)] : [row];
  });
}

/** A size in bytes as a number and its unit: bytes under a kilobyte, then one decimal. */
export function sizeParts(bytes: number): { value: string; unit: "bytes" | "kb" | "mb" } {
  if (bytes < 1000) return { value: String(Math.round(bytes)), unit: "bytes" };
  if (bytes < 1_000_000) return { value: (bytes / 1000).toFixed(1), unit: "kb" };
  return { value: (bytes / 1_000_000).toFixed(1), unit: "mb" };
}

/** A file's text as lines, without the empty one a final newline would add. */
export function fileLines(text: string): string[] {
  const lines = text.split(/\r?\n/);
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

// ── Workers ────────────────────────────────────────────────────────────────────────────

/** A `delegation.workers` answer: rows without a worker id drop. */
export function readWorkers(answer: unknown): Worker[] {
  if (!Array.isArray(answer)) return [];
  return answer.flatMap((item): Worker[] => {
    const id = isObject(item) ? str(item.worker) : null;
    if (!isObject(item) || !id) return [];
    return [{
      worker: id, kind: str(item.kind) ?? "", name: str(item.name) ?? "", execution: str(item.execution) ?? "", state: str(item.state) ?? "",
      pending_results: typeof item.pending_results === "number" ? item.pending_results : 0,
      last_output_at: typeof item.last_output_at === "number" ? item.last_output_at : null,
      ...(str(item.engine) ? { engine: item.engine as string } : {}),
    }];
  });
}

export const isWorkerLive = (worker: Pick<Worker, "state">) => worker.state === "starting" || worker.state === "running";

// ── Activity ───────────────────────────────────────────────────────────────────────────

type Permission = Extract<TurnItem, { type: "permission" }>;

/** The permission requests of these turns nobody has answered yet, oldest first. */
export function openPermissions(turns: TurnView[]): Permission[] {
  return turns.flatMap((turn) => turn.items.filter((item): item is Permission => item.type === "permission" && !item.resolved));
}

export interface ActivityItem {
  key: string;
  seq: number;
  /** Milliseconds since the Unix epoch; 0 when the record has none. */
  at: number;
  kind: "tool" | "permission" | "ended" | "system";
  title: string;
  /** A tool's kind, the option a permission got, a turn's stop reason. */
  note: string | null;
  /** A tool's status as the engine says it; `waiting`, `answered` or `cancelled` for a permission. */
  status: string | null;
}

/** What the agent did, without what was said: tool calls, permission requests, turn ends and failures. Newest first. */
export function activityOf(records: ConversationRecord[]): ActivityItem[] {
  const at = new Map(records.map((record) => [record.seq, record.at]));
  const items = reduceConversation(records, "turn").flatMap((turn) => turn.items.flatMap((item): ActivityItem[] => {
    const head = { key: item.key, seq: item.seq, at: at.get(item.seq) ?? 0 };
    switch (item.type) {
      case "tool": return [{ ...head, kind: "tool", title: item.title, note: item.toolKind, status: item.status }];
      case "permission": {
        const chosen = item.resolved?.option ?? null;
        const status = !item.resolved ? "waiting" : chosen === null ? "cancelled" : "answered";
        return [{ ...head, kind: "permission", title: item.title, note: chosen === null ? null : item.options.find((option) => option.id === chosen)?.name ?? chosen, status }];
      }
      case "ended": return [{ ...head, kind: "ended", title: "", note: item.stopReason || null, status: null }];
      case "entry": return item.role === "system" ? [{ ...head, kind: "system", title: item.text, note: null, status: null }] : [];
      default: return [];
    }
  }));
  return items.sort((a, b) => b.seq - a.seq);
}

/**
 * A number that changes when the agent may have changed the workspace: the `seq` of its
 * last tool update or turn end. The view asks `code.inspect` again when it moves, instead
 * of asking often.
 */
export function workStamp(records: ConversationRecord[]): number {
  let stamp = 0;
  for (const record of records) {
    if (record.type === "activity" && (record.event.kind === "tool_update" || record.event.kind === "turn_ended") && record.seq > stamp) stamp = record.seq;
  }
  return stamp;
}
