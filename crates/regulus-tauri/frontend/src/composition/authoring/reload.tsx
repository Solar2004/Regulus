import { useCallback, useRef, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { t } from "@/lib/i18n";
import type { Requester } from "../editor/api";
import type { ModuleInUse, ProjectReloaded, ReloadedModule } from "../model";
import { inUseOf, reloadProject, type AuthoringTimeouts } from "./api";
import "./authoring.css";

/**
 * `project.reload`, asking before anything is stopped: when live runs use the module the
 * core refuses (`-32014`) and changes nothing; only if `askStop` says yes does the second
 * call stop them. `null` when the person kept the runs.
 */
export async function reloadWithConsent(rpc: Requester, params: { id: string; module?: string }, timeouts: AuthoringTimeouts,
  askStop: (inUse: ModuleInUse) => Promise<boolean>): Promise<ProjectReloaded | null> {
  try {
    return await reloadProject(rpc, params, timeouts);
  } catch (error) {
    const inUse = inUseOf(error);
    if (!inUse) throw error;
    if (!await askStop(inUse)) return null;
    return await reloadProject(rpc, { ...params, stopExecutions: true }, timeouts);
  }
}

/** A dialog that lists the runs a reload would stop, and a function that opens it and waits for the answer. */
export function useStopConsent(): [(inUse: ModuleInUse) => Promise<boolean>, ReactNode] {
  const [asking, setAsking] = useState<ModuleInUse | null>(null);
  const answer = useRef<((stop: boolean) => void) | null>(null);
  const ask = useCallback((inUse: ModuleInUse) => new Promise<boolean>((resolve) => {
    answer.current?.(false);
    answer.current = resolve;
    setAsking(inUse);
  }), []);
  const close = (stop: boolean) => { answer.current?.(stop); answer.current = null; setAsking(null); };
  const dialog = <Dialog open={!!asking} onOpenChange={(open) => { if (!open) close(false); }}>
    {asking && <DialogContent>
      <DialogHeader>
        <DialogTitle>{t("authoring.inUse.title")}</DialogTitle>
        <DialogDescription>{t("authoring.inUse.description", { count: asking.executions.length })}</DialogDescription>
      </DialogHeader>
      <ul className="authoring-list">{asking.executions.map((run) => <li key={run.id}>
        <code>{run.id}</code><span>{run.profile_id} · {run.workflow_id}</span>{run.nodes.length > 0 && <small>{t("authoring.inUse.nodes", { nodes: run.nodes.join(", ") })}</small>}
      </li>)}</ul>
      <DialogFooter>
        <Button variant="outline" onClick={() => close(false)}>{t("authoring.inUse.keep")}</Button>
        <Button variant="destructive" onClick={() => close(true)}>{t("authoring.inUse.stop", { count: asking.executions.length })}</Button>
      </DialogFooter>
    </DialogContent>}
  </Dialog>;
  return [ask, dialog];
}

/** One module's reload outcome in words. */
export function outcomeText(item: ReloadedModule): string {
  const id = item.id ?? item.local_id;
  switch (item.outcome) {
    case "replaced": return t("authoring.outcome.replaced", { id });
    case "started": return t("authoring.outcome.started", { id });
    case "kept": return t("authoring.outcome.kept", { id, error: item.error ?? "" });
    case "failed": return t("authoring.outcome.failed", { id, error: item.error ?? "" });
    case "blocked": return t("authoring.outcome.blocked", { id });
    case "removed": return t("authoring.outcome.removed", { id });
    case "skipped": return t("authoring.outcome.skipped", { id, error: item.error ?? "" });
  }
}
