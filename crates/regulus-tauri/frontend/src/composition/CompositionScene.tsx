import { useCallback, useEffect, useRef, useState } from "react";
import { ReactFlowProvider, useReactFlow } from "@xyflow/react";
import { Blocks, ChevronRight, CircleCheck, CircleAlert, FilePlus2, FileStack, GitBranch, Info, LayoutDashboard, ListChecks, Network, Play, Plus, Save, Settings2, Trash2, Undo2, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { flushEdits } from "@/components/code/flush";
import { t } from "@/lib/i18n";
import { useCore } from "@/core/connection";
import { useRegulusUi } from "@/store/useRegulusUi";
import { useComposition } from "./client";
import * as doc from "./editor/document";
import { bySeverity, checkProfile, deleteProfile, getProfile, messageOf, saveProfile, saveTemplate, startExecution } from "./editor/api";
import { CODE, codeOf, moduleShape, syncProfile } from "./authoring/api";
import { applySync, hasShape, localIdOf, reloadSummary, syncCounts, syncFits, type SyncOrigin } from "./authoring/logic";
import { NewNodeDialog } from "./authoring/NewNodeDialog";
import { SourcePanel } from "./authoring/SourcePanel";
import { SyncDialog } from "./authoring/SyncDialog";
import { useAuthoring } from "./authoring/store";
import { defaultsOf } from "./editor/fields";
import type { NodeCreated, Port, ProfileSynced, ProjectReloaded } from "./model";

/** A sync preview and the document it was asked for. */
type Preview = { synced: ProfileSynced; origin: SyncOrigin; nodeIds: string[] | undefined };
import { EditorCanvas } from "./editor/Canvas";
import { measuredSizes } from "./editor/flow";
import { arrangeWorkflow, placeNode, type LayoutOptions } from "./editor/layout";
import { ConfirmDialog, CreateBoxDialog, NewProfileDialog, SaveTemplateDialog, SettingsDialog, type Confirm } from "./editor/dialogs";
import { NodeInspector, SelectionInspector, WorkflowInspector } from "./editor/Inspector";
import { SeverityMark } from "./editor/nodes";
import { Palette, type PieceRef } from "./editor/Palette";
import { boxNode, findNodeType, nodeFromType } from "./editor/palette";
import { useCanvasSettings } from "./editor/settings";
import { ALL, canUndoArrange, useEditor, type Notice } from "./editor/store";
import "@xyflow/react/dist/style.css";
import "./composition.css";
import "./editor/editor.css";

type Composition = ReturnType<typeof useComposition>;

const noticeIcon = { info: Info, success: CircleCheck, error: CircleAlert } as const;

function NoticeLine({ notice, onClose }: { notice: Notice; onClose: () => void }) {
  const Icon = noticeIcon[notice.tone];
  return <div className={`editor-notice tone-${notice.tone}`} role={notice.tone === "error" ? "alert" : "status"}>
    <Icon size={14} strokeWidth={1.75} aria-hidden="true" /><strong>{notice.text}</strong>{notice.detail && <code>{notice.detail}</code>}
    {notice.action && <Button variant="outline" size="xs" className="notice-action" onClick={notice.action.run}>{notice.action.label}</Button>}
    <button onClick={onClose} aria-label={t("editor.dismiss")}><X size={14} /></button>
  </div>;
}

function Editor({ composition }: { composition: Composition }) {
  const { client, profiles, modules, manifests, reload } = composition;
  const coreSource = useCore((s) => s.source);
  const settings = useCanvasSettings();
  const { doc: profile, isNew, dirty, workflowId, trail, selection, arranged, arrangeRev, diagnostics, notice, busy } = useEditor();
  const { open, edit, saved, showWorkflow, openBox, backTo, select, focus, setDiagnostics, notify, setBusy } = useEditor.getState();
  const [boxing, setBoxing] = useState<string[] | null>(null);
  const [savingTemplate, setSavingTemplate] = useState(false);
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  const [creating, setCreating] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [syncPreview, setSyncPreview] = useState<Preview | null>(null);
  const [creatingNode, setCreatingNode] = useState(false);
  const source = useAuthoring((s) => s.source);
  const { openSource, closeSource, modulesChanged } = useAuthoring.getState();
  const canvasRef = useRef<HTMLDivElement>(null);
  const flow = useReactFlow();

  // A profile asked for from another view ("Open in canvas") opens; otherwise the first one
  // opens by itself, and a draft survives leaving the view.
  const requested = useRegulusUi((s) => s.canvasProfileId);
  useEffect(() => { if (!requested && !useEditor.getState().doc && profiles[0]) open(profiles[0]); }, [requested, profiles, open]);

  /** Runs `action` now, or after asking when there are unsaved changes. */
  const guard = (action: () => void) => {
    flushEdits();
    if (!useEditor.getState().dirty) { action(); return; }
    setConfirm({ title: t("editor.discard.title"), description: t("editor.discard.description"), action: t("editor.discard.action"), destructive: true, run: action });
  };

  const fail = useCallback((text: Parameters<typeof t>[0], error: unknown) => notify({ tone: "error", text: t(text), detail: messageOf(error) }), [notify]);

  const openProfile = (id: string, then?: () => void) => guard(async () => {
    if (!client) return;
    try { open(await getProfile(client, id)); then?.(); } catch (error) { fail("editor.notice.openFailed", error); }
  });

  // Another view may also say where to land: a workflow, and a node of it to select. Kept
  // by the request it came with, so an effect that runs twice lands twice on the same place.
  const landing = useRef<{ profileId: string; at: ReturnType<typeof useRegulusUi.getState>["canvasFocus"] } | null>(null);
  useEffect(() => {
    if (!requested) { landing.current = null; return; }
    if (!client) return;
    if (landing.current?.profileId !== requested) landing.current = { profileId: requested, at: useRegulusUi.getState().canvasFocus };
    const { at } = landing.current;
    const land = () => { if (at) focus(at); };
    useRegulusUi.setState({ canvasProfileId: null, canvasFocus: null });
    if (useEditor.getState().doc?.id !== requested) { reload(); openProfile(requested, land); } else land();
  }, [requested, client]);

  const save = useCallback(async () => {
    flushEdits();
    const state = useEditor.getState();
    if (!state.doc || !client || state.busy) return;
    const sent = state.doc;
    setBusy(true);
    try {
      let result;
      try { result = await saveProfile(client, sent, state.isNew); } catch (error) { fail("editor.notice.saveFailed", error); return; }
      // Written: from now on a save replaces it, whatever happens next.
      const clean = useEditor.getState().doc === sent;
      useEditor.setState({ isNew: false, diagnostics: result.diagnostics, ...(clean ? { dirty: false } : {}) });
      reload();
      let fresh;
      try { fresh = await getProfile(client, result.profile_id); } catch (error) { fail("editor.notice.reloadFailed", error); return; }
      // Edits made while saving stay; only what was sent is now clean.
      if (useEditor.getState().doc === sent) saved(fresh, result.diagnostics);
      const warnings = result.diagnostics.filter((d) => d.severity !== "info").length;
      notify({ tone: "success", text: t(warnings ? "editor.notice.savedWithWarnings" : "editor.notice.saved"), detail: result.path });
    } finally { setBusy(false); }
  }, [client, fail, notify, reload, saved, setBusy]);

  const check = async () => {
    flushEdits();
    if (!client || !profile) return;
    try {
      const found = await checkProfile(client, profile.id);
      setDiagnostics(found);
      const errors = found.filter((d) => d.severity === "error").length;
      notify({ tone: errors ? "error" : found.length ? "info" : "success", text: t(found.length ? "editor.notice.checked" : "editor.notice.clean"), detail: dirty ? t("editor.notice.savedVersion") : undefined });
    } catch (error) { fail("editor.notice.checkFailed", error); }
  };

  const run = async () => {
    flushEdits();
    if (!client || !profile) return;
    try {
      const execution = await startExecution(client, profile.id, profile.main_workflow_id);
      const disabled = (execution.disabled ?? []).map((node) => `${node.node_id} (${node.missing.join(", ")})`).join("; ");
      notify({ tone: disabled ? "info" : "success", text: `${t("editor.notice.started")} ${execution.id}`, detail: [disabled && `${t("editor.notice.disabled")} ${disabled}`, dirty && t("editor.notice.savedVersion")].filter(Boolean).join(" · ") || undefined });
    } catch (error) { fail("editor.notice.runFailed", error); }
  };

  const remove = () => {
    if (!client || !profile) return;
    if (isNew) { guard(() => open(profiles[0] ?? null)); return; }
    setConfirm({
      title: t("editor.deleteProfile.title"), description: `${t("editor.deleteProfile.description")} ${profile.id}`, action: t("editor.deleteProfile.action"), destructive: true,
      run: async () => {
        try {
          const result = await deleteProfile(client, profile.id);
          const rest = profiles.filter((item) => item.id !== result.profile_id);
          open(rest[0] ? await getProfile(client, rest[0].id) : null);
          notify({ tone: "success", text: t("editor.notice.deleted"), detail: result.path });
          reload();
        } catch (error) { fail("editor.notice.deleteFailed", error); }
      },
    });
  };

  /** What an arrangement and a free-place search share: the fallback footprint and the grid. */
  const layoutBase = () => ({ fallback: { width: settings.nodeWidth, height: settings.nodeHeight }, grid: settings.snapToGrid ? settings.gridSize : undefined });
  const layoutOptions = (): LayoutOptions => ({ ...layoutBase(), direction: settings.layoutDirection, layerGap: settings.layoutLayerGap, nodeGap: settings.layoutNodeGap });

  const addPiece = async (piece: PieceRef, screen?: { x: number; y: number }) => {
    // A node type whose ports depend on its config starts with the core's shape for its defaults.
    const asked = useEditor.getState().doc?.id;
    let shaped: Port[] | undefined;
    if (piece.kind === "type" && client) {
      const found = findNodeType(manifests, piece.key);
      if (found && hasShape(found.nodeType)) {
        try { shaped = (await moduleShape(client, found.manifest.module.id, found.nodeType.type, defaultsOf(found.nodeType.field))).ports; }
        catch { /* an older core: the manifest's fixed ports */ }
      }
    }
    const state = useEditor.getState();
    const current = state.doc && doc.workflowOf(state.doc, state.workflowId);
    // Another profile opened while the shape was on its way: the node was not asked for there.
    if (!state.doc || !current || state.doc.id !== asked) return;
    let point = screen;
    if (!point) {
      const rect = canvasRef.current?.getBoundingClientRect();
      point = rect ? { x: rect.left + rect.width / 2, y: rect.top + rect.height / 3 } : { x: 0, y: 0 };
    }
    // The new node goes where it was dropped, or in the nearest free place beside the nodes it
    // would touch (their measured size when React Flow has it). No other node moves.
    const sizes = measuredSizes(flow.getNodes());
    const tallest = Math.max(settings.nodeHeight, ...[...sizes.values()].map((size) => size.height));
    const position = placeNode(current, flow.screenToFlowPosition(point), { width: settings.nodeWidth, height: tallest }, sizes, { ...layoutBase(), gap: settings.cascadeStep });
    const taken = current.nodes.map((node) => node.id);
    let created;
    if (piece.kind === "type") {
      const found = findNodeType(manifests, piece.key);
      if (!found) return;
      created = nodeFromType(found.manifest, found.nodeType, position, taken, shaped);
    } else {
      const inner = doc.workflowOf(state.doc, piece.workflowId);
      if (!inner) return;
      created = boxNode(inner, position, taken);
    }
    const node = created;
    edit((p) => doc.addNode(p, current.id, node));
    select({ nodes: [node.id], wires: [] });
  };

  const deleteSelection = () => {
    const { nodes, wires } = useEditor.getState().selection;
    edit((p) => doc.removeWires(doc.removeNodes(p, workflowId, nodes), workflowId, wires));
    select({ nodes: [], wires: [] });
  };

  /** Moves the nodes into a new workflow named `name` and puts its box where they were. */
  const createBox = (nodeIds: string[], name: string) => {
    const state = useEditor.getState();
    if (!state.doc || state.workflowId === ALL) return;
    const made = doc.createBoxFromSelection(state.doc, state.workflowId, nodeIds, name);
    if (!made) return;
    // The box lands at the middle of what it replaced: keep it off the nodes that stayed.
    const kept = doc.workflowOf(made.profile, state.workflowId)!;
    const box = doc.nodeOf(kept, made.boxId)!;
    const sizes = measuredSizes(flow.getNodes());
    const position = placeNode({ ...kept, nodes: kept.nodes.filter((item) => item.id !== box.id) }, box.position, { width: settings.nodeWidth, height: settings.nodeHeight }, sizes, { ...layoutBase(), gap: settings.cascadeStep });
    edit(() => doc.moveNodes(made.profile, state.workflowId, [{ id: box.id, position }]));
    select({ nodes: [made.boxId], wires: [] });
    notify({ tone: "success", text: t("editor.box.created", { workflow: made.workflowId }), detail: made.ports.length ? made.ports.map((port) => `${port.direction === "input" ? "→" : "←"} ${port.id} (${port.kind} ${port.capability})`).join(" · ") : t("editor.box.noPorts") });
  };

  /** Arranges the workflow on screen, or only the nodes `only` names; the rest stay put. */
  const arrange = (only: string[] | null) => {
    const state = useEditor.getState();
    if (!state.doc || state.workflowId === ALL) return;
    const { moves } = arrangeWorkflow(state.doc, state.workflowId, only, measuredSizes(flow.getNodes()), layoutOptions());
    if (!moves.length) { notify({ tone: "info", text: t("editor.layout.nothing") }); return; }
    state.arrange(state.workflowId, moves);
    notify({ tone: "success", text: t("editor.layout.arranged", { count: moves.length }), detail: t("editor.layout.arrangedDetail") });
    if (!only) requestAnimationFrame(() => void flow.fitView({ padding: settings.fitPadding }));
  };

  const undoArrange = () => {
    const count = useEditor.getState().arranged?.moves.length ?? 0;
    notify(useEditor.getState().undoArrange() ? { tone: "info", text: t("editor.layout.undone", { count }) } : { tone: "error", text: t("editor.layout.undoGone") });
  };

  /**
   * Takes a sync preview into the document as one change, with a way back, only if the
   * document is still the one the preview was asked for: another profile leaves it unused,
   * and any change since asks the core again (`auto` keeps applying by itself only when no
   * wire would go).
   */
  const applyPreview = (preview: Preview, auto: boolean) => {
    const state = useEditor.getState();
    const fits = syncFits(preview.origin, state.doc);
    if (fits === "other") { notify({ tone: "info", text: t("authoring.sync.otherProfile") }); return; }
    if (fits === "changed") { void requestSync(preview.nodeIds, auto, true); return; }
    const next = applySync(state.doc!, preview.synced);
    if (next === state.doc) { notify({ tone: "info", text: t("authoring.sync.none") }); return; }
    state.applySynced(next);
    const counts = syncCounts(preview.synced);
    notify({
      tone: "success", text: t("authoring.sync.applied"),
      detail: preview.synced.nodes.filter((item) => item.added.length || item.removed.length || item.changed.length).map((item) => item.node_id).join(", ") + (counts.wires ? ` · −${counts.wires}` : ""),
      action: { label: t("authoring.sync.undo"), run: () => notify(useEditor.getState().undoSync() ? { tone: "info", text: t("authoring.sync.undone") } : { tone: "error", text: t("authoring.sync.undoGone") }) },
    });
  };

  /**
   * Asks the core what syncing ports would do to the document as it is now (`profile.sync`,
   * nothing written). `auto` (a field the shape reads just changed) applies it at once when
   * no wire would go; anything else is shown first. `again`: the document changed under an
   * earlier preview, so this one replaces it.
   */
  const requestSync = async (nodeIds: string[] | undefined, auto: boolean, again = false) => {
    flushEdits();
    const state = useEditor.getState();
    if (!client || !state.doc) return;
    const origin: SyncOrigin = { profileId: state.doc.id, doc: state.doc };
    let synced: ProfileSynced;
    try {
      synced = await syncProfile(client, state.doc, nodeIds ? { workflowId: state.workflowId === ALL ? undefined : state.workflowId, nodeIds } : {});
    } catch (error) {
      if (syncFits(origin, useEditor.getState().doc) === "other") return;
      notify({ tone: "error", text: t(codeOf(error) === CODE.methodNotFound ? "authoring.sync.unsupported" : "authoring.sync.failed"), detail: messageOf(error) });
      return;
    }
    const preview: Preview = { synced, origin, nodeIds };
    const fits = syncFits(origin, useEditor.getState().doc);
    if (fits === "other") return;
    if (fits === "changed") { void requestSync(nodeIds, auto, true); return; }
    if (!syncCounts(synced).nodes) { if (!auto && !again) notify({ tone: "info", text: t("authoring.sync.none") }); return; }
    if (auto && !synced.drops_wires) applyPreview(preview, true);
    else {
      setSyncPreview(preview);
      if (again && !auto) notify({ tone: "info", text: t("authoring.sync.refreshed") });
    }
  };

  /** After a reload: modules and manifests are read again, and stale nodes of this profile are offered a review. */
  const reloaded = (result: ProjectReloaded) => {
    modulesChanged();
    const stale = reloadSummary(result, useEditor.getState().doc?.id ?? null).stale;
    if (stale.length) notify({ tone: "info", text: t("authoring.reload.stale", { count: stale.length }), detail: stale.map((item) => `${item.workflow_id}.${item.node_id}`).join(", "),
      action: { label: t("authoring.sync.review"), run: () => void requestSync(undefined, false) } });
  };

  const nodeCreated = (created: NodeCreated) => {
    modulesChanged();
    openSource({ projectId: created.project_id, module: localIdOf(created.module_id), path: created.source, moduleId: created.module_id });
    notify({ tone: created.module.outcome === "blocked" ? "info" : "success", text: t(created.module.outcome === "blocked" ? "authoring.newNode.createdBlocked" : "authoring.newNode.created", { module: created.module_id }),
      detail: created.notes.join(" · ") || undefined });
  };

  const trustProject = async (path: string) => { if (client) await client.request("project.trust", { path }); };

  const addWorkflow = () => {
    let id = "";
    edit((p) => { const added = doc.addWorkflow(p, t("editor.workflow.newName")); id = added.id; return added.profile; });
    if (id) showWorkflow(id);
  };

  const removeWorkflow = (id: string) => setConfirm({
    title: t("editor.workflow.deleteTitle"),
    description: [`${t("editor.workflow.deleteDescription")} ${id}`, ...(() => {
      const boxes = profile ? doc.boxesOf(profile, id) : [];
      return boxes.length ? [`${t("editor.workflow.deleteBoxes")} ${boxes.map((box) => `${box.workflowId}.${box.nodeId}`).join(", ")}`] : [];
    })()].join(" "), action: t("editor.workflow.delete"), destructive: true,
    run: () => { showWorkflow(useEditor.getState().doc?.main_workflow_id ?? ALL); edit((p) => doc.removeWorkflow(p, id)); },
  });

  // Ctrl/Cmd + the save key saves. Leaving the page with unsaved changes asks (the shell's `guardUnload`).
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      // A key an editor already took (a source file's own save) is not the profile's.
      if (event.defaultPrevented) return;
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === settings.saveKey) { event.preventDefault(); void save(); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [save, settings.saveKey]);

  // Autosave: a saved profile is written `autosaveMs` after its last change.
  useEffect(() => {
    if (!settings.autosave || !dirty || isNew) return;
    const timer = setTimeout(() => void save(), settings.autosaveMs);
    return () => clearTimeout(timer);
  }, [settings.autosave, settings.autosaveMs, dirty, isNew, profile, save]);

  const newProfileDialog = <NewProfileDialog open={creating} taken={[...profiles.map((item) => item.id), ...(profile && isNew ? [profile.id] : [])]} onClose={() => setCreating(false)} onCreate={(id, name) => open(doc.newProfile(id, name, t("editor.workflow.mainName")), true)} />;
  if (!profile) return <div className="graph-connection-state" role="status"><GitBranch size={36} strokeWidth={1.25} /><h2>{t("graph.noProfiles")}</h2><p>{t("editor.noProfilesDescription")}</p>
    <Button onClick={() => setCreating(true)}><FilePlus2 size={14} />{t("editor.newProfile.title")}</Button>
    {newProfileDialog}
  </div>;

  const workflow = workflowId === ALL ? undefined : doc.workflowOf(profile, workflowId);
  const options = isNew ? [...profiles.map((item) => ({ id: item.id, name: item.name })), { id: profile.id, name: profile.name }] : profiles.map((item) => ({ id: item.id, name: item.name }));
  const single = selection.nodes.length === 1 && selection.wires.length === 0 && workflow ? doc.nodeOf(workflow, selection.nodes[0]) : undefined;
  const nodeCount = workflow ? workflow.nodes.length : profile.workflows.reduce((sum, item) => sum + item.nodes.length, 0);
  const wireCount = workflow ? workflow.wires.length : profile.workflows.reduce((sum, item) => sum + item.wires.length, 0);
  const sorted = bySeverity(diagnostics);
  const undoable = canUndoArrange({ doc: profile, arranged }, workflowId);

  return <>
    <header className="graph-toolbar">
      <div><h1>{t("canvas.title")}</h1>{dirty && <span className="dirty-mark" title={t("editor.dirty")}>{t("editor.dirty")}</span>}{isNew && <span className="graph-mode">{t("editor.unsavedNew")}</span>}</div>
      <label>{t("graph.profile")}<select value={profile.id} onChange={(event) => openProfile(event.target.value)} disabled={busy}>{options.map((item) => <option value={item.id} key={item.id}>{item.name || item.id}</option>)}</select></label>
      <div className="toolbar-actions">
        <Button variant="outline" size="sm" onClick={() => guard(() => setCreating(true))}><FilePlus2 size={14} />{t("editor.action.new")}</Button>
        <Button size="sm" onClick={() => void save()} disabled={busy || (!dirty && !isNew)} title={t("editor.action.saveHint")}><Save size={14} />{t("editor.action.save")}</Button>
        <Button variant="outline" size="sm" onClick={() => void check()} disabled={busy || isNew}><ListChecks size={14} />{t("editor.action.check")}</Button>
        <Button variant="outline" size="sm" onClick={() => void run()} disabled={busy || isNew}><Play size={14} />{t("editor.action.run")}</Button>
        {workflow && <>
          <Button variant="outline" size="sm" onClick={() => arrange(null)} title={t("editor.layout.arrangeWorkflowHint")}><Network size={14} />{t("editor.layout.arrangeWorkflow")}</Button>
          <Button variant="outline" size="sm" onClick={() => arrange(selection.nodes)} disabled={selection.nodes.length < 2} title={t("editor.layout.arrangeSelectionHint")}><LayoutDashboard size={14} />{t("editor.layout.arrangeSelection")}</Button>
          {undoable && <Button variant="ghost" size="sm" onClick={undoArrange} title={t("editor.layout.undoHint")}><Undo2 size={14} />{t("editor.layout.undo")}</Button>}
        </>}
        <Button variant="outline" size="sm" onClick={() => { flushEdits(); setSavingTemplate(true); }} disabled={busy || isNew} title={t("editor.template.hint")}><FileStack size={14} />{t("editor.template.action")}</Button>
        <Button variant="ghost" size="sm" onClick={remove} disabled={busy}><Trash2 size={14} />{t("editor.action.delete")}</Button>
        <Button variant="ghost" size="icon-sm" onClick={() => setShowSettings(true)} aria-label={t("editor.settings.title")}><Settings2 size={14} /></Button>
      </div>
      <span className="graph-source">{t(coreSource === "mock" ? "graph.mock" : "graph.core")}</span>
    </header>
    <nav className="workflow-tabs" aria-label={t("graph.workflows")}>
      <button aria-current={workflowId === ALL ? "page" : undefined} onClick={() => showWorkflow(ALL)}><GitBranch size={14} />{t("graph.all")}</button>
      {profile.workflows.map((item) => <button key={item.id} aria-current={workflowId === item.id ? "page" : undefined} onClick={() => showWorkflow(item.id)}>{item.kind === "module" ? <Blocks size={14} /> : <GitBranch size={14} />}{item.name || item.id}</button>)}
      <button onClick={addWorkflow}><Plus size={14} />{t("editor.workflow.add")}</button>
    </nav>
    {trail.length > 1 && <nav className="workflow-trail" aria-label={t("editor.box.trail")}>
      <ol>{trail.map((id, index) => <li key={`${id}:${index}`}>
        {index > 0 && <ChevronRight size={12} aria-hidden="true" />}
        {index === trail.length - 1 ? <span aria-current="page">{doc.workflowOf(profile, id)?.name || id}</span>
          : <button type="button" onClick={() => backTo(index)}>{doc.workflowOf(profile, id)?.name || id}</button>}
      </li>)}</ol>
    </nav>}
    <div className="graph-body">
      {workflow && !source && <Palette manifests={manifests} modules={modules} profile={profile} workflowId={workflow.id} onAdd={(piece) => void addPiece(piece)} onNewNode={client ? () => setCreatingNode(true) : undefined} />}
      <div className="editor-center" ref={canvasRef}>
        <EditorCanvas key={`${profile.id}:${workflowId}`} profile={profile} workflowId={workflowId} modules={modules} diagnostics={diagnostics} selection={selection} layoutRev={arrangeRev} onAdd={(piece, at) => void addPiece(piece, at)}
          onCreateBox={setBoxing} onArrangeSelection={() => arrange(useEditor.getState().selection.nodes)} onDeleteSelection={deleteSelection} />
        {sorted.length > 0 && <section className="editor-diagnostics" aria-label={t("editor.diagnostics.title")}>
          <header><h2>{t("editor.diagnostics.title")}</h2><button onClick={() => setDiagnostics([])} aria-label={t("editor.dismiss")}><X size={14} /></button></header>
          <ul>{sorted.map((d, index) => <li key={index}><button disabled={!d.node_id} onClick={() => { if (d.node_id) { showWorkflow(d.workflow_id ?? workflowId); select({ nodes: [d.node_id], wires: [] }); } }}>
            <SeverityMark severity={d.severity} /><span>{d.message}</span><code>{[d.workflow_id, d.node_id ?? d.wire_id].filter(Boolean).join(" · ")} {d.code}</code>
          </button></li>)}</ul>
        </section>}
      </div>
      {source && client && <SourcePanel key={`${source.projectId}/${source.module}`} rpc={client} source={source} profileId={profile.id}
        onClose={closeSource} onReloaded={reloaded} onTrust={trustProject} />}
      {workflow && (single
        ? <NodeInspector key={single.id} profile={profile} workflowId={workflow.id} node={single} manifests={manifests} modules={modules} diagnostics={diagnostics} edit={edit}
            onOpenWorkflow={openBox} onCreateBox={() => setBoxing([single.id])} onDelete={deleteSelection} onClose={() => select({ nodes: [], wires: [] })}
            rpc={client} onEditSource={openSource} onSync={(nodeIds, auto) => void requestSync(nodeIds, auto)} />
        : selection.nodes.length + selection.wires.length > 0
          ? <SelectionInspector nodes={selection.nodes.length} wires={selection.wires.length} onCreateBox={() => setBoxing(selection.nodes)} onArrange={() => arrange(selection.nodes)} onDelete={deleteSelection} />
          : <WorkflowInspector key={workflow.id} profile={profile} workflow={workflow} manifests={manifests} edit={edit} onDelete={() => removeWorkflow(workflow.id)} />)}
    </div>
    {notice && <NoticeLine notice={notice} onClose={() => notify(null)} />}
    <footer className="graph-footer"><div><span className="wire-sample uses" />{t("graph.uses")}<span className="wire-sample then" />{t("graph.then")}</div><span>{nodeCount} {t("graph.nodes")} · {wireCount} {t("graph.connections")}</span><span>{t(workflow ? "editor.footer.edit" : "editor.footer.all")}</span></footer>
    <ConfirmDialog confirm={confirm} onClose={() => setConfirm(null)} />
    <SyncDialog preview={syncPreview?.synced ?? null} onClose={() => setSyncPreview(null)} onApply={() => { if (syncPreview) applyPreview(syncPreview, false); }} />
    {client && <NewNodeDialog open={creatingNode} rpc={client} preferredProject={profile.project_id} onClose={() => setCreatingNode(false)} onCreated={nodeCreated} />}
    {newProfileDialog}
    <SettingsDialog open={showSettings} onClose={() => setShowSettings(false)} />
    <CreateBoxDialog nodes={boxing} workflowIds={profile.workflows.map((item) => item.id)} onClose={() => setBoxing(null)} onCreate={(name) => { if (boxing) createBox(boxing, name); }} />
    {client && <SaveTemplateDialog key={profile.id} open={savingTemplate} profileId={profile.id} workflowId={workflow?.id ?? null} dirty={dirty}
      onClose={() => setSavingTemplate(false)} onSave={(params) => saveTemplate(client, params)} onOpenTemplates={() => useRegulusUi.getState().setView("templates")} />}
  </>;
}

export default function CompositionScene() {
  const composition = useComposition();
  const source = useCore((s) => s.source);
  const { status, reason, retry } = composition;
  return <section className="composition-scene">
    {status === "ready" ? <ReactFlowProvider><Editor composition={composition} /></ReactFlowProvider>
      : <div className="graph-connection-state" role="status"><GitBranch size={36} strokeWidth={1.25} /><h2>{t(status === "loading" ? "graph.loading" : status === "incompatible" ? "graph.incompatible" : "graph.offline")}</h2><p>{t(status === "loading" ? "graph.loadingDescription" : source === "mock" ? "graph.offlineDescription" : "graph.offlineCore")}</p>{reason && status !== "loading" && <code className="graph-reason">{reason}</code>}{status !== "loading" && <Button variant="outline" onClick={retry}>{t("graph.retry")}</Button>}</div>}
  </section>;
}

