// The Regulus Code mascot as data: the pixel grids, roles, poses and states of
// assets/design/regulus-code-mascot.json (the one source the terminal reads too), and what
// the component draws from them. Pure: tested in mascot.test.ts.
import source from "../../../../../assets/design/regulus-code-mascot.json";

export type MascotSize = keyof typeof source.grids;
export type MascotMood = keyof typeof source.states;
type RoleKey = keyof typeof source.roles;

interface Grid {
  rows: string[];
  poses: Record<string, number[][]>;
}

const grids: Record<MascotSize, Grid> = source.grids;
const roleOf = (cell: string) => (source.roles as Record<string, { name: string; token: string }>)[cell];
const EYE = (Object.keys(source.roles) as RoleKey[]).find((key) => source.roles[key].name === "eye")!;
const FACE = (Object.keys(source.roles) as RoleKey[]).find((key) => source.roles[key].name === "face")!;

/** A horizontal run of cells of one role: one rectangle. */
export interface MascotRun {
  x: number;
  y: number;
  width: number;
  /** `mane`, `face`, `muzzle`, `star`, `eye` or `nose`. */
  role: string;
  /** The palette token the role is painted with. */
  token: string;
}

function runsOf(rows: string[]): MascotRun[] {
  const runs: MascotRun[] = [];
  rows.forEach((row, y) => {
    let start = 0;
    for (let x = 1; x <= row.length; x++) {
      if (x < row.length && row[x] === row[start]) continue;
      if (row[start] !== source.empty) runs.push({ x: start, y, width: x - start, role: roleOf(row[start]).name, token: roleOf(row[start]).token });
      start = x;
    }
  });
  return runs;
}

/** The pose of a grid by name; one it does not have is `idle`. */
const poseOf = (grid: Grid, pose: string) => grid.poses[pose] ?? grid.poses.idle;

/** A grid's rows with the eyes where `pose` puts them, as the JSON's rows are written for `idle`. */
export function posedRows(size: MascotSize, pose: string): string[] {
  const grid = grids[size];
  const eyes = new Set(poseOf(grid, pose).map(([row, column]) => `${row},${column}`));
  return grid.rows.map((row, y) => [...row].map((cell, x) => eyes.has(`${y},${x}`) ? EYE : cell === EYE ? FACE : cell).join(""));
}

export interface MascotDrawing {
  width: number;
  height: number;
  /** Everything but the eyes: where an eye is closed the face shows. */
  body: MascotRun[];
  /** The eyes of each pose of the mood, in the order they take turns. */
  eyes: MascotRun[][];
  /** The palette token of the forehead star in this mood. */
  star: string;
}

/** What to draw for a size and a mood. */
export function mascotDrawing(size: MascotSize, mood: MascotMood): MascotDrawing {
  const grid = grids[size];
  const state = source.states[mood];
  const closed = grid.rows.map((row) => row.split(EYE).join(FACE));
  const eyes = state.poses.map((pose) => poseOf(grid, pose).map(([y, x]) => ({ x, y, width: 1, role: roleOf(EYE).name, token: roleOf(EYE).token })));
  const body = runsOf(closed).map((run) => run.role === "star" ? { ...run, token: state.star } : run);
  return { width: grid.rows[0].length, height: grid.rows.length, body, eyes, star: state.star };
}

/** Every run of a grid in a pose, eyes included: what the static SVG holds for `idle`. */
export const mascotRuns = (size: MascotSize, pose: string): MascotRun[] => runsOf(posedRows(size, pose));

/** A palette token as the theme's CSS variable (assets/design/tokens.css). */
export const tokenColor = (token: string) => `var(--rg-color-${token})`;

export const mascotMoods = Object.keys(source.states) as MascotMood[];
export const mascotSource = source;
