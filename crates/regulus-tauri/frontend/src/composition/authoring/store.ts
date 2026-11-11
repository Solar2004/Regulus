// Which module source is open, its unsaved text, and a counter the views watch to read
// modules again after a reload. One source is open at a time: its text lives in the editor
// while the panel is on screen, and here (`draft`) so that leaving the view or the panel
// closing by itself does not lose it. Another source asked for while there is unsaved text
// waits in `pending` until the person saves, discards or keeps editing.
import { create } from "zustand";
import { registerUnsaved } from "@/lib/unsaved";
import type { SourceRef } from "./logic";

/** Unsaved text of the open source and the file it was edited from (its text and revision). */
export interface SourceDraft {
  key: string;
  /** Project-relative path, for "unsaved changes in …". */
  path: string;
  text: string;
  base: { text: string; revision: string | null };
}

/** One key per local module of a project: what `source.*` addresses. */
export const sourceKey = (source: { projectId: string; module: string }) => `${source.projectId}\u0000${source.module}`;

interface AuthoringState {
  /** The module source shown in the source panel, or `null`. */
  source: SourceRef | null;
  /** Unsaved text of `source`, or `null` when the editor matches the file it read. */
  draft: SourceDraft | null;
  /** Another source asked for while `draft` was unsaved: shown after the person decides. */
  pending: SourceRef | null;
  /** Grows when modules changed (a reload, a new node): views read `module.list` and `module.manifest` again. */
  modulesRev: number;
  /** Shows `source`; with unsaved text in another one, asks first (`pending`). */
  openSource: (source: SourceRef) => void;
  /** Closes the panel and drops its unsaved text (the panel asked before). */
  closeSource: () => void;
  /** Records the unsaved text of source `key`, or `null` once it matches the file; ignored unless `key` is the open source. */
  keepDraft: (key: string, draft: Omit<SourceDraft, "key"> | null) => void;
  /** The person decided about `pending`: open it (the unsaved text was saved or discarded) or stay. */
  resolvePending: (open: boolean) => void;
  modulesChanged: () => void;
}

export const useAuthoring = create<AuthoringState>()((set) => ({
  source: null,
  draft: null,
  pending: null,
  modulesRev: 0,
  openSource: (source) => set((state) => {
    if (state.source && sourceKey(state.source) === sourceKey(source)) return { pending: null };
    return state.draft ? { pending: source } : { source, pending: null };
  }),
  closeSource: () => set({ source: null, draft: null, pending: null }),
  keepDraft: (key, draft) => set((state) => !state.source || sourceKey(state.source) !== key ? {} : { draft: draft && { key, ...draft } }),
  resolvePending: (open) => set((state) => !state.pending ? {} : open ? { source: state.pending, draft: null, pending: null } : { pending: null }),
  modulesChanged: () => set((state) => ({ modulesRev: state.modulesRev + 1 })),
}));

registerUnsaved(() => useAuthoring.getState().draft?.path ?? null);
