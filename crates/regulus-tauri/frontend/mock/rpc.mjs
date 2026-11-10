import fixture from "./profile.json" with { type: "json" };
import config from "./config.json" with { type: "json" };
import manifests from "./manifests.json" with { type: "json" };
import { codeMethod, codeState } from "./code.mjs";
import { operateMethod, operateState } from "./operate.mjs";
import { settingsMethod, settingsState } from "./settings.mjs";

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

// A subset of `profile.check` (regulus_core::graph): same codes, severities and fields.
const SELF = "self";
function diagnose(profile, modules) {
  const found = [];
  const push = (severity, code, message, workflow_id = null, node_id = null, wire_id = null) =>
    found.push({ severity, code, message, workflow_id, node_id, wire_id });
  const workflow = (id) => profile.workflows.find((item) => item.id === id);
  if (!workflow(profile.main_workflow_id)) {
    push("error", "profile.main_missing", `main workflow \`${profile.main_workflow_id}\` does not exist`);
  }
  const installed = (id) => modules.some((module) => baseId(module.id) === baseId(id));
  const portsOf = (node) => (node.ports ?? []).length || node.module_id || !node.internal_workflow_id
    ? node.ports ?? [] : workflow(node.internal_workflow_id)?.ports ?? [];
  for (const flow of profile.workflows) {
    const nodes = flow.nodes ?? [];
    const endpoint = (end) => {
      if (end.node_id === SELF) {
        const port = (flow.ports ?? []).find((item) => item.id === end.port_id);
        return port && { port, direction: port.direction === "input" ? "output" : "input" };
      }
      const node = nodes.find((item) => item.id === end.node_id);
      const port = node && portsOf(node).find((item) => item.id === end.port_id);
      return port && { port, direction: port.direction };
    };
    for (const node of nodes) {
      if (node.module_id && !installed(node.module_id)) {
        push("info", "node.module_not_installed", `module \`${node.module_id}\` is not installed`, flow.id, node.id);
      }
      if (node.internal_workflow_id && !workflow(node.internal_workflow_id)) {
        push("error", "node.internal_workflow_missing", `internal workflow \`${node.internal_workflow_id}\` does not exist`, flow.id, node.id);
      }
    }
    const incoming = new Map();
    for (const wire of flow.wires ?? []) {
      const from = endpoint(wire.from);
      const to = endpoint(wire.to);
      if (!from || !to) {
        push("error", "wire.endpoint_missing", `wire joins \`${wire.from.node_id}.${wire.from.port_id}\` to \`${wire.to.node_id}.${wire.to.port_id}\`, but one of those ports does not exist`, flow.id, null, wire.id);
        continue;
      }
      const key = `${wire.to.node_id}.${wire.to.port_id}`;
      incoming.set(key, (incoming.get(key) ?? 0) + 1);
      if (from.direction !== "output" || to.direction !== "input") push("error", "wire.direction", "a wire must go from an output to an input", flow.id, null, wire.id);
      if (from.port.kind !== wire.kind || to.port.kind !== wire.kind) push("error", "wire.kind_mismatch", "both ports must be of the same kind as the wire", flow.id, null, wire.id);
      if (from.port.capability !== to.port.capability) push("error", "wire.capability_mismatch", `\`${from.port.capability}\` does not fit into \`${to.port.capability}\``, flow.id, null, wire.id);
    }
    for (const node of nodes) for (const port of portsOf(node).filter((item) => item.direction === "input")) {
      const count = incoming.get(`${node.id}.${port.id}`) ?? 0;
      if (count > 1 && port.cardinality !== "many") push("error", "port.too_many_wires", `input \`${port.id}\` accepts one wire and has ${count}`, flow.id, node.id);
      if (count === 0 && port.required) push("warning", "port.required_unwired", `required input \`${port.id}\` (${port.capability}) has no wire; the node will be disabled`, flow.id, node.id);
    }
  }
  return found;
}

/** In-memory state of the mock core: what `profile.save` and `profile.delete` change. */
export function createState() {
  const operate = operateState();
  const { codeManifests, codeTemplates, ...code } = codeState();
  const { settingsModules, ...settings } = settingsState();
  // The Settings modules come with the state they show: active, broken, disabled,
  // incompatible, and a local one blocked until its project is trusted (with `project`).
  // A module code.json describes in full replaces the example of the same name in the profile fixture.
  const examples = fixture.modules.filter((module) => !codeManifests.some((manifest) => baseId(manifest.module.id) === baseId(module.id)));
  const modules = [...examples, ...manifests.map(summaryOf), ...operate.manifests.map(summaryOf), ...codeManifests.map(summaryOf),
    ...settingsModules.map(({ manifest, state, project }) => ({ ...summaryOf(manifest), state, ...(project ? { project } : {}) }))];
  return {
    ...operate,
    ...code,
    ...settings,
    templates: [...operate.templates, ...codeTemplates],
    profiles: structuredClone(fixture.profiles),
    executions: structuredClone(fixture.executions),
    modules,
    manifests: [...examples.map(manifestOf), ...structuredClone(manifests), ...operate.manifests, ...codeManifests, ...settingsModules.map(({ manifest }) => manifest)],
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
        const found = diagnose(profile, state.modules);
        const errors = found.filter((item) => item.severity === "error");
        if (errors.length) {
          return error(id, -32008, `profile \`${profile.id}\` has errors: ${errors.map((item) => `${item.message} (${item.code})`).join("; ")}`);
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
        result = { profile_id: profile.id, path, diagnostics: found };
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
      case "profile.check": {
        const profile = state.profiles.find((item) => item.id === params.id);
        if (!profile) return error(id, -32602, "Unknown profile");
        result = diagnose(profile, state.modules);
        break;
      }
      // execution.start and the other Operate methods live in operate.mjs, code.inspect and
      // the workers in code.mjs, secret.*, config.client and service.* in settings.mjs.
      default: {
        const answer = operateMethod(state, method, params) ?? codeMethod(state, method, params) ?? settingsMethod(state, method, params);
        if (!answer) return error(id, -32601, "Method not found");
        if (answer.error) return error(id, ...answer.error);
        result = answer.result;
      }
    }
    return { jsonrpc: "2.0", id, result };
  };
}
