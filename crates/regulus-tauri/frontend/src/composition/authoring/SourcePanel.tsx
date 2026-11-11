import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { CircleAlert, CircleCheck, FileCode2, Info, ListChecks, RefreshCw, Save, ShieldCheck, TriangleAlert, X } from "lucide-react";
import { CodeEditor } from "@/components/code/CodeEditor";
import type { EditorHandle } from "@/components/code/setup";
import type { PlacedProblem } from "@/components/code/syntax";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import { messageOf, type Requester } from "../editor/api";
import { ConfirmDialog, type Confirm } from "../editor/dialogs";
import { useCanvasSettings } from "../editor/settings";
import type { ProjectReloaded, SourceCheck, SourceDiagnostic, SourceFile } from "../model";
import { CODE, checkSource, codeOf, conflictOf, getSource, saveSource } from "./api";
import { reloadSummary, type SourceRef } from "./logic";
import { outcomeText, reloadWithConsent, useStopConsent } from "./reload";
import { sourceKey, useAuthoring } from "./store";
import "./authoring.css";

/** `text` is what the editor starts with: the file, or the unsaved text kept from before. */
type Load = { state: "loading" } | { state: "ready"; file: SourceFile; text: string } | { state: "failed"; code: number | null; message: string };
type Feedback = { tone: "info" | "success" | "error"; text: string; detail?: string } | null;

const severityIcon = { error: CircleAlert, warning: TriangleAlert, info: Info } as const;

/** The core's problems that have a place, for the editor's gutter. */
const placed = (check: SourceCheck | null): PlacedProblem[] => (check?.diagnostics ?? [])
  .filter((d): d is SourceDiagnostic & { line: number } => typeof d.line === "number")
  .map((d) => ({ line: d.line, column: d.column, end_line: d.end_line, end_column: d.end_column, message: `${d.message} (${d.code})`, severity: d.severity }));

/** Why the file cannot be opened, by the core's code. */
function failureText(code: number | null): string {
  if (code === CODE.methodNotFound) return t("authoring.source.unsupported");
  if (code === CODE.sourceUnavailable) return t("authoring.source.unavailable");
  if (code === CODE.projectNotFound) return t("authoring.source.projectClosed");
  return t("authoring.source.openFailed");
}

/**
 * The Python file of one local module, edited in place. Typing stays in the editor; the
 * core checks the text without running it, saves it only over the revision that was read
 * (a newer file on disk is a conflict the person resolves), and reloads the module only
 * when asked, asking again before it would stop a run. Unsaved text is kept in the
 * authoring store, so leaving the view does not lose it, and another source waits until
 * the person saves or discards it.
 */
