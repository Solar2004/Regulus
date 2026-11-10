// The workspace pane of a session: the folder the agent works in, read through
// `code.inspect` (read-only: nothing here writes or runs), what changed in it, what the
// agent did and the workers it started. The panels are a registry whose order and
// visibility are a setting; the folder is asked again whenever the agent finishes something.
import { useEffect, useMemo, useState, type KeyboardEvent } from "react";
import { FileDiff, Files, GitBranch, ListTree, RefreshCw, Users, type LucideIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { CompositionNode, Workflow } from "@/composition/model";
import { t } from "@/lib/i18n";
import type { ConversationRecord } from "@/operate/conversation";
import { fill } from "@/operate/logic";
import { useOperateSettings } from "@/operate/settings";
import { ActivityPanel } from "./ActivityPanel";
import { ChangesPanel } from "./ChangesPanel";
import type { CodeApis, CodeData } from "./data";
import { FilesPanel } from "./FilesPanel";
import { isWorkerLive, type CodeSession } from "./logic";
import { activePanel, codePanelRegistry, shownPanels, type CodePanelId } from "./panels";
import { Problem } from "./Problem";
import { useAsked, useWorkspaceCache, workspaceActions, workspaceKey, workspaceView } from "./session";
import { inspectInterval, useCodeSettings } from "./settings";
import { WorkersPanel } from "./WorkersPanel";

const panelIcons: Record<CodePanelId, LucideIcon> = { files: Files, changes: FileDiff, activity: ListTree, workers: Users };

export function Workspace({ api, data, session, workflow, workerNodes, stamp, activity }: {
  api: CodeApis; data: CodeData; session: CodeSession; workflow: Workflow | undefined; workerNodes: CompositionNode[];
  /** Changes when the agent may have changed the folder. */
  stamp: number;
  /** The conversation's records, for the activity panel. */
  activity: ConversationRecord[];
}) {
  const { execution, target, live } = session;
  const settings = useCodeSettings();
  const refreshMs = useOperateSettings((s) => s.refreshMs);
  const key = workspaceKey(execution.id, target.workspaceId);
  const view = useWorkspaceCache((s) => workspaceView(s, key));
  const actions = useMemo(() => workspaceActions(key), [key]);
  // The person's "refresh" and the agent's work both ask every panel again.
  const [asked, setAsked] = useState(0);
  const fresh = `${stamp}:${asked}`;
  const summary = useAsked(() => api.code.summary(execution.id, target.workspaceId), [api, key, fresh], { enabled: live, everyMs: inspectInterval(settings.inspectRefreshMs) });
  useEffect(() => { if (summary.data) actions.summary(summary.data); }, [summary.data, actions]);
  const delegate = workerNodes[0];
  const workers = useAsked(() => api.code.workers(execution.id, delegate.id), [api, execution.id, delegate?.id], { enabled: live && !!delegate, everyMs: refreshMs });

  const shown = shownPanels(settings.panels, { hasWorkers: workerNodes.length > 0 });
  const active = activePanel(settings.panel, shown);
  const git = view.summary?.git;
  const counts: Partial<Record<CodePanelId, number>> = {
    changes: git?.available ? git.changes.length : undefined,
    workers: workers.data?.filter(isWorkerLive).length,
  };
  const onKey = (event: KeyboardEvent) => {
    if (!active || (event.key !== "ArrowRight" && event.key !== "ArrowLeft")) return;
    const next = shown[(shown.indexOf(active) + (event.key === "ArrowRight" ? 1 : shown.length - 1)) % shown.length];
    settings.showPanel(next);
    document.getElementById(`code-tab-${next}`)?.focus();
  };

  return <aside className="code-workspace" aria-label={t("code.workspace.title")}>
    <header className="code-workspace-head">
      <p className="code-git">
        <GitBranch size={13} aria-hidden="true" />
        {!live ? <span>{t("code.workspace.notRead")}</span>
          : !view.summary ? <span>{t(summary.failure ? "code.workspace.unread" : "code.workspace.reading")}</span>
            : !git?.available ? <span title={git?.reason ?? undefined}>{t("code.git.none")}</span>
              : <>
                <strong>{git.branch ?? t("code.git.detached")}</strong>
                <span>{git.changes.length === 0 ? t("code.git.clean") : fill(t(git.changes.length === 1 ? "code.git.changedOne" : "code.git.changed"), { count: git.changes.length })}</span>
                {(git.ahead > 0 || git.behind > 0) && <span>{fill(t("code.git.aheadBehind"), { ahead: git.ahead, behind: git.behind })}</span>}
              </>}
      </p>
      <Button variant="ghost" size="icon-sm" disabled={!live} aria-label={t("code.workspace.refresh")} title={t("code.workspace.refresh")} onClick={() => setAsked((n) => n + 1)}><RefreshCw size={13} /></Button>
    </header>
    <div className="code-tabs" role="tablist" aria-label={t("code.workspace.title")} onKeyDown={onKey}>
      {shown.map((id) => {
        const Icon = panelIcons[id];
        const count = counts[id];
        return <button key={id} id={`code-tab-${id}`} type="button" role="tab" aria-selected={id === active} aria-controls="code-panel" tabIndex={id === active ? 0 : -1} onClick={() => settings.showPanel(id)}>
          <Icon size={14} strokeWidth={1.75} aria-hidden="true" />{t(codePanelRegistry.find((panel) => panel.id === id)!.title)}
          {count !== undefined && count > 0 && <span className="operate-count">{count}</span>}
        </button>;
      })}
    </div>
    <div className="code-panel" id="code-panel" role="tabpanel" aria-labelledby={active ? `code-tab-${active}` : undefined}>
      {summary.failure && live && (active === "files" || active === "changes") ? <Problem failure={summary.failure} onRetry={summary.reload} />
        : active === "files" ? <FilesPanel api={api.code} executionId={execution.id} nodeId={target.workspaceId} live={live} fresh={fresh} view={view} actions={actions} />
          : active === "changes" ? <ChangesPanel api={api.code} executionId={execution.id} nodeId={target.workspaceId} live={live} fresh={fresh} view={view} actions={actions} />
            : active === "activity" ? <ActivityPanel records={activity} />
              : active === "workers" && delegate ? <WorkersPanel api={api} data={data} session={session} delegate={delegate} workflow={workflow} workers={workers} />
                : <p className="operate-empty">{t("code.workspace.noPanels")}</p>}
    </div>
  </aside>;
}
