import { PackageOpen, TriangleAlert } from "lucide-react";
import { t } from "@/lib/i18n";
import type { Unavailable } from "./logic";
import { Mark } from "./StateMark";

/** A module method whose module is absent or unusable: a state, never a generic error. */
export function NotAvailable({ kind, reason }: { kind: Unavailable; reason?: string }) {
  return <div className="operate-note" role="status">
    {kind === "not-installed"
      ? <Mark icon={PackageOpen} tone="muted" label={t("operate.node.notInstalled")} />
      : <Mark icon={TriangleAlert} tone="danger" label={t("operate.unavailable")} />}
    <p>{t(kind === "not-installed" ? "operate.notInstalledDescription" : "operate.unavailableDescription")}</p>
    {reason && kind === "unavailable" && <code className="operate-reason">{reason}</code>}
  </div>;
}
