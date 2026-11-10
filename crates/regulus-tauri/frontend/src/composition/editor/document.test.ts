import { describe, expect, test } from "vitest";
import * as doc from "./document";
import { node, port, sample } from "./fixtures.test-util";

describe("ids", () => {
  test("slugify keeps file-safe characters", () => {
    expect(doc.slugify("  My Profile: Ñandú v2 ")).toBe("my-profile-nandu-v2");
    expect(doc.slugify("../etc")).toBe("etc");
    expect(doc.slugify("a/b")).toBe("a-b");
  });
  test("uniqueId avoids taken ids and self", () => {
    expect(doc.uniqueId("greeter", [])).toBe("greeter");
    expect(doc.uniqueId("greeter", ["greeter", "greeter-2"])).toBe("greeter-3");
    expect(doc.uniqueId("self", [])).toBe("self-2");
    expect(doc.uniqueId("///", [], "node")).toBe("node");
  });
  test("baseModuleId drops the version", () => expect(doc.baseModuleId("regulus/hello@0.1.0")).toBe("regulus/hello"));
});

describe("profile changes", () => {
  test("newProfile has one empty main workflow", () => {
    const profile = doc.newProfile("x", "X", "Main");
    expect(profile.main_workflow_id).toBe("main");
    expect(profile.workflows).toHaveLength(1);
    expect(profile.workflows[0]).toMatchObject({ id: "main", kind: "main", nodes: [], wires: [] });
  });

  test("changes never touch the original", () => {
    const before = sample();
    const frozen = structuredClone(before);
    doc.removeNodes(before, "main", ["a"]);
    doc.moveNodes(before, "main", [{ id: "a", position: { x: 1, y: 2 } }]);
    doc.setConfig(before, "main", "a", "name", "Ana");
    expect(before).toEqual(frozen);
  });

  test("addNode appends and refuses a duplicate id", () => {
    const profile = doc.addNode(sample(), "main", node("d", []));
    expect(profile.workflows[0].nodes.map((n) => n.id)).toEqual(["a", "b", "c", "box", "d"]);
    expect(doc.addNode(profile, "main", node("d", [])).workflows[0].nodes).toHaveLength(5);
  });

  test("moveNodes moves only the named nodes and keeps unmoved ones identical", () => {
    const before = sample();
    const after = doc.moveNodes(before, "main", [{ id: "a", position: { x: 7, y: 9 } }]);
    expect(after.workflows[0].nodes[0].position).toEqual({ x: 7, y: 9 });
    expect(after.workflows[0].nodes[1]).toBe(before.workflows[0].nodes[1]);
  });

  test("removeNodes takes their wires with them", () => {
    const after = doc.removeNodes(sample(), "main", ["b"]);
    expect(after.workflows[0].nodes.map((n) => n.id)).toEqual(["a", "c", "box"]);
    expect(after.workflows[0].wires).toEqual([]);
  });

  test("addWire gives a fresh id and removeWires removes by id", () => {
    let profile = doc.addWire(sample(), "main", "then", { node_id: "self", port_id: "ask" }, { node_id: "a", port_id: "echo" });
    const wire = profile.workflows[0].wires[1];
    expect(wire).toMatchObject({ kind: "then", from: { node_id: "self" }, to: { node_id: "a", port_id: "echo" } });
    expect(wire.id).not.toContain("/");
    profile = doc.addWire(profile, "main", "then", { node_id: "self", port_id: "ask" }, { node_id: "a", port_id: "echo" });
    expect(new Set(profile.workflows[0].wires.map((w) => w.id)).size).toBe(3);
    expect(doc.removeWires(profile, "main", [wire.id]).workflows[0].wires).toHaveLength(2);
  });

  test("setConfig sets and removes keys; renameNode and setInternalWorkflow edit the node", () => {
    let profile = doc.setConfig(sample(), "main", "a", "name", "Ana");
    expect(profile.workflows[0].nodes[0].config).toEqual({ name: "Ana" });
    profile = doc.setConfig(profile, "main", "a", "name", undefined);
    expect(profile.workflows[0].nodes[0].config).toEqual({});
    profile = doc.renameNode(profile, "main", "a", "Ana");
    expect(profile.workflows[0].nodes[0].name).toBe("Ana");
    profile = doc.setInternalWorkflow(profile, "main", "a", "inner");
    expect(profile.workflows[0].nodes[0].internal_workflow_id).toBe("inner");
    expect(doc.setInternalWorkflow(profile, "main", "a", "").workflows[0].nodes[0].internal_workflow_id).toBeNull();
  });

  test("an unknown workflow or node changes nothing", () => {
    const before = sample();
    expect(doc.setConfig(before, "nope", "a", "k", 1)).toBe(before);
    expect(doc.renameNode(before, "main", "nope", "x")).toEqual(before);
  });
});

