import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { t } from "@/lib/i18n";
import { messageOf, type Requester } from "../editor/api";
import { useCanvasSettings } from "../editor/settings";
import type { NodeCreated, ProjectSummary } from "../model";
import { CODE, codeOf, initNode, listProjects, openProject } from "./api";

/** The id rule of `regulus node init` (the core checks it again). */
export const NODE_ID = /^[a-z0-9][a-z0-9_-]{0,63}$/;

/** Why `node.init` failed, by the core's code. */
function failureText(code: number | null): string {
  if (code === CODE.sourceConflict) return t("authoring.newNode.exists");
  if (code === CODE.methodNotFound) return t("authoring.newNode.unsupported");
  if (code === CODE.projectNotFound) return t("authoring.newNode.projectClosed");
  return t("authoring.newNode.failed");
}

/**
 * A new Python node module in an open project (`regulus node init`): the core writes
 * `tools/<id>/run.py`, its SDK copy and the `[[modules.local]]` entry, never over anything
 * that exists, and starts it only if the project is trusted. Without an open project (a new
 * home), a folder is opened here by its path (`project.open`, untrusted), so the journey
 * needs no command line.
 */
export function NewNodeDialog({ open, rpc, preferredProject, onClose, onCreated }: {
  open: boolean; rpc: Requester; preferredProject: string | null; onClose: () => void; onCreated: (created: NodeCreated) => void;
}) {
  const reloadMs = useCanvasSettings((s) => s.reloadTimeoutMs);
  const checkMs = useCanvasSettings((s) => s.checkTimeoutMs);
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [projectId, setProjectId] = useState("");
  const [id, setId] = useState("");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<{ text: string; detail: string } | null>(null);
  /** A folder to open as a project, while its row is shown. */
  const [folder, setFolder] = useState<string | null>(null);
  const [opening, setOpening] = useState(false);

  useEffect(() => {
    if (!open) return;
    let disposed = false;
    setProjects(null);
    setFailure(null);
    listProjects(rpc).then((listed) => {
      if (disposed) return;
      const usable = listed.filter((project) => !project.error);
      setProjects(usable);
      setProjectId((current) => usable.some((project) => project.id === current) ? current
        : usable.find((project) => project.id === preferredProject)?.id ?? usable[0]?.id ?? "");
    }, (error) => { if (!disposed) { setProjects([]); setFailure({ text: failureText(codeOf(error)), detail: messageOf(error) }); } });
    return () => { disposed = true; };
  }, [open, rpc, preferredProject]);

  const valid = NODE_ID.test(id);
  const close = () => { if (!busy && !opening) { setId(""); setFailure(null); setFolder(null); onClose(); } };
  // No project yet: the folder row is the way on.
  const folderShown = folder !== null || projects?.length === 0;

  /** Opens the typed folder as a project (untrusted) and picks it. */
  const openFolder = async () => {
    const path = (folder ?? "").trim();
    if (!path || opening) return;
    setOpening(true);
    setFailure(null);
    try {
      const opened = await openProject(rpc, path);
      const listed = (await listProjects(rpc)).filter((project) => !project.error);
      setProjects(listed);
      setProjectId(opened.id);
      setFolder(null);
    } catch (error) {
      setFailure({ text: t("authoring.newNode.openFailed"), detail: messageOf(error) });
    } finally {
      setOpening(false);
    }
  };
  const create = async () => {
    if (!valid || !projectId || busy) return;
    setBusy(true);
    setFailure(null);
    try {
      const created = await initNode(rpc, projectId, id, { reloadMs, checkMs });
      setId("");
      onCreated(created);
      onClose();
    } catch (error) {
      setFailure({ text: failureText(codeOf(error)), detail: messageOf(error) });
    } finally {
      setBusy(false);
    }
  };

  return <Dialog open={open} onOpenChange={(next) => { if (!next) close(); }}>
    {open && <DialogContent>
      <form className="editor-form" onSubmit={(event) => { event.preventDefault(); void create(); }}>
        <DialogHeader><DialogTitle>{t("authoring.newNode.title")}</DialogTitle><DialogDescription>{t("authoring.newNode.description")}</DialogDescription></DialogHeader>
        {projects && projects.length === 0 && !failure && <p className="field-error" role="alert">{t("authoring.newNode.noProjects")}</p>}
        <label><span>{t("authoring.newNode.project")}</span>
          <select value={projectId} disabled={!projects?.length} onChange={(event) => setProjectId(event.target.value)}>
            {projects === null && <option value="">{t("authoring.newNode.loadingProjects")}</option>}
            {projects?.map((project) => <option key={project.id} value={project.id}>{project.name || project.id} — {project.path}</option>)}
          </select>
        </label>
        {folderShown ? <label><span>{t("authoring.newNode.folder")}</span>
          <span className="authoring-folder">
            <input value={folder ?? ""} spellCheck={false} placeholder="/home/me/my-project" onChange={(event) => setFolder(event.target.value)}
              onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); void openFolder(); } }} />
            <Button type="button" variant="outline" size="sm" disabled={!(folder ?? "").trim() || opening} onClick={() => void openFolder()}>
              {t(opening ? "authoring.newNode.opening" : "authoring.newNode.openAction")}
            </Button>
          </span>
          <small className="form-hint">{t("authoring.newNode.folderHint")}</small>
        </label>
          : <Button type="button" variant="ghost" size="sm" className="authoring-folder-toggle" onClick={() => setFolder("")}>{t("authoring.newNode.openFolder")}</Button>}
        <label><span>{t("authoring.newNode.id")}</span>
          <input autoFocus value={id} spellCheck={false} aria-invalid={id && !valid ? true : undefined} onChange={(event) => setId(event.target.value)} placeholder="caps" />
        </label>
        {id && !valid ? <small className="field-error" role="alert">{t("authoring.newNode.idRule")}</small>
          : <small className="form-hint">{t("authoring.newNode.files", { id: id || "<id>" })}</small>}
        {failure && <p className="field-error" role="alert">{failure.text} <code>{failure.detail}</code></p>}
        <DialogFooter>
          <Button type="button" variant="outline" onClick={close} disabled={busy || opening}>{t("editor.cancel")}</Button>
          <Button type="submit" disabled={!valid || !projectId || busy}>{t(busy ? "authoring.newNode.creating" : "authoring.newNode.create")}</Button>
        </DialogFooter>
      </form>
    </DialogContent>}
  </Dialog>;
}
