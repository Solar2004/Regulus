// The "Save as template" form of the canvas, as data: what `template.save` gets and what its
// errors mean (`regulus template save`, docs/04-client.md §9). Tested in template.test.ts.

/** One variable row of the form: an empty default makes the variable required. */
export interface TemplateVarRow {
  name: string;
  description: string;
  default: string;
}

export interface TemplateForm {
  name: string;
  description: string;
  /** Save only the workflow on screen (and the workflows its boxes place), as the main one. */
  onlyWorkflow: boolean;
  vars: TemplateVarRow[];
  /** Where to write it; empty: the first `[templates] paths` of the core. */
  path: string;
  force: boolean;
}

export interface TemplateSaveParams {
  name: string;
  description?: string;
  profile_id: string;
  workflow_id?: string;
  vars?: Record<string, { description: string; default?: string }>;
  path?: string;
  force?: boolean;
}

export const emptyTemplateForm = (name: string): TemplateForm => ({ name, description: "", onlyWorkflow: false, vars: [], path: "", force: false });

/** A var is written `{{name}}` in the template: a word, not starting with a digit. */
const VAR_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

export type TemplateFormProblem = { problem: "name" } | { problem: "varName" | "varDuplicate"; row: number };

/** The params of `template.save`, or the first thing in the form that stops it. */
export function templateSaveParams(profileId: string, workflowId: string | null, form: TemplateForm): { ok: true; params: TemplateSaveParams } | ({ ok: false } & TemplateFormProblem) {
  const name = form.name.trim();
  if (!name) return { ok: false, problem: "name" };
  const vars: NonNullable<TemplateSaveParams["vars"]> = {};
  for (const [row, item] of form.vars.entries()) {
    const varName = item.name.trim();
    if (!VAR_NAME.test(varName)) return { ok: false, problem: "varName", row };
    if (varName in vars) return { ok: false, problem: "varDuplicate", row };
    vars[varName] = { description: item.description.trim(), ...(item.default !== "" ? { default: item.default } : {}) };
  }
  const description = form.description.trim();
  const path = form.path.trim();
  return {
    ok: true,
    params: {
      name, profile_id: profileId,
      ...(description ? { description } : {}),
      ...(form.onlyWorkflow && workflowId ? { workflow_id: workflowId } : {}),
      ...(Object.keys(vars).length ? { vars } : {}),
      ...(path ? { path } : {}),
      ...(form.force ? { force: true } : {}),
    },
  };
}

/** `template.save` errors by code: malformed, already there, a core without the method. */
const saveErrors = { [-32005]: "invalid", [-32007]: "exists", [-32601]: "unsupported", [-32602]: "params" } as const;

export type TemplateSaveError = (typeof saveErrors)[keyof typeof saveErrors] | "other";

export function templateSaveErrorOf(error: unknown): TemplateSaveError {
  const code = (error as { code?: unknown } | null)?.code;
  return typeof code === "number" && code in saveErrors ? saveErrors[code as keyof typeof saveErrors] : "other";
}
