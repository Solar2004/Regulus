import { describe, expect, test } from "vitest";
import { RpcError } from "@/core/connection";
import type { ManifestNodeType, ModuleDefinition, ProfileSynced, ProjectReloaded } from "../model";
import { node, port, sample } from "../editor/fixtures.test-util";
import { CODE, conflictOf, inUseOf } from "./api";
import { applySync, changedKeys, hasShape, localIdOf, projectOf, reloadSummary, samePorts, shapeFields, sourceOf, syncCounts, syncFits } from "./logic";

const modules: ModuleDefinition[] = [
  { id: "regulus/hello@0.1.0", name: "Hello", node_types: ["greeter"], workflow_id: null },
  { id: "demo/caps@0.2.0", name: "Caps", node_types: ["caps"], workflow_id: null, project: "demo" },
  { id: "demo/old@0.1.0", name: "Old", node_types: ["old"], workflow_id: null, project: "demo" },
];

describe("sources", () => {
  test("a local id is what follows the project", () => {
    expect(localIdOf("demo/caps@0.2.0")).toBe("caps");
    expect(localIdOf("demo/caps")).toBe("caps");
    expect(localIdOf("caps")).toBe("caps");
  });

  test("only a local module whose project lists a file for it is editable, by data and never by id", () => {
    const sources = [{ module: "caps", path: "tools/caps/run.py", error: null }, { module: "old", path: null, error: "no .py in command" }];
    expect(sourceOf(modules, "demo/caps@0.1.0", sources)).toEqual({ projectId: "demo", module: "caps", path: "tools/caps/run.py", moduleId: "demo/caps@0.2.0" });
    expect(sourceOf(modules, "demo/old", sources)).toBeNull();
    expect(sourceOf(modules, "demo/caps", undefined)).toBeNull();
    expect(sourceOf(modules, "regulus/hello@0.1.0", sources)).toBeNull();
    expect(sourceOf(modules, null, sources)).toBeNull();
    expect(projectOf(modules, "demo/caps")).toBe("demo");
    expect(projectOf(modules, "regulus/hello@0.1.0")).toBeNull();
  });
});

describe("shape", () => {
  const split: ManifestNodeType = {
    type: "split", name: "Split",
    port: [port("in", "input")],
    dynamic_port: [
      { ...port("out_{item}", "output"), repeat: "lanes" },
      { ...port("debug", "output"), when: { field: "mode", equals: "debug" } },
    ],
    field: [{ id: "mode", type: "select", options: ["fast", "debug"] }, { id: "lanes", type: "list" }, { id: "level", type: "number", when: { field: "verbose" } }],
  };

  test("the fields a shape reads are those its conditions and repeats name", () => {
    expect([...shapeFields(split)].sort()).toEqual(["lanes", "mode", "verbose"]);
    expect(shapeFields({ type: "t", name: "T", port: [port("in", "input")] }).size).toBe(0);
  });

  test("a type has a shape when it is managed or its ports depend on fields", () => {
    expect(hasShape(split)).toBe(true);
    expect(hasShape({ type: "t", name: "T", managed_ports: true })).toBe(true);
    expect(hasShape({ type: "t", name: "T", port: [port("in", "input")] })).toBe(false);
    expect(hasShape(null)).toBe(false);
  });

  test("ports compare with the core's defaults written out, names included", () => {
    expect(samePorts([port("a", "input")], [{ ...port("a", "input"), cardinality: "one", lazy: false }])).toBe(true);
    expect(samePorts([port("a", "input")], [{ ...port("a", "input"), name: "A" }])).toBe(false);
    // An empty name is the id, as the core fills it when a profile loads.
    expect(samePorts([{ ...port("a", "input"), name: "" }], [port("a", "input")])).toBe(true);
    expect(samePorts([port("a", "input"), port("b", "output")], [port("b", "output"), port("a", "input")])).toBe(false);
    expect(changedKeys(port("a", "input"), { ...port("a", "input"), capability: "json@1", required: true })).toEqual(["capability", "required"]);
    expect(changedKeys(port("a", "input"), { ...port("a", "input"), name: "" })).toEqual([]);
  });
});

