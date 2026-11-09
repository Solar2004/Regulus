// The Operate view (docs/04-client.md §2, §9): what runs now. Mirrors the TUI
// (crates/regulus-cli/src/tui/) over the same methods; every action has its CLI command.
import { Fragment, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { Activity, ArrowDown, ArrowUp, Bot, Crosshair, GitFork, MessagesSquare, Play, RefreshCw, RotateCcw, Send, SlidersHorizontal, Square } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { Profile } from "@/composition/model";
import { useCore } from "@/core/connection";
import { t, type LocaleKey } from "@/lib/i18n";
import { operateApi, type OperateApi } from "./client";
import {
  errorText, eventTouches, fill, inputPorts, isLive, mergeEvents, nodeRows, parseSendValue, portName,
  projectsOf, runnableWorkflows, workflowOf, type NodeRow,
} from "./logic";
import type { BusEvent, Execution, ModuleSummary } from "./model";
import { arrangePanels, changePanel, movePanel, panelRegistry, type PanelContext, type PanelId, type Placement } from "./panels";
import { operateLimits, useOperateSettings, type OperateSettingKey } from "./settings";
import { descendants, loadTree, type ExecutionTree } from "./tree";
import { ExecutionStateMark, NodeStateMark } from "./StateMark";
import { Decisions, NodePanel } from "./Talk";
import { usePolling } from "./usePolling";
import { ValueView } from "./ValueView";
import "./operate.css";

type Notice = { tone: "info" | "danger"; text: string } | null;

function SettingsPanel() {
  const settings = useOperateSettings();
  const keys = Object.keys(operateLimits) as OperateSettingKey[];
  return <section className="operate-settings" aria-label={t("operate.settings.title")}>
    {keys.map((key) => <label key={key} className="operate-field">
      <span>{t(`operate.settings.${key}` as LocaleKey)}</span>
      <input type="number" min={operateLimits[key].min} max={operateLimits[key].max} value={settings[key]}
        onChange={(e) => settings.setSetting(key, Number(e.target.value))} />
      <small>{t(`operate.settings.${key}Description` as LocaleKey)}</small>
      {settings.overrides[key] === undefined && settings.configuredFrom[key] && <small className="operate-source">{t("operate.settings.fromFile", { file: settings.configuredFrom[key] })}</small>}
    </label>)}
    <PanelLayout />
    <Button variant="outline" size="sm" onClick={settings.reset}><RotateCcw size={14} />{t("operate.settings.reset")}</Button>
  </section>;
}

/** Order, column and visibility of the execution's panels; saved on this device like the numbers above. */
function PanelLayout() {
  const { panels, ownPanels, panelsFrom, setPanels } = useOperateSettings();
  const title = (id: PanelId) => t(panelRegistry.find((panel) => panel.id === id)!.title);
  return <fieldset className="operate-panels-layout">
    <legend>{t("operate.panels.title")}</legend>
    <small>{t("operate.panels.help")}</small>
    {!ownPanels && panelsFrom && <small className="operate-source">{t("operate.settings.fromFile", { file: panelsFrom })}</small>}
    <ol>{panels.map((entry, index) => <li key={entry.id}>
      <label className="operate-check"><input type="checkbox" checked={!entry.hidden} onChange={(e) => setPanels(changePanel(panels, entry.id, { hidden: !e.target.checked }))} />{title(entry.id)}</label>
      <select aria-label={fill(t("operate.panels.place"), { panel: title(entry.id) })} value={entry.place} onChange={(e) => setPanels(changePanel(panels, entry.id, { place: e.target.value as Placement }))}>
        <option value="main">{t("operate.panels.main")}</option><option value="aside">{t("operate.panels.aside")}</option>
      </select>
      <Button variant="ghost" size="icon-sm" disabled={index === 0} aria-label={fill(t("operate.panels.up"), { panel: title(entry.id) })} onClick={() => setPanels(movePanel(panels, entry.id, -1))}><ArrowUp size={13} /></Button>
      <Button variant="ghost" size="icon-sm" disabled={index === panels.length - 1} aria-label={fill(t("operate.panels.down"), { panel: title(entry.id) })} onClick={() => setPanels(movePanel(panels, entry.id, 1))}><ArrowDown size={13} /></Button>
    </li>)}</ol>
  </fieldset>;
}

function ProfileRow({ profile, onRun, busy }: { profile: Profile; onRun: (workflowId: string | undefined) => void; busy: boolean }) {
  const runnable = runnableWorkflows(profile);
  const [workflowId, setWorkflowId] = useState(profile.main_workflow_id);
  return <li className="operate-row">
    <div><strong>{profile.name}</strong><small>{profile.id}{profile.project_id ? ` · ${profile.project_id}` : ""}</small></div>
    {runnable.length > 1 && <select aria-label={t("operate.workflow")} value={workflowId} onChange={(e) => setWorkflowId(e.target.value)}>
      {runnable.map((workflow) => <option key={workflow.id} value={workflow.id}>{workflow.name}</option>)}
    </select>}
    <Button size="sm" disabled={busy} onClick={() => onRun(workflowId === profile.main_workflow_id ? undefined : workflowId)}><Play size={14} />{t("operate.run")}</Button>
  </li>;
}

function SendForm({ api, execution, ports, onSent }: { api: OperateApi; execution: Execution; ports: ReturnType<typeof inputPorts>; onSent: (notice: Notice) => void }) {
  const [portId, setPortId] = useState(ports[0]?.id ?? "");
  const [raw, setRaw] = useState("");
  const port = ports.find((item) => item.id === portId) ?? ports[0];
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!port) return;
    const parsed = parseSendValue(port.capability, raw);
    if (!parsed.ok) { onSent({ tone: "danger", text: t("operate.sendForm.notJson") }); return; }
    try {
      const sent = await api.send(execution.id, port.id, parsed.value);
      onSent({ tone: "info", text: fill(t("operate.sendForm.sent"), { port: port.id, event: sent.event }) });
      setRaw("");
    } catch (error) { onSent({ tone: "danger", text: errorText(error) }); }
  };
  return <form className="operate-send" onSubmit={submit} aria-label={t("operate.sendForm.title")}>
    <label className="operate-field"><span>{t("operate.sendForm.port")}</span>
      <select value={port?.id} onChange={(e) => setPortId(e.target.value)}>{ports.map((item) => <option key={item.id} value={item.id}>{item.name} · {item.capability}</option>)}</select></label>
    <label className="operate-field grow"><span>{t("operate.sendForm.value")}</span>
      <input value={raw} onChange={(e) => setRaw(e.target.value)} placeholder={t(port && port.capability.startsWith("text@") ? "operate.sendForm.textHint" : "operate.sendForm.jsonHint")} /></label>
    <Button type="submit" size="sm" disabled={!isLive(execution.state)}><Send size={14} />{t("operate.send")}</Button>
  </form>;
}

