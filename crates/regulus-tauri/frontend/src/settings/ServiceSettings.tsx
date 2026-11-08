import { Ban, CircleCheck, Copy, KeyRound, OctagonX, RotateCcw, TriangleAlert, type LucideIcon } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { ConfirmDialog, type Confirm } from "@/composition/editor/dialogs";
import { t, type LocaleKey } from "@/lib/i18n";
import { errorText, unavailableOf, type Unavailable } from "@/operate/logic";
import { NotAvailable } from "@/operate/NotAvailable";
import { Mark } from "@/operate/StateMark";
import type { SettingsApi } from "./client";
import {
  formatTime, isRevoked, lastValid, logQuery, logRows, parsePolicy, parseScope, policyFields, policyLimits, sortKeys, statusClass,
  type PolicyField, type StatusClass, type StatusFilter,
} from "./logic";
import type { KeyRecord, LogEntry, NewKey, ServiceEndpoint } from "./model";
import { useSettingsApi } from "./useSettingsApi";

type Notice = { tone: "success" | "danger"; text: string };
type Missing = { kind: Unavailable; reason: string };

const statusLook: Record<StatusClass, { icon: LucideIcon; tone: "success" | "warning" | "danger" }> = {
  ok: { icon: CircleCheck, tone: "success" },
  refused: { icon: OctagonX, tone: "warning" },
  failed: { icon: TriangleAlert, tone: "danger" },
};

/**
 * The `service` module from outside: keys, live endpoints and the request log. Whether the
 * module is there is known by how its methods answer (-32601, -32002), never by its id.
 */
