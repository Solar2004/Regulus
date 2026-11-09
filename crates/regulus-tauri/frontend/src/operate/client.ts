// The Operate view's calls, over any JSON-RPC line with `request` (the shared RpcClient
// in the app, a plain socket in the live test). Each is the same call a CLI command makes.
import type { Profile, ProfileSummary } from "@/composition/model";
import type { AgentTurn, ChannelMessage, EventPage, Execution, HumanAnswered, ModuleSummary, Participant, PendingDecision } from "./model";

export interface Rpc {
  request<T>(method: string, params?: Record<string, unknown>, timeout?: number): Promise<T>;
}

/** Method names: the core's (`regulus-proto`) and the modules' (`agent`, `communication`, `human`). */
export const methods = {
  profileList: "profile.list",
  profileGet: "profile.get",
  moduleList: "module.list",
  executionList: "execution.list",
  executionStart: "execution.start",
  executionStop: "execution.stop",
  executionTree: "execution.tree",
  eventList: "event.list",
  workflowSend: "workflow.send",
  agentPrompt: "agent.prompt",
  commHistory: "comm.history",
  commParticipants: "comm.participants",
  commSend: "comm.send",
  humanList: "human.list",
  humanGet: "human.get",
  humanAnswer: "human.answer",
} as const;

/** Who messages typed in the app are from, as in the TUI and `regulus msg send`. */
export const sender = "user";

/** At most this many pages per refresh, so one busy execution cannot stall the view. */
export const maxPagesPerRefresh = 10;

export function operateApi(rpc: Rpc) {
  return {
    /** Every profile in full (`regulus profile ls` + `regulus graph`). */
    async profiles(): Promise<Profile[]> {
      const summaries = await rpc.request<ProfileSummary[]>(methods.profileList);
      return Promise.all(summaries.map(({ id }) => rpc.request<Profile>(methods.profileGet, { id })));
    },
    modules: () => rpc.request<ModuleSummary[]>(methods.moduleList),
    /** `regulus ps`. */
    executions: (projectId: string | null) => rpc.request<Execution[]>(methods.executionList, projectId ? { project_id: projectId } : {}),
    /** `regulus run <profile> [--workflow]`. */
    start: (profileId: string, workflowId: string | undefined, timeout: number) =>
      rpc.request<Execution>(methods.executionStart, workflowId ? { profile_id: profileId, workflow_id: workflowId } : { profile_id: profileId }, timeout),
    /** `regulus exec tree <id>`: the execution and the ones it started, as the core answers. */
    tree: (id: string) => rpc.request<unknown>(methods.executionTree, { execution_id: id }),
    /** `regulus stop <id>`. */
    stop: (id: string) => rpc.request<Execution>(methods.executionStop, { id }),
    /** `regulus events --execution <id>`: one page after `since`. */
    events: (executionId: string, since: number, limit: number) => rpc.request<EventPage>(methods.eventList, { execution_id: executionId, since, limit }),
    /** Pages after `since` until a short one, at most `maxPagesPerRefresh`. */
    async eventsAfter(executionId: string, since: number, limit: number): Promise<EventPage> {
      let page = await this.events(executionId, since, limit);
      const events = [...page.events];
      for (let pages = 1; page.events.length >= limit && pages < maxPagesPerRefresh; pages++) {
        page = await this.events(executionId, page.next, limit);
        events.push(...page.events);
      }
      return { events, next: page.next, pending: page.pending };
    },
    /** `regulus send <execution> <port> <value>`. */
    send: (executionId: string, port: string, value: unknown) => rpc.request<{ event: number }>(methods.workflowSend, { execution_id: executionId, port, value }),
    /** `regulus agent prompt`: answers when the turn ends. */
    prompt: (executionId: string, nodeId: string, text: string, timeout: number) => rpc.request<AgentTurn>(methods.agentPrompt, { execution_id: executionId, node_id: nodeId, text }, timeout),
    /** `regulus msg history` and the participants of a channel node. */
    async channel(executionId: string, nodeId: string) {
      const params = { execution_id: executionId, node_id: nodeId };
      const [messages, participants] = await Promise.all([
        rpc.request<ChannelMessage[]>(methods.commHistory, params),
        rpc.request<Participant[]>(methods.commParticipants, params),
      ]);
      return { messages, participants };
    },
    /** `regulus msg send`, as `user`. */
    say: (executionId: string, nodeId: string, text: string) => rpc.request<unknown>(methods.commSend, { execution_id: executionId, node_id: nodeId, from: sender, text }),
    /** `regulus human ls [--execution]`. */
    decisions: (executionId?: string) => rpc.request<PendingDecision[]>(methods.humanList, executionId ? { execution_id: executionId } : {}),
    /** `regulus human show <id>`. */
    decision: (id: string) => rpc.request<PendingDecision>(methods.humanGet, { id }),
    /** `regulus human answer <id> approve|reject [--field] [--text]`. */
    answer: (id: string, decision: "approve" | "reject", fields: Record<string, string>, text: string | null) =>
      rpc.request<HumanAnswered>(methods.humanAnswer, { id, decision, fields, text }),
  };
}

export type OperateApi = ReturnType<typeof operateApi>;
