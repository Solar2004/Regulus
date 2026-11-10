import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Background, Controls, ReactFlow, type Connection, type EdgeChange, type FinalConnectionState, type NodeChange } from "@xyflow/react";
import { Boxes, GitBranch, LayoutDashboard, PackageOpen, Trash2 } from "lucide-react";
import { t, type LocaleKey } from "@/lib/i18n";
import type { Diagnostic, ModuleDefinition, Profile } from "../model";
import * as doc from "./document";
import { canConnect, type Refusal } from "./connect";
import { buildUnifiedFlow, buildWorkflowFlow, endOf, nodeIdOf, positionChanges, selfAnchors, wireIdOf, type FlowNode, type PieceData, type PieceNode } from "./flow";
import { nodeTypes } from "./nodes";
import { PIECE_MIME, type PieceRef } from "./Palette";
import { useCanvasSettings } from "./settings";
import { ALL, useEditor, type Selection } from "./store";

const refusalText: Record<Refusal, LocaleKey> = {
  sameNode: "editor.refuse.sameNode",
  missingPort: "editor.refuse.missingPort",
  direction: "editor.refuse.direction",
  kind: "editor.refuse.kind",
  capability: "editor.refuse.capability",
  taken: "editor.refuse.taken",
  duplicate: "editor.refuse.duplicate",
};

type Menu = { x: number; y: number };

/** The canvas's right-click menu: what can be done with the selected nodes. */
function NodeMenu({ menu, box, count, onOpen, onCreateBox, onArrange, onDelete, onClose }: {
  menu: Menu; box: string | null; count: number; onOpen: () => void; onCreateBox: () => void; onArrange: () => void; onDelete: () => void; onClose: () => void;
}) {
  const first = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    first.current?.focus();
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  const run = (action: () => void) => () => { onClose(); action(); };
  return <>
    <div className="canvas-menu-backdrop" onClick={onClose} onContextMenu={(event) => { event.preventDefault(); onClose(); }} />
    <div className="canvas-menu" role="menu" aria-label={t("editor.menu.title")} style={{ left: menu.x, top: menu.y }}>
      {box && <button ref={first} role="menuitem" onClick={run(onOpen)}><GitBranch size={14} />{t("editor.box.open")}</button>}
      <button ref={box ? undefined : first} role="menuitem" onClick={run(onCreateBox)}><Boxes size={14} />{t("editor.box.createCount", { count })}</button>
      {count > 1 && <button role="menuitem" onClick={run(onArrange)}><LayoutDashboard size={14} />{t("editor.layout.arrangeCount", { count })}</button>}
      <button role="menuitem" className="is-destructive" onClick={run(onDelete)}><Trash2 size={14} />{t(count > 1 ? "editor.selection.delete" : "editor.node.delete")}</button>
    </div>
  </>;
}

