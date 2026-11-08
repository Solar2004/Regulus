import { useEffect, useMemo } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { Download, RotateCcw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import { updateApi, updateView, useUpdates } from "./update";

/** Settings → General → Updates: the running version, the channel's, and installing it. */
export function UpdateSettings() {
  const inShell = isTauri();
  const api = useMemo(() => updateApi(invoke), []);
  const state = useUpdates();
  const { load } = state;
  useEffect(() => { if (inShell) void load(api); }, [inShell, load, api]);
  const view = updateView(inShell, state);
  const checking = view.kind === "checking" || view.kind === "installing";
  const canCheck = view.kind === "unchecked" || view.kind === "upToDate" || view.kind === "available" || view.kind === "failed" || view.kind === "checking";

  let status: string;
  switch (view.kind) {
    case "browser": status = t("settings.update.browser"); break;
    case "loading": status = t("graph.loading"); break;
    case "off": status = t("settings.update.off", { path: view.configPath }); break;
    case "noKey": status = t("settings.update.noKey", { path: view.configPath }); break;
    case "unchecked": status = t("settings.update.unchecked", { channel: view.channel }); break;
    case "checking": status = t("settings.update.checking", { channel: view.channel }); break;
    case "upToDate": status = t("settings.update.upToDate", { channel: view.channel }); break;
    case "available": status = t("settings.update.available", { version: view.version, channel: view.channel }); break;
    case "installing": status = t("settings.update.installing", { version: view.version }); break;
    case "failed": status = t("settings.update.failed"); break;
  }

  return <div className="settings-row">
    <div>
      <h2>{t("settings.update.title")}</h2>
      <p>{"current" in view ? t("settings.update.current", { version: view.current }) : t("settings.update.description")}</p>
      <p className={view.kind === "failed" ? "settings-problem" : "settings-hint"} role={view.kind === "failed" ? "alert" : "status"}>{status}</p>
      {view.kind === "failed" && <code className="operate-reason">{view.reason}</code>}
      {view.kind === "available" && view.notes && <p className="settings-hint">{view.notes}</p>}
    </div>
    {canCheck && <div className="settings-actions">
      <Button variant="outline" size="sm" disabled={checking} onClick={() => void state.check(api)}><RotateCcw size={14} />{t("settings.update.check")}</Button>
      {view.kind === "available" && <Button size="sm" onClick={() => void state.install(api)}><Download size={14} />{t("settings.update.install", { version: view.version })}</Button>}
    </div>}
  </div>;
}
