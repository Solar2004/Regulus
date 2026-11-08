import { useEffect, useRef } from "react";

/**
 * Runs `poll` now and every `ms` while the component is mounted (the view is shown) and the
 * page is visible. A poll never overlaps the one before it.
 */
export function usePolling(poll: () => Promise<void> | void, ms: number, deps: unknown[]) {
  const latest = useRef(poll);
  latest.current = poll;
  useEffect(() => {
    let disposed = false;
    let busy = false;
    const tick = async () => {
      if (disposed || busy || document.visibilityState === "hidden") return;
      busy = true;
      try { await latest.current(); } finally { busy = false; }
    };
    void tick();
    const timer = setInterval(() => void tick(), ms);
    const onVisible = () => { if (document.visibilityState === "visible") void tick(); };
    document.addEventListener("visibilitychange", onVisible);
    return () => { disposed = true; clearInterval(timer); document.removeEventListener("visibilitychange", onVisible); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ms, ...deps]);
}