describe("ports of a box", () => {
  test("a box without ports takes its workflow's", () => {
    const profile = sample();
    const box = profile.workflows[0].nodes[3];
    expect(doc.isBox(box)).toBe(true);
    expect(doc.portsOf(profile, box).map((p) => p.id)).toEqual(["q", "r"]);
    expect(doc.portsOf(profile, profile.workflows[0].nodes[0]).map((p) => p.id)).toEqual(["tick", "echo", "greeting"]);
  });
});

describe("workflows", () => {
  test("addWorkflow adds an extra workflow with a free id", () => {
    const { profile, id } = doc.addWorkflow(sample(), "Inner");
    expect(id).toBe("inner-2");
    expect(profile.workflows[profile.workflows.length - 1]).toMatchObject({ id: "inner-2", name: "Inner", kind: "extra" });
  });

  test("removeWorkflow refuses main and removes the boxes that placed it, with their wires", () => {
    const before = doc.addWire(sample(), "main", "then", { node_id: "a", port_id: "tick" }, { node_id: "box", port_id: "q" });
    expect(doc.removeWorkflow(before, "main")).toBe(before);
    expect(doc.boxesOf(before, "inner")).toEqual([{ workflowId: "main", nodeId: "box" }]);
    const after = doc.removeWorkflow(before, "inner");
    expect(after.workflows.map((w) => w.id)).toEqual(["main"]);
    expect(after.workflows[0].nodes.map((n) => n.id)).toEqual(["a", "b", "c"]);
    expect(after.workflows[0].wires.map((w) => w.id)).toEqual(["w1"]);
  });

  test("after removing a workflow no wire names a node that is gone", () => {
    let profile = doc.addNode(sample(), "main", node("own", [port("q", "input")], { module_id: null, internal_workflow_id: "inner" }));
    profile = doc.addWire(profile, "main", "then", { node_id: "a", port_id: "tick" }, { node_id: "own", port_id: "q" });
    profile = doc.addWire(profile, "main", "then", { node_id: "box", port_id: "r" }, { node_id: "a", port_id: "echo" });
    // A module node that names the workflow as its inner one stays, unhooked.
    profile = doc.setInternalWorkflow(profile, "main", "a", "inner");
    const after = doc.removeWorkflow(profile, "inner");
    for (const workflow of after.workflows) {
      const ids = new Set(workflow.nodes.map((n) => n.id));
      for (const wire of workflow.wires) for (const end of [wire.from, wire.to]) expect(end.node_id === "self" || ids.has(end.node_id)).toBe(true);
    }
    expect(after.workflows[0].nodes.map((n) => n.id)).toEqual(["a", "b", "c"]);
    expect(after.workflows[0].nodes[0].internal_workflow_id).toBeNull();
  });

  test("renameWorkflow changes the name only", () => {
    expect(doc.renameWorkflow(sample(), "inner", "Renamed").workflows[1]).toMatchObject({ id: "inner", name: "Renamed" });
  });

  test("addWorkflowPort adds a then input by default with a free id", () => {
    const { profile, id } = doc.addWorkflowPort(sample(), "inner");
    expect(id).toBe("in");
    expect(profile.workflows[1].ports?.[profile.workflows[1].ports!.length - 1]).toMatchObject({ id: "in", direction: "input", kind: "then", capability: "text@1" });
    expect(doc.addWorkflowPort(profile, "inner", { direction: "output" }).id).toBe("out");
  });

  test("renaming a port follows into self wires and wires to boxes", () => {
    const profile = doc.addWire(sample(), "main", "then", { node_id: "a", port_id: "tick" }, { node_id: "box", port_id: "q" });
    const after = doc.updateWorkflowPort(profile, "inner", "q", { id: "Question" });
    expect(after.workflows[1].ports?.[0].id).toBe("question");
    expect(after.workflows[1].wires[0].from).toEqual({ node_id: "self", port_id: "question" });
    expect(after.workflows[0].wires[1].to).toEqual({ node_id: "box", port_id: "question" });
    // A taken or empty id keeps the old one; other fields still change.
    const kept = doc.updateWorkflowPort(after, "inner", "question", { id: "r", capability: "path@1" });
    expect(kept.workflows[1].ports?.[0]).toMatchObject({ id: "question", capability: "path@1" });
  });

  test("workflow ports take either kind: uses crosses the boundary too", () => {
    const added = doc.addWorkflowPort(sample(), "inner", { kind: "uses", capability: "llm.endpoint@1" });
    expect(added.profile.workflows[1].ports?.find((p) => p.id === added.id)).toMatchObject({ kind: "uses", capability: "llm.endpoint@1", direction: "input" });
    const output = doc.addWorkflowPort(sample(), "inner", { kind: "uses", direction: "output", required: true, lazy: true });
    expect(output.profile.workflows[1].ports?.find((p) => p.id === output.id)).toMatchObject({ id: "out", kind: "uses", direction: "output", required: true, lazy: true });
    // A new kind turns the port around: the wires that reached it go.
    const turned = doc.updateWorkflowPort(sample(), "inner", "q", { kind: "uses", capability: "path@1" });
    expect(turned.workflows[1].ports?.[0]).toMatchObject({ kind: "uses", capability: "path@1" });
    expect(turned.workflows[1].wires.map((w) => w.id)).toEqual(["h-out"]);
    // Required, lazy and cardinality change in place and keep the wires.
    const flags = doc.updateWorkflowPort(sample(), "inner", "q", { required: true, lazy: true, cardinality: "many" });
    expect(flags.workflows[1].ports?.[0]).toMatchObject({ required: true, lazy: true, cardinality: "many" });
    expect(flags.workflows[1].wires).toHaveLength(2);
  });

  test("a new direction drops the wires that reached the port, inside and through boxes", () => {
    const profile = doc.addWire(sample(), "main", "then", { node_id: "a", port_id: "tick" }, { node_id: "box", port_id: "q" });
    expect(doc.wiresOfPort(profile, "inner", "q")).toBe(2);
    expect(doc.wiresOfPort(profile, "inner", "r")).toBe(1);
    const after = doc.updateWorkflowPort(profile, "inner", "q", { direction: "output", id: "q2" });
    expect(after.workflows[1].ports?.[0]).toMatchObject({ id: "q2", direction: "output" });
    expect(after.workflows[1].wires.map((w) => w.id)).toEqual(["h-out"]);
    expect(after.workflows[0].wires.map((w) => w.id)).toEqual(["w1"]);
    expect(doc.wiresOfPort(after, "inner", "q2")).toBe(0);
    // A loaded "uses" port turned into "then" loses its wires the same way.
    const uses = { ...sample() };
    uses.workflows[1].ports![0].kind = "uses";
    expect(doc.updateWorkflowPort(uses, "inner", "q", { kind: "then" }).workflows[1].wires.map((w) => w.id)).toEqual(["h-out"]);
  });

  test("removing a port drops the wires that reached it", () => {
    const profile = doc.addWire(sample(), "main", "then", { node_id: "a", port_id: "tick" }, { node_id: "box", port_id: "q" });
    const after = doc.removeWorkflowPort(profile, "inner", "q");
    expect(after.workflows[1].ports?.map((p) => p.id)).toEqual(["r"]);
    expect(after.workflows[1].wires.map((w) => w.id)).toEqual(["h-out"]);
    expect(after.workflows[0].wires.map((w) => w.id)).toEqual(["w1"]);
  });

  test("a port of a box that declares its own ports is not the workflow's", () => {
    let profile = sample();
    profile = doc.addNode(profile, "main", node("own", [port("q", "input")], { module_id: null, internal_workflow_id: "inner" }));
    profile = doc.addWire(profile, "main", "then", { node_id: "a", port_id: "tick" }, { node_id: "own", port_id: "q" });
    const after = doc.removeWorkflowPort(profile, "inner", "q");
    expect(after.workflows[0].wires.some((w) => w.to.node_id === "own")).toBe(true);
  });
});

