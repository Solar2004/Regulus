// Shapes the Regulus Code view reads besides those of Operate (`operate/model.ts`): the
// read-only `code.inspect` of the module that offers `code.workspace@1`, and the workers a
// delegate node lists. As the modules serve them; the view reads them through `logic.ts`,
// which tolerates a field that is missing or shaped another way.

export type InspectAction = "summary" | "list" | "read" | "search" | "diff";

/** `summary.git`: `null` when the folder is not a repository or git is not there. */
export interface GitSummary {
  /** `null` on a detached HEAD or a repository with no commit yet. */
  branch: string | null;
  /** `git status --porcelain` text: `XY path` per line, empty when clean. */
  status: string;
  /** Why git gave nothing, when the module says. */
  reason?: string;
}

export interface WorkspaceSummary {
  root: string;
  git: GitSummary | null;
  /** The tools the workspace node offers, by name. */
  tools: string[];
}

export interface WorkspaceEntry {
  /** Relative to the root, `/` separated. */
  path: string;
  kind: "file" | "directory";
}

export interface WorkspaceListing {
  root: string;
  /** The folder listed; empty is the root. */
  path: string;
  entries: WorkspaceEntry[];
  truncated: boolean;
}

export interface WorkspaceFile {
  root: string;
  path: string;
  text: string;
  truncated: boolean;
}

export interface SearchMatch {
  path: string;
  /** 1-based. */
  line: number;
  text: string;
}

export interface WorkspaceSearch {
  root: string;
  matches: SearchMatch[];
  truncated: boolean;
}

export interface WorkspaceDiff {
  root: string;
  /** One unified diff of the working tree. */
  text: string;
  truncated: boolean;
}

/** One worker of a delegate node (`delegation.workers`, `regulus workers ls`). */
export interface Worker {
  /** `w-<n>`, stable for the life of the delegate. */
  worker: string;
  kind: string;
  name: string;
  /** The child execution; empty while it starts. */
  execution: string;
  /** `starting`, `running`, `closed`, `stopped`, `failed`, `interrupted`… as the module says. */
  state: string;
  pending_results: number;
  /** Milliseconds since the Unix epoch. */
  last_output_at: number | null;
  engine?: string;
}
