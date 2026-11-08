// Pure logic of the Templates view: the apply form and what `requires` finds installed.
// Tested in logic.test.ts.
import { findModule } from "@/operate/logic";
import type { ModuleSummary } from "@/operate/model";
import type { TemplateRequire, TemplateSummary, TemplateVar } from "./model";

export type FormValues = Record<string, string>;

export const isRequired = (variable: TemplateVar) => variable.default === undefined || variable.default === null;

/** The form as it opens: defaults filled in, required vars empty. */
export function initialValues(vars: TemplateVar[]): FormValues {
  return Object.fromEntries(vars.map((variable) => [variable.name, isRequired(variable) ? "" : String(variable.default)]));
}

/** Required vars still empty, in the template's order. */
export function missingVars(vars: TemplateVar[], values: FormValues): string[] {
  return vars.filter((variable) => isRequired(variable) && !(values[variable.name] ?? "").trim()).map((variable) => variable.name);
}

/** The `vars` of `template.apply`: what was typed; an emptied optional var takes its default in the core. */
export function varsToSend(vars: TemplateVar[], values: FormValues): Record<string, string> {
  const sent: Record<string, string> = {};
  for (const variable of vars) {
    const value = values[variable.name] ?? "";
    if (value.trim() || isRequired(variable)) sent[variable.name] = value;
  }
  return sent;
}

export type RequireState = "installed" | "missing" | "broken";

/** Whether a required module is there and usable; the version range is the core's to judge. */
export function requireState(require: TemplateRequire, modules: ModuleSummary[]): RequireState {
  const module = findModule(modules, require.module);
  if (!module) return "missing";
  return module.state?.status === "broken" || module.state?.status === "incompatible" ? "broken" : "installed";
}

export function canApply(template: TemplateSummary, values: FormValues): boolean {
  return !template.error && missingVars(template.vars, values).length === 0;
}

/** Template errors of the core (docs/composition-preview.md), by code. */
export const applyErrors = {
  [-32004]: "notFound",
  [-32005]: "invalid",
  [-32006]: "unmet",
  [-32007]: "exists",
  [-32602]: "params",
} as const;

export type ApplyError = (typeof applyErrors)[keyof typeof applyErrors] | "other";

export function applyErrorOf(error: unknown): ApplyError {
  const code = (error as { code?: unknown } | null)?.code;
  return typeof code === "number" && code in applyErrors ? applyErrors[code as keyof typeof applyErrors] : "other";
}
