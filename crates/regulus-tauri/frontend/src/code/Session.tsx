// One coding session: an execution on screen. The conversation is the Agent box's own
// (`agent.send`, `agent.history`, `agent.cancel`, the panel Operate uses); around it, how the
// session stands, what waits for the person, and the folder it works in. Nothing here keeps
// a second record: the transcript, the decisions and the folder are asked from the core.
import { useMemo, useState } from "react";
import { ArrowLeft, CircleCheck, CircleStop, Loader, PanelRight, Play, ShieldQuestion, Square, TriangleAlert, type LucideIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t, type LocaleKey } from "@/lib/i18n";
import { AgentConversation, PermissionCard, useDecisionList } from "@/operate/Conversation";
import { decisionFor, reduceConversation } from "@/operate/conversation";
import { Decision } from "@/operate/Decision";
import { errorText, fill, nodeRows } from "@/operate/logic";
import { agentConversationKey, cachedConversation, useConversationCache } from "@/operate/session";
import { useOperateSettings } from "@/operate/settings";
import { ExecutionStateMark, Mark, NodeStateMark, nodeReason } from "@/operate/StateMark";
import { useRegulusUi } from "@/store/useRegulusUi";
import { codeMethods } from "./client";
import type { CodeApis, CodeData } from "./data";
import { HarnessMenu } from "./HarnessMenu";
import { folderName, harnessParts, nodesAnswering, openPermissions, sessionMood, sessionProblems, workStamp, type CodeSession, type SessionMood } from "./logic";
import { Mascot } from "./Mascot";
import { useWorkspaceCache, workspaceKey, workspaceView } from "./session";
import { useCodeSettings } from "./settings";
import { Workspace } from "./Workspace";

type Tone = "success" | "warning" | "danger" | "muted" | "info";

/** How a live session stands, as an icon and a word; a stopped or failed one says so with the execution's own mark. */
const moodMarks: Record<Extract<SessionMood, "idle" | "working" | "waiting">, { icon: LucideIcon; tone: Tone; label: LocaleKey }> = {
  idle: { icon: CircleCheck, tone: "success", label: "code.mood.idle" },
  working: { icon: Loader, tone: "info", label: "code.mood.working" },
  waiting: { icon: ShieldQuestion, tone: "warning", label: "code.mood.waiting" },
};

