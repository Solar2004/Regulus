import { beforeEach, describe, expect, test } from "vitest";
import { unsavedWork } from "@/lib/unsaved";
import { sample } from "../editor/fixtures.test-util";
import { useEditor } from "../editor/store";
import { sourceKey, useAuthoring } from "./store";

const caps = { projectId: "demo", module: "caps", path: "tools/caps/run.py", moduleId: "demo/caps@0.1.0" };
const shout = { projectId: "demo", module: "shout", path: "tools/shout/run.py", moduleId: "demo/shout@0.1.0" };
const base = { text: "print(1)\n", revision: "r1" };

beforeEach(() => {
  useAuthoring.setState({ source: null, draft: null, pending: null });
  useEditor.getState().open(null);
});

describe("the open source's unsaved text", () => {
  test("stays in the store when its panel goes (another view), and counts as unsaved work", () => {
    useAuthoring.getState().openSource(caps);
    useAuthoring.getState().keepDraft(sourceKey(caps), { path: caps.path, text: "print(2)\n", base });
    // The panel unmounting does not touch the store: the draft is there when it comes back.
    expect(useAuthoring.getState().draft).toEqual({ key: sourceKey(caps), path: caps.path, text: "print(2)\n", base });
    expect(unsavedWork()).toEqual([caps.path]);
    useAuthoring.getState().keepDraft(sourceKey(caps), null);
    expect(unsavedWork()).toEqual([]);
  });

  test("another source waits while there is unsaved text; the person opens it or stays", () => {
    const { openSource, keepDraft, resolvePending } = useAuthoring.getState();
    openSource(caps);
    keepDraft(sourceKey(caps), { path: caps.path, text: "edited", base });
    openSource(shout);
    expect(useAuthoring.getState()).toMatchObject({ source: caps, pending: shout });
    expect(useAuthoring.getState().draft?.text).toBe("edited");
    resolvePending(false);
    expect(useAuthoring.getState()).toMatchObject({ source: caps, pending: null });
    expect(useAuthoring.getState().draft?.text).toBe("edited");
    openSource(shout);
    resolvePending(true);
    expect(useAuthoring.getState()).toMatchObject({ source: shout, pending: null, draft: null });
  });

  test("without unsaved text another source opens at once, and the same one keeps its draft", () => {
    const { openSource, keepDraft } = useAuthoring.getState();
    openSource(caps);
    openSource(shout);
    expect(useAuthoring.getState()).toMatchObject({ source: shout, pending: null });
    keepDraft(sourceKey(shout), { path: shout.path, text: "x", base });
    openSource(shout);
    expect(useAuthoring.getState()).toMatchObject({ source: shout, pending: null });
    expect(useAuthoring.getState().draft?.text).toBe("x");
  });

  test("a late write from a panel that is gone does not land on the source shown now", () => {
    const { openSource, keepDraft, resolvePending, closeSource } = useAuthoring.getState();
    openSource(caps);
    keepDraft(sourceKey(caps), { path: caps.path, text: "caps edit", base });
    openSource(shout);
    resolvePending(true);
    keepDraft(sourceKey(caps), { path: caps.path, text: "caps edit, flushed on unmount", base });
    keepDraft(sourceKey(caps), null);
    expect(useAuthoring.getState()).toMatchObject({ source: shout, draft: null });
    keepDraft(sourceKey(shout), { path: shout.path, text: "shout edit", base });
    keepDraft(sourceKey(caps), null);
    expect(useAuthoring.getState().draft?.text).toBe("shout edit");
    closeSource();
    expect(useAuthoring.getState()).toMatchObject({ source: null, draft: null, pending: null });
    keepDraft(sourceKey(shout), { path: shout.path, text: "after close", base });
    expect(useAuthoring.getState().draft).toBeNull();
  });

  test("source and profile drafts are separate unsaved work", () => {
    useAuthoring.getState().openSource(caps);
    useAuthoring.getState().keepDraft(sourceKey(caps), { path: caps.path, text: "edited", base });
    const profile = sample();
    useEditor.getState().open(profile);
    expect(unsavedWork()).toEqual([caps.path]);
    useEditor.getState().edit((p) => ({ ...p, name: "Renamed" }));
    expect(unsavedWork().sort()).toEqual(["Renamed", caps.path].sort());
    // Opening another profile does not touch the source draft.
    useEditor.getState().open(sample());
    expect(useAuthoring.getState().draft?.text).toBe("edited");
  });
});
