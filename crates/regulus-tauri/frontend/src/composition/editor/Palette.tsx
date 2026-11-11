import { useMemo, useState } from "react";
import { Blocks, FilePlus2, GitBranch, Plus, Search } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ModuleStatus } from "@/components/ui/status";
import { t, type LocaleKey } from "@/lib/i18n";
import type { Manifest, ModuleDefinition, Profile } from "../model";
import { boxableWorkflows, filterPalette, groupPalette, paletteEntries, type PieceState } from "./palette";
import { stateView } from "./nodes";
import { useCanvasSettings } from "./settings";

/** What a palette entry adds: a node type of a module, or another workflow as a box. */
export type PieceRef = { kind: "type"; key: string } | { kind: "box"; workflowId: string };
export const PIECE_MIME = "application/x-regulus-piece";

export function Palette({ manifests, modules, profile, workflowId, onAdd, onNewNode }: {
  manifests: Manifest[]; modules: ModuleDefinition[]; profile: Profile; workflowId: string;
  onAdd: (piece: PieceRef) => void;
  /** Starts a new Python node module in a project; absent without a core. */
  onNewNode?: () => void;
}) {
  const [query, setQuery] = useState("");
  const grouping = useCanvasSettings((s) => s.paletteGrouping);
  const groups = useMemo(() => groupPalette(filterPalette(paletteEntries(manifests, modules), query), grouping), [manifests, modules, query, grouping]);
  const boxes = useMemo(() => {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    return boxableWorkflows(profile, workflowId).filter((workflow) => words.every((word) => `${workflow.name} ${workflow.id}`.toLowerCase().includes(word)));
  }, [profile, workflowId, query]);
  const drag = (piece: PieceRef) => (event: React.DragEvent) => {
    event.dataTransfer.setData(PIECE_MIME, JSON.stringify(piece));
    event.dataTransfer.effectAllowed = "copy";
  };
  const groupLabel = (label: string) => grouping === "state" ? t(stateView[label as PieceState].label) : label;

  return <aside className="editor-palette" aria-label={t("editor.palette.title")}>
    <header><h2>{t("editor.palette.title")}</h2><p>{t("editor.palette.help")}</p>
      {onNewNode && <Button variant="outline" size="sm" className="palette-new" onClick={onNewNode}><FilePlus2 size={14} />{t("authoring.newNode.action")}</Button>}
    </header>
    <label className="palette-search"><Search size={14} aria-hidden="true" /><input type="search" value={query} placeholder={t("editor.palette.search")} aria-label={t("editor.palette.search")} onChange={(event) => setQuery(event.target.value)} /></label>
    <div className="palette-groups">
      {groups.map((group) => <section key={group.key}>
        {group.label && <h3>{groupLabel(group.label)}</h3>}
        <ul>{group.entries.map((entry) => <li key={entry.key}>
          <button draggable onDragStart={drag({ kind: "type", key: entry.key })} onClick={() => onAdd({ kind: "type", key: entry.key })} title={t("editor.palette.add")}>
            <span className="palette-name"><Blocks size={14} aria-hidden="true" />{entry.name}<Plus size={12} className="palette-plus" aria-hidden="true" /></span>
            <span className="palette-meta"><code>{entry.type}</code><span>{entry.moduleName}</span></span>
            <ModuleStatus state={stateView[entry.state].kind} label={t(stateView[entry.state].label as LocaleKey)} />
          </button>
        </li>)}</ul>
      </section>)}
      {boxes.length > 0 && <section>
        <h3>{t("editor.palette.workflows")}</h3>
        <ul>{boxes.map((workflow) => <li key={workflow.id}>
          <button draggable onDragStart={drag({ kind: "box", workflowId: workflow.id })} onClick={() => onAdd({ kind: "box", workflowId: workflow.id })} title={t("editor.palette.add")}>
            <span className="palette-name"><GitBranch size={14} aria-hidden="true" />{workflow.name || workflow.id}<Plus size={12} className="palette-plus" aria-hidden="true" /></span>
            <span className="palette-meta"><code>{workflow.id}</code><span>{t("editor.kind.box")}</span></span>
          </button>
        </li>)}</ul>
      </section>}
      {!groups.length && !boxes.length && <p className="palette-empty">{t(manifests.length ? "editor.palette.noMatch" : "editor.palette.empty")}</p>}
    </div>
  </aside>;
}
