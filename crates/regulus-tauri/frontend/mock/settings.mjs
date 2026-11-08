// What Settings calls: the core's `secret.*`, `config.client`, `project.list` and
// `project.trust`, and the `service` module's methods, mirrored with canned data from settings.json. Shapes and errors follow `regulus-core::secrets`
// and `crates/modules/service`; the mock adds no method of its own.
import extra from "./settings.json" with { type: "json" };

const baseId = (id) => id.split("@")[0];
const failure = (code, message) => ({ error: [code, message] });
const ok = (result) => ({ result });
const SERVICE = "regulus/service";
const POLICY = ["cooldown_ms", "requests", "window_ms", "concurrency", "budget_requests", "budget_ms"];
const describe = (state) => state.status === "disabled" ? `missing required inputs: ${state.missing.join(", ")}`
  : state.status === "broken" ? state.reason
  : state.status === "incompatible" ? `requires proto ${state.requires}, core has ${state.have}` : "available";

/** What `createState` adds: the extra modules (with their states), secrets and the service store. */
export function settingsState() {
  const data = structuredClone(extra);
  return {
    settingsModules: data.modules,
    projects: data.projects,
    secrets: new Map(Object.entries(data.secrets)),
    keys: data.keys,
    serviceLog: data.log,
    serviceEndpoints: data.endpoints,
    clientConfig: data.client,
  };
}

const validName = (name) => typeof name === "string" && /^[A-Za-z0-9._-]+$/.test(name);
const validScope = (entry) => {
  if (typeof entry !== "string") return false;
  const [project, workflow, ...rest] = entry.split("/");
  return !rest.length && project !== "" && (workflow === undefined || workflow !== "");
};
const hex = (bytes) => Array.from(crypto.getRandomValues(new Uint8Array(bytes)), (b) => b.toString(16).padStart(2, "0")).join("");
const isEmpty = (policy) => POLICY.every((field) => !policy[field]);

const isTable = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

/** `config.client` as `regulus_core::config::layer_client` builds it: the project's `[client]`
 * on top of the global one, key by key; a table on both sides merges key by key too. */
export function layerClient(global, project = {}) {
  const client = structuredClone(global);
  for (const [key, value] of Object.entries(project)) {
    client[key] = isTable(client[key]) && isTable(value) ? { ...client[key], ...value } : structuredClone(value);
  }
  const sources = {};
  for (const [key, value] of Object.entries(client)) {
    if (isTable(value)) for (const name of Object.keys(value)) sources[`${key}.${name}`] = isTable(project[key]) && name in project[key] ? "regulus.toml" : "config.toml";
    else sources[key] = key in project ? "regulus.toml" : "config.toml";
  }
  return { client, sources };
}

/** Answers `secret.*`, `config.client` and `service.*`; `null` for any other method or an absent module. */
export function settingsMethod(state, method, params) {
  switch (method) {
    case "secret.list": return ok([...state.secrets.keys()]);
    case "secret.set":
      if (typeof params.name !== "string" || typeof params.value !== "string") return failure(-32602, "Invalid params");
      if (!validName(params.name)) return failure(-32602, `invalid secret name \`${params.name}\``);
      state.secrets.set(params.name, params.value);
      return ok(null);
    case "config.client": {
      const { global, projects, warnings } = state.clientConfig;
      const projectId = params.project_id ?? null;
      if (projectId !== null && !Object.hasOwn(projects, projectId)) return failure(-32010, `no open project \`${projectId}\``);
      return ok({ ...layerClient(global, projectId === null ? {} : projects[projectId]), warnings });
    }
    case "project.list": return ok(state.projects);
    case "project.trust": {
      // `regulus project trust <path>`: the folder's blocked modules start (1.14); the mock
      // marks them idle, never used. `remove` blocks them again.
      const project = state.projects.find((item) => item.path === params.path || `${item.path}/regulus.toml` === params.path);
      if (!project) return failure(-32602, `no project at ${params.path}`);
      const trusted = !params.remove;
      for (const module of state.modules) {
        if (module.project !== project.id) continue;
        module.state = trusted
          ? { status: "idle", last_used: null }
          : { status: "blocked", reason: "untrusted project: reopen with --trust" };
      }
      return ok({ path: project.path, trusted });
    }
    case "secret.delete":
      if (!state.secrets.delete(params.name)) return failure(-32602, `secret \`${params.name}\` is not set`);
      return ok(null);
  }
  if (!method.startsWith("service.")) return null;
  const module = state.modules.find((item) => baseId(item.id) === SERVICE);
  if (!module) return null;
  const status = module.state?.status ?? "idle";
  if (status !== "active" && status !== "idle") return failure(-32002, `module \`${module.id}\` is unavailable: ${describe(module.state)}`);
  switch (method) {
    case "service.key.list": return ok(state.keys);
    case "service.key.new": {
      const scope = params.scope;
      if (!Array.isArray(scope) || !scope.length) return failure(-32602, "a key needs a scope: <project>[/<workflow>], `*` for any");
      const bad = scope.find((entry) => !validScope(entry));
      if (bad !== undefined) return failure(-32602, `\`${bad}\` is not <project>[/<workflow>]`);
      const policy = params.policy ?? {};
      const unknown = Object.keys(policy).find((field) => !POLICY.includes(field));
      if (unknown) return failure(-32602, `unknown field \`${unknown}\``);
      const key = `rgk_${hex(32)}`;
      const record = { id: `k-${hex(4)}`, name: typeof params.name === "string" ? params.name : "", scope, hint: key.slice(0, 10), created_ms: Date.now() };
      if (!isEmpty(policy)) record.policy = { ...Object.fromEntries(POLICY.map((field) => [field, 0])), ...policy };
      state.keys.push(record);
      return ok({ ...record, key });
    }
    case "service.key.revoke": {
      const record = state.keys.find((item) => item.id === params.id);
      if (!record) return failure(-32602, `no key \`${params.id}\``);
      if (record.revoked_ms === undefined) record.revoked_ms = Date.now();
      return ok(record);
    }
    case "service.log": {
      const entries = state.serviceLog.filter((entry) =>
        (params.key == null || entry.key_id === params.key) && (params.workflow == null || entry.workflow === params.workflow));
      return ok(params.last == null ? entries : entries.slice(Math.max(0, entries.length - params.last)));
    }
    case "service.endpoints": return ok(state.serviceEndpoints);
    default: return failure(-32601, "Method not found");
  }
}
