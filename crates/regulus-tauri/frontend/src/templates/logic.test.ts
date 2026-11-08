import { expect, test } from "vitest";
import type { ModuleSummary } from "@/operate/model";
import type { TemplateSummary } from "./model";
import { applyErrorOf, canApply, initialValues, missingVars, requireState, varsToSend } from "./logic";

const vars = [
  { name: "who", description: "who is greeted" },
  { name: "file", description: "file", default: "greeting.txt" },
  { name: "max_turns", description: "turns", default: "5" },
];
const template: TemplateSummary = { name: "greeting", description: "", path: "/t/greeting.toml", vars, requires: [] };

test("the form opens with defaults filled and required vars empty", () => {
  expect(initialValues(vars)).toEqual({ who: "", file: "greeting.txt", max_turns: "5" });
});

test("required vars must be filled; optional ones may be emptied", () => {
  expect(missingVars(vars, initialValues(vars))).toEqual(["who"]);
  expect(missingVars(vars, { who: "  ", file: "" })).toEqual(["who"]);
  expect(missingVars(vars, { who: "Ana", file: "" })).toEqual([]);
  expect(canApply(template, { who: "Ana" })).toBe(true);
  expect(canApply(template, {})).toBe(false);
  expect(canApply({ ...template, error: "bad toml" }, { who: "Ana" })).toBe(false);
});

test("an emptied optional var is left to the core's default", () => {
  expect(varsToSend(vars, { who: "Ana", file: "", max_turns: "2" })).toEqual({ who: "Ana", max_turns: "2" });
});

test("requires are installed, missing or broken by module state", () => {
  const modules: ModuleSummary[] = [
    { id: "regulus/hello@0.1.0", name: "Hello", node_types: [], workflow_id: null, state: { status: "idle", last_used: null } },
    { id: "regulus/files@0.1.0", name: "Files", node_types: [], workflow_id: null, state: { status: "broken", reason: "x" } },
  ];
  expect(requireState({ module: "regulus/hello", version: ">=0.1" }, modules)).toBe("installed");
  expect(requireState({ module: "regulus/files", version: ">=0.1" }, modules)).toBe("broken");
  expect(requireState({ module: "regulus/agent", version: ">=0.1" }, modules)).toBe("missing");
});

test("a blocked module (an untrusted project's local module) is not usable", () => {
  const modules: ModuleSummary[] = [
    { id: "demo/echo@0.1.0", name: "Echo", node_types: ["echo"], workflow_id: null, state: { status: "blocked", reason: "untrusted project: reopen with --trust" } },
  ];
  expect(requireState({ module: "demo/echo", version: ">=0.1" }, modules)).toBe("broken");
});

test("core errors map to what the form says", () => {
  expect(applyErrorOf({ code: -32006 })).toBe("unmet");
  expect(applyErrorOf({ code: -32007 })).toBe("exists");
  expect(applyErrorOf({ code: -32602 })).toBe("params");
  expect(applyErrorOf({ code: -1 })).toBe("other");
  expect(applyErrorOf(new Error("x"))).toBe("other");
});
