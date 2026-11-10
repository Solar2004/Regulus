// The workers of the session: executions the agent started through its delegate node
// (`delegation.workers`, as `regulus workers ls`), each with its own conversation. They are
// Regulus executions, not a CLI's subagents: listed, read and closed like any other.
import { useState } from "react";
import { CircleCheck, CircleStop, Loader, MessageSquareText, TriangleAlert, X, type LucideIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { CompositionNode, Workflow } from "@/composition/model";
import { t, type LocaleKey } from "@/lib/i18n";
import { AgentConversation } from "@/operate/Conversation";
import { errorText, fill, nodeRows } from "@/operate/logic";
import { Mark } from "@/operate/StateMark";
import type { CodeApis, CodeData } from "./data";
import { codeAgents, isWorkerLive, type CodeSession } from "./logic";
import type { Worker } from "./model";
import { Problem } from "./Problem";
import type { Asked } from "./session";

type Tone = "success" | "warning" | "danger" | "muted" | "info";
const states: Record<string, { icon: LucideIcon; tone: Tone; label: LocaleKey }> = {
  starting: { icon: Loader, tone: "info", label: "operate.execution.starting" },
  running: { icon: CircleCheck, tone: "success", label: "operate.execution.running" },
  closed: { icon: CircleStop, tone: "muted", label: "code.workers.closed" },
  stopped: { icon: CircleStop, tone: "muted", label: "operate.execution.stopped" },
  failed: { icon: TriangleAlert, tone: "danger", label: "operate.execution.failed" },
  interrupted: { icon: TriangleAlert, tone: "warning", label: "operate.execution.interrupted" },
};

function WorkerState({ state }: { state: string }) {
  const known = states[state];
  // A state this app does not know shows as the module names it.
  return known ? <Mark icon={known.icon} tone={known.tone} label={t(known.label)} /> : <Mark icon={CircleStop} tone="muted" label={state || "—"} />;
}

function WorkerRow({ api, data, session, delegate, worker, onChanged }: {
  api: CodeApis; data: CodeData; session: CodeSession; delegate: CompositionNode; worker: Worker; onChanged: () => void;
}) {
  const [reading, setReading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const child = data.executions.find((execution) => execution.id === worker.execution);
  const workflow: Workflow | undefined = child && data.profiles.find((profile) => profile.id === child.profile_id)?.workflows.find((item) => item.id === child.workflow_id);
  const agent = child ? nodeRows(workflow, child, data.modules).find((row) => codeAgents(workflow, data.manifests).some((node) => node.id === row.id)) : undefined;
  const close = async () => {
    setBusy(true);
    try { await api.code.closeWorker(session.execution.id, delegate.id, worker.worker); setError(null); onChanged(); }
    catch (failure) { setError(errorText(failure)); } finally { setBusy(false); }
  };
  return <li className="code-worker">
    <header>
      <strong>{worker.name || worker.worker}</strong>
      <small>{[worker.worker !== worker.name ? worker.worker : null, worker.kind, worker.execution].filter(Boolean).join(" · ")}</small>
      <WorkerState state={worker.state} />
    </header>
    <p className="code-fine">
      {fill(t(worker.pending_results === 1 ? "code.workers.resultsOne" : "code.workers.results"), { count: worker.pending_results })}
      {worker.last_output_at ? ` · ${fill(t("code.workers.lastOutput"), { time: new Date(worker.last_output_at).toLocaleTimeString() })}` : ""}
    </p>
    <div className="decision-actions">
      {child && agent && <Button size="sm" variant="outline" aria-expanded={reading} onClick={() => setReading(!reading)}><MessageSquareText size={13} />{t(reading ? "code.workers.hide" : "code.workers.read")}</Button>}
      {isWorkerLive(worker) && <Button size="sm" variant="ghost" disabled={busy} onClick={() => void close()}><X size={13} />{t("code.workers.close")}</Button>}
    </div>
    {error && <p className="operate-error" role="alert">{error}</p>}
    {reading && child && agent && <div className="code-worker-talk"><AgentConversation key={`${child.id}/${agent.id}`} api={api.operate} executionId={child.id} row={agent} /></div>}
  </li>;
}

export function WorkersPanel({ api, data, session, delegate, workers }: {
  api: CodeApis; data: CodeData; session: CodeSession; delegate: CompositionNode; workflow: Workflow | undefined; workers: Asked<Worker[]>;
}) {
  if (!session.live) return <p className="operate-empty">{t("code.workers.stopped")}</p>;
  if (workers.failure) return <Problem failure={workers.failure} onRetry={workers.reload} />;
  if (!workers.data) return <p className="operate-empty">{t("code.workers.reading")}</p>;
  if (workers.data.length === 0) return <p className="operate-empty">{fill(t("code.workers.empty"), { node: delegate.name || delegate.id })}</p>;
  return <ul className="code-workers" aria-label={t("code.panel.workers")}>{workers.data.map((worker) =>
    <WorkerRow key={worker.worker} api={api} data={data} session={session} delegate={delegate} worker={worker} onChanged={workers.reload} />)}</ul>;
}
