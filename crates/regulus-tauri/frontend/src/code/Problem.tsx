import { RotateCcw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import { NotAvailable } from "@/operate/NotAvailable";
import type { CallFailure } from "./client";

/**
 * Why a call of the view gave nothing. A module that is absent or unusable is a state of
 * its own (docs/04-client.md §3); anything else is the core's own words, never a made-up one.
 */
export function Problem({ failure, onRetry }: { failure: CallFailure; onRetry?: () => void }) {
  if (failure.kind === "not-installed" || failure.kind === "unavailable") {
    return <NotAvailable kind={failure.kind} reason={failure.reason} onRetry={onRetry}
      description={t(failure.kind === "not-installed" ? "code.problem.notInstalled" : "code.problem.unavailable")} />;
  }
  return <div className="operate-error" role="alert">
    <strong>{t(failure.kind === "unreadable" ? "code.problem.unreadable" : "code.problem.refused")}</strong>
    {failure.kind === "refused" && <code className="operate-reason">{failure.reason}</code>}
    {onRetry && <Button variant="outline" size="sm" onClick={onRetry}><RotateCcw size={14} />{t("operate.retry")}</Button>}
  </div>;
}