export function ServiceSettings() {
  const api = useSettingsApi();
  const [missing, setMissing] = useState<Missing | null>(null);
  const [keys, setKeys] = useState<KeyRecord[] | null>(null);
  const [endpoints, setEndpoints] = useState<ServiceEndpoint[] | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  /** Keys shown in clear until each one is confirmed as saved: a second key never hides the first. */
  const [revealed, setRevealed] = useState<NewKey[]>([]);
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  /** Bumped after a key changes, so the log asks again too. */
  const [changes, setChanges] = useState(0);
  /** Only the latest load is kept: an older answer that comes late is dropped. */
  const latest = useRef(0);

  /** A failure of a module method: absent or unusable is a state, anything else a notice. */
  const failed = useCallback((error: unknown) => {
    const kind = unavailableOf(error);
    if (kind) setMissing({ kind, reason: errorText(error) });
    else setNotice({ tone: "danger", text: errorText(error) });
  }, []);

  const load = useCallback(async () => {
    if (!api) return;
    const asked = ++latest.current;
    try {
      const [listed, live] = await Promise.all([api.keys(), api.endpoints()]);
      if (asked !== latest.current) return;
      setKeys(sortKeys(listed)); setEndpoints(live); setMissing(null);
    } catch (error) { if (asked === latest.current) failed(error); }
  }, [api, failed]);
  useEffect(() => { setKeys(null); setEndpoints(null); setMissing(null); void load(); }, [load]);

  const revoke = (key: KeyRecord) => setConfirm({
    title: t("settings.service.revokeTitle", { name: key.name || key.id }),
    description: t("settings.service.revokeDescription"),
    action: t("settings.service.revoke"),
    destructive: true,
    run: async () => {
      if (!api) return;
      try { await api.revokeKey(key.id); setNotice({ tone: "success", text: t("settings.service.revoked", { name: key.name || key.id }) }); }
      catch (error) { failed(error); }
      setChanges((count) => count + 1);
      await load();
    },
  });

  const shown = revealed.map((created) => <Revealed key={created.id} created={created} onDone={() => setRevealed((list) => list.filter((item) => item.id !== created.id))} />);

  const heading = <div className="settings-row"><div><h2>{t("settings.service.title")}</h2><p>{t("settings.service.description")}</p></div></div>;
  if (!api) return <>{heading}<p className="settings-empty">{t("settings.needsCore")}</p></>;
  if (missing) return <>{heading}<NotAvailable kind={missing.kind} reason={missing.kind === "unavailable" ? missing.reason : undefined}
    description={t(missing.kind === "not-installed" ? "settings.service.notInstalled" : "settings.service.unavailable")} onRetry={() => void load()} />{shown}</>;

  return <>
    {heading}
    {notice && <p className={`settings-notice tone-${notice.tone}`} role={notice.tone === "danger" ? "alert" : "status"}>{notice.text}</p>}
    {shown}

    <section className="settings-block" aria-labelledby="service-keys">
      <header><h3 id="service-keys">{t("settings.service.keys")}</h3><Button variant="outline" size="sm" onClick={() => void load()}><RotateCcw size={14} />{t("settings.refresh")}</Button></header>
      {keys === null ? <p className="settings-empty" role="status">{t("graph.loading")}</p>
        : keys.length === 0 ? <p className="settings-empty">{t("settings.service.noKeys")}</p>
        : <div className="settings-table-wrap"><table className="settings-table">
          <thead><tr><th>{t("settings.service.keyName")}</th><th>{t("settings.service.hint")}</th><th>{t("settings.service.scope")}</th><th>{t("settings.service.limits")}</th><th>{t("settings.service.created")}</th><th>{t("settings.service.state")}</th><th><span className="sr-only">{t("settings.actions")}</span></th></tr></thead>
          <tbody>{keys.map((key) => <tr key={key.id} data-revoked={isRevoked(key) || undefined}>
            <td><span>{key.name || t("settings.service.unnamed")}</span><small>{key.id}</small></td>
            <td><code>{t("settings.service.hintShown", { hint: key.hint })}</code></td>
            <td>{key.scope.map((entry) => <code key={entry} className="settings-chip">{entry}</code>)}</td>
            <td>{policyLimits(key.policy).map(({ field, value }) => <small key={field}>{t("settings.policy.limit", { label: t(policyLabels[field]), value })}</small>)}{!policyLimits(key.policy).length && <small>{t("settings.service.noLimits")}</small>}</td>
            <td><small>{formatTime(key.created_ms)}</small></td>
            <td>{isRevoked(key)
              ? <Mark icon={Ban} tone="muted" label={t("settings.service.revokedState")} title={formatTime(key.revoked_ms!)} />
              : <Mark icon={KeyRound} tone="success" label={t("settings.service.live")} />}</td>
            <td>{!isRevoked(key) && <Button variant="ghost" size="sm" aria-label={t("settings.service.revokeNamed", { name: key.name || key.id })} onClick={() => revoke(key)}>{t("settings.service.revoke")}</Button>}</td>
          </tr>)}</tbody>
        </table></div>}
      <NewKeyForm api={api} onCreated={(created) => { setRevealed((list) => [...list, created]); setNotice(null); setChanges((count) => count + 1); void load(); }} onError={failed} />
    </section>

    <section className="settings-block" aria-labelledby="service-endpoints">
      <header><h3 id="service-endpoints">{t("settings.service.endpoints")}</h3></header>
      {endpoints === null ? <p className="settings-empty" role="status">{t("graph.loading")}</p>
        : endpoints.length === 0 ? <p className="settings-empty">{t("settings.service.noEndpoints")}</p>
        : <ul className="settings-list">{endpoints.map((endpoint) => <li key={`${endpoint.execution_id}/${endpoint.node_id}`}>
          <code>{endpoint.url}</code><span className="settings-subtle">{t("settings.service.endpointOf", { execution: endpoint.execution_id, node: endpoint.node_id })}</span>
        </li>)}</ul>}
    </section>

    <ServiceLog api={api} keys={keys ?? []} changes={changes} onError={failed} />
    <ConfirmDialog confirm={confirm} onClose={() => setConfirm(null)} />
  </>;
}

/** The key in clear, the one time the module gives it. */
function Revealed({ created, onDone }: { created: NewKey; onDone: () => void }) {
  const [copied, setCopied] = useState<"yes" | "failed" | null>(null);
  const copy = async () => {
    try { await navigator.clipboard.writeText(created.key); setCopied("yes"); }
    catch { setCopied("failed"); }
  };
  return <div className="settings-revealed">
    <p role="alert"><TriangleAlert size={14} strokeWidth={1.75} aria-hidden="true" />{t("settings.service.onceWarning", { name: created.name || created.id })}</p>
    <div className="settings-revealed-key">
      <input readOnly value={created.key} aria-label={t("settings.service.theKey")} onFocus={(event) => event.currentTarget.select()} />
      <Button size="sm" onClick={() => void copy()}><Copy size={14} />{t(copied === "yes" ? "settings.service.copied" : "settings.service.copy")}</Button>
    </div>
    {copied === "failed" && <small className="settings-problem">{t("settings.service.copyFailed")}</small>}
    <Button variant="outline" size="sm" onClick={onDone}>{t("settings.service.savedIt")}</Button>
  </div>;
}

