export interface Port {
  id: string;
  name: string;
  capability: string;
  direction: "input" | "output";
  kind: "uses" | "then";
  required: boolean;
}

export interface CompositionNode {
  id: string;
  type: string;
  name: string;
  module_id: string | null;
  resource: string | null;
  ports: Port[];
  config: Record<string, string | number | boolean>;
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

export interface ModuleDefinition {
  id: string;
  name: string;
  node_types: string[];
  workflow_id: string | null;
}

export interface ProfileSummary {
  id: string;
  name: string;
  project_id: string | null;
}
