// The folder as a tree that loads as it opens (`code.inspect list`, one level at a time),
// a search over it (`search`: text as typed, case aside) and a viewer (`read`). Read-only.
import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { ChevronDown, ChevronRight, FileText, Folder, FolderOpen, Search, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t, type LocaleKey } from "@/lib/i18n";
import { fill } from "@/operate/logic";
import { callFailure, type CallFailure, type CodeApi } from "./client";
import { baseName, changeKind, fileLines, sizeParts, treeRows, type ChangeKind, type GitChange } from "./logic";
import { Problem } from "./Problem";
import { useAsked, type workspaceActions, type WorkspaceView } from "./session";
import { useCodeSettings } from "./settings";

type Actions = ReturnType<typeof workspaceActions>;

/** One letter per kind of change, as git users read it, always with its word as the title. */
export const changeMarks: Record<ChangeKind, { letter: string; tone: string; label: LocaleKey }> = {
  modified: { letter: "M", tone: "warning", label: "code.change.modified" },
  added: { letter: "A", tone: "success", label: "code.change.added" },
  untracked: { letter: "U", tone: "success", label: "code.change.untracked" },
  deleted: { letter: "D", tone: "danger", label: "code.change.deleted" },
  renamed: { letter: "R", tone: "info", label: "code.change.renamed" },
  conflicted: { letter: "C", tone: "danger", label: "code.change.conflicted" },
  ignored: { letter: "I", tone: "muted", label: "code.change.ignored" },
};

export function ChangeMark({ change }: { change: GitChange }) {
  const mark = changeMarks[changeKind(change)];
  return <abbr className={`code-change-mark tone-${mark.tone}`} title={t(mark.label)}>{mark.letter}</abbr>;
}

export const sizeText = (bytes: number) => {
  const { value, unit } = sizeParts(bytes);
  return fill(t(`code.size.${unit}` as LocaleKey), { value });
};

function Viewer({ api, executionId, nodeId, file, fresh, onClose }: {
  api: CodeApi; executionId: string; nodeId: string; file: { path: string; line: number | null }; fresh: string; onClose: () => void;
}) {
  const read = useAsked(() => api.read(executionId, nodeId, file.path), [api, executionId, nodeId, file.path, fresh]);
  const lines = useMemo(() => read.data ? fileLines(read.data.text) : [], [read.data]);
  const target = useRef<HTMLLIElement>(null);
  useEffect(() => { target.current?.scrollIntoView?.({ block: "center" }); }, [file.path, file.line, read.data]);
  return <section className="code-viewer" aria-label={file.path}>
    <header>
      <FileText size={14} aria-hidden="true" />
      <strong title={file.path}>{file.path}</strong>
      {read.data?.size !== undefined && <small>{sizeText(read.data.size)}</small>}
      <Button variant="ghost" size="icon-xs" aria-label={t("code.files.close")} onClick={onClose}><X size={13} /></Button>
    </header>
    {read.failure ? <Problem failure={read.failure} onRetry={read.reload} />
      : !read.data ? <p className="operate-empty">{t("code.files.reading")}</p>
        : <>
          {read.data.truncated && <p className="code-fine code-cut">{t("code.files.truncated")}</p>}
          {read.data.text === "" ? <p className="operate-empty">{t("code.files.emptyFile")}</p>
            : <ol className="code-lines">{lines.map((line, index) => <li key={index} ref={index + 1 === file.line ? target : undefined} className={index + 1 === file.line ? "is-target" : undefined}>
              <code>{line || " "}</code>
            </li>)}</ol>}
        </>}
  </section>;
}

