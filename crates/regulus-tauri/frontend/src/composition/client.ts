import { useEffect, useState } from "react";
import { useCore, type CoreStatus, type RpcClient } from "@/core/connection";
import type { ModuleDefinition, Profile, ProfileSummary } from "./model";

type Loaded = { client: RpcClient; profiles: Profile[]; modules: ModuleDefinition[]; failed: boolean };

/** Profiles and modules over the shared connection. */
export function useComposition() {
  const { status, client, reason, connect } = useCore();
  const [loaded, setLoaded] = useState<Loaded | null>(null);

  useEffect(() => {
    if (!client) return;
    let disposed = false;
    async function load(client: RpcClient) {
      try {
        const [summaries, modules] = await Promise.all([
          client.request<ProfileSummary[]>("profile.list"),
          client.request<ModuleDefinition[]>("module.list"),
        ]);
        const profiles = await Promise.all(summaries.map(({ id }) => client.request<Profile>("profile.get", { id })));
        if (!disposed) setLoaded({ client, profiles, modules, failed: false });
      } catch {
        if (!disposed) setLoaded({ client, profiles: [], modules: [], failed: true });
      }
    }
    void load(client);
    return () => { disposed = true; };
  }, [client]);

  const current = loaded && loaded.client === client ? loaded : null;
  const shown: CoreStatus = status !== "ready" ? status : !current ? "loading" : current.failed ? "offline" : "ready";
  return { status: shown, reason, profiles: current?.profiles ?? [], modules: current?.modules ?? [], retry: connect };
}
