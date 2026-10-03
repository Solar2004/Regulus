import {
  CircleCheck,
  CircleDashed,
  OctagonX,
  PackageOpen,
  TriangleAlert,
} from "lucide-react"

import { cn } from "@/lib/utils"

// Module states from docs/04-client.md §4 + §12.4.
// Rule: status is always a bare Lucide SVG icon + text label.
// The icon itself never carries background, border, or border-radius.
// Color alone never identifies a state; the text label is mandatory.
export type ModuleStateKind =
  | "active"
  | "idle"
  | "not-installed"
  | "disabled"
  | "broken"

const STATUS_META: Record<
  ModuleStateKind,
  { icon: typeof CircleCheck; className: string }
> = {
  active: { icon: CircleCheck, className: "text-success" },
  idle: { icon: CircleDashed, className: "text-warning" },
  "not-installed": { icon: PackageOpen, className: "text-muted-foreground" },
  disabled: { icon: OctagonX, className: "text-danger" },
  broken: { icon: TriangleAlert, className: "text-danger" },
}

export function ModuleStatus({
  state,
  label,
  className,
}: {
  state: ModuleStateKind
  label: string
  className?: string
}) {
  const meta = STATUS_META[state]
  const Icon = meta.icon
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center gap-1.5 text-xs font-medium whitespace-nowrap",
        meta.className,
        className,
      )}
    >
      <Icon
        size={14}
        strokeWidth={1.75}
        aria-hidden="true"
        className="shrink-0"
      />
      {label}
    </span>
  )
}
