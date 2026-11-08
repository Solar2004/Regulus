// Templates in the app (docs/11-composition.md §7): list, show, apply, then open the
// profile in the canvas or run it. Same methods as `regulus template ls|show|apply`.
import { useEffect, useMemo, useState, type FormEvent } from "react";
import { CircleCheck, FileCode, GitBranch, LayoutTemplate, PackageOpen, Play, RefreshCw, TriangleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useCore } from "@/core/connection";
import { t, type LocaleKey } from "@/lib/i18n";
import { operateApi } from "@/operate/client";
import { errorText, fill } from "@/operate/logic";
import type { ModuleSummary } from "@/operate/model";
import { useOperateSettings } from "@/operate/settings";
import { Mark } from "@/operate/StateMark";
import { useRegulusUi } from "@/store/useRegulusUi";
import { templatesApi } from "./client";
import { applyErrorOf, canApply, initialValues, isRequired, missingVars, requireState, varsToSend, type FormValues } from "./logic";
import type { TemplateApplied, TemplateSummary } from "./model";
import "@/operate/operate.css";

const requireMarks = {
  installed: { icon: CircleCheck, tone: "success", label: "templates.installed" },
  missing: { icon: PackageOpen, tone: "muted", label: "templates.missing" },
  broken: { icon: TriangleAlert, tone: "danger", label: "templates.broken" },
} as const;

function ApplyForm({ template, onApplied }: { template: TemplateSummary; onApplied: (applied: TemplateApplied) => void }) {
  const { client } = useCore();
  const [values, setValues] = useState<FormValues>(() => initialValues(template.vars));
  const [id, setId] = useState("");
  const [force, setForce] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<{ kind: LocaleKey; message: string } | null>(null);
  const missing = missingVars(template.vars, values);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!client || !canApply(template, values)) return;
    setBusy(true);
    try {
      onApplied(await templatesApi(client).apply({ template: template.name, vars: varsToSend(template.vars, values), id, force }));
      setFailure(null);
    } catch (error) {
      setFailure({ kind: `templates.error.${applyErrorOf(error)}` as LocaleKey, message: errorText(error) });
    } finally { setBusy(false); }
  };
  return <form className="template-form" onSubmit={submit} aria-label={fill(t("templates.applyTitle"), { name: template.name })}>
    {template.vars.length === 0 && <p className="operate-empty">{t("templates.noVars")}</p>}
    {template.vars.map((variable) => <label key={variable.name} className="operate-field">
      <span>{variable.name}{isRequired(variable) ? <em className="required"> {t("templates.required")}</em> : null}</span>
      <input value={values[variable.name] ?? ""} aria-invalid={missing.includes(variable.name) || undefined}
        placeholder={isRequired(variable) ? "" : fill(t("templates.default"), { value: String(variable.default) })}
        onChange={(e) => setValues({ ...values, [variable.name]: e.target.value })} />
      {variable.description && <small>{variable.description}</small>}
    </label>)}
    <label className="operate-field"><span>{t("templates.profileId")}</span><input value={id} placeholder={t("templates.profileIdHint")} onChange={(e) => setId(e.target.value)} /></label>
    <label className="operate-check"><input type="checkbox" checked={force} onChange={(e) => setForce(e.target.checked)} />{t("templates.force")}</label>
    {missing.length > 0 && <p className="operate-subtle">{fill(t("templates.fillRequired"), { vars: missing.join(", ") })}</p>}
    {failure && <div className="operate-error" role="alert"><strong>{t(failure.kind)}</strong><code className="operate-reason">{failure.message}</code></div>}
    <Button type="submit" disabled={busy || !canApply(template, values)}><LayoutTemplate size={14} />{t("templates.apply")}</Button>
  </form>;
}

