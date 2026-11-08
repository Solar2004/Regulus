import fixture from "./profile.json" with { type: "json" };
import config from "./config.json" with { type: "json" };
import manifests from "./manifests.json" with { type: "json" };

const LIVE = new Set(["starting", "running"]);
const baseId = (id) => id.split("@")[0];
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

// A module of the profile fixture has no manifest file: its manifest is its summary, with
// node types that declare no ports or fields.
const manifestOf = (module) => ({
  module: { id: module.id, name: module.name, requires_proto: null, methods: [], workflow_id: module.workflow_id ?? null },
  node: module.node_types.map((type) => ({ type, name: type, port: [], field: [] })),
});
const summaryOf = (manifest) => ({
  id: manifest.module.id,
  name: manifest.module.name,
  node_types: manifest.node.map((node) => node.type),
  workflow_id: manifest.module.workflow_id,
  state: { status: "idle", last_used: null },
});

// The id becomes a file name in the core: one plain name.
const badId = (id) => typeof id !== "string" || id === "" || id.startsWith(".") || /[\/\\\u0000-\u001f\u007f]/.test(id);
const nullIn = (value) => value === null || (typeof value === "object" && Object.values(value).some(nullIn));

// What the core fills on load: names, and the ports of a box from its workflow.
function normalize(profile) {
  const named = (item) => { if (!item.name) item.name = item.id; };
  named(profile);
  for (const workflow of profile.workflows) {
    named(workflow);
    (workflow.ports ?? []).forEach(named);
    for (const node of workflow.nodes ?? []) { named(node); (node.ports ?? []).forEach(named); }
  }
  for (const node of profile.workflows.flatMap((workflow) => workflow.nodes ?? [])) {
    const inner = profile.workflows.find((workflow) => workflow.id === node.internal_workflow_id);
    if (!node.module_id && inner && !(node.ports ?? []).length) node.ports = structuredClone(inner.ports ?? []);
  }
  return profile;
}

/** In-memory state of the mock core: what `profile.save` and `profile.delete` change. */
export function createState() {
  const modules = [...fixture.modules, ...manifests.map(summaryOf)];
  return {
    profiles: structuredClone(fixture.profiles),
    executions: structuredClone(fixture.executions),
    modules,
    manifests: [...fixture.modules.map(manifestOf), ...structuredClone(manifests)],
  };
}

/** One connection. Sessions that share `state` see each other's saves. */
export function createSession(state = createState()) {
  let negotiated = false;
  return function respond(raw) {
    let request;
    const error = (id, code, message) => ({ jsonrpc: "2.0", id, error: { code, message } });
    try { request = JSON.parse(raw.toString()); }
    catch { return error(null, -32700, "Parse error"); }
    if (!request || request.jsonrpc !== "2.0" || typeof request.method !== "string" ||
        (request.id !== undefined && typeof request.id !== "string" && typeof request.id !== "number")) {
      return error(null, -32600, "Invalid request");
    }
    if (request.id === undefined) return null;
    const { id, method, params = {} } = request;
    if (!params || typeof params !== "object" || Array.isArray(params)) return error(id, -32602, "Invalid params");
    if (method === "hello") {
      negotiated = typeof params.proto === "string" && params.proto.split(".")[0] === config.protocol.split(".")[0];
      return { jsonrpc: "2.0", id, result: { core: config.server, proto: config.protocol, compatible: negotiated } };
    }
    if (!negotiated) return error(id, -32001, "Compatible hello required");
    let result;
    switch (method) {
      case "profile.list": result = state.profiles.map(({ id, name, project_id }) => ({ id, name, project_id })); break;
      case "profile.get":
        result = state.profiles.find((profile) => profile.id === params.id);
        if (!result) return error(id, -32602, "Unknown profile");
        break;
      case "module.list": result = state.modules; break;
      case "module.manifest": {
        if (params.id === undefined || params.id === null) { result = state.manifests; break; }
        if (typeof params.id !== "string") return error(id, -32602, "Invalid params");
        result = state.manifests.find((manifest) => manifest.module.id === params.id) ??
          state.manifests.find((manifest) => baseId(manifest.module.id) === baseId(params.id));
        if (!result) return error(id, -32602, `Unknown module \`${params.id}\``);
        break;
      }
      case "profile.save": {
        const profile = params.profile;
        if (!isObject(profile) || typeof profile.id !== "string" || typeof profile.main_workflow_id !== "string" ||
            !Array.isArray(profile.workflows)) return error(id, -32602, "Invalid params");
        if (badId(profile.id)) return error(id, -32602, `profile id \`${profile.id}\` cannot be a file name`);
        if (!profile.workflows.some((workflow) => workflow.id === profile.main_workflow_id)) {
          return error(id, -32008, `profile \`${profile.id}\` has errors: main workflow \`${profile.main_workflow_id}\` does not exist (profile.main_missing)`);
        }
        if (profile.workflows.some((workflow) => (workflow.nodes ?? []).some((node) => nullIn(node.config ?? {})))) {
          return error(id, -32602, `profile \`${profile.id}\` cannot be written as TOML: a node config holds null`);
        }
        const at = state.profiles.findIndex((existing) => existing.id === profile.id);
        const path = `mock/profiles/${profile.id}.toml`;
        if (at >= 0 && params.force !== true) {
          return error(id, -32007, `profile \`${profile.id}\` already exists at ${path}; use force to replace it`);
        }
        const saved = normalize(structuredClone(profile));
        if (at >= 0) state.profiles[at] = saved; else state.profiles.push(saved);
        result = { profile_id: profile.id, path, diagnostics: [] };
        break;
      }
      case "profile.delete": {
        const at = state.profiles.findIndex((profile) => profile.id === params.id);
        if (at < 0) return error(id, -32602, "Unknown profile");
        const busy = state.executions.filter((execution) => execution.profile_id === params.id && LIVE.has(execution.state?.status));
        if (busy.length) {
          return error(id, -32009, `profile \`${params.id}\` is in use by ${busy.map((execution) => execution.id).join(", ")}; stop it first`);
        }
        state.profiles.splice(at, 1);
        result = { profile_id: params.id, path: `mock/profiles/${params.id}.toml` };
        break;
      }
      case "execution.list": result = state.executions; break;
      default: return error(id, -32601, "Method not found");
    }
    return { jsonrpc: "2.0", id, result };
  };
}
