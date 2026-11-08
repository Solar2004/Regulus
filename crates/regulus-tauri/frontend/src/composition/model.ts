export interface Port {
  id: string;
  name: string;
  capability: string;
  direction: "input" | "output";
  kind: "uses" | "then";
  required: boolean;
  /** Absent means "one": the input takes a single wire. */
  cardinality?: "one" | "many";
  lazy?: boolean;
}

/** What a node config holds: any JSON value but `null` (TOML has none). */
export type ConfigValue = string | number | boolean | ConfigValue[] | { [key: string]: ConfigValue };

export interface CompositionNode {
  id: string;
  type: string;
  name: string;
  module_id: string | null;
  resource: string | null;
  ports: Port[];
  config: Record<string, ConfigValue>;
  position: { x: number; y: number };
  internal_workflow_id: string | null;
  exposed_parameters: string[];
}

export interface Wire {
  id: string;
  kind: "uses" | "then";
  from: { node_id: string; port_id: string };
  to: { node_id: string; port_id: string };
}

export interface Workflow {
  id: string;
  name: string;
  kind: "main" | "extra" | "module";
  module_id: string | null;
  /** Its own inputs and outputs (protocol 1.7); inner wires reach them as `self`. */
  ports?: Port[];
  nodes: CompositionNode[];
  wires: Wire[];
  layout: { x: number; y: number; width: number; height: number };
}

export interface Profile {
  id: string;
  name: string;
  project_id: string | null;
  extends: string | null;
  main_workflow_id: string;
  workflows: Workflow[];
}

export type ModuleState =
  | { status: "active"; since: number }
  | { status: "idle"; last_used: number | null }
  | { status: "disabled"; missing: string[] }
  | { status: "broken"; reason: string }
  | { status: "incompatible"; requires: string; have: string };

export interface ModuleDefinition {
  id: string;
  name: string;
  node_types: string[];
  workflow_id: string | null;
  /** Absent in the mock's example modules: they read as installed and idle. */
  state?: ModuleState;
}

/** A field of a node type, as `module.manifest` gives it (TOML names). */
export interface ManifestField {
  id: string;
  type: string;
  default?: ConfigValue | null;
  options?: ConfigValue[];
}

export interface ManifestNodeType {
  type: string;
  name: string;
  port?: Port[];
  field?: ManifestField[];
}

export interface Manifest {
  module: { id: string; name: string; requires_proto?: string | null; methods?: string[]; workflow_id?: string | null };
  node?: ManifestNodeType[];
}

export interface Diagnostic {
  severity: "error" | "warning" | "info";
  code: string;
  message: string;
  workflow_id: string | null;
  node_id: string | null;
  wire_id: string | null;
}

/** What `profile.save` and `template.apply` answer. */
export interface ProfileSaved {
  profile_id: string;
  path: string;
  diagnostics: Diagnostic[];
}

export interface Execution {
  id: string;
  profile_id: string;
  workflow_id: string;
  project_id: string | null;
  state: { status: string; reason?: string };
  disabled?: { node_id: string; missing: string[] }[];
}

export interface ProfileSummary {
  id: string;
  name: string;
  project_id: string | null;
}
