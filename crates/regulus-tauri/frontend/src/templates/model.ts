// Template shapes as `regulus-proto` serves them (protocol 1.10).

export interface TemplateVar {
  name: string;
  description: string;
  /** Absent: the var is required. */
  default?: string | null;
}

export interface TemplateRequire {
  module: string;
  version: string;
}

export interface TemplateSummary {
  name: string;
  description: string;
  path: string;
  vars: TemplateVar[];
  requires: TemplateRequire[];
  /** The file could not be read as a template. */
  error?: string;
}

export interface TemplateText {
  name: string;
  path: string;
  text: string;
}

export interface Diagnostic {
  severity: "error" | "warning" | "info";
  code: string;
  message: string;
  workflow_id?: string | null;
  node_id?: string | null;
  wire_id?: string | null;
}

export interface TemplateApplied {
  profile_id: string;
  path: string;
  diagnostics: Diagnostic[];
}
