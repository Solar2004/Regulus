// The Templates view's calls: `regulus template ls|show|apply`.
import type { Rpc } from "@/operate/client";
import type { TemplateApplied, TemplateSummary, TemplateText } from "./model";

export const templateMethods = { list: "template.list", get: "template.get", apply: "template.apply" } as const;

export interface ApplyRequest {
  template: string;
  vars: Record<string, string>;
  /** Another profile id than the template's. */
  id?: string;
  /** Replace a profile that already exists. */
  force?: boolean;
}

export function templatesApi(rpc: Rpc) {
  return {
    list: () => rpc.request<TemplateSummary[]>(templateMethods.list),
    get: (template: string) => rpc.request<TemplateText>(templateMethods.get, { template }),
    apply: ({ template, vars, id, force }: ApplyRequest) => rpc.request<TemplateApplied>(templateMethods.apply, {
      template, vars, ...(id?.trim() ? { id: id.trim() } : {}), ...(force ? { force: true } : {}),
    }),
  };
}

export type TemplatesApi = ReturnType<typeof templatesApi>;
