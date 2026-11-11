// The one copy of the profile being edited. React Flow nodes and edges are derived from
// `doc`; its callbacks come back here as the pure changes of `document.ts`.
import { create } from "zustand";
import { registerUnsaved } from "@/lib/unsaved";
import type { Diagnostic, Profile } from "../model";
import { moveNodes, nodeOf, workflowOf } from "./document";
import type { Move } from "./layout";

export type Notice = { tone: "info" | "success" | "error"; text: string; detail?: string; action?: { label: string; run: () => void } };
export type Selection = { nodes: string[]; wires: string[] };

/** The last port sync, kept so it can be taken back while nothing changed after it. */
export type Synced = { before: Profile; after: Profile };

/** The last arrangement, kept so one step can be taken back. */
export type Arrangement = { workflowId: string; moves: Move[] };

export const ALL = "all";
const nothing: Selection = { nodes: [], wires: [] };

/** Whether the last arrangement can be taken back here: it was made in this workflow and its nodes have not moved since. */
export function canUndoArrange(state: { doc: Profile | null; arranged: Arrangement | null }, workflowId: string): boolean {
  const { doc, arranged } = state;
  const workflow = doc && arranged && arranged.workflowId === workflowId ? workflowOf(doc, workflowId) : undefined;
  return !!arranged && !!workflow && arranged.moves.every(({ id, to }) => {
    const node = nodeOf(workflow, id);
    return !!node && node.position.x === to.x && node.position.y === to.y;
  });
}

/** The trail after opening the box of workflow `id`: one step deeper, or back to it if it is already on the way. */
export function enterWorkflow(trail: string[], id: string): string[] {
  const at = trail.indexOf(id);
  return at >= 0 ? trail.slice(0, at + 1) : [...trail, id];
}

/** The trail without the workflows a profile no longer has, from the first one missing on. */
export function trailIn(trail: string[], profile: Profile): string[] {
  const gone = trail.findIndex((id) => !profile.workflows.some((workflow) => workflow.id === id));
  return gone < 0 ? trail : trail.slice(0, gone);
}

interface EditorState {
  doc: Profile | null;
  /** Not yet written by the core: the first save goes without `force`. */
  isNew: boolean;
  dirty: boolean;
  /** The workflow on screen, or `ALL`. */
  workflowId: string;
  /** How the workflow on screen was reached: the tab first, then each box opened (the breadcrumb). */
  trail: string[];
  selection: Selection;
  /** The last arrangement of nodes, while it can still be undone. */
  arranged: Arrangement | null;
  /** Grows with each arrangement or undo, so the canvas places the workflow's own ports again. */
  arrangeRev: number;
  /** The last port sync, while it can still be undone. */
  synced: Synced | null;
  /** What the last save or check said, for the profile on screen. */
  diagnostics: Diagnostic[];
  notice: Notice | null;
  busy: boolean;
  open: (profile: Profile | null, isNew?: boolean) => void;
  /** Applies one change to the edited profile; a change that returns the same profile is no change. */
  edit: (change: (profile: Profile) => Profile) => void;
  saved: (profile: Profile, diagnostics: Diagnostic[]) => void;
  showWorkflow: (id: string) => void;
  /** Opens the workflow inside a box: one step deeper in the trail. */
  openBox: (id: string) => void;
  /** Goes back to the workflow at `index` of the trail. */
  backTo: (index: number) => void;
  select: (selection: Selection) => void;
  /** Applies an arrangement of workflow `workflowId` (the moves of `layout.ts`) and remembers it for `undoArrange`. */
  arrange: (workflowId: string, moves: Move[]) => void;
  /** Puts the nodes of the last arrangement back; `false` when something moved them since and it is gone. */
  undoArrange: () => boolean;
  /** Shows a workflow of the profile and selects a node of it; what the profile does not have is left as it is. */
  focus: (at: { workflowId?: string; nodeId?: string }) => void;
  /** Takes a synced document (one change) and remembers the one before it for `undoSync`. */
  applySynced: (next: Profile) => void;
  /** Puts back the document from before the last sync; `false` once something changed after it. */
  undoSync: () => boolean;
  setDiagnostics: (diagnostics: Diagnostic[]) => void;
  notify: (notice: Notice | null) => void;
  setBusy: (busy: boolean) => void;
}

