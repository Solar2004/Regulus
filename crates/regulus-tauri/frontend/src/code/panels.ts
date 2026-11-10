// The panels of the workspace pane, as data: which there are, in what order, and when one
// has something to show. Order and visibility are a setting of the view, with `code.panels`
// of `config.client` read on top of the defaults (docs/04-client.md §8, §11). Pure: tested
// in panels.test.ts.
import type { LocaleKey } from "@/lib/i18n";

/** What a panel can look at to decide whether it shows. */
export interface CodePanelContext {
  /** The session has a node that keeps workers. */
  hasWorkers: boolean;
}

export interface CodePanelDefinition {
  id: string;
  title: LocaleKey;
  isAvailable: (ctx: CodePanelContext) => boolean;
}

const always = () => true;

/** Every panel the view knows, in their default order. A new panel is one more entry. */
export const codePanelRegistry = [
  { id: "files", title: "code.panel.files", isAvailable: always },
  { id: "changes", title: "code.panel.changes", isAvailable: always },
  { id: "activity", title: "code.panel.activity", isAvailable: always },
  { id: "workers", title: "code.panel.workers", isAvailable: (ctx) => ctx.hasWorkers },
] as const satisfies readonly CodePanelDefinition[];

export type CodePanelId = (typeof codePanelRegistry)[number]["id"];

export interface CodePanelEntry {
  id: CodePanelId;
  hidden: boolean;
}

const known = (id: unknown): id is CodePanelId => codePanelRegistry.some((panel) => panel.id === id);
const definition = (id: CodePanelId) => codePanelRegistry.find((panel) => panel.id === id)!;

export const defaultCodeLayout = (): CodePanelEntry[] => codePanelRegistry.map((panel) => ({ id: panel.id, hidden: false }));

/**
 * A layout as written: a list whose items are a panel id or `{ id, hidden? }`. Unknown ids
 * and repeats drop; the panels the list leaves out are appended, `hidden` when `unlisted`
 * says so (a list in config.toml shows only what it names) or shown (a layout saved before
 * a panel existed still gets it). `null` when there is no list to read.
 */
export function readCodeLayout(raw: unknown, unlisted: "shown" | "hidden"): CodePanelEntry[] | null {
  if (!Array.isArray(raw)) return null;
  const entries: CodePanelEntry[] = [];
  for (const item of raw) {
    const fields = typeof item === "string" ? { id: item } : item && typeof item === "object" ? item as Record<string, unknown> : {};
    if (!known(fields.id) || entries.some((entry) => entry.id === fields.id)) continue;
    entries.push({ id: fields.id, hidden: fields.hidden === true });
  }
  if (!entries.length) return null;
  for (const panel of codePanelRegistry) {
    if (!entries.some((entry) => entry.id === panel.id)) entries.push({ id: panel.id, hidden: unlisted === "hidden" });
  }
  return entries;
}

/** The panels to offer for a session, in the layout's order. */
export function shownPanels(layout: CodePanelEntry[], ctx: CodePanelContext): CodePanelId[] {
  return layout.filter((entry) => !entry.hidden && definition(entry.id).isAvailable(ctx)).map((entry) => entry.id);
}

/** The panel on screen: the one picked if it is offered, else the first that is. */
export const activePanel = (picked: string | null, shown: CodePanelId[]): CodePanelId | null =>
  shown.find((id) => id === picked) ?? shown[0] ?? null;

/** The layout with panel `id` one step earlier (`-1`) or later (`1`). */
export function moveCodePanel(layout: CodePanelEntry[], id: CodePanelId, step: -1 | 1): CodePanelEntry[] {
  const at = layout.findIndex((entry) => entry.id === id);
  const to = at + step;
  if (at < 0 || to < 0 || to >= layout.length) return layout;
  const next = [...layout];
  [next[at], next[to]] = [next[to], next[at]];
  return next;
}

export const toggleCodePanel = (layout: CodePanelEntry[], id: CodePanelId, hidden: boolean): CodePanelEntry[] =>
  layout.map((entry) => entry.id === id ? { ...entry, hidden } : entry);
