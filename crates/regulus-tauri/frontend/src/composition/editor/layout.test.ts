import { describe, expect, test } from "vitest";
import { arrangeBoxes, arrangeWorkflow, clearOrigin, footprint, layoutWires, placeNode, type Box, type LayoutOptions, type Move, type Rect } from "./layout";
import { measuredSizes } from "./flow";
import { node as fixtureNode, port, sample } from "./fixtures.test-util";
import type { Profile } from "../model";

const options: LayoutOptions = { direction: "right", layerGap: 100, nodeGap: 20, fallback: { width: 200, height: 100 } };
const box = (id: string, x = 0, y = 0, width = 200, height = 100): Box => ({ id, x, y, width, height });
const apply = (boxes: Box[], moves: Move[]): Box[] => boxes.map((item) => {
  const move = moves.find((m) => m.id === item.id);
  return move ? { ...item, x: move.to.x, y: move.to.y } : item;
});
const overlaps = (a: Rect, b: Rect) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
const overlapping = (boxes: Box[]) => boxes.flatMap((a, i) => boxes.slice(i + 1).filter((b) => overlaps(a, b)).map((b) => `${a.id}/${b.id}`));
const at = (boxes: Box[], id: string) => boxes.find((item) => item.id === id)!;

describe("arrange", () => {
  test("a chain runs left to right, one rank after the other, in a row", () => {
    const boxes = ["a", "b", "c"].map((id) => box(id, 500, 500));
    const placed = apply(boxes, arrangeBoxes(boxes, [["a", "b"], ["b", "c"]], null, options));
    expect(placed.map((item) => item.x)).toEqual([500, 800, 1100]);
    expect(new Set(placed.map((item) => item.y))).toEqual(new Set([500]));
  });

  test("top to bottom swaps the axes", () => {
    const boxes = ["a", "b", "c"].map((id) => box(id));
    const placed = apply(boxes, arrangeBoxes(boxes, [["a", "b"], ["b", "c"]], null, { ...options, direction: "down" }));
    expect(placed.map((item) => item.y)).toEqual([0, 200, 400]);
    expect(new Set(placed.map((item) => item.x))).toEqual(new Set([0]));
  });

  test("the block keeps the top-left corner it had", () => {
    const boxes = [box("a", 300, 70), box("b", 900, 400)];
    const placed = apply(boxes, arrangeBoxes(boxes, [["a", "b"]], null, options));
    expect(Math.min(...placed.map((item) => item.x))).toBe(300);
    expect(Math.min(...placed.map((item) => item.y))).toBe(70);
  });

  test("sources of one node share a rank and use their real heights, not the fallback", () => {
    const boxes = [box("a", 0, 0, 200, 300), box("b", 0, 0, 200, 50), box("sink")];
    const placed = apply(boxes, arrangeBoxes(boxes, [["a", "sink"], ["b", "sink"]], null, options));
    expect(at(placed, "a").x).toBe(at(placed, "b").x);
    expect(at(placed, "b").y).toBeGreaterThanOrEqual(at(placed, "a").y + 300 + options.nodeGap);
    expect(overlapping(placed)).toEqual([]);
  });

  test("a rank is as wide as its widest node", () => {
    const boxes = [box("a", 0, 0, 400, 100), box("b")];
    const placed = apply(boxes, arrangeBoxes(boxes, [["a", "b"]], null, options));
    expect(at(placed, "b").x).toBe(at(placed, "a").x + 400 + options.layerGap);
  });

  test("the fallback footprint and the measured size agree through footprint()", () => {
    const measured = new Map([["a", { width: 321, height: 123 }], ["z", { width: 0, height: 0 }]]);
    expect(footprint(measured, "a", options.fallback)).toEqual({ width: 321, height: 123 });
    expect(footprint(measured, "missing", options.fallback)).toEqual(options.fallback);
    expect(footprint(measured, "z", options.fallback)).toEqual(options.fallback);
    expect(footprint(undefined, "a", options.fallback)).toEqual(options.fallback);
  });

  test("a cycle terminates, places every node, does not overlap and is repeatable", () => {
    const boxes = ["a", "b", "c", "d"].map((id) => box(id));
    const wires: [string, string][] = [["a", "b"], ["b", "c"], ["c", "a"], ["c", "d"], ["d", "d"]];
    const first = arrangeBoxes(boxes, wires, null, options);
    expect(arrangeBoxes(boxes, wires, null, options)).toEqual(first);
    const placed = apply(boxes, first);
    expect(overlapping(placed)).toEqual([]);
    // The wire that closes the cycle is the one ignored: a still comes before b before c before d.
    expect(at(placed, "a").x).toBeLessThan(at(placed, "b").x);
    expect(at(placed, "b").x).toBeLessThan(at(placed, "c").x);
    expect(at(placed, "c").x).toBeLessThan(at(placed, "d").x);
  });

  test("a loop of every node and two-node loops still lay out", () => {
    const boxes = ["a", "b"].map((id) => box(id));
    const placed = apply(boxes, arrangeBoxes(boxes, [["a", "b"], ["b", "a"]], null, options));
    expect(overlapping(placed)).toEqual([]);
    expect(at(placed, "a").x).toBeLessThan(at(placed, "b").x);
  });

  test("disconnected groups and loose nodes are stacked, never on top of each other", () => {
    const boxes = ["a", "b", "c", "d", "lone", "other"].map((id) => box(id));
    const moves = arrangeBoxes(boxes, [["a", "b"], ["c", "d"]], null, options);
    const placed = apply(boxes, moves);
    expect(overlapping(placed)).toEqual([]);
    expect(moves.length).toBeGreaterThan(0);
    expect(arrangeBoxes(boxes, [["a", "b"], ["c", "d"]], null, options)).toEqual(moves);
  });

  test("wires to unknown nodes, repeated wires and self-loops are ignored", () => {
    const boxes = [box("a"), box("b")];
    const a = arrangeBoxes(boxes, [["a", "b"]], null, options);
    expect(arrangeBoxes(boxes, [["a", "b"], ["a", "b"], ["a", "a"], ["x", "a"], ["b", "y"]], null, options)).toEqual(a);
  });

  test("arranging an arranged layout moves nothing", () => {
    const boxes = [box("a"), box("b"), box("c")];
    const wires: [string, string][] = [["a", "b"], ["a", "c"]];
    const placed = apply(boxes, arrangeBoxes(boxes, wires, null, options));
    expect(arrangeBoxes(placed, wires, null, options)).toEqual([]);
  });

  test("nothing to arrange gives no moves", () => {
    expect(arrangeBoxes([], [], null, options)).toEqual([]);
    expect(arrangeBoxes([box("a")], [], new Set(["nope"]), options)).toEqual([]);
  });

  test("with a grid, every position is a multiple of it and nothing overlaps", () => {
    const boxes = [box("a", 13, 7, 251, 133), box("b", 0, 0, 249, 161), box("c"), box("d", 0, 0, 301, 77)];
    for (const direction of ["right", "down"] as const) {
      const placed = apply(boxes, arrangeBoxes(boxes, [["a", "c"], ["b", "c"], ["c", "d"]], null, { ...options, direction, grid: 16 }));
      expect(placed.every((item) => item.x % 16 === 0 && item.y % 16 === 0)).toBe(true);
      expect(overlapping(placed)).toEqual([]);
    }
  });

  test("random graphs, both directions: always every node placed, never overlapping, repeatable", () => {
    let seed = 7;
    const random = () => { seed = (seed * 1664525 + 1013904223) % 4294967296; return seed / 4294967296; };
    for (let round = 0; round < 40; round++) {
      const count = 2 + Math.floor(random() * 14);
      const boxes = Array.from({ length: count }, (_, n) => box(`n${n}`, Math.floor(random() * 900), Math.floor(random() * 900), 120 + Math.floor(random() * 300), 40 + Math.floor(random() * 260)));
      const wires: [string, string][] = Array.from({ length: Math.floor(random() * count * 1.5) }, () => [`n${Math.floor(random() * count)}`, `n${Math.floor(random() * count)}`]);
      for (const direction of ["right", "down"] as const) {
        const config = { ...options, direction };
        const moves = arrangeBoxes(boxes, wires, null, config);
        expect(arrangeBoxes(boxes, wires, null, config)).toEqual(moves);
        expect(overlapping(apply(boxes, moves)), `round ${round} ${direction}`).toEqual([]);
      }
    }
  });
});

