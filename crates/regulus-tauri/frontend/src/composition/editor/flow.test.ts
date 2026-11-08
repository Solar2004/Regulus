import { describe, expect, test } from "vitest";
import { positionChanges, buildUnifiedFlow, buildWorkflowFlow, endOf, flowEdgeId, flowNodeId, nodeIdOf, SELF_IN, SELF_OUT, selfAnchors, severityOf, wireIdOf, type FlowOptions, type PieceNode } from "./flow";
import type { Diagnostic } from "../model";
import { sample } from "./fixtures.test-util";
import { moveNodes } from "./document";

const diagnostic = (patch: Partial<Diagnostic>): Diagnostic => ({ severity: "warning", code: "x", message: "m", workflow_id: null, node_id: null, wire_id: null, ...patch });
const options = (patch: Partial<FlowOptions> = {}): FlowOptions => ({
  modules: [{ id: "regulus/hello@0.1.0", name: "Hello", node_types: ["greeter"], workflow_id: null }],
  diagnostics: [], selection: { nodes: [], wires: [] }, measured: new Map(), anchors: null, nodeWidth: 250,
  labels: { uses: "Uses", then: "Then" }, ...patch,
});

describe("ids", () => {
  test("canvas ids map back to nodes, wires and wire ends", () => {
    expect(nodeIdOf(flowNodeId("a:b"))).toBe("a:b");
    expect(nodeIdOf(SELF_IN)).toBeNull();
    expect(wireIdOf(flowEdgeId("w1"))).toBe("w1");
    expect(endOf(flowNodeId("a"), "tick")).toEqual({ node_id: "a", port_id: "tick" });
    expect(endOf(SELF_OUT, "reply")).toEqual({ node_id: "self", port_id: "reply" });
    expect(endOf("a:main:a", "tick")).toBeNull();
    expect(endOf(flowNodeId("a"), null)).toBeNull();
  });
});

describe("one workflow", () => {
  test("nodes, self ports, wires, selection and states", () => {
    const { nodes, edges } = buildWorkflowFlow(sample(), "main", options({ selection: { nodes: ["a"], wires: ["w1"] } }));
    expect(nodes.map((node) => node.id)).toEqual(["n:a", "n:b", "n:c", "n:box", SELF_IN, SELF_OUT]);
    const pieces = nodes.filter((node): node is PieceNode => node.type === "piece");
    expect(pieces.map((node) => node.data.state)).toEqual(["installed", "not-installed", "not-installed", "installed"]);
    expect(pieces[0].selected).toBe(true);
    expect(pieces[3].data.ports.map((port) => port.id)).toEqual(["q", "r"]);
    expect(edges).toHaveLength(1);
    expect(edges[0]).toMatchObject({ id: "w:w1", source: "n:a", target: "n:b", sourceHandle: "tick", targetHandle: "in", selected: true, className: "wire-then", label: "Then" });
  });

  test("self wires join the workflow's own ports", () => {
    const { nodes, edges } = buildWorkflowFlow(sample(), "inner", options());
    expect(nodes.filter((node) => node.type === "self").map((node) => node.data)).toMatchObject([{ side: "in", ports: [{ id: "q" }] }, { side: "out", ports: [{ id: "r" }] }]);
    expect(edges.map((edge) => [edge.source, edge.target])).toEqual([[SELF_IN, "n:h"], ["n:h", SELF_OUT]]);
  });

  test("diagnostics mark the nodes and wires they name", () => {
    const diagnostics = [diagnostic({ node_id: "c", severity: "warning" }), diagnostic({ node_id: "c", severity: "error", workflow_id: "main" }), diagnostic({ node_id: "a", workflow_id: "inner" }), diagnostic({ wire_id: "w1", severity: "error" })];
    expect(severityOf(diagnostics, "main", "c")).toBe("error");
    expect(severityOf(diagnostics, "main", "a")).toBeNull();
    const { edges } = buildWorkflowFlow(sample(), "main", options({ diagnostics }));
    expect(edges[0].className).toBe("wire-then is-flagged");
  });

  test("a measured size is passed back; an unknown workflow draws nothing", () => {
    const measured = new Map([["n:a", { width: 250, height: 120 }]]);
    expect(buildWorkflowFlow(sample(), "main", options({ measured })).nodes[0].measured).toEqual({ width: 250, height: 120 });
    expect(buildWorkflowFlow(sample(), "nope", options())).toEqual({ nodes: [], edges: [] });
  });

  test("self ports sit left and right of the nodes", () => {
    const workflow = sample().workflows[0];
    expect(selfAnchors(workflow, 300)).toEqual({ in: { x: -300, y: 0 }, out: { x: 700, y: 0 } });
    expect(selfAnchors({ ...workflow, nodes: [] }, 300)).toEqual({ in: { x: 0, y: 0 }, out: { x: 600, y: 0 } });
  });
});

describe("unified view", () => {
  test("frames with read-only nodes and the wires between nodes", () => {
    const { nodes, edges } = buildUnifiedFlow(sample(), options());
    expect(nodes.filter((node) => node.type === "workflow").map((node) => node.id)).toEqual(["a:main", "a:inner"]);
    const pieces = nodes.filter((node): node is PieceNode => node.type === "piece");
    expect(pieces.every((node) => !node.data.editable && node.draggable === false)).toBe(true);
    expect(edges.map((edge) => edge.id)).toEqual(["a:main:w1"]);
  });
});

describe("render cost", () => {
  test("data of nodes that did not change is the same object, so memoized nodes skip rendering", () => {
    const cache = new Map();
    const base = sample();
    const before = buildWorkflowFlow(base, "main", options({ cache })).nodes;
    const after = buildWorkflowFlow(moveNodes(base, "main", [{ id: "a", position: { x: 9, y: 9 } }]), "main", options({ cache })).nodes;
    expect(after[0].data).not.toBe(before[0].data);
    expect(after[0].position).toEqual({ x: 9, y: 9 });
    expect(after.slice(1, 4).map((node) => node.data)).toEqual(before.slice(1, 4).map((node) => node.data));
    after.slice(1, 4).forEach((node, at) => expect(node.data).toBe(before[at + 1].data));
  });

  test("a node mid-drag is drawn where React Flow has it, without touching the document", () => {
    const profile = sample();
    const nodes = buildWorkflowFlow(profile, "main", options({ dragging: new Map([["a", { x: 500, y: 600 }]]) })).nodes;
    expect(nodes[0].position).toEqual({ x: 500, y: 600 });
    expect(profile.workflows[0].nodes[0].position).toEqual({ x: 100, y: 50 });
  });

  test("position changes split into mid-drag and to-write", () => {
    expect(positionChanges([
      { type: "position", id: "n:a", position: { x: 1, y: 2 }, dragging: true },
      { type: "position", id: "n:b", position: { x: 3, y: 4 }, dragging: false },
      { type: "position", id: "n:c", position: { x: 5, y: 6 } },
      { type: "position", id: "self:in", position: { x: 0, y: 0 } },
      { type: "position", id: "n:d" },
      { type: "select", id: "n:e" },
    ])).toEqual({ live: [{ id: "a", position: { x: 1, y: 2 } }], done: [{ id: "b", position: { x: 3, y: 4 } }, { id: "c", position: { x: 5, y: 6 } }] });
  });
});