const policyLabels: Record<PolicyField, LocaleKey> = {
  cooldown_ms: "settings.policy.cooldown_ms",
  requests: "settings.policy.requests",
  window_ms: "settings.policy.window_ms",
  concurrency: "settings.policy.concurrency",
  budget_requests: "settings.policy.budget_requests",
  budget_ms: "settings.policy.budget_ms",
};

function NewKeyForm({ api, onCreated, onError }: { api: SettingsApi; onCreated: (key: NewKey) => void; onError: (error: unknown) => void }) {
  const [name, setName] = useState("");
  const [scope, setScope] = useState("");
  const [policy, setPolicy] = useState<Partial<Record<PolicyField, string>>>({});
  const [busy, setBusy] = useState(false);
  const scopeParse = parseScope(scope);
  const policyParse = parsePolicy(policy);
  const scopeProblem = !scope.trim() ? null : !scopeParse.ok && scopeParse.problem === "invalid" ? t("settings.service.badScope", { entries: scopeParse.bad.join(", ") }) : null;
  const policyProblem = policyParse.ok ? null : policyParse.problem === "window" ? t("settings.policy.window") : t("settings.policy.number", { field: policyParse.field });

  const create = async () => {
    if (!scopeParse.ok || !policyParse.ok) return;
    setBusy(true);
    try {
      const created = await api.newKey(name.trim(), scopeParse.scope, policyParse.policy);
      setName(""); setScope(""); setPolicy({});
      onCreated(created);
    } catch (error) { onError(error); }
    finally { setBusy(false); }
  };

  return <form className="settings-form" onSubmit={(event) => { event.preventDefault(); void create(); }}>
    <h3>{t("settings.service.newKey")}</h3>
    <div className="settings-fields">
      <label className="settings-field"><span>{t("settings.service.keyName")}</span><input value={name} onChange={(event) => setName(event.target.value)} /></label>
      <label className="settings-field grow"><span>{t("settings.service.scope")}</span>
        <input value={scope} spellCheck={false} placeholder={t("settings.service.scopePlaceholder")} aria-invalid={scopeProblem ? true : undefined} onChange={(event) => setScope(event.target.value)} />
      </label>
      <Button type="submit" disabled={busy || !scopeParse.ok || !policyParse.ok}><KeyRound size={14} />{t("settings.service.create")}</Button>
    </div>
    <small className="settings-hint">{t("settings.service.scopeHint")}</small>
    {scopeProblem && <small className="settings-problem" role="alert">{scopeProblem}</small>}
    <details className="settings-policy">
      <summary>{t("settings.policy.title")}</summary>
      <p className="settings-hint">{t("settings.policy.description")}</p>
      <div className="settings-policy-fields">{policyFields.map((field) => <label key={field} className="settings-field">
        <span>{t(policyLabels[field])}</span>
        <input inputMode="numeric" value={policy[field] ?? ""} placeholder={t("settings.policy.placeholder")} aria-invalid={!policyParse.ok && policyParse.problem === "number" && policyParse.field === field ? true : undefined}
          onChange={(event) => setPolicy((current) => ({ ...current, [field]: event.target.value }))} />
      </label>)}</div>
      {policyProblem && <small className="settings-problem" role="alert">{policyProblem}</small>}
    </details>
  </form>;
}

const statusFilters: StatusFilter[] = ["all", "ok", "refused", "failed"];
/** How many rows `service.log` is asked for at first; the form changes it. */
const initialLast = "100";