describe("arrange a selection", () => {
  const boxes = [box("a", 0, 0), box("b", 0, 0), box("c", 0, 0), box("fixed1", 50, 50), box("fixed2", 900, 900)];

  test("only the selected nodes move; the others are not even in the result", () => {
    const moves = arrangeBoxes(boxes, [["a", "b"], ["b", "c"]], new Set(["a", "b", "c"]), options);
    expect(moves.every((move) => ["a", "b", "c"].includes(move.id))).toBe(true);
    const placed = apply(boxes, moves);
    expect(at(placed, "fixed1")).toEqual(boxes[3]);
    expect(at(placed, "fixed2")).toEqual(boxes[4]);
  });

  test("the arranged block goes aside rather than onto a node that stays", () => {
    const moves = arrangeBoxes(boxes, [["a", "b"], ["b", "c"]], new Set(["a", "b", "c"]), options);
    expect(overlapping(apply(boxes, moves))).toEqual([]);
  });

  test("a wire that leaves the selection does not shape it", () => {
    const withOutside = arrangeBoxes(boxes, [["a", "b"], ["b", "fixed2"], ["fixed1", "a"]], new Set(["a", "b"]), options);
    const without = arrangeBoxes(boxes, [["a", "b"]], new Set(["a", "b"]), options);
    expect(withOutside).toEqual(without);
  });

  test("with the block clear of everything it stays at the corner it had", () => {
    const apart = [box("a", 1000, 1000), box("b", 1000, 1000), box("x", 0, 0)];
    const placed = apply(apart, arrangeBoxes(apart, [["a", "b"]], new Set(["a", "b"]), options));
    expect(at(placed, "a")).toMatchObject({ x: 1000, y: 1000 });
    expect(at(placed, "x")).toEqual(apart[2]);
  });
});