const kindKey: Record<BusEvent["kind"], LocaleKey> = {
  emitted: "operate.events.emitted", delivered: "operate.events.delivered", failed: "operate.events.failed", cut: "operate.events.cut",
};

function EventLog({ api, execution, nodes }: { api: OperateApi; execution: Execution; nodes: NodeRow[] }) {
  const { refreshMs, eventPageSize, eventKeep } = useOperateSettings();
  const [events, setEvents] = useState<BusEvent[]>([]);
  const cursor = useRef(0);
  const [pending, setPending] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [node, setNode] = useState("");
  usePolling(async (current) => {
    try {
      const page = await api.eventsAfter(execution.id, cursor.current, eventPageSize);
      if (!current()) return;
      setEvents((current) => mergeEvents(current, page.events, eventKeep));
      cursor.current = page.next;
      setPending(page.pending);
      setError(null);
    } catch (failure) { if (current()) setError(errorText(failure)); }
  }, refreshMs, [api, execution.id, eventPageSize, eventKeep]);
  const shown = node ? events.filter((event) => eventTouches(event, node)) : events;
  return <section className="operate-panel events" aria-label={t("operate.events.title")}>
    <header><h3>{t("operate.events.title")}</h3><span className="operate-count">{shown.length}</span>
      {pending > 0 && <small className="operate-subtle">{fill(t("operate.events.pending"), { count: pending })}</small>}
      <label className="operate-inline">{t("operate.events.filter")}<select value={node} onChange={(e) => setNode(e.target.value)}>
        <option value="">{t("operate.events.allNodes")}</option><option value="self">self</option>
        {nodes.map((row) => <option key={row.id} value={row.id}>{row.name}</option>)}
      </select></label>
    </header>
    {error && <p className="operate-error" role="alert">{error}</p>}
    {shown.length === 0 ? <p className="operate-empty">{t("operate.events.empty")}</p> :
      <ol className="event-log">{shown.map((event) => <li key={event.seq} className={`event event-${event.kind}`}>
        <span className="event-seq">#{event.seq}</span>
        <time>{new Date(event.at).toLocaleTimeString()}</time>
        <span className="event-kind">{t(kindKey[event.kind])}</span>
        <span className="event-route">{portName(event.from)}{(event.kind === "delivered" || event.kind === "failed") && <> → {portName(event.to)}</>}</span>
        <span className="event-body">
          {event.kind === "emitted" && <><small>{event.capability}</small> <ValueView value={event.value} /></>}
          {event.kind === "failed" && <span className="operate-error">{event.error}</span>}
          {event.kind === "cut" && fill(t("operate.events.cutAt"), { depth: event.max_depth })}
        </span>
      </li>)}</ol>}
  </section>;
}

