import capability from "../../../src-tauri/capabilities/default.json";
import { describe, expect, test } from "vitest";
import { holdClose, registerUnsaved } from "./unsaved";

const request = () => {
  let held = false;
  return { event: { preventDefault: () => { held = true; } }, held: () => held };
};

describe("a desktop window's close request", () => {
  test("is held and asked about only while something is unsaved", () => {
    let unsaved: string | null = null;
    const off = registerUnsaved(() => unsaved);
    const asked: string[][] = [];
    const clean = request();
    expect(holdClose(clean.event, (items) => asked.push(items))).toBe(false);
    expect(clean.held()).toBe(false);
    unsaved = "tools/caps/run.py";
    const dirty = request();
    expect(holdClose(dirty.event, (items) => asked.push(items))).toBe(true);
    expect(dirty.held()).toBe(true);
    expect(asked).toEqual([["tools/caps/run.py"]]);
    off();
  });

  test("the window may destroy itself: the close the page allows needs it", () => {
    // With a close-requested listener Tauri leaves closing to the page, which calls destroy().
    expect(capability.permissions).toEqual(expect.arrayContaining(["core:window:allow-close", "core:window:allow-destroy"]));
  });
});
