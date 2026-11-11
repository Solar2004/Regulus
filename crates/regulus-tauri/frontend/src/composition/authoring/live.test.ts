// Drives a real core (protocol 1.17) through the authoring calls the app makes, with the
// app's own client functions: create a Python node, break and fix its source, save over a
// stale revision, reload without restarting the core, read the new shape, sync a stale node
// and run real data through it. Skipped unless REGULUS_RPC_URL and REGULUS_LIVE_PROJECT (an
// empty folder holding only a regulus.toml, opened and trusted here, closed at the end) are set:
//   REGULUS_RPC_URL="ws://127.0.0.1:<port>/?token=<token>" REGULUS_LIVE_PROJECT=/var/tmp/x pnpm test authoring/live
import { afterAll, describe, expect, test } from "vitest";
import { RpcClient, RpcError } from "@/core/connection";
import type { Diagnostic, Manifest, Profile, ProjectSummary } from "../model";
import { addNode, addWire, addWorkflowPort, newProfile } from "../editor/document";
import { deleteProfile, getProfile, loadManifests, saveProfile, startExecution, checkProfile } from "../editor/api";
import { entryKey, findNodeType, nodeFromType } from "../editor/palette";
import { CODE, checkSource, conflictOf, getSource, initNode, moduleShape, projectSources, saveSource, syncProfile } from "./api";
import { applySync, samePorts } from "./logic";
import { reloadWithConsent } from "./reload";

