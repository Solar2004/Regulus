import { describe, expect, test } from "vitest";
import { sample, port } from "../editor/fixtures.test-util";
import { useEditor } from "../editor/store";
import { listItems } from "../editor/fields";
import { manifestPorts } from "../editor/palette";

describe("a sync in the editor document", () => {
  test("is one change that can be undone while nothing changed after it", () => {
    const before = sample();
    useEditor.getState().open(before);
    const opened = useEditor.getState().doc!;
    const after = { ...opened, name: "synced" };
    useEditor.getState().applySynced(after);
    expect(useEditor.getState()).toMatchObject({ doc: after, dirty: true });
    expect(useEditor.getState().undoSync()).toBe(true);
    expect(useEditor.getState().doc).toBe(opened);
    // A second undo has nothing to take back.
    expect(useEditor.getState().undoSync()).toBe(false);
  });

  test("is gone once the profile changed after it", () => {
    useEditor.getState().open(sample());
    const opened = useEditor.getState().doc!;
    useEditor.getState().applySynced({ ...opened, name: "synced" });
    useEditor.getState().edit((profile) => ({ ...profile, name: "edited later" }));
    expect(useEditor.getState().undoSync()).toBe(false);
    expect(useEditor.getState().doc!.name).toBe("edited later");
  });
});

describe("node types without a shape from the core", () => {
  test("start with the manifest's fixed ports only, as copies", () => {
    const fixed = [port("in", "input"), port("out", "output")];
    const ports = manifestPorts({ type: "t", name: "T", port: fixed, dynamic_port: [{ ...port("x_{item}", "output"), repeat: "xs" }] });
    expect(ports).toEqual(fixed);
    expect(ports[0]).not.toBe(fixed[0]);
  });

  test("a list field reads its items as text", () => {
    expect(listItems(["a", "b"])).toEqual(["a", "b"]);
    expect(listItems(undefined)).toEqual([]);
    expect(listItems("solo")).toEqual(["solo"]);
    expect(listItems([1, true])).toEqual(["1", "true"]);
  });
});
