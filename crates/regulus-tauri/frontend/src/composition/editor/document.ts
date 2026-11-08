// The edited profile as data: every change is a pure function from one profile to the
// next. The editor store keeps the result; React Flow only draws it.
import type { CompositionNode, ConfigValue, Port, Profile, Wire, Workflow } from "../model";

/** The node id inner wires use for their workflow's own ports (`regulus_proto::WORKFLOW_SELF`). */
export const SELF = "self";
/** Joins a box to the nodes inside it at run time: ids may not contain it. */
export const BOX_SEPARATOR = "/";

export type End = { node_id: string; port_id: string };
export type Point = { x: number; y: number };

/** A module id without its version: `regulus/hello@0.1.0` → `regulus/hello`. */
export const baseModuleId = (id: string) => id.split("@")[0];

/** A name turned into an id the core accepts as a file or node name. */
export function slugify(text: string): string {
  return text.trim().toLowerCase().normalize("NFKD").replace(/[̀-ͯ]/g, "")
    .replace(/[^a-z0-9._-]+/g, "-").replace(/^[-.]+|-+$/g, "");
}

/** `base`, or `base-2`, `base-3`… whichever is free; never `self` or empty. */
export function uniqueId(base: string, taken: Iterable<string>, fallback = "node"): string {
  const used = new Set(taken);
  const root = slugify(base) || fallback;
  const free = (id: string) => !used.has(id) && id !== SELF;
  if (free(root)) return root;
  for (let n = 2; ; n++) if (free(`${root}-${n}`)) return `${root}-${n}`;
}

/** Why an id cannot be used for a new profile, or `null`. */
export function profileIdProblem(id: string, taken: string[]): "empty" | "invalid" | "taken" | null {
  if (!id) return "empty";
  if (slugify(id) !== id) return "invalid";
  return taken.includes(id) ? "taken" : null;
}

export const workflowOf = (profile: Profile, id: string) => profile.workflows.find((workflow) => workflow.id === id);
export const nodeOf = (workflow: Workflow, id: string) => workflow.nodes.find((node) => node.id === id);

/** A node with no module and an internal workflow runs that workflow in line. */
export const isBox = (node: CompositionNode) => !node.module_id && !!node.internal_workflow_id;

/** The ports a node offers: its own, or, for a box that declares none, its workflow's (`Profile::ports_of`). */
export function portsOf(profile: Profile, node: CompositionNode): Port[] {
  if (node.ports.length || !isBox(node)) return node.ports;
  return workflowOf(profile, node.internal_workflow_id!)?.ports ?? node.ports;
}

const emptyWorkflow = (id: string, name: string, kind: Workflow["kind"]): Workflow =>
  ({ id, name, kind, module_id: null, nodes: [], wires: [], layout: { x: 0, y: 0, width: 0, height: 0 } });

/** A new profile: one empty main workflow. */
export function newProfile(id: string, name: string, mainName: string): Profile {
  const main = emptyWorkflow("main", mainName, "main");
  return { id, name, project_id: null, extends: null, main_workflow_id: main.id, workflows: [main] };
}

function updateWorkflow(profile: Profile, id: string, change: (workflow: Workflow) => Workflow): Profile {
  if (!workflowOf(profile, id)) return profile;
  return { ...profile, workflows: profile.workflows.map((workflow) => workflow.id === id ? change(workflow) : workflow) };
}

function updateNode(profile: Profile, workflowId: string, nodeId: string, change: (node: CompositionNode) => CompositionNode): Profile {
  return updateWorkflow(profile, workflowId, (workflow) => nodeOf(workflow, nodeId)
    ? { ...workflow, nodes: workflow.nodes.map((node) => node.id === nodeId ? change(node) : node) }
    : workflow);
}

export function addNode(profile: Profile, workflowId: string, node: CompositionNode): Profile {
  return updateWorkflow(profile, workflowId, (workflow) => nodeOf(workflow, node.id) ? workflow : { ...workflow, nodes: [...workflow.nodes, node] });
}

export function moveNodes(profile: Profile, workflowId: string, moves: { id: string; position: Point }[]): Profile {
  const to = new Map(moves.map(({ id, position }) => [id, position]));
  return updateWorkflow(profile, workflowId, (workflow) => ({
    ...workflow,
    nodes: workflow.nodes.map((node) => {
      const position = to.get(node.id);
      return position && (position.x !== node.position.x || position.y !== node.position.y) ? { ...node, position: { x: position.x, y: position.y } } : node;
    }),
  }));
}

