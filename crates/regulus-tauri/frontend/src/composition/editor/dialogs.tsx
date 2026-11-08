import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { t } from "@/lib/i18n";
import { profileIdProblem, slugify } from "./document";
import { canvasLimits, useCanvasSettings, type CanvasSettings } from "./settings";
import type { Grouping } from "./palette";

export type Confirm = { title: string; description: string; action: string; destructive?: boolean; run: () => void };

export function ConfirmDialog({ confirm, onClose }: { confirm: Confirm | null; onClose: () => void }) {
  return <Dialog open={!!confirm} onOpenChange={(open) => { if (!open) onClose(); }}>
    {confirm && <DialogContent>
      <DialogHeader><DialogTitle>{confirm.title}</DialogTitle><DialogDescription>{confirm.description}</DialogDescription></DialogHeader>
      <DialogFooter>
        <Button variant="outline" onClick={onClose}>{t("editor.cancel")}</Button>
        <Button variant={confirm.destructive ? "destructive" : "default"} onClick={() => { onClose(); confirm.run(); }}>{confirm.action}</Button>
      </DialogFooter>
    </DialogContent>}
  </Dialog>;
}

export function NewProfileDialog({ open, taken, onClose, onCreate }: { open: boolean; taken: string[]; onClose: () => void; onCreate: (id: string, name: string) => void }) {
  const [name, setName] = useState("");
  const [id, setId] = useState("");
  const [idEdited, setIdEdited] = useState(false);
  const shownId = idEdited ? id : slugify(name);
  const problem = profileIdProblem(shownId, taken);
  const close = () => { setName(""); setId(""); setIdEdited(false); onClose(); };
  return <Dialog open={open} onOpenChange={(next) => { if (!next) close(); }}>
    <DialogContent>
      <form className="editor-form" onSubmit={(event) => { event.preventDefault(); if (!problem) { onCreate(shownId, name.trim() || shownId); close(); } }}>
        <DialogHeader><DialogTitle>{t("editor.newProfile.title")}</DialogTitle><DialogDescription>{t("editor.newProfile.description")}</DialogDescription></DialogHeader>
        <label><span>{t("editor.newProfile.name")}</span><input autoFocus value={name} onChange={(event) => setName(event.target.value)} /></label>
        <label><span>{t("editor.newProfile.id")}</span><input value={shownId} aria-invalid={problem && (name || idEdited) ? true : undefined} onChange={(event) => { setIdEdited(true); setId(event.target.value); }} /></label>
        {problem && (name || idEdited) && <small className="field-error" role="alert">{t(`editor.newProfile.${problem}`)}</small>}
        <DialogFooter>
          <Button type="button" variant="outline" onClick={close}>{t("editor.cancel")}</Button>
          <Button type="submit" disabled={!!problem}>{t("editor.newProfile.create")}</Button>
        </DialogFooter>
      </form>
    </DialogContent>
  </Dialog>;
}

const numeric: { key: keyof typeof canvasLimits; label: Parameters<typeof t>[0]; step: number }[] = [
  { key: "minZoom", label: "editor.settings.minZoom", step: 0.05 },
  { key: "maxZoom", label: "editor.settings.maxZoom", step: 0.1 },
  { key: "gridSize", label: "editor.settings.gridSize", step: 1 },
  { key: "nodeWidth", label: "editor.settings.nodeWidth", step: 10 },
  { key: "autosaveMs", label: "editor.settings.autosaveMs", step: 1000 },
];

export function SettingsDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const settings = useCanvasSettings();
  const set = (patch: Partial<CanvasSettings>) => settings.set(patch);
  return <Dialog open={open} onOpenChange={(next) => { if (!next) onClose(); }}>
    <DialogContent>
      <div className="editor-form">
        <DialogHeader><DialogTitle>{t("editor.settings.title")}</DialogTitle><DialogDescription>{t("editor.settings.description")}</DialogDescription></DialogHeader>
        {numeric.map(({ key, label, step }) => <label key={key}><span>{t(label)}</span>
          <input type="number" step={step} min={canvasLimits[key][0]} max={canvasLimits[key][1]} defaultValue={settings[key]}
            onBlur={(event) => { const value = Number(event.target.value); set({ [key]: value }); event.target.value = String(useCanvasSettings.getState()[key]); }} />
        </label>)}
        <label className="check-row"><input type="checkbox" checked={settings.snapToGrid} onChange={(event) => set({ snapToGrid: event.target.checked })} /><span>{t("editor.settings.snap")}</span></label>
        <label className="check-row"><input type="checkbox" checked={settings.autosave} onChange={(event) => set({ autosave: event.target.checked })} /><span>{t("editor.settings.autosave")}</span></label>
        <label><span>{t("editor.settings.grouping")}</span>
          <select value={settings.paletteGrouping} onChange={(event) => set({ paletteGrouping: event.target.value as Grouping })}>
            <option value="module">{t("editor.settings.byModule")}</option><option value="state">{t("editor.settings.byState")}</option><option value="none">{t("editor.settings.noGroups")}</option>
          </select>
        </label>
        <DialogFooter>
          <Button variant="outline" onClick={() => { settings.reset(); onClose(); }}>{t("editor.settings.reset")}</Button>
          <Button onClick={onClose}>{t("editor.done")}</Button>
        </DialogFooter>
      </div>
    </DialogContent>
  </Dialog>;
}