export function Session({ api, data, session, sessions, tools, banner, onLeave, onOpen }: {
  api: CodeApis; data: CodeData; session: CodeSession; sessions: CodeSession[];
  /** The view's own buttons, shown at the end of the bar. */
  tools: React.ReactNode;
  /** What the view shows under the bar: its settings, a notice. */
  banner: React.ReactNode;
  onLeave: () => void; onOpen: (executionId: string) => void;
}) {
  const { execution, target, live } = session;
  const settings = useCodeSettings();
  const startTimeoutMs = useOperateSettings((s) => s.startTimeoutMs);
  const openInCanvas = useRegulusUi((s) => s.openInCanvas);
  const workflow = useMemo(() => data.profiles.find((profile) => profile.id === target.profileId)?.workflows.find((item) => item.id === target.workflowId), [data.profiles, target]);
  const rows = useMemo(() => nodeRows(workflow, execution, data.modules), [workflow, execution, data.modules]);
  const [pickedAgent, setPickedAgent] = useState<string | null>(null);
  const agents = rows.filter((row) => target.agentIds.includes(row.id));
  const agent = agents.find((row) => row.id === pickedAgent) ?? agents[0] ?? null;
  const workerNodes = useMemo(() => nodesAnswering(workflow, data.manifests, codeMethods.workers), [workflow, data.manifests]);
  const parts = useMemo(() => harnessParts(workflow, agent?.id ?? null, workerNodes.map((node) => node.id)), [workflow, agent?.id, workerNodes]);
  const problems = sessionProblems(rows);

  // How the turn stands, read from what the conversation panel last drew.
  const cached = useConversationCache((s) => cachedConversation(s, agentConversationKey(execution.id, agent?.id ?? "")));
  const turns = useMemo(() => reduceConversation(cached.records, "turn"), [cached.records]);
  const asking = openPermissions(turns);
  const working = cached.session?.state === "busy" || cached.pending.length > 0 || turns.some((turn) => turn.open);
  const mood = sessionMood(execution, { waiting: asking.length > 0, working });
  const stamp = workStamp(cached.records);
  // One poll of the pending decisions for the whole session: the cards of the transcript and the dock share it.
  const decisionList = useDecisionList(api.operate, live);
  // The requests a person can answer here: those a decider put in front of them. One nobody
  // is wired to answer stays in the transcript, where its card says so.
  const answerable = asking.flatMap((item) => {
    const decision = agent ? decisionFor(decisionList.decisions, execution.id, agent.id, item.id) : undefined;
    return decision ? [{ item, decision }] : [];
  });
  const matched = new Set(answerable.map(({ decision }) => decision.id));
  const others = decisionList.decisions.filter((pending) => pending.execution_id === execution.id && !matched.has(pending.id));

  const summary = useWorkspaceCache((s) => workspaceView(s, workspaceKey(execution.id, target.workspaceId)).summary);
  const root = summary?.root ?? target.root;
  const earlier = sessions.filter((other) => other.target.profileId === target.profileId && other.target.workflowId === target.workflowId && other.execution.id !== execution.id);
  const newer = live ? null : earlier.find((other) => other.live) ?? null;

  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const stop = async () => {
    setBusy(true);
    try { data.seen(await api.operate.stop(execution.id)); setFailure(null); }
    catch (error) { setFailure(errorText(error)); } finally { setBusy(false); setConfirming(false); }
  };
  const startAgain = async () => {
    setBusy(true);
    try {
      const started = await api.operate.start(target.profileId, target.workflowId, startTimeoutMs);
      data.seen(started);
      if (started.state.status === "failed") setFailure(`${started.id}: ${started.state.reason}`); else onOpen(started.id);
    } catch (error) { setFailure(errorText(error)); } finally { setBusy(false); }
  };

  const dock = <>
    {live && cached.records.length === 0 && earlier.length > 0 && <p className="code-fine code-context">{t("code.session.newContext")}</p>}
    {(answerable.length > 0 || others.length > 0) && <section className="code-dock" aria-label={t("code.dock.title")}>
      <h4><ShieldQuestion size={14} aria-hidden="true" />{t("code.dock.title")}</h4>
      {answerable.length > 0 && <ul>{answerable.map(({ item, decision }) => <PermissionCard key={item.key} api={api.operate} item={item} decision={decision} onAnswered={decisionList.reload} />)}</ul>}
      {others.length > 0 && <ul className="decisions">{others.map((pending) => <Decision key={pending.id} api={api.operate} pending={pending} onDone={decisionList.reload} />)}</ul>}
    </section>}
  </>;

  return <>
    <header className="code-session-bar">
      <Button variant="ghost" size="icon-sm" aria-label={t("code.session.back")} title={t("code.session.back")} onClick={onLeave}><ArrowLeft size={15} /></Button>
      {settings.mascot && <Mascot scale={2} mood={mood} decorative />}
      <div className="code-session-title">
        <h1>{root ? folderName(root) || root : target.profileName}</h1>
        <code title={root ?? undefined}>{root ?? target.profileId}</code>
      </div>
      {mood === "failed" || mood === "stopped" ? <ExecutionStateMark state={execution.state} />
        : <Mark icon={moodMarks[mood].icon} tone={moodMarks[mood].tone} label={t(moodMarks[mood].label)} />}
      <span className="code-session-id" title={fill(t("code.session.idTitle"), { profile: target.profileId })}>{execution.id}</span>
      <div className="code-session-actions">
        {agents.length > 1 && <label className="operate-inline">{t("operate.conversation.subject")}
          <select value={agent?.id ?? ""} onChange={(event) => setPickedAgent(event.target.value)}>{agents.map((row) => <option key={row.id} value={row.id}>{row.name}</option>)}</select></label>}
        <HarnessMenu profileId={target.profileId} workflowId={target.workflowId} parts={parts} live={live} />
        {live && !confirming && <Button size="sm" variant="outline" disabled={busy} onClick={() => setConfirming(true)}><Square size={13} />{t("code.session.stop")}</Button>}
        <Button variant="ghost" size="icon-sm" aria-pressed={settings.workspaceShown} aria-label={t("code.session.toggleWorkspace")} title={t("code.session.toggleWorkspace")}
          onClick={() => settings.setWorkspaceShown(!settings.workspaceShown)}><PanelRight size={15} /></Button>
        {tools}
      </div>
    </header>
    {banner}
    {confirming && <p className="code-strip" role="alertdialog" aria-label={t("code.session.stopTitle")}>
      <span>{t("code.session.stopTitle")}</span>
      <Button size="sm" variant="destructive" disabled={busy} onClick={() => void stop()}><Square size={13} />{t("code.session.stopNow")}</Button>
      <Button size="sm" variant="ghost" onClick={() => setConfirming(false)}>{t("code.session.keep")}</Button>
    </p>}
    {failure && <p className="operate-notice tone-danger" role="alert">{failure}<button type="button" className="link-button" onClick={() => setFailure(null)}>{t("operate.dismiss")}</button></p>}
    {"reason" in execution.state && <p className="code-strip tone-danger" role="status"><TriangleAlert size={14} aria-hidden="true" /><span>{execution.state.reason}</span></p>}
    {!live && <p className="code-strip" role="status">
      <CircleStop size={14} aria-hidden="true" /><span>{t("code.session.stopped")}</span>
      {newer ? <Button size="sm" variant="outline" onClick={() => onOpen(newer.execution.id)}>{fill(t("code.session.openNewer"), { id: newer.execution.id })}</Button>
        : <Button size="sm" variant="outline" disabled={busy} onClick={() => void startAgain()}><Play size={13} />{t("code.session.startNew")}</Button>}
    </p>}
    {problems.length > 0 && <ul className="code-problems" aria-label={t("code.session.problems")}>{problems.map((row) => <li key={row.id}>
      <strong>{row.name}</strong><NodeStateMark state={row.state} />{nodeReason(row.state) && <span>{nodeReason(row.state)}</span>}
      <button type="button" className="link-button" onClick={() => openInCanvas(target.profileId, { workflowId: target.workflowId, nodeId: row.id })}>{t("code.session.fixInCanvas")}</button>
    </li>)}</ul>}
    <div className="code-session-body" data-workspace={settings.workspaceShown ? "shown" : "hidden"} style={{ ["--code-workspace" as string]: `${settings.workspaceWidth}%` }}>
      <div className="code-conversation">
        {agent ? <AgentConversation key={`${execution.id}/${agent.id}`} api={api.operate} executionId={execution.id} row={agent} decisionList={decisionList} dock={dock} />
          : <p className="operate-empty">{t("code.session.noAgent")}</p>}
      </div>
      {settings.workspaceShown && <Workspace api={api} data={data} session={session} workflow={workflow} workerNodes={workerNodes} stamp={stamp} activity={cached.records} />}
    </div>
  </>;
}
