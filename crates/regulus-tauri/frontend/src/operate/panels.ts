// The panels of an execution in the Operate view, as data: which there are, where each goes
// and when it has something to show. The layout (order, column, hidden) is a setting of the
// view, with `operate.panels` of `config.client` read on top of the defaults
// (docs/04-client.md §8). Pure: tested in panels.test.ts.
import type { Port, Workflow } from "@/composition/model";
import type { LocaleKey } from "@/lib/i18n";
import { hasConversation } from "./conversation";
import type { Execution } from "./model";

export type Placement = "main" | "aside";

/** What a panel can look at to decide whether it shows. */
export interface PanelContext {
  execution: Execution;
  workflow: Workflow | undefined;
  /** The workflow's own inputs (`workflow.send`). */
  inputs: Port[];
  /** Executions of the list that this one started. */
  children: Execution[];
}

export interface PanelDefinition {
  id: string;
  title: LocaleKey;
  placement: Placement;
  isAvailable: (ctx: PanelContext) => boolean;
}

const always = () => true;

/** Every panel the view knows, in their default order. */
export const panelRegistry = [
  { id: "nodes", title: "operate.nodes.title", placement: "main", isAvailable: always },
  // A direct agent node, or a workflow with text in and out (bound by the person, never by itself).
  { id: "conversation", title: "operate.conversation.title", placement: "main", isAvailable: (ctx) => hasConversation(ctx.workflow) },
  { id: "send", title: "operate.sendForm.title", placement: "main", isAvailable: (ctx) => ctx.inputs.length > 0 },
  { id: "events", title: "operate.events.title", placement: "main", isAvailable: always },
  { id: "tree", title: "operate.tree.title", placement: "aside", isAvailable: (ctx) => !!ctx.execution.parent || ctx.children.length > 0 },
  { id: "node", title: "operate.panels.node", placement: "aside", isAvailable: always },
  { id: "decisions", title: "operate.human.title", placement: "aside", isAvailable: always },
] as const satisfies readonly PanelDefinition[];

export type PanelId = (typeof panelRegistry)[number]["id"];

export interface PanelEntry {
  id: PanelId;
  place: Placement;
  hidden: boolean;
}

const known = (id: unknown): id is PanelId => panelRegistry.some((panel) => panel.id === id);
const definition = (id: PanelId) => panelRegistry.find((panel) => panel.id === id)!;

export const defaultLayout = (): PanelEntry[] => panelRegistry.map((panel) => ({ id: panel.id, place: panel.placement, hidden: false }));

/**
 * A layout as written: a list whose items are a panel id or `{ id, place?, hidden? }`.
 * Unknown ids and repeats drop; the panels the list leaves out are appended, `hidden` when
 * `unlisted` says so (a list in config.toml shows only what it names) or shown (a layout
 * saved before a panel existed still gets it). `null` when there is no list to read.
 */
export function readLayout(raw: unknown, unlisted: "shown" | "hidden"): PanelEntry[] | null {
  if (!Array.isArray(raw)) return null;
  const entries: PanelEntry[] = [];
  for (const item of raw) {
    const fields = typeof item === "string" ? { id: item } : item && typeof item === "object" ? item as Record<string, unknown> : {};
    if (!known(fields.id) || entries.some((entry) => entry.id === fields.id)) continue;
    const place = fields.place === "main" || fields.place === "aside" ? fields.place : definition(fields.id).placement;
    entries.push({ id: fields.id, place, hidden: fields.hidden === true });
  }
  if (!entries.length) return null;
  for (const panel of panelRegistry) {
    if (!entries.some((entry) => entry.id === panel.id)) entries.push({ id: panel.id, place: panel.placement, hidden: unlisted === "hidden" });
  }
  return entries;
}

/** The panels to draw for an execution, per column, in the layout's order. */
export function arrangePanels(layout: PanelEntry[], ctx: PanelContext): Record<Placement, PanelId[]> {
  const shown = layout.filter((entry) => !entry.hidden && definition(entry.id).isAvailable(ctx));
  return { main: shown.filter((entry) => entry.place === "main").map((entry) => entry.id), aside: shown.filter((entry) => entry.place === "aside").map((entry) => entry.id) };
}

/** The layout with panel `id` one step earlier (`-1`) or later (`1`). */
export function movePanel(layout: PanelEntry[], id: PanelId, step: -1 | 1): PanelEntry[] {
  const at = layout.findIndex((entry) => entry.id === id);
  const to = at + step;
  if (at < 0 || to < 0 || to >= layout.length) return layout;
  const next = [...layout];
  [next[at], next[to]] = [next[to], next[at]];
  return next;
}

export function changePanel(layout: PanelEntry[], id: PanelId, patch: Partial<Omit<PanelEntry, "id">>): PanelEntry[] {
  return layout.map((entry) => entry.id === id ? { ...entry, ...patch } : entry);
}
