import { describe, expect, test } from "vitest";
import { indent, indentUnit, outdent, rowsFor } from "./textarea";

const U = "    ";

describe("indent unit", () => {
  test("tab, 1-8 spaces, or four spaces", () => {
    expect(indentUnit("tab")).toBe("\t");
    expect(indentUnit(2)).toBe("  ");
    expect(indentUnit(8)).toBe(" ".repeat(8));
    for (const odd of [undefined, 0, 9, 2.5, "2", null]) expect(indentUnit(odd)).toBe(U);
  });
});

describe("indent", () => {
  test("a caret gets one unit and moves past it", () => {
    expect(indent("ab", 1, 1, U)).toEqual({ text: "a    b", start: 5, end: 5 });
  });

  test("a selection inside one line is replaced, like typing", () => {
    expect(indent("abcd", 1, 3, "\t")).toEqual({ text: "a\td", start: 2, end: 2 });
  });

  test("a selection over lines indents each touched line and keeps it selected", () => {
    const text = "if x:\nreturn 1\nreturn 2";
    const out = indent(text, 7, text.length, U);
    expect(out.text).toBe("if x:\n    return 1\n    return 2");
    expect(out.text.slice(out.start, out.end)).toBe("eturn 1\n    return 2");
  });

  test("a selection that ends at the start of a line leaves that line alone", () => {
    const text = "a\nb\nc";
    expect(indent(text, 0, 4, "  ").text).toBe("  a\n  b\nc");
  });
});

describe("outdent", () => {
  test("removes one unit from each touched line and keeps the selection on the same text", () => {
    const text = "def f():\n        a\n    b";
    const out = outdent(text, 9, text.length, U);
    expect(out.text).toBe("def f():\n    a\nb");
    expect(out.text.slice(out.start, out.end)).toBe("    a\nb");
  });

  test("a caret in an indented line pulls the line back and the caret with it", () => {
    expect(outdent("x\n    y", 7, 7, U)).toEqual({ text: "x\ny", start: 3, end: 3 });
  });

  test("fewer spaces than a unit are removed; never a newline or text", () => {
    expect(outdent("  a", 3, 3, U)).toEqual({ text: "a", start: 1, end: 1 });
    expect(outdent("  \nb", 0, 4, U).text).toBe("\nb");
    expect(outdent("abc", 1, 1, U)).toEqual({ text: "abc", start: 1, end: 1 });
  });

  test("with tabs only a whole tab is removed", () => {
    expect(outdent("\ta\n  b", 0, 6, "\t").text).toBe("a\n  b");
  });
});

test("rows follow the value's lines between the bounds", () => {
  expect(rowsFor("", 3, 10)).toBe(3);
  expect(rowsFor("a\nb\nc\nd", 3, 10)).toBe(4);
  expect(rowsFor("\n".repeat(40), 3, 10)).toBe(10);
});
