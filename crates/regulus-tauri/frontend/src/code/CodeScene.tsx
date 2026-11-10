// The Regulus Code view: an agent that works in one folder. It is a focused projection of
// what Operate, Templates and the canvas already do, over the same methods: a session is an
// execution of a profile whose workflow has a code workspace and an agent, found by what
// its nodes offer, never by an id. Every action here has its CLI command (`regulus code`).
import { useMemo, useState } from "react";
import { RefreshCw, SlidersHorizontal } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useCore } from "@/core/connection";
import { t } from "@/lib/i18n";
import { operateApi } from "@/operate/client";
import { fill } from "@/operate/logic";
import { templatesApi } from "@/templates/client";
import { codeApi } from "./client";
import { useCodeData, useCodeLabel, type CodeApis } from "./data";
import { Launchpad } from "./Launchpad";
import { codeSessions, codeTargets } from "./logic";
import { Mascot } from "./Mascot";
import { Session } from "./Session";
import { useCodeSettings } from "./settings";
import { ViewSettings } from "./ViewSettings";
import "@/operate/operate.css";
import "./code.css";

export default function CodeScene() {
  const { status, client, reason, connect, source } = useCore();
  const api = useMemo<CodeApis | null>(() => client ? { code: codeApi(client), operate: operateApi(client), templates: templatesApi(client) } : null, [client]);
  const { data, error, dismiss } = useCodeData(api);
  const label = useCodeLabel();
  const showMascot = useCodeSettings((s) => s.mascot);
  const executionId = useCodeSettings((s) => s.executionId);
  const openExecution = useCodeSettings((s) => s.openExecution);
  const [showSettings, setShowSettings] = useState(false);
  const targets = useMemo(() => data ? codeTargets(data.profiles, data.manifests) : [], [data?.profiles, data?.manifests]); // eslint-disable-line react-hooks/exhaustive-deps
  const sessions = useMemo(() => data ? codeSessions(data.executions, targets) : [], [data?.executions, targets]); // eslint-disable-line react-hooks/exhaustive-deps
  const open = sessions.find((session) => session.execution.id === executionId) ?? null;
  const ready = status === "ready" && api && data;

  if (!ready) return <section className="code-scene">
    <div className="code-connection" role="status">
      {showMascot && <Mascot scale={6} mood={status === "loading" || status === "ready" ? "idle" : "stopped"} decorative />}
      <h2>{status === "loading" || status === "ready" ? fill(t("code.connecting"), { label }) : t(status === "incompatible" ? "graph.incompatible" : "shell.offline")}</h2>
      {status !== "loading" && status !== "ready" && <p>{t(source === "mock" ? "graph.offlineDescription" : "graph.offlineCore")}</p>}
      {reason && status !== "loading" && <code className="operate-reason">{reason}</code>}
      {status !== "loading" && status !== "ready" && <Button variant="outline" onClick={connect}>{t("graph.retry")}</Button>}
    </div>
  </section>;

  const settingsButton = <Button variant="ghost" size="sm" aria-pressed={showSettings} onClick={() => setShowSettings(!showSettings)}><SlidersHorizontal size={14} />{t("operate.settings.title")}</Button>;
  const notice = error && <p className="operate-notice tone-danger" role="alert">{error}<button type="button" className="link-button" onClick={dismiss}>{t("operate.dismiss")}</button></p>;
  return <section className="code-scene">
    {open
      ? <Session key={open.execution.id} api={api} data={data} session={open} sessions={sessions} tools={settingsButton} banner={<>{showSettings && <ViewSettings />}{notice}</>}
        onLeave={() => openExecution(null)} onOpen={openExecution} />
      : <>
        <header className="code-topbar">
          <span className="graph-source">{t(source === "mock" ? "graph.mock" : "graph.core")}</span>
          <Button variant="ghost" size="sm" onClick={() => void data.reload()}><RefreshCw size={14} />{t("operate.reload")}</Button>
          {settingsButton}
        </header>
        {showSettings && <ViewSettings />}
        {notice}
        <Launchpad api={api} data={data} targets={targets} sessions={sessions} onOpen={openExecution} />
      </>}
  </section>;
}