function TreeRows({ tree, currentId, onSelect, onStop }: { tree: ExecutionTree; currentId: string; onSelect: (id: string) => void; onStop: (id: string) => void }) {
  const { execution } = tree;
  const current = execution.id === currentId;
  return <li>
    <div className={`tree-row${current ? " is-current" : ""}`}>
      <GitFork size={13} aria-hidden="true" />
      <strong>{execution.id}</strong>
      <small>{execution.workflow_id}</small>
      <ExecutionStateMark state={execution.state} />
      {execution.caller && <small>{fill(t("operate.tree.caller"), { node: execution.caller })}</small>}
      {execution.tag && <small>{fill(t("operate.tree.tag"), { tag: execution.tag })}</small>}
      <span className="tree-actions">
        {!current && <Button size="sm" variant="ghost" aria-label={fill(t("operate.tree.select"), { id: execution.id })} onClick={() => onSelect(execution.id)}><Crosshair size={13} />{t("operate.tree.selectShort")}</Button>}
        {!current && isLive(execution.state) && <Button size="sm" variant="outline" aria-label={fill(t("operate.stopOne"), { id: execution.id })} onClick={() => onStop(execution.id)}><Square size={13} />{t("operate.stop")}</Button>}
      </span>
    </div>
    {tree.children.length > 0 && <ul>{tree.children.map((child) => <TreeRows key={child.execution.id} tree={child} currentId={currentId} onSelect={onSelect} onStop={onStop} />)}</ul>}
  </li>;
}

/** The executions this one started, at any depth (`execution.tree`, or the list's `parent` on an older core). */
function TreePanel({ api, execution, executions, onSelect, onStop }: {
  api: OperateApi; execution: Execution; executions: Execution[]; onSelect: (id: string) => void; onStop: (id: string) => void;
}) {
  const { refreshMs } = useOperateSettings();
  const [loaded, setLoaded] = useState<Awaited<ReturnType<typeof loadTree>> | null>(null);
  const [error, setError] = useState<string | null>(null);
  const list = useRef(executions);
  list.current = executions;
  usePolling(async (current) => {
    try {
      const next = await loadTree(api.tree, list.current, execution.id);
      if (!current()) return;
      setLoaded(next);
      setError(null);
    } catch (failure) { if (current()) setError(errorText(failure)); }
  }, refreshMs, [api, execution.id]);
  const tree = loaded?.tree;
  return <section className="operate-panel tree" aria-label={t("operate.tree.title")}>
    <header><h3>{t("operate.tree.title")}</h3>{tree && <span className="operate-count">{descendants(tree)}</span>}</header>
    {execution.parent && <p className="operate-subtle"><button type="button" className="link-button" onClick={() => onSelect(execution.parent!)}>{fill(t("operate.execution.childOf"), { parent: execution.parent })}</button></p>}
    {error && <p className="operate-error" role="alert">{error}</p>}
    {loaded?.source === "list" && <small className="operate-subtle">{t("operate.tree.fromList")}</small>}
    {tree && (tree.children.length ? <ul className="execution-tree"><TreeRows tree={tree} currentId={execution.id} onSelect={onSelect} onStop={onStop} /></ul>
      : <p className="operate-empty">{t("operate.tree.empty")}</p>)}
  </section>;
}

