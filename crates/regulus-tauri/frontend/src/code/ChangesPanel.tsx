// What changed in the folder: the paths git status names, and the diff of the one picked
// (`code.inspect diff`: tracked changes against the last commit, asked once and split here).
// A file git does not track yet has no diff: it says so and opens in the file viewer.
import { useMemo } from "react";
import { FileText } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t, type LocaleKey } from "@/lib/i18n";
import { fill } from "@/operate/logic";
import type { CodeApi } from "./client";
import { diffStats, parseDiff, type DiffFile, type DiffLine } from "./diff";
import { ChangeMark, changeMarks } from "./FilesPanel";
import { changeKind } from "./logic";
import { Problem } from "./Problem";
import { useAsked, type workspaceActions, type WorkspaceView } from "./session";
import { useCodeSettings } from "./settings";

type Actions = ReturnType<typeof workspaceActions>;

const lineMarks: Record<DiffLine["kind"], string> = { add: "+", remove: "−", context: " ", hunk: "", note: "" };

function DiffView({ file }: { file: DiffFile }) {
  if (file.status === "binary") return <p className="operate-empty">{t("code.changes.binary")}</p>;
  if (file.lines.length === 0) return <p className="operate-empty">{t("code.changes.noLines")}</p>;
  return <table className="code-diff" aria-label={fill(t("code.changes.diffOf"), { path: file.path })}>
    <tbody>{file.lines.map((line, index) => line.kind === "hunk" || line.kind === "note"
      ? <tr key={index} className={`diff-${line.kind}`}><td colSpan={4}>{line.text}</td></tr>
      : <tr key={index} className={`diff-${line.kind}`}>
        <td className="diff-number">{line.oldNumber ?? ""}</td>
        <td className="diff-number">{line.newNumber ?? ""}</td>
        <td className="diff-mark" aria-label={line.kind === "add" ? t("code.changes.added") : line.kind === "remove" ? t("code.changes.removed") : undefined}>{lineMarks[line.kind]}</td>
        <td className="diff-text"><code>{line.text || " "}</code></td>
      </tr>)}</tbody>
  </table>;
}

export function ChangesPanel({ api, executionId, nodeId, live, fresh, view, actions }: {
  api: CodeApi; executionId: string; nodeId: string; live: boolean; fresh: string; view: WorkspaceView; actions: Actions;
}) {
  const showPanel = useCodeSettings((s) => s.showPanel);
  const git = view.summary?.git;
  const diff = useAsked(() => api.diff(executionId, nodeId), [api, executionId, nodeId, fresh], { enabled: live && !!git?.available });
  const files = useMemo(() => diff.data ? parseDiff(diff.data.text) : [], [diff.data]);
  const stats = diffStats(files);

  if (!live) return <p className="operate-empty">{t("code.workspace.stopped")}</p>;
  if (!view.summary) return <p className="operate-empty">{t("code.workspace.reading")}</p>;
  if (!git?.available) return <div className="operate-note" role="status">
    <strong>{t("code.git.none")}</strong>
    <p>{t("code.changes.noGit")}</p>
    {git?.reason && <code className="operate-reason">{git.reason}</code>}
  </div>;
  if (git.changes.length === 0) return <p className="operate-empty">{t("code.changes.clean")}</p>;

  const picked = git.changes.find((change) => change.path === view.change) ?? git.changes[0];
  const kind = changeKind(picked);
  const file = files.find((item) => item.path === picked.path || item.oldPath === picked.path);
  const openFile = () => { actions.show(picked.path); showPanel("files"); };
  return <div className="code-changes">
    <div className="code-change-list">
      <p className="code-fine">{fill(t("code.changes.stats"), { files: git.changes.length, additions: stats.additions, deletions: stats.deletions })}</p>
      {git.truncated && <p className="code-fine code-cut">{t("code.changes.statusCut")}</p>}
      <ul>{git.changes.map((change) => {
        const changed = files.find((item) => item.path === change.path || item.oldPath === change.path);
        return <li key={change.path}>
          <button type="button" aria-current={change.path === picked.path ? "true" : undefined} onClick={() => actions.change(change.path)} title={change.from ? fill(t("code.changes.from"), { path: change.from }) : change.path}>
            <ChangeMark change={change} />
            <span className="code-change-path">{change.path}</span>
            {changed && changed.status !== "binary" && <small><span className="tone-success">+{changed.additions}</span> <span className="tone-danger">−{changed.deletions}</span></small>}
          </button>
        </li>;
      })}</ul>
    </div>
    <section className="code-change-diff" aria-label={picked.path}>
      <header>
        <strong title={picked.path}>{picked.path}</strong>
        <small>{t(changeMarks[kind].label as LocaleKey)}{picked.from ? ` · ${fill(t("code.changes.from"), { path: picked.from })}` : ""}</small>
        {kind !== "deleted" && <Button variant="ghost" size="sm" onClick={openFile}><FileText size={13} />{t("code.changes.openFile")}</Button>}
      </header>
      {diff.failure ? <Problem failure={diff.failure} onRetry={diff.reload} />
        : !diff.data ? <p className="operate-empty">{t("code.changes.reading")}</p>
          : <>
            {diff.data.truncated && <p className="code-fine code-cut">{t("code.changes.diffCut")}</p>}
            {file ? <DiffView file={file} />
              : <p className="operate-empty">{t(kind === "untracked" ? "code.changes.untracked" : diff.data.truncated ? "code.changes.beyondCut" : "code.changes.noDiff")}</p>}
          </>}
    </section>
  </div>;
}
