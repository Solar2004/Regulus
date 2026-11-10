// Where a coding session starts: point it at a folder. A folder already set up runs its
// saved harness (`execution.start`); a new one gets a profile from the template first
// (`template.apply`, as `regulus code <folder>` does). Below, the folders set up and their
// sessions: open a running one, read an earlier one, or edit a harness in the canvas.
import { useMemo, useState, type FormEvent } from "react";
import {
  ArrowRight, CircleCheck, CircleDashed, CircleHelp, FolderOpen, GitBranch, KeyRound, OctagonX, PackageOpen, Play, Plus, TriangleAlert, type LucideIcon,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { t, type LocaleKey } from "@/lib/i18n";
import { errorText, fill } from "@/operate/logic";
import { useOperateSettings } from "@/operate/settings";
import { ExecutionStateMark, Mark } from "@/operate/StateMark";
import { useRegulusUi } from "@/store/useRegulusUi";
import { applyErrorOf, initialValues, isRequired, missingVars, requireState, varsToSend, type FormValues } from "@/templates/logic";
import type { Diagnostic, TemplateSummary, TemplateVar } from "@/templates/model";
import { useCodeLabel, type CodeApis, type CodeData } from "./data";
import {
  codeModuleIds, codeTemplates, credentialState, folderName, folderRows, isAbsolutePath, normalizeRoot, profileIdProblem, setupVars, splitVars,
  suggestProfileId, targetForRoot, type CodeSession, type CodeTarget, type CredentialState, type FolderRow,
} from "./logic";
import { Mascot } from "./Mascot";
import { useCodeSettings } from "./settings";

type Tone = "success" | "warning" | "danger" | "muted" | "info";

const requireMarks = {
  installed: { icon: CircleCheck, tone: "success", label: "templates.installed" },
  missing: { icon: PackageOpen, tone: "muted", label: "templates.missing" },
  broken: { icon: TriangleAlert, tone: "danger", label: "templates.broken" },
} as const;

const credentialMarks: Record<CredentialState["kind"], { icon: LucideIcon; tone: Tone; label: LocaleKey }> = {
  none: { icon: KeyRound, tone: "muted", label: "code.credential.none" },
  stored: { icon: CircleCheck, tone: "success", label: "code.credential.stored" },
  missing: { icon: TriangleAlert, tone: "danger", label: "code.credential.missing" },
  unknown: { icon: CircleHelp, tone: "muted", label: "code.credential.unknown" },
  malformed: { icon: OctagonX, tone: "danger", label: "code.credential.malformed" },
};

/** The provider vars read better with a word than with their name; any other var shows as the template names it. */
const varLabels: Record<string, LocaleKey> = { preset: "code.var.preset", base_url: "code.var.base_url", credential: "code.var.credential", model: "code.var.model" };

/**
 * One template var. A provider var has a word of its own and keeps the template's
 * description as its tooltip; any other shows as the template names and describes it.
 */
function VarField({ variable, value, onChange, children }: { variable: TemplateVar; value: string; onChange: (value: string) => void; children?: React.ReactNode }) {
  const label = varLabels[variable.name];
  return <label className="operate-field" title={label ? variable.description : undefined}>
    <span>{label ? t(label) : variable.name}{isRequired(variable) && <em className="required"> {t("templates.required")}</em>}</span>
    <input value={value} spellCheck={false} placeholder={isRequired(variable) || !variable.default ? "" : fill(t("templates.default"), { value: String(variable.default) })}
      onChange={(event) => onChange(event.target.value)} />
    {children ?? (!label && variable.description && <small>{variable.description}</small>)}
  </label>;
}

/** The form that sets a folder up, or starts the one already set up there. */
function Setup({ api, data, targets, sessions, template, candidates, onTemplate, onOpen }: {
  api: CodeApis; data: CodeData; targets: CodeTarget[]; sessions: CodeSession[];
  template: TemplateSummary; candidates: TemplateSummary[]; onTemplate: (name: string) => void; onOpen: (executionId: string) => void;
}) {
  const settings = useCodeSettings();
  const startTimeoutMs = useOperateSettings((s) => s.startTimeoutMs);
  const setView = useRegulusUi((s) => s.setView);
  const openInCanvas = useRegulusUi((s) => s.openInCanvas);
  const vars = useMemo(() => splitVars(template.vars), [template]);
  const [root, setRoot] = useState("");
  const [typed, setTyped] = useState<FormValues>({});
  const [context, setContext] = useState<string[]>([]);
  const [ownId, setOwnId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<{ title: string; reason: string } | null>(null);
  const [diagnostics, setDiagnostics] = useState<Diagnostic[]>([]);

  // Defaults of the template, then how the provider was set last time, then what was typed.
  const remembered = Object.fromEntries(Object.entries(settings.lastVars).filter(([name]) => vars.provider.some((variable) => variable.name === name)));
  const values: FormValues = { ...initialValues(template.vars), ...remembered, ...typed, ...(vars.root ? { [vars.root.name]: normalizeRoot(root) } : {}) };
  const set = (name: string, value: string) => setTyped({ ...typed, [name]: value });
  const addContext = (value: string) => {
    if (context.includes(value)) return;
    setContext([...context, value]);
    const system = values.system ?? "";
    set("system", `${system}\nAdditional repository context: ${value}`);
  };

  const folder = normalizeRoot(root);
  const absolute = isAbsolutePath(folder);
  const existing = absolute ? targetForRoot(targets, folder) : undefined;
  const running = existing ? sessions.find((session) => session.live && session.target.profileId === existing.profileId && session.target.workflowId === existing.workflowId) : undefined;
  const taken = data.profiles.map((profile) => profile.id);
  const profileId = ownId ?? (absolute ? suggestProfileId(folder, taken, settings.profilePrefix) : "");
  const idProblem = existing || !absolute ? null : profileIdProblem(profileId, taken);
  const credential = credentialState(values.credential ?? "", data.secrets);
  const unmet = template.requires.filter((require) => requireState(require, data.modules) !== "installed");
  const missing = missingVars(template.vars, values);
  const canSetUp = absolute && !template.error && unmet.length === 0 && missing.length === 0 && !idProblem && credential.kind !== "malformed" && !!vars.root;
  const canGo = !busy && (existing ? true : canSetUp);

  const go = async (event: FormEvent) => {
    event.preventDefault();
    if (!canGo) return;
    if (running) { onOpen(running.execution.id); return; }
    setBusy(true);
    setFailure(null);
    let wrote = existing ? null : profileId;
    try {
      let target = existing ? { profileId: existing.profileId, workflowId: existing.workflowId as string | undefined } : null;
      if (!target) {
        const applied = await api.templates.apply({ template: template.name, vars: varsToSend(template.vars, values), id: profileId });
        setDiagnostics(applied.diagnostics);
        target = { profileId: applied.profile_id, workflowId: undefined };
        wrote = applied.profile_id;
        settings.rememberSetup(folder, values);
      }
      const execution = await api.operate.start(target.profileId, target.workflowId, startTimeoutMs);
      // The profiles are read again first: the session is found through its profile.
      await data.reload();
      data.seen(execution);
      if (execution.state.status === "failed") setFailure({ title: t("code.setup.startFailed"), reason: execution.state.reason });
      else onOpen(execution.id);
    } catch (error) {
      const applying = !existing && wrote === profileId && !data.profiles.some((profile) => profile.id === profileId);
      const known = applying ? t(`templates.error.${applyErrorOf(error)}` as LocaleKey) : t("code.setup.startFailed");
      setFailure({ title: known, reason: errorText(error) });
      void data.reload();
    } finally { setBusy(false); }
  };

  const suggestions = [...new Set([...settings.recent, ...data.projects.filter((project) => !project.error).map((project) => project.path)])]
    .filter((path) => normalizeRoot(path) !== folder).slice(0, settings.recentRoots);
  const hint = !folder ? t("code.setup.rootHint")
    : !absolute ? t("code.setup.rootRelative")
      : running ? fill(t("code.setup.rootRunning"), { id: running.execution.id })
        : existing ? fill(t("code.setup.rootKnown"), { profile: existing.profileId })
          : fill(t("code.setup.rootNew"), { profile: profileId, template: template.name });
  const credentialMark = credentialMarks[credential.kind];

  return <form className="code-setup" onSubmit={go} aria-label={t("code.setup.title")}>
    <label className="code-root">
      <span>{t("code.setup.root")}</span>
      <span className="code-root-line"><FolderOpen size={18} strokeWidth={1.5} aria-hidden="true" />
        <input value={root} spellCheck={false} autoComplete="off" placeholder={t("code.setup.rootPlaceholder")} aria-describedby="code-root-hint"
          aria-invalid={!!folder && !absolute || undefined} onChange={(event) => { setRoot(event.target.value); setFailure(null); }} />
      </span>
      <small id="code-root-hint" aria-live="polite">{hint}</small>
    </label>
    {suggestions.length > 0 && <p className="code-suggestions"><span>{t("code.setup.suggestions")}</span>
      {suggestions.map((path) => <button key={path} type="button" title={path} onClick={() => setRoot(path)}>{folderName(path) || path}</button>)}
    </p>}

    {existing ? <p className="code-note">
      <button type="button" className="link-button" onClick={() => openInCanvas(existing.profileId, { workflowId: existing.workflowId })}>{t("code.setup.editKnown")}</button>
    </p> : <>
      {vars.provider.length > 0 && <fieldset className="code-provider">
        <legend>{t("code.setup.provider")}</legend>
        {vars.more.some((variable) => variable.name === "tool_policy") && <label className="operate-field code-policy">
          <span>{t("code.setup.permissionMode")}</span>
          <select value={values.tool_policy ?? "ask"} onChange={(event) => set("tool_policy", event.target.value)}>
            <option value="ask">{t("code.setup.permissionAsk")}</option>
            <option value="allow">{t("code.setup.permissionAuto")}</option>
            <option value="deny">{t("code.setup.permissionDeny")}</option>
          </select>
          <small>{t("code.setup.permissionHelp")}</small>
        </label>}
        {vars.provider.map((variable) => variable.name === "credential"
          ? <VarField key={variable.name} variable={variable} value={values[variable.name] ?? ""} onChange={(value) => set(variable.name, value)}>
            <small className="code-credential"><Mark icon={credentialMark.icon} tone={credentialMark.tone} label={fill(t(credentialMark.label), { name: "name" in credential ? credential.name : "" })} />
              {(credential.kind === "missing" || credential.kind === "malformed") && <button type="button" className="link-button" onClick={() => setView("settings")}>{t("code.credential.settings")}</button>}
            </small>
          </VarField>
          : <VarField key={variable.name} variable={variable} value={values[variable.name] ?? ""} onChange={(value) => set(variable.name, value)} />)}
        <p className="code-fine">{t("code.setup.providerNote")}</p>
        <details className="code-context-picker">
          <summary><Plus size={14} />{t("code.setup.addContext")}</summary>
          <div className="code-context-actions">
            <Button type="button" size="sm" variant="outline" onClick={() => addContext("AGENTS.md")}>{t("code.setup.contextInstructions")}</Button>
            <Button type="button" size="sm" variant="outline" onClick={() => addContext("README.md")}>{t("code.setup.contextReadme")}</Button>
          </div>
          {context.length > 0 && <small>{fill(t("code.setup.contextAdded"), { items: context.join(", ") })}</small>}
        </details>
      </fieldset>}
      <details className="code-more">
        <summary>{fill(t("code.setup.more"), { count: vars.more.length + 1 + (candidates.length > 1 ? 1 : 0) })}</summary>
        <div className="code-more-fields">
          {vars.more.map((variable) => <VarField key={variable.name} variable={variable} value={values[variable.name] ?? ""} onChange={(value) => set(variable.name, value)} />)}
          <label className="operate-field"><span>{t("templates.profileId")}</span>
            <input value={profileId} spellCheck={false} aria-invalid={!!idProblem || undefined} onChange={(event) => setOwnId(event.target.value)} />
            <small>{idProblem ? t(`editor.newProfile.${idProblem}` as LocaleKey) : t("code.setup.profileIdHelp")}</small></label>
          {candidates.length > 1 && <label className="operate-field"><span>{t("code.setup.template")}</span>
            <select value={template.name} onChange={(event) => onTemplate(event.target.value)}>{candidates.map((item) => <option key={item.name} value={item.name}>{item.name}</option>)}</select>
            <small>{template.description}</small></label>}
        </div>
      </details>
      <ul className="template-requires" aria-label={t("templates.requires")}>{template.requires.map((require) => {
        const mark = requireMarks[requireState(require, data.modules)];
        return <li key={require.module}><code>{require.module}</code><Mark icon={mark.icon} tone={mark.tone} label={t(mark.label)} /></li>;
      })}</ul>
      {template.error && <div className="operate-error" role="status"><Mark icon={TriangleAlert} tone="danger" label={t("templates.unusable")} /><code className="operate-reason">{template.error}</code></div>}
      {unmet.length > 0 && <p className="code-note tone-danger">{fill(t("code.setup.unmet"), { modules: unmet.map((require) => require.module).join(", ") })}
        <button type="button" className="link-button" onClick={() => setView("settings")}>{t("code.setup.moduleSettings")}</button></p>}
      {missing.filter((name) => name !== setupVars.root).length > 0 && <p className="code-note">{fill(t("templates.fillRequired"), { vars: missing.filter((name) => name !== setupVars.root).join(", ") })}</p>}
    </>}

    {failure && <div className="operate-error" role="alert"><strong>{failure.title}</strong><code className="operate-reason">{failure.reason}</code></div>}
    {diagnostics.length > 0 && <ul className="diagnostics">{diagnostics.map((item, index) => <li key={index} className={`tone-${item.severity === "error" ? "danger" : item.severity === "warning" ? "warning" : "info"}`}>
      <strong>{t(`templates.severity.${item.severity}` as LocaleKey)}</strong> <code>{item.code}</code> {item.message}</li>)}</ul>}
    <div className="code-setup-go">
      <Button type="submit" size="lg" disabled={!canGo}>{running ? <ArrowRight size={16} /> : <Play size={16} />}
        {busy ? t("code.setup.starting") : t(running ? "code.setup.open" : existing ? "code.setup.start" : "code.setup.go")}</Button>
      <p className="code-fine">{t("code.setup.shellNote")}</p>
    </div>
  </form>;
}

/** One folder set up: its session, the earlier ones, and its harness. */
function Folder({ row, busy, onStart, onOpen }: { row: FolderRow; busy: boolean; onStart: (target: CodeTarget) => void; onOpen: (executionId: string) => void }) {
  const openInCanvas = useRegulusUi((s) => s.openInCanvas);
  const { target, live, earlier } = row;
  const name = target.root ? folderName(target.root) || target.root : target.profileName;
  return <li className="code-folder">
    <div className="code-folder-name">
      <strong>{name}</strong>
      <code title={target.root ?? undefined}>{target.root ?? t("code.folders.noRoot")}</code>
    </div>
    {live ? <ExecutionStateMark state={live.execution.state} /> : <Mark icon={CircleDashed} tone="muted" label={t("code.folders.noSession")} />}
    <div className="code-folder-actions">
      {live ? <Button size="sm" onClick={() => onOpen(live.execution.id)}><ArrowRight size={14} />{t("code.folders.open")}</Button>
        : <Button size="sm" variant="outline" disabled={busy} onClick={() => onStart(target)}><Play size={14} />{t(earlier.length ? "code.folders.startAgain" : "code.folders.start")}</Button>}
      <Button size="sm" variant="ghost" title={fill(t("code.folders.editTitle"), { profile: target.profileId })} onClick={() => openInCanvas(target.profileId, { workflowId: target.workflowId })}><GitBranch size={14} />{t("code.folders.edit")}</Button>
    </div>
    {earlier.length > 0 && <div className="code-folder-earlier">
      {!live && <p className="code-fine">{t("code.folders.newContext")}</p>}
      <details>
        <summary>{fill(t(earlier.length === 1 ? "code.folders.earlierOne" : "code.folders.earlier"), { count: earlier.length })}</summary>
        <ul>{earlier.map((session) => <li key={session.execution.id}>
          <code>{session.execution.id}</code><ExecutionStateMark state={session.execution.state} />
          <button type="button" className="link-button" onClick={() => onOpen(session.execution.id)}>{t("code.folders.read")}</button>
        </li>)}</ul>
      </details>
    </div>}
  </li>;
}

export function Launchpad({ api, data, targets, sessions, onOpen }: {
  api: CodeApis; data: CodeData; targets: CodeTarget[]; sessions: CodeSession[]; onOpen: (executionId: string) => void;
}) {
  const label = useCodeLabel();
  const settings = useCodeSettings();
  const startTimeoutMs = useOperateSettings((s) => s.startTimeoutMs);
  const setView = useRegulusUi((s) => s.setView);
  const candidates = useMemo(() => codeTemplates(data.templates, data.manifests, settings.template), [data.templates, data.manifests, settings.template]);
  const [picked, setPicked] = useState<string | null>(null);
  const template = candidates.find((item) => item.name === picked) ?? candidates[0] ?? null;
  const installed = codeModuleIds(data.manifests).length > 0;
  const rows = useMemo(() => folderRows(targets, sessions), [targets, sessions]);
  const [starting, setStarting] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  const start = async (target: CodeTarget) => {
    setStarting(true);
    setFailure(null);
    try {
      const execution = await api.operate.start(target.profileId, target.workflowId, startTimeoutMs);
      data.seen(execution);
      if (execution.state.status === "failed") setFailure(`${execution.id}: ${execution.state.reason}`);
      else onOpen(execution.id);
    } catch (error) { setFailure(errorText(error)); } finally { setStarting(false); }
  };

  return <div className="code-launch">
    <header className="code-hero">
      {settings.mascot && <Mascot scale={8} mood={starting ? "working" : "idle"} />}
      <div>
        <h1>{label}</h1>
        <p className="code-lede">{t("code.lede")}</p>
      </div>
    </header>

    <section className="code-block" aria-label={t("code.setup.title")}>
      <h2>{t("code.setup.title")}</h2>
      {template
        ? <Setup key={template.name} api={api} data={data} targets={targets} sessions={sessions} template={template} candidates={candidates} onTemplate={setPicked} onOpen={onOpen} />
        : <div className="code-absent" role="status">
          <Mark icon={PackageOpen} tone="muted" label={t(installed ? "code.absent.template" : "operate.node.notInstalled")} />
          <p>{installed ? fill(t("code.absent.templateDescription"), { template: settings.template }) : t("code.absent.moduleDescription")}</p>
          <div className="decision-actions">
            <Button variant="outline" size="sm" onClick={() => setView(installed ? "templates" : "settings")}>{t(installed ? "code.absent.templates" : "code.setup.moduleSettings")}</Button>
            <Button variant="ghost" size="sm" onClick={() => void data.reload()}>{t("operate.retry")}</Button>
          </div>
        </div>}
    </section>

    <section className="code-block" aria-label={t("code.folders.title")}>
      <h2>{t("code.folders.title")}{rows.length > 0 && <span className="operate-count">{rows.length}</span>}</h2>
      {failure && <div className="operate-error" role="alert"><strong>{t("code.setup.startFailed")}</strong><code className="operate-reason">{failure}</code></div>}
      {rows.length === 0 ? <p className="operate-empty">{t("code.folders.empty")}</p>
        : <ul className="code-folders">{rows.map((row) => <Folder key={`${row.target.profileId}/${row.target.workflowId}`} row={row} busy={starting} onStart={(target) => void start(target)} onOpen={onOpen} />)}</ul>}
    </section>
  </div>;
}
