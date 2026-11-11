// Syntax problems read from a parse tree, as plain data. Nothing here runs the code: the
// tree comes from the editor's own incremental parser (Lezer), which only reads the text.
// The core's check, when there is one, is the authority; this gives feedback while typing.

/** Language hints the editor has a parser for; any other is plain text with line numbers. */
export const PARSED_LANGUAGES: Record<string, "python"> = { python: "python", py: "python", python3: "python" };
export const parserFor = (language: string | undefined) => language ? PARSED_LANGUAGES[language.trim().toLowerCase()] : undefined;

/** The part of a Lezer tree this needs: walking its nodes. */
export interface TreeLike {
  iterate(spec: { enter: (node: { type: { isError: boolean }; from: number; to: number }) => boolean | void }): void;
}

/** One problem: where it is, and whether something is missing there or unexpected. */
export interface SyntaxProblem {
  from: number;
  to: number;
  kind: "missing" | "unexpected";
  /** The unexpected text (first line, cut short), empty when something is missing. */
  text: string;
}

/** Characters of unexpected text shown in a message. */
export const SNIPPET_MAX = 24;

/**
 * The error nodes of `tree` over `doc`, in order. Errors that touch or overlap are one
 * problem, so a single slip does not flood the list; at most `limit` are returned.
 */
export function syntaxProblems(tree: TreeLike, doc: string, limit = 50): SyntaxProblem[] {
  const spans: { from: number; to: number }[] = [];
  tree.iterate({
    enter: (node) => {
      if (!node.type.isError) return;
      const last = spans[spans.length - 1];
      if (last && node.from <= last.to) last.to = Math.max(last.to, node.to);
      else spans.push({ from: node.from, to: node.to });
      // An error's children are part of it.
      return false;
    },
  });
  return spans.slice(0, limit).map(({ from, to }) => {
    const raw = doc.slice(from, to).split("\n")[0].trim();
    if (!raw) return { from, to, kind: "missing", text: "" };
    return { from, to, kind: "unexpected", text: raw.length > SNIPPET_MAX ? `${raw.slice(0, SNIPPET_MAX)}…` : raw };
  });
}

/** A 1-based line and column of offset `at` in `doc` (columns count UTF-16 units, as the editor does). */
export function lineColumn(doc: string, at: number): { line: number; column: number } {
  const before = doc.slice(0, Math.max(0, Math.min(at, doc.length)));
  const line = before.split("\n").length;
  return { line, column: before.length - (before.lastIndexOf("\n") + 1) + 1 };
}

/** A problem reported by line and column (the core's check), clamped to the text as it is now. */
export interface PlacedProblem {
  line: number;
  column?: number | null;
  end_line?: number | null;
  end_column?: number | null;
  message: string;
  severity?: "error" | "warning" | "info";
}

/**
 * Offsets for a problem given by 1-based line and column, columns in Unicode code points
 * (as the core counts them); lines past the end land on the last one.
 */
export function offsetsOf(doc: string, problem: Pick<PlacedProblem, "line" | "column" | "end_line" | "end_column">): { from: number; to: number } {
  const lines = doc.split("\n");
  const starts: number[] = [];
  let at = 0;
  for (const line of lines) { starts.push(at); at += line.length + 1; }
  const place = (line: number, column: number | null | undefined) => {
    const index = Math.min(Math.max(1, line), lines.length) - 1;
    const text = lines[index];
    const points = Math.max(0, (column ?? 1) - 1);
    // Code points to UTF-16 units: the same for plain text, longer past an emoji.
    const units = /[\uD800-\uDFFF]/.test(text) ? Array.from(text).slice(0, points).join("").length : points;
    return starts[index] + Math.min(units, text.length);
  };
  const from = place(problem.line, problem.column);
  const to = problem.end_line ? place(problem.end_line, problem.end_column) : from;
  return to > from ? { from, to } : { from, to: Math.min(doc.length, from + 1) };
}
