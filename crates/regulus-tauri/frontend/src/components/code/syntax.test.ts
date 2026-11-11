import { pythonLanguage } from "@codemirror/lang-python";
import { describe, expect, test } from "vitest";
import { lineColumn, offsetsOf, parserFor, syntaxProblems } from "./syntax";

const problems = (text: string) => syntaxProblems(pythonLanguage.parser.parse(text), text);

describe("python syntax problems", () => {
  test("valid code has none", () => {
    expect(problems("import regulus as rg\n\n@rg.node(\"t\")\ndef t(ctx, port, value):\n    ctx.emit(\"out\", value)\n\nrg.run(version=\"0.1.0\")\n")).toEqual([]);
  });

  test("an unclosed call is found on its line", () => {
    const text = "def t(ctx, port, value):\n    ctx.emit(\"out\", value\n";
    const found = problems(text);
    expect(found.length).toBeGreaterThan(0);
    expect(lineColumn(text, found[0].from).line).toBeGreaterThanOrEqual(2);
  });

  test("an unexpected token is named, cut short", () => {
    const text = "x = 1 +* 2\n";
    const found = problems(text);
    expect(found.length).toBeGreaterThan(0);
    expect(found.some((item) => item.kind === "unexpected" || item.kind === "missing")).toBe(true);
    for (const item of found) expect(item.text.length).toBeLessThanOrEqual(25);
  });

  test("errors that touch are one problem and the list has a limit", () => {
    const text = "def (:\n".repeat(200);
    const found = syntaxProblems(pythonLanguage.parser.parse(text), text, 10);
    expect(found.length).toBeLessThanOrEqual(10);
  });

  test("fixing the code clears the problems", () => {
    expect(problems("def f(:\n    pass\n").length).toBeGreaterThan(0);
    expect(problems("def f():\n    pass\n")).toEqual([]);
  });
});

describe("positions", () => {
  test("line and column are 1-based", () => {
    expect(lineColumn("ab\ncd", 0)).toEqual({ line: 1, column: 1 });
    expect(lineColumn("ab\ncd", 4)).toEqual({ line: 2, column: 2 });
  });

  test("a core problem lands on its line and column, clamped to the text", () => {
    const text = "a = 1\nb = (\n";
    expect(offsetsOf(text, { line: 2, column: 5 })).toEqual({ from: 10, to: 11 });
    expect(offsetsOf(text, { line: 2, column: 1, end_line: 2, end_column: 6 })).toEqual({ from: 6, to: 11 });
    // Past the end: the last line, never out of range.
    const far = offsetsOf(text, { line: 99, column: 99 });
    expect(far.from).toBeLessThanOrEqual(text.length);
    expect(far.to).toBeLessThanOrEqual(text.length);
  });

  test("core columns count code points, the editor counts UTF-16 units", () => {
    const text = "s = \"😀\" + x(\n";
    // Column 11 (1-based, code points) is `x`; in UTF-16 the emoji takes two units.
    expect(text.slice(offsetsOf(text, { line: 1, column: 11 }).from)[0]).toBe("x");
  });

  test("the language hint decides whether there is a parser", () => {
    expect(parserFor("python")).toBe("python");
    expect(parserFor(" Py ")).toBe("python");
    expect(parserFor("lua")).toBeUndefined();
    expect(parserFor(undefined)).toBeUndefined();
  });
});
