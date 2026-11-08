// Whether a wire may join two ports: the same rules `profile.check` applies in the core
// (`regulus_core::graph::check_workflow`), asked before the wire exists.
import type { Port, Profile, Wire } from "../model";
import { SELF, nodeOf, portsOf, workflowOf, type End } from "./document";

export type Refusal = "sameNode" | "missingPort" | "direction" | "kind" | "capability" | "taken" | "duplicate";

export type ConnectResult =
  | { ok: true; kind: Wire["kind"] }
  | { ok: false; reason: Refusal; from?: Port; to?: Port };

/** One end of a wire as seen from inside the workflow: a `self` input feeds nodes, so inside it is an output. */
export function endpoint(profile: Profile, workflowId: string, end: End): { port: Port; direction: Port["direction"] } | null {
  const workflow = workflowOf(profile, workflowId);
  if (!workflow) return null;
  if (end.node_id === SELF) {
    const port = workflow.ports?.find((item) => item.id === end.port_id);
    return port ? { port, direction: port.direction === "input" ? "output" : "input" } : null;
  }
  const node = nodeOf(workflow, end.node_id);
  const port = node && portsOf(profile, node).find((item) => item.id === end.port_id);
  return port ? { port, direction: port.direction } : null;
}

/** May a wire go from `from` (an output) to `to` (an input) in this workflow? */
export function canConnect(profile: Profile, workflowId: string, from: End, to: End): ConnectResult {
  if (from.node_id === to.node_id && from.node_id !== SELF) return { ok: false, reason: "sameNode" };
  const source = endpoint(profile, workflowId, from);
  const target = endpoint(profile, workflowId, to);
  if (!source || !target) return { ok: false, reason: "missingPort" };
  const ports = { from: source.port, to: target.port };
  if (source.direction !== "output" || target.direction !== "input") return { ok: false, reason: "direction", ...ports };
  if (source.port.kind !== target.port.kind) return { ok: false, reason: "kind", ...ports };
  if (source.port.capability !== target.port.capability) return { ok: false, reason: "capability", ...ports };
  const wires = workflowOf(profile, workflowId)!.wires.filter((wire) => wire.to.node_id === to.node_id && wire.to.port_id === to.port_id);
  if (wires.some((wire) => wire.from.node_id === from.node_id && wire.from.port_id === from.port_id)) return { ok: false, reason: "duplicate", ...ports };
  if (wires.length && target.port.cardinality !== "many") return { ok: false, reason: "taken", ...ports };
  return { ok: true, kind: source.port.kind };
}
