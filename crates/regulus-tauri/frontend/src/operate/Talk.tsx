// The right-hand panels of an execution: talking to an agent, a channel's log, and the
// decisions waiting for a person. Each degrades to "not installed" without its module.
import { useState, type FormEvent } from "react";
import { Bot, Check, MessagesSquare, Send, UserRound, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import type { OperateApi } from "./client";
import { agentAnswer, canTalk, errorText, fill, unavailableOf, type NodeRow, type Unavailable } from "./logic";
import type { ChannelMessage, Participant, PendingDecision } from "./model";
import { NotAvailable } from "./NotAvailable";
import { isThinking, turnsOf, useOperateSession } from "./session";
import { useOperateSettings } from "./settings";
import { NodeStateMark, nodeReason } from "./StateMark";
import { usePolling } from "./usePolling";
import { ValueView } from "./ValueView";

function Composer({ label, busy, onSend }: { label: string; busy?: boolean; onSend: (text: string) => Promise<boolean> }) {
  const [text, setText] = useState("");
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const value = text.trim();
    if (!value || busy) return;
    if (await onSend(value)) setText("");
  };
  return <form className="operate-composer" onSubmit={submit}>
    <textarea aria-label={label} placeholder={label} value={text} rows={2} onChange={(e) => setText(e.target.value)}
      onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void submit(e); } }} />
    <Button type="submit" size="sm" disabled={busy || !text.trim()}><Send size={14} />{t("operate.send")}</Button>
  </form>;
}

function AgentChat({ api, executionId, row }: { api: OperateApi; executionId: string; row: NodeRow }) {
  const timeout = useOperateSettings((s) => s.agentTimeoutMs);
  const turns = useOperateSession((s) => turnsOf(s, executionId, row.id)) ?? [];
  const thinking = useOperateSession((s) => isThinking(s, executionId, row.id));
  const { add, setThinking } = useOperateSession.getState();
  const [missing, setMissing] = useState<{ kind: Unavailable; reason: string } | null>(null);
  const send = async (text: string) => {
    add(executionId, row.id, { role: "prompt", text });
    setThinking(executionId, row.id, true);
    try {
      add(executionId, row.id, { role: "answer", ...agentAnswer(await api.prompt(executionId, row.id, text, timeout)) });
    } catch (error) {
      const kind = unavailableOf(error);
      if (kind) setMissing({ kind, reason: errorText(error) });
      add(executionId, row.id, { role: "error", text: errorText(error) });
    } finally {
      setThinking(executionId, row.id, false);
    }
    return true;
  };
  if (missing) return <NotAvailable kind={missing.kind} reason={missing.reason} />;
  return <>
    <ol className="operate-turns" aria-live="polite">
      {turns.length === 0 && <li className="operate-empty">{t("operate.agent.empty")}</li>}
      {turns.map((turn, index) => <li key={index} className={`turn turn-${turn.role}`}>
        <span className="turn-who">{turn.role === "prompt" ? <UserRound size={13} /> : <Bot size={13} />}{t(turn.role === "prompt" ? "operate.agent.you" : turn.role === "error" ? "operate.agent.error" : "operate.agent.agent")}</span>
        <p>{turn.text}</p>
        {turn.role === "answer" && turn.stopReason && <small>{fill(t("operate.agent.stopped"), { reason: turn.stopReason })}</small>}
      </li>)}
      {thinking && <li className="turn turn-thinking">{t("operate.agent.thinking")}</li>}
    </ol>
    <Composer label={t("operate.agent.prompt")} busy={thinking} onSend={send} />
  </>;
}

