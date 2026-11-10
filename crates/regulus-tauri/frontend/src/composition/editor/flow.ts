// React Flow nodes and edges derived from the edited profile. Nothing here is kept: each
// render draws the document again, so there is a single copy of it.
import { MarkerType, type Edge, type Node } from "@xyflow/react";
import type { CompositionNode, Diagnostic, ModuleDefinition, Port, Profile, Workflow } from "../model";
import { SELF, portsOf, workflowOf, type End, type Point, type Size } from "./document";
import { moduleStateOf, type PieceState } from "./palette";
import type { Selection } from "./store";

export type Severity = Diagnostic["severity"];
export type PieceData = { item: CompositionNode; ports: Port[]; workflowId: string; state: PieceState; severity: Severity | null; editable: boolean };
export type SelfData = { ports: Port[]; side: "in" | "out"; severity: Severity | null };
export type FrameData = { label: string };
export type PieceNode = Node<PieceData, "piece">;
export type SelfNode = Node<SelfData, "self">;
export type FrameNode = Node<FrameData, "workflow">;
export type FlowNode = PieceNode | SelfNode | FrameNode;

export const SELF_IN = "self:in";
export const SELF_OUT = "self:out";
const NODE_PREFIX = "n:";
const WIRE_PREFIX = "w:";

export const flowNodeId = (nodeId: string) => `${NODE_PREFIX}${nodeId}`;
export const flowEdgeId = (wireId: string) => `${WIRE_PREFIX}${wireId}`;
/** The node id a canvas node stands for in the edited workflow; `null` for the workflow's own ports and the unified view. */
export const nodeIdOf = (flowId: string) => flowId.startsWith(NODE_PREFIX) ? flowId.slice(NODE_PREFIX.length) : null;
export const wireIdOf = (flowId: string) => flowId.startsWith(WIRE_PREFIX) ? flowId.slice(WIRE_PREFIX.length) : null;

/** A wire end from a canvas node and handle. */
export function endOf(flowId: string | null | undefined, handle: string | null | undefined): End | null {
  if (!flowId || !handle) return null;
  if (flowId === SELF_IN || flowId === SELF_OUT) return { node_id: SELF, port_id: handle };
  const nodeId = nodeIdOf(flowId);
  return nodeId === null ? null : { node_id: nodeId, port_id: handle };
}

/** The size React Flow measured for each node of the edited workflow, by document node id; nodes not measured yet are left out. */
export function measuredSizes(nodes: readonly { id: string; measured?: { width?: number; height?: number } }[]): Map<string, Size> {
  const sizes = new Map<string, Size>();
  for (const { id: flowId, measured } of nodes) {
    const id = nodeIdOf(flowId);
    if (id !== null && measured?.width && measured.height) sizes.set(id, { width: measured.width, height: measured.height });
  }
  return sizes;
}

/** Where the workflow's own inputs and outputs sit: left and right of its nodes. */
export function selfAnchors(workflow: Workflow, gap: number): { in: Point; out: Point } {
  if (!workflow.nodes.length) return { in: { x: 0, y: 0 }, out: { x: gap * 2, y: 0 } };
  const xs = workflow.nodes.map((node) => node.position.x);
  const y = Math.min(...workflow.nodes.map((node) => node.position.y));
  return { in: { x: Math.min(...xs) - gap, y }, out: { x: Math.max(...xs) + gap, y } };
}

const worst = (found: Diagnostic[]): Severity | null =>
  found.some((d) => d.severity === "error") ? "error" : found.some((d) => d.severity === "warning") ? "warning" : found.length ? "info" : null;

/** Diagnostics that name a node of this workflow (or of no workflow in particular). */
export function severityOf(diagnostics: Diagnostic[], workflowId: string, nodeId: string): Severity | null {
  return worst(diagnostics.filter((d) => d.node_id === nodeId && (d.workflow_id === null || d.workflow_id === workflowId)));
}

export interface FlowOptions {
  modules: ModuleDefinition[];
  diagnostics: Diagnostic[];
  selection: Selection;
  measured: Map<string, { width: number; height: number }>;
  anchors: { in: Point; out: Point } | null;
  nodeWidth: number;
  labels: { uses: string; then: string };
  /** Positions of nodes being dragged right now (React Flow owns them until the drop). */
  dragging?: Map<string, Point>;
  /** Last `data` per canvas node id: reused while its inputs are the same, so `memo` holds. */
  cache?: Map<string, PieceData>;
}

const sameData = (a: PieceData, b: PieceData) => a.item === b.item && a.ports === b.ports && a.workflowId === b.workflowId &&
  a.state === b.state && a.severity === b.severity && a.editable === b.editable;

/** `data` for a node, the cached object if nothing it shows changed. */
function pieceData(id: string, next: PieceData, cache: Map<string, PieceData> | undefined): PieceData {
  const known = cache?.get(id);
  if (known && sameData(known, next)) return known;
  cache?.set(id, next);
  return next;
}

