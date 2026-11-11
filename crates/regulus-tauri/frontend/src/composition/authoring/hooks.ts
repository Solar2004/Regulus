import { useEffect, useState } from "react";
import type { Requester } from "../editor/api";
import type { ProjectSource } from "../model";
import { projectSources } from "./api";
import { useAuthoring } from "./store";

/**
 * The editable files of one project's local modules (`project.get` → `sources`), read again
 * after modules change. `undefined` while unknown; empty from a core that does not list them.
 */
export function useProjectSources(rpc: Requester | null | undefined, projectId: string | null): ProjectSource[] | undefined {
  const rev = useAuthoring((s) => s.modulesRev);
  const key = `${projectId}\u0000${rev}`;
  const [read, setRead] = useState<{ key: string; sources: ProjectSource[] } | null>(null);
  useEffect(() => {
    if (!rpc || !projectId) return;
    let disposed = false;
    projectSources(rpc, projectId).then(
      (sources) => { if (!disposed) setRead({ key, sources }); },
      () => { if (!disposed) setRead({ key, sources: [] }); });
    return () => { disposed = true; };
  }, [rpc, projectId, rev]);
  return read?.key === key ? read.sources : undefined;
}
