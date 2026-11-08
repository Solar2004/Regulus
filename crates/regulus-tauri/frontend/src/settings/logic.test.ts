import { describe, expect, test } from "vitest";
import type { ModuleDefinition, ModuleState } from "@/composition/model";
import type { KeyRecord, LogEntry } from "./model";
import {
  emptyPolicy, isRevoked, logQuery, logRows, moduleRows, moduleStatus, parsePolicy, parseScope, policyLimits,
  scopeEntryValid, secretNameProblem, sortKeys, sortNames, statusClass, statusCounts,
} from "./logic";

describe("secret names", () => {
  test("take what the core takes: letters, digits, dot, dash and underscore", () => {
    expect(secretNameProblem("")).toBe("empty");
    expect(secretNameProblem("anthropic-api_key.v2")).toBeNull();
    expect(secretNameProblem("has space")).toBe("invalid");
    expect(secretNameProblem("slash/name")).toBe("invalid");
    expect(secretNameProblem("ñ")).toBe("invalid");
  });
  test("list in a stable order", () => {
    expect(sortNames(["b", "a", "c"])).toEqual(["a", "b", "c"]);
  });
});

describe("key scopes", () => {
  test("an entry is a project, a project and a workflow, global or *", () => {
    for (const entry of ["*", "global", "acme", "acme/summarize", "*/summarize", "acme/*"]) expect(scopeEntryValid(entry)).toBe(true);
    for (const entry of ["/x", "acme/", "a/b/c", ""]) expect(scopeEntryValid(entry)).toBe(false);
  });
  test("a line splits on commas and spaces, keeps each entry once and names the bad ones", () => {
    expect(parseScope(" acme/summarize, global  acme/summarize ")).toEqual({ ok: true, scope: ["acme/summarize", "global"] });
    expect(parseScope(" , ")).toEqual({ ok: false, problem: "empty" });
    expect(parseScope("acme, a/b/c, /x")).toEqual({ ok: false, problem: "invalid", bad: ["a/b/c", "/x"] });
  });
});

describe("key policies", () => {
  test("blank fields are no limit and numbers are whole and not negative", () => {
    expect(parsePolicy({})).toEqual({ ok: true, policy: emptyPolicy });
    expect(parsePolicy({ cooldown_ms: " 500 ", concurrency: "2" })).toEqual({ ok: true, policy: { ...emptyPolicy, cooldown_ms: 500, concurrency: 2 } });
    expect(parsePolicy({ budget_ms: "1.5" })).toEqual({ ok: false, problem: "number", field: "budget_ms" });
    expect(parsePolicy({ requests: "-1", window_ms: "10" })).toEqual({ ok: false, problem: "number", field: "requests" });
    expect(parsePolicy({ concurrency: "99999999999999999999" })).toEqual({ ok: false, problem: "number", field: "concurrency" });
  });
  test("requests count per window: one without the other is refused", () => {
    expect(parsePolicy({ requests: "5" })).toEqual({ ok: false, problem: "window" });
    expect(parsePolicy({ window_ms: "1000" })).toEqual({ ok: false, problem: "window" });
    expect(parsePolicy({ requests: "5", window_ms: "1000" })).toMatchObject({ ok: true, policy: { requests: 5, window_ms: 1000 } });
  });
  test("limits drop the zeros and keep the CLI's order", () => {
    expect(policyLimits(undefined)).toEqual([]);
    expect(policyLimits({ budget_ms: 9, requests: 0, cooldown_ms: 3 })).toEqual([{ field: "cooldown_ms", value: 3 }, { field: "budget_ms", value: 9 }]);
  });
});

const key = (id: string, created_ms: number, revoked_ms?: number | null): KeyRecord => ({ id, name: id, scope: ["*"], hint: "rgk_abcdef", created_ms, revoked_ms });

describe("keys", () => {
  test("revoked when the core gives a time; absent or null is live", () => {
    expect(isRevoked(key("a", 1))).toBe(false);
    expect(isRevoked(key("a", 1, null))).toBe(false);
    expect(isRevoked(key("a", 1, 5))).toBe(true);
  });
  test("live first, newest first", () => {
    expect(sortKeys([key("old", 1), key("gone", 9, 10), key("new", 5)]).map((item) => item.id)).toEqual(["new", "old", "gone"]);
  });
});

const entry = (at_ms: number, status: number): LogEntry => ({ at_ms, project: "acme", workflow: "summarize", mode: "sync", status, ms: 3, result: "ok" });

describe("service log", () => {
  test("statuses fall in answered, refused and failed", () => {
    expect([200, 202, 401, 429, 500, 504].map(statusClass)).toEqual(["ok", "ok", "refused", "refused", "failed", "failed"]);
  });
  test("rows are newest first and of the chosen class", () => {
    const entries = [entry(1, 200), entry(2, 429), entry(3, 502), entry(4, 200)];
    expect(logRows(entries, "all").map((row) => row.at_ms)).toEqual([4, 3, 2, 1]);
    expect(logRows(entries, "ok").map((row) => row.at_ms)).toEqual([4, 1]);
    expect(logRows(entries, "refused").map((row) => row.at_ms)).toEqual([2]);
    expect(entries.map((row) => row.at_ms)).toEqual([1, 2, 3, 4]);
  });
  test("the query leaves out blanks and a last that is not a whole number above 0", () => {
    expect(logQuery({ last: "", key: " ", workflow: "" })).toEqual({});
    expect(logQuery({ last: "50", key: "k-1", workflow: " summarize " })).toEqual({ last: 50, key: "k-1", workflow: "summarize" });
    expect(logQuery({ last: "0", key: "", workflow: "" })).toEqual({});
    expect(logQuery({ last: "2.5", key: "", workflow: "" })).toEqual({});
  });
});

const module = (id: string, state?: ModuleState): ModuleDefinition => ({ id, name: id, node_types: [], workflow_id: null, state });

describe("module states", () => {
  test("each state carries its reason; idle never used is its own state", () => {
    expect(moduleStatus(module("a", { status: "active", since: 7 }))).toEqual({ kind: "active", since: 7 });
    expect(moduleStatus(module("a", { status: "idle", last_used: null }))).toEqual({ kind: "unused" });
    expect(moduleStatus(module("a", { status: "idle", last_used: 9 }))).toEqual({ kind: "idle", lastUsed: 9 });
    expect(moduleStatus(module("a", { status: "disabled", missing: ["engine"] }))).toEqual({ kind: "disabled", missing: ["engine"] });
    expect(moduleStatus(module("a", { status: "broken", reason: "init failed" }))).toEqual({ kind: "broken", reason: "init failed" });
    expect(moduleStatus(module("a", { status: "incompatible", requires: "^2", have: "1.9" }))).toEqual({ kind: "incompatible", requires: "^2", have: "1.9" });
  });
  test("a module without state reads as installed and never used", () => {
    expect(moduleStatus(module("a"))).toEqual({ kind: "unused" });
  });
  test("rows put what does not work first, then by name, and counts add up", () => {
    const rows = moduleRows([
      module("zeta", { status: "active", since: 1 }),
      module("alpha", { status: "idle", last_used: null }),
      module("mid", { status: "broken", reason: "x" }),
      module("beta", { status: "active", since: 2 }),
    ]);
    expect(rows.map((row) => row.id)).toEqual(["mid", "alpha", "beta", "zeta"]);
    expect(statusCounts(rows)).toEqual({ broken: 1, unused: 1, active: 2 });
  });
});
