import { create } from "zustand";

/** One line of an agent conversation in this UI session: the core does not keep turns yet. */
export type Turn =
  | { role: "prompt"; text: string }
  | { role: "answer"; text: string; stopReason: string | null }
  | { role: "error"; text: string };

const keyOf = (executionId: string, nodeId: string) => `${executionId}/${nodeId}`;

interface SessionState {
  turns: Record<string, Turn[]>;
  thinking: Record<string, boolean>;
  add: (executionId: string, nodeId: string, turn: Turn) => void;
  setThinking: (executionId: string, nodeId: string, thinking: boolean) => void;
}

/** Conversations survive switching views, not a reload (docs/04-client.md §7). */
export const useOperateSession = create<SessionState>()((set) => ({
  turns: {},
  thinking: {},
  add: (executionId, nodeId, turn) => set((state) => {
    const key = keyOf(executionId, nodeId);
    return { turns: { ...state.turns, [key]: [...(state.turns[key] ?? []), turn] } };
  }),
  setThinking: (executionId, nodeId, thinking) => set((state) => ({ thinking: { ...state.thinking, [keyOf(executionId, nodeId)]: thinking } })),
}));

export const turnsOf = (state: SessionState, executionId: string, nodeId: string) => state.turns[keyOf(executionId, nodeId)];
export const isThinking = (state: SessionState, executionId: string, nodeId: string) => state.thinking[keyOf(executionId, nodeId)] ?? false;
