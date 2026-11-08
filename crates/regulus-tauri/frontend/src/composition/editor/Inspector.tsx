import { useEffect, useRef, useState } from "react";
import { ArrowRight, GitBranch, LockKeyhole, Minus, Plus, RotateCcw, Trash2, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ModuleStatus } from "@/components/ui/status";
import { t } from "@/lib/i18n";
import type { CompositionNode, ConfigValue, Diagnostic, Manifest, ModuleDefinition, Port, Profile, Workflow } from "../model";
import * as doc from "./document";
import { coerce, displayValue, fieldRows, mapEntries, mapFrom, resetValue, type FieldRow } from "./fields";
import { manifestTypeOf, moduleStateOf } from "./palette";
import { SeverityMark, stateView } from "./nodes";
import { useEditor } from "./store";

type Edit = (change: (profile: Profile) => Profile) => void;

/** A text box that keeps what is typed until it parses, and follows outside changes. */
function DraftInput({ value, parse, onCommit, commitOn = "change", label, ...rest }: {
  value: string; parse: (text: string) => { ok: true; value: ConfigValue } | { ok: false; error: string };
  onCommit: (value: ConfigValue) => void; commitOn?: "change" | "blur"; label: string;
} & Omit<React.InputHTMLAttributes<HTMLInputElement>, "value" | "onChange">) {
  const [text, setText] = useState(value);
  const [error, setError] = useState<string | null>(null);
  const committed = useRef(value);
  useEffect(() => { if (value !== committed.current) { committed.current = value; setText(value); setError(null); } }, [value]);
  const commit = (next: string) => {
    const parsed = parse(next);
    if (!parsed.ok) { setError(parsed.error); return; }
    setError(null);
    committed.current = displayValue(parsed.value);
    onCommit(parsed.value);
  };
  return <span className="draft-input">
    <input {...rest} aria-label={label} aria-invalid={error ? true : undefined} value={text}
      onChange={(event) => { setText(event.target.value); if (commitOn === "change") commit(event.target.value); }}
      onBlur={() => { if (commitOn === "blur") commit(text); }}
      onKeyDown={(event) => { if (event.key === "Enter" && commitOn === "blur") commit(text); }} />
    {error && <small className="field-error" role="alert">{t(`editor.field.invalid.${error}` as "editor.field.invalid.number")}</small>}
  </span>;
}

function MapControl({ value, label, onCommit }: { value: ConfigValue | undefined; label: string; onCommit: (value: ConfigValue) => void }) {
  const external = JSON.stringify(value ?? {});
  const [rows, setRows] = useState(() => mapEntries(value));
  const committed = useRef(external);
  useEffect(() => { if (external !== committed.current) { committed.current = external; setRows(mapEntries(value)); } }, [external, value]);
  const update = (next: [string, string][]) => {
    setRows(next);
    const map = mapFrom(next);
    committed.current = JSON.stringify(map);
    onCommit(map);
  };
  return <div className="map-control" role="group" aria-label={label}>
    {rows.map(([key, item], index) => <div className="map-row" key={index}>
      <input aria-label={t("editor.field.mapKey")} placeholder={t("editor.field.mapKey")} value={key} onChange={(event) => update(rows.map((row, at) => at === index ? [event.target.value, row[1]] : row))} />
      <input aria-label={t("editor.field.mapValue")} placeholder={t("editor.field.mapValue")} value={item} onChange={(event) => update(rows.map((row, at) => at === index ? [row[0], event.target.value] : row))} />
      <button type="button" aria-label={t("editor.field.mapRemove")} onClick={() => update(rows.filter((_, at) => at !== index))}><Minus size={12} /></button>
    </div>)}
    <button type="button" className="map-add" onClick={() => setRows([...rows, ["", ""]])}><Plus size={12} />{t("editor.field.mapAdd")}</button>
  </div>;
}

