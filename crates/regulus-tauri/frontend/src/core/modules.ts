import { useEffect, useState } from "react";
import type { ModuleDefinition } from "@/composition/model";
import { useCore, type RpcClient } from "./connection";

/** The modules the connected core lists; `null` until it answers or while it is away. */
export function useModuleList(): ModuleDefinition[] | null {
  const { client, status } = useCore();
  const [listed, setListed] = useState<{ client: RpcClient; modules: ModuleDefinition[] } | null>(null);
  useEffect(() => {
    if (!client || status !== "ready") return;
    let disposed = false;
    client.request<ModuleDefinition[]>("module.list")
      .then((modules) => { if (!disposed) setListed({ client, modules }); })
      .catch(() => { if (!disposed) setListed({ client, modules: [] }); });
    return () => { disposed = true; };
  }, [client, status]);
  return client && status === "ready" && listed?.client === client ? listed.modules : null;
}

/** The sidebar line for `modules`: how many the core has, or that it has not said yet. */
export function moduleCountKey(modules: ModuleDefinition[] | null): "settings.modulesUnknown" | "settings.modulesEmpty" | "settings.modulesLoadedOne" | "settings.modulesLoaded" {
  if (modules === null) return "settings.modulesUnknown";
  if (modules.length === 0) return "settings.modulesEmpty";
  return modules.length === 1 ? "settings.modulesLoadedOne" : "settings.modulesLoaded";
}
