// Automatic layout of a workflow's nodes, as pure functions. Nothing here moves a node by
// itself: positions change only when the user asks to arrange, and the result is a list of
// moves the editor store applies (and can take back).
//
// The method is a small layered layout: wires fix the order (a cycle loses the wire that
// closes it), each node goes one rank after its latest source, ranks are ordered by the
// average place of their neighbours, and groups of nodes with no wire between them are
// stacked one after another. Everything is decided by the order of the nodes in the document,
// so the same workflow always gives the same layout.
import type { Profile, Workflow } from "../model";
import { SELF, moveNodes, nodeOf, workflowOf, type Point, type Size } from "./document";

/** Which way wires run: ranks go left to right, or top to bottom. */
export type LayoutDirection = "right" | "down";
export const LAYOUT_DIRECTIONS: readonly LayoutDirection[] = ["right", "down"];

export interface LayoutOptions {
  direction: LayoutDirection;
  /** Space between one rank and the next, in pixels. */
  layerGap: number;
  /** Space between nodes of the same rank and between unconnected groups, in pixels. */
  nodeGap: number;
  /** The footprint of a node nobody has measured yet. */
  fallback: Size;
  /** With a grid, every position and spacing is a multiple of it. */
  grid?: number;
}

export type Rect = { x: number; y: number; width: number; height: number };
/** A node as the layout sees it: where it is and the room it takes. */
export type Box = Rect & { id: string };
export type Move = { id: string; from: Point; to: Point };

/** The measured size of a node when it has one, the fallback otherwise. */
export function footprint(measured: ReadonlyMap<string, Size> | undefined, id: string, fallback: Size): Size {
  const own = measured?.get(id);
  return {
    width: own && own.width > 0 ? own.width : fallback.width,
    height: own && own.height > 0 ? own.height : fallback.height,
  };
}

const roundUp = (value: number, grid?: number) => grid && grid > 0 ? Math.ceil(value / grid) * grid : Math.ceil(value);
const roundDown = (value: number, grid?: number) => grid && grid > 0 ? Math.floor(value / grid) * grid : Math.floor(value);
const roundNear = (value: number, grid?: number) => grid && grid > 0 ? Math.round(value / grid) * grid : Math.round(value);

/** Whether `a` and `b` come closer than `gap` to each other (touching at exactly `gap` is fine). */
function crowds(a: Rect, b: Rect, gap: number): boolean {
  return a.x < b.x + b.width + gap && b.x < a.x + a.width + gap && a.y < b.y + b.height + gap && b.y < a.y + a.height + gap;
}

/**
 * Where to put a block of `rects` (relative to its origin) so none of them comes closer than
 * `gap` to an obstacle. `origin` itself when it is already clear; otherwise the nearest of the
 * places that put the block against the side of an obstacle, and, if none is free, to the
 * right of everything. Only the block moves, as a whole.
 */
export function clearOrigin(rects: readonly Rect[], origin: Point, obstacles: readonly Rect[], gap: number, grid?: number): Point {
  if (!rects.length || !obstacles.length) return origin;
  const at = (point: Point) => rects.map((rect) => ({ ...rect, x: point.x + rect.x, y: point.y + rect.y }));
  const free = (point: Point) => { const placed = at(point); return !obstacles.some((obstacle) => placed.some((rect) => crowds(rect, obstacle, gap))); };
  if (free(origin)) return origin;
  const left = Math.min(...rects.map((rect) => rect.x));
  const top = Math.min(...rects.map((rect) => rect.y));
  const right = Math.max(...rects.map((rect) => rect.x + rect.width));
  const bottom = Math.max(...rects.map((rect) => rect.y + rect.height));
  const candidates: Point[] = [];
  for (const o of obstacles) {
    candidates.push(
      { x: roundUp(o.x + o.width + gap - left, grid), y: origin.y },
      { x: roundDown(o.x - gap - right, grid), y: origin.y },
      { x: origin.x, y: roundUp(o.y + o.height + gap - top, grid) },
      { x: origin.x, y: roundDown(o.y - gap - bottom, grid) },
    );
  }
  let best: Point | null = null;
  let bestCost = Infinity;
  for (const candidate of candidates) {
    const cost = (candidate.x - origin.x) ** 2 + (candidate.y - origin.y) ** 2;
    if (cost < bestCost && free(candidate)) { best = candidate; bestCost = cost; }
  }
  if (best) return best;
  const farRight = Math.max(...obstacles.map((o) => o.x + o.width));
  return { x: roundUp(farRight + gap - left, grid), y: origin.y };
}

/**
 * Where a new node of size `size` goes when it is wanted at `point`: there if the room is free,
 * otherwise the nearest free place beside the nodes it would touch. No other node moves.
 */
