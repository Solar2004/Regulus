import { useCallback, useEffect, useState } from "react";
import { useCore, type CoreStatus, type RpcClient } from "@/core/connection";
import type { Manifest, ModuleDefinition, Profile, ProfileSummary } from "./model";
import { loadManifests } from "./editor/api";

type Loaded = { client: RpcClient; profiles: Profile[]; modules: ModuleDefinition[]; manifests: Manifest[]; failed: boolean };

/** Profiles, modules and their manifests over the shared connection; `reload` reads them again. */
export function useComposition() {
  const { status, client, reason, connect } = useCore();
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [version, setVersion] = useState(0);

  useEffect(() => {
    if (!client) return;
    let disposed = false;
    async function load(client: RpcClient) {
      try {
        const [summaries, modules, manifests] = await Promise.all([
          client.request<ProfileSummary[]>("profile.list"),
          client.request<ModuleDefinition[]>("module.list"),
          // A core without `module.manifest` (protocol < 1.11) still shows its profiles.
          loadManifests(client).catch(() => [] as Manifest[]),
        ]);
        const profiles = await Promise.all(summaries.map(({ id }) => client.request<Profile>("profile.get", { id })));
        if (!disposed) setLoaded({ client, profiles, modules, manifests, failed: false });
      } catch {
        if (!disposed) setLoaded({ client, profiles: [], modules: [], manifests: [], failed: true });
      }
    }
    void load(client);
    return () => { disposed = true; };
  }, [client, version]);

  const reload = useCallback(() => setVersion((value) => value + 1), []);
  const current = loaded && loaded.client === client ? loaded : null;
  const shown: CoreStatus = status !== "ready" ? status : !current ? "loading" : current.failed ? "offline" : "ready";
  return {
    status: shown, reason, client: current ? client : null, profiles: current?.profiles ?? [], modules: current?.modules ?? [],
    manifests: current?.manifests ?? [], retry: connect, reload,
  };
}
