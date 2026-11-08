import { invoke, isTauri } from "@tauri-apps/api/core";
import { create } from "zustand";

// In the desktop app the shell finds or starts the core; in a browser the page talks to
// VITE_RPC_URL, or to the mock on the dev server (docs/04-client.md §4).
const settings = {
  path: import.meta.env.VITE_RPC_PATH || "/mock-rpc",
  url: import.meta.env.VITE_RPC_URL as string | undefined,
  protocol: "1.0",
  timeout: 5000,
};

/** A JSON-RPC error answer: `code` tells a missing module (-32601, -32002) from a refusal. */
export class RpcError extends Error {
  constructor(message: string, readonly code: number) { super(message); }
}

type Pending = { resolve: (value: unknown) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> };

export class RpcClient {
  private socket: WebSocket;
  private nextId = 0;
  private pending = new Map<number, Pending>();
  private opened: Promise<void>;
  /** Called once the socket closes, whoever closed it. */
  onClose: (() => void) | null = null;

  constructor(url: string) {
    this.socket = new WebSocket(url);
    this.opened = new Promise((resolve, reject) => {
      const timer = setTimeout(() => { reject(new Error("Connection timeout")); this.socket.close(); }, settings.timeout);
      this.socket.addEventListener("open", () => { clearTimeout(timer); resolve(); }, { once: true });
      this.socket.addEventListener("error", () => { clearTimeout(timer); reject(new Error("Connection failed")); }, { once: true });
      this.socket.addEventListener("close", () => { clearTimeout(timer); reject(new Error("Connection closed")); }, { once: true });
    });
    this.socket.addEventListener("message", (event) => {
      try {
        const response = JSON.parse(event.data);
        const pending = this.pending.get(response.id);
        if (!pending) return;
        clearTimeout(pending.timer);
        this.pending.delete(response.id);
        if (response.error) pending.reject(new RpcError(response.error.message, response.error.code));
        else pending.resolve(response.result);
      } catch { this.close(); }
    });
    this.socket.addEventListener("close", () => {
      for (const pending of this.pending.values()) {
        clearTimeout(pending.timer);
        pending.reject(new Error("Connection closed"));
      }
      this.pending.clear();
      this.onClose?.();
    });
  }

  get closed() { return this.socket.readyState === WebSocket.CLOSING || this.socket.readyState === WebSocket.CLOSED; }

  /** `timeout` in ms; agent turns take minutes, so their callers pass a longer one. */
  async request<T>(method: string, params: Record<string, unknown> = {}, timeout = settings.timeout): Promise<T> {
    await this.opened;
    if (this.socket.readyState !== WebSocket.OPEN) throw new Error("Connection closed");
    const id = ++this.nextId;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error("Request timeout")); }, timeout);
      this.pending.set(id, { resolve: (value) => resolve(value as T), reject, timer });
      this.socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    });
  }

  close() { this.socket.close(); }
}

/** Where a core listens, as the desktop shell reports it. */
type Endpoint = { url: string; token: string };

class Incompatible extends Error {}

const withToken = ({ url, token }: Endpoint) => `${url}?token=${encodeURIComponent(token)}`;
const reasonOf = (error: unknown) => error instanceof Error ? error.message : String(error);

/** Opens a socket and says `hello`; only a compatible core is kept. */
async function greet(url: string): Promise<RpcClient> {
  const client = new RpcClient(url);
  try {
    const hello = await client.request<{ compatible: boolean; core: string; proto: string }>("hello", { client: "regulus-gui", proto: settings.protocol });
    if (!hello.compatible) throw new Incompatible(`core ${hello.core} speaks protocol ${hello.proto}, this app speaks ${settings.protocol}`);
    return client;
  } catch (error) {
    client.close();
    throw error;
  }
}

export type CoreSource = "mock" | "core";

async function open(): Promise<{ client: RpcClient; source: CoreSource }> {
  if (!isTauri()) {
    const url = settings.url || `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}${settings.path}`;
    return { client: await greet(url), source: settings.url ? "core" : "mock" };
  }
  // A port left behind may belong to a core that is gone: only `hello` proves it is there.
  const known = await invoke<Endpoint | null>("core_endpoint");
  if (known) {
    try {
      return { client: await greet(withToken(known)), source: "core" };
    } catch (error) {
      if (error instanceof Incompatible) throw error;
    }
  }
  const started = await invoke<Endpoint>("start_core");
  return { client: await greet(withToken(started)), source: "core" };
}

export type CoreStatus = "loading" | "ready" | "offline" | "incompatible";

interface CoreState {
  status: CoreStatus;
  source: CoreSource;
  /** Why the last attempt failed, as the shell or the core said it. */
  reason: string | null;
  client: RpcClient | null;
  connect: () => void;
}

let attempts = 0;

/** The one connection every view shares. */
export const useCore = create<CoreState>()((set, get) => ({
  status: "loading",
  source: isTauri() || settings.url ? "core" : "mock",
  reason: null,
  client: null,
  connect: () => {
    const attempt = ++attempts;
    get().client?.close();
    set({ status: "loading", reason: null, client: null });
    open().then(({ client, source }) => {
      if (attempt !== attempts) { client.close(); return; }
      const lost = () => { if (attempt === attempts) set({ status: "offline", reason: null, client: null }); };
      client.onClose = lost;
      if (client.closed) lost();
      else set({ status: "ready", source, client });
    }, (error) => {
      if (attempt === attempts) set({ status: error instanceof Incompatible ? "incompatible" : "offline", reason: reasonOf(error), client: null });
    });
  },
}));
