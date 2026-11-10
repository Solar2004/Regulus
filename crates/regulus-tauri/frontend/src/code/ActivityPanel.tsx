// What the agent did, without what was said: its tool calls, the permissions it asked for,
// how its turns ended. Read from the same record the conversation draws (`agent.history`):
// nothing is asked twice and nothing is kept here.
import { useMemo } from "react";
import { CircleCheck, CircleSlash, CircleStop, Clock, ShieldQuestion, TriangleAlert, Wrench, type LucideIcon } from "lucide-react";
import { t, type LocaleKey } from "@/lib/i18n";
import { WorkState } from "@/operate/Conversation";
import type { ConversationRecord } from "@/operate/conversation";
import { fill } from "@/operate/logic";
import { Mark } from "@/operate/StateMark";
import { activityOf, type ActivityItem } from "./logic";

type Tone = "success" | "warning" | "danger" | "muted" | "info";
const permissionMarks: Record<string, { icon: LucideIcon; tone: Tone; label: LocaleKey }> = {
  waiting: { icon: Clock, tone: "warning", label: "code.activity.waiting" },
  answered: { icon: CircleCheck, tone: "success", label: "code.activity.answered" },
  cancelled: { icon: CircleSlash, tone: "muted", label: "code.activity.cancelled" },
};
const icons: Record<ActivityItem["kind"], LucideIcon> = { tool: Wrench, permission: ShieldQuestion, ended: CircleStop, system: TriangleAlert };

function Row({ item }: { item: ActivityItem }) {
  const Icon = icons[item.kind];
  const permission = item.kind === "permission" && item.status ? permissionMarks[item.status] : undefined;
  return <li className={`code-activity-${item.kind}`}>
    <Icon size={13} strokeWidth={1.75} aria-hidden="true" />
    <span className="code-activity-what">
      {item.kind === "ended" ? fill(t("operate.conversation.turnEnded"), { reason: item.note ?? "—" })
        : item.kind === "permission" ? fill(t("operate.conversation.permission"), { title: item.title }) : item.title}
      {item.kind === "tool" && item.note && <small>{item.note}</small>}
    </span>
    {item.kind === "tool" && item.status && <WorkState status={item.status} />}
    {permission && <Mark icon={permission.icon} tone={permission.tone} label={item.status === "answered" && item.note ? fill(t("operate.conversation.permissionChosen"), { option: item.note }) : t(permission.label)} />}
    {item.at > 0 && <time>{new Date(item.at).toLocaleTimeString()}</time>}
  </li>;
}

export function ActivityPanel({ records }: { records: ConversationRecord[] }) {
  const items = useMemo(() => activityOf(records), [records]);
  if (items.length === 0) return <p className="operate-empty">{t("code.activity.empty")}</p>;
  return <ol className="code-activity" aria-label={t("code.panel.activity")}>{items.map((item) => <Row key={item.key} item={item} />)}</ol>;
}