/** One workflow on the canvas (or all of them, to look at). Remounted per workflow, so its view state starts fresh. */
export function EditorCanvas({ profile, workflowId, modules, diagnostics, selection, layoutRev, onAdd, onCreateBox, onArrangeSelection, onDeleteSelection }: {
  profile: Profile; workflowId: string; modules: ModuleDefinition[]; diagnostics: Diagnostic[]; selection: Selection;
  /** Changes when the nodes were arranged (or the arrangement undone): the workflow's own ports are placed again. */
  layoutRev: number;
  onAdd: (piece: PieceRef, screen: { x: number; y: number }) => void;
  /** Asks to move these nodes into a new workflow placed as a box. */
  onCreateBox: (nodeIds: string[]) => void;
  onArrangeSelection: () => void;
  onDeleteSelection: () => void;
}) {
  const settings = useCanvasSettings();
  const { edit, select, notify, showWorkflow, openBox } = useEditor.getState();
  const [menu, setMenu] = useState<Menu | null>(null);
  const frame = useRef<HTMLDivElement>(null);
  /** The box a node opens, if it is one and its workflow is in the profile. */
  const boxOf = useCallback((nodeId: string) => {
    const current = useEditor.getState().doc ?? profile;
    const item = doc.workflowOf(current, workflowId) && doc.nodeOf(doc.workflowOf(current, workflowId)!, nodeId);
    return item?.internal_workflow_id && doc.workflowOf(current, item.internal_workflow_id) ? item.internal_workflow_id : null;
  }, [profile, workflowId]);
  const openMenu = useCallback((event: React.MouseEvent | MouseEvent, nodeId: string | null) => {
    if (unifiedRef.current) return;
    event.preventDefault();
    const current = useEditor.getState().selection;
    // A right click on a node outside the selection acts on that node alone.
    if (nodeId && !current.nodes.includes(nodeId)) select({ nodes: [nodeId], wires: [] });
    if (!nodeId && !current.nodes.length) return;
    const rect = frame.current?.getBoundingClientRect();
    setMenu({ x: event.clientX - (rect?.left ?? 0), y: event.clientY - (rect?.top ?? 0) });
  }, [select]);
  const [measured, setMeasured] = useState(() => new Map<string, { width: number; height: number }>());
  const unified = workflowId === ALL;
  const unifiedRef = useRef(unified);
  unifiedRef.current = unified;
  const workflow = unified ? undefined : doc.workflowOf(profile, workflowId);
  // The workflow's own ports stay where they were placed when the tab opened, until the nodes are arranged.
  const anchors = useMemo(() => workflow ? selfAnchors(workflow, settings.selfGap) : null, [workflow?.id, settings.selfGap, layoutRev]);
  const labels = useMemo(() => ({ uses: t("graph.uses"), then: t("graph.then") }), []);
  // Mid-drag positions belong to React Flow; the document gets them on the drop.
  const [dragging, setDragging] = useState(() => new Map<string, doc.Point>());
  const cache = useRef(new Map<string, PieceData>());
  const graph = useMemo(() => {
    const options = { modules, diagnostics, selection, measured, anchors, nodeWidth: settings.nodeWidth, labels, dragging, cache: cache.current };
    return unified ? buildUnifiedFlow(profile, options) : buildWorkflowFlow(profile, workflowId, options);
  }, [profile, workflowId, unified, modules, diagnostics, selection, measured, anchors, settings.nodeWidth, labels, dragging]);

  // Stable identity: it reads the store and updates local state functionally.
  const onNodesChange = useCallback((changes: NodeChange<FlowNode>[]) => {
    const sizes = changes.flatMap((change) => change.type === "dimensions" && change.dimensions ? [[change.id, change.dimensions] as const] : []);
    if (sizes.length) setMeasured((previous) => {
      const changed = sizes.filter(([id, size]) => { const known = previous.get(id); return !known || known.width !== size.width || known.height !== size.height; });
      return changed.length ? new Map([...previous, ...changed]) : previous;
    });
    if (unified) return;
    const { live, done } = positionChanges(changes);
    if (live.length || done.length) setDragging((previous) => {
      const next = new Map(previous);
      for (const { id, position } of live) next.set(id, position);
      for (const { id } of done) next.delete(id);
      return next;
    });
    if (done.length) edit((p) => doc.moveNodes(p, workflowId, done));
    const removed: string[] = [];
    let nodes = useEditor.getState().selection.nodes;
    for (const change of changes) {
      const id = "id" in change ? nodeIdOf(change.id) : null;
      if (id === null) continue;
      if (change.type === "remove") removed.push(id);
      else if (change.type === "select") nodes = change.selected ? nodes.includes(id) ? nodes : [...nodes, id] : nodes.filter((item) => item !== id);
    }
    if (removed.length) edit((p) => doc.removeNodes(p, workflowId, removed));
    const current = useEditor.getState().selection;
    const kept = nodes.filter((id) => !removed.includes(id));
    if (kept.length !== current.nodes.length || kept.some((id, at) => id !== current.nodes[at])) select({ ...current, nodes: kept });
  }, [unified, workflowId, edit, select]);

  const onEdgesChange = useCallback((changes: EdgeChange[]) => {
    if (unified) return;
    const removed: string[] = [];
    let wires = useEditor.getState().selection.wires;
    for (const change of changes) {
      const id = "id" in change ? wireIdOf(change.id) : null;
      if (id === null) continue;
      if (change.type === "remove") removed.push(id);
      else if (change.type === "select") wires = change.selected ? wires.includes(id) ? wires : [...wires, id] : wires.filter((item) => item !== id);
    }
    if (removed.length) edit((p) => doc.removeWires(p, workflowId, removed));
    const current = useEditor.getState().selection;
    const kept = wires.filter((id) => !removed.includes(id));
    if (kept.length !== current.wires.length || kept.some((id, at) => id !== current.wires[at])) select({ ...current, wires: kept });
  }, [unified, workflowId, edit, select]);

  const check = useCallback((connection: Connection | { source: string; sourceHandle?: string | null; target: string; targetHandle?: string | null }) => {
    const from = endOf(connection.source, connection.sourceHandle);
    const to = endOf(connection.target, connection.targetHandle);
    if (!from || !to) return null;
    return { from, to, result: canConnect(useEditor.getState().doc ?? profile, workflowId, from, to) };
  }, [profile, workflowId]);

  const onConnect = useCallback((connection: Connection) => {
    const checked = check(connection);
    if (!checked) return;
    const { from, to, result } = checked;
    if (result.ok) { edit((p) => doc.addWire(p, workflowId, result.kind, from, to)); notify(null); }
    else notify({ tone: "error", text: t(refusalText[result.reason]), detail: result.from && result.to ? `${result.from.capability} (${result.from.kind}) → ${result.to.capability} (${result.to.kind})` : undefined });
  }, [check, edit, notify, workflowId]);

  // A drop React Flow refused never reaches onConnect: say why here.
  const onConnectEnd = useCallback((_: MouseEvent | TouchEvent, state: FinalConnectionState) => {
    if (state.isValid !== false || !state.fromHandle || !state.toHandle) return;
    const [source, target] = state.fromHandle.type === "source" ? [state.fromHandle, state.toHandle] : [state.toHandle, state.fromHandle];
    if (source.type === target.type) { notify({ tone: "error", text: t("editor.refuse.direction") }); return; }
    const checked = check({ source: source.nodeId, sourceHandle: source.id, target: target.nodeId, targetHandle: target.id });
    if (checked && !checked.result.ok) onConnect({ source: source.nodeId, sourceHandle: source.id ?? null, target: target.nodeId, targetHandle: target.id ?? null });
  }, [check, notify, onConnect]);

  const isValidConnection = useCallback((connection: Connection | { source: string; target: string; sourceHandle?: string | null; targetHandle?: string | null }) => check(connection)?.result.ok ?? false, [check]);

  const count = unified ? profile.workflows.reduce((sum, item) => sum + item.nodes.length, 0) : workflow?.nodes.length ?? 0;
  const menuNodes = selection.nodes;
  const menuBox = menuNodes.length === 1 ? boxOf(menuNodes[0]) : null;
  return <div className="graph-canvas" ref={frame} onDragOver={(event) => { if (!unified && event.dataTransfer.types.includes(PIECE_MIME)) { event.preventDefault(); event.dataTransfer.dropEffect = "copy"; } }}
    onDrop={(event) => {
      const raw = event.dataTransfer.getData(PIECE_MIME);
      if (!raw || unified) return;
      event.preventDefault();
      try { onAdd(JSON.parse(raw) as PieceRef, { x: event.clientX, y: event.clientY }); } catch { /* not ours */ }
    }}>
    <ReactFlow nodes={graph.nodes} edges={graph.edges} nodeTypes={nodeTypes}
      onNodesChange={onNodesChange} onEdgesChange={onEdgesChange} onConnect={onConnect} onConnectEnd={onConnectEnd} isValidConnection={isValidConnection}
      onNodeClick={(_, node) => { if (unified && node.type === "piece") { const { workflowId: target, item } = (node as PieceNode).data; showWorkflow(target); select({ nodes: [item.id], wires: [] }); } }}
      onNodeDoubleClick={(_, node) => {
        if (unified || node.type !== "piece") return;
        const inner = boxOf((node as PieceNode).data.item.id);
        if (inner) openBox(inner);
      }}
      onNodeContextMenu={(event, node) => { if (node.type === "piece") openMenu(event, (node as PieceNode).data.item.id); }}
      onSelectionContextMenu={(event) => openMenu(event, null)}
      onPaneClick={() => select({ nodes: [], wires: [] })}
      nodesDraggable={!unified} nodesConnectable={!unified} edgesReconnectable={false} elementsSelectable
      deleteKeyCode={unified ? null : settings.deleteKeys} multiSelectionKeyCode={["Meta", "Control", "Shift"]}
      snapToGrid={settings.snapToGrid} snapGrid={[settings.gridSize, settings.gridSize]}
      fitView fitViewOptions={{ padding: settings.fitPadding }} minZoom={settings.minZoom} maxZoom={settings.maxZoom}
      colorMode="dark" ariaLabelConfig={{ "controls.ariaLabel": t("graph.controls"), "controls.zoomIn.ariaLabel": t("graph.zoomIn"), "controls.zoomOut.ariaLabel": t("graph.zoomOut"), "controls.fitView.ariaLabel": t("graph.fit"), "node.a11yDescription.default": t("editor.nodeHelp"), "node.a11yDescription.keyboardDisabled": t("graph.nodeHelp") }}>
      <Background color="var(--rg-color-border)" gap={settings.snapToGrid ? settings.gridSize : undefined} />
      <Controls showInteractive={false} />
    </ReactFlow>
    {menu && menuNodes.length > 0 && <NodeMenu menu={menu} box={menuBox} count={menuNodes.length} onClose={() => setMenu(null)}
      onOpen={() => { if (menuBox) openBox(menuBox); }} onCreateBox={() => onCreateBox(menuNodes)} onArrange={onArrangeSelection} onDelete={onDeleteSelection} />}
    {count === 0 && !(workflow?.ports?.length) && <div className="graph-empty-overlay"><PackageOpen size={32} /><p>{t(unified ? "graph.emptyWorkflow" : "editor.emptyWorkflow")}</p></div>}
  </div>;
}