/** `main` with a provider `p`, an agent `ag` that uses it, a writer `w` fed by the agent, and `tools` serving `ag`. */
function usesSample() {
  const uses = (id: string, direction: "input" | "output", capability: string, extra = {}) => port(id, direction, "uses", capability, extra);
  let profile = sample();
  profile = doc.addNode(profile, "main", node("p", [uses("endpoint", "output", "llm.endpoint@1")], { position: { x: 0, y: 0 } }));
  profile = doc.addNode(profile, "main", node("p2", [uses("endpoint", "output", "llm.endpoint@1")], { position: { x: 0, y: 200 } }));
  profile = doc.addNode(profile, "main", node("ag", [uses("provider", "input", "llm.endpoint@1", { required: true }), uses("tools", "input", "tool@1", { cardinality: "many" }), port("prompt", "input"), port("reply", "output")], { position: { x: 300, y: 0 } }));
  profile = doc.addNode(profile, "main", node("tools", [uses("toolset", "output", "tool@1")], { position: { x: 300, y: 200 } }));
  profile = doc.addNode(profile, "main", node("w", [port("in", "input"), port("done", "output")], { position: { x: 600, y: 0 } }));
  profile = doc.addNode(profile, "main", node("w2", [port("in", "input")], { position: { x: 600, y: 200 } }));
  const wire = (kind: "uses" | "then", from: string, to: string) => {
    const [fn, fp] = from.split("."); const [tn, tp] = to.split(".");
    profile = doc.addWire(profile, "main", kind, { node_id: fn, port_id: fp }, { node_id: tn, port_id: tp });
  };
  wire("uses", "p.endpoint", "ag.provider");
  wire("uses", "tools.toolset", "ag.tools");
  wire("then", "self.ask", "ag.prompt");
  wire("then", "ag.reply", "w.in");
  wire("then", "ag.reply", "w2.in");
  return profile;
}

