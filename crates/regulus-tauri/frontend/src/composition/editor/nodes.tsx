import { memo } from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import { Blocks, CircleAlert, FileText, GitBranch, Info, LogIn, LogOut, TriangleAlert } from "lucide-react";
import { ModuleStatus, type ModuleStateKind } from "@/components/ui/status";
import { t, type LocaleKey } from "@/lib/i18n";
import type { FrameNode, PieceNode, SelfNode, Severity } from "./flow";
import type { PieceState } from "./palette";

/** The status component's kind and the word for a piece state; incompatible borrows "broken"'s icon. */
export const stateView: Record<PieceState, { kind: ModuleStateKind; label: LocaleKey }> = {
  installed: { kind: "active", label: "editor.state.installed" },
  disabled: { kind: "disabled", label: "editor.state.disabled" },
  broken: { kind: "broken", label: "editor.state.broken" },
  incompatible: { kind: "broken", label: "editor.state.incompatible" },
  "not-installed": { kind: "not-installed", label: "editor.state.notInstalled" },
};

const severityView: Record<Severity, { icon: typeof Info; label: LocaleKey }> = {
  error: { icon: CircleAlert, label: "editor.severity.error" },
  warning: { icon: TriangleAlert, label: "editor.severity.warning" },
  info: { icon: Info, label: "editor.severity.info" },
};

export function SeverityMark({ severity }: { severity: Severity }) {
  const { icon: Icon, label } = severityView[severity];
  return <span className={`severity-mark severity-${severity}`}><Icon size={12} strokeWidth={1.75} aria-hidden="true" />{t(label)}</span>;
}

export const Piece = memo(function Piece({ data, selected }: NodeProps<PieceNode>) {
  const { item, ports, state, severity, editable } = data;
  const Icon = item.resource ? FileText : Blocks;
  const kind = item.resource ? "editor.kind.resource" : item.module_id ? "graph.moduleNode" : item.internal_workflow_id ? "editor.kind.box" : "editor.kind.plain";
  return <article className={`composition-node${selected ? " is-selected" : ""}${severity ? ` has-${severity}` : ""}${state === "not-installed" ? " is-absent" : ""}`}>
    <header><Icon size={16} strokeWidth={1.5} /><span>{t(kind)}</span>{item.internal_workflow_id && <GitBranch size={14} />}</header>
    <h3>{item.name || item.id}</h3>
    <p className="node-type">{item.resource || item.type}</p>
    {(state !== "installed" || severity) && <div className="node-flags">
      {state !== "installed" && <ModuleStatus state={stateView[state].kind} label={t(stateView[state].label)} />}
      {severity && <SeverityMark severity={severity} />}
    </div>}
    <div className="node-ports">{ports.map((port) => <div className={`node-port port-${port.direction}`} key={port.id}>
      <Handle type={port.direction === "input" ? "target" : "source"} position={port.direction === "input" ? Position.Left : Position.Right} id={port.id} isConnectable={editable} className={`port-handle handle-${port.kind}`} />
      <span>{port.name || port.id}{port.required && port.direction === "input" && <small className="port-required">{t("graph.required")}</small>}</span><code>{port.capability}{port.cardinality === "many" && ` · ${t("editor.port.many")}`}</code>
    </div>)}</div>
    {item.internal_workflow_id && <div className="node-internal"><GitBranch size={12} />{t("graph.internalAvailable")}</div>}
  </article>;
});

/** The workflow's own inputs (left, they feed nodes) or outputs (right, they take wires). */
export const SelfPorts = memo(function SelfPorts({ data }: NodeProps<SelfNode>) {
  const input = data.side === "in";
  return <article className={`composition-node self-node${data.severity ? ` has-${data.severity}` : ""}`}>
    <header>{input ? <LogIn size={16} strokeWidth={1.5} /> : <LogOut size={16} strokeWidth={1.5} />}<span>{t(input ? "editor.self.inputs" : "editor.self.outputs")}</span></header>
    <div className="node-ports">{data.ports.map((port) => <div className={`node-port port-${input ? "output" : "input"}`} key={port.id}>
      <Handle type={input ? "source" : "target"} position={input ? Position.Right : Position.Left} id={port.id} className={`port-handle handle-${port.kind}`} />
      <span>{port.name || port.id}</span><code>self.{port.id} · {port.capability}</code>
    </div>)}</div>
  </article>;
});

export function WorkflowFrame({ data }: NodeProps<FrameNode>) {
  return <div className="workflow-frame"><span>{data.label}</span></div>;
}

export const nodeTypes = { piece: Piece, self: SelfPorts, workflow: WorkflowFrame };
