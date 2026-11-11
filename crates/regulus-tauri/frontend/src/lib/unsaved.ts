// What would be lost if the page or the window closed now. Each editor that keeps unsaved
// work (the profile on the canvas, a module source) registers a check when its code loads;
// the shell asks before the page unloads or the window closes while any says something.

const checks = new Set<() => string | null>();

/** Registers a check that names the unsaved work, or returns `null`; the returned function unregisters it. */
export function registerUnsaved(check: () => string | null): () => void {
  checks.add(check);
  return () => { checks.delete(check); };
}

/** What is unsaved now, one entry per editor (a profile's name, a file's path). */
export function unsavedWork(): string[] {
  return [...checks].map((check) => check()).filter((item): item is string => !!item);
}

/** A `beforeunload` listener: the browser asks before leaving while something is unsaved. */
export function guardUnload(event: BeforeUnloadEvent): void {
  if (unsavedWork().length) event.preventDefault();
}

/**
 * A desktop window's close request (title bar, Alt+F4, the window manager): kept open, and
 * `ask` told what is unsaved, while something is. True when the close was held.
 */
export function holdClose(event: { preventDefault(): void }, ask: (items: string[]) => void): boolean {
  const found = unsavedWork();
  if (!found.length) return false;
  event.preventDefault();
  ask(found);
  return true;
}
