import { beforeEach, describe, expect, test } from "vitest";
import { ALL, canUndoArrange, enterWorkflow, trailIn, useEditor } from "./store";
import { addWorkflow, moveNodes, removeNodes, removeWorkflow, renameNode } from "./document";
import { sample } from "./fixtures.test-util";

describe("editor store", () => {
  beforeEach(() => useEditor.getState().open(null));

  test("opening a profile shows its main workflow, clean; a new one is dirty", () => {
    const profile = sample();
    useEditor.getState().open(profile);
    expect(useEditor.getState()).toMatchObject({ workflowId: "main", dirty: false, isNew: false });
    expect(useEditor.getState().doc).not.toBe(profile);
    useEditor.getState().open(profile, true);
    expect(useEditor.getState()).toMatchObject({ dirty: true, isNew: true });
    useEditor.getState().open(null);
    expect(useEditor.getState().workflowId).toBe(ALL);
  });

  test("edits mark dirty; a change that changes nothing does not", () => {
    useEditor.getState().open(sample());
    useEditor.getState().edit((profile) => renameNode(profile, "nope", "a", "x"));
    expect(useEditor.getState().dirty).toBe(false);
    useEditor.getState().edit((profile) => moveNodes(profile, "main", [{ id: "a", position: { x: 5, y: 5 } }]));
    expect(useEditor.getState().dirty).toBe(true);
    expect(useEditor.getState().doc?.workflows[0].nodes[0].position).toEqual({ x: 5, y: 5 });
  });

  test("saving cleans and keeps the workflow on screen if it still exists", () => {
    useEditor.getState().open(sample(), true);
    useEditor.getState().edit((profile) => addWorkflow(profile, "Extra").profile);
    useEditor.getState().showWorkflow("extra");
    useEditor.getState().select({ nodes: ["a"], wires: [] });
    expect(useEditor.getState().selection.nodes).toEqual(["a"]);
    useEditor.getState().saved(useEditor.getState().doc!, []);
    expect(useEditor.getState()).toMatchObject({ dirty: false, isNew: false, workflowId: "extra" });
    useEditor.getState().saved(sample(), []);
    expect(useEditor.getState().workflowId).toBe("main");
  });

  test("focus shows a workflow and selects a node of it; what the profile lacks changes nothing", () => {
    useEditor.getState().open(sample());
    useEditor.getState().focus({ workflowId: "inner", nodeId: "h" });
    expect(useEditor.getState()).toMatchObject({ workflowId: "inner", trail: ["inner"], selection: { nodes: ["h"], wires: [] } });
    // A node the workflow does not have: the workflow shows, nothing is selected.
    useEditor.getState().focus({ workflowId: "main", nodeId: "nope" });
    expect(useEditor.getState()).toMatchObject({ workflowId: "main", selection: { nodes: [], wires: [] } });
    // Only a node: it is looked for in the workflow on screen.
    useEditor.getState().focus({ nodeId: "b" });
    expect(useEditor.getState()).toMatchObject({ workflowId: "main", selection: { nodes: ["b"], wires: [] } });
    useEditor.getState().focus({ workflowId: "gone", nodeId: "a" });
    expect(useEditor.getState()).toMatchObject({ workflowId: "main", selection: { nodes: ["b"], wires: [] } });
  });
});

