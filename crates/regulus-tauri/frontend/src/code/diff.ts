// A unified diff as files, hunks and numbered lines. `code.inspect diff` gives one text for
// the whole working tree; the view splits it here, so it never asks per file. Pure: tested
// in diff.test.ts.

export type DiffLineKind = "context" | "add" | "remove" | "hunk" | "note";

export interface DiffLine {
  kind: DiffLineKind;
  /** The line without its `+`, `-` or space marker; a hunk header or a note as it came. */
  text: string;
  /** Line number in the old file, for context and removed lines. */
  oldNumber: number | null;
  /** Line number in the new file, for context and added lines. */
  newNumber: number | null;
}

export type DiffStatus = "modified" | "added" | "deleted" | "renamed" | "binary";

export interface DiffFile {
  /** The path the file has now (the old one for a deleted file). */
  path: string;
  /** Where it came from, when it was renamed. */
  oldPath: string | null;
  status: DiffStatus;
  additions: number;
  deletions: number;
  lines: DiffLine[];
}

const NOWHERE = "/dev/null";
const GIT_HEADER = "diff --git ";

/** A path of a diff header: without quotes, without a tab-separated date, without the `a/` or `b/` prefix. */
function headerPath(raw: string): string {
  const cut = raw.split("\t")[0].trim();
  const path = cut.length > 1 && cut.startsWith('"') && cut.endsWith('"') ? cut.slice(1, -1) : cut;
  return path === NOWHERE ? path : path.replace(/^[ab]\//, "");
}

/** The new path of `diff --git a/x b/y`, also when the paths are quoted or hold spaces. */
function gitHeaderPath(line: string): string {
  const rest = line.slice(GIT_HEADER.length);
  const quoted = rest.match(/^"(.+)" "(.+)"$/);
  if (quoted) return headerPath(quoted[2]);
  const middle = rest.indexOf(" b/");
  return middle < 0 ? rest : headerPath(rest.slice(middle + 1));
}

const newFile = (path: string): DiffFile => ({ path, oldPath: null, status: "modified", additions: 0, deletions: 0, lines: [] });

/** One line between a file's start and its first hunk, or between its hunks. */
function readHeader(file: DiffFile, line: string) {
  if (line.startsWith("--- ")) { if (headerPath(line.slice(4)) === NOWHERE) file.status = "added"; }
  else if (line.startsWith("+++ ")) {
    const to = headerPath(line.slice(4));
    if (to === NOWHERE) file.status = "deleted";
    else if (file.status !== "renamed") file.path = to;
  }
  else if (line.startsWith("new file mode")) file.status = "added";
  else if (line.startsWith("deleted file mode")) file.status = "deleted";
  else if (line.startsWith("rename from ")) { file.status = "renamed"; file.oldPath = headerPath(line.slice("rename from ".length)); }
  else if (line.startsWith("rename to ")) { file.status = "renamed"; file.path = headerPath(line.slice("rename to ".length)); }
  else if (line.startsWith("Binary files ") || line.startsWith("GIT binary patch")) file.status = "binary";
}

/**
 * Splits a unified diff (as `git diff` prints it, or plain `---`/`+++` pairs) into its
 * files. A hunk is as long as its header says, so a removed line that itself starts with
 * `-- ` is never taken for the next file.
 */
export function parseDiff(text: string): DiffFile[] {
  const files: DiffFile[] = [];
  let file: DiffFile | undefined;
  let oldNumber = 0;
  let newNumber = 0;
  // Lines the open hunk still owes, per side; both at zero: between hunks.
  let oldLeft = 0;
  let newLeft = 0;
  for (const line of text.split(/\r?\n/)) {
    if (file && (oldLeft > 0 || newLeft > 0)) {
      const mark = line[0];
      if (mark === "+") { file.additions += 1; newLeft -= 1; file.lines.push({ kind: "add", text: line.slice(1), oldNumber: null, newNumber: newNumber++ }); continue; }
      if (mark === "-") { file.deletions += 1; oldLeft -= 1; file.lines.push({ kind: "remove", text: line.slice(1), oldNumber: oldNumber++, newNumber: null }); continue; }
      if (mark === "\\") { file.lines.push({ kind: "note", text: line.slice(1).trim(), oldNumber: null, newNumber: null }); continue; }
      // A context line; one that lost its leading space (an empty line) counts too.
      if (mark === " " || line === "") { oldLeft -= 1; newLeft -= 1; file.lines.push({ kind: "context", text: line.slice(1), oldNumber: oldNumber++, newNumber: newNumber++ }); continue; }
      // Anything else: the hunk was shorter than its header said.
      oldLeft = 0;
      newLeft = 0;
    }
    if (line.startsWith(GIT_HEADER)) { file = newFile(gitHeaderPath(line)); files.push(file); continue; }
    const hunk = line.match(/^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/);
    if (hunk && file) {
      oldNumber = Number(hunk[1]);
      newNumber = Number(hunk[3]);
      oldLeft = hunk[2] === undefined ? 1 : Number(hunk[2]);
      newLeft = hunk[4] === undefined ? 1 : Number(hunk[4]);
      file.lines.push({ kind: "hunk", text: line, oldNumber: null, newNumber: null });
      continue;
    }
    if (file && line.startsWith("\\")) { file.lines.push({ kind: "note", text: line.slice(1).trim(), oldNumber: null, newNumber: null }); continue; }
    // A diff without `diff --git` lines starts a file at each `---` that follows a hunk.
    if (line.startsWith("--- ") && (!file || file.lines.length > 0)) { file = newFile(headerPath(line.slice(4))); files.push(file); }
    if (file) readHeader(file, line);
  }
  return files;
}

export interface DiffStats {
  files: number;
  additions: number;
  deletions: number;
}

export function diffStats(files: DiffFile[]): DiffStats {
  return files.reduce((sum, file) => ({ files: sum.files + 1, additions: sum.additions + file.additions, deletions: sum.deletions + file.deletions }), { files: 0, additions: 0, deletions: 0 });
}
