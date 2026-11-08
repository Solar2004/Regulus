// The one copy of the profile being edited. React Flow nodes and edges are derived from
// `doc`; its callbacks come back here as the pure changes of `document.ts`.
import { create } from "zustand";
import type { Diagnostic, Profile } from "../model";

export type Notice = { tone: "info" | "success" | "error"; text: string; detail?: string };
export type Selection = { nodes: string[]; wires: string[] };

export const ALL = "all";
const nothing: Selection = { nodes: [], wires: [] };

interface EditorState {
  doc: Profile | null;
  /** Not yet written by the core: the first save goes without `force`. */
  isNew: boolean;
  dirty: boolean;
  /** The workflow on screen, or `ALL`. */
  workflowId: string;
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
  select: (selection: Selection) => void;
  setDiagnostics: (diagnostics: Diagnostic[]) => void;
  notify: (notice: Notice | null) => void;
  setBusy: (busy: boolean) => void;
}

export const useEditor = create<EditorState>()((set) => ({
  doc: null,
  isNew: false,
  dirty: false,
  workflowId: ALL,
  selection: nothing,
  diagnostics: [],
  notice: null,
  busy: false,
  open: (profile, isNew = false) => set({
    doc: profile ? structuredClone(profile) : null, isNew, dirty: isNew, workflowId: profile?.main_workflow_id ?? ALL,
    selection: nothing, diagnostics: [], notice: null,
  }),
  edit: (change) => set((state) => {
    if (!state.doc) return {};
    const doc = change(state.doc);
    return doc === state.doc ? {} : { doc, dirty: true };
  }),
  saved: (profile, diagnostics) => set((state) => ({
    doc: structuredClone(profile), isNew: false, dirty: false, diagnostics,
    workflowId: state.workflowId === ALL || profile.workflows.some((workflow) => workflow.id === state.workflowId) ? state.workflowId : profile.main_workflow_id,
  })),
  showWorkflow: (workflowId) => set({ workflowId, selection: nothing }),
  select: (selection) => set({ selection }),
  setDiagnostics: (diagnostics) => set({ diagnostics }),
  notify: (notice) => set({ notice }),
  setBusy: (busy) => set({ busy }),
}));
