// A coding harness as the `regulus-code` template makes it, for the tests of this view:
// the workflow `code` (agent, native engine, provider, workspace, decider, workers) and the
// workflow `worker` its workers run.
import { node, port } from "@/composition/editor/fixtures.test-util";
import type { Manifest, Profile, Wire, Workflow } from "@/composition/model";

const uses = (id: string, from: [string, string], to: [string, string]): Wire =>
  ({ id, kind: "uses", from: { node_id: from[0], port_id: from[1] }, to: { node_id: to[0], port_id: to[1] } });
const layout = { x: 0, y: 0, width: 0, height: 0 };

export const agentNode = () => node("agent", [
  port("engine", "input", "uses", "agent.engine@1", { required: true }),
  port("decisions", "input", "uses", "decision@1"),
  port("prompt", "input"), port("reply", "output"),
], { type: "agent", module_id: "regulus/agent@0.1.0" });

export const workspaceNode = (root: string | null = "/home/ada/app") => node("workspace", [
  port("workspace", "output", "uses", "code.workspace@1"), port("tools", "output", "uses", "tool@1"),
], { type: "workspace", module_id: "regulus/code@0.1.0", config: root === null ? {} : { root } });

export function codeWorkflow(root: string | null = "/home/ada/app"): Workflow {
  return {
    id: "code", name: "Regulus Code", kind: "main", module_id: null, layout,
    nodes: [
      agentNode(),
      node("native", [
        port("engine", "output", "uses", "agent.engine@1"), port("model", "input", "uses", "llm.endpoint@1", { required: true }),
        port("tools", "input", "uses", "tool@1", { cardinality: "many" }),
      ], { type: "native", module_id: "regulus/engine-native@0.1.0" }),
      node("provider", [port("endpoint", "output", "uses", "llm.endpoint@1")], { type: "provider", module_id: "regulus/providers@0.1.0" }),
      workspaceNode(root),
      node("decider", [port("decisions", "output", "uses", "decision@1")], { type: "decider", module_id: "regulus/human@0.1.0" }),
      node("workers", [port("tools", "output", "uses", "tool@1"), port("engine", "input", "uses", "agent.engine@1", { cardinality: "many", lazy: true })],
        { type: "delegate", module_id: "regulus/delegation@0.1.0" }),
    ],
    wires: [
      uses("native-agent", ["native", "engine"], ["agent", "engine"]),
      uses("provider-native", ["provider", "endpoint"], ["native", "model"]),
      uses("workspace-native", ["workspace", "tools"], ["native", "tools"]),
      uses("workers-native", ["workers", "tools"], ["native", "tools"]),
      uses("decider-agent", ["decider", "decisions"], ["agent", "decisions"]),
      uses("native-workers", ["native", "engine"], ["workers", "engine"]),
    ],
  };
}

/** One worker: an agent on whatever engine the delegate binds; no workspace of its own. */
export const workerWorkflow = (): Workflow => ({
  id: "worker", name: "Worker", kind: "extra", module_id: null, layout,
  ports: [port("prompt", "input"), port("reply", "output"), port("engine", "input", "uses", "agent.engine@1")],
  nodes: [node("worker", [port("engine", "input", "uses", "agent.engine@1", { required: true }), port("prompt", "input"), port("reply", "output")], { type: "agent", module_id: "regulus/agent@0.1.0" })],
  wires: [uses("worker-engine", ["self", "engine"], ["worker", "engine"])],
});

export const codeProfile = (id = "code-app", root: string | null = "/home/ada/app"): Profile =>
  ({ id, name: `Regulus Code in ${root ?? id}`, project_id: null, extends: null, main_workflow_id: "code", workflows: [codeWorkflow(root), workerWorkflow()] });

/** The manifests the harness needs: who offers `code.workspace`, who declares `delegation.workers`. */
export const manifests = (): Manifest[] => [
  { module: { id: "regulus/code@0.1.0", name: "Code", methods: ["code.inspect"] }, node: [{ type: "workspace", name: "Workspace", port: workspaceNode().ports }] },
  { module: { id: "regulus/delegation@0.1.0", name: "Delegation", methods: ["delegation.workers", "delegation.close"] }, node: [{ type: "delegate", name: "Delegate", port: [] }] },
  { module: { id: "regulus/agent@0.1.0", name: "Agent", methods: ["agent.send", "agent.history"] }, node: [{ type: "agent", name: "Agent", port: agentNode().ports }] },
];