function ChannelPanel({ api, executionId, row }: { api: OperateApi; executionId: string; row: NodeRow }) {
  const refreshMs = useOperateSettings((s) => s.refreshMs);
  const [state, setState] = useState<{ messages: ChannelMessage[]; participants: Participant[] } | null>(null);
  const [missing, setMissing] = useState<{ kind: Unavailable; reason: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = async () => {
    try { setState(await api.channel(executionId, row.id)); setError(null); }
    catch (failure) { const kind = unavailableOf(failure); if (kind) setMissing({ kind, reason: errorText(failure) }); else setError(errorText(failure)); }
  };
  usePolling(load, refreshMs, [executionId, row.id, api]);
  if (missing) return <NotAvailable kind={missing.kind} reason={missing.reason} />;
  const send = async (text: string) => {
    try { await api.say(executionId, row.id, text); setError(null); await load(); return true; }
    catch (failure) { setError(errorText(failure)); return false; }
  };
  return <>
    <h4>{t("operate.channel.participants")}</h4>
    <ul className="operate-participants">
      {state?.participants.length === 0 && <li className="operate-empty">{t("operate.channel.noParticipants")}</li>}
      {state?.participants.map((item) => <li key={item.name}><span>{item.name}</span><small>{[item.kind, item.state, item.queued ? fill(t("operate.channel.queued"), { count: item.queued }) : null].filter(Boolean).join(" · ")}</small></li>)}
    </ul>
    <h4>{t("operate.channel.messages")}</h4>
    <ol className="operate-turns">
      {state?.messages.length === 0 && <li className="operate-empty">{t("operate.channel.empty")}</li>}
      {state?.messages.map((message) => <li key={message.id} className={`turn ${message.error ? "turn-error" : ""}`}>
        <span className="turn-who">{message.from}{message.to?.length ? ` → ${message.to.join(", ")}` : ""}</span>
        <p>{message.error ?? message.text}</p>
      </li>)}
    </ol>
    {error && <p className="operate-error" role="alert">{error}</p>}
    <Composer label={fill(t("operate.channel.sendAs"), { sender: "user" })} onSend={send} />
  </>;
}

/** What the selected node says: a conversation, a channel, or why it says nothing. */
export function NodePanel({ api, executionId, row }: { api: OperateApi; executionId: string; row: NodeRow }) {
  const reason = nodeReason(row.state);
  return <section className="operate-panel node-panel" aria-label={row.name}>
    <header>
      {row.kind === "agent" ? <Bot size={16} /> : row.kind === "channel" ? <MessagesSquare size={16} /> : null}
      <h3>{row.name}</h3><NodeStateMark state={row.state} />
    </header>
    <p className="operate-subtle">{row.type}{reason && <> · {reason}</>}</p>
    {row.kind === "other" ? <p className="operate-empty">{t("operate.node.silent")}</p>
      : !canTalk(row) ? <p className="operate-empty">{t("operate.node.notRunning")}</p>
        : row.kind === "agent" ? <AgentChat key={`${executionId}/${row.id}`} api={api} executionId={executionId} row={row} />
          : <ChannelPanel key={`${executionId}/${row.id}`} api={api} executionId={executionId} row={row} />}
  </section>;
}

function Decision({ api, pending, onDone }: { api: OperateApi; pending: PendingDecision; onDone: () => void }) {
  const fields = Object.entries(pending.form?.properties ?? {});
  const [values, setValues] = useState<Record<string, string>>({});
  const [text, setText] = useState(pending.kind === "review" && typeof pending.value === "string" ? pending.value : "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const answer = async (decision: "approve" | "reject") => {
    setBusy(true);
    try {
      const edited = pending.kind === "review" && text === pending.value ? null : text.trim() ? text : null;
      await api.answer(pending.id, decision, values, edited);
      onDone();
    } catch (failure) { setError(errorText(failure)); } finally { setBusy(false); }
  };
  const seconds = (ms: number) => Math.max(0, Math.round(ms / 1000));
  return <li className="decision">
    <header><strong>{pending.prompt || pending.id}</strong><small>{pending.kind} · {pending.node_id} · {fill(t("operate.human.age"), { seconds: seconds(Date.now() - pending.created_ms) })}{pending.deadline_ms ? ` · ${fill(t("operate.human.deadline"), { seconds: seconds(pending.deadline_ms - Date.now()) })}` : ""}</small></header>
    {pending.kind !== "review" && <ValueView value={pending.value} />}
    {fields.map(([name, field]) => <label key={name} className="operate-field"><span>{name}{pending.form?.required?.includes(name) ? " *" : ""}</span>
      <input value={values[name] ?? ""} onChange={(e) => setValues({ ...values, [name]: e.target.value })} placeholder={field.type} />
      {field.description && <small>{field.description}</small>}</label>)}
    <label className="operate-field"><span>{t(pending.kind === "review" ? "operate.human.edit" : "operate.human.comment")}</span>
      <textarea rows={pending.kind === "review" ? 5 : 2} value={text} onChange={(e) => setText(e.target.value)} /></label>
    {error && <p className="operate-error" role="alert">{error}</p>}
    <div className="decision-actions">
      <Button size="sm" disabled={busy} onClick={() => void answer("approve")}><Check size={14} />{t("operate.human.approve")}</Button>
      <Button size="sm" variant="outline" disabled={busy} onClick={() => void answer("reject")}><X size={14} />{t("operate.human.reject")}</Button>
    </div>
  </li>;
}

/** Decisions waiting in this execution (module `human`); nothing at all without the module. */
export function Decisions({ api, executionId }: { api: OperateApi; executionId: string }) {
  const refreshMs = useOperateSettings((s) => s.refreshMs);
  const [pending, setPending] = useState<PendingDecision[]>([]);
  const [absent, setAbsent] = useState(false);
  const load = async () => {
    try { setPending(await api.decisions(executionId)); }
    catch (error) { if (unavailableOf(error)) setAbsent(true); }
  };
  usePolling(load, refreshMs, [executionId, api]);
  if (absent || pending.length === 0) return null;
  return <section className="operate-panel" aria-label={t("operate.human.title")}>
    <header><UserRound size={16} /><h3>{t("operate.human.title")}</h3><span className="operate-count">{pending.length}</span></header>
    <ul className="decisions">{pending.map((item) => <Decision key={item.id} api={api} pending={item} onDone={() => void load()} />)}</ul>
  </section>;
}
