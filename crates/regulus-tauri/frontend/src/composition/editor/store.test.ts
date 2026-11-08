import { beforeEach, describe, expect, test } from "vitest";
import { ALL, useEditor } from "./store";
import { addWorkflow, moveNodes, renameNode } from "./document";
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