export function SourcePanel({ rpc, source, profileId, onClose, onReloaded, onTrust }: {
  rpc: Requester; source: SourceRef; profileId: string | null;
  onClose: () => void;
  /** After a reload: refresh modules and say what became stale in the open profile. */
  onReloaded: (result: ProjectReloaded) => void;
  /** Trusts the project folder (`project.trust`), so its modules may start. */
  onTrust: (path: string) => Promise<void>;
}) {
  const settings = useCanvasSettings();
  const timeouts = { reloadMs: settings.reloadTimeoutMs, checkMs: settings.checkTimeoutMs };
  const [load, setLoad] = useState<Load>({ state: "loading" });
  const editor = useRef<EditorHandle | null>(null);
  /** The text as last read from or written to the file, and its revision. */
  const base = useRef<{ text: string; revision: string | null }>({ text: "", revision: null });
  const [dirty, setDirty] = useState(false);
  const [check, setCheck] = useState<SourceCheck | null>(null);
  const [checking, setChecking] = useState(false);
  const [busy, setBusy] = useState<"save" | "reload" | null>(null);
  const [conflict, setConflict] = useState<{ revision: string | null } | null>(null);
  const [feedback, setFeedback] = useState<Feedback>(null);
  const [blockedPath, setBlockedPath] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  const [askStop, stopDialog] = useStopConsent();
  const checkTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const checkSeq = useRef(0);
  const key = sourceKey(source);
  const pending = useAuthoring((s) => s.pending);
  const kept = useAuthoring((s) => s.draft?.key === key);
  /** False once the panel is gone: the editor still hands over its last text, nothing else runs. */
  const alive = useRef(true);

  const open = useCallback(async () => {
    setLoad({ state: "loading" });
    try {
      const file = await getSource(rpc, source.projectId, source.module);
      const draft = useAuthoring.getState().draft;
      if (draft?.key === key) {
        // Unsaved text from before (another view, a source asked for meanwhile): it comes back
        // over the file it was edited from; a newer file on disk is a conflict to resolve.
        base.current = draft.base;
        setLoad({ state: "ready", file, text: draft.text });
        setDirty(true);
        setConflict(file.revision === draft.base.revision ? null : { revision: file.revision });
        setFeedback({ tone: "info", text: t("authoring.source.restored") });
      } else {
        base.current = { text: file.text, revision: file.revision };
        setLoad({ state: "ready", file, text: file.text });
        setDirty(false);
        setConflict(null);
      }
    } catch (error) {
      setLoad({ state: "failed", code: codeOf(error), message: messageOf(error) });
    }
  }, [rpc, source.projectId, source.module, key]);
  useEffect(() => { void open(); }, [open]);
  useEffect(() => { alive.current = true; return () => { alive.current = false; if (checkTimer.current) clearTimeout(checkTimer.current); }; }, []);

  const text = () => editor.current?.text() ?? base.current.text;
  /** Writes `body` as the unsaved text of this source, or forgets it once it is the file's. */
  const keep = (body: string) => useAuthoring.getState().keepDraft(key, body === base.current.text ? null
    : { path: load.state === "ready" ? load.file.path : source.path, text: body, base: base.current });

  /** Asks the core to check `body` (the editor's text); an answer older than the last question is dropped. */
  const runCheck = useCallback(async (body: string) => {
    const seq = ++checkSeq.current;
    setChecking(true);
    try {
      const found = await checkSource(rpc, source.projectId, source.module, body, timeouts);
      if (seq === checkSeq.current) setCheck(found);
    } catch (error) {
      if (seq === checkSeq.current) setFeedback({ tone: "error", text: t("authoring.source.checkFailed"), detail: messageOf(error) });
    } finally {
      if (seq === checkSeq.current) setChecking(false);
    }
  }, [rpc, source.projectId, source.module, timeouts.checkMs]);

  // Typing paused (or the panel is going): keep the text, say whether there is something
  // to save and, if set, let the core check it.
  const onCommit = (body: string) => {
    keep(body);
    if (!alive.current) return;
    setDirty(body !== base.current.text);
    if (checkTimer.current) clearTimeout(checkTimer.current);
    if (settings.sourceCheckMs > 0) checkTimer.current = setTimeout(() => void runCheck(body), settings.sourceCheckMs);
  };
  // The first edit after a commit: there is something to save from now on.
  const onPending = () => { setDirty(true); keep(text()); };

  /** Writes the editor's text; `force` writes over a newer file. True when it was written. */
  const save = async (force = false): Promise<boolean> => {
    if (load.state !== "ready" || busy === "save") return false;
    const body = text();
    setBusy("save");
    setFeedback(null);
    try {
      const saved = await saveSource(rpc, { projectId: source.projectId, module: source.module, text: body, revision: base.current.revision, force }, timeouts);
      base.current = { text: body, revision: saved.revision };
      // What was typed while saving stays unsaved, over the file just written (also when the panel is gone).
      const now = alive.current ? text() : useAuthoring.getState().draft?.key === key ? useAuthoring.getState().draft!.text : body;
      keep(now);
      setDirty(now !== body);
      setConflict(null);
      setCheck(saved.check);
      checkSeq.current++;
      setChecking(false);
      const errors = saved.check.diagnostics.filter((d) => d.severity === "error").length;
      setFeedback(errors ? { tone: "error", text: t("authoring.source.savedWithErrors", { count: errors }), detail: saved.path }
        : { tone: "success", text: t("authoring.source.saved"), detail: saved.path });
      return true;
    } catch (error) {
      const stale = conflictOf(error);
      if (stale) { setConflict({ revision: stale.revision }); return false; }
      setFeedback({ tone: "error", text: t("authoring.source.saveFailed"), detail: messageOf(error) });
      return false;
    } finally {
      setBusy(null);
    }
  };

  const reload = async () => {
    setBusy("reload");
    setBlockedPath(null);
    try {
      const result = await reloadWithConsent(rpc, { id: source.projectId, module: source.module }, timeouts, askStop);
      if (!result) { setFeedback({ tone: "info", text: t("authoring.reload.keptRuns") }); return; }
      const summary = reloadSummary(result, profileId);
      const mine = result.modules.find((item) => item.local_id === source.module) ?? result.modules[0];
      if (summary.blocked.length) setBlockedPath(result.project.path);
      setFeedback({
        tone: summary.ok && !summary.blocked.length ? "success" : summary.ok ? "info" : "error",
        text: mine ? outcomeText(mine) : t("authoring.reload.done"),
        detail: [
          result.stopped_executions.length ? t("authoring.reload.stopped", { runs: result.stopped_executions.join(", ") }) : "",
          summary.stale.length ? t("authoring.reload.stale", { count: summary.stale.length }) : "",
        ].filter(Boolean).join(" · ") || undefined,
      });
      onReloaded(result);
    } catch (error) {
      setFeedback({ tone: "error", text: codeOf(error) === CODE.methodNotFound ? t("authoring.reload.unsupported") : t("authoring.reload.failed"), detail: messageOf(error) });
    } finally {
      setBusy(null);
    }
  };

  const saveAndReload = async () => { if (await save()) await reload(); };

  /** Drops the edits and shows the file as it is on disk now. */
  const loadTheirs = async () => {
    try {
      const file = await getSource(rpc, source.projectId, source.module);
      base.current = { text: file.text, revision: file.revision };
      // A new value from outside: the editor shows it without counting it as an edit (Ctrl+Z brings mine back).
      setLoad({ state: "ready", file, text: file.text });
      keep(file.text);
      setDirty(false);
      setConflict(null);
      setFeedback({ tone: "info", text: t("authoring.conflict.loaded") });
    } catch (error) {
      setFeedback({ tone: "error", text: t("authoring.source.openFailed"), detail: messageOf(error) });
    }
  };

  const trust = async () => {
    if (!blockedPath) return;
    try { await onTrust(blockedPath); setBlockedPath(null); await reload(); }
    catch (error) { setFeedback({ tone: "error", text: t("authoring.reload.trustFailed"), detail: messageOf(error) }); }
  };

  const close = () => {
    if (!dirty && !kept) { onClose(); return; }
    setConfirm({ title: t("authoring.source.discardTitle"), description: t("authoring.source.discard"), action: t("authoring.source.discardAction"), destructive: true, run: onClose });
  };

  /** Another source was asked for while this one has unsaved text: save it first, drop it, or stay. */
  const openPending = async (save_: boolean) => {
    if (save_ && !(await save())) return;
    useAuthoring.getState().resolvePending(true);
  };

  const problems = check?.diagnostics ?? [];
  // One array per check, so the editor relints only when the core said something new.
  const gutter = useMemo(() => placed(check), [check]);
  const errors = problems.filter((d) => d.severity === "error").length;
  return <aside className="source-panel" aria-label={t("authoring.source.title")}>
    <header className="source-head">
      <div className="source-title">
        <FileCode2 size={16} aria-hidden="true" />
        <strong title={load.state === "ready" ? load.file.file : undefined}>{load.state === "ready" ? load.file.path : source.path}</strong>
        <code>{source.moduleId}</code>
        <span className={`source-state${dirty ? " is-dirty" : ""}`}>{t(conflict ? "authoring.source.conflict" : dirty ? "authoring.source.unsaved" : "authoring.source.clean")}</span>
      </div>
      <div className="source-actions">
        <Button variant="outline" size="sm" disabled={load.state !== "ready" || checking} onClick={() => void runCheck(text())}><ListChecks size={14} />{t(checking ? "authoring.source.checking" : "authoring.source.check")}</Button>
        <Button size="sm" disabled={load.state !== "ready" || !!busy || (!dirty && !conflict)} onClick={() => void save()} title={t("authoring.source.saveHint")}><Save size={14} />{t(busy === "save" ? "authoring.source.saving" : "authoring.source.save")}</Button>
        <Button variant="outline" size="sm" disabled={load.state !== "ready" || !!busy} onClick={() => void (dirty ? saveAndReload() : reload())} title={t("authoring.source.reloadHint")}>
          <RefreshCw size={14} />{t(busy === "reload" ? "authoring.source.reloading" : dirty ? "authoring.source.saveReload" : "authoring.source.reload")}
        </Button>
        <Button variant="ghost" size="icon-sm" onClick={close} aria-label={t("authoring.source.close")}><X size={14} /></Button>
      </div>
    </header>
    {pending && <div className="source-banner tone-warning" role="alert">
      <TriangleAlert size={14} aria-hidden="true" />
      <span>{t("authoring.source.switchAsk", { path: pending.path })}</span>
      <Button size="xs" disabled={load.state !== "ready" || !!busy} onClick={() => void openPending(true)}>{t("authoring.source.switchSave")}</Button>
      <Button variant="destructive" size="xs" onClick={() => void openPending(false)}>{t("authoring.source.switchDiscard")}</Button>
      <Button variant="ghost" size="xs" onClick={() => useAuthoring.getState().resolvePending(false)}>{t("authoring.source.switchStay")}</Button>
    </div>}
    {conflict && <div className="source-banner tone-warning" role="alert">
      <TriangleAlert size={14} aria-hidden="true" />
      <span>{t(conflict.revision === null ? "authoring.conflict.gone" : "authoring.conflict.changed")}</span>
      <Button variant="outline" size="xs" onClick={() => void loadTheirs()}>{t("authoring.conflict.loadTheirs")}</Button>
      <Button variant="destructive" size="xs" onClick={() => void save(true)}>{t("authoring.conflict.overwrite")}</Button>
      <Button variant="ghost" size="xs" onClick={() => setConflict(null)}>{t("authoring.conflict.keep")}</Button>
    </div>}
    {feedback && <div className={`source-banner tone-${feedback.tone}`} role={feedback.tone === "error" ? "alert" : "status"}>
      {feedback.tone === "success" ? <CircleCheck size={14} aria-hidden="true" /> : feedback.tone === "error" ? <CircleAlert size={14} aria-hidden="true" /> : <Info size={14} aria-hidden="true" />}
      <span>{feedback.text}</span>{feedback.detail && <code>{feedback.detail}</code>}
      {blockedPath && <Button variant="outline" size="xs" onClick={() => void trust()}><ShieldCheck size={12} />{t("authoring.reload.trust")}</Button>}
      <button type="button" className="source-dismiss" onClick={() => { setFeedback(null); setBlockedPath(null); }} aria-label={t("editor.dismiss")}><X size={12} /></button>
    </div>}
    <div className="source-body">
      {load.state === "loading" && <p className="source-empty" role="status">{t("authoring.source.loading")}</p>}
      {load.state === "failed" && <div className="source-empty" role="alert"><p>{failureText(load.code)}</p><code>{load.message}</code>
        <Button variant="outline" size="sm" onClick={() => void open()}>{t("graph.retry")}</Button></div>}
      {load.state === "ready" && <CodeEditor key={`${load.file.project_id}/${load.file.module}`} value={load.text} language={load.file.language} indent={" ".repeat(settings.sourceIndent)}
        label={t("authoring.source.editorLabel", { path: load.file.path })} className="fill" commitDelayMs={settings.codeCommitMs} lintDelayMs={settings.codeLintMs}
        onCommit={onCommit} onPending={onPending} onSave={() => void save()} problems={gutter} flushWithProfile={false}
        onReady={(handle) => { editor.current = handle; }}
        fallback={() => <p className="source-empty" role="alert">{t("authoring.source.editorFailed")}</p>} />}
    </div>
    <section className="source-report" aria-label={t("authoring.source.report")}>
      <p className="source-checked">{check
        ? check.checked
          ? t(errors ? "authoring.check.problems" : "authoring.check.clean", { count: problems.length, interpreter: check.interpreter ?? "" })
          : t("authoring.check.unavailable")
        : t(settings.sourceCheckMs > 0 ? "authoring.check.pendingAuto" : "authoring.check.pending")}</p>
      {problems.length > 0 && <ul className="source-problems">{problems.map((d, index) => {
        const Icon = severityIcon[d.severity];
        return <li key={index}><button type="button" disabled={d.line === null} onClick={() => { if (d.line !== null) editor.current?.goTo(d.line, d.column ?? 1); }}>
          <Icon size={12} aria-hidden="true" className={`tone-${d.severity}`} />
          <span>{d.message}</span><code>{d.line !== null ? `${d.line}:${d.column ?? 1} · ` : ""}{d.code}</code>
        </button></li>;
      })}</ul>}
      {check && check.nodes.length > 0 && <div className="source-nodes">
        <h3>{t(check.complete ? "authoring.check.nodes" : "authoring.check.nodesPartial")}</h3>
        <ul>{check.nodes.map((node) => <li key={node.type}>
          <strong>{node.name || node.type}</strong> <code>{node.type}</code>
          <small>{[...(node.port ?? []).map((port) => `${port.direction === "input" ? "→" : "←"} ${port.id}`),
            ...(node.dynamic_port ?? []).map((port) => `${port.direction === "input" ? "→" : "←"} ${port.id}${port.repeat ? ` ×${port.repeat}` : ""}${port.when ? ` ?${port.when.field}` : ""}`)].join("  ")}</small>
        </li>)}</ul>
        {check.matches_loaded === false && <p className="source-hint">{t("authoring.check.reloadToApply")}</p>}
      </div>}
    </section>
    {stopDialog}
    <ConfirmDialog confirm={confirm} onClose={() => setConfirm(null)} />
  </aside>;
}
