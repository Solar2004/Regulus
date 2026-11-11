// What the authoring views decide, as pure functions with tests. The shape of a node is
// never worked out here: the core's `module.shape` and `profile.sync` are the one resolver.
import type { ManifestNodeType, ModuleDefinition, Port, Profile, ProfileSynced, ProjectReloaded, ProjectSource, ReloadOutcome } from "../model";
import { baseModuleId } from "../editor/document";

/** A local module's id inside its project: `demo/caps@0.1.0` → `caps`. */
export function localIdOf(moduleId: string): string {
  const base = baseModuleId(moduleId);
  const slash = base.indexOf("/");
  return slash < 0 ? base : base.slice(slash + 1);
}

export interface SourceRef {
  projectId: string;
  /** The local id `source.*` and `project.reload` take. */
  module: string;
  /** Project-relative path of the editable file. */
  path: string;
  /** The module's id as `module.list` gives it. */
  moduleId: string;
}

/** The module.list entry for an id, exact first, then by id without version. */
export function moduleEntry(modules: ModuleDefinition[], moduleId: string | null): ModuleDefinition | undefined {
  if (!moduleId) return undefined;
  return modules.find((item) => item.id === moduleId) ?? modules.find((item) => baseModuleId(item.id) === baseModuleId(moduleId));
}

/**
 * The editable source of a module: a local module (its `module.list` entry names its
 * project) whose project lists a file for it (`project.get` → `sources`, proto 1.17).
 * `sources` is that project's list, or `undefined` while it is not known.
 */
export function sourceOf(modules: ModuleDefinition[], moduleId: string | null, sources: ProjectSource[] | undefined): SourceRef | null {
  const entry = moduleEntry(modules, moduleId);
  if (!entry?.project || !sources) return null;
  const local = localIdOf(entry.id);
  const found = sources.find((item) => item.module === local);
  return found?.path ? { projectId: entry.project, module: local, path: found.path, moduleId: entry.id } : null;
}

/** The project a module belongs to, if it is a local one. */
export const projectOf = (modules: ModuleDefinition[], moduleId: string | null) => moduleEntry(modules, moduleId)?.project ?? null;

/** Field ids a node type's shape reads: named by a `when` of a dynamic port or a field, or by a dynamic port's `repeat`. */
export function shapeFields(nodeType: ManifestNodeType | null | undefined): Set<string> {
  const found = new Set<string>();
  for (const port of nodeType?.dynamic_port ?? []) {
    if (port.when?.field) found.add(port.when.field);
    if (port.repeat) found.add(port.repeat);
  }
  for (const field of nodeType?.field ?? []) if (field.when?.field) found.add(field.when.field);
  return found;
}

/** Whether the core keeps this type's ports equal to its shape, or its shape depends on its config. */
export const hasShape = (nodeType: ManifestNodeType | null | undefined) => !!nodeType && (nodeType.managed_ports === true || shapeFields(nodeType).size > 0);

/** A port with the defaults the core assumes written out (an empty name reads as the id, as a loaded profile has it), so two forms of the same port compare equal. */
const normal = (port: Port) => ({
  id: port.id, name: port.name || port.id, capability: port.capability, direction: port.direction, kind: port.kind,
  required: !!port.required, cardinality: port.cardinality ?? "one", lazy: !!port.lazy,
});

/** The keys in which two forms of a port differ, in a fixed order (`name`, `capability`…). */
export function changedKeys(before: Port, after: Port): string[] {
  const a = normal(before) as Record<string, unknown>;
  const b = normal(after) as Record<string, unknown>;
  return Object.keys(a).filter((key) => a[key] !== b[key]);
}

/** Same ports in the same order, every key compared (a different name counts, as in `profile.check`). */
export function samePorts(a: Port[], b: Port[]): boolean {
  return a.length === b.length && a.every((port, index) => JSON.stringify(normal(port)) === JSON.stringify(normal(b[index])));
}

/**
 * Takes a `profile.sync` preview into the document being edited: each listed node gets the
 * ports the preview gives it, and the wires the preview drops go. Only those nodes and
 * wires change, so edits elsewhere are kept. `only` limits it to some nodes. Returns the
 * same profile when nothing changes.
 */
export function applySync(current: Profile, synced: ProfileSynced, only?: { workflow_id: string; node_id: string }[]): Profile {
  const chosen = only ? new Set(only.map((item) => `${item.workflow_id}\u0000${item.node_id}`)) : null;
  const entries = synced.nodes.filter((item) => !item.problems.length || item.added.length || item.removed.length || item.changed.length)
    .filter((item) => !chosen || chosen.has(`${item.workflow_id}\u0000${item.node_id}`));
  if (!entries.length) return current;
  let changed = false;
  const workflows = current.workflows.map((workflow) => {
    const here = entries.filter((item) => item.workflow_id === workflow.id);
    if (!here.length) return workflow;
    const from = synced.profile.workflows.find((item) => item.id === workflow.id);
    const ports = new Map(here.flatMap((item) => {
      const node = from?.nodes.find((candidate) => candidate.id === item.node_id);
      return node ? [[item.node_id, node.ports] as const] : [];
    }));
    const dropped = new Set(here.flatMap((item) => item.wires_removed.map(({ wire }) => wire.id)));
    const nodes = workflow.nodes.map((node) => {
      const next = ports.get(node.id);
      if (!next || samePorts(node.ports, next)) return node;
      changed = true;
      return { ...node, ports: structuredClone(next) };
    });
    const wires = workflow.wires.filter((wire) => !dropped.has(wire.id));
    if (wires.length !== workflow.wires.length) changed = true;
    return { ...workflow, nodes, wires };
  });
  return changed ? { ...current, workflows } : current;
}

/** The document a `profile.sync` preview was asked for: its profile and that exact draft. */
export interface SyncOrigin { profileId: string; doc: Profile }

/**
 * Whether a preview may still be applied to the document now: `fits` while it is the very
 * draft the preview was asked for, `changed` when the same profile was edited since (ask
 * the core again), `other` when another profile is open (or none).
 */
export function syncFits(origin: SyncOrigin, current: Profile | null): "fits" | "changed" | "other" {
  if (!current || current.id !== origin.profileId) return "other";
  return current === origin.doc ? "fits" : "changed";
}

/** What a sync preview changes, counted. */
export function syncCounts(synced: ProfileSynced): { nodes: number; added: number; removed: number; changed: number; wires: number } {
  const counted = synced.nodes.filter((item) => item.added.length || item.removed.length || item.changed.length || item.wires_removed.length);
  return {
    nodes: counted.length,
    added: counted.reduce((sum, item) => sum + item.added.length, 0),
    removed: counted.reduce((sum, item) => sum + item.removed.length, 0),
    changed: counted.reduce((sum, item) => sum + item.changed.length, 0),
    wires: counted.reduce((sum, item) => sum + item.wires_removed.length, 0),
  };
}

/** Outcomes that leave a module not running the new code, worst first. */
const FAILED: ReloadOutcome[] = ["failed", "kept", "skipped"];

/** A reload's result for one line of feedback: whether everything took, what failed, and stale nodes of one profile. */
export function reloadSummary(result: ProjectReloaded, profileId: string | null) {
  const failed = result.modules.filter((item) => FAILED.includes(item.outcome));
  return {
    ok: failed.length === 0,
    failed,
    blocked: result.modules.filter((item) => item.outcome === "blocked"),
    stale: profileId ? result.stale.filter((item) => item.profile_id === profileId) : [],
  };
}
