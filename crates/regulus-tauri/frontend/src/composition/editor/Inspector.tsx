import { useEffect, useRef, useState } from "react";
import { ArrowRight, Boxes, GitBranch, LockKeyhole, Minus, Plus, Replace, RotateCcw, Trash2, X } from "lucide-react";
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

/** The "uses" inputs of a node and who serves each: pick another node that offers the same capability to rewire. */
function Dependencies({ profile, workflowId, node, edit }: { profile: Profile; workflowId: string; node: CompositionNode; edit: Edit }) {
  const workflow = doc.workflowOf(profile, workflowId);
  const inputs = doc.portsOf(profile, node).filter((port) => port.kind === "uses" && port.direction === "input");
  if (!workflow || !inputs.length) return null;
  const label = (end: doc.End) => end.node_id === doc.SELF ? `${doc.SELF}.${end.port_id}` : `${doc.nodeOf(workflow, end.node_id)?.name || end.node_id}.${end.port_id}`;
  return <>
    <h3>{t("editor.dependency.title")}</h3>
    <p className="inspector-hint">{t("editor.dependency.help")}</p>
    <ul className="dependency-list">{inputs.map((port) => {
      const to = { node_id: node.id, port_id: port.id };
      const wires = doc.wiresInto(workflow, to);
      const candidates = doc.dependencyCandidates(profile, workflowId, to);
      const keyOf = (end: doc.End) => `${end.node_id}.${end.port_id}`;
      const pick = (replacing: string | undefined) => (event: React.ChangeEvent<HTMLSelectElement>) => {
        const chosen = candidates.find((item) => keyOf(item.end) === event.target.value);
        if (chosen) edit((p) => doc.replaceDependency(p, workflowId, to, chosen.end, replacing));
      };
      const picker = (replacing: string | undefined, current: string) => <label className="dependency-pick">
        <Replace size={12} aria-hidden="true" />
        <select aria-label={`${t(replacing ? "editor.dependency.replace" : "editor.dependency.connect")} ${port.id}`} value={current} onChange={pick(replacing)} disabled={!candidates.length}>
          <option value="">{t(candidates.length ? replacing ? "editor.dependency.replace" : "editor.dependency.connect" : "editor.dependency.none")}</option>
          {candidates.map((item) => <option key={keyOf(item.end)} value={keyOf(item.end)} disabled={wires.some((wire) => keyOf(wire.from) === keyOf(item.end))}>{item.label}</option>)}
        </select>
      </label>;
      return <li key={port.id}>
        <div className="dependency-head"><span>{port.name || port.id}</span><code>{port.capability}</code>{port.required && <small>{t("graph.required")}</small>}</div>
        {wires.length === 0 && <small className="dependency-unwired">{t("editor.dependency.unwired")}</small>}
        {wires.map((wire) => <div className="dependency-row" key={wire.id}><span>{t("editor.dependency.from")} <code>{label(wire.from)}</code></span>{picker(wire.id, "")}</div>)}
        {(wires.length === 0 || port.cardinality === "many") && picker(undefined, "")}
      </li>;
    })}</ul>
  </>;
}

