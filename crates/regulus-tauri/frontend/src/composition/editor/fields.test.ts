import { describe, expect, test } from "vitest";
import type { ManifestField } from "../model";
import { coerce, controlFor, defaultsOf, displayValue, fieldRows, isSecret, mapEntries, mapFrom, resetValue } from "./fields";

const fields: ManifestField[] = [
  { id: "path", type: "text", default: "output.md" },
  { id: "mode", type: "select", default: "overwrite", options: ["overwrite", "append"] },
  { id: "max_bytes", type: "number", default: 1048576 },
  { id: "env", type: "map", default: {} },
  { id: "workflow", type: "workflow", default: "" },
  { id: "cases", type: "rules", default: [] },
  { id: "verbose", type: "boolean", default: false },
  { id: "token", type: "text", default: null },
];

describe("controls", () => {
  test("each type gets its control; unknown types are raw text", () => {
    expect(fields.map(controlFor)).toEqual(["text", "select", "number", "map", "workflow", "raw", "select", "text"]);
    expect(controlFor({ id: "x", type: "text", default: true })).toBe("select");
    expect(controlFor({ id: "x", type: "weird", options: ["a"] })).toBe("select");
  });
});

describe("rows", () => {
  test("manifest fields in order, then config keys it does not declare, as raw", () => {
    const rows = fieldRows(fields, { path: "a.md", extra: 3, mode: "overwrite" });
    expect(rows.map((row) => row.id)).toEqual(["path", "mode", "max_bytes", "env", "workflow", "cases", "verbose", "token", "extra"]);
    expect(rows[0]).toMatchObject({ value: "a.md", defaultValue: "output.md", isDefault: false });
    expect(rows[1]).toMatchObject({ isDefault: true });
    expect(rows[2]).toMatchObject({ value: undefined, isDefault: true });
    expect(rows[6].options).toEqual([true, false]);
    expect(rows[7].defaultValue).toBeUndefined();
    expect(rows[rows.length - 1]).toMatchObject({ id: "extra", type: null, control: "raw", value: 3, isDefault: false });
  });

  test("no manifest: every config key is raw", () => {
    expect(fieldRows(undefined, { a: "x" })).toEqual([{ id: "a", type: null, control: "raw", value: "x", defaultValue: undefined, options: [], isDefault: false, secret: false }]);
  });

  test("secret references are marked as such", () => {
    expect(isSecret("secret:openai")).toBe(true);
    expect(isSecret("openai")).toBe(false);
    expect(fieldRows(fields, { path: "secret:key" })[0].secret).toBe(true);
  });
});

describe("defaults and reset", () => {
  test("defaultsOf gives every non-null default, as copies", () => {
    const config = defaultsOf(fields);
    expect(config).toEqual({ path: "output.md", mode: "overwrite", max_bytes: 1048576, env: {}, workflow: "", cases: [], verbose: false });
    expect(config.env).not.toBe(fields[3].default);
    expect(defaultsOf(undefined)).toEqual({});
  });

  test("reset writes the default, or removes a key without one", () => {
    const rows = fieldRows(fields, { path: "x", token: "y", extra: 1 });
    expect(resetValue(rows[0])).toBe("output.md");
    expect(resetValue(rows[7])).toBeUndefined();
    expect(resetValue(rows[rows.length - 1])).toBeUndefined();
  });
});

describe("coerce", () => {
  const rows = fieldRows(fields, { extra: 3, label: "x", list: [1] });
  const row = (id: string) => rows.find((item) => item.id === id)!;

  test("numbers parse or fail", () => {
    expect(coerce(row("max_bytes"), "42")).toEqual({ ok: true, value: 42 });
    expect(coerce(row("max_bytes"), "4.5e3")).toEqual({ ok: true, value: 4500 });
    expect(coerce(row("max_bytes"), "")).toEqual({ ok: false, error: "number" });
    expect(coerce(row("max_bytes"), "12abc")).toEqual({ ok: false, error: "number" });
  });

  test("a select keeps the option's own type", () => {
    expect(coerce(row("mode"), "append")).toEqual({ ok: true, value: "append" });
    expect(coerce(row("verbose"), "true")).toEqual({ ok: true, value: true });
    expect(coerce(row("mode"), "nope")).toEqual({ ok: false, error: "option" });
  });

  test("raw text stays text for strings and is JSON otherwise", () => {
    expect(coerce(row("label"), "y")).toEqual({ ok: true, value: "y" });
    expect(coerce(row("extra"), "7")).toEqual({ ok: true, value: 7 });
    expect(coerce(row("list"), "[1,2]")).toEqual({ ok: true, value: [1, 2] });
    expect(coerce(row("list"), "[1,")).toEqual({ ok: false, error: "json" });
    expect(coerce(row("extra"), "null")).toEqual({ ok: false, error: "json" });
    expect(coerce(row("cases"), "[]")).toEqual({ ok: true, value: [] });
  });

  test("text is taken as typed", () => expect(coerce(row("path"), " a ")).toEqual({ ok: true, value: " a " }));
});

describe("maps", () => {
  test("rows from a map and back", () => {
    expect(mapEntries({ A: "1", B: 2 })).toEqual([["A", "1"], ["B", "2"]]);
    expect(mapEntries("x")).toEqual([]);
    expect(mapFrom([["A", "1"], [" ", "dropped"], ["B ", "2"], ["A", "3"]])).toEqual({ A: "3", B: "2" });
  });
  test("displayValue", () => {
    expect(displayValue(undefined)).toBe("");
    expect(displayValue("a")).toBe("a");
    expect(displayValue([1])).toBe("[1]");
    expect(displayValue(false)).toBe("false");
  });
});
