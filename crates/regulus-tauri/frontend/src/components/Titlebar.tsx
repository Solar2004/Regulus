import { useCallback, useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Copy, Minus, Square, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import { t } from "@/lib/i18n";
import { unsavedWork } from "@/lib/unsaved";

function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function ControlButton({
  label,
  onClick,
  danger,
  children,
}: {
  label: string;
  onClick: () => void;
  danger?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      onClick={onClick}
      onDoubleClick={(e) => e.stopPropagation()}
      className={cn(
        "flex h-9 w-11 items-center justify-center text-muted-foreground transition-colors",
        danger
          ? "hover:bg-danger-bg hover:text-danger"
          : "hover:bg-hover hover:text-foreground",
      )}
    >
      {children}
    </button>
  );
}

export default function Titlebar() {
  const [maximized, setMaximized] = useState(false);
  /** Unsaved work found when the close button was pressed: the window asks before closing. */
  const [unsaved, setUnsaved] = useState<string[] | null>(null);
  const tauri = isTauri();
  const closeWindow = () => getCurrentWindow().close().catch(() => {});
  const askClose = () => {
    const found = unsavedWork();
    if (found.length) setUnsaved(found);
    else void closeWindow();
  };

  useEffect(() => {
    if (!tauri) return;
    const win = getCurrentWindow();
    win.isMaximized().then(setMaximized).catch(() => {});
    // Sync icon when maximized state changes outside the buttons
    // (OS shortcuts, taskbar). core:event:allow-listen is in core:default.
    let unlisten: (() => void) | undefined;
    win
      .onResized(() => {
        win.isMaximized().then(setMaximized).catch(() => {});
      })
      .then((off) => {
        unlisten = off;
      })
      .catch(() => {});
    return () => unlisten?.();
  }, [tauri]);

  const toggleMaximize = useCallback(async () => {
    if (!tauri) return;
    const win = getCurrentWindow();
    await win.toggleMaximize().catch(() => {});
    win.isMaximized().then(setMaximized).catch(() => {});
  }, [tauri]);

  const startDrag = useCallback(
    (e: React.MouseEvent) => {
      // Manual drag: data-tauri-drag-region alone does not move the window
      // on this setup. Clicks on the control buttons must not drag.
      if (!tauri || e.button !== 0) return;
      if ((e.target as HTMLElement).closest("button")) return;
      getCurrentWindow().startDragging().catch(() => {});
    },
    [tauri],
  );

  // The dialog sits beside the header: events from a portal bubble through React parents, and
  // the header's mouse-down drags the window.
  return (
    <>
      <header
        onMouseDown={startDrag}
        onDoubleClick={toggleMaximize}
        className="flex h-9 shrink-0 items-center justify-between border-b border-border bg-sidebar shadow-[var(--rg-shadow-rim)] select-none"
      >
        <div className="flex items-center gap-2 px-3">
          <span className="text-[13px] font-medium text-foreground">{t("app.name")}</span>
        </div>
        {tauri && (
          <div className="flex h-full items-stretch">
            <ControlButton
              label={t("window.minimize")}
              onClick={() => getCurrentWindow().minimize().catch(() => {})}
            >
              <Minus size={16} strokeWidth={1.5} />
            </ControlButton>
            <ControlButton
              label={maximized ? t("window.restore") : t("window.maximize")}
              onClick={toggleMaximize}
            >
              {maximized ? (
                <Copy size={16} strokeWidth={1.5} />
              ) : (
                <Square size={16} strokeWidth={1.5} />
              )}
            </ControlButton>
            <ControlButton
              label={t("window.close")}
              danger
              onClick={askClose}
            >
              <X size={16} strokeWidth={1.5} />
            </ControlButton>
          </div>
        )}
      </header>
      <Dialog open={!!unsaved} onOpenChange={(open) => { if (!open) setUnsaved(null); }}>
        {unsaved && <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("window.unsaved.title")}</DialogTitle>
            <DialogDescription>{t("window.unsaved.description", { items: unsaved.join(", ") })}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setUnsaved(null)}>{t("editor.cancel")}</Button>
            <Button variant="destructive" onClick={() => { setUnsaved(null); void closeWindow(); }}>{t("window.unsaved.close")}</Button>
          </DialogFooter>
        </DialogContent>}
      </Dialog>
    </>
  );
}