/** Removes the nodes and every wire that touches them. */
export function removeNodes(profile: Profile, workflowId: string, ids: string[]): Profile {
  const gone = new Set(ids);
  return updateWorkflow(profile, workflowId, (workflow) => ({
    ...workflow,
    nodes: workflow.nodes.filter((node) => !gone.has(node.id)),
    wires: workflow.wires.filter((wire) => !gone.has(wire.from.node_id) && !gone.has(wire.to.node_id)),
  }));
}

export function removeWires(profile: Profile, workflowId: string, ids: string[]): Profile {
  const gone = new Set(ids);
  return updateWorkflow(profile, workflowId, (workflow) => ({ ...workflow, wires: workflow.wires.filter((wire) => !gone.has(wire.id)) }));
}

/** Adds a wire with a fresh id. Whether it may exist is `canConnect`'s question. */
export function addWire(profile: Profile, workflowId: string, kind: Wire["kind"], from: End, to: End): Profile {
  return updateWorkflow(profile, workflowId, (workflow) => {
    const id = uniqueId(`${from.node_id}-${from.port_id}-${to.node_id}-${to.port_id}`, workflow.wires.map((wire) => wire.id), "wire");
    return { ...workflow, wires: [...workflow.wires, { id, kind, from: { ...from }, to: { ...to } }] };
  });
}

export function renameNode(profile: Profile, workflowId: string, nodeId: string, name: string): Profile {
  return updateNode(profile, workflowId, nodeId, (node) => ({ ...node, name }));
}

/** Sets one config value; `undefined` removes the key, so the module's default applies. */
export function setConfig(profile: Profile, workflowId: string, nodeId: string, key: string, value: ConfigValue | undefined): Profile {
  return updateNode(profile, workflowId, nodeId, (node) => {
    const config = { ...node.config };
    if (value === undefined) delete config[key]; else config[key] = value;
    return { ...node, config };
  });
}

export function setInternalWorkflow(profile: Profile, workflowId: string, nodeId: string, internal: string | null): Profile {
  return updateNode(profile, workflowId, nodeId, (node) => ({ ...node, internal_workflow_id: internal || null }));
}

/** A new extra workflow; returns the profile and the id it got. */
export function addWorkflow(profile: Profile, name: string): { profile: Profile; id: string } {
  const id = uniqueId(name, profile.workflows.map((workflow) => workflow.id), "workflow");
  return { profile: { ...profile, workflows: [...profile.workflows, emptyWorkflow(id, name, "extra")] }, id };
}

export function renameWorkflow(profile: Profile, id: string, name: string): Profile {
  return updateWorkflow(profile, id, (workflow) => ({ ...workflow, name }));
}

/** The box nodes that place workflow `id` elsewhere in the profile. */
export function boxesOf(profile: Profile, id: string): { workflowId: string; nodeId: string }[] {
  return profile.workflows.filter((workflow) => workflow.id !== id).flatMap((workflow) => workflow.nodes
    .filter((node) => isBox(node) && node.internal_workflow_id === id)
    .map((node) => ({ workflowId: workflow.id, nodeId: node.id })));
}

/**
 * Removes a workflow other than the main one. The boxes that placed it go too, with their
 * wires: without the workflow they would have neither module nor ports. A module node that
 * named it as its inner workflow only stops pointing at it (its ports are its own).
 */
export function removeWorkflow(profile: Profile, id: string): Profile {
  if (id === profile.main_workflow_id || !workflowOf(profile, id)) return profile;
  let result: Profile = { ...profile, workflows: profile.workflows.filter((workflow) => workflow.id !== id) };
  for (const workflow of result.workflows) {
    const boxes = workflow.nodes.filter((node) => isBox(node) && node.internal_workflow_id === id).map((node) => node.id);
    if (boxes.length) result = removeNodes(result, workflow.id, boxes);
  }
  return {
    ...result,
    workflows: result.workflows.map((workflow) => workflow.nodes.some((node) => node.internal_workflow_id === id)
      ? { ...workflow, nodes: workflow.nodes.map((node) => node.internal_workflow_id === id ? { ...node, internal_workflow_id: null } : node) }
      : workflow),
  };
}

