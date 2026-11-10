// What the plain text area of a `code` or `multiline` field does with Tab, Shift+Tab and its
// height, as pure functions. It is a text box, not a code editor: nothing here highlights,
// parses or checks the code.

/** A text with the caret or selection it leaves (`start` = `end` for a caret). */
export interface TextEdit { text: string; start: number; end: number }

/** The indentation a field asks for: `"tab"`, a number of spaces (1-8), or 4 spaces. */
export function indentUnit(hint: unknown): string {
  if (hint === "tab") return "\t";
  if (typeof hint === "number" && Number.isInteger(hint) && hint >= 1 && hint <= 8) return " ".repeat(hint);
  return "    ";
}

const lineStart = (text: string, at: number) => text.lastIndexOf("\n", at - 1) + 1;

/** The start of every line the selection touches; a selection ending at a line start leaves that line out. */
function touchedLines(text: string, start: number, end: number): number[] {
  const last = end > start && text[end - 1] === "\n" ? end - 1 : end;
  const starts = [lineStart(text, start)];
  for (let at = text.indexOf("\n", starts[0]); at !== -1 && at < last; at = text.indexOf("\n", at + 1)) starts.push(at + 1);
  return starts;
}

/** Tab: a caret or a selection within one line is replaced by one unit; a selection over lines indents each. */
export function indent(text: string, start: number, end: number, unit: string): TextEdit {
  if (start === end || !text.slice(start, end).includes("\n")) {
    const at = start + unit.length;
    return { text: text.slice(0, start) + unit + text.slice(end), start: at, end: at };
  }
  const lines = touchedLines(text, start, end);
  let out = text;
  for (const at of [...lines].reverse()) out = out.slice(0, at) + unit + out.slice(at);
  return { text: out, start: start + unit.length, end: end + unit.length * lines.length };
}

/** Shift+Tab: each touched line loses one unit of leading indentation (or what spaces it has, if fewer). */
export function outdent(text: string, start: number, end: number, unit: string): TextEdit {
  const lines = touchedLines(text, start, end);
  let out = text;
  let newStart = start;
  let newEnd = end;
  for (const at of [...lines].reverse()) {
    const eol = out.indexOf("\n", at);
    const lead = out.slice(at, Math.min(at + unit.length, eol === -1 ? out.length : eol));
    const cut = lead === unit ? unit.length : unit === "\t" ? 0 : lead.length - lead.replace(/^ +/, "").length;
    if (!cut) continue;
    out = out.slice(0, at) + out.slice(at + cut);
    newEnd -= Math.min(cut, Math.max(0, end - at));
    if (at < start) newStart -= Math.min(cut, start - at);
  }
  return { text: out, start: newStart, end: Math.max(newStart, newEnd) };
}

/** Visible rows: one per line of the value, kept between `min` and `max`. */
export const rowsFor = (text: string, min: number, max: number) => Math.min(max, Math.max(min, text.split("\n").length));
