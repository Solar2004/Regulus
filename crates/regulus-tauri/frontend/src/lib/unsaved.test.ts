import { describe, expect, test } from "vitest";
import { guardUnload, registerUnsaved, unsavedWork } from "./unsaved";

const leaving = () => {
  let prevented = false;
  guardUnload({ preventDefault: () => { prevented = true; } } as BeforeUnloadEvent);
  return prevented;
};

describe("unsaved work across views", () => {
  test("is what the registered editors name, and leaving asks only while there is some", () => {
    let name: string | null = null;
    const off = registerUnsaved(() => name);
    expect(unsavedWork()).toEqual([]);
    expect(leaving()).toBe(false);
    name = "tools/caps/run.py";
    expect(unsavedWork()).toEqual(["tools/caps/run.py"]);
    expect(leaving()).toBe(true);
    off();
    expect(unsavedWork()).toEqual([]);
    expect(leaving()).toBe(false);
  });
});