function ExecutionDetail({ api, execution, executions, profiles, modules, onStop, onNotice, onSelect }: {
  api: OperateApi; execution: Execution; executions: Execution[]; profiles: Profile[]; modules: ModuleSummary[];
  onStop: (id: string) => void; onNotice: (notice: Notice) => void; onSelect: (id: string) => void;
}) {
  const layout = useOperateSettings((state) => state.panels);
  const workflow = workflowOf(profiles, execution);
  const rows = nodeRows(workflow, execution, modules);
  const ports = inputPorts(workflow);
  const [nodeId, setNodeId] = useState<string | null>(null);
  const selected = rows.find((row) => row.id === nodeId) ?? null;
  const ctx: PanelContext = { execution, workflow, inputs: ports, children: executions.filter((item) => item.parent === execution.id) };
  const shown = arrangePanels(layout, ctx);
  // Each panel of the registry, drawn; the layout says which, where and in what order.
  const render: Record<PanelId, () => React.ReactNode> = {
    nodes: () => <section className="operate-panel" aria-label={t("operate.nodes.title")}>
      <header><h3>{t("operate.nodes.title")}</h3><span className="operate-count">{rows.length}</span></header>
      {!workflow && <p className="operate-empty">{t("operate.nodes.unknown")}</p>}
      <ul className="node-rows">{rows.map((row) => <li key={row.id}>
        <button type="button" aria-pressed={row.id === nodeId} onClick={() => setNodeId(row.id === nodeId ? null : row.id)}>
          <span className="node-name">{row.kind === "agent" ? <Bot size={14} /> : row.kind === "channel" ? <MessagesSquare size={14} /> : null}{row.name}</span>
          <small>{row.type}</small>
          <NodeStateMark state={row.state} />
          {row.state.kind === "disabled" && <small className="node-why">{fill(t("operate.node.missing"), { inputs: row.state.missing.join(", ") })}</small>}
          {(row.state.kind === "broken") && <small className="node-why">{row.state.reason}</small>}
        </button>
      </li>)}</ul>
    </section>,
    send: () => <SendForm api={api} execution={execution} ports={ports} onSent={onNotice} />,
    events: () => <EventLog api={api} execution={execution} nodes={rows} />,
    tree: () => <TreePanel api={api} execution={execution} executions={executions} onSelect={onSelect} onStop={onStop} />,
    node: () => selected ? <NodePanel api={api} executionId={execution.id} row={selected} /> : <p className="operate-empty">{t("operate.nodes.pick")}</p>,
    decisions: () => <Decisions api={api} executionId={execution.id} />,
  };
  return <div className="operate-detail">
    <div className="operate-main">
      <header className="operate-detail-head">
        <div><h2>{execution.id}</h2><small>{execution.profile_id} · {workflow?.name ?? execution.workflow_id}
          {execution.parent && <> · <button type="button" className="link-button" onClick={() => onSelect(execution.parent!)}>{fill(t("operate.execution.childOf"), { parent: execution.parent })}</button></>}
          {execution.caller && <> · {fill(t("operate.tree.caller"), { node: execution.caller })}</>}</small></div>
        <ExecutionStateMark state={execution.state} />
        {isLive(execution.state) && <Button size="sm" variant="outline" onClick={() => onStop(execution.id)}><Square size={14} />{t("operate.stop")}</Button>}
      </header>
      {"reason" in execution.state && <p className="operate-error">{execution.state.reason}</p>}
      {shown.main.map((id) => <Fragment key={id}>{render[id]()}</Fragment>)}
    </div>
    {shown.aside.length > 0 && <aside className="operate-side">{shown.aside.map((id) => <Fragment key={id}>{render[id]()}</Fragment>)}</aside>}
  </div>;
}