function FieldControl({ row, workflows, onChange }: { row: FieldRow; workflows: Workflow[]; onChange: (value: ConfigValue | undefined) => void }) {
  const current = row.value ?? row.defaultValue;
  const label = row.id;
  switch (row.control) {
    case "select": {
      const options = row.options.map(displayValue);
      const shown = displayValue(current);
      return <select aria-label={label} value={shown} onChange={(event) => { const parsed = coerce(row, event.target.value); if (parsed.ok) onChange(parsed.value); }}>
        {!options.includes(shown) && <option value={shown}>{shown || t("editor.field.unset")}</option>}
        {options.map((option) => <option key={option} value={option}>{option}</option>)}
      </select>;
    }
    case "workflow":
      return <select aria-label={label} value={displayValue(current)} onChange={(event) => onChange(event.target.value)}>
        <option value="">{t("editor.field.noWorkflow")}</option>
        {workflows.map((workflow) => <option key={workflow.id} value={workflow.id}>{workflow.name || workflow.id}</option>)}
        {typeof current === "string" && current && !workflows.some((workflow) => workflow.id === current) && <option value={current}>{current}</option>}
      </select>;
    case "map":
      return <MapControl value={current} label={label} onCommit={onChange} />;
    case "number":
      return <DraftInput type="number" label={label} value={displayValue(current)} parse={(text) => coerce(row, text)} onCommit={onChange} />;
    case "raw":
      return <DraftInput className="raw-input" label={label} value={displayValue(current)} parse={(text) => coerce(row, text)} onCommit={onChange} />;
    default:
      return <input aria-label={label} value={displayValue(current)} onChange={(event) => onChange(event.target.value)} />;
  }
}

