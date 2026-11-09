import { describe, expect, test } from "vitest";
import type { Execution } from "./model";
import { descendants, loadTree, readTree, treeFromList } from "./tree";

const execution = (id: string, parent?: string, extra: Partial<Execution> = {}): Execution =>
  ({ id, profile_id: "p", workflow_id: "main", project_id: null, state: { status: "running" }, ...(parent ? { parent } : {}), ...extra });

describe("invocation tree", () => {
  test("readTree takes execution or root, nests children and drops what is not an execution", () => {
    const answer = { execution: execution("a"), children: [{ execution: execution("b", "a", { caller: "call", tag: "t" }), children: [{ execution: execution("c", "b"), children: [] }] }, { nope: 1 }] };
    const tree = readTree(answer)!;
    expect(tree.execution.id).toBe("a");
    expect(tree.children.map((child) => child.execution.caller)).toEqual(["call"]);
    expect(tree.children[0].children[0].execution.id).toBe("c");
    expect(descendants(tree)).toBe(2);
    expect(readTree({ root: execution("a"), children: [] })?.execution.id).toBe("a");
    expect(readTree({ children: [] })).toBeNull();
    expect(readTree(null)).toBeNull();
  });

  test("treeFromList builds it from parent, at any depth, without looping", () => {
    const list = [execution("a"), execution("b", "a"), execution("c", "b"), execution("d", "a"), execution("x")];
    const tree = treeFromList(list, "a")!;
    expect(tree.children.map((child) => child.execution.id)).toEqual(["b", "d"]);
    expect(tree.children[0].children.map((child) => child.execution.id)).toEqual(["c"]);
    expect(treeFromList(list, "nope")).toBeNull();
    const looped = [execution("a", "b"), execution("b", "a")];
    expect(descendants(treeFromList(looped, "a")!)).toBe(1);
  });

  test("loadTree uses the core, falls back to the list without the method, and rethrows other errors", async () => {
    const list = [execution("a"), execution("b", "a")];
    expect(await loadTree(async () => ({ execution: execution("a"), children: [] }), list, "a")).toMatchObject({ source: "core", tree: { children: [] } });
    const fallback = await loadTree(async () => { throw Object.assign(new Error("Method not found"), { code: -32601 }); }, list, "a");
    expect(fallback.source).toBe("list");
    expect(fallback.tree?.children.map((child) => child.execution.id)).toEqual(["b"]);
    await expect(loadTree(async () => { throw Object.assign(new Error("boom"), { code: -32602 }); }, list, "a")).rejects.toThrow("boom");
  });
});