export const useEditor = create<EditorState>()((set, get) => ({
  doc: null,
  isNew: false,
  dirty: false,
  workflowId: ALL,
  trail: [],
  selection: nothing,
  arranged: null,
  arrangeRev: 0,
  synced: null,
  diagnostics: [],
  notice: null,
  busy: false,
  open: (profile, isNew = false) => set({
    doc: profile ? structuredClone(profile) : null, isNew, dirty: isNew, workflowId: profile?.main_workflow_id ?? ALL,
    trail: profile ? [profile.main_workflow_id] : [], selection: nothing, arranged: null, synced: null, diagnostics: [], notice: null,
  }),
  edit: (change) => set((state) => {
    if (!state.doc) return {};
    const doc = change(state.doc);
    if (doc === state.doc) return {};
    const trail = trailIn(state.trail, doc);
    if (trail.length === state.trail.length) return { doc, dirty: true };
    // The workflow on screen went away: show the last one of the trail still there.
    const shown = state.workflowId === ALL || doc.workflows.some((workflow) => workflow.id === state.workflowId);
    const back = trail.length ? trail : [doc.main_workflow_id];
    return shown ? { doc, dirty: true, trail } : { doc, dirty: true, trail: back, workflowId: back[back.length - 1], selection: nothing };
  }),
  saved: (profile, diagnostics) => set((state) => {
    const kept = state.workflowId === ALL || profile.workflows.some((workflow) => workflow.id === state.workflowId);
    return {
      doc: structuredClone(profile), isNew: false, dirty: false, diagnostics,
      workflowId: kept ? state.workflowId : profile.main_workflow_id,
      trail: kept ? trailIn(state.trail, profile) : [profile.main_workflow_id],
    };
  }),
  showWorkflow: (workflowId) => set({ workflowId, trail: workflowId === ALL ? [] : [workflowId], selection: nothing }),
  openBox: (id) => set((state) => ({ workflowId: id, trail: enterWorkflow(state.workflowId === ALL ? [] : state.trail.length ? state.trail : [state.workflowId], id), selection: nothing })),
  backTo: (index) => set((state) => index < 0 || index >= state.trail.length ? {} : { workflowId: state.trail[index], trail: state.trail.slice(0, index + 1), selection: nothing }),
  select: (selection) => set({ selection }),
  arrange: (workflowId, moves) => set((state) => {
    if (!state.doc || !moves.length) return {};
    const doc = moveNodes(state.doc, workflowId, moves.map(({ id, to }) => ({ id, position: to })));
    return doc === state.doc ? {} : { doc, dirty: true, arranged: { workflowId, moves }, arrangeRev: state.arrangeRev + 1 };
  }),
  undoArrange: () => {
    const state = get();
    const { arranged } = state;
    if (!arranged || !state.doc) return false;
    if (!canUndoArrange(state, arranged.workflowId)) { set({ arranged: null }); return false; }
    set({ doc: moveNodes(state.doc, arranged.workflowId, arranged.moves.map(({ id, from }) => ({ id, position: from }))), dirty: true, arranged: null, arrangeRev: state.arrangeRev + 1 });
    return true;
  },
  focus: ({ workflowId, nodeId }) => set((state) => {
    const workflow = state.doc?.workflows.find((item) => item.id === (workflowId ?? state.workflowId));
    if (!workflow) return {};
    const shown = workflow.id === state.workflowId ? {} : { workflowId: workflow.id, trail: [workflow.id] };
    const found = nodeId !== undefined && workflow.nodes.some((node) => node.id === nodeId);
    return { ...shown, selection: found ? { nodes: [nodeId], wires: [] } : workflow.id === state.workflowId ? state.selection : nothing };
  }),
  applySynced: (next) => set((state) => !state.doc || next === state.doc ? {} : { doc: next, dirty: true, synced: { before: state.doc, after: next } }),
  undoSync: () => {
    const { synced, doc } = get();
    if (!synced || doc !== synced.after) { set({ synced: null }); return false; }
    set({ doc: synced.before, dirty: true, synced: null });
    return true;
  },
  setDiagnostics: (diagnostics) => set({ diagnostics }),
  notify: (notice) => set({ notice }),
  setBusy: (busy) => set({ busy }),
}));

registerUnsaved(() => {
  const { doc, dirty } = useEditor.getState();
  return doc && dirty ? doc.name || doc.id : null;
});
