import { expect, test } from "vitest";
import { methods, operateApi, type Rpc } from "./client";

test("historyAfter pages agent.history from the cursor until a short page, entries and activity together", async () => {
  const calls: Record<string, unknown>[] = [];
  const pages = [
    { entries: [{ seq: 1 }], activity: [{ seq: 2 }], next: 2, session: { native_resume: false, state: "busy" } },
    { entries: [], activity: [{ seq: 3 }], next: 3, session: { native_resume: false, state: "idle" } },
  ];
  const rpc: Rpc = { request: async <T,>(method: string, params?: Record<string, unknown>) => { expect(method).toBe(methods.agentHistory); calls.push(params!); return pages.shift() as T; } };
  const page = await operateApi(rpc).historyAfter("e", "agent", 0, 2);
  expect(calls).toEqual([{ execution_id: "e", node_id: "agent", since: 0, limit: 2 }, { execution_id: "e", node_id: "agent", since: 2, limit: 2 }]);
  expect(page.entries.length + page.activity.length).toBe(3);
  expect(page.next).toBe(3);
  expect(page.session.state).toBe("idle");
});
