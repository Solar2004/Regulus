// Shapes the core serves for the Operate view, as `regulus-proto` and the modules
// define them (docs/composition-preview.md). Profiles and workflows come from
// `composition/model.ts`; this file only adds what runs.

export type ExecutionState =
  | { status: "starting" }
  | { status: "running" }
  | { status: "stopped" }
  | { status: "failed"; reason: string }
  | { status: "interrupted"; reason: string };

export interface DisabledNode {
  node_id: string;
  /** Required inputs with nothing usable behind them. */
  missing: string[];
}

export interface Execution {
  id: string;
  profile_id: string;
  workflow_id: string;
  project_id: string | null;
  state: ExecutionState;
  disabled?: DisabledNode[];
  /** The execution that started this one as a child (protocol 1.8). */
  parent?: string;
}

export type ModuleState =
  | { status: "active"; since: number }
  | { status: "idle"; last_used: number | null }
  | { status: "disabled"; missing: string[] }
  | { status: "broken"; reason: string }
  | { status: "incompatible"; requires: string; have: string }
  /** Not started on purpose, e.g. a local module of an untrusted project (proto 1.14). */
  | { status: "blocked"; reason: string };

export interface ModuleSummary {
  id: string;
  name: string;
  node_types: string[];
  workflow_id: string | null;
  state: ModuleState;
  /** A local module's project (proto 1.15). */
  project?: string;
}

export interface PortRef {
  node_id: string;
  port_id: string;
}

interface BusEventBase {
  seq: number;
  /** Milliseconds since the Unix epoch. */
  at: number;
  execution_id: string;
  project_id: string | null;
  /** The `emitted` event this one is about. */
  event: number;
  from: PortRef;
  capability: string;
  depth: number;
}

export type BusEvent = BusEventBase & (
  | { kind: "emitted"; value: unknown }
  | { kind: "delivered"; to: PortRef }
  | { kind: "failed"; to: PortRef; error: string }
  | { kind: "cut"; max_depth: number }
);

export interface EventPage {
  events: BusEvent[];
  /** Pass as `since` to follow. */
  next: number;
  /** Deliveries still queued: 0 means the bus is idle. */
  pending: number;
}

/** `agent.prompt` answer (module `agent`). */
export interface AgentTurn {
  text: string;
  stop_reason?: string;
}

/** One message of a channel, as `comm.history` returns it. */
export interface ChannelMessage {
  id: number;
  /** Seconds since the Unix epoch. */
  at?: number;
  from: string;
  to?: string[];
  text?: string;
  reply_to?: number;
  error?: string;
}

/** One participant of a channel, as `comm.participants` returns it. */
export interface Participant {
  name: string;
  node?: string;
  kind?: string;
  state?: string;
  queued?: number;
}

/** A decision waiting for a person (module `human`, `human.list`). */
export interface PendingDecision {
  id: string;
  /** `approval` or `review`. */
  kind: string;
  execution_id: string;
  node_id: string;
  value: unknown;
  prompt: string;
  /** JSON schema of the fields an approval fills in. */
  form?: { properties?: Record<string, { type?: string; description?: string }>; required?: string[] } | null;
  created_ms: number;
  deadline_ms?: number;
}

export interface HumanAnswered {
  id: string;
  decision: string;
  execution_id: string;
  node_id: string;
  port: string;
}
