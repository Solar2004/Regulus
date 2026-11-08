import { CircleCheck, CircleDashed, CircleStop, FileText, Loader, OctagonX, PackageOpen, TriangleAlert, type LucideIcon } from "lucide-react";
import { t, type LocaleKey } from "@/lib/i18n";
import type { ExecutionState } from "./model";
import { fill, type NodeState } from "./logic";

type Tone = "success" | "warning" | "danger" | "muted" | "info";

/** A state is an icon and a word, never a color alone (docs/08-design-system.md). */
export function Mark({ icon: Icon, tone, label, title }: { icon: LucideIcon; tone: Tone; label: string; title?: string }) {
  return <span className={`state-mark tone-${tone}`} title={title}><Icon size={14} strokeWidth={1.75} aria-hidden="true" />{label}</span>;
}

const nodeMeta: Record<NodeState["kind"], { icon: LucideIcon; tone: Tone; label: LocaleKey }> = {
  running: { icon: CircleCheck, tone: "success", label: "operate.node.running" },
  stopped: { icon: CircleStop, tone: "muted", label: "operate.node.stopped" },
  unused: { icon: CircleDashed, tone: "warning", label: "operate.node.unused" },
  resource: { icon: FileText, tone: "muted", label: "operate.node.resource" },
  "not-installed": { icon: PackageOpen, tone: "muted", label: "operate.node.notInstalled" },
  disabled: { icon: OctagonX, tone: "danger", label: "operate.node.disabled" },
  broken: { icon: TriangleAlert, tone: "danger", label: "operate.node.broken" },
  incompatible: { icon: TriangleAlert, tone: "danger", label: "operate.node.incompatible" },
};

/** Why a node is in its state, when there is a reason to say. */
export function nodeReason(state: NodeState): string | null {
  if (state.kind === "disabled") return fill(t("operate.node.missing"), { inputs: state.missing.join(", ") });
  if (state.kind === "broken") return state.reason;
  if (state.kind === "incompatible") return fill(t("operate.node.needsProtocol"), { requires: state.requires, have: state.have });
  return null;
}

export function NodeStateMark({ state }: { state: NodeState }) {
  const meta = nodeMeta[state.kind];
  return <Mark icon={meta.icon} tone={meta.tone} label={t(meta.label)} title={nodeReason(state) ?? undefined} />;
}

const executionMeta: Record<ExecutionState["status"], { icon: LucideIcon; tone: Tone; label: LocaleKey }> = {
  starting: { icon: Loader, tone: "info", label: "operate.execution.starting" },
  running: { icon: CircleCheck, tone: "success", label: "operate.execution.running" },
  stopped: { icon: CircleStop, tone: "muted", label: "operate.execution.stopped" },
  failed: { icon: TriangleAlert, tone: "danger", label: "operate.execution.failed" },
  interrupted: { icon: OctagonX, tone: "warning", label: "operate.execution.interrupted" },
};

export function ExecutionStateMark({ state }: { state: ExecutionState }) {
  const meta = executionMeta[state.status] ?? executionMeta.stopped;
  return <Mark icon={meta.icon} tone={meta.tone} label={t(meta.label)} title={"reason" in state ? state.reason : undefined} />;
}
