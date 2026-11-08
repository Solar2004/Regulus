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

  test("removeWorkflow refuses main and unhooks the boxes", () => {
    const before = sample();
    expect(doc.removeWorkflow(before, "main")).toBe(before);
    const after = doc.removeWorkflow(before, "inner");
    expect(after.workflows.map((w) => w.id)).toEqual(["main"]);
    expect(after.workflows[0].nodes[3].internal_workflow_id).toBeNull();
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

describe("new profile ids", () => {
  test("empty, not file-safe or taken ids are refused", () => {
    expect(doc.profileIdProblem("", [])).toBe("empty");
    expect(doc.profileIdProblem("My Id", [])).toBe("invalid");
    expect(doc.profileIdProblem("../x", [])).toBe("invalid");
    expect(doc.profileIdProblem("demo", ["demo"])).toBe("taken");
    expect(doc.profileIdProblem("demo-2", ["demo"])).toBeNull();
  });
});

describe("free position", () => {
  test("a click-added node does not land on another", () => {
    const main = sample().workflows[0];
    expect(doc.freePosition(main, { x: 1000, y: 1000 }, { x: 290, y: 40 })).toEqual({ x: 1000, y: 1000 });
    // a sits at 100,50 and b at 400,80: one step clears a, a second clears b.
    expect(doc.freePosition(main, { x: 110, y: 60 }, { x: 290, y: 40 })).toEqual({ x: 690, y: 60 });
  });
});
