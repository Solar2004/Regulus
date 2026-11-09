// The conversation panel (docs/04-client.md §8): talking to a direct agent node
// (`agent.send`, `agent.history`, `agent.cancel`) or to a workflow the person bound as a
// conversation (`workflow.send` on its input, replies and activity from `event.list`).
// The core's record is the truth; the cache only draws the last state at once.
import { useEffect, useMemo, useRef, useState } from "react";
import {
  Bot, Brain, CircleCheck, CircleDashed, CircleHelp, CircleSlash, CircleStop, Clock, ListChecks, Loader, MessageSquareText,
  OctagonX, Radio, ShieldCheck, ShieldQuestion, Square, TriangleAlert, UserRound, Workflow as WorkflowIcon, Wrench, type LucideIcon,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import type { Workflow } from "@/composition/model";
import { t, type LocaleKey } from "@/lib/i18n";
import type { OperateApi } from "./client";
import { Composer } from "./Composer";
import {
  asksPermission, bindingFor, busRecords, conversationPorts, decisionFor, historyRecords, reduceConversation, sameSubject,
  type ConversationBinding, type Subject, type TurnItem, type TurnView,
} from "./conversation";
import { Decision } from "./Decision";
import { canTalk, errorText, fill, isLive, unavailableOf, type NodeRow, type Unavailable } from "./logic";
import type { AgentSession, Execution, PendingDecision } from "./model";
import { NotAvailable } from "./NotAvailable";
import { cachedConversation, useConversationCache } from "./session";
import { useOperateSettings } from "./settings";
import { Mark } from "./StateMark";
import { usePolling } from "./usePolling";
import { ValueView } from "./ValueView";

type Tone = "success" | "warning" | "danger" | "muted" | "info";
type Meta = { icon: LucideIcon; tone: Tone; label: LocaleKey };

/** A word the engine or ACP says, as icon and text; one this app does not know shows as it came. */
function Said({ value, known, fallback }: { value: string; known: Record<string, Meta>; fallback: Omit<Meta, "label"> }) {
  const meta = known[value];
  return meta ? <Mark icon={meta.icon} tone={meta.tone} label={t(meta.label)} /> : <Mark icon={fallback.icon} tone={fallback.tone} label={value || "—"} />;
}

const sessionStates: Record<string, Meta> = {
  idle: { icon: CircleCheck, tone: "success", label: "operate.conversation.state.idle" },
  busy: { icon: Loader, tone: "info", label: "operate.conversation.state.busy" },
  ended: { icon: CircleStop, tone: "muted", label: "operate.conversation.state.ended" },
  stopped: { icon: CircleStop, tone: "muted", label: "operate.conversation.state.stopped" },
  unknown: { icon: CircleHelp, tone: "muted", label: "operate.conversation.state.unknown" },
};
const SessionState = ({ state }: { state: string }) => <Said value={state} known={sessionStates} fallback={{ icon: CircleHelp, tone: "muted" }} />;

/** ACP's tool and plan statuses. */
const workStates: Record<string, Meta> = {
  pending: { icon: CircleDashed, tone: "muted", label: "operate.conversation.status.pending" },
  in_progress: { icon: Loader, tone: "info", label: "operate.conversation.status.inProgress" },
  completed: { icon: CircleCheck, tone: "success", label: "operate.conversation.status.completed" },
  failed: { icon: TriangleAlert, tone: "danger", label: "operate.conversation.status.failed" },
};
const WorkState = ({ status }: { status: string }) => <Said value={status} known={workStates} fallback={{ icon: CircleHelp, tone: "muted" }} />;

/** Every pending decision the module `human` lists, polled while `enabled`; nothing without the module. */
function useDecisionList(api: OperateApi, enabled: boolean) {
  const refreshMs = useOperateSettings((s) => s.refreshMs);
  const [decisions, setDecisions] = useState<PendingDecision[]>([]);
  const [absent, setAbsent] = useState(false);
  const [tick, setTick] = useState(0);
  usePolling(async (current) => {
    try { const listed = await api.decisions(); if (current()) setDecisions(listed); }
    catch (error) { if (current() && unavailableOf(error)) setAbsent(true); }
  }, refreshMs, [api, tick], { enabled: enabled && !absent });
  return { decisions: enabled ? decisions : [], reload: () => setTick((n) => n + 1) };
}

function PermissionCard({ api, item, decision, onAnswered }: {
  api: OperateApi; item: Extract<TurnItem, { type: "permission" }>; decision: PendingDecision | undefined; onAnswered: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const answer = async (choice: { option: string } | "reject") => {
    if (!decision) return;
    setBusy(true);
    try {
      if (choice === "reject") await api.answer(decision.id, "reject", {}, null);
      else await api.answer(decision.id, "approve", { option: choice.option }, null);
      setError(null);
      onAnswered();
    } catch (failure) { setError(errorText(failure)); } finally { setBusy(false); }
  };
  const chosen = item.resolved && (item.options.find((option) => option.id === item.resolved!.option)?.name ?? item.resolved.option);
  return <li className="card card-permission">
    <header>
      {item.resolved ? <ShieldCheck size={14} aria-hidden="true" /> : <ShieldQuestion size={14} aria-hidden="true" />}
      <strong>{fill(t("operate.conversation.permission"), { title: item.title })}</strong>
      {item.tool && <small>{fill(t("operate.conversation.permissionTool"), { tool: item.tool })}</small>}
      {item.node && <small>{fill(t("operate.conversation.fromNode"), { node: item.node })}</small>}
    </header>
    {item.resolved ? (item.resolved.option === null
      ? <Mark icon={CircleSlash} tone="muted" label={t("operate.conversation.permissionCancelled")} />
      : <Mark icon={CircleCheck} tone="success" label={fill(t("operate.conversation.permissionChosen"), { option: chosen ?? "" })} />)
      : decision ? <div className="decision-actions">
        {item.options.map((option) => <Button key={option.id} size="sm" variant={option.kind.startsWith("reject") ? "outline" : "default"} disabled={busy}
          title={option.kind} onClick={() => void answer({ option: option.id })}>{option.name}</Button>)}
        {/* `human.answer reject` takes the request's reject option: a button of its own only when none is offered. */}
        {!item.options.some((option) => option.kind.startsWith("reject")) && <Button size="sm" variant="ghost" disabled={busy} onClick={() => void answer("reject")}><OctagonX size={13} />{t("operate.human.reject")}</Button>}
      </div>
        : <p className="operate-subtle"><Clock size={12} aria-hidden="true" /> {t("operate.conversation.permissionWaiting")}</p>}
    {error && <p className="operate-error" role="alert">{error}</p>}
  </li>;
}

function PlanEntries({ entries }: { entries: unknown }) {
  const list = Array.isArray(entries) ? entries : null;
  const readable = list?.every((entry) => entry && typeof entry === "object" && typeof (entry as { content?: unknown }).content === "string");
  if (!list || !readable) return <ValueView value={entries} />;
  return <ol className="plan-entries">{list.map((entry, index) => {
    const { content, status } = entry as { content: string; status?: unknown };
    return <li key={index}><WorkState status={typeof status === "string" ? status : ""} /><span>{content}</span></li>;
  })}</ol>;
}

const NodeTag = ({ node }: { node: string | null }) => node ? <small className="card-node">{fill(t("operate.conversation.fromNode"), { node })}</small> : null;

function Item({ api, item, agentName, decisionOf, onAnswered }: {
  api: OperateApi; item: TurnItem; agentName: string; decisionOf: (item: Extract<TurnItem, { type: "permission" }>) => PendingDecision | undefined; onAnswered: () => void;
}) {
  switch (item.type) {
    case "entry": {
      const who = item.role === "user" ? { icon: <UserRound size={13} />, label: t("operate.agent.you") }
        : item.role === "agent" ? { icon: <Bot size={13} />, label: item.port ? fill(t("operate.conversation.reply"), { port: item.port }) : agentName }
          : { icon: <TriangleAlert size={13} />, label: t("operate.conversation.system") };
      return <li className={`bubble bubble-${item.role === "user" || item.role === "agent" ? item.role : "system"}`}>
        <span className="turn-who">{who.icon}{who.label}<time>{new Date(item.at).toLocaleTimeString()}</time></span>
        <p>{item.text}</p>
      </li>;
    }
    case "live": return <li className="bubble bubble-agent is-live">
      <span className="turn-who"><Bot size={13} />{item.node ?? agentName}<Mark icon={Loader} tone="info" label={t("operate.conversation.writing")} /></span>
      <p>{item.text}</p>
    </li>;
    case "thought": return <li className="card card-thought"><details>
      <summary><Brain size={14} aria-hidden="true" />{t("operate.conversation.thought")}<NodeTag node={item.node} /></summary>
      <p>{item.text}</p>
    </details></li>;
    case "tool": return <li className="card card-tool">
      <header><Wrench size={14} aria-hidden="true" /><strong>{item.title}</strong>{item.toolKind && <small>{item.toolKind}</small>}<NodeTag node={item.node} /><WorkState status={item.status} /></header>
      {item.content !== null && item.content !== undefined && <details><summary>{t("operate.conversation.toolContent")}</summary><ValueView value={item.content} /></details>}
    </li>;
    case "plan": return <li className="card card-plan">
      <header><ListChecks size={14} aria-hidden="true" /><strong>{t("operate.conversation.plan")}</strong><NodeTag node={item.node} /></header>
      <PlanEntries entries={item.entries} />
    </li>;
    case "permission": return <PermissionCard api={api} item={item} decision={decisionOf(item)} onAnswered={onAnswered} />;
    case "ended": return <li className="conversation-line"><CircleStop size={12} aria-hidden="true" />{fill(t("operate.conversation.turnEnded"), { reason: item.stopReason || "—" })}<NodeTag node={item.node} /></li>;
    case "state": return <li className="conversation-line"><Radio size={12} aria-hidden="true" />{t("operate.conversation.session")}<SessionState state={item.state} /><NodeTag node={item.node} /></li>;
    case "other": return <li className="conversation-line"><CircleHelp size={12} aria-hidden="true" />{fill(t("operate.conversation.event"), { kind: item.name })}<ValueView value={item.raw} /></li>;
  }
}

/** The turns, each with its cards, and what was sent and is not in the record yet. */
function Transcript({ api, turns, pending, agentName, decisionOf, onAnswered }: {
  api: OperateApi; turns: TurnView[]; pending: { text: string; at: number }[]; agentName: string;
  decisionOf: (item: Extract<TurnItem, { type: "permission" }>) => PendingDecision | undefined; onAnswered: () => void;
}) {
  const end = useRef<HTMLDivElement>(null);
  const size = turns.reduce((sum, turn) => sum + turn.items.length, pending.length);
  useEffect(() => { end.current?.scrollIntoView?.({ block: "nearest" }); }, [size]);
  return <div className="conversation-log" aria-live="polite">
    {turns.length === 0 && pending.length === 0 && <p className="operate-empty">{t("operate.conversation.empty")}</p>}
    {turns.map((turn) => <ol key={turn.key} className={`conversation-turn${turn.open ? " is-open" : ""}`}>
      {turn.items.map((item) => <Item key={item.key} api={api} item={item} agentName={agentName} decisionOf={decisionOf} onAnswered={onAnswered} />)}
    </ol>)}
    {pending.length > 0 && <ol className="conversation-turn">{pending.map((send, index) => <li key={`${send.at}-${index}`} className="bubble bubble-user is-pending">
      <span className="turn-who"><UserRound size={13} />{t("operate.agent.you")}<Mark icon={Clock} tone="muted" label={t("operate.conversation.queued")} /></span>
      <p>{send.text}</p>
    </li>)}</ol>}
    <div ref={end} />
  </div>;
}

function SessionLine({ session }: { session: AgentSession | null }) {
  if (!session) return null;
  return <p className="conversation-session">
    <span>{session.engine_session_id ? fill(t("operate.conversation.sessionId"), { id: session.engine_session_id }) : t("operate.conversation.sessionNone")}</span>
    <span>{t(session.native_resume ? "operate.conversation.resumed" : "operate.conversation.notResumed")}</span>
    <SessionState state={session.state} />
  </p>;
}

/**
 * A cursor that starts at 0 whenever the key or the connection changes: the record is read
 * again from the start and rebuilt, then followed from `next`.
 */
function useCursor(api: OperateApi, key: string) {
  const cursor = useRef<number | null>(null);
  useEffect(() => { cursor.current = null; }, [api, key]);
  return cursor;
}

/** A direct agent node: its conversation from `agent.history`, turns sent with `agent.send`. */
export function AgentConversation({ api, executionId, row }: { api: OperateApi; executionId: string; row: NodeRow }) {
  const { refreshMs, eventPageSize, conversationKeep } = useOperateSettings();
  const key = `agent:${executionId}/${row.id}`;
  const cached = useConversationCache((s) => cachedConversation(s, key));
  const cursor = useCursor(api, key);
  const [missing, setMissing] = useState<{ kind: Unavailable; reason: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  usePolling(async (current) => {
    const rebuild = cursor.current === null;
    try {
      const page = await api.historyAfter(executionId, row.id, cursor.current ?? 0, eventPageSize);
      if (!current()) return;
      useConversationCache.getState().page(key, { records: historyRecords(page), next: page.next, session: page.session }, rebuild, conversationKeep);
      cursor.current = page.next;
      setError(null);
    } catch (failure) {
      if (!current()) return;
      const kind = unavailableOf(failure);
      if (kind) setMissing({ kind, reason: errorText(failure) }); else setError(errorText(failure));
    }
  }, refreshMs, [api, key, eventPageSize, conversationKeep], { enabled: !missing });
  const turns = useMemo(() => reduceConversation(cached.records, "turn"), [cached.records]);
  const { decisions, reload } = useDecisionList(api, asksPermission(turns));
  if (missing) return <NotAvailable kind={missing.kind} reason={missing.reason} onRetry={() => setMissing(null)} />;
  const talkable = canTalk(row) && cached.session?.state !== "stopped";
  const working = cached.session?.state === "busy" || cached.pending.length > 0 || turns.some((turn) => turn.open);
  const send = async (text: string) => {
    try {
      const { turn } = await api.agentSend(executionId, row.id, text);
      useConversationCache.getState().addPending(key, { text, at: Date.now(), turn });
      setError(null);
      return true;
    } catch (failure) { setError(errorText(failure)); return false; }
  };
  const cancel = async () => {
    try { await api.agentCancel(executionId, row.id); setError(null); } catch (failure) { setError(errorText(failure)); }
  };
  return <div className="conversation">
    <SessionLine session={cached.session} />
    <Transcript api={api} turns={turns} pending={cached.pending} agentName={row.name}
      decisionOf={(item) => decisionFor(decisions, executionId, row.id, item.id)} onAnswered={reload} />
    {error && <p className="operate-error" role="alert">{error}</p>}
    {!talkable && <p className="operate-subtle">{t("operate.conversation.readOnly")}</p>}
    <div className="conversation-actions">
      {working && talkable && <Button size="sm" variant="outline" onClick={() => void cancel()}><Square size={13} />{t("operate.conversation.cancel")}</Button>}
    </div>
    <Composer label={fill(t("operate.conversation.prompt"), { name: row.name })} disabled={!talkable} onSend={send} />
  </div>;
}

/** Picks where turns go in and replies come out; nothing is bound until the person says so. */
function BindingForm({ workflow, initial, onBind, onCancel }: {
  workflow: Workflow; initial: ConversationBinding | null; onBind: (binding: ConversationBinding) => void; onCancel?: () => void;
}) {
  const ports = conversationPorts(workflow);
  const [input, setInput] = useState(initial?.input ?? ports.inputs[0]?.id ?? "");
  const [output, setOutput] = useState(initial?.output ?? ports.outputs[0]?.id ?? "");
  return <form className="conversation-bind" onSubmit={(e) => { e.preventDefault(); onBind({ input, output }); }}>
    <p>{t("operate.conversation.bindHelp")}</p>
    <label className="operate-field"><span>{t("operate.conversation.bindInput")}</span>
      <select value={input} onChange={(e) => setInput(e.target.value)}>{ports.inputs.map((port) => <option key={port.id} value={port.id}>{port.name} · {port.capability}</option>)}</select></label>
    <label className="operate-field"><span>{t("operate.conversation.bindOutput")}</span>
      <select value={output} onChange={(e) => setOutput(e.target.value)}>{ports.outputs.map((port) => <option key={port.id} value={port.id}>{port.name} · {port.capability}</option>)}</select></label>
    <div className="decision-actions">
      <Button type="submit" size="sm" disabled={!input || !output}><MessageSquareText size={14} />{t("operate.conversation.bind")}</Button>
      {onCancel && <Button type="button" size="sm" variant="ghost" onClick={onCancel}>{t("operate.conversation.keep")}</Button>}
    </div>
  </form>;
}

/** A workflow bound as a conversation: turns on its input, replies on its output, activity of the agents inside. */
function BoundConversation({ api, execution, workflow, binding }: { api: OperateApi; execution: Execution; workflow: Workflow; binding: ConversationBinding }) {
  const { refreshMs, eventPageSize, conversationKeep } = useOperateSettings();
  const key = `workflow:${execution.id}:${binding.input}>${binding.output}`;
  const cached = useConversationCache((s) => cachedConversation(s, key));
  const cursor = useCursor(api, key);
  const [error, setError] = useState<string | null>(null);
  usePolling(async (current) => {
    const rebuild = cursor.current === null;
    try {
      const page = await api.eventsAfter(execution.id, cursor.current ?? 0, eventPageSize);
      if (!current()) return;
      useConversationCache.getState().page(key, { records: busRecords(page.events, binding), next: page.next }, rebuild, conversationKeep);
      cursor.current = page.next;
      setError(null);
    } catch (failure) { if (current()) setError(errorText(failure)); }
  }, refreshMs, [api, key, eventPageSize, conversationKeep]);
  const turns = useMemo(() => reduceConversation(cached.records, "sequence"), [cached.records]);
  const { decisions, reload } = useDecisionList(api, true);
  // What waits for a person in this execution and is not behind a card of the conversation.
  const matched = new Set(turns.flatMap((turn) => turn.items.flatMap((item) =>
    item.type === "permission" && item.node ? [decisionFor(decisions, execution.id, item.node, item.id)?.id] : [])));
  const waiting = decisions.filter((pending) => pending.execution_id === execution.id && !matched.has(pending.id));
  const live = isLive(execution.state);
  const send = async (text: string) => {
    try {
      const { event } = await api.send(execution.id, binding.input, text);
      useConversationCache.getState().addPending(key, { text, at: Date.now(), event });
      setError(null);
      return true;
    } catch (failure) { setError(errorText(failure)); return false; }
  };
  return <div className="conversation">
    <Transcript api={api} turns={turns} pending={cached.pending} agentName={workflow.name}
      decisionOf={(item) => item.node ? decisionFor(decisions, execution.id, item.node, item.id) : undefined} onAnswered={reload} />
    {waiting.length > 0 && <section aria-label={t("operate.conversation.waiting")}>
      <h4>{t("operate.conversation.waiting")}</h4>
      <ul className="decisions">{waiting.map((pending) => <Decision key={pending.id} api={api} pending={pending} onDone={reload} />)}</ul>
    </section>}
    {error && <p className="operate-error" role="alert">{error}</p>}
    {!live && <p className="operate-subtle">{t("operate.conversation.readOnly")}</p>}
    <Composer label={fill(t("operate.conversation.promptWorkflow"), { port: binding.input })} disabled={!live} onSend={send} />
  </div>;
}

function WorkflowConversation({ api, execution, workflow }: { api: OperateApi; execution: Execution; workflow: Workflow }) {
  const { ownConversations, configuredConversations, conversationsFrom, bindConversation } = useOperateSettings();
  const bound = bindingFor(ownConversations, configuredConversations, execution.profile_id, workflow);
  const [changing, setChanging] = useState(false);
  const bind = (binding: ConversationBinding | null) => { bindConversation(execution.profile_id, workflow.id, binding); setChanging(false); };
  if (!bound || changing) return <BindingForm workflow={workflow} initial={bound?.binding ?? null} onBind={bind} onCancel={bound ? () => setChanging(false) : undefined} />;
  return <>
    <p className="conversation-binding">
      <WorkflowIcon size={13} aria-hidden="true" />
      <span>{fill(t("operate.conversation.bound"), { input: bound.binding.input, output: bound.binding.output })}</span>
      <small>{bound.source === "own" ? t("operate.conversation.boundHere") : fill(t("operate.conversation.boundFrom"), { file: conversationsFrom ?? "config.toml" })}</small>
      <button type="button" className="link-button" onClick={() => setChanging(true)}>{t("operate.conversation.change")}</button>
      {bound.source === "own" && <button type="button" className="link-button" onClick={() => bind(null)}>{t("operate.conversation.forget")}</button>}
    </p>
    <BoundConversation key={`${bound.binding.input}>${bound.binding.output}`} api={api} execution={execution} workflow={workflow} binding={bound.binding} />
  </>;
}

/** The panel: a subject picker when there is more than one, then its conversation. */
export function ConversationPanel({ api, execution, workflow, rows, subjects, subject, onSubject }: {
  api: OperateApi; execution: Execution; workflow: Workflow | undefined; rows: NodeRow[];
  subjects: Subject[]; subject: Subject | null; onSubject: (subject: Subject) => void;
}) {
  const label = (item: Subject) => item.kind === "workflow"
    ? fill(t("operate.conversation.subjectWorkflow"), { name: workflow?.name ?? execution.workflow_id })
    : fill(t("operate.conversation.subjectAgent"), { name: rows.find((row) => row.id === item.nodeId)?.name ?? item.nodeId });
  const at = subjects.findIndex((item) => sameSubject(item, subject));
  const row = subject?.kind === "agent" ? rows.find((item) => item.id === subject.nodeId) : undefined;
  return <section className="operate-panel conversation-panel" aria-label={t("operate.conversation.title")}>
    <header><MessageSquareText size={16} /><h3>{t("operate.conversation.title")}</h3>
      {subjects.length > 1 && <label className="operate-inline">{t("operate.conversation.subject")}
        <select value={at} onChange={(e) => onSubject(subjects[Number(e.target.value)])}>
          {subjects.map((item, index) => <option key={index} value={index}>{label(item)}</option>)}
        </select></label>}
    </header>
    {subject?.kind === "workflow" && workflow ? <WorkflowConversation key={`${execution.id}/${workflow.id}`} api={api} execution={execution} workflow={workflow} />
      : row ? <AgentConversation key={`${execution.id}/${row.id}`} api={api} executionId={execution.id} row={row} />
        : <p className="operate-empty">{t("operate.conversation.none")}</p>}
  </section>;
}