function Applied({ applied }: { applied: TemplateApplied }) {
  const { client } = useCore();
  const openInCanvas = useRegulusUi((s) => s.openInCanvas);
  const setView = useRegulusUi((s) => s.setView);
  const startTimeoutMs = useOperateSettings((s) => s.startTimeoutMs);
  const [ran, setRan] = useState<string | null>(null);
  const [runError, setRunError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const run = async () => {
    if (!client) return;
    setBusy(true);
    try {
      const execution = await operateApi(client).start(applied.profile_id, undefined, startTimeoutMs);
      setRan(fill(t("operate.started"), { id: execution.id })); setRunError(null);
    } catch (error) { setRunError(errorText(error)); } finally { setBusy(false); }
  };
  return <div className="template-applied" role="status">
    <p><CircleCheck size={14} /> {fill(t("templates.applied"), { id: applied.profile_id, path: applied.path })}</p>
    {applied.diagnostics.length > 0 && <ul className="diagnostics">{applied.diagnostics.map((item, index) => <li key={index} className={`tone-${item.severity === "error" ? "danger" : item.severity === "warning" ? "warning" : "info"}`}><strong>{t(`templates.severity.${item.severity}` as LocaleKey)}</strong> <code>{item.code}</code> {item.message}</li>)}</ul>}
    <div className="decision-actions">
      <Button variant="outline" size="sm" onClick={() => openInCanvas(applied.profile_id)}><GitBranch size={14} />{t("templates.openCanvas")}</Button>
      <Button size="sm" disabled={busy} onClick={() => void run()}><Play size={14} />{t("templates.runNow")}</Button>
      {ran && <><span className="operate-subtle">{ran}</span><Button variant="ghost" size="sm" onClick={() => setView("operate")}>{t("templates.goOperate")}</Button></>}
    </div>
    {runError && <div className="operate-error" role="alert"><code className="operate-reason">{runError}</code></div>}
  </div>;
}

function TemplateCard({ template, modules }: { template: TemplateSummary; modules: ModuleSummary[] }) {
  const { client } = useCore();
  const [mode, setMode] = useState<"closed" | "apply" | "show">("closed");
  const [text, setText] = useState<string | null>(null);
  const [applied, setApplied] = useState<TemplateApplied | null>(null);
  const show = async () => {
    if (mode === "show") { setMode("closed"); return; }
    setMode("show");
    if (client && text === null) {
      try { setText((await templatesApi(client).get(template.path || template.name)).text); } catch (error) { setText(errorText(error)); }
    }
  };
  return <li className="template-card">
    <header>
      <div><h3>{template.name}</h3><p>{template.description}</p><small>{template.path}</small></div>
      <div className="decision-actions">
        <Button variant="outline" size="sm" aria-pressed={mode === "show"} onClick={() => void show()}><FileCode size={14} />{t("templates.show")}</Button>
        {!template.error && <Button size="sm" aria-pressed={mode === "apply"} onClick={() => setMode(mode === "apply" ? "closed" : "apply")}><LayoutTemplate size={14} />{t("templates.apply")}</Button>}
      </div>
    </header>
    {template.error ? <div className="operate-error" role="status"><Mark icon={TriangleAlert} tone="danger" label={t("templates.unusable")} /><code className="operate-reason">{template.error}</code></div> : <>
      {template.requires.length > 0 && <ul className="template-requires" aria-label={t("templates.requires")}>{template.requires.map((require) => {
        const mark = requireMarks[requireState(require, modules)];
        return <li key={require.module}><code>{require.module} {require.version}</code><Mark icon={mark.icon} tone={mark.tone} label={t(mark.label)} /></li>;
      })}</ul>}
      {template.vars.length > 0 && <dl className="template-vars">{template.vars.map((variable) => <div key={variable.name}>
        <dt>{variable.name}{isRequired(variable) && <em className="required"> {t("templates.required")}</em>}</dt>
        <dd>{variable.description}{!isRequired(variable) && <small> · {fill(t("templates.default"), { value: String(variable.default) })}</small>}</dd>
      </div>)}</dl>}
    </>}
    {mode === "show" && <pre className="template-text" aria-label={t("templates.text")}>{text ?? t("graph.loading")}</pre>}
    {mode === "apply" && <ApplyForm template={template} onApplied={setApplied} />}
    {applied && <Applied applied={applied} />}
  </li>;
}

export default function TemplatesScene() {
  const { status, client, reason, connect, source } = useCore();
  const [templates, setTemplates] = useState<TemplateSummary[] | null>(null);
  const [modules, setModules] = useState<ModuleSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const api = useMemo(() => client ? { templates: templatesApi(client), operate: operateApi(client) } : null, [client]);
  const load = async () => {
    if (!api) return;
    try {
      const [listed, installed] = await Promise.all([api.templates.list(), api.operate.modules()]);
      setTemplates(listed); setModules(installed); setError(null);
    } catch (failure) { setError(errorText(failure)); setTemplates((current) => current ?? []); }
  };
  useEffect(() => { setTemplates(null); void load(); }, [api]); // eslint-disable-line react-hooks/exhaustive-deps
  return <section className="operate-scene">
    <header className="graph-toolbar">
      <div><LayoutTemplate size={16} /><h1>{t("templates.title")}</h1></div>
      <div><span className="graph-source">{t(source === "mock" ? "graph.mock" : "graph.core")}</span>
        <Button variant="ghost" size="sm" onClick={() => void load()}><RefreshCw size={14} />{t("operate.reload")}</Button></div>
    </header>
    {status !== "ready" || templates === null ? <div className="graph-connection-state" role="status"><LayoutTemplate size={36} strokeWidth={1.25} />
      <h2>{t(status === "loading" || status === "ready" ? "graph.loading" : status === "incompatible" ? "graph.incompatible" : "graph.offline")}</h2>
      {reason && status !== "loading" && <code className="graph-reason">{reason}</code>}
      {status !== "loading" && status !== "ready" && <Button variant="outline" onClick={connect}>{t("graph.retry")}</Button>}
    </div> : <div className="templates-body">
      <p className="scene-description">{t("templates.description")}</p>
      {error && <p className="operate-error" role="alert">{error}</p>}
      {templates.length === 0 ? <p className="operate-empty">{t("templates.empty")}</p>
        : <ul className="template-list">{templates.map((template) => <TemplateCard key={template.path || template.name} template={template} modules={modules} />)}</ul>}
    </div>}
  </section>;
}
