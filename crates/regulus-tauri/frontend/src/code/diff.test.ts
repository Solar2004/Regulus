import { describe, expect, test } from "vitest";
import { diffStats, parseDiff } from "./diff";

const git = `diff --git a/src/app.ts b/src/app.ts
index 1a2b3c4..5d6e7f8 100644
--- a/src/app.ts
+++ b/src/app.ts
@@ -1,4 +1,5 @@
 import { run } from "./run";
-const port = 80;
+const port = 8080;
+const host = "127.0.0.1";
 
 run(port);
@@ -10,2 +11,2 @@ export function stop() {
-  halt();
+  halt(true);
 }
diff --git a/notes.md b/notes.md
new file mode 100644
index 0000000..e69de29
--- /dev/null
+++ b/notes.md
@@ -0,0 +1,2 @@
+# Notes
+Nothing urgent.
diff --git a/old.txt b/old.txt
deleted file mode 100644
index e69de29..0000000
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-gone
\\ No newline at end of file
diff --git a/a name.ts b/b name.ts
similarity index 90%
rename from a name.ts
rename to b name.ts
index 1111111..2222222 100644
--- a/a name.ts
+++ b/b name.ts
@@ -1 +1 @@
-x
+y
diff --git a/logo.png b/logo.png
index 3333333..4444444 100644
Binary files a/logo.png and b/logo.png differ
`;

describe("a unified diff as files", () => {
  const files = parseDiff(git);

  test("one file per diff header, with what happened to it", () => {
    expect(files.map((file) => [file.path, file.status, file.additions, file.deletions])).toEqual([
      ["src/app.ts", "modified", 3, 2], ["notes.md", "added", 2, 0], ["old.txt", "deleted", 0, 1], ["b name.ts", "renamed", 1, 1], ["logo.png", "binary", 0, 0],
    ]);
    expect(files[3].oldPath).toBe("a name.ts");
    expect(diffStats(files)).toEqual({ files: 5, additions: 6, deletions: 4 });
  });

  test("lines carry the number they have on each side", () => {
    const [first] = files;
    expect(first.lines.map((line) => [line.kind, line.oldNumber, line.newNumber])).toEqual([
      ["hunk", null, null], ["context", 1, 1], ["remove", 2, null], ["add", null, 2], ["add", null, 3], ["context", 3, 4], ["context", 4, 5],
      ["hunk", null, null], ["remove", 10, null], ["add", null, 11], ["context", 11, 12],
    ]);
    expect(first.lines[3].text).toBe("const port = 8080;");
    // The empty context line kept its place.
    expect(first.lines[5].text).toBe("");
  });

  test("the note about a missing final newline stays with its file", () => {
    expect(files[2].lines.map((line) => line.kind)).toEqual(["hunk", "remove", "note"]);
    expect(files[2].lines[2].text).toBe("No newline at end of file");
  });

  test("a hunk is as long as its header says: a removed line starting with dashes is not a new file", () => {
    const sql = "diff --git a/q.sql b/q.sql\n--- a/q.sql\n+++ b/q.sql\n@@ -1,2 +1,2 @@\n--- old comment\n+-- new comment\n select 1;\n";
    const [file, ...rest] = parseDiff(sql);
    expect(rest).toEqual([]);
    expect(file.lines.map((line) => [line.kind, line.text])).toEqual([["hunk", "@@ -1,2 +1,2 @@"], ["remove", "-- old comment"], ["add", "-- new comment"], ["context", "select 1;"]]);
  });

  test("a diff without git headers starts a file at each pair of ---/+++", () => {
    const plain = "--- a/one.txt\t2026-11-09\n+++ b/one.txt\t2026-11-10\n@@ -1 +1 @@\n-a\n+b\n--- /dev/null\n+++ b/two.txt\n@@ -0,0 +1 @@\n+new\n";
    expect(parseDiff(plain).map((file) => [file.path, file.status, file.additions, file.deletions])).toEqual([["one.txt", "modified", 1, 1], ["two.txt", "added", 1, 0]]);
  });

  test("quoted paths and nothing at all", () => {
    expect(parseDiff('diff --git "a/sp ace.md" "b/sp ace.md"\n--- "a/sp ace.md"\n+++ "b/sp ace.md"\n@@ -1 +1 @@\n-a\n+b\n')[0].path).toBe("sp ace.md");
    expect(parseDiff("")).toEqual([]);
    expect(diffStats([])).toEqual({ files: 0, additions: 0, deletions: 0 });
  });
});
