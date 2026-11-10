import { expect, test } from "vitest";
import palette from "../../../../../assets/design/palette.json";
import svg from "../../../../../assets/design/regulus-code-mascot.svg?raw";
import { mascotDrawing, mascotMoods, mascotRuns, mascotSource, posedRows, tokenColor } from "./mascot";

test("the idle pose is the grid as the JSON writes it", () => {
  expect(posedRows("full", "idle")).toEqual(mascotSource.grids.full.rows);
  expect(posedRows("mini", "idle")).toEqual(mascotSource.grids.mini.rows);
  // A pose a grid does not have draws as idle.
  expect(posedRows("mini", "left")).toEqual(mascotSource.grids.mini.rows);
});

test("the SVG in assets/design is this component's idle drawing, rectangle for rectangle", () => {
  const rects = [...svg.matchAll(/<rect class="(\w+)" x="(\d+)" y="(\d+)" width="(\d+)" height="1" \/>/g)]
    .map(([, role, x, y, width]) => ({ role, x: Number(x), y: Number(y), width: Number(width) }));
  expect(rects.length).toBeGreaterThan(40);
  expect(rects).toEqual(mascotRuns("full", "idle").map(({ role, x, y, width }) => ({ role, x, y, width })));
  expect(svg).toContain(`viewBox="0 0 ${mascotSource.grids.full.rows[0].length} ${mascotSource.grids.full.rows.length}"`);
});

test("a pose only moves the eyes, and keeps them inside the face", () => {
  const closed = (rows: string[]) => rows.map((row) => row.split("E").join("F"));
  for (const pose of Object.keys(mascotSource.grids.full.poses)) {
    expect(closed(posedRows("full", pose)), pose).toEqual(closed(mascotSource.grids.full.rows));
    expect(posedRows("full", pose).join("").split("E").length - 1, pose).toBe(mascotSource.grids.full.poses[pose as "idle"].length);
  }
});

test("every mood draws: its poses in turn over a body without eyes, and a star color of the palette", () => {
  const colors: Record<string, string> = palette.colors;
  expect(mascotMoods).toEqual(["idle", "working", "waiting", "failed", "stopped"]);
  for (const mood of mascotMoods) {
    const drawing = mascotDrawing("full", mood);
    expect([drawing.width, drawing.height]).toEqual([15, 14]);
    expect(drawing.body.some((run) => run.role === "eye")).toBe(false);
    expect(drawing.body.filter((run) => run.role === "star")).toEqual([{ x: 7, y: 4, width: 1, role: "star", token: mascotSource.states[mood].star }]);
    expect(drawing.eyes.length).toBe(mascotSource.states[mood].poses.length);
    expect(drawing.eyes.every((eyes) => eyes.length > 0)).toBe(true);
    expect(colors[drawing.star], mood).toBeDefined();
  }
  expect(mascotDrawing("full", "waiting").star).toBe("warning");
  // The small grid blinks by closing its eyes: a pose without any.
  expect(mascotDrawing("mini", "idle").eyes.map((eyes) => eyes.length)).toEqual([2, 0]);
});

test("each role is painted with a token of the palette, never a color of its own", () => {
  const colors: Record<string, string> = palette.colors;
  for (const role of Object.values(mascotSource.roles)) expect(colors[role.token], role.name).toBeDefined();
  expect(new Set(mascotRuns("full", "idle").map((run) => run.token))).toEqual(new Set(["steel", "focus", "text", "canvas"]));
  expect(tokenColor("steel")).toBe("var(--rg-color-steel)");
});
