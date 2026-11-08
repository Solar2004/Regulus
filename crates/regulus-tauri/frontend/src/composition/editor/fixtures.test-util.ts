// Small profiles the editor tests share.
import type { CompositionNode, Port, Profile } from "../model";

export const port = (id: string, direction: Port["direction"], kind: Port["kind"] = "then", capability = "text@1", extra: Partial<Port> = {}): Port =>
  ({ id, name: id, capability, direction, kind, required: false, ...extra });

export const node = (id: string, ports: Port[], extra: Partial<CompositionNode> = {}): CompositionNode => ({
  id, type: "greeter", name: id, module_id: "regulus/hello@0.1.0", resource: null, ports, config: {},
  position: { x: 0, y: 0 }, internal_workflow_id: null, exposed_parameters: [], ...extra,
});

/** `main` with a greeter `a`, a writer `b` and an agent `c`; `inner` with ports, placed in `main` as the box `box`. */
export function sample(): Profile {
  return {
    id: "p", name: "P", project_id: null, extends: null, main_workflow_id: "main",
    workflows: [
      {
        id: "main", name: "Main", kind: "main", module_id: null, layout: { x: 0, y: 0, width: 0, height: 0 },
        ports: [port("ask", "input"), port("reply", "output")],
        nodes: [
          node("a", [port("tick", "output"), port("echo", "input", "then", "text@1", { cardinality: "many" }), port("greeting", "output", "uses")], { position: { x: 100, y: 50 } }),
          node("b", [port("in", "input"), port("written", "output", "then", "path@1")], { type: "write", module_id: "regulus/files@0.1.0", position: { x: 400, y: 80 } }),
          node("c", [port("engine", "input", "uses", "agent.engine@1", { required: true }), port("greeting", "input", "uses")], { type: "agent", module_id: "regulus/agent@0.1.0" }),
          node("box", [], { type: "workflow", module_id: null, internal_workflow_id: "inner" }),
        ],
        wires: [{ id: "w1", kind: "then", from: { node_id: "a", port_id: "tick" }, to: { node_id: "b", port_id: "in" } }],
      },
      {
        id: "inner", name: "Inner", kind: "extra", module_id: null, layout: { x: 0, y: 0, width: 0, height: 0 },
        ports: [port("q", "input"), port("r", "output")],
        nodes: [node("h", [port("echo", "input"), port("reply", "output")])],
        wires: [
          { id: "in-h", kind: "then", from: { node_id: "self", port_id: "q" }, to: { node_id: "h", port_id: "echo" } },
          { id: "h-out", kind: "then", from: { node_id: "h", port_id: "reply" }, to: { node_id: "self", port_id: "r" } },
        ],
      },
    ],
  };
}