function ServiceLog({ api, keys, changes, onError }: { api: SettingsApi; keys: KeyRecord[]; changes: number; onError: (error: unknown) => void }) {
  const [form, setForm] = useState({ last: initialLast, key: "", workflow: "" });
  const [status, setStatus] = useState<StatusFilter>("all");
  const [entries, setEntries] = useState<LogEntry[] | null>(null);
  const [busy, setBusy] = useState(false);
  const latest = useRef(0);
  const lastOk = lastValid(form.last);

  const query = useCallback(async (filters: typeof form) => {
    const asked = ++latest.current;
    setBusy(true);
    try { const found = await api.log(logQuery(filters)); if (asked === latest.current) setEntries(found); }
    catch (error) { if (asked === latest.current) onError(error); }
    finally { if (asked === latest.current) setBusy(false); }
  }, [api, onError]);
  // Asks again when a key changes, with the filters as they stand.
  const formNow = useRef(form);
  formNow.current = form;
  useEffect(() => { if (lastValid(formNow.current.last)) void query(formNow.current); }, [query, changes]);

  const rows = entries ? logRows(entries, status) : [];
  return <section className="settings-block" aria-labelledby="service-log">
    <header><h3 id="service-log">{t("settings.service.log")}</h3></header>
    <form className="settings-fields" onSubmit={(event) => { event.preventDefault(); if (lastOk) void query(form); }}>
      <label className="settings-field"><span>{t("settings.service.logKey")}</span>
        <select value={form.key} onChange={(event) => setForm({ ...form, key: event.target.value })}>
          <option value="">{t("settings.service.allKeys")}</option>
          {keys.map((key) => <option key={key.id} value={key.id}>{key.name ? t("settings.service.keyOption", { name: key.name, id: key.id }) : key.id}</option>)}
        </select>
      </label>
      <label className="settings-field"><span>{t("settings.service.logWorkflow")}</span><input value={form.workflow} spellCheck={false} onChange={(event) => setForm({ ...form, workflow: event.target.value })} /></label>
      <label className="settings-field"><span>{t("settings.service.logLast")}</span><input inputMode="numeric" value={form.last} aria-invalid={lastOk ? undefined : true} onChange={(event) => setForm({ ...form, last: event.target.value })} /></label>
      <label className="settings-field"><span>{t("settings.service.logStatus")}</span>
        <select value={status} onChange={(event) => setStatus(event.target.value as StatusFilter)}>{statusFilters.map((filter) => <option key={filter} value={filter}>{t(`settings.service.status.${filter}`)}</option>)}</select>
      </label>
      <Button type="submit" variant="outline" size="sm" disabled={busy || !lastOk}><RotateCcw size={14} />{t("settings.refresh")}</Button>
    </form>
    {!lastOk && <small className="settings-problem" role="alert">{t("settings.service.badLast")}</small>}
    {entries === null ? <p className="settings-empty" role="status">{t("graph.loading")}</p>
      : rows.length === 0 ? <p className="settings-empty">{t(entries.length ? "settings.service.noMatch" : "settings.service.noRequests")}</p>
      : <div className="settings-table-wrap"><table className="settings-table">
        <thead><tr><th>{t("settings.service.at")}</th><th>{t("settings.service.logKey")}</th><th>{t("settings.service.target")}</th><th>{t("settings.service.mode")}</th><th>{t("settings.service.status")}</th><th>{t("settings.service.ms")}</th><th>{t("settings.service.result")}</th></tr></thead>
        <tbody>{rows.map((entry, index) => {
          const look = statusLook[statusClass(entry.status)];
          return <tr key={`${entry.at_ms}-${index}`}>
            <td><small>{formatTime(entry.at_ms)}</small></td>
            <td>{entry.key_name || entry.key_id ? <><span>{entry.key_name || entry.key_id}</span>{entry.key_name && entry.key_id && <small>{entry.key_id}</small>}</> : <small>{t("settings.none")}</small>}</td>
            <td><code>{entry.project}/{entry.workflow ?? t("settings.none")}</code>{entry.execution_id && <small>{entry.execution_id}</small>}</td>
            <td><small>{entry.mode}</small></td>
            <td><Mark icon={look.icon} tone={look.tone} label={String(entry.status)} /></td>
            <td><small>{entry.ms}</small></td>
            <td className="settings-result">{entry.result}</td>
          </tr>;
        })}</tbody>
      </table></div>}
  </section>;
}
