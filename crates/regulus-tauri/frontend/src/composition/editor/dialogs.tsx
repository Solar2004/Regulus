import { useState } from "react";
import { ArrowRight, Minus, Plus } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { t } from "@/lib/i18n";
import { profileIdProblem, slugify, uniqueId } from "./document";
import { emptyTemplateForm, templateSaveErrorOf, templateSaveParams, type TemplateForm, type TemplateSaveParams } from "./template";
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

/** Names the new workflow of "Create box from selection"; the id it gets is shown as it is typed. */
export function CreateBoxDialog({ nodes, workflowIds, onClose, onCreate }: { nodes: string[] | null; workflowIds: string[]; onClose: () => void; onCreate: (name: string) => void }) {
  const [name, setName] = useState("");
  const close = () => { setName(""); onClose(); };
  const shown = name.trim() || t("editor.box.defaultName");
  return <Dialog open={!!nodes} onOpenChange={(next) => { if (!next) close(); }}>
    {nodes && <DialogContent>
      <form className="editor-form" onSubmit={(event) => { event.preventDefault(); onCreate(shown); close(); }}>
        <DialogHeader><DialogTitle>{t("editor.box.create")}</DialogTitle><DialogDescription>{t("editor.box.dialogDescription", { count: nodes.length })}</DialogDescription></DialogHeader>
        <label><span>{t("editor.box.name")}</span><input autoFocus value={name} placeholder={t("editor.box.defaultName")} onChange={(event) => setName(event.target.value)} /></label>
        <small className="form-hint">{t("editor.box.idPreview", { id: uniqueId(shown, workflowIds, "workflow") })}</small>
        <DialogFooter>
          <Button type="button" variant="outline" onClick={close}>{t("editor.cancel")}</Button>
          <Button type="submit">{t("editor.box.createAction")}</Button>
        </DialogFooter>
      </form>
    </DialogContent>}
  </Dialog>;
}

type SaveOutcome = { ok: true; path: string } | { ok: false; error: ReturnType<typeof templateSaveErrorOf>; message: string };

/**
 * "Save as template": the saved profile (or the workflow on screen) written as a template
 * file by `template.save`. Shows where it went, or the core's error by its code.
 */
export function SaveTemplateDialog({ open, profileId, workflowId, dirty, onClose, onSave, onOpenTemplates }: {
  open: boolean; profileId: string; workflowId: string | null; dirty: boolean; onClose: () => void;
  onSave: (params: TemplateSaveParams) => Promise<{ path: string }>; onOpenTemplates: () => void;
}) {
  const [form, setForm] = useState<TemplateForm>(() => emptyTemplateForm(profileId));
  const [outcome, setOutcome] = useState<SaveOutcome | null>(null);
  const [busy, setBusy] = useState(false);
  const checked = templateSaveParams(profileId, workflowId, form);
  const set = (patch: Partial<TemplateForm>) => { setForm({ ...form, ...patch }); setOutcome(null); };
  const setVar = (index: number, patch: Partial<TemplateForm["vars"][number]>) => set({ vars: form.vars.map((row, at) => at === index ? { ...row, ...patch } : row) });
  const close = () => { setForm(emptyTemplateForm(profileId)); setOutcome(null); onClose(); };
  const submit = async () => {
    if (!checked.ok || busy) return;
    setBusy(true);
    try { setOutcome({ ok: true, ...(await onSave(checked.params)) }); }
    catch (error) { setOutcome({ ok: false, error: templateSaveErrorOf(error), message: error instanceof Error ? error.message : String(error) }); }
    finally { setBusy(false); }
  };
  const problem = checked.ok ? null : checked;
  return <Dialog open={open} onOpenChange={(next) => { if (!next) close(); }}>
    {open && <DialogContent className="template-dialog">
      <form className="editor-form" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
        <DialogHeader><DialogTitle>{t("editor.template.title")}</DialogTitle><DialogDescription>{t("editor.template.description", { profile: profileId })}</DialogDescription></DialogHeader>
        {dirty && <small className="form-hint" role="note">{t("editor.template.savedVersion")}</small>}
        <label><span>{t("editor.template.name")}</span><input autoFocus value={form.name} aria-invalid={problem?.problem === "name" ? true : undefined} onChange={(event) => set({ name: event.target.value })} /></label>
        {problem?.problem === "name" && <small className="field-error" role="alert">{t("editor.template.nameEmpty")}</small>}
        <label><span>{t("editor.template.descriptionField")}</span><input value={form.description} onChange={(event) => set({ description: event.target.value })} /></label>
        <label className="check-row"><input type="checkbox" checked={form.onlyWorkflow} disabled={!workflowId} onChange={(event) => set({ onlyWorkflow: event.target.checked })} />
          <span>{workflowId ? t("editor.template.onlyWorkflow", { workflow: workflowId }) : t("editor.template.onlyWorkflowNone")}</span></label>
        <fieldset className="template-vars">
          <legend>{t("editor.template.vars")}</legend>
          <p className="form-hint">{t("editor.template.varsHelp")}</p>
          {form.vars.map((row, index) => <div className="template-var" key={index}>
            <input aria-label={t("editor.template.varName")} placeholder={t("editor.template.varName")} value={row.name} aria-invalid={problem && "row" in problem && problem.row === index ? true : undefined} onChange={(event) => setVar(index, { name: event.target.value })} />
            <input aria-label={t("editor.template.varDescription")} placeholder={t("editor.template.varDescription")} value={row.description} onChange={(event) => setVar(index, { description: event.target.value })} />
            <input aria-label={t("editor.template.varDefault")} placeholder={t("editor.template.varDefault")} value={row.default} onChange={(event) => setVar(index, { default: event.target.value })} />
            <button type="button" aria-label={t("editor.template.varRemove")} onClick={() => set({ vars: form.vars.filter((_, at) => at !== index) })}><Minus size={12} /></button>
          </div>)}
          {problem && "row" in problem && <small className="field-error" role="alert">{t(problem.problem === "varName" ? "editor.template.varNameInvalid" : "editor.template.varDuplicate")}</small>}
          <button type="button" className="map-add" onClick={() => set({ vars: [...form.vars, { name: "", description: "", default: "" }] })}><Plus size={12} />{t("editor.template.varAdd")}</button>
        </fieldset>
        <label><span>{t("editor.template.path")}</span><input value={form.path} placeholder={t("editor.template.pathHint")} onChange={(event) => set({ path: event.target.value })} /></label>
        <label className="check-row"><input type="checkbox" checked={form.force} onChange={(event) => set({ force: event.target.checked })} /><span>{t("editor.template.force")}</span></label>
        {outcome?.ok && <p className="template-outcome tone-success" role="status">{t("editor.template.saved")} <code>{outcome.path}</code>
          <button type="button" className="link-button" onClick={() => { close(); onOpenTemplates(); }}>{t("editor.template.openTemplates")}<ArrowRight size={12} /></button></p>}
        {outcome && !outcome.ok && <p className="template-outcome tone-error" role="alert">{t(`editor.template.error.${outcome.error}`)} <code>{outcome.message}</code></p>}
        <DialogFooter>
          <Button type="button" variant="outline" onClick={close}>{t(outcome?.ok ? "editor.done" : "editor.cancel")}</Button>
          <Button type="submit" disabled={!checked.ok || busy}>{t("editor.template.save")}</Button>
        </DialogFooter>
      </form>
    </DialogContent>}
  </Dialog>;
}
