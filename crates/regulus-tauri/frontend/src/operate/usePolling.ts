import { useEffect, useRef } from "react";

/** Whether the effect a poll started in is still the current one: check it after each await. */
export type StillCurrent = () => boolean;

/**
 * Runs `poll` now and every `ms` while the component is mounted (the view is shown), the
 * page is visible and `enabled` holds. Polls never overlap, not even across a change of
 * `deps`: a tick still running from the previous effect makes the new one wait for the next
 * interval, and its `current()` turns false so it drops what it got. A poll that throws is
 * reported to `onError` and polling goes on.
 */
export function usePolling(
  poll: (current: StillCurrent) => Promise<void> | void,
  ms: number,
  deps: unknown[],
  { enabled = true, onError }: { enabled?: boolean; onError?: (error: unknown) => void } = {},
) {
  const latest = useRef({ poll, onError });
  latest.current = { poll, onError };
  const busy = useRef(false);
  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    const current = () => !disposed;
    const tick = async () => {
      if (disposed || busy.current || document.visibilityState === "hidden") return;
      busy.current = true;
      try { await latest.current.poll(current); }
      catch (error) { if (!disposed) latest.current.onError?.(error); }
      finally { busy.current = false; }
    };
    void tick();
    const timer = setInterval(() => void tick(), ms);
    const onVisible = () => { if (document.visibilityState === "visible") void tick(); };
    document.addEventListener("visibilitychange", onVisible);
    return () => { disposed = true; clearInterval(timer); document.removeEventListener("visibilitychange", onVisible); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ms, enabled, ...deps]);
}