const env = (globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env ?? {};
const url = env.REGULUS_RPC_URL;
const folder = env.REGULUS_LIVE_PROJECT;
const stamp = Date.now();
const timeouts = { reloadMs: 60000, checkMs: 20000 };

/** run.py with `caps` (in → out) plus, when asked, a `shout` output that only `mode = "loud"` has. */
const source = (extra: string) => `import regulus as rg


@rg.node(
    "caps",
    name="Caps",
    inputs=[rg.then_in("in", "text@1")],
    outputs=[rg.then_out("out", "text@1")${extra}],
    fields=[rg.field("prefix", "text", default=""), rg.field("mode", "select", default="quiet", options=["quiet", "loud"])],
)
def handle(ctx, port, value):
    ctx.emit("out", ctx.config["prefix"] + value.upper())


if __name__ == "__main__":
    rg.run(version="0.1.0")
`;
const loud = `, rg.then_out("shout", "text@1", when={"field": "mode", "equals": "loud"})`;

describe.skipIf(!url || !folder)("live core: authoring a Python node", () => {
  const rpc = url ? new RpcClient(url) : null!;
  let project: ProjectSummary | null = null;
  const profileId = `fe-live-${stamp}`;
  afterAll(async () => {
    if (!rpc) return;
    await deleteProfile(rpc, profileId).catch(() => {});
    if (project) await rpc.request("project.close", { id: project.id }).catch(() => {});
    rpc.close();
  });

  test("create → edit → error → fix → save → conflict → reload → shape → run → stale → sync", async () => {
    expect((await rpc.request<{ compatible: boolean; proto: string }>("hello", { client: "authoring-live-test", proto: "1.0" })).compatible).toBe(true);
    project = await rpc.request<ProjectSummary>("project.open", { path: folder, trust: true });

    // Create: the scaffolder writes tools/caps/run.py and starts it (trusted).
    const created = await initNode(rpc, project.id, "caps", timeouts);
    expect(created.module.outcome).not.toBe("failed");
    const sources = await projectSources(rpc, project.id);
    expect(sources.find((item) => item.module === "caps")?.path).toBe(created.source);
    // A second init of the same id never overwrites.
    await expect(initNode(rpc, project.id, "caps", timeouts)).rejects.toMatchObject({ code: CODE.sourceConflict });

    // Edit with a syntax error: the core checks without running and points at the line.
    const opened = await getSource(rpc, project.id, "caps");
    const broken = source("").replace("def handle(ctx, port, value):", "def handle(ctx, port, value)");
    const brokenCheck = await checkSource(rpc, project.id, "caps", broken, timeouts);
    const syntax = brokenCheck.diagnostics.find((d) => d.code === "python.syntax");
    expect(syntax?.severity).toBe("error");
    expect(syntax?.line).toBe(broken.split("\n").findIndex((line) => line.startsWith("def handle(ctx, port, value)")) + 1);

    // Fix and save over the revision that was read.
    const fixedCheck = await checkSource(rpc, project.id, "caps", source(""), timeouts);
    expect(fixedCheck.diagnostics.filter((d) => d.severity === "error")).toEqual([]);
    expect(fixedCheck.nodes.map((node) => node.type)).toEqual(["caps"]);
    const saved = await saveSource(rpc, { projectId: project.id, module: "caps", text: source(""), revision: opened.revision }, timeouts);
    expect(saved.revision).not.toBe(opened.revision);

    // Saving again over the old revision is a conflict that carries the current one.
    const stale = await saveSource(rpc, { projectId: project.id, module: "caps", text: source("# mine\n"), revision: opened.revision }, timeouts).catch((error: unknown) => error);
    expect(stale).toBeInstanceOf(RpcError);
    expect(conflictOf(stale)?.revision).toBe(saved.revision);

    // Reload without restarting the core: the new manifest is served.
    const reloaded = await reloadWithConsent(rpc, { id: project.id, module: "caps" }, timeouts, async () => false);
    expect(reloaded?.modules.find((item) => item.local_id === "caps")?.outcome).toMatch(/replaced|started/);
    const manifests: Manifest[] = await loadManifests(rpc);
    const caps = findNodeType(manifests, entryKey(manifests.find((item) => item.module.id.startsWith(`${project!.id}/caps@`))!.module.id, "caps"))!;
    expect(caps.nodeType.managed_ports).toBe(true);

    // A profile: self.ask → caps.in, caps.out → self.reply, built as the palette does (the core's shape).
    const shape = await moduleShape(rpc, caps.manifest.module.id, "caps", { mode: "quiet" });
    expect(shape.ports.map((port) => port.id)).toEqual(["in", "out"]);
    let profile: Profile = newProfile(profileId, "FE live", "Main");
    profile = addWorkflowPort(profile, "main", { id: "ask", direction: "input" }).profile;
    profile = addWorkflowPort(profile, "main", { id: "reply", direction: "output" }).profile;
    const node = nodeFromType(caps.manifest, caps.nodeType, { x: 0, y: 0 }, [], shape.ports);
    profile = addNode(profile, "main", { ...node, config: { prefix: "> ", mode: "quiet" } });
    profile = addWire(profile, "main", "then", { node_id: "self", port_id: "ask" }, { node_id: node.id, port_id: "in" });
    profile = addWire(profile, "main", "then", { node_id: node.id, port_id: "out" }, { node_id: "self", port_id: "reply" });
    await saveProfile(rpc, profile, true);

    // Real data through the custom node, as written by the scaffolder's caller.
    const run = await startExecution(rpc, profileId);
    await rpc.request("workflow.send", { execution_id: run.id, port: "ask", value: "hello" });
    let reply: unknown;
    for (let tries = 0; tries < 50 && reply === undefined; tries++) {
      const page = await rpc.request<{ events: { from: { node_id: string; port_id: string }; value?: unknown }[] }>("event.list", { execution_id: run.id, since: 0, limit: 200 });
      reply = page.events.find((event) => event.from.node_id === node.id && event.from.port_id === "out")?.value;
      if (reply === undefined) await new Promise((resolve) => setTimeout(resolve, 100));
    }
    expect(reply).toBe("> HELLO");
    await rpc.request("execution.stop", { id: run.id }).catch(() => {});

    // The source gains a `shout` port for mode = "loud"; after a reload, a loud node is stale.
    const before = await getSource(rpc, project.id, "caps");
    await saveSource(rpc, { projectId: project.id, module: "caps", text: source(loud), revision: before.revision }, timeouts);
    await reloadWithConsent(rpc, { id: project.id, module: "caps" }, timeouts, async () => false);
    const loudProfile = structuredClone(await getProfile(rpc, profileId));
    loudProfile.workflows[0].nodes[0].config.mode = "loud";
    const loudShape = await moduleShape(rpc, caps.manifest.module.id, "caps", { mode: "loud" });
    expect(loudShape.ports.map((port) => port.id)).toEqual(["in", "out", "shout"]);
    expect(samePorts(loudProfile.workflows[0].nodes[0].ports, loudShape.ports)).toBe(false);
    await saveProfile(rpc, loudProfile, false);
    const found: Diagnostic[] = await checkProfile(rpc, profileId);
    expect(found.some((d) => d.code === "node.ports_stale" && d.node_id === node.id)).toBe(true);

    // Sync is a preview; nothing is written until the client saves what it applied.
    const preview = await syncProfile(rpc, loudProfile);
    expect(preview.drops_wires).toBe(false);
    expect(preview.nodes[0].added.map((port) => port.id)).toEqual(["shout"]);
    expect((await getProfile(rpc, profileId)).workflows[0].nodes[0].ports.map((port) => port.id)).toEqual(["in", "out"]);
    const synced = applySync(loudProfile, preview);
    await saveProfile(rpc, synced, false);
    expect({ ports: synced.workflows[0].nodes[0].ports, stale: (await checkProfile(rpc, profileId)).filter((d) => d.code === "node.ports_stale") })
      .toEqual({ ports: loudShape.ports, stale: [] });

  }, 120000);
});
