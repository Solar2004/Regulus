// "Edit in canvas": the boxes a coding session is made of, each a jump to that node in the
// canvas with its inspector open. The harness is a profile like any other: its engine,
// provider, tools and workers are changed there, not here.
import { Bot, Boxes, ChevronDown, Cpu, FolderCode, GitBranch, ShieldQuestion, Sparkles, Users, type LucideIcon } from "lucide-react";
import { buttonVariants } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { t, type LocaleKey } from "@/lib/i18n";
import { fill } from "@/operate/logic";
import { useRegulusUi } from "@/store/useRegulusUi";
import type { HarnessPart, HarnessRole } from "./logic";

const roles: Record<HarnessRole, { icon: LucideIcon; label: LocaleKey }> = {
  agent: { icon: Bot, label: "code.harness.agent" },
  engine: { icon: Cpu, label: "code.harness.engine" },
  model: { icon: Sparkles, label: "code.harness.model" },
  workspace: { icon: FolderCode, label: "code.harness.workspace" },
  decider: { icon: ShieldQuestion, label: "code.harness.decider" },
  workers: { icon: Users, label: "code.harness.workers" },
};

export function HarnessMenu({ profileId, workflowId, parts, live }: { profileId: string; workflowId: string; parts: HarnessPart[]; live: boolean }) {
  const openInCanvas = useRegulusUi((s) => s.openInCanvas);
  return <DropdownMenu>
    <DropdownMenuTrigger className={buttonVariants({ variant: "outline", size: "sm" })}><GitBranch size={14} />{t("code.harness.edit")}<ChevronDown size={13} /></DropdownMenuTrigger>
    <DropdownMenuContent align="end" className="code-harness-menu">
      <DropdownMenuGroup>
        <DropdownMenuLabel>{fill(t("code.harness.title"), { profile: profileId })}</DropdownMenuLabel>
        <DropdownMenuItem onClick={() => openInCanvas(profileId, { workflowId })}><Boxes size={14} />{t("code.harness.whole")}</DropdownMenuItem>
      </DropdownMenuGroup>
      {parts.length > 0 && <DropdownMenuSeparator />}
      <DropdownMenuGroup>
        {parts.map((part) => {
          const role = roles[part.role];
          return <DropdownMenuItem key={`${part.role}:${part.nodeId}`} onClick={() => openInCanvas(profileId, { workflowId, nodeId: part.nodeId })}>
            <role.icon size={14} /><span>{t(role.label)}</span><code>{part.name}</code>
          </DropdownMenuItem>;
        })}
      </DropdownMenuGroup>
      {live && <p className="code-harness-note">{t("code.harness.liveNote")}</p>}
    </DropdownMenuContent>
  </DropdownMenu>;
}