export function NodeInspector({ profile, workflowId, node, manifests, modules, diagnostics, edit, onOpenWorkflow, onCreateBox, onDelete, onClose }: {
  profile: Profile; workflowId: string; node: CompositionNode; manifests: Manifest[]; modules: ModuleDefinition[]; diagnostics: Diagnostic[];
  edit: Edit; onOpenWorkflow: (id: string) => void; onCreateBox: () => void; onDelete: () => void; onClose: () => void;
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
    {internal && <Button variant="outline" className="open-internal" onClick={() => onOpenWorkflow(internal.id)}><GitBranch size={14} />{t("editor.box.open")}<ArrowRight size={14} /></Button>}

    <h3>{t("graph.ports")}</h3>
    {ports.length === 0 && <p>{t("graph.noPorts")}</p>}
    <ul className="inspector-ports">{ports.map((port) => <li key={port.id}><div><span>{port.name || port.id}</span><span>{t(port.direction === "input" ? "graph.input" : "graph.output")}</span></div><code>{port.capability}</code><small>{t(port.kind === "uses" ? "graph.uses" : "graph.then")}{port.direction === "input" && ` · ${t(port.required ? "graph.required" : "graph.optional")} · ${t(port.cardinality === "many" ? "editor.port.many" : "editor.port.one")}`}</small></li>)}</ul>

    <Dependencies profile={profile} workflowId={workflowId} node={node} edit={edit} />

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

    <Button variant="outline" className="inspector-delete" onClick={onCreateBox}><Boxes size={14} />{t("editor.box.create")}</Button>
    <Button variant="destructive" className="inspector-action" onClick={onDelete}><Trash2 size={14} />{t("editor.node.delete")}</Button>
  </aside>;
}

function PortRow({ port, capabilities, onChange, onRemove }: { port: Port; capabilities: string; onChange: (patch: Partial<Port>) => void; onRemove: () => void }) {
  const input = port.direction === "input";
  return <li className="workflow-port">
    <div className="port-line">
      <DraftInput label={t("editor.port.id")} value={port.id} commitOn="blur" parse={(text) => doc.slugify(text) ? { ok: true, value: text } : { ok: false, error: "id" }} onCommit={(value) => onChange({ id: String(value), name: doc.slugify(String(value)) })} />
      <button type="button" aria-label={`${t("editor.port.remove")} ${port.id}`} onClick={onRemove}><Trash2 size={12} /></button>
    </div>
    <label><span>{t("editor.port.capability")}</span><input value={port.capability} list={capabilities} onChange={(event) => onChange({ capability: event.target.value })} /></label>
    <div className="port-selects">
      <select aria-label={t("editor.port.direction")} value={port.direction} onChange={(event) => onChange({ direction: event.target.value as Port["direction"] })}><option value="input">{t("graph.input")}</option><option value="output">{t("graph.output")}</option></select>
      <select aria-label={t("editor.port.kind")} value={port.kind} onChange={(event) => onChange({ kind: event.target.value as Port["kind"] })}><option value="then">{t("graph.then")}</option><option value="uses">{t("graph.uses")}</option></select>
      <select aria-label={t("editor.port.cardinality")} value={port.cardinality ?? "one"} disabled={!input} onChange={(event) => onChange({ cardinality: event.target.value as "one" | "many" })}><option value="one">{t("editor.port.one")}</option><option value="many">{t("editor.port.many")}</option></select>
    </div>
    <div className="port-flags">
      <label className="check-row"><input type="checkbox" checked={port.required} onChange={(event) => onChange({ required: event.target.checked })} /><span>{t("graph.required")}</span></label>
      {port.kind === "uses" && <label className="check-row"><input type="checkbox" checked={port.lazy ?? false} onChange={(event) => onChange({ lazy: event.target.checked })} /><span>{t("editor.port.lazy")}</span></label>}
    </div>
  </li>;
}

export function WorkflowInspector({ profile, workflow, manifests, edit, onDelete }: { profile: Profile; workflow: Workflow; manifests: Manifest[]; edit: Edit; onDelete: () => void }) {
  const main = workflow.id === profile.main_workflow_id;
  const [newKind, setNewKind] = useState<Port["kind"]>("then");
  const manifestPorts = manifests.flatMap((manifest) => (manifest.node ?? []).flatMap((node) => node.port ?? []));
  const listId = (kind: Port["kind"]) => `capabilities-${workflow.id}-${kind}`;
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
    {(["then", "uses"] as const).map((kind) => <datalist key={kind} id={listId(kind)}>{doc.knownCapabilities(profile, manifestPorts, kind).map((capability) => <option key={capability} value={capability} />)}</datalist>)}
    <ul className="workflow-ports">{(workflow.ports ?? []).map((port) => <PortRow key={port.id} port={port} capabilities={listId(port.kind)}
      onChange={(patch) => changePort(port, patch)}
      onRemove={() => edit((p) => doc.removeWorkflowPort(p, workflow.id, port.id))} />)}</ul>
    <div className="port-actions">
      <select aria-label={t("editor.port.newKind")} value={newKind} onChange={(event) => setNewKind(event.target.value as Port["kind"])}><option value="then">{t("graph.then")}</option><option value="uses">{t("graph.uses")}</option></select>
      <Button variant="outline" size="sm" onClick={() => edit((p) => doc.addWorkflowPort(p, workflow.id, { direction: "input", kind: newKind }).profile)}><Plus size={12} />{t("editor.port.addInput")}</Button>
      <Button variant="outline" size="sm" onClick={() => edit((p) => doc.addWorkflowPort(p, workflow.id, { direction: "output", kind: newKind }).profile)}><Plus size={12} />{t("editor.port.addOutput")}</Button>
    </div>
    {main ? <p className="inspector-hint">{t("editor.workflow.mainKept")}</p> : <Button variant="destructive" className="inspector-delete" onClick={onDelete}><Trash2 size={14} />{t("editor.workflow.delete")}</Button>}
  </aside>;
}

export function SelectionInspector({ nodes, wires, onCreateBox, onDelete }: { nodes: number; wires: number; onCreateBox: () => void; onDelete: () => void }) {
  return <aside className="node-inspector" aria-label={t("editor.selection.title")}>
    <header><span>{t("editor.selection.title")}</span></header>
    <p>{nodes} {t("graph.nodes")} · {wires} {t("graph.connections")}</p>
    {nodes > 0 && <><p className="inspector-hint">{t("editor.box.createHelp")}</p>
      <Button variant="outline" className="inspector-action" onClick={onCreateBox}><Boxes size={14} />{t("editor.box.create")}</Button></>}
    <Button variant="destructive" className="inspector-delete" onClick={onDelete}><Trash2 size={14} />{t("editor.selection.delete")}</Button>
  </aside>;
}
