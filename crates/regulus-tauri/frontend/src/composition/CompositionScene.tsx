import { memo, useMemo, useState } from "react";
import { Background, Controls, Handle, MarkerType, Position, ReactFlow, type Edge, type Node, type NodeProps } from "@xyflow/react";
import { ArrowRight, Blocks, FileText, GitBranch, LockKeyhole, PackageOpen, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import { useComposition } from "./client";
import type { CompositionNode, Profile, Workflow } from "./model";
import "@xyflow/react/dist/style.css";
import "./composition.css";

type CanvasNode = Node<{ item: CompositionNode; workflow: Workflow }, "piece">;
type GroupNode = Node<{ label: string }, "workflow">;

const Piece = memo(function Piece({ data, selected }: NodeProps<CanvasNode>) {
  const { item } = data;
  const Icon = item.resource ? FileText : Blocks;
  return <article className={`composition-node${selected ? " is-selected" : ""}`}>
    <header><Icon size={16} strokeWidth={1.5} /><span>{item.resource ? t("graph.resource") : t("graph.moduleNode")}</span>{item.internal_workflow_id && <GitBranch size={14} />}</header>
    <h3>{item.name}</h3>
    <p className="node-type">{item.resource || item.type}</p>
    <div className="node-ports">{item.ports.map((port) => <div className={`node-port port-${port.direction}`} key={port.id}>
      <Handle type={port.direction === "input" ? "target" : "source"} position={port.direction === "input" ? Position.Left : Position.Right} id={port.id} isConnectable={false} className={`port-handle handle-${port.kind}`} />
      <span>{port.name}</span><code>{port.capability}</code>
    </div>)}</div>
    {item.internal_workflow_id && <div className="node-internal"><GitBranch size={12} />{t("graph.internalAvailable")}</div>}
  </article>;
});

function WorkflowFrame({ data }: NodeProps<GroupNode>) {
  return <div className="workflow-frame"><span>{data.label}</span></div>;
}

const nodeTypes = { piece: Piece, workflow: WorkflowFrame };
const viewportDefaults = { minZoom: 0.15, maxZoom: 1.8, fitPadding: 0.12 };

function buildGraph(profile: Profile, workflowId: string) {
  const workflows = workflowId === "all" ? profile.workflows : profile.workflows.filter((workflow) => workflow.id === workflowId);
  const nodes: (CanvasNode | GroupNode)[] = [];
  const edges: Edge[] = [];
  for (const workflow of workflows) {
    const unified = workflowId === "all";
    if (unified) nodes.push({
      id: `workflow:${workflow.id}`, type: "workflow", data: { label: workflow.name },
      position: { x: workflow.layout.x, y: workflow.layout.y },
      style: { width: workflow.layout.width, height: workflow.layout.height },
      selectable: false, draggable: false, focusable: false,
    });
    for (const item of workflow.nodes) nodes.push({
      id: `${workflow.id}:${item.id}`, type: "piece", data: { item, workflow },
      position: item.position, ...(unified ? { parentId: `workflow:${workflow.id}` } : {}),
      ariaLabel: item.name,
    });
    for (const wire of workflow.wires) edges.push({
      id: `${workflow.id}:${wire.id}`, source: `${workflow.id}:${wire.from.node_id}`, target: `${workflow.id}:${wire.to.node_id}`,
      sourceHandle: wire.from.port_id, targetHandle: wire.to.port_id, type: "smoothstep",
      label: t(wire.kind === "uses" ? "graph.uses" : "graph.then"),
      className: `wire-${wire.kind}`, markerEnd: { type: MarkerType.ArrowClosed, color: "var(--rg-color-focus)" },
      selectable: false, focusable: false,
    });
  }
  return { nodes, edges, count: workflows.reduce((count, workflow) => count + workflow.nodes.length, 0) };
}

function NodeInspector({ node, workflow, profile, onOpenWorkflow, onClose }: {
  node: CompositionNode; workflow: Workflow; profile: Profile;
  onOpenWorkflow: (id: string) => void; onClose: () => void;
}) {
  const internal = profile.workflows.find((item) => item.id === node.internal_workflow_id);
  return <aside className="node-inspector" aria-label={t("graph.inspector")}>
    <header><span>{t("graph.inspector")}</span><button onClick={onClose} aria-label={t("graph.closeInspector")}><X size={16} /></button></header>
    <h2>{node.name}</h2><p className="inspector-origin">{node.module_id || node.resource}</p>
    <dl><dt>{t("graph.workflow")}</dt><dd>{workflow.name}</dd><dt>{t("graph.nodeType")}</dt><dd>{node.type}</dd></dl>
    {internal && <Button variant="outline" className="open-internal" onClick={() => onOpenWorkflow(internal.id)}><GitBranch size={14} />{t("graph.openInternal")}<ArrowRight size={14} /></Button>}
    <h3>{t("graph.ports")}</h3>
    {node.ports.length === 0 && <p>{t("graph.noPorts")}</p>}
    <ul className="inspector-ports">{node.ports.map((port) => <li key={port.id}><div><span>{port.name}</span><span>{t(port.direction === "input" ? "graph.input" : "graph.output")}</span></div><code>{port.capability}</code><small>{t(port.kind === "uses" ? "graph.uses" : "graph.then")}{port.direction === "input" && ` · ${t(port.required ? "graph.required" : "graph.optional")}`}</small></li>)}</ul>
    <h3>{t("graph.configuration")}</h3>
    {Object.keys(node.config).length === 0 ? <p>{t("graph.noConfiguration")}</p> : <dl className="inspector-config">{Object.entries(node.config).map(([key, value]) => <div key={key}><dt>{key}{!node.exposed_parameters.includes(key) && <LockKeyhole size={11} aria-label={t("graph.locked")} />}</dt><dd>{String(value)}</dd>{node.exposed_parameters.includes(key) && <small>{t("graph.exposed")}</small>}</div>)}</dl>}
    <p className="inspector-note">{t("graph.readOnlyDescription")}</p>
  </aside>;
}

export default function CompositionScene() {
  const { status, profiles, retry } = useComposition();
  const [profileId, setProfileId] = useState<string | null>(null);
  const [workflowId, setWorkflowId] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const profile = profiles.find((item) => item.id === profileId) || profiles[0];
  const activeWorkflow = workflowId || profile?.main_workflow_id || "all";
  const graph = useMemo(() => profile ? buildGraph(profile, activeWorkflow) : { nodes: [], edges: [], count: 0 }, [profile, activeWorkflow]);
  const selectedNode = graph.nodes.find((node): node is CanvasNode => node.type === "piece" && node.id === selected);
  const openWorkflow = (id: string) => { setWorkflowId(id); setSelected(null); };

  return <section className="composition-scene">
    <header className="graph-toolbar"><div><h1>{t("canvas.title")}</h1><span className="graph-mode">{t("graph.readOnly")}</span></div>
      {status === "ready" && profile && <label>{t("graph.profile")}<select value={profile.id} onChange={(event) => { setProfileId(event.target.value); setWorkflowId(null); setSelected(null); }}>{profiles.map((item) => <option value={item.id} key={item.id}>{item.name}</option>)}</select></label>}
      <span className="graph-source">{t("graph.mock")}</span>
    </header>
    {status === "ready" && profile ? <>
      <nav className="workflow-tabs" aria-label={t("graph.workflows")}><button aria-current={activeWorkflow === "all" ? "page" : undefined} onClick={() => openWorkflow("all")}><GitBranch size={14} />{t("graph.all")}</button>{profile.workflows.map((workflow) => <button key={workflow.id} aria-current={activeWorkflow === workflow.id ? "page" : undefined} onClick={() => openWorkflow(workflow.id)}>{workflow.kind === "module" ? <Blocks size={14} /> : <GitBranch size={14} />}{workflow.name}</button>)}</nav>
      <div className="graph-body"><div className="graph-canvas">
        <ReactFlow key={`${profile.id}:${activeWorkflow}`} nodes={graph.nodes.map((node) => ({ ...node, selected: node.id === selected }))} edges={graph.edges} nodeTypes={nodeTypes}
          onNodesChange={(changes) => {
            for (const change of changes) if (change.type === "select") {
              if (change.selected) setSelected(change.id);
              else setSelected((current) => current === change.id ? null : current);
            }
          }}
          onNodeClick={(_, node) => { if (node.type === "piece") setSelected(node.id); }} onPaneClick={() => setSelected(null)}
          nodesDraggable={false} nodesConnectable={false} edgesReconnectable={false} deleteKeyCode={null}
          fitView fitViewOptions={{ padding: viewportDefaults.fitPadding }} minZoom={viewportDefaults.minZoom} maxZoom={viewportDefaults.maxZoom}
          colorMode="dark" ariaLabelConfig={{ "controls.ariaLabel": t("graph.controls"), "controls.zoomIn.ariaLabel": t("graph.zoomIn"), "controls.zoomOut.ariaLabel": t("graph.zoomOut"), "controls.fitView.ariaLabel": t("graph.fit"), "node.a11yDescription.default": t("graph.nodeHelp"), "node.a11yDescription.keyboardDisabled": t("graph.nodeHelp") }}>
          <Background color="var(--rg-color-border)" />
          <Controls showInteractive={false} />
        </ReactFlow>
        {graph.count === 0 && <div className="graph-empty-overlay"><PackageOpen size={32} /><p>{t("graph.emptyWorkflow")}</p></div>}
      </div>{selectedNode && <NodeInspector node={selectedNode.data.item} workflow={selectedNode.data.workflow} profile={profile} onOpenWorkflow={openWorkflow} onClose={() => setSelected(null)} />}</div>
      <footer className="graph-footer"><div><span className="wire-sample uses" />{t("graph.uses")}<span className="wire-sample then" />{t("graph.then")}</div><span>{graph.count} {t("graph.nodes")} · {graph.edges.length} {t("graph.connections")}</span><span>{t("graph.noExecution")}</span></footer>
    </> : <div className="graph-connection-state" role="status"><GitBranch size={36} strokeWidth={1.25} /><h2>{t(status === "loading" ? "graph.loading" : status === "incompatible" ? "graph.incompatible" : status === "ready" ? "graph.noProfiles" : "graph.offline")}</h2><p>{t(status === "loading" ? "graph.loadingDescription" : status === "ready" ? "graph.noProfilesDescription" : "graph.offlineDescription")}</p>{status !== "loading" && <Button variant="outline" onClick={retry}>{t("graph.retry")}</Button>}</div>}
  </section>;
}