describe("create box from selection", () => {
  test("cut Then wires become ports; inside they reach self, outside the box", () => {
    const made = doc.createBoxFromSelection(usesSample(), "main", ["ag"], "Agent box")!;
    expect(made).not.toBeNull();
    expect(made.workflowId).toBe("agent-box");
    expect(made.boxId).toBe("agent-box");
    const inner = doc.workflowOf(made.profile, "agent-box")!;
    expect(inner).toMatchObject({ name: "Agent box", kind: "extra" });
    expect(inner.nodes.map((n) => n.id)).toEqual(["ag"]);
    const byId = Object.fromEntries(inner.ports!.map((p) => [p.id, p]));
    expect(byId.prompt).toMatchObject({ direction: "input", kind: "then", capability: "text@1" });
    // One inner output that fed two outside nodes is one port, wired to both.
    expect(byId.reply).toMatchObject({ direction: "output", kind: "then" });
    expect(inner.wires.filter((w) => w.to.node_id === "self").map((w) => `${w.from.node_id}.${w.from.port_id}>${w.to.port_id}`)).toEqual(["ag.reply>reply"]);
    const main = doc.workflowOf(made.profile, "main")!;
    expect(main.nodes.some((n) => n.id === "ag")).toBe(false);
    const box = doc.nodeOf(main, "agent-box")!;
    expect(doc.isBox(box)).toBe(true);
    expect(box.internal_workflow_id).toBe("agent-box");
    expect(main.wires.filter((w) => w.from.node_id === "agent-box").map((w) => `${w.from.port_id}>${w.to.node_id}`).sort()).toEqual(["reply>w", "reply>w2"]);
    expect(main.wires.find((w) => w.to.node_id === "agent-box" && w.kind === "then")?.from).toEqual({ node_id: "self", port_id: "ask" });
  });

  test("cut Uses wires become Uses ports: a provider in, a toolset out", () => {
    // The agent stays outside; the provider and the tools go in the box.
    const made = doc.createBoxFromSelection(usesSample(), "main", ["p", "tools"], "Services")!;
    const inner = doc.workflowOf(made.profile, made.workflowId)!;
    expect(inner.ports!.map((p) => [p.id, p.direction, p.kind, p.capability])).toEqual([
      ["endpoint", "output", "uses", "llm.endpoint@1"], ["toolset", "output", "uses", "tool@1"],
    ]);
    const main = doc.workflowOf(made.profile, "main")!;
    expect(main.wires.filter((w) => w.kind === "uses").map((w) => `${w.from.node_id}.${w.from.port_id}>${w.to.node_id}.${w.to.port_id}`).sort())
      .toEqual(["services.endpoint>ag.provider", "services.toolset>ag.tools"]);
    // The agent inside, the provider outside: a Uses input, required like the agent's.
    const other = doc.createBoxFromSelection(usesSample(), "main", ["ag", "tools"], "Worker")!;
    const worker = doc.workflowOf(other.profile, "worker")!;
    expect(worker.ports!.find((p) => p.id === "provider")).toMatchObject({ direction: "input", kind: "uses", capability: "llm.endpoint@1", required: true });
    expect(worker.wires.find((w) => w.from.node_id === "self" && w.from.port_id === "provider")?.to).toEqual({ node_id: "ag", port_id: "provider" });
    // tools → ag is now an inner wire, untouched.
    expect(worker.wires.some((w) => w.from.node_id === "tools" && w.to.node_id === "ag")).toBe(true);
    // Every wire still fits: the canvas would accept each one.
    for (const workflow of other.profile.workflows) for (const wire of workflow.wires) {
      const from = doc.portAt(other.profile, workflow, wire.from);
      const to = doc.portAt(other.profile, workflow, wire.to);
      expect(from && to && from.capability === to.capability && from.kind === wire.kind && to.kind === wire.kind).toBe(true);
    }
  });

  test("several outside wires into one inner input share a port that takes many", () => {
    let profile = usesSample();
    profile = doc.addWire(profile, "main", "uses", { node_id: "p2", port_id: "endpoint" }, { node_id: "a", port_id: "greeting" });
    // ag.tools takes many: feed it from two outside nodes.
    profile = doc.addNode(profile, "main", node("tools2", [port("toolset", "output", "uses", "tool@1")]));
    profile = doc.addWire(profile, "main", "uses", { node_id: "tools2", port_id: "toolset" }, { node_id: "ag", port_id: "tools" });
    const made = doc.createBoxFromSelection(profile, "main", ["ag"], "x")!;
    const tools = doc.workflowOf(made.profile, "x")!.ports!.find((p) => p.id === "tools")!;
    expect(tools.cardinality).toBe("many");
    expect(doc.workflowOf(made.profile, "x")!.wires.filter((w) => w.to.port_id === "tools")).toHaveLength(1);
    expect(doc.workflowOf(made.profile, "main")!.wires.filter((w) => w.to.node_id === "x" && w.to.port_id === "tools")).toHaveLength(2);
  });

  test("a box inside the selection moves with its wires and keeps its workflow", () => {
    let profile = doc.addWire(sample(), "main", "then", { node_id: "a", port_id: "tick" }, { node_id: "box", port_id: "q" });
    profile = doc.addWire(profile, "main", "then", { node_id: "box", port_id: "r" }, { node_id: "a", port_id: "echo" });
    const made = doc.createBoxFromSelection(profile, "main", ["box", "a"], "Outer")!;
    const outer = doc.workflowOf(made.profile, "outer")!;
    expect(outer.nodes.map((n) => n.id)).toEqual(["a", "box"]);
    expect(doc.nodeOf(outer, "box")!.internal_workflow_id).toBe("inner");
    expect(outer.wires.filter((w) => w.from.node_id !== "self" && w.to.node_id !== "self").map((w) => w.id)).toContain("a-tick-box-q");
    // a.tick → b.in crossed out: one output port.
    expect(outer.ports!.map((p) => [p.id, p.direction])).toEqual([["tick", "output"]]);
    expect(doc.workflowOf(made.profile, "inner")).toEqual(doc.workflowOf(profile, "inner"));
  });

  test("names that collide get a free id: workflow, box node and ports", () => {
    let profile = sample();
    profile = doc.addNode(profile, "main", node("inner-2", []));
    profile = doc.addNode(profile, "main", node("two", [port("in", "input")]));
    profile = doc.addNode(profile, "main", node("three", [port("in", "input")]));
    profile = doc.addWire(profile, "main", "then", { node_id: "a", port_id: "tick" }, { node_id: "two", port_id: "in" });
    profile = doc.addWire(profile, "main", "then", { node_id: "a", port_id: "tick" }, { node_id: "three", port_id: "in" });
    const made = doc.createBoxFromSelection(profile, "main", ["two", "three"], "Inner")!;
    expect(made.workflowId).toBe("inner-2");
    // `inner-2` is a node that stays in main: the box takes the next id.
    expect(made.boxId).toBe("inner-2-2");
    expect(made.ports.map((p) => p.id)).toEqual(["in", "in-2"]);
    expect(doc.createBoxFromSelection(profile, "main", ["nope"], "x")).toBeNull();
    expect(doc.createBoxFromSelection(profile, "nope", ["a"], "x")).toBeNull();
  });
});