export default function OperateScene() {
  const { status, client, reason, connect, source } = useCore();
  const settings = useOperateSettings();
  const api = useMemo(() => client ? operateApi(client) : null, [client]);
  const [profiles, setProfiles] = useState<Profile[] | null>(null);
  const [modules, setModules] = useState<ModuleSummary[]>([]);
  const [executions, setExecutions] = useState<Execution[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice>(null);
  const [busy, setBusy] = useState(false);
  const [showSettings, setShowSettings] = useState(false);

  const loadProfiles = async () => {
    if (!api) return;
    try { setProfiles(await api.profiles()); } catch (error) { setNotice({ tone: "danger", text: errorText(error) }); setProfiles((current) => current ?? []); }
  };
  useEffect(() => { setProfiles(null); void loadProfiles(); }, [api]); // eslint-disable-line react-hooks/exhaustive-deps
  usePolling(async (current) => {
    if (!api) return;
    try {
      const [listed, installed] = await Promise.all([api.executions(settings.projectId), api.modules()]);
      if (!current()) return;
      setExecutions(listed);
      setModules(installed);
      // An execution of a profile not seen yet (applied or saved elsewhere): reload them, as the TUI does.
      if (profiles && listed.some((execution) => !profiles.some((profile) => profile.id === execution.profile_id))) void loadProfiles();
    } catch (error) { if (current()) setNotice({ tone: "danger", text: errorText(error) }); }
  }, settings.refreshMs, [api, settings.projectId, profiles]);

  const run = async (profile: Profile, workflowId: string | undefined) => {
    if (!api) return;
    setBusy(true);
    try {
      const execution = await api.start(profile.id, workflowId, settings.startTimeoutMs);
      setExecutions((current) => [...current.filter((item) => item.id !== execution.id), execution]);
      setSelectedId(execution.id);
      setNotice(execution.state.status === "failed" ? { tone: "danger", text: `${execution.id}: ${execution.state.reason}` } : { tone: "info", text: fill(t("operate.started"), { id: execution.id }) });
    } catch (error) { setNotice({ tone: "danger", text: errorText(error) }); } finally { setBusy(false); }
  };
  const stop = async (id: string) => {
    if (!api) return;
    try {
      const stopped = await api.stop(id);
      setExecutions((current) => current.map((item) => item.id === id ? stopped : item));
      setNotice({ tone: "info", text: fill(t("operate.stopped"), { id }) });
    } catch (error) { setNotice({ tone: "danger", text: errorText(error) }); }
  };

  const projects = projectsOf(profiles ?? []);
  const shownProfiles = (profiles ?? []).filter((profile) => !settings.projectId || profile.project_id === settings.projectId);
  const selected = executions.find((execution) => execution.id === selectedId) ?? null;
  const ready = status === "ready" && api && profiles !== null;

  return <section className="operate-scene">
    <header className="graph-toolbar">
      <div><Activity size={16} /><h1>{t("operate.title")}</h1>
        {projects.length > 0 && <label>{t("operate.project")}<select value={settings.projectId ?? ""} onChange={(e) => settings.setProject(e.target.value || null)}>
          <option value="">{t("operate.allProjects")}</option>{projects.map((id) => <option key={id} value={id}>{id}</option>)}
        </select></label>}
      </div>
      <div>
        <span className="graph-source">{t(source === "mock" ? "graph.mock" : "graph.core")}</span>
        <Button variant="ghost" size="sm" aria-pressed={showSettings} onClick={() => setShowSettings(!showSettings)}><SlidersHorizontal size={14} />{t("operate.settings.title")}</Button>
      </div>
    </header>
    {showSettings && <SettingsPanel />}
    {notice && <p className={`operate-notice tone-${notice.tone}`} role={notice.tone === "danger" ? "alert" : "status"}>{notice.text}<button type="button" className="link-button" onClick={() => setNotice(null)}>{t("operate.dismiss")}</button></p>}
    {!ready ? <div className="graph-connection-state" role="status"><Activity size={36} strokeWidth={1.25} />
      <h2>{t(status === "loading" || status === "ready" ? "graph.loading" : status === "incompatible" ? "graph.incompatible" : "graph.offline")}</h2>
      {reason && status !== "loading" && <code className="graph-reason">{reason}</code>}
      {status !== "loading" && status !== "ready" && <Button variant="outline" onClick={connect}>{t("graph.retry")}</Button>}
    </div> : <div className="operate-body">
      <nav className="operate-lists">
        <section className="operate-panel" aria-label={t("operate.profiles")}>
          <header><h3>{t("operate.profiles")}</h3><span className="operate-count">{shownProfiles.length}</span>
            <Button variant="ghost" size="sm" aria-label={t("operate.reload")} onClick={() => void loadProfiles()}><RefreshCw size={13} /></Button></header>
          {shownProfiles.length === 0 && <p className="operate-empty">{t("operate.noProfiles")}</p>}
          <ul>{shownProfiles.map((profile) => <ProfileRow key={profile.id} profile={profile} busy={busy} onRun={(workflowId) => void run(profile, workflowId)} />)}</ul>
        </section>
        <section className="operate-panel" aria-label={t("operate.executions")}>
          <header><h3>{t("operate.executions")}</h3><span className="operate-count">{executions.length}</span></header>
          {executions.length === 0 && <p className="operate-empty">{t("operate.noExecutions")}</p>}
          <ul>{[...executions].reverse().map((execution) => <li key={execution.id} className="operate-row execution-row">
            <button type="button" aria-pressed={execution.id === selectedId} onClick={() => setSelectedId(execution.id)}>
              <strong>{execution.id}</strong><ExecutionStateMark state={execution.state} />
              <small>{execution.profile_id} · {execution.workflow_id}{execution.parent ? ` · ${fill(t("operate.execution.childOf"), { parent: execution.parent })}` : ""}</small>
            </button>
            {isLive(execution.state) && <Button size="sm" variant="outline" aria-label={fill(t("operate.stopOne"), { id: execution.id })} onClick={() => void stop(execution.id)}><Square size={13} /></Button>}
          </li>)}</ul>
        </section>
      </nav>
      {selected ? <ExecutionDetail key={selected.id} api={api} execution={selected} executions={executions} profiles={profiles} modules={modules} onStop={(id) => void stop(id)} onNotice={setNotice} onSelect={setSelectedId} />
        : <p className="operate-empty operate-pick">{t("operate.pick")}</p>}
    </div>}
  </section>;
}