export function NodeInspector({ profile, workflowId, node, manifests, modules, diagnostics, edit, onOpenWorkflow, onDelete, onClose }: {
  profile: Profile; workflowId: string; node: CompositionNode; manifests: Manifest[]; modules: ModuleDefinition[]; diagnostics: Diagnostic[];
  edit: Edit; onOpenWorkflow: (id: string) => void; onDelete: () => void; onClose: () => void;
}) {
  const nodeType = manifestTypeOf(manifests, node.module_id, node.type);
  const rows = fieldRows(nodeType?.field, node.config);
  const state = moduleStateOf(modules, node.module_id);
  const ports = doc.portsOf(profile, node);
  const others = profile.workflows.filter((workflow) => workflow.id !== workflowId);
  const internal = node.internal_workflow_id ? doc.workflowOf(profile, node.internal_workflow_id) : undefined;
  const found = diagnostics.filter((d) => d.node_id === node.id && (d.workflow_id === null || d.workflow_id === workflowId));
  const set = (key: string, value: ConfigValue | undefined) => edit((p) => doc.setConfig(p, workflowId, node.id, key, value));

  return <aside className="node-inspector" aria-label={t("graph.inspector")}>
    <header><span>{t("graph.inspector")}</span><button onClick={onClose} aria-label={t("graph.closeInspector")}><X size={16} /></button></header>
    <label className="inspector-field"><span>{t("editor.node.name")}</span><input value={node.name} placeholder={node.id} onChange={(event) => edit((p) => doc.renameNode(p, workflowId, node.id, event.target.value))} /></label>
    <dl>
      <dt>{t("editor.node.id")}</dt><dd><code>{node.id}</code></dd>
      <dt>{t("graph.nodeType")}</dt><dd>{node.type}</dd>
      {node.module_id && <><dt>{t("editor.node.module")}</dt><dd><code>{node.module_id}</code><br /><ModuleStatus state={stateView[state].kind} label={t(stateView[state].label)} /></dd></>}
      {node.resource && <><dt>{t("graph.resource")}</dt><dd><code>{node.resource}</code></dd></>}
    </dl>
    {state === "not-installed" && <p className="inspector-hint">{t("editor.node.absentHint")}</p>}
    {found.length > 0 && <ul className="inspector-diagnostics">{found.map((d, index) => <li key={index}><SeverityMark severity={d.severity} /><span>{d.message}</span><code>{d.code}</code></li>)}</ul>}

    <label className="inspector-field"><span>{t("editor.node.internal")}</span>
      <select value={node.internal_workflow_id ?? ""} onChange={(event) => edit((p) => doc.setInternalWorkflow(p, workflowId, node.id, event.target.value || null))}>
        <option value="">{t("editor.node.noInternal")}</option>
        {others.map((workflow) => <option key={workflow.id} value={workflow.id}>{workflow.name || workflow.id}</option>)}
        {node.internal_workflow_id && !internal && <option value={node.internal_workflow_id}>{node.internal_workflow_id}</option>}
      </select>
    </label>
    {!node.module_id && <p className="inspector-hint">{t("editor.node.boxHint")}</p>}
    {internal && <Button variant="outline" className="open-internal" onClick={() => onOpenWorkflow(internal.id)}><GitBranch size={14} />{t("graph.openInternal")}<ArrowRight size={14} /></Button>}

    <h3>{t("graph.ports")}</h3>
    {ports.length === 0 && <p>{t("graph.noPorts")}</p>}
    <ul className="inspector-ports">{ports.map((port) => <li key={port.id}><div><span>{port.name || port.id}</span><span>{t(port.direction === "input" ? "graph.input" : "graph.output")}</span></div><code>{port.capability}</code><small>{t(port.kind === "uses" ? "graph.uses" : "graph.then")}{port.direction === "input" && ` · ${t(port.required ? "graph.required" : "graph.optional")} · ${t(port.cardinality === "many" ? "editor.port.many" : "editor.port.one")}`}</small></li>)}</ul>

    <h3>{t("graph.configuration")}</h3>
    {!nodeType && node.module_id && <p className="inspector-hint">{t("editor.field.noManifest")}</p>}
    {rows.length === 0 ? <p>{t("editor.field.none")}</p> : <div className="inspector-config">{rows.map((row) => <div className="config-row" key={row.id}>
      <div className="config-head">
        <span className="config-id">{row.id}</span>
        <span className="config-type">{row.type ?? t("editor.field.unknown")}</span>
        {row.type !== null && <button type="button" className="config-reset" disabled={row.isDefault} onClick={() => set(row.id, resetValue(row))} aria-label={`${t("editor.field.reset")} ${row.id}`}><RotateCcw size={12} />{t("editor.field.reset")}</button>}
        {row.type === null && <button type="button" className="config-reset" onClick={() => set(row.id, undefined)} aria-label={`${t("editor.field.remove")} ${row.id}`}><Minus size={12} />{t("editor.field.remove")}</button>}
      </div>
      <FieldControl row={row} workflows={profile.workflows} onChange={(value) => set(row.id, value)} />
      {row.secret && <small className="config-secret"><LockKeyhole size={11} aria-hidden="true" />{t("editor.field.secret")}</small>}
      {row.defaultValue !== undefined && <small className="config-default">{t("editor.field.default")} <code>{displayValue(row.defaultValue) || t("editor.field.empty")}</code></small>}
    </div>)}</div>}

    <Button variant="destructive" className="inspector-delete" onClick={onDelete}><Trash2 size={14} />{t("editor.node.delete")}</Button>
  </aside>;
}

function PortRow({ port, onChange, onRemove }: { port: Port; onChange: (patch: Partial<Port>) => void; onRemove: () => void }) {
  return <li className="workflow-port">
    <div className="port-line">
      <DraftInput label={t("editor.port.id")} value={port.id} commitOn="blur" parse={(text) => doc.slugify(text) ? { ok: true, value: text } : { ok: false, error: "id" }} onCommit={(value) => onChange({ id: String(value), name: doc.slugify(String(value)) })} />
      <button type="button" aria-label={`${t("editor.port.remove")} ${port.id}`} onClick={onRemove}><Trash2 size={12} /></button>
    </div>
    <label><span>{t("editor.port.capability")}</span><input value={port.capability} onChange={(event) => onChange({ capability: event.target.value })} /></label>
    <div className="port-selects">
      <select aria-label={t("editor.port.direction")} value={port.direction} onChange={(event) => onChange({ direction: event.target.value as Port["direction"] })}><option value="input">{t("graph.input")}</option><option value="output">{t("graph.output")}</option></select>
      <select aria-label={t("editor.port.kind")} value={port.kind} onChange={(event) => onChange({ kind: event.target.value as Port["kind"] })}><option value={doc.WORKFLOW_PORT_KIND}>{t("graph.then")}</option>{port.kind !== doc.WORKFLOW_PORT_KIND && <option value={port.kind} disabled>{t("graph.uses")}</option>}</select>
      <select aria-label={t("editor.port.cardinality")} value={port.cardinality ?? "one"} onChange={(event) => onChange({ cardinality: event.target.value as "one" | "many" })}><option value="one">{t("editor.port.one")}</option><option value="many">{t("editor.port.many")}</option></select>
    </div>
    {port.kind === "uses" && <small className="field-error">{t("editor.port.usesHint")}</small>}
  </li>;
}

