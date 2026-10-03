import { useCallback, useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Copy, Minus, Square, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { t } from "@/lib/i18n";

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
  const tauri = isTauri();

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

  return (
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
            onClick={() => getCurrentWindow().close().catch(() => {})}
          >
            <X size={16} strokeWidth={1.5} />
          </ControlButton>
        </div>
      )}
    </header>
  );
}
