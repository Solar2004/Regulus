import { create } from "zustand";
import { mergeRecords, stillPending, type ConversationRecord, type PendingSend } from "./conversation";
import type { AgentSession } from "./model";

/**
 * What a conversation showed last, so switching views or nodes draws it at once. Only a
 * cache: the core's record (`agent.history`, `event.list`) is the truth, and each time a
 * conversation opens it is read again from the start and rebuilt. Not persisted.
 */
export interface CachedConversation {
  records: ConversationRecord[];
  /** The cursor after the last page read. */
  next: number;
  session: AgentSession | null;
  /** Turns sent from this app and not in the record yet. */
  pending: PendingSend[];
}

const empty: CachedConversation = { records: [], next: 0, session: null, pending: [] };

interface ConversationCacheState {
  byKey: Record<string, CachedConversation>;
  /** A page read: `rebuild` replaces what there was (a read from the start), else it merges. */
  page: (key: string, page: { records: ConversationRecord[]; next: number; session?: AgentSession | null }, rebuild: boolean, keep: number) => void;
  addPending: (key: string, send: PendingSend) => void;
  dropPending: (key: string, send: PendingSend) => void;
}

export const useConversationCache = create<ConversationCacheState>()((set) => ({
  byKey: {},
  page: (key, page, rebuild, keep) => set((state) => {
    const before = state.byKey[key] ?? empty;
    const records = rebuild ? mergeRecords([], page.records, keep) : mergeRecords(before.records, page.records, keep);
    // A node that stopped takes no more turns: what was waiting will never come.
    const stopped = page.session?.state === "stopped";
    const pending = stopped ? [] : stillPending(before.pending, records, page.next);
    const session = page.session === undefined ? before.session : page.session;
    return { byKey: { ...state.byKey, [key]: { records, next: page.next, session, pending } } };
  }),
  addPending: (key, send) => set((state) => {
    const before = state.byKey[key] ?? empty;
    return { byKey: { ...state.byKey, [key]: { ...before, pending: [...before.pending, send] } } };
  }),
  dropPending: (key, send) => set((state) => {
    const before = state.byKey[key] ?? empty;
    return { byKey: { ...state.byKey, [key]: { ...before, pending: before.pending.filter((item) => item !== send) } } };
  }),
}));

export const cachedConversation = (state: ConversationCacheState, key: string): CachedConversation => state.byKey[key] ?? empty;

/** Where a direct agent's conversation is cached: any view that draws from it uses the same key. */
export const agentConversationKey = (executionId: string, nodeId: string) => `agent:${executionId}/${nodeId}`;
