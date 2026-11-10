import { expect, test } from "vitest";
import { activePanel, defaultCodeLayout, moveCodePanel, readCodeLayout, shownPanels, toggleCodePanel } from "./panels";

test("the default layout shows every panel in the registry's order", () => {
  expect(defaultCodeLayout().map((entry) => entry.id)).toEqual(["files", "changes", "activity", "workers"]);
  expect(defaultCodeLayout().every((entry) => !entry.hidden)).toBe(true);
});

test("a layout from config.toml shows only what it names; one saved here gets the panels added since", () => {
  expect(readCodeLayout(["changes", "nope", "files", "changes"], "hidden")).toEqual([
    { id: "changes", hidden: false }, { id: "files", hidden: false }, { id: "activity", hidden: true }, { id: "workers", hidden: true },
  ]);
  expect(readCodeLayout([{ id: "activity", hidden: true }, "files"], "shown")?.map((entry) => [entry.id, entry.hidden])).toEqual([
    ["activity", true], ["files", false], ["changes", false], ["workers", false],
  ]);
  expect(readCodeLayout("files", "shown")).toBeNull();
  expect(readCodeLayout(["nope"], "shown")).toBeNull();
});

test("a panel with nothing to show stays out, and the one on screen falls back to the first offered", () => {
  const layout = toggleCodePanel(defaultCodeLayout(), "activity", true);
  expect(shownPanels(layout, { hasWorkers: false })).toEqual(["files", "changes"]);
  expect(shownPanels(layout, { hasWorkers: true })).toEqual(["files", "changes", "workers"]);
  expect(activePanel("workers", ["files", "changes"])).toBe("files");
  expect(activePanel("changes", ["files", "changes"])).toBe("changes");
  expect(activePanel(null, [])).toBeNull();
});

test("moving a panel swaps it with its neighbour and stops at the ends", () => {
  const layout = defaultCodeLayout();
  expect(moveCodePanel(layout, "changes", -1).map((entry) => entry.id)).toEqual(["changes", "files", "activity", "workers"]);
  expect(moveCodePanel(layout, "workers", 1)).toBe(layout);
  expect(moveCodePanel(layout, "files", -1)).toBe(layout);
});
