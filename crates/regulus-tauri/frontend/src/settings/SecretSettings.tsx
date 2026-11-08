import { KeyRound, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { ConfirmDialog, type Confirm } from "@/composition/editor/dialogs";
import { t } from "@/lib/i18n";
import { errorText } from "@/operate/logic";
import { secretNameProblem, sortNames } from "./logic";
import { useSettingsApi } from "./useSettingsApi";

type Notice = { tone: "success" | "danger"; text: string };

/** Secret names, add and delete. Values go in and never come back: the core does not give them. */
export function SecretSettings() {
  const api = useSettingsApi();
  const [names, setNames] = useState<string[] | null>(null);
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [confirm, setConfirm] = useState<Confirm | null>(null);

  const load = useCallback(async () => {
    if (!api) return;
    try { setNames(sortNames(await api.secrets())); }
    catch (error) { setNotice({ tone: "danger", text: errorText(error) }); setNames((current) => current ?? []); }
  }, [api]);
  useEffect(() => { setNames(null); void load(); }, [load]);

  const trimmed = name.trim();
  const problem = secretNameProblem(trimmed);
  const replaces = !!names?.includes(trimmed);

  const save = async () => {
    if (!api || problem || !value) return;
    setBusy(true);
    try {
      await api.setSecret(trimmed, value);
      setNotice({ tone: "success", text: t(replaces ? "settings.secrets.replaced" : "settings.secrets.saved", { name: trimmed }) });
      setName(""); setValue("");
      await load();
    } catch (error) { setNotice({ tone: "danger", text: errorText(error) }); }
    finally { setBusy(false); setValue(""); }
  };

  const remove = (secret: string) => setConfirm({
    title: t("settings.secrets.deleteTitle", { name: secret }),
    description: t("settings.secrets.deleteDescription", { name: secret }),
    action: t("settings.secrets.delete"),
    destructive: true,
    run: async () => {
      if (!api) return;
      try { await api.deleteSecret(secret); setNotice({ tone: "success", text: t("settings.secrets.deleted", { name: secret }) }); }
      catch (error) { setNotice({ tone: "danger", text: errorText(error) }); }
      await load();
    },
  });

  return <>
    <div className="settings-row"><div><h2>{t("settings.secrets.title")}</h2><p>{t("settings.secrets.description")}</p></div></div>
    {!api ? <p className="settings-empty">{t("settings.needsCore")}</p> : <>
      {notice && <p className={`settings-notice tone-${notice.tone}`} role={notice.tone === "danger" ? "alert" : "status"}>{notice.text}</p>}
      {names === null ? <p className="settings-empty" role="status">{t("graph.loading")}</p>
        : names.length === 0 ? <p className="settings-empty">{t("settings.secrets.empty")}</p>
        : <ul className="settings-list" aria-label={t("settings.secrets.title")}>{names.map((secret) => <li key={secret}>
          <KeyRound size={14} strokeWidth={1.75} aria-hidden="true" /><code>{secret}</code><span className="settings-subtle">{t("settings.secrets.hidden")}</span>
          <Button variant="ghost" size="sm" aria-label={t("settings.secrets.deleteNamed", { name: secret })} onClick={() => remove(secret)}><Trash2 size={14} />{t("settings.secrets.delete")}</Button>
        </li>)}</ul>}
      <form className="settings-form" onSubmit={(event) => { event.preventDefault(); void save(); }}>
        <h3>{t("settings.secrets.add")}</h3>
        <div className="settings-fields">
          <label className="settings-field"><span>{t("settings.secrets.name")}</span>
            <input value={name} spellCheck={false} autoComplete="off" placeholder="anthropic-api-key" aria-invalid={trimmed && problem ? true : undefined} onChange={(event) => setName(event.target.value)} />
          </label>
          <label className="settings-field grow"><span>{t("settings.secrets.value")}</span>
            <input type="password" value={value} spellCheck={false} autoComplete="new-password" onChange={(event) => setValue(event.target.value)} />
          </label>
          <Button type="submit" disabled={busy || !!problem || !value}>{t(replaces ? "settings.secrets.replace" : "settings.secrets.save")}</Button>
        </div>
        {trimmed && problem === "invalid" && <small className="settings-problem" role="alert">{t("settings.secrets.invalidName")}</small>}
        {replaces && !problem && <small className="settings-hint">{t("settings.secrets.replaces", { name: trimmed })}</small>}
        <small className="settings-hint">{t("settings.secrets.reference", { name: trimmed || "<name>" })}</small>
      </form>
    </>}
    <ConfirmDialog confirm={confirm} onClose={() => setConfirm(null)} />
  </>;
}
