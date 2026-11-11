// Editors that hold typed text not yet written to the document they belong to. Whatever
// reads that document as a whole (save, check, run, switching profile) asks them to
// write it first, so a keystroke never needs to rebuild the whole document.

const pending = new Set<() => void>();

/** Registers an editor's flush; the returned function unregisters it. */
export function registerFlush(flush: () => void): () => void {
  pending.add(flush);
  return () => { pending.delete(flush); };
}

/** Writes every editor's pending text now. */
export function flushEdits(): void {
  for (const flush of [...pending]) flush();
}