export function placeNode(workflow: Workflow, point: Point, size: Size, measured: ReadonlyMap<string, Size> | undefined, options: Pick<LayoutOptions, "fallback" | "grid"> & { gap: number }): Point {
  const obstacles: Rect[] = workflow.nodes.map((node) => ({ ...node.position, ...footprint(measured, node.id, options.fallback) }));
  const start = { x: roundNear(point.x, options.grid), y: roundNear(point.y, options.grid) };
  return clearOrigin([{ x: 0, y: 0, ...size }], start, obstacles, options.gap, options.grid);
}

/** One group of connected nodes laid out on its own: positions relative to its corner, and its extent. */
function layoutGroup(members: number[], edges: Map<number, number[]>, mainSize: number[], crossSize: number[], layerGap: number, nodeGap: number, grid?: number): { placed: Map<number, [number, number]>; main: number; cross: number } {
  const inGroup = new Set(members);
  const out = (node: number) => (edges.get(node) ?? []).filter((next) => inGroup.has(next));
  const indegree = new Map(members.map((node) => [node, 0]));
  for (const node of members) for (const next of out(node)) indegree.set(next, (indegree.get(next) ?? 0) + 1);

  // Cycle breaking: a depth-first walk from the nodes nothing feeds (then any left), in document
  // order. A wire that returns to a node still on the walk closes a cycle and is not followed.
  const state = new Map<number, 1 | 2>();
  const finished: number[] = [];
  const kept = new Map<number, number[]>(members.map((node) => [node, []]));
  const walk = (node: number) => {
    state.set(node, 1);
    for (const next of out(node)) {
      if (state.get(next) === 1) continue;
      kept.get(node)!.push(next);
      if (!state.has(next)) walk(next);
    }
    state.set(node, 2);
    finished.push(node);
  };
  for (const node of members) if (indegree.get(node) === 0 && !state.has(node)) walk(node);
  for (const node of members) if (!state.has(node)) walk(node);

  // Ranks: a node sits one after the latest node that feeds it. Reverse finishing order is a
  // topological order of the wires that were kept.
  const rank = new Map(members.map((node) => [node, 0]));
  for (const node of [...finished].reverse()) for (const next of kept.get(node)!) rank.set(next, Math.max(rank.get(next)!, rank.get(node)! + 1));
  const ranks: number[][] = [];
  for (const node of members) (ranks[rank.get(node)!] ??= []).push(node);

  // Order inside each rank: sweep by the average place of the neighbours in the rank before,
  // then after, a few times. Ties keep the order they had, so nothing shuffles for no reason.
  const before = new Map<number, number[]>(members.map((node) => [node, []]));
  for (const node of members) for (const next of kept.get(node)!) before.get(next)!.push(node);
  const slot = new Map<number, number>();
  const renumber = () => ranks.forEach((layer) => layer.forEach((node, at) => slot.set(node, at)));
  renumber();
  const sweep = (layer: number[], neighbours: (node: number) => number[]) => {
    const key = new Map(layer.map((node) => {
      const near = neighbours(node);
      return [node, near.length ? near.reduce((sum, other) => sum + slot.get(other)!, 0) / near.length : slot.get(node)!] as const;
    }));
    layer.sort((a, b) => key.get(a)! - key.get(b)! || slot.get(a)! - slot.get(b)! || a - b);
    renumber();
  };
  for (let round = 0; round < 4; round++) {
    for (let at = 1; at < ranks.length; at++) sweep(ranks[at], (node) => before.get(node)!);
    for (let at = ranks.length - 2; at >= 0; at--) sweep(ranks[at], (node) => kept.get(node)!);
  }

  // Coordinates: ranks one after another along the main axis; inside a rank nodes one after
  // another along the cross axis, each rank centred on the tallest one.
  const extents = ranks.map((layer) => layer.reduce((sum, node, at) => sum + crossSize[node] + (at ? nodeGap : 0), 0));
  const cross = Math.max(0, ...extents);
  const placed = new Map<number, [number, number]>();
  let main = 0;
  ranks.forEach((layer, at) => {
    let along = roundDown((cross - extents[at]) / 2, grid);
    for (const node of layer) { placed.set(node, [main, along]); along += crossSize[node] + nodeGap; }
    main += Math.max(...layer.map((node) => mainSize[node])) + layerGap;
  });
  return { placed, main: Math.max(0, main - layerGap), cross };
}

/**
 * New positions for `boxes`. With `movable`, only those nodes are laid out and every other box
 * stays exactly where it is (the block is moved aside as a whole if it would land on one).
 * Wires that do not join two nodes being laid out do not count. The block keeps the top-left
 * corner it had. Only nodes whose position changes are returned.
 */