export function FilesPanel({ api, executionId, nodeId, live, fresh, view, actions }: {
  api: CodeApi; executionId: string; nodeId: string; live: boolean; fresh: string; view: WorkspaceView; actions: Actions;
}) {
  const searchMinChars = useCodeSettings((s) => s.searchMinChars);
  const [failure, setFailure] = useState<CallFailure | null>(null);
  const [sought, setSought] = useState("");
  const open = view.open.join("\n");

  // The root and every open folder are listed again when the agent finishes something.
  useEffect(() => {
    if (!live) return;
    let current = true;
    for (const folder of ["", ...view.open]) {
      api.list(executionId, nodeId, folder).then(
        (listing) => { if (current) { actions.listing(listing); if (!folder) setFailure(null); } },
        (error) => { if (!current) return; if (folder) actions.forget(folder); else setFailure(callFailure(error)); },
      );
    }
    return () => { current = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api, executionId, nodeId, live, fresh, open]);

  const found = useAsked(() => api.search(executionId, nodeId, sought), [api, executionId, nodeId, sought], { enabled: live && sought.length >= searchMinChars });
  const search = (event: FormEvent) => { event.preventDefault(); setSought(view.query.trim()); };
  const clear = () => { actions.query(""); setSought(""); };

  const rows = useMemo(() => treeRows(view.listings, new Set(view.open)), [view.listings, view.open]);
  const changes = view.summary?.git.changes ?? [];
  const changeOf = (path: string) => changes.find((change) => change.path === path);
  const under = (folder: string) => changes.filter((change) => change.path.startsWith(`${folder}/`)).length;

  if (!live) return <p className="operate-empty">{t("code.workspace.stopped")}</p>;
  if (failure) return <Problem failure={failure} onRetry={() => { setFailure(null); actions.toggle(""); actions.toggle(""); }} />;
  const searching = sought.length >= searchMinChars;
  return <div className="code-files" data-viewing={view.file ? "yes" : "no"}>
    <div className="code-tree">
      <form className="code-search" role="search" onSubmit={search}>
        <Search size={13} aria-hidden="true" />
        <input type="search" value={view.query} aria-label={t("code.files.search")} placeholder={t("code.files.search")} onChange={(event) => actions.query(event.target.value)} />
        {(view.query || searching) && <Button type="button" variant="ghost" size="icon-xs" aria-label={t("code.files.clearSearch")} onClick={clear}><X size={12} /></Button>}
      </form>
      {view.query.trim().length > 0 && view.query.trim().length < searchMinChars && <p className="code-fine">{fill(t("code.files.searchShort"), { count: searchMinChars })}</p>}
      {searching ? (found.failure ? <Problem failure={found.failure} onRetry={found.reload} />
        : !found.data ? <p className="operate-empty">{t("code.files.searching")}</p>
          : found.data.matches.length === 0 ? <p className="operate-empty">{fill(t("code.files.noMatches"), { query: sought })}</p>
            : <>
              <p className="code-fine">{fill(t(found.data.truncated ? "code.files.matchesCut" : "code.files.matches"), { count: found.data.matches.length })}</p>
              <ul className="code-matches">{found.data.matches.map((match, index) => <li key={index}>
                <button type="button" aria-current={view.file?.path === match.path && view.file.line === match.line ? "true" : undefined} onClick={() => actions.show(match.path, match.line)}>
                  <span>{match.path}<small>:{match.line}</small></span><code>{match.text.trim()}</code>
                </button>
              </li>)}</ul>
            </>)
        : rows.length === 0 ? <p className="operate-empty">{t(view.listings[""] ? "code.files.empty" : "code.files.listing")}</p>
          : <ul className="code-tree-rows" role="tree" aria-label={t("code.panel.files")}>{rows.map((row) => {
            const change = row.kind === "file" ? changeOf(row.path) : undefined;
            const inside = row.kind === "directory" ? under(row.path) : 0;
            return <li key={row.path} role="none">
              <button type="button" role="treeitem" aria-expanded={row.kind === "directory" ? row.open : undefined} aria-current={view.file?.path === row.path ? "true" : undefined}
                style={{ paddingInlineStart: `calc(var(--rg-space-2) + ${row.depth} * var(--rg-space-4))` }}
                onClick={() => row.kind === "directory" ? actions.toggle(row.path) : actions.show(row.path)}>
                {row.kind === "directory" ? (row.open ? <ChevronDown size={12} aria-hidden="true" /> : <ChevronRight size={12} aria-hidden="true" />) : <span className="code-tree-gap" />}
                {row.kind === "directory" ? (row.open ? <FolderOpen size={13} aria-hidden="true" /> : <Folder size={13} aria-hidden="true" />) : <FileText size={13} aria-hidden="true" />}
                <span className="code-tree-name">{row.name}</span>
                {change && <ChangeMark change={change} />}
                {inside > 0 && <small title={fill(t(inside === 1 ? "code.files.changedInsideOne" : "code.files.changedInside"), { count: inside })}>{inside}</small>}
              </button>
              {row.open && view.cut.includes(row.path) && <p className="code-fine code-cut">{t("code.files.listCut")}</p>}
            </li>;
          })}</ul>}
      {!searching && view.cut.includes("") && <p className="code-fine code-cut">{t("code.files.listCut")}</p>}
    </div>
    {view.file ? <Viewer api={api} executionId={executionId} nodeId={nodeId} file={view.file} fresh={fresh} onClose={actions.closeFile} />
      : <p className="operate-empty code-viewer-empty">{fill(t("code.files.pick"), { name: view.summary ? baseName(view.summary.root) || view.summary.root : "" })}</p>}
  </div>;
}