/** Rewrites, or with `next = null` drops, the wires that reach port `portId` of workflow `id`: its own `self` wires and those to the boxes that place it elsewhere. */
function rewireBoundary(profile: Profile, id: string, portId: string, next: string | null): Profile {
  const fix = (end: End, owner: Workflow) => {
    const reaches = end.port_id === portId && (owner.id === id
      ? end.node_id === SELF
      : owner.nodes.some((node) => node.id === end.node_id && isBox(node) && !node.ports.length && node.internal_workflow_id === id));
    return reaches ? next === null ? null : { ...end, port_id: next } : end;
  };
  return {
    ...profile,
    workflows: profile.workflows.map((workflow) => ({
      ...workflow,
      wires: workflow.wires.flatMap((wire) => {
        const from = fix(wire.from, workflow);
        const to = fix(wire.to, workflow);
        return from && to ? [from === wire.from && to === wire.to ? wire : { ...wire, from, to }] : [];
      }),
    })),
  };
}

/** Only "then" values cross a workflow's boundary: the core refuses a "uses" workflow port (`workflow.port_uses`). */
export const WORKFLOW_PORT_KIND: Port["kind"] = "then";

/** Adds a workflow port (an input by default, always "then") and returns its id. */
export function addWorkflowPort(profile: Profile, workflowId: string, port: Partial<Port> = {}): { profile: Profile; id: string } {
  const workflow = workflowOf(profile, workflowId);
  if (!workflow) return { profile, id: "" };
  const id = uniqueId(port.id ?? (port.direction === "output" ? "out" : "in"), (workflow.ports ?? []).map((item) => item.id), "port");
  const added: Port = { name: id, capability: "text@1", direction: "input", required: false, ...port, kind: WORKFLOW_PORT_KIND, id };
  return { profile: updateWorkflow(profile, workflowId, (item) => ({ ...item, ports: [...(item.ports ?? []), added] })), id };
}

/** How many wires reach port `portId` of workflow `workflowId`, inside (`self`) and through its boxes. */
export function wiresOfPort(profile: Profile, workflowId: string, portId: string): number {
  const reaches = (end: End, owner: Workflow) => end.port_id === portId && (owner.id === workflowId
    ? end.node_id === SELF
    : owner.nodes.some((node) => node.id === end.node_id && isBox(node) && !node.ports.length && node.internal_workflow_id === workflowId));
  return profile.workflows.reduce((count, workflow) => count + workflow.wires.filter((wire) => reaches(wire.from, workflow) || reaches(wire.to, workflow)).length, 0);
}

/**
 * Changes a workflow port. A new id follows into every wire that names it. A new direction
 * or kind turns every wire that reaches the port around, so those wires are dropped (ask
 * `wiresOfPort` first to say how many). A "uses" kind is ignored: the core refuses it.
 */
export function updateWorkflowPort(profile: Profile, workflowId: string, portId: string, patch: Partial<Port>): Profile {
  const workflow = workflowOf(profile, workflowId);
  const port = workflow?.ports?.find((item) => item.id === portId);
  if (!workflow || !port) return profile;
  const { kind, ...rest } = patch;
  const safe: Partial<Port> = kind === undefined || kind === WORKFLOW_PORT_KIND ? patch : rest;
  let next = safe.id === undefined ? portId : slugify(safe.id);
  if (!next || (next !== portId && workflow.ports!.some((item) => item.id === next))) next = portId;
  const changed: Port = { ...port, ...safe, id: next };
  const updated = updateWorkflow(profile, workflowId, (item) => ({ ...item, ports: item.ports!.map((existing) => existing.id === portId ? changed : existing) }));
  if (changed.direction !== port.direction || changed.kind !== port.kind) return rewireBoundary(updated, workflowId, portId, null);
  return next === portId ? updated : rewireBoundary(updated, workflowId, portId, next);
}

export function removeWorkflowPort(profile: Profile, workflowId: string, portId: string): Profile {
  const workflow = workflowOf(profile, workflowId);
  if (!workflow?.ports?.some((port) => port.id === portId)) return profile;
  const updated = updateWorkflow(profile, workflowId, (item) => ({ ...item, ports: item.ports!.filter((port) => port.id !== portId) }));
  return rewireBoundary(updated, workflowId, portId, null);
}

/** `point`, moved right by `size.x` until no node of the workflow sits within `size` of it. */
export function freePosition(workflow: Workflow, point: Point, size: Point): Point {
  let { x } = point;
  const near = () => workflow.nodes.some((node) => Math.abs(node.position.x - x) < size.x && Math.abs(node.position.y - point.y) < size.y);
  for (let tries = 0; tries < 100 && near(); tries++) x += size.x;
  return { x, y: point.y };
}
