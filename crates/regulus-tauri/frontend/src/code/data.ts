// What the Regulus Code view knows of the core and how it asks for it: the lines it talks
// over, the lists every part of the view reads, and the view's own name.
import { useCallback, useEffect, useState } from "react";
import type { Manifest, Profile, ProjectSummary } from "@/composition/model";
import { t } from "@/lib/i18n";
import type { OperateApi } from "@/operate/client";
import { errorText } from "@/operate/logic";
import type { Execution, ModuleSummary } from "@/operate/model";
import { useOperateSettings } from "@/operate/settings";
import { usePolling } from "@/operate/usePolling";
import type { TemplatesApi } from "@/templates/client";
import type { TemplateSummary } from "@/templates/model";
import type { CodeApi } from "./client";
import { useCodeSettings } from "./settings";

export interface CodeApis {
  code: CodeApi;
  operate: OperateApi;
  templates: TemplatesApi;
}

/** What the view knows of the core, and how to ask for it again. */
export interface CodeData {
  profiles: Profile[];
  manifests: Manifest[];
  templates: TemplateSummary[];
  modules: ModuleSummary[];
  executions: Execution[];
  /** Names of the secrets the core holds; `null` when it could not say. */
  secrets: string[] | null;
  projects: ProjectSummary[];
  /** Profiles, manifests, templates, secrets and projects again: after something changed them elsewhere. */
  reload: () => Promise<void>;
  /** Puts an execution in the list at once, before the next poll brings it. */
  seen: (execution: Execution) => void;
}

/** The view's name: the person's, config.toml's, or the one in the translation file. */
export const useCodeLabel = () => useCodeSettings((s) => s.label) || t("code.name");

export function useCodeData(api: CodeApis | null) {
  const refreshMs = useOperateSettings((s) => s.refreshMs);
  const [loaded, setLoaded] = useState<Pick<CodeData, "profiles" | "manifests" | "templates" | "secrets" | "projects"> | null>(null);
  const [modules, setModules] = useState<ModuleSummary[]>([]);
  const [executions, setExecutions] = useState<Execution[]>([]);
  const [error, setError] = useState<string | null>(null);
  const reload = useCallback(async () => {
    if (!api) return;
    try {
      // What an older core or an absent module cannot answer reads as nothing, not as a failure of the view.
      const [profiles, manifests, templates, secrets, projects] = await Promise.all([
        api.operate.profiles(), api.code.manifests().catch(() => [] as Manifest[]), api.templates.list().catch(() => [] as TemplateSummary[]),
        api.code.secrets().catch(() => null), api.code.projects().catch(() => [] as ProjectSummary[]),
      ]);
      setLoaded({ profiles, manifests, templates, secrets, projects });
      setError(null);
    } catch (failure) {
      setError(errorText(failure));
      setLoaded((before) => before ?? { profiles: [], manifests: [], templates: [], secrets: null, projects: [] });
    }
  }, [api]);
  useEffect(() => { setLoaded(null); void reload(); }, [reload]);
  const known = loaded?.profiles;
  usePolling(async (current) => {
    if (!api) return;
    try {
      const [listed, installed] = await Promise.all([api.operate.executions(null), api.operate.modules()]);
      if (!current()) return;
      setExecutions(listed);
      setModules(installed);
      // An execution of a profile not seen yet (set up from the terminal): read the profiles again.
      if (known && listed.some((execution) => !known.some((profile) => profile.id === execution.profile_id))) void reload();
    } catch (failure) { if (current()) setError(errorText(failure)); }
  }, refreshMs, [api, known, reload]);
  const seen = useCallback((execution: Execution) => setExecutions((before) => [...before.filter((item) => item.id !== execution.id), execution]), []);
  const data: CodeData | null = loaded ? { ...loaded, modules, executions, reload, seen } : null;
  return { data, error, dismiss: () => setError(null) };
}
