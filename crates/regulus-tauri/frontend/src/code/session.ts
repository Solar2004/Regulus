// What the workspace pane of a session showed last, so switching panels or views draws it
// at once, and the hook its panels ask the core with. Only a cache: the folder is the
// truth, and it is asked again whenever the agent finishes something. Not persisted.
import { useCallback, useEffect, useRef, useState } from "react";
import { create } from "zustand";
import { callFailure, type CallFailure } from "./client";
import { ancestorsOf, directChildren, type SummaryView } from "./logic";
import type { WorkspaceEntry, WorkspaceListing } from "./model";

export interface WorkspaceView {
  summary: SummaryView | null;
  /** What sits in each folder asked for, by its path ("" is the root). */
  listings: Record<string, WorkspaceEntry[]>;
  /** Folders whose listing the module cut. */
  cut: string[];
  /** Folders open in the tree. */
  open: string[];
  /** The file in the viewer, and the line to show in it. */
  file: { path: string; line: number | null } | null;
  /** The changed path whose diff shows. */
  change: string | null;
  /** What the search box holds. */
  query: string;
}

const empty: WorkspaceView = { summary: null, listings: {}, cut: [], open: [], file: null, change: null, query: "" };

interface WorkspaceCacheState {
  byKey: Record<string, WorkspaceView>;
  patch: (key: string, change: (view: WorkspaceView) => Partial<WorkspaceView>) => void;
}

export const useWorkspaceCache = create<WorkspaceCacheState>()((set) => ({
  byKey: {},
  patch: (key, change) => set((state) => {
    const before = state.byKey[key] ?? empty;
    return { byKey: { ...state.byKey, [key]: { ...before, ...change(before) } } };
  }),
}));

export const workspaceKey = (executionId: string, nodeId: string) => `${executionId}/${nodeId}`;
export const workspaceView = (state: WorkspaceCacheState, key: string): WorkspaceView => state.byKey[key] ?? empty;

/** The changes one key of the cache takes, as the panels call them. */
export function workspaceActions(key: string) {
  const patch = (change: (view: WorkspaceView) => Partial<WorkspaceView>) => useWorkspaceCache.getState().patch(key, change);
  return {
    summary: (summary: SummaryView) => patch(() => ({ summary })),
    listing: (listing: WorkspaceListing) => patch((view) => ({
      listings: { ...view.listings, [listing.path]: directChildren(listing.path, listing.entries) },
      cut: listing.truncated ? [...new Set([...view.cut, listing.path])] : view.cut.filter((path) => path !== listing.path),
    })),
    /** A folder that is gone: forgotten, with what was open under it. */
    forget: (folder: string) => patch((view) => {
      const { [folder]: _gone, ...listings } = view.listings;
      return { listings, open: view.open.filter((path) => path !== folder && !path.startsWith(`${folder}/`)) };
    }),
    toggle: (folder: string) => patch((view) => ({ open: view.open.includes(folder) ? view.open.filter((path) => path !== folder) : [...view.open, folder] })),
    /** Shows a file in the viewer, with the folders above it open. */
    show: (path: string, line: number | null = null) => patch((view) => ({ file: { path, line }, open: [...new Set([...view.open, ...ancestorsOf(path)])] })),
    closeFile: () => patch(() => ({ file: null })),
    change: (path: string | null) => patch(() => ({ change: path })),
    query: (query: string) => patch(() => ({ query })),
  };
}

export interface Asked<T> {
  data: T | null;
  failure: CallFailure | null;
  loading: boolean;
  /** Asks again now. */
  reload: () => void;
}

/**
 * Asks the core with `load` when `deps` change, every `everyMs` while the page shows (never,
 * when `null`), and on `reload`. A slower answer of an earlier ask is dropped. `enabled`
 * false asks nothing and keeps what there was.
 */
export function useAsked<T>(load: () => Promise<T>, deps: unknown[], { enabled = true, everyMs = null }: { enabled?: boolean; everyMs?: number | null } = {}): Asked<T> {
  const [state, setState] = useState<{ data: T | null; failure: CallFailure | null; loading: boolean }>({ data: null, failure: null, loading: enabled });
  const [tick, setTick] = useState(0);
  const latest = useRef(load);
  latest.current = load;
  useEffect(() => {
    if (!enabled) return;
    let current = true;
    const ask = () => {
      if (document.visibilityState === "hidden") return;
      setState((before) => ({ ...before, loading: true }));
      latest.current().then(
        (data) => { if (current) setState({ data, failure: null, loading: false }); },
        (error) => { if (current) setState((before) => ({ data: before.data, failure: callFailure(error), loading: false })); },
      );
    };
    ask();
    const timer = everyMs === null ? null : setInterval(ask, everyMs);
    return () => { current = false; if (timer !== null) clearInterval(timer); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, everyMs, tick, ...deps]);
  const reload = useCallback(() => setTick((n) => n + 1), []);
  return { ...state, reload };
}