describe("replace dependency", () => {
  test("candidates offer the same capability as a uses output, other nodes and self inputs", () => {
    let profile = usesSample();
    profile = doc.addWorkflowPort(profile, "main", { id: "model", kind: "uses", capability: "llm.endpoint@1" }).profile;
    const found = doc.dependencyCandidates(profile, "main", { node_id: "ag", port_id: "provider" });
    expect(found.map((item) => `${item.end.node_id}.${item.end.port_id}`)).toEqual(["self.model", "p.endpoint", "p2.endpoint"]);
    expect(doc.dependencyCandidates(profile, "main", { node_id: "ag", port_id: "prompt" })).toEqual([]);
  });

  test("replacing rewires: the old wire goes, a uses wire from the new one comes", () => {
    const before = usesSample();
    const after = doc.replaceDependency(before, "main", { node_id: "ag", port_id: "provider" }, { node_id: "p2", port_id: "endpoint" });
    const into = doc.wiresInto(doc.workflowOf(after, "main")!, { node_id: "ag", port_id: "provider" });
    expect(into.map((w) => [w.kind, w.from.node_id])).toEqual([["uses", "p2"]]);
    // The same source again changes nothing.
    expect(doc.replaceDependency(after, "main", { node_id: "ag", port_id: "provider" }, { node_id: "p2", port_id: "endpoint" })).toBe(after);
    // An input that takes many: only the named wire is replaced.
    let many = doc.addNode(before, "main", node("tools2", [port("toolset", "output", "uses", "tool@1")]));
    many = doc.addNode(many, "main", node("tools3", [port("toolset", "output", "uses", "tool@1")]));
    many = doc.addWire(many, "main", "uses", { node_id: "tools2", port_id: "toolset" }, { node_id: "ag", port_id: "tools" });
    const named = doc.wiresInto(doc.workflowOf(many, "main")!, { node_id: "ag", port_id: "tools" }).find((w) => w.from.node_id === "tools")!;
    const swapped = doc.replaceDependency(many, "main", { node_id: "ag", port_id: "tools" }, { node_id: "tools3", port_id: "toolset" }, named.id);
    expect(doc.wiresInto(doc.workflowOf(swapped, "main")!, { node_id: "ag", port_id: "tools" }).map((w) => w.from.node_id).sort()).toEqual(["tools2", "tools3"]);
  });

  test("knownCapabilities lists capabilities of one kind, once", () => {
    expect(doc.knownCapabilities(usesSample(), [port("x", "input", "uses", "decision@1")], "uses")).toEqual(["agent.engine@1", "decision@1", "llm.endpoint@1", "text@1", "tool@1"]);
  });
});

describe("new profile ids", () => {
  test("empty, not file-safe or taken ids are refused", () => {
    expect(doc.profileIdProblem("", [])).toBe("empty");
    expect(doc.profileIdProblem("My Id", [])).toBe("invalid");
    expect(doc.profileIdProblem("../x", [])).toBe("invalid");
    expect(doc.profileIdProblem("demo", ["demo"])).toBe("taken");
    expect(doc.profileIdProblem("demo-2", ["demo"])).toBeNull();
  });
});
