// Drives a real core with the calls the canvas makes, built with the canvas's own pure
// functions. Skipped unless REGULUS_RPC_URL is set, e.g.
//   REGULUS_RPC_URL="ws://127.0.0.1:<port>/?token=<token>" pnpm test live
import { afterAll, describe, expect, test } from "vitest";
import { RpcClient } from "@/core/connection";
import type { Manifest } from "../model";
import { canConnect } from "./connect";
import { addNode, addWire, newProfile, setConfig, workflowOf } from "./document";
import { checkProfile, deleteProfile, getProfile, loadManifests, saveProfile, startExecution } from "./api";
import { entryKey, findNodeType, nodeFromType } from "./palette";

const url = process.env.REGULUS_RPC_URL;
const id = `canvas-live-${Date.now()}`;

type Page = { events: { kind: string; from: { node_id: string; port_id: string }; to?: { node_id: string; port_id: string }; value?: unknown }[]; pending: number };

describe.skipIf(!url)("live core", () => {
  const rpc = url ? new RpcClient(url) : null!;
  afterAll(() => rpc?.close());

  test("create, wire, save, check, run, warn and delete a profile", async () => {
    expect((await rpc.request<{ compatible: boolean }>("hello", { client: "canvas-live-test", proto: "1.0" })).compatible).toBe(true);
    const manifests: Manifest[] = await loadManifests(rpc);
    const typeOf = (module: string, type: string) => {
      const manifest = manifests.find((item) => item.module.id.startsWith(`${module}@`))!;
      return findNodeType(manifests, entryKey(manifest.module.id, type))!;
    };
    const greeter = typeOf("regulus/hello", "greeter");
    const write = typeOf("regulus/files", "write");

    // New profile, greeter → write over "then", as the palette and a drag would do it.
    let profile = newProfile(id, "Canvas live", "Main");
    const hello = nodeFromType(greeter.manifest, greeter.nodeType, { x: 0, y: 0 }, []);
    const writer = nodeFromType(write.manifest, write.nodeType, { x: 320, y: 0 }, [hello.id]);
    profile = addNode(addNode(profile, "main", hello), "main", writer);
    profile = setConfig(profile, "main", writer.id, "path", `${id}.txt`);
    const ok = canConnect(profile, "main", { node_id: hello.id, port_id: "tick" }, { node_id: writer.id, port_id: "in" });
    expect(ok).toEqual({ ok: true, kind: "then" });
    profile = addWire(profile, "main", "then", { node_id: hello.id, port_id: "tick" }, { node_id: writer.id, port_id: "in" });
    // A mismatch the canvas refuses before it reaches the core.
    expect(canConnect(profile, "main", { node_id: writer.id, port_id: "written" }, { node_id: hello.id, port_id: "echo" })).toMatchObject({ ok: false, reason: "capability" });

    const saved = await saveProfile(rpc, profile, true);
    expect(saved.profile_id).toBe(id);
    expect(saved.diagnostics.filter((d) => d.severity === "error")).toEqual([]);
    console.log("saved", saved.path, JSON.stringify(saved.diagnostics));
    // Saving it again as new is refused; the canvas sends force for a loaded profile.
    await expect(saveProfile(rpc, profile, true)).rejects.toThrow(/already/);
    const loaded = await getProfile(rpc, id);
    expect(workflowOf(loaded, "main")!.nodes.map((node) => [node.id, node.position])).toEqual([[hello.id, { x: 0, y: 0 }], [writer.id, { x: 320, y: 0 }]]);

    const checked = await checkProfile(rpc, id);
    expect(checked.filter((d) => d.severity === "error")).toEqual([]);

    const execution = await startExecution(rpc, id, loaded.main_workflow_id);
    expect(execution).toMatchObject({ profile_id: id, workflow_id: "main" });
    console.log("started", JSON.stringify(execution));
    let page: Page = { events: [], pending: 1 };
    for (let tries = 0; tries < 50; tries++) {
      page = await rpc.request<Page>("event.list", { execution_id: execution.id });
      if (page.events.some((event) => event.kind === "delivered") && page.pending === 0) break;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    console.log("events", JSON.stringify(page.events.map((event) => [event.kind, `${event.from.node_id}.${event.from.port_id}`, event.to && `${event.to.node_id}.${event.to.port_id}`, event.value])));
    expect(page.events).toContainEqual(expect.objectContaining({ kind: "delivered", from: { node_id: hello.id, port_id: "tick" }, to: { node_id: writer.id, port_id: "in" } }));

    // While it runs, the profile cannot be deleted: the core says why.
    await expect(deleteProfile(rpc, id)).rejects.toThrow(/in use|stop/);
    await rpc.request("execution.stop", { id: execution.id });

    // An agent without its required "uses" engine: saved, with the warning.
    const agentManifest = manifests.find((item) => item.module.id.startsWith("regulus/agent@"))!;
    const agentType = agentManifest.node!.find((node) => node.port?.some((port) => port.capability === "agent.engine@1" && port.direction === "input"))!;
    const agent = nodeFromType(agentManifest, agentType, { x: 0, y: 240 }, [hello.id, writer.id]);
    const warned = await saveProfile(rpc, addNode(loaded, "main", agent), false);
    console.log("warned", JSON.stringify(warned.diagnostics));
    expect(warned.diagnostics).toContainEqual(expect.objectContaining({ severity: "warning", code: "port.required_unwired", node_id: agent.id }));

    // A wire the canvas would refuse, forced into the document, is refused by the core with its message.
    const bad = addWire(loaded, "main", "then", { node_id: writer.id, port_id: "written" }, { node_id: hello.id, port_id: "echo" });
    await expect(saveProfile(rpc, bad, false)).rejects.toThrow(/has errors: .*wire\.capability_mismatch/);

    const deleted = await deleteProfile(rpc, id);
    expect(deleted.profile_id).toBe(id);
    await expect(getProfile(rpc, id)).rejects.toThrow();
  }, 30000);
});
