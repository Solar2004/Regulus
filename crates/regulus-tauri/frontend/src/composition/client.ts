import { useEffect, useState } from "react";
import type { ModuleDefinition, Profile, ProfileSummary } from "./model";

const connection = {
  path: import.meta.env.VITE_RPC_PATH || "/mock-rpc",
  url: import.meta.env.VITE_RPC_URL,
  protocol: "1.0",
  timeout: 5000,
};

type Pending = { resolve: (value: unknown) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> };

export class CompositionClient {
  private socket: WebSocket;
  private nextId = 0;
  private pending = new Map<number, Pending>();
  private opened: Promise<void>;

  constructor(onDisconnect: () => void) {
    const url = connection.url || `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}${connection.path}`;
    this.socket = new WebSocket(url);
    this.opened = new Promise((resolve, reject) => {
      const timer = setTimeout(() => { reject(new Error("Connection timeout")); this.socket.close(); }, connection.timeout);
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
        if (response.error) pending.reject(new Error(response.error.message));
        else pending.resolve(response.result);
      } catch { this.close(); }
    });
    this.socket.addEventListener("close", () => {
      for (const pending of this.pending.values()) {
        clearTimeout(pending.timer);
        pending.reject(new Error("Connection closed"));
      }
      this.pending.clear();
      onDisconnect();
    });
  }

  async request<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    await this.opened;
    if (this.socket.readyState !== WebSocket.OPEN) throw new Error("Connection closed");
    const id = ++this.nextId;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error("Request timeout")); }, connection.timeout);
      this.pending.set(id, { resolve: (value) => resolve(value as T), reject, timer });
      this.socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    });
  }

  close() { this.socket.close(); }
}

export function useComposition() {
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<{
    status: "loading" | "ready" | "offline" | "incompatible";
    profiles: Profile[];
    modules: ModuleDefinition[];
  }>({ status: "loading", profiles: [], modules: [] });

  useEffect(() => {
    let disposed = false;
    setState({ status: "loading", profiles: [], modules: [] });
    const client = new CompositionClient(() => {
      if (!disposed) setState((previous) => ({ ...previous, status: previous.status === "incompatible" ? "incompatible" : "offline" }));
    });
    async function load() {
      try {
        const hello = await client.request<{ compatible: boolean }>("hello", { client: "regulus-gui", proto: connection.protocol });
        if (!hello.compatible) {
          if (!disposed) setState({ status: "incompatible", profiles: [], modules: [] });
          client.close();
          return;
        }
        const [summaries, modules] = await Promise.all([
          client.request<ProfileSummary[]>("profile.list"),
          client.request<ModuleDefinition[]>("module.list"),
        ]);
        const profiles = await Promise.all(summaries.map(({ id }) => client.request<Profile>("profile.get", { id })));
        if (!disposed) setState({ status: "ready", profiles, modules });
      } catch {
        if (!disposed) setState({ status: "offline", profiles: [], modules: [] });
        client.close();
      }
    }
    void load();
    return () => { disposed = true; client.close(); };
  }, [attempt]);

  return { ...state, retry: () => setAttempt((value) => value + 1) };
}