export function arrangeBoxes(boxes: readonly Box[], wires: readonly (readonly [string, string])[], movable: ReadonlySet<string> | null, options: LayoutOptions): Move[] {
  const { grid, direction } = options;
  const layerGap = roundUp(options.layerGap, grid);
  const nodeGap = roundUp(options.nodeGap, grid);
  const picked = boxes.filter((box) => !movable || movable.has(box.id));
  if (!picked.length) return [];
  const fixed = boxes.filter((box) => movable && !movable.has(box.id));
  const index = new Map(picked.map((box, at) => [box.id, at]));
  const horizontal = direction === "right";
  const mainSize = picked.map((box) => roundUp(horizontal ? box.width : box.height, grid));
  const crossSize = picked.map((box) => roundUp(horizontal ? box.height : box.width, grid));

  const edges = new Map<number, number[]>();
  const seen = new Set<string>();
  const sides = new Map<number, Set<number>>(picked.map((_, at) => [at, new Set<number>()]));
  for (const [from, to] of wires) {
    const a = index.get(from);
    const b = index.get(to);
    if (a === undefined || b === undefined || a === b || seen.has(`${a}>${b}`)) continue;
    seen.add(`${a}>${b}`);
    (edges.get(a) ?? edges.set(a, []).get(a)!).push(b);
    sides.get(a)!.add(b);
    sides.get(b)!.add(a);
  }
  for (const next of edges.values()) next.sort((x, y) => x - y);

  // Groups of nodes joined by wires (either way), each starting at its first node in document order.
  const groupOf = new Map<number, number>();
  const groups: number[][] = [];
  picked.forEach((_, start) => {
    if (groupOf.has(start)) return;
    const members: number[] = [];
    const queue = [start];
    groupOf.set(start, groups.length);
    for (let head = 0; head < queue.length; head++) {
      members.push(queue[head]);
      for (const next of [...sides.get(queue[head])!].sort((x, y) => x - y)) if (!groupOf.has(next)) { groupOf.set(next, groups.length); queue.push(next); }
    }
    groups.push(members.sort((x, y) => x - y));
  });

  const relative = new Map<number, Point>();
  let along = 0;
  for (const members of groups) {
    const laid = layoutGroup(members, edges, mainSize, crossSize, layerGap, nodeGap, grid);
    for (const [node, [main, cross]] of laid.placed) {
      const shift = along + cross;
      relative.set(node, horizontal ? { x: main, y: shift } : { x: shift, y: main });
    }
    along += laid.cross + nodeGap;
  }

  const origin = {
    x: roundNear(Math.min(...picked.map((box) => box.x)), grid),
    y: roundNear(Math.min(...picked.map((box) => box.y)), grid),
  };
  const block: Rect[] = picked.map((box, at) => ({ ...relative.get(at)!, width: roundUp(box.width, grid), height: roundUp(box.height, grid) }));
  const clear = clearOrigin(block, origin, fixed, nodeGap, grid);
  const moves: Move[] = [];
  picked.forEach((box, at) => {
    const to = { x: clear.x + relative.get(at)!.x, y: clear.y + relative.get(at)!.y };
    if (to.x !== box.x || to.y !== box.y) moves.push({ id: box.id, from: { x: box.x, y: box.y }, to });
  });
  return moves;
}

/** The wires of `workflow` between two of its nodes: the ones that shape a layout. */
export function layoutWires(workflow: Workflow): [string, string][] {
  return workflow.wires.filter((wire) => wire.from.node_id !== SELF && wire.to.node_id !== SELF
    && nodeOf(workflow, wire.from.node_id) && nodeOf(workflow, wire.to.node_id)).map((wire) => [wire.from.node_id, wire.to.node_id]);
}

/**
 * Arranges a workflow, or only the nodes `only` names (the rest stay where they are). Only
 * positions change: wires, config and everything else of the profile are the same objects.
 * Fewer than two nodes to arrange is no change.
 */
export function arrangeWorkflow(profile: Profile, workflowId: string, only: readonly string[] | null, measured: ReadonlyMap<string, Size> | undefined, options: LayoutOptions): { profile: Profile; moves: Move[] } {
  const workflow = workflowOf(profile, workflowId);
  if (!workflow) return { profile, moves: [] };
  const movable = only ? new Set(only.filter((id) => nodeOf(workflow, id))) : null;
  if ((movable ? movable.size : workflow.nodes.length) < 2) return { profile, moves: [] };
  const boxes: Box[] = workflow.nodes.map((node) => ({ id: node.id, ...node.position, ...footprint(measured, node.id, options.fallback) }));
  const moves = arrangeBoxes(boxes, layoutWires(workflow), movable, options);
  return moves.length ? { profile: moveNodes(profile, workflowId, moves.map(({ id, to }) => ({ id, position: to }))), moves } : { profile, moves };
}
