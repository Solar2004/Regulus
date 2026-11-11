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
  | { status: "incompatible"; requires: string; have: string }
  /** Not started on purpose, e.g. a local module of an untrusted project (proto 1.14). */
  | { status: "blocked"; reason: string };

/** One entry of `project.list` (proto 1.12): the folder is what `project.trust` takes. */
export interface ProjectSummary {
  id: string;
  name: string;
  path: string;
  warnings?: string[];
  error?: string;
}

export interface ModuleDefinition {
  id: string;
  name: string;
  node_types: string[];
  workflow_id: string | null;
  /** Absent in the mock's example modules: they read as installed and idle. */
  state?: ModuleState;
  /** A local module's project (`[[modules.local]]`, proto 1.15): what to trust when it is blocked. */
  project?: string;
}

/** Where each `[[modules.local]]` of a project keeps its editable Python file, or why it has none (`project.get` → `sources`, proto 1.17). */
export interface ProjectSource {
  module: string;
  path: string | null;
  error: string | null;
}

/** A field of a node type, as `module.manifest` gives it (TOML names). */
export interface ManifestField {
  id: string;
  type: string;
  default?: ConfigValue | null;
  options?: ConfigValue[];
  // Optional presentation hints a module may add; the core passes them through unread
  // (`Field.extra`), and a key this app does not know is simply not used.
  /** Draw a text value as several lines. */
  multiline?: boolean;
  /** Language of a `code` field, shown next to its type (no highlighting yet). */
  language?: string;
  /** A human name for the field; the id stays visible next to it. */
  label?: string;
  /** One line of help shown under the control. */
  description?: string;
  /** What Tab inserts in a `code` field: `"tab"` or a number of spaces (default 4). */
  indent?: number | "tab";
  /** Shown only while the condition holds; the value is kept when hidden (proto 1.17). */
  when?: ShapeCondition;
}

/** When a manifest port or field exists: a field's value equals, is in a list, or is truthy. Only the core reads it (`module.shape`). */
export interface ShapeCondition {
  field: string;
  equals?: ConfigValue;
  in?: ConfigValue[];
}

/** A `dynamic_port` of a manifest: a port that exists under a condition, or once per item of a list field. */
export interface ManifestPort extends Port {
  when?: ShapeCondition;
  /** One port per item of this `list` field; `id` and `name` hold `{item}`. */
  repeat?: string;
}

export interface ManifestNodeType {
  type: string;
  name: string;
  port?: Port[];
  /** Ports that depend on the config; only the core resolves them (`module.shape`, proto 1.17). */
  dynamic_port?: ManifestPort[];
  field?: ManifestField[];
  /** Every node of this type has exactly the ports `module.shape` gives; a difference is stale (proto 1.17). */
  managed_ports?: boolean;
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

/** `module.shape`: the concrete ports of a node type for a config, from the core's one resolver (proto 1.17). */
export interface NodeShape {
  ports: Port[];
  hidden_fields: string[];
  problems: string[];
}

export interface SyncedNode {
  workflow_id: string;
  node_id: string;
  module_id: string | null;
  node_type: string;
  added: Port[];
  removed: Port[];
  changed: { before: Port; after: Port }[];
  wires_removed: { wire: Wire; reason: string }[];
  problems: string[];
}

/** `profile.sync`: a preview, never written. */
export interface ProfileSynced {
  profile: Profile;
  drops_wires: boolean;
  nodes: SyncedNode[];
}

export type ReloadOutcome = "started" | "replaced" | "kept" | "failed" | "blocked" | "removed" | "skipped";

export interface ReloadedModule {
  local_id: string;
  id: string | null;
  previous: string | null;
  outcome: ReloadOutcome;
  state: ModuleState | null;
  error: string | null;
  node_types?: { added: string[]; removed: string[]; changed: string[] };
}

/** `project.reload`. */
export interface ProjectReloaded {
  project: ProjectSummary;
  modules: ReloadedModule[];
  stopped_executions: string[];
  stale: { profile_id: string; workflow_id: string; node_id: string }[];
}

/** The live executions that keep `project.reload` from swapping a module (`-32014`'s `data`). */
export interface ModuleInUse {
  executions: { id: string; profile_id: string; workflow_id: string; nodes: string[] }[];
}

/** `source.get`. */
export interface SourceFile {
  project_id: string;
  module: string;
  path: string;
  file: string;
  language: string;
  text: string;
  revision: string;
}

export interface SourceDiagnostic {
  severity: "error" | "warning" | "info";
  code: string;
  message: string;
  line: number | null;
  column: number | null;
  end_line?: number | null;
  end_column?: number | null;
}

/** `source.check` and the check `source.save` returns: read statically, nothing imported or run. */
export interface SourceCheck {
  language: string;
  checked: boolean;
  interpreter: string | null;
  diagnostics: SourceDiagnostic[];
  nodes: ManifestNodeType[];
  complete: boolean;
  matches_loaded: boolean | null;
}

export interface SourceSaved {
  path: string;
  file: string;
  revision: string;
  check: SourceCheck;
}

/** `node.init`: a new Python node module in a project. */
export interface NodeCreated {
  project_id: string;
  module_id: string;
  node_type: string;
  source: string;
  files: string[];
  rewritten: boolean;
  notes: string[];
  module: ReloadedModule;
}
