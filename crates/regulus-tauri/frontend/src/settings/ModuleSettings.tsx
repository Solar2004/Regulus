import { CircleCheck, CircleDashed, OctagonX, PackageOpen, ShieldCheck, ShieldOff, TriangleAlert, type LucideIcon } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import type { ProjectSummary } from "@/composition/model";
import { useCore } from "@/core/connection";
import { moduleCountKey, useModuleList } from "@/core/modules";
import { t, type LocaleKey } from "@/lib/i18n";
import { Mark } from "@/operate/StateMark";
import { formatTime, moduleRows, statusCounts, trustTarget, type ModuleStatus } from "./logic";
import { useSettingsApi } from "./useSettingsApi";

type Tone = "success" | "warning" | "danger" | "muted";

// docs/08-design-system.md: active, idle never used, disabled, broken and blocked as the
// table says; idle after use is not in it and stays quiet, incompatible reads as broken.
const meta: Record<ModuleStatus["kind"], { icon: LucideIcon; tone: Tone; label: LocaleKey }> = {
  active: { icon: CircleCheck, tone: "success", label: "settings.module.active" },
  unused: { icon: CircleDashed, tone: "warning", label: "settings.module.unused" },
  idle: { icon: CircleDashed, tone: "muted", label: "settings.module.idle" },
  disabled: { icon: OctagonX, tone: "danger", label: "settings.module.disabled" },
  broken: { icon: TriangleAlert, tone: "danger", label: "settings.module.broken" },
  blocked: { icon: ShieldOff, tone: "warning", label: "settings.module.blocked" },
  incompatible: { icon: TriangleAlert, tone: "danger", label: "settings.module.incompatible" },
};

/** Why a module is in its state, in words. */
function reasonOf(status: ModuleStatus): string {
  switch (status.kind) {
    case "active": return t("settings.module.since", { when: formatTime(status.since) });
    case "unused": return t("settings.module.neverUsed");
    case "idle": return t("settings.module.lastUsed", { when: formatTime(status.lastUsed) });
    case "disabled": return t("settings.module.missing", { inputs: status.missing.join(", ") });
    case "broken": return status.reason;
    case "blocked": return status.reason;
    case "incompatible": return t("settings.module.needsProtocol", { requires: status.requires, have: status.have });
  }
}

const countKeys: Record<ModuleStatus["kind"], LocaleKey> = {
  active: "settings.module.count.active",
  unused: "settings.module.count.unused",
  idle: "settings.module.count.idle",
  disabled: "settings.module.count.disabled",
  broken: "settings.module.count.broken",
  blocked: "settings.module.count.blocked",
  incompatible: "settings.module.count.incompatible",
};

const problem = (status: ModuleStatus) => status.kind === "broken" || status.kind === "incompatible" || status.kind === "disabled";

/** The blocked module's project and the button that trusts it (`regulus project trust`). */
function TrustAction({ status, projects, onTrusted }: { status: ModuleStatus; projects: ProjectSummary[] | null; onTrusted: () => void }) {
  const api = useSettingsApi();
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const target = trustTarget(status, projects);
  if (!target || !api) return <p className="settings-module-why">{t("settings.module.trustCli")}</p>;
  const trust = async () => {
    setBusy(true);
    setFailure(null);
    try {
      await api.trustProject(target.path);
      onTrusted();
    } catch (error) {
      setFailure(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };
  return <div className="settings-module-action">
    <Button variant="outline" size="sm" disabled={busy} onClick={() => void trust()} aria-label={t("settings.module.trust", { project: target.name || target.id })}>
      <ShieldCheck size={14} />{t(busy ? "settings.module.trusting" : "settings.module.trust", { project: target.name || target.id })}
    </Button>
    <code>{target.path}</code>
    {failure && <p className="settings-module-why tone-danger" role="alert">{t("settings.module.trustFailed", { error: failure })}</p>}
  </div>;
}

export function ModuleSettings() {
  const [epoch, setEpoch] = useState(0);
  const modules = useModuleList(epoch);
  const { status } = useCore();
  const api = useSettingsApi();
  const rows = modules ? moduleRows(modules) : [];
  const counts = statusCounts(rows);
  const anyBlocked = rows.some((row) => row.status.kind === "blocked");
  // The projects are asked for only when a blocked module needs its folder.
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  useEffect(() => {
    if (!api || !anyBlocked) return;
    let disposed = false;
    api.projects()
      .then((listed) => { if (!disposed) setProjects(listed); })
      .catch(() => { if (!disposed) setProjects([]); });
    return () => { disposed = true; };
  }, [api, anyBlocked, epoch]);
  return <>
    <div className="settings-row"><div><h2>{t("settings.moduleSpace")}</h2><p>{t("settings.moduleSpaceDescription")}</p></div></div>
    {rows.length
      ? <>
        <p className="settings-counts">{[
          t(moduleCountKey(modules), { count: rows.length }),
          ...(Object.keys(countKeys) as ModuleStatus["kind"][]).filter((kind) => counts[kind]).map((kind) => t(countKeys[kind], { count: counts[kind]! })),
        ].join(t("settings.listSeparator"))}</p>
        <ul className="settings-modules" aria-label={t("settings.modules")}>{rows.map((row) => {
          const look = meta[row.status.kind];
          return <li key={row.id} data-state={row.status.kind}>
            <div className="settings-module-head">
              <span className="settings-module-name">{row.name}</span>
              <code>{row.id}</code>
              <Mark icon={look.icon} tone={look.tone} label={t(look.label)} />
            </div>
            <p className={problem(row.status) ? "settings-module-why tone-danger" : "settings-module-why"}>{reasonOf(row.status)}</p>
            {row.status.kind === "broken" && <p className="settings-module-why">{t("settings.module.reload")}</p>}
            {row.status.kind === "blocked" && <>
              <p className="settings-module-why">{t("settings.module.blockedWhy")}</p>
              <TrustAction status={row.status} projects={projects} onTrusted={() => setEpoch((n) => n + 1)} />
            </>}
            {row.nodeTypes.length > 0 && <small>{t("settings.module.nodes", { types: row.nodeTypes.join(", ") })}</small>}
          </li>;
        })}</ul>
      </>
      : modules === null && status !== "offline" && status !== "incompatible"
        ? <p className="settings-empty" role="status">{t("settings.modulesLoading")}</p>
        : <div className="module-empty"><PackageOpen size={36} strokeWidth={1.25} /><h3>{t(moduleCountKey(modules))}</h3><p>{t("settings.modulesDescription")}</p></div>}
    <div className="layers"><h3>{t("settings.layers")}</h3><p>{t("settings.layersDescription")}</p></div>
  </>;
}
