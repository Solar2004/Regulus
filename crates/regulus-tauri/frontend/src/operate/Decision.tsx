// One decision waiting for a person (module `human`, `human.list`): approve or reject it,
// with the fields its form asks for. A field the form limits to some values is a select.
import { useState } from "react";
import { Check, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t, type LocaleKey } from "@/lib/i18n";
import type { OperateApi } from "./client";
import { errorText, fill } from "./logic";
import type { PendingDecision } from "./model";
import { ValueView } from "./ValueView";

const humanKinds: Record<string, LocaleKey> = { approval: "operate.human.kind.approval", review: "operate.human.kind.review", decision: "operate.human.kind.decision" };
/** A kind this app knows is translated; another one shows as the module names it. */
export const humanKind = (kind: string) => humanKinds[kind] ? t(humanKinds[kind]) : kind;

export function Decision({ api, pending, onDone }: { api: OperateApi; pending: PendingDecision; onDone: () => void }) {
  const fields = Object.entries(pending.form?.properties ?? {});
  const [values, setValues] = useState<Record<string, string>>({});
  const [text, setText] = useState(pending.kind === "review" && typeof pending.value === "string" ? pending.value : "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const answer = async (decision: "approve" | "reject") => {
    setBusy(true);
    try {
      const edited = pending.kind === "review" && text === pending.value ? null : text.trim() ? text : null;
      await api.answer(pending.id, decision, values, edited);
      onDone();
    } catch (failure) { setError(errorText(failure)); } finally { setBusy(false); }
  };
  const seconds = (ms: number) => Math.max(0, Math.round(ms / 1000));
  return <li className="decision">
    <header><strong>{pending.prompt || pending.id}</strong><small>{humanKind(pending.kind)} · {pending.node_id} · {fill(t("operate.human.age"), { seconds: seconds(Date.now() - pending.created_ms) })}{pending.deadline_ms ? ` · ${fill(t("operate.human.deadline"), { seconds: seconds(pending.deadline_ms - Date.now()) })}` : ""}</small></header>
    {pending.kind !== "review" && <ValueView value={pending.value} />}
    {fields.map(([name, field]) => <label key={name} className="operate-field"><span>{name}{pending.form?.required?.includes(name) && <em className="required"> {t("operate.required")}</em>}</span>
      {Array.isArray(field.enum)
        ? <select value={values[name] ?? ""} onChange={(e) => setValues({ ...values, [name]: e.target.value })}>
          <option value="">{t("operate.human.choose")}</option>
          {field.enum.map((option) => <option key={String(option)} value={String(option)}>{String(option)}</option>)}
        </select>
        : <input value={values[name] ?? ""} onChange={(e) => setValues({ ...values, [name]: e.target.value })} placeholder={field.type} />}
      {field.description && <small>{field.description}</small>}</label>)}
    <label className="operate-field"><span>{t(pending.kind === "review" ? "operate.human.edit" : "operate.human.comment")}</span>
      <textarea rows={pending.kind === "review" ? 5 : 2} value={text} onChange={(e) => setText(e.target.value)} /></label>
    {error && <p className="operate-error" role="alert">{error}</p>}
    <div className="decision-actions">
      <Button size="sm" disabled={busy} onClick={() => void answer("approve")}><Check size={14} />{t("operate.human.approve")}</Button>
      <Button size="sm" variant="outline" disabled={busy} onClick={() => void answer("reject")}><X size={14} />{t("operate.human.reject")}</Button>
    </div>
  </li>;
}
