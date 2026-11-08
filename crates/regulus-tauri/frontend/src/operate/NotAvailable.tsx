import { PackageOpen, RotateCcw, TriangleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import type { Unavailable } from "./logic";
import { Mark } from "./StateMark";

/** A module method whose module is absent or unusable: a state, never a generic error. */
/** `description` replaces the Operate wording where the method is not about an execution. */
export function NotAvailable({ kind, reason, description, onRetry }: { kind: Unavailable; reason?: string; description?: string; onRetry?: () => void }) {
  return <div className="operate-note" role="status">
    {kind === "not-installed"
      ? <Mark icon={PackageOpen} tone="muted" label={t("operate.node.notInstalled")} />
      : <Mark icon={TriangleAlert} tone="danger" label={t("operate.unavailable")} />}
    <p>{description ?? t(kind === "not-installed" ? "operate.notInstalledDescription" : "operate.unavailableDescription")}</p>
    {reason && <code className="operate-reason">{reason}</code>}
    {onRetry && <Button variant="outline" size="sm" onClick={onRetry}><RotateCcw size={14} />{t("operate.retry")}</Button>}
  </div>;
}
