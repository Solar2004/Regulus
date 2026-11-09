import { describe, expect, test } from "vitest";
import { arrangePanels, changePanel, defaultLayout, movePanel, panelRegistry, readLayout, type PanelContext } from "./panels";
import type { Execution } from "./model";

const execution = (id: string, parent?: string): Execution => ({ id, profile_id: "p", workflow_id: "main", project_id: null, state: { status: "running" }, ...(parent ? { parent } : {}) });
const ctx = (extra: Partial<PanelContext> = {}): PanelContext => ({ execution: execution("a"), workflow: undefined, inputs: [], children: [], ...extra });

describe("operate panels", () => {
  test("the registry has unique ids and the default layout follows it", () => {
    expect(new Set(panelRegistry.map((panel) => panel.id)).size).toBe(panelRegistry.length);
    expect(defaultLayout().map((entry) => `${entry.place}:${entry.id}`)).toEqual(["main:nodes", "main:send", "main:events", "aside:tree", "aside:node", "aside:decisions"]);
  });

  test("a panel shows when it is in the layout, not hidden and available", () => {
    expect(arrangePanels(defaultLayout(), ctx())).toEqual({ main: ["nodes", "events"], aside: ["node", "decisions"] });
    const input = { id: "in", name: "in", capability: "text@1", direction: "input" as const, kind: "then" as const, required: false };
    expect(arrangePanels(defaultLayout(), ctx({ inputs: [input], children: [execution("b", "a")] }))).toEqual({ main: ["nodes", "send", "events"], aside: ["tree", "node", "decisions"] });
    // A child shows the tree too: it has a parent.
    expect(arrangePanels(defaultLayout(), ctx({ execution: execution("b", "a") })).aside).toContain("tree");
    expect(arrangePanels(changePanel(defaultLayout(), "events", { hidden: true, place: "aside" }), ctx()).main).toEqual(["nodes"]);
  });

  test("readLayout reads ids or tables, drops unknown and repeated ones, and appends the rest", () => {
    const fromConfig = readLayout(["events", { id: "tree", place: "main" }, "nope", "events", { id: "node", hidden: true }], "hidden")!;
    expect(fromConfig.map((entry) => [entry.id, entry.place, entry.hidden])).toEqual([
      ["events", "main", false], ["tree", "main", false], ["node", "aside", true],
      ["nodes", "main", true], ["send", "main", true], ["decisions", "aside", true],
    ]);
    expect(readLayout(["events"], "shown")!.filter((entry) => entry.hidden)).toEqual([]);
    expect(readLayout("events", "shown")).toBeNull();
    expect(readLayout([], "shown")).toBeNull();
    expect(readLayout(["nope"], "shown")).toBeNull();
  });

  test("movePanel swaps with the neighbour and stops at the ends", () => {
    expect(movePanel(defaultLayout(), "send", -1).map((entry) => entry.id).slice(0, 2)).toEqual(["send", "nodes"]);
    const layout = defaultLayout();
    expect(movePanel(layout, "nodes", -1)).toBe(layout);
    expect(movePanel(layout, "decisions", 1)).toBe(layout);
  });
});
