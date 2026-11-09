// The invocation tree of an execution: the children it started (a `call`, a delegate…), at
// any depth. Pure: tested in tree.test.ts.
import type { Execution } from "./model";
import { rpcCodes } from "./logic";

export interface ExecutionTree {
  execution: Execution;
  children: ExecutionTree[];
}

const isObject = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
const isExecution = (value: unknown): value is Execution => isObject(value) && typeof value.id === "string" && isObject(value.state);

/** An `execution.tree` answer as the view reads it: the execution under `execution` (or `root`), children nested the same way. */
export function readTree(answer: unknown): ExecutionTree | null {
  if (!isObject(answer)) return null;
  const execution = answer.execution ?? answer.root;
  if (!isExecution(execution)) return null;
  const children = Array.isArray(answer.children) ? answer.children.map(readTree).filter((child): child is ExecutionTree => child !== null) : [];
  return { execution, children };
}

/** The same tree from the executions list and their `parent`, for a core without `execution.tree`. */
export function treeFromList(executions: Execution[], id: string, seen = new Set<string>()): ExecutionTree | null {
  const execution = executions.find((item) => item.id === id);
  if (!execution || seen.has(id)) return null;
  seen.add(id);
  const children = executions.filter((item) => item.parent === id).map((child) => treeFromList(executions, child.id, seen)).filter((child): child is ExecutionTree => child !== null);
  return { execution, children };
}

/** How many executions hang below the root. */
export const descendants = (tree: ExecutionTree): number => tree.children.reduce((sum, child) => sum + 1 + descendants(child), 0);

export type LoadedTree = { tree: ExecutionTree | null; source: "core" | "list" };

/** Asks the core for the tree; a core without the method (`-32601`) gets it built from the list. */
export async function loadTree(fetch: (id: string) => Promise<unknown>, executions: Execution[], id: string): Promise<LoadedTree> {
  try {
    return { tree: readTree(await fetch(id)), source: "core" };
  } catch (error) {
    if ((error as { code?: unknown } | null)?.code !== rpcCodes.methodNotFound) throw error;
    return { tree: treeFromList(executions, id), source: "list" };
  }
}
