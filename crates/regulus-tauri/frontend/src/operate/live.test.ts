// The Operate and Templates calls against a real core. Skipped unless REGULUS_RPC_URL is
// set (ws://127.0.0.1:<port>/?token=<token>). Ralph needs REGULUS_RALPH_DIR (a workdir
// with PROMPT.md) and REGULUS_RALPH_CHECK (a shell line that passes when the work is
// done); the agent chat needs REGULUS_AGENT_PROFILE (a loaded profile like
// crates/modules/agent/examples/then.toml). Agents run with opencode's free model.
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { RpcClient } from "@/core/connection";
import { templatesApi } from "@/templates/client";
import { operateApi } from "./client";
import { agentAnswer, mergeEvents, portName, renderValue } from "./logic";
import type { BusEvent, Execution } from "./model";

// Node globals without @types/node in the app build.
const env = (globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env ?? {};
const url = env.REGULUS_RPC_URL;
const minute = 60_000;
let client: RpcClient;
const api = () => operateApi(client);

beforeAll(async () => {
  if (!url) return;
  client = new RpcClient(url);
  const hello = await client.request<{ compatible: boolean }>("hello", { client: "regulus-gui-live-test", proto: "1.0" });
  expect(hello.compatible).toBe(true);
});
afterAll(() => client?.close());

const line = (event: BusEvent) => {
  const head = `#${event.seq} ${event.execution_id} ${portName(event.from)}`;
  switch (event.kind) {
    case "emitted": return `${head} emitted ${event.capability} ${renderValue(event.value, 120).short}`;
    case "delivered": return `${head} → ${portName(event.to)} delivered`;
    case "failed": return `${head} → ${portName(event.to)} failed: ${event.error}`;
    case "cut": return `${head} cut at depth ${event.max_depth}`;
  }
};

/** Polls `event.list` as the view does until `done` holds or `ms` pass. */
async function follow(execution: Execution, done: (events: BusEvent[]) => boolean, ms: number) {
  let events: BusEvent[] = [];
  let since = 0;
  const until = Date.now() + ms;
  while (Date.now() < until) {
    const page = await api().eventsAfter(execution.id, since, 200);
    events = mergeEvents(events, page.events, 0);
    since = page.next;
    if (done(events)) break;
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  for (const event of events) console.log(line(event));
  return events;
}
const emittedFrom = (events: BusEvent[], from: string) => events.some((event) => event.kind === "emitted" && portName(event.from) === from);

describe.skipIf(!url)("live core", () => {
  test("apply greeting with who=Ana, run it, and see the note written", async () => {
    const templates = await templatesApi(client).list();
    expect(templates.some((template) => template.name === "greeting")).toBe(true);
    const applied = await templatesApi(client).apply({ template: "greeting", vars: { who: "Ana" }, force: true });
    console.log("applied", applied);
    const execution = await api().start(applied.profile_id, undefined, 2 * minute);
    expect(execution.profile_id).toBe("greeting");
    const events = await follow(execution, (events) => emittedFrom(events, "note.written"), minute);
    expect(emittedFrom(events, "note.written")).toBe(true);
    const stopped = await api().stop(execution.id);
    expect(stopped.state.status).toBe("stopped");
  }, 2 * minute);

  test.skipIf(!env.REGULUS_RALPH_DIR)("apply ralph, send go, and follow it to self.done or self.exhausted", async () => {
    const applied = await templatesApi(client).apply({
      template: "ralph", force: true,
      vars: { workdir: env.REGULUS_RALPH_DIR!, max_turns: "2", test_command: env.REGULUS_RALPH_CHECK ?? "true" },
    });
    console.log("applied", applied);
    const execution = await api().start(applied.profile_id, undefined, 2 * minute);
    const profile = (await api().profiles()).find((item) => item.id === applied.profile_id)!;
    const inputs = profile.workflows.find((workflow) => workflow.id === execution.workflow_id)?.ports?.filter((port) => port.direction === "input");
    expect(inputs?.map((port) => port.id)).toEqual(["go"]);
    const sent = await api().send(execution.id, "go", "none yet: no check has run");
    console.log("sent event", sent.event);
    const events = await follow(execution, (events) => emittedFrom(events, "self.done") || emittedFrom(events, "self.exhausted"), 10 * minute);
    await api().stop(execution.id);
    expect(emittedFrom(events, "self.done") || emittedFrom(events, "self.exhausted")).toBe(true);
  }, 11 * minute);

  test.skipIf(!env.REGULUS_AGENT_PROFILE)("prompt an agent node and get pong", async () => {
    const execution = await api().start(env.REGULUS_AGENT_PROFILE!, undefined, 2 * minute);
    try {
      const turn = await api().prompt(execution.id, "agent", "Reply with exactly: pong", 5 * minute);
      console.log("agent.prompt →", turn);
      expect(agentAnswer(turn).text.trim().toLowerCase()).toContain("pong");
    } finally {
      await api().stop(execution.id);
    }
  }, 6 * minute);

  test("a module method nobody serves is reported by code, not as a generic error", async () => {
    await expect(client.request("nomodule.method")).rejects.toMatchObject({ code: -32601 });
  });
});