describe("breadcrumb", () => {
  beforeEach(() => useEditor.getState().open(null));

  test("enterWorkflow goes one step deeper, or back to a workflow already on the way", () => {
    expect(enterWorkflow(["main"], "inner")).toEqual(["main", "inner"]);
    expect(enterWorkflow(["main", "inner", "deep"], "inner")).toEqual(["main", "inner"]);
    expect(enterWorkflow([], "inner")).toEqual(["inner"]);
  });

  test("trailIn cuts the trail at the first workflow the profile no longer has", () => {
    expect(trailIn(["main", "inner"], sample())).toEqual(["main", "inner"]);
    expect(trailIn(["main", "gone", "inner"], sample())).toEqual(["main"]);
  });

  test("opening a box walks in, the trail walks back, a tab starts over", () => {
    useEditor.getState().open(sample());
    expect(useEditor.getState().trail).toEqual(["main"]);
    useEditor.getState().select({ nodes: ["a"], wires: [] });
    useEditor.getState().openBox("inner");
    expect(useEditor.getState()).toMatchObject({ workflowId: "inner", trail: ["main", "inner"], selection: { nodes: [] } });
    useEditor.getState().backTo(0);
    expect(useEditor.getState()).toMatchObject({ workflowId: "main", trail: ["main"] });
    useEditor.getState().openBox("inner");
    useEditor.getState().showWorkflow("main");
    expect(useEditor.getState().trail).toEqual(["main"]);
    useEditor.getState().showWorkflow(ALL);
    expect(useEditor.getState().trail).toEqual([]);
    useEditor.getState().openBox("inner");
    expect(useEditor.getState().trail).toEqual(["inner"]);
  });

  test("removing the workflow on screen goes back along the trail", () => {
    useEditor.getState().open(sample());
    useEditor.getState().openBox("inner");
    useEditor.getState().edit((profile) => removeWorkflow(profile, "inner"));
    expect(useEditor.getState()).toMatchObject({ workflowId: "main", trail: ["main"] });
  });
});

describe("arranging", () => {
  beforeEach(() => useEditor.getState().open(null));
  const moves = [
    { id: "a", from: { x: 100, y: 50 }, to: { x: 0, y: 0 } },
    { id: "b", from: { x: 400, y: 80 }, to: { x: 300, y: 0 } },
  ];
  const position = (id: string) => useEditor.getState().doc!.workflows[0].nodes.find((node) => node.id === id)!.position;

  test("applies the moves, marks dirty and can be undone to the exact old positions", () => {
    useEditor.getState().open(sample());
    const wires = useEditor.getState().doc!.workflows[0].wires;
    useEditor.getState().arrange("main", moves);
    expect(position("a")).toEqual({ x: 0, y: 0 });
    expect(position("b")).toEqual({ x: 300, y: 0 });
    expect(useEditor.getState()).toMatchObject({ dirty: true });
    expect(canUndoArrange(useEditor.getState(), "main")).toBe(true);
    expect(canUndoArrange(useEditor.getState(), "inner")).toBe(false);
    expect(useEditor.getState().undoArrange()).toBe(true);
    expect(position("a")).toEqual({ x: 100, y: 50 });
    expect(position("b")).toEqual({ x: 400, y: 80 });
    expect(useEditor.getState().doc!.workflows[0].wires).toEqual(wires);
    expect(useEditor.getState()).toMatchObject({ arranged: null });
    expect(useEditor.getState().undoArrange()).toBe(false);
  });

  test("other edits keep the undo; moving an arranged node by hand takes it away", () => {
    useEditor.getState().open(sample());
    useEditor.getState().arrange("main", moves);
    useEditor.getState().edit((profile) => renameNode(profile, "main", "c", "renamed"));
    expect(canUndoArrange(useEditor.getState(), "main")).toBe(true);
    useEditor.getState().edit((profile) => moveNodes(profile, "main", [{ id: "a", position: { x: 9, y: 9 } }]));
    expect(canUndoArrange(useEditor.getState(), "main")).toBe(false);
    expect(useEditor.getState().undoArrange()).toBe(false);
    expect(position("a")).toEqual({ x: 9, y: 9 });
    expect(useEditor.getState().arranged).toBeNull();
  });

  test("a deleted node, a new profile or no moves leave no undo", () => {
    useEditor.getState().open(sample());
    const rev = useEditor.getState().arrangeRev;
    useEditor.getState().arrange("main", []);
    expect(useEditor.getState()).toMatchObject({ arranged: null, dirty: false, arrangeRev: rev });
    useEditor.getState().arrange("main", moves);
    useEditor.getState().edit((profile) => removeNodes(profile, "main", ["b"]));
    expect(canUndoArrange(useEditor.getState(), "main")).toBe(false);
    useEditor.getState().arrange("main", moves);
    useEditor.getState().open(sample());
    expect(useEditor.getState().arranged).toBeNull();
  });

  test("saving keeps the undo while the positions are the same", () => {
    useEditor.getState().open(sample());
    useEditor.getState().arrange("main", moves);
    useEditor.getState().saved(structuredClone(useEditor.getState().doc!), []);
    expect(canUndoArrange(useEditor.getState(), "main")).toBe(true);
  });
});