describe("clear places", () => {
  const rects: Rect[] = [{ x: 0, y: 0, width: 100, height: 100 }];
  test("a free spot is kept", () => {
    expect(clearOrigin(rects, { x: 500, y: 500 }, [{ x: 0, y: 0, width: 100, height: 100 }], 10)).toEqual({ x: 500, y: 500 });
  });
  test("a taken spot moves to the nearest side of the obstacle, with the gap", () => {
    const moved = clearOrigin(rects, { x: 90, y: 40 }, [{ x: 0, y: 0, width: 100, height: 100 }], 10);
    expect(moved).toEqual({ x: 110, y: 40 });
  });
  test("a wall of obstacles leaves the far side", () => {
    const wall: Rect[] = [{ x: 0, y: 0, width: 100, height: 100 }, { x: 110, y: 0, width: 100, height: 100 }, { x: 0, y: 110, width: 210, height: 100 }];
    const moved = clearOrigin(rects, { x: 5, y: 5 }, wall, 10);
    const placed = { ...rects[0], x: moved.x, y: moved.y };
    expect(wall.some((o) => overlaps(placed, { x: o.x - 10, y: o.y - 10, width: o.width + 20, height: o.height + 20 }))).toBe(false);
  });
});

describe("place a new node", () => {
  const main = () => sample().workflows[0];
  const config = { fallback: { width: 250, height: 100 }, gap: 40 };

  test("where it is wanted when the room is free", () => {
    expect(placeNode(main(), { x: 1000, y: 1000 }, { width: 250, height: 100 }, undefined, config)).toEqual({ x: 1000, y: 1000 });
  });

  test("beside the node it would land on, using its measured size", () => {
    const workflow = main();
    const sizes = new Map([["a", { width: 250, height: 400 }]]);
    const spot = placeNode(workflow, { x: 110, y: 300 }, { width: 250, height: 100 }, sizes, config);
    const wanted = { ...spot, width: 250, height: 100 };
    for (const node of workflow.nodes) {
      const own = footprint(sizes, node.id, config.fallback);
      expect(overlaps(wanted, { ...node.position, ...own }), node.id).toBe(false);
    }
  });

  test("a drop never overlaps and moves no other node", () => {
    const workflow = main();
    const before = JSON.stringify(workflow.nodes.map((node) => node.position));
    const spot = placeNode(workflow, { x: 100, y: 50 }, { width: 250, height: 100 }, undefined, config);
    expect(spot).not.toEqual({ x: 100, y: 50 });
    expect(JSON.stringify(workflow.nodes.map((node) => node.position))).toBe(before);
  });

  test("snaps to the grid", () => {
    const spot = placeNode(main(), { x: 1003, y: 1011 }, { width: 250, height: 100 }, undefined, { ...config, grid: 16 });
    expect(spot).toEqual({ x: 1008, y: 1008 });
  });
});

