/** The mock core's in-memory state: opaque to its callers. */
export type MockState = Record<string, unknown>;

export interface MockResponse {
  jsonrpc: "2.0";
  id: number | string | null;
  result?: unknown;
  error?: { code: number; message: string };
}

export function createState(): MockState;
/** One connection: takes a raw JSON-RPC request, gives its response (`null` for a notification). */
export function createSession(state?: MockState): (raw: string) => MockResponse | null;