export function WorkflowInspector({ profile, workflow, edit, onDelete }: { profile: Profile; workflow: Workflow; edit: Edit; onDelete: () => void }) {
  const main = workflow.id === profile.main_workflow_id;
  // A new direction or kind turns the port around: its wires go, and the user is told.
  const changePort = (port: Port, patch: Partial<Port>) => {
    const turned = (patch.direction !== undefined && patch.direction !== port.direction) || (patch.kind !== undefined && patch.kind !== port.kind);
    const dropped = turned ? doc.wiresOfPort(profile, workflow.id, port.id) : 0;
    edit((p) => doc.updateWorkflowPort(p, workflow.id, port.id, patch));
    if (dropped) useEditor.getState().notify({ tone: "info", text: t("editor.port.wiresDropped"), detail: `${port.id}: ${dropped}` });
  };
  return <aside className="node-inspector" aria-label={t("editor.workflow.inspector")}>
    <header><span>{t("editor.workflow.inspector")}</span></header>
    <label className="inspector-field"><span>{t("editor.workflow.name")}</span><input value={workflow.name} placeholder={workflow.id} onChange={(event) => edit((p) => doc.renameWorkflow(p, workflow.id, event.target.value))} /></label>
    <dl><dt>{t("editor.node.id")}</dt><dd><code>{workflow.id}</code></dd><dt>{t("editor.workflow.kind")}</dt><dd>{t(main ? "editor.workflow.main" : "editor.workflow.extra")}</dd></dl>
    <h3>{t("editor.workflow.ports")}</h3>
    <p>{t("editor.workflow.portsHelp")}</p>
    <ul className="workflow-ports">{(workflow.ports ?? []).map((port) => <PortRow key={port.id} port={port}
      onChange={(patch) => changePort(port, patch)}
      onRemove={() => edit((p) => doc.removeWorkflowPort(p, workflow.id, port.id))} />)}</ul>
    <div className="port-actions">
      <Button variant="outline" size="sm" onClick={() => edit((p) => doc.addWorkflowPort(p, workflow.id, { direction: "input" }).profile)}><Plus size={12} />{t("editor.port.addInput")}</Button>
      <Button variant="outline" size="sm" onClick={() => edit((p) => doc.addWorkflowPort(p, workflow.id, { direction: "output" }).profile)}><Plus size={12} />{t("editor.port.addOutput")}</Button>
    </div>
    {main ? <p className="inspector-hint">{t("editor.workflow.mainKept")}</p> : <Button variant="destructive" className="inspector-delete" onClick={onDelete}><Trash2 size={14} />{t("editor.workflow.delete")}</Button>}
  </aside>;
}

export function SelectionInspector({ nodes, wires, onDelete }: { nodes: number; wires: number; onDelete: () => void }) {
  return <aside className="node-inspector" aria-label={t("editor.selection.title")}>
    <header><span>{t("editor.selection.title")}</span></header>
    <p>{nodes} {t("graph.nodes")} · {wires} {t("graph.connections")}</p>
    <Button variant="destructive" className="inspector-delete" onClick={onDelete}><Trash2 size={14} />{t("editor.selection.delete")}</Button>
  </aside>;
}