describe("arrange a profile", () => {
  const io = [port("i", "input"), port("o", "output")];
  const wired = (): Profile => {
    const profile = sample();
    const main = profile.workflows[0];
    main.nodes = [
      fixtureNode("a", io, { position: { x: 400, y: 400 }, config: { greeting: "hi" } }),
      fixtureNode("b", io, { position: { x: 10, y: 10 } }),
      fixtureNode("c", io, { position: { x: 700, y: 20 } }),
    ];
    main.wires = [
      { id: "ab", kind: "then", from: { node_id: "a", port_id: "o" }, to: { node_id: "b", port_id: "i" } },
      { id: "bc", kind: "uses", from: { node_id: "b", port_id: "o" }, to: { node_id: "c", port_id: "i" } },
      { id: "in", kind: "then", from: { node_id: "self", port_id: "ask" }, to: { node_id: "a", port_id: "i" } },
    ];
    return profile;
  };

  test("only positions change: wires, config and the other workflow are the same objects", () => {
    const profile = wired();
    const main = profile.workflows[0];
    const { profile: next, moves } = arrangeWorkflow(profile, "main", null, undefined, options);
    expect(moves.length).toBeGreaterThan(0);
    const after = next.workflows[0];
    expect(after.wires).toBe(main.wires);
    expect(after.ports).toBe(main.ports);
    expect(next.workflows[1]).toBe(profile.workflows[1]);
    after.nodes.forEach((node, at) => { expect({ ...node, position: 0 }).toEqual({ ...main.nodes[at], position: 0 }); });
    expect(after.nodes.map((node) => node.id)).toEqual(["a", "b", "c"]);
    expect(after.nodes[0].config).toBe(main.nodes[0].config);
  });

  test("wires to the workflow's own ports do not shape the layout", () => {
    expect(layoutWires(wired().workflows[0])).toEqual([["a", "b"], ["b", "c"]]);
  });

  test("following the wires: a, then b, then c", () => {
    const { profile } = arrangeWorkflow(wired(), "main", null, undefined, options);
    const [a, b, c] = profile.workflows[0].nodes;
    expect(a.position.x).toBeLessThan(b.position.x);
    expect(b.position.x).toBeLessThan(c.position.x);
  });

  test("a selection leaves the other nodes where they were", () => {
    const profile = wired();
    const { profile: next, moves } = arrangeWorkflow(profile, "main", ["a", "b"], undefined, options);
    expect(moves.every((move) => move.id !== "c")).toBe(true);
    expect(next.workflows[0].nodes[2]).toBe(profile.workflows[0].nodes[2]);
  });

  test("fewer than two nodes, a missing workflow or an empty selection change nothing", () => {
    const profile = wired();
    expect(arrangeWorkflow(profile, "main", ["a"], undefined, options)).toEqual({ profile, moves: [] });
    expect(arrangeWorkflow(profile, "main", ["a", "ghost"], undefined, options).profile).toBe(profile);
    expect(arrangeWorkflow(profile, "nope", null, undefined, options).profile).toBe(profile);
    expect(arrangeWorkflow(profile, "inner", null, undefined, options).profile).toBe(profile);
  });

  test("measured sizes from the canvas take part", () => {
    const sizes = measuredSizes([{ id: "n:a", measured: { width: 600, height: 100 } }, { id: "self:in", measured: { width: 9, height: 9 } }, { id: "n:b" }]);
    expect([...sizes.keys()]).toEqual(["a"]);
    const { profile } = arrangeWorkflow(wired(), "main", null, sizes, options);
    const [a, b] = profile.workflows[0].nodes;
    expect(b.position.x - a.position.x).toBe(600 + options.layerGap);
  });
});