describe("applying a sync preview", () => {
  function preview(): ProfileSynced {
    const profile = sample();
    // The core's answer: `a` loses `tick` (and the wire w1) and gains `tock`.
    const a = profile.workflows[0].nodes.find((item) => item.id === "a")!;
    a.ports = [port("tock", "output"), ...a.ports.filter((item) => item.id !== "tick")];
    profile.workflows[0].wires = [];
    return {
      profile, drops_wires: true,
      nodes: [{
        workflow_id: "main", node_id: "a", module_id: "regulus/hello@0.1.0", node_type: "greeter",
        added: [port("tock", "output")], removed: [port("tick", "output")], changed: [],
        wires_removed: [{ wire: sample().workflows[0].wires[0], reason: "port `tick` removed" }], problems: [],
      }],
    };
  }

  test("the listed node takes the new ports and the listed wire goes", () => {
    const current = sample();
    const next = applySync(current, preview());
    const a = next.workflows[0].nodes.find((item) => item.id === "a")!;
    expect(a.ports.map((item) => item.id)).toEqual(["tock", "echo", "greeting"]);
    expect(next.workflows[0].wires).toEqual([]);
    expect(current.workflows[0].wires).toHaveLength(1);
  });

  test("edits elsewhere since the preview are kept", () => {
    const current = sample();
    current.workflows[0].nodes.push(node("late", [port("in", "input")]));
    current.workflows[0].wires.push({ id: "w2", kind: "then", from: { node_id: "a", port_id: "tick" }, to: { node_id: "late", port_id: "in" } });
    const next = applySync(current, preview());
    expect(next.workflows[0].nodes.some((item) => item.id === "late")).toBe(true);
    // w2 was not in the preview: only the core's list is dropped (profile.check flags the rest).
    expect(next.workflows[0].wires.map((wire) => wire.id)).toEqual(["w2"]);
  });

  test("limited to other nodes, or with nothing to change, the profile is the same object", () => {
    const current = sample();
    expect(applySync(current, preview(), [{ workflow_id: "main", node_id: "b" }])).toBe(current);
    expect(applySync(current, { profile: current, drops_wires: false, nodes: [] })).toBe(current);
  });

  test("counts say what a preview does", () => {
    expect(syncCounts(preview())).toEqual({ nodes: 1, added: 1, removed: 1, changed: 0, wires: 1 });
  });
});

describe("reload results", () => {
  const result: ProjectReloaded = {
    project: { id: "demo", name: "Demo", path: "/p/demo" },
    modules: [
      { local_id: "caps", id: "demo/caps@0.2.0", previous: "demo/caps@0.1.0", outcome: "replaced", state: { status: "idle", last_used: null }, error: null },
      { local_id: "bad", id: "demo/bad@0.1.0", previous: "demo/bad@0.1.0", outcome: "kept", state: { status: "idle", last_used: null }, error: "SyntaxError" },
    ],
    stopped_executions: [],
    stale: [{ profile_id: "p", workflow_id: "main", node_id: "a" }, { profile_id: "q", workflow_id: "main", node_id: "x" }],
  };

  test("a kept old module is a failure with its reason, and stale nodes are those of the open profile", () => {
    const summary = reloadSummary(result, "p");
    expect(summary.ok).toBe(false);
    expect(summary.failed.map((item) => item.local_id)).toEqual(["bad"]);
    expect(summary.stale).toEqual([{ profile_id: "p", workflow_id: "main", node_id: "a" }]);
    expect(reloadSummary({ ...result, modules: [result.modules[0]] }, null)).toMatchObject({ ok: true, stale: [] });
  });
});

describe("error data", () => {
  test("a module in use carries its executions; other errors are not that", () => {
    const executions = [{ id: "exec-1", profile_id: "p", workflow_id: "main", nodes: ["a"] }];
    expect(inUseOf(new RpcError("in use", CODE.moduleInUse, { executions }))).toEqual({ executions });
    expect(inUseOf(new RpcError("in use", CODE.moduleInUse))).toEqual({ executions: [] });
    expect(inUseOf(new RpcError("other", CODE.invalidParams))).toBeNull();
    expect(inUseOf(new Error("plain"))).toBeNull();
  });

  test("a conflict carries the file's current revision, or null when it is gone", () => {
    expect(conflictOf(new RpcError("stale", CODE.sourceConflict, { revision: "r2", path: "tools/caps/run.py" }))).toEqual({ revision: "r2", path: "tools/caps/run.py" });
    expect(conflictOf(new RpcError("stale", CODE.sourceConflict, { revision: null }))).toEqual({ revision: null, path: null });
    expect(conflictOf(new RpcError("no", CODE.sourceUnavailable))).toBeUndefined();
  });
});

describe("where a sync preview may land", () => {
  test("only on the very draft it was asked for; an edit since asks again, another profile never takes it", () => {
    const asked = sample();
    const origin = { profileId: asked.id, doc: asked };
    expect(syncFits(origin, asked)).toBe("fits");
    // The same profile, edited while the core answered (a field, a wire): a fresh preview is needed.
    expect(syncFits(origin, { ...asked, name: "edited since" })).toBe("changed");
    // Another profile with the same workflow and node ids would otherwise take the ports.
    const other = { ...structuredClone(asked), id: "other" };
    expect(syncFits(origin, other)).toBe("other");
    expect(syncFits(origin, null)).toBe("other");
  });
});
