import { beforeEach, describe, expect, test } from "vitest";
import { ALL, enterWorkflow, trailIn, useEditor } from "./store";
import { addWorkflow, moveNodes, removeWorkflow, renameNode } from "./document";
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
