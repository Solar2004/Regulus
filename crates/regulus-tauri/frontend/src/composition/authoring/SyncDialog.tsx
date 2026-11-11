import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { t } from "@/lib/i18n";
import type { Port, ProfileSynced } from "../model";
import { changedKeys, syncCounts } from "./logic";

const portLine = (port: Port) => `${port.direction === "input" ? "→" : "←"} ${port.id} · ${port.kind} ${port.capability}`;

/**
 * What a port sync would do, from the core's preview (`profile.sync`), before anything
 * changes: ports added, removed and changed per node, and every wire that would go with
 * its reason. Applying is one change of the document, which can be undone and is not
 * saved until the person saves.
 */
export function SyncDialog({ preview, onApply, onClose }: { preview: ProfileSynced | null; onApply: () => void; onClose: () => void }) {
  const counts = preview ? syncCounts(preview) : null;
  const shown = preview?.nodes.filter((item) => item.added.length || item.removed.length || item.changed.length || item.wires_removed.length || item.problems.length) ?? [];
  return <Dialog open={!!preview} onOpenChange={(open) => { if (!open) onClose(); }}>
    {preview && counts && <DialogContent className="sync-dialog">
      <DialogHeader>
        <DialogTitle>{t("authoring.sync.title")}</DialogTitle>
        <DialogDescription>{t(preview.drops_wires ? "authoring.sync.dropsWires" : "authoring.sync.keepsWires", { nodes: counts.nodes, wires: counts.wires })}</DialogDescription>
      </DialogHeader>
      <ul className="sync-nodes">{shown.map((item) => <li key={`${item.workflow_id}/${item.node_id}`}>
        <div className="sync-node-head"><strong>{item.node_id}</strong><code>{item.workflow_id} · {item.node_type}</code></div>
        {item.added.map((port) => <div className="sync-line is-added" key={`+${port.id}`}><span aria-hidden="true">+</span>{t("authoring.sync.added")} <code>{portLine(port)}</code></div>)}
        {item.removed.map((port) => <div className="sync-line is-removed" key={`-${port.id}`}><span aria-hidden="true">−</span>{t("authoring.sync.removed")} <code>{portLine(port)}</code></div>)}
        {item.changed.map(({ before, after }) => <div className="sync-line is-changed" key={`~${before.id}`}><span aria-hidden="true">~</span>{t("authoring.sync.changed")} <code>{portLine(before)}</code> → <code>{portLine(after)}</code>
          <small>{changedKeys(before, after).join(", ")}</small></div>)}
        {item.wires_removed.map(({ wire, reason }) => <div className="sync-line is-wire" key={`w${wire.id}`}><span aria-hidden="true">✕</span>{t("authoring.sync.wire")} <code>{wire.from.node_id}.{wire.from.port_id} → {wire.to.node_id}.{wire.to.port_id}</code><small>{reason}</small></div>)}
        {item.problems.map((problem, index) => <div className="sync-line is-problem" key={`p${index}`}><span aria-hidden="true">!</span>{problem}</div>)}
      </li>)}</ul>
      {!shown.length && <p>{t("authoring.sync.nothing")}</p>}
      <p className="form-hint">{t("authoring.sync.hint")}</p>
      <DialogFooter>
        <Button variant="outline" onClick={onClose}>{t("editor.cancel")}</Button>
        <Button variant={preview.drops_wires ? "destructive" : "default"} disabled={!counts.nodes} onClick={() => { onClose(); onApply(); }}>
          {t(preview.drops_wires ? "authoring.sync.applyDropping" : "authoring.sync.apply", { wires: counts.wires })}
        </Button>
      </DialogFooter>
    </DialogContent>}
  </Dialog>;
}
