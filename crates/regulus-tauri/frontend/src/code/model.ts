// Shapes the Regulus Code view reads besides those of Operate (`operate/model.ts`): the
// read-only `code.inspect` of the module that offers `code.workspace@1`, and the workers a
// delegate node lists. As the modules serve them; the view reads them through `logic.ts`,
// which tolerates a field that is missing or shaped another way.

export type InspectAction = "summary" | "list" | "read" | "search" | "diff";

/** `summary.git` when the folder is a repository git could read. */
export interface GitSummary {
  /** `null` on a detached HEAD. */
  branch: string | null;
  /** `git status --porcelain=v1` text: `XY path` per line, `XY old -> new` for a rename, empty when clean. */
  status: string;
  head?: string | null;
  upstream?: string;
  ahead?: number;
  behind?: number;
  /** The same lines, parsed. */
  changes?: { path: string; code: string; from?: string }[];
  truncated?: boolean;
}

export interface WorkspaceSummary {
  root: string;
  /** `null`: not a repository, git not installed, or git refused or timed out. */
  git: GitSummary | null;
  /** Why `git` is `null`. */
  git_reason?: string;
  /** The tools the workspace node offers, by name. */
  tools: string[];
}

export interface WorkspaceEntry {
  /** Relative to the root, `/` separated. */
  path: string;
  /** A link is listed with the kind of what it points to, when that stays inside the root. */
  kind: "file" | "directory";
  /** Bytes, for a file. */
  size?: number;
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
  /** The file from its start, as far as the module's limit lets. */
  text: string;
  /** The file goes on past `text`. */
  truncated: boolean;
  /** The whole file, in bytes. */
  size?: number;
}

/** One line that holds the query; the query is literal and case-insensitive. */
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
  /** Tracked changes against `HEAD`, staged and unstaged, as one unified diff. Untracked files are not in it. */
  text: string;
  truncated: boolean;
  /** Why there is no diff, outside a repository. */
  git_reason?: string;
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
