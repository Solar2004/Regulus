// What can be added to a workflow: the node types of every manifest, with the state of
// the module that brings them, and the profile's own workflows as boxes.
import type { CompositionNode, Manifest, ManifestNodeType, ModuleDefinition, Port, Profile, Workflow } from "../model";
import { baseModuleId, uniqueId, type Point } from "./document";
import { defaultsOf } from "./fields";

export type PieceState = "installed" | "disabled" | "broken" | "incompatible" | "not-installed";
export type Grouping = "module" | "state" | "none";

export interface PaletteEntry {
  key: string;
  moduleId: string;
  moduleName: string;
  type: string;
  name: string;
  state: PieceState;
}

/** How a module stands in `module.list`; absent there is "not installed", never an error. */
export function moduleStateOf(modules: ModuleDefinition[], moduleId: string | null): PieceState {
  if (!moduleId) return "installed";
  const module = modules.find((item) => item.id === moduleId) ?? modules.find((item) => baseModuleId(item.id) === baseModuleId(moduleId));
  if (!module) return "not-installed";
  const status = module.state?.status ?? "idle";
  if (status === "blocked") return "broken";
  return status === "active" || status === "idle" ? "installed" : status;
}

export const entryKey = (moduleId: string, type: string) => `${moduleId}#${type}`;

export function paletteEntries(manifests: Manifest[], modules: ModuleDefinition[]): PaletteEntry[] {
  return manifests.flatMap((manifest) => (manifest.node ?? []).map((node) => ({
    key: entryKey(manifest.module.id, node.type),
    moduleId: manifest.module.id,
    moduleName: manifest.module.name,
    type: node.type,
    name: node.name || node.type,
    state: moduleStateOf(modules, manifest.module.id),
  })));
}

/** Entries whose name, type or module contains every word of the query, case aside. */
export function filterPalette(entries: PaletteEntry[], query: string): PaletteEntry[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return entries;
  return entries.filter((entry) => {
    const text = `${entry.name} ${entry.type} ${entry.moduleName} ${entry.moduleId}`.toLowerCase();
    return words.every((word) => text.includes(word));
  });
}

const STATE_ORDER: PieceState[] = ["installed", "disabled", "incompatible", "broken", "not-installed"];

/** Groups in a stable order: by module name, by state (usable first), or one group. */
export function groupPalette(entries: PaletteEntry[], grouping: Grouping): { key: string; label: string; entries: PaletteEntry[] }[] {
  if (grouping === "none") return entries.length ? [{ key: "all", label: "", entries }] : [];
  const groups = new Map<string, { key: string; label: string; entries: PaletteEntry[] }>();
  for (const entry of entries) {
    const key = grouping === "module" ? entry.moduleId : entry.state;
    const group = groups.get(key) ?? { key, label: grouping === "module" ? entry.moduleName : entry.state, entries: [] };
    group.entries.push(entry);
    groups.set(key, group);
  }
  const order = (key: string) => grouping === "state" ? STATE_ORDER.indexOf(key as PieceState) : 0;
  return [...groups.values()].sort((a, b) => order(a.key) - order(b.key) || a.label.localeCompare(b.label));
}

export function findNodeType(manifests: Manifest[], key: string): { manifest: Manifest; nodeType: ManifestNodeType } | null {
  for (const manifest of manifests) for (const nodeType of manifest.node ?? []) {
    if (entryKey(manifest.module.id, nodeType.type) === key) return { manifest, nodeType };
  }
  return null;
}

/**
 * The ports a node of this type starts with when the core cannot say its shape (an older
 * core): the manifest's fixed ports; the dynamic ones need the core.
 */
export const manifestPorts = (nodeType: ManifestNodeType): Port[] => structuredClone(nodeType.port ?? []);

/** A node of a manifest type: its ports (the core's shape when given), its fields' defaults as config, a free id. */
export function nodeFromType(manifest: Manifest, nodeType: ManifestNodeType, position: Point, taken: Iterable<string>, ports?: Port[]): CompositionNode {
  return {
    id: uniqueId(nodeType.type, taken),
    type: nodeType.type,
    name: nodeType.name || nodeType.type,
    module_id: manifest.module.id,
    resource: null,
    ports: ports ? structuredClone(ports) : manifestPorts(nodeType),
    config: defaultsOf(nodeType.field),
    position: { x: position.x, y: position.y },
    internal_workflow_id: null,
    exposed_parameters: [],
  };
}

/** The workflows that can be placed as a box inside `workflowId`: any other one with ports. */
export const boxableWorkflows = (profile: Profile, workflowId: string): Workflow[] =>
  profile.workflows.filter((workflow) => workflow.id !== workflowId && (workflow.ports?.length ?? 0) > 0);

/** A box: no module, the workflow inside, and no ports of its own (they are the workflow's). */
export function boxNode(workflow: Workflow, position: Point, taken: Iterable<string>): CompositionNode {
  return {
    id: uniqueId(workflow.id, taken),
    type: "workflow",
    name: workflow.name || workflow.id,
    module_id: null,
    resource: null,
    ports: [],
    config: {},
    position: { x: position.x, y: position.y },
    internal_workflow_id: workflow.id,
    exposed_parameters: [],
  };
}

/** The manifest type of a node already in a profile, matching the module with or without version. */
export function manifestTypeOf(manifests: Manifest[], moduleId: string | null, type: string): ManifestNodeType | null {
  if (!moduleId) return null;
  const manifest = manifests.find((item) => item.module.id === moduleId) ?? manifests.find((item) => baseModuleId(item.module.id) === baseModuleId(moduleId));
  return manifest?.node?.find((node) => node.type === type) ?? null;
}
