// Shapes the core and the `service` module serve for Settings, as `regulus-proto`
// (`secret.*`) and `crates/modules/service` define them. Secrets travel by name only.

/** Limits of one policy, in milliseconds or counts; 0 is no limit. */
export interface Policy {
  cooldown_ms: number;
  requests: number;
  window_ms: number;
  concurrency: number;
  budget_requests: number;
  budget_ms: number;
}

/** A key as `service.key.list` gives it: never the secret, only its first characters. */
export interface KeyRecord {
  id: string;
  name: string;
  /** `<project>[/<workflow>]`, `global` (a profile with no project) or `*`. */
  scope: string[];
  hint: string;
  created_ms: number;
  /** Absent or null while the key is live. */
  revoked_ms?: number | null;
  /** Absent when it sets no limit. */
  policy?: Partial<Policy>;
}

/** What `service.key.new` answers: the key in clear, this one time. */
export type NewKey = KeyRecord & { key: string };

/** One request in the service log. */
export interface LogEntry {
  at_ms: number;
  key_id?: string | null;
  key_name?: string | null;
  project: string;
  workflow?: string | null;
  /** `sync`, `async` or `job` (a poll). */
  mode: string;
  /** HTTP's status, the neutral language of every engine. */
  status: number;
  ms: number;
  result?: string;
  execution_id?: string | null;
}

/** Where a live engine listens. */
export interface ServiceEndpoint {
  url: string;
  execution_id: string;
  node_id: string;
}

/** `service.log` filters; each one left out is no filter. */
export interface LogQuery {
  last?: number;
  key?: string;
  workflow?: string;
}
