import { describe, expect, test } from "vitest";
import manifests from "../../../mock/manifests.json";
import type { Manifest, ModuleDefinition } from "../model";
import { boxableWorkflows, boxNode, entryKey, filterPalette, findNodeType, groupPalette, moduleStateOf, nodeFromType, paletteEntries } from "./palette";
import { sample } from "./fixtures.test-util";

const all = manifests as unknown as Manifest[];
const modules: ModuleDefinition[] = [
  { id: "regulus/flow@0.1.0", name: "Flow", node_types: ["call", "switch"], workflow_id: null, state: { status: "idle", last_used: null } },
  { id: "regulus/files@0.1.0", name: "Files", node_types: ["read", "write"], workflow_id: null, state: { status: "broken", reason: "boom" } },
  { id: "regulus/old@0.1.0", name: "Old", node_types: [], workflow_id: null, state: { status: "incompatible", requires: "^2", have: "1.11" } },
  { id: "regulus/active@0.1.0", name: "Active", node_types: [], workflow_id: null, state: { status: "active", since: 1 } },
];

describe("module state", () => {
  test("installed, broken, incompatible and not installed, by id with or without version", () => {
    expect(moduleStateOf(modules, "regulus/flow")).toBe("installed");
    expect(moduleStateOf(modules, "regulus/active@0.2.0")).toBe("installed");
    expect(moduleStateOf(modules, "regulus/files@0.1.0")).toBe("broken");
    expect(moduleStateOf(modules, "regulus/old")).toBe("incompatible");
    expect(moduleStateOf(modules, "regulus/agent@0.1.0")).toBe("not-installed");
    expect(moduleStateOf(modules, null)).toBe("installed");
    expect(moduleStateOf([{ id: "x", name: "X", node_types: [], workflow_id: null }], "x")).toBe("installed");
  });
});

describe("entries", () => {
  const entries = paletteEntries(all, modules);
  test("one per node type, with module and state", () => {
    expect(entries.map((entry) => entry.type)).toEqual(["call", "switch", "read", "write"]);
    expect(entries[3]).toMatchObject({ key: entryKey("regulus/files@0.1.0", "write"), moduleName: "Files", name: "Prompt to file", state: "broken" });
  });
  test("search matches every word in name, type or module", () => {
    expect(filterPalette(entries, "").length).toBe(4);
    expect(filterPalette(entries, "FILE prompt").map((entry) => entry.type)).toEqual(["read", "write"]);
    expect(filterPalette(entries, "flow sw").map((entry) => entry.type)).toEqual(["switch"]);
    expect(filterPalette(entries, "zzz")).toEqual([]);
  });
  test("groups by module, by state with usable first, or none", () => {
    expect(groupPalette(entries, "module").map((group) => [group.label, group.entries.length])).toEqual([["Files", 2], ["Flow", 2]]);
    expect(groupPalette(entries, "state").map((group) => group.key)).toEqual(["installed", "broken"]);
    expect(groupPalette(entries, "none")).toHaveLength(1);
    expect(groupPalette([], "none")).toEqual([]);
  });
});

describe("new nodes", () => {
  test("a node from a manifest type has its ports, defaults as config and a free id", () => {
    const found = findNodeType(all, entryKey("regulus/files@0.1.0", "write"))!;
    const created = nodeFromType(found.manifest, found.nodeType, { x: 10, y: 20 }, ["write"]);
    expect(created).toMatchObject({ id: "write-2", type: "write", name: "Prompt to file", module_id: "regulus/files@0.1.0", position: { x: 10, y: 20 }, internal_workflow_id: null });
    expect(created.ports.map((port) => port.id)).toEqual(["in", "written"]);
    expect(created.config).toMatchObject({ path: "output.md", mode: "overwrite" });
    expect(created.ports).not.toBe(found.nodeType.port);
    expect(findNodeType(all, "nope#x")).toBeNull();
  });
  test("a box places another workflow with ports", () => {
    const profile = sample();
    expect(boxableWorkflows(profile, "main").map((workflow) => workflow.id)).toEqual(["inner"]);
    expect(boxableWorkflows(profile, "inner").map((workflow) => workflow.id)).toEqual(["main"]);
    expect(boxNode(profile.workflows[1], { x: 1, y: 2 }, ["inner"])).toMatchObject({ id: "inner-2", module_id: null, internal_workflow_id: "inner", ports: [], type: "workflow", name: "Inner" });
  });
});

describe("manifest of a placed node", () => {
  test("found by module with or without version, absent otherwise", async () => {
    const { manifestTypeOf } = await import("./palette");
    expect(manifestTypeOf(all, "regulus/files", "write")?.name).toBe("Prompt to file");
    expect(manifestTypeOf(all, "regulus/files@9.9.9", "read")?.name).toBe("File to prompt");
    expect(manifestTypeOf(all, "regulus/agent@0.1.0", "agent")).toBeNull();
    expect(manifestTypeOf(all, null, "workflow")).toBeNull();
  });
});
