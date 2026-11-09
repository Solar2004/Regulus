// The right-hand panels of an execution: the selected node (an agent's conversation, a
// channel's log) and the decisions waiting for a person. Each degrades to "not installed"
// without its module.
import { useState } from "react";
import { Bot, MessageSquareText, MessagesSquare, UserRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import type { OperateApi } from "./client";
import { Composer } from "./Composer";
import { AgentConversation } from "./Conversation";
import { Decision } from "./Decision";
import { canTalk, errorText, fill, unavailableOf, type NodeRow, type Unavailable } from "./logic";
import type { ChannelMessage, Participant, PendingDecision } from "./model";
import { NotAvailable } from "./NotAvailable";
import { useOperateSettings } from "./settings";
import { NodeStateMark, nodeReason } from "./StateMark";
import { usePolling, type StillCurrent } from "./usePolling";

function ChannelPanel({ api, executionId, row }: { api: OperateApi; executionId: string; row: NodeRow }) {
  const refreshMs = useOperateSettings((s) => s.refreshMs);
  const [state, setState] = useState<{ messages: ChannelMessage[]; participants: Participant[] } | null>(null);
  const [missing, setMissing] = useState<{ kind: Unavailable; reason: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = async (current: StillCurrent = () => true) => {
    try { const loaded = await api.channel(executionId, row.id); if (current()) { setState(loaded); setError(null); } }
    catch (failure) {
      if (!current()) return;
      const kind = unavailableOf(failure);
      if (kind) setMissing({ kind, reason: errorText(failure) }); else setError(errorText(failure));
    }
  };
  // No polling while the module is absent: retry starts it again.
  usePolling(load, refreshMs, [executionId, row.id, api], { enabled: !missing });
  if (missing) return <NotAvailable kind={missing.kind} reason={missing.reason} onRetry={() => setMissing(null)} />;
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

/**
 * What the selected node says: a conversation, a channel, or why it says nothing. An
 * agent's conversation shows in the Conversation panel when that panel is shown.
 */
export function NodePanel({ api, executionId, row, conversationShown, onTalk }: {
  api: OperateApi; executionId: string; row: NodeRow; conversationShown: boolean; onTalk: () => void;
}) {
  const reason = nodeReason(row.state);
  return <section className="operate-panel node-panel" aria-label={row.name}>
    <header>
      {row.kind === "agent" ? <Bot size={16} /> : row.kind === "channel" ? <MessagesSquare size={16} /> : null}
      <h3>{row.name}</h3><NodeStateMark state={row.state} />
    </header>
    <p className="operate-subtle">{row.type}{reason && <> · {reason}</>}</p>
    {row.kind === "other" ? <p className="operate-empty">{t("operate.node.silent")}</p>
      : row.kind === "agent" ? (conversationShown
        ? <div className="operate-note"><p>{t("operate.conversation.inPanel")}</p>
          <Button variant="outline" size="sm" onClick={onTalk}><MessageSquareText size={14} />{t("operate.conversation.showInPanel")}</Button></div>
        // History reads for a stopped node too; sending waits for a live one.
        : <AgentConversation key={`${executionId}/${row.id}`} api={api} executionId={executionId} row={row} />)
        : !canTalk(row) ? <p className="operate-empty">{t("operate.node.notRunning")}</p>
          : <ChannelPanel key={`${executionId}/${row.id}`} api={api} executionId={executionId} row={row} />}
  </section>;
}

/** Decisions waiting in this execution (module `human`); nothing at all without the module. */
export function Decisions({ api, executionId }: { api: OperateApi; executionId: string }) {
  const refreshMs = useOperateSettings((s) => s.refreshMs);
  const [pending, setPending] = useState<PendingDecision[]>([]);
  const [absent, setAbsent] = useState(false);
  const load = async (current: StillCurrent = () => true) => {
    try { const listed = await api.decisions(executionId); if (current()) setPending(listed); }
    catch (error) { if (current() && unavailableOf(error)) setAbsent(true); }
  };
  // Without the human module there is nothing to ask for until the execution changes.
  usePolling(load, refreshMs, [executionId, api], { enabled: !absent });
  if (absent || pending.length === 0) return null;
  return <section className="operate-panel" aria-label={t("operate.human.title")}>
    <header><UserRound size={16} /><h3>{t("operate.human.title")}</h3><span className="operate-count">{pending.length}</span></header>
    <ul className="decisions">{pending.map((item) => <Decision key={item.id} api={api} pending={item} onDone={() => void load()} />)}</ul>
  </section>;
}
