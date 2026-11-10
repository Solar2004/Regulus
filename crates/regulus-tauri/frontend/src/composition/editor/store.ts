// The one copy of the profile being edited. React Flow nodes and edges are derived from
// `doc`; its callbacks come back here as the pure changes of `document.ts`.
import { create } from "zustand";
import type { Diagnostic, Profile } from "../model";

export type Notice = { tone: "info" | "success" | "error"; text: string; detail?: string };
export type Selection = { nodes: string[]; wires: string[] };

export const ALL = "all";
const nothing: Selection = { nodes: [], wires: [] };

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
  /** Shows a workflow of the profile and selects a node of it; what the profile does not have is left as it is. */
  focus: (at: { workflowId?: string; nodeId?: string }) => void;
  setDiagnostics: (diagnostics: Diagnostic[]) => void;
  notify: (notice: Notice | null) => void;
  setBusy: (busy: boolean) => void;
}

export const useEditor = create<EditorState>()((set) => ({
  doc: null,
  isNew: false,
  dirty: false,
  workflowId: ALL,
  trail: [],
  selection: nothing,
  diagnostics: [],
  notice: null,
  busy: false,
  open: (profile, isNew = false) => set({
    doc: profile ? structuredClone(profile) : null, isNew, dirty: isNew, workflowId: profile?.main_workflow_id ?? ALL,
    trail: profile ? [profile.main_workflow_id] : [], selection: nothing, diagnostics: [], notice: null,
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
  focus: ({ workflowId, nodeId }) => set((state) => {
    const workflow = state.doc?.workflows.find((item) => item.id === (workflowId ?? state.workflowId));
    if (!workflow) return {};
    const shown = workflow.id === state.workflowId ? {} : { workflowId: workflow.id, trail: [workflow.id] };
    const found = nodeId !== undefined && workflow.nodes.some((node) => node.id === nodeId);
    return { ...shown, selection: found ? { nodes: [nodeId], wires: [] } : workflow.id === state.workflowId ? state.selection : nothing };
  }),
  setDiagnostics: (diagnostics) => set({ diagnostics }),
  notify: (notice) => set({ notice }),
  setBusy: (busy) => set({ busy }),
}));