/** Position changes split: those mid-drag (kept on the canvas) and those to write to the document (a drop, a key press). */
export function positionChanges(changes: { type: string; id?: string; position?: Point; dragging?: boolean }[]): { live: { id: string; position: Point }[]; done: { id: string; position: Point }[] } {
  const live: { id: string; position: Point }[] = [];
  const done: { id: string; position: Point }[] = [];
  for (const change of changes) {
    if (change.type !== "position" || !change.position || !change.id) continue;
    const id = nodeIdOf(change.id);
    if (id === null) continue;
    (change.dragging ? live : done).push({ id, position: { x: change.position.x, y: change.position.y } });
  }
  return { live, done };
}

function edgeOf(id: string, source: string, target: string, wire: Workflow["wires"][number], options: FlowOptions, extra: Partial<Edge> = {}): Edge {
  const flagged = options.diagnostics.some((d) => d.wire_id === wire.id);
  return {
    id, source, target, sourceHandle: wire.from.port_id, targetHandle: wire.to.port_id, type: "smoothstep",
    label: wire.kind === "uses" ? options.labels.uses : options.labels.then,
    className: `wire-${wire.kind}${flagged ? " is-flagged" : ""}`,
    markerEnd: { type: MarkerType.ArrowClosed, color: "var(--rg-color-focus)" },
    ...extra,
  };
}

const withSize = (id: string, options: FlowOptions) => {
  const measured = options.measured.get(id);
  return measured ? { measured } : {};
};

/** One workflow, editable: its nodes, its wires and, if it has ports, its own inputs and outputs. */
export function buildWorkflowFlow(profile: Profile, workflowId: string, options: FlowOptions): { nodes: FlowNode[]; edges: Edge[] } {
  const workflow = workflowOf(profile, workflowId);
  if (!workflow) return { nodes: [], edges: [] };
  const nodes: FlowNode[] = [];
  const selected = new Set(options.selection.nodes);
  for (const item of workflow.nodes) {
    const id = flowNodeId(item.id);
    nodes.push({
      id, type: "piece", position: options.dragging?.get(item.id) ?? item.position, selected: selected.has(item.id), ariaLabel: item.name || item.id,
      style: { width: options.nodeWidth },
      data: pieceData(id, { item, ports: portsOf(profile, item), workflowId, state: moduleStateOf(options.modules, item.module_id), severity: severityOf(options.diagnostics, workflowId, item.id), editable: true }, options.cache),
      ...withSize(id, options),
    });
  }
  const own = workflow.ports ?? [];
  const anchors = options.anchors ?? selfAnchors(workflow, options.nodeWidth);
  const selfSeverity = worst(options.diagnostics.filter((d) => d.node_id === SELF && d.workflow_id === workflowId));
  for (const side of ["in", "out"] as const) {
    const ports = own.filter((port) => port.direction === (side === "in" ? "input" : "output"));
    if (!ports.length) continue;
    const id = side === "in" ? SELF_IN : SELF_OUT;
    nodes.push({ id, type: "self", position: anchors[side], draggable: false, deletable: false, style: { width: options.nodeWidth }, data: { ports, side, severity: selfSeverity }, ...withSize(id, options) });
  }
  const flowEnd = (end: End, side: "in" | "out") => end.node_id === SELF ? side === "in" ? SELF_IN : SELF_OUT : flowNodeId(end.node_id);
  const wires = new Set(options.selection.wires);
  const edges = workflow.wires.map((wire) => edgeOf(flowEdgeId(wire.id), flowEnd(wire.from, "in"), flowEnd(wire.to, "out"), wire, options, { selected: wires.has(wire.id) }));
  return { nodes, edges };
}

/** Every workflow inside its frame, to look at; editing happens in a workflow's own tab. */
export function buildUnifiedFlow(profile: Profile, options: FlowOptions): { nodes: FlowNode[]; edges: Edge[] } {
  const nodes: FlowNode[] = [];
  const edges: Edge[] = [];
  for (const workflow of profile.workflows) {
    const frame = `a:${workflow.id}`;
    nodes.push({
      id: frame, type: "workflow", data: { label: workflow.name || workflow.id },
      position: { x: workflow.layout.x, y: workflow.layout.y }, style: { width: workflow.layout.width, height: workflow.layout.height },
      selectable: false, draggable: false, focusable: false,
    });
    for (const item of workflow.nodes) {
      const id = `${frame}:${item.id}`;
      nodes.push({
        id, type: "piece", position: item.position, parentId: frame, draggable: false, ariaLabel: item.name || item.id, style: { width: options.nodeWidth },
        data: { item, ports: portsOf(profile, item), workflowId: workflow.id, state: moduleStateOf(options.modules, item.module_id), severity: severityOf(options.diagnostics, workflow.id, item.id), editable: false },
        ...withSize(id, options),
      });
    }
    for (const wire of workflow.wires) {
      if (wire.from.node_id === SELF || wire.to.node_id === SELF) continue;
      edges.push(edgeOf(`${frame}:${wire.id}`, `${frame}:${wire.from.node_id}`, `${frame}:${wire.to.node_id}`, wire, options, { selectable: false, focusable: false }));
    }
  }
  return { nodes, edges };
}
