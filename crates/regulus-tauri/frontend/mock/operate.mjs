// What the Operate and Templates views call, mirrored from the core and its modules with
// canned data (docs/composition-preview.md). Shapes follow `regulus-proto` and the
// `agent`, `communication` and `human` modules; the mock adds no method of its own.
import extra from "./operate.json" with { type: "json" };

const LIVE = new Set(["starting", "running"]);
const baseId = (id) => id.split("@")[0];
const failure = (code, message) => ({ error: [code, message] });
const ok = (result) => ({ result });

/** What `createState` adds: three modules, templates, the bus and the channels. */
export function operateState() {
  return { manifests: structuredClone(extra.manifests), templates: structuredClone(extra.templates), events: [], seq: 0, runs: 0, messages: new Map() };
}

const hasModule = (state, id) => state.modules.some((module) => baseId(module.id) === baseId(id));
const workflowOf = (state, execution) =>
  state.profiles.find((profile) => profile.id === execution.profile_id)?.workflows.find((workflow) => workflow.id === execution.workflow_id);
const takes = (node, capability) => (node?.ports ?? []).some((port) => port.direction === "input" && port.capability.split("@")[0] === capability);

function emit(state, execution, from, capability, value, depth = 0) {
  const seq = ++state.seq;
  state.events.push({ seq, at: Date.now(), execution_id: execution.id, project_id: execution.project_id, event: seq, from, capability, depth, kind: "emitted", value });
  return seq;
}
function deliver(state, execution, emitted, to) {
  const event = state.events.find((item) => item.seq === emitted);
  state.events.push({ ...event, seq: ++state.seq, at: Date.now(), kind: "delivered", value: undefined, to });
}

/** A live execution and the node of it a module method names. */
function liveNode(state, params) {
  const execution = state.executions.find((item) => item.id === params.execution_id);
  if (!execution || !LIVE.has(execution.state.status)) return failure(-32602, `execution \`${params.execution_id}\` is not running`);
  const node = workflowOf(state, execution)?.nodes.find((item) => item.id === params.node_id);
  if (!node) return failure(-32602, `no node \`${params.node_id}\` in \`${execution.id}\``);
  if ((execution.disabled ?? []).some((item) => item.node_id === node.id)) return failure(-32602, `node \`${node.id}\` is disabled`);
  return { execution, node };
}

function render(template, vars) {
  const values = {};
  for (const variable of template.vars) {
    const value = vars[variable.name] ?? variable.default;
    if (value === undefined || value === null) return failure(-32602, `template \`${template.name}\` needs a value for var \`${variable.name}\``);
    values[variable.name] = String(value);
  }
  const text = JSON.stringify(template.profile).replace(/\{\{(\w+)\}\}/g, (_, name) => JSON.stringify(values[name] ?? "").slice(1, -1));
  return { profile: JSON.parse(text) };
}

/** The workflows a workflow places as boxes, at any depth, itself first. */
function embedded(profile, id, seen = new Set()) {
  if (seen.has(id)) return [];
  seen.add(id);
  const workflow = profile.workflows.find((item) => item.id === id);
  if (!workflow) return [];
  return [workflow, ...workflow.nodes.filter((node) => !node.module_id && node.internal_workflow_id).flatMap((node) => embedded(profile, node.internal_workflow_id, seen))];
}

/** `template.save`: the saved profile, or one workflow of it as main, kept as a template the Templates view lists. */
function saveTemplate(state, params) {
  const name = typeof params.name === "string" ? params.name.trim() : "";
  if (!name || /[\/\\]/.test(name)) return failure(-32005, `template name \`${params.name}\` cannot be a file name`);
  const profile = state.profiles.find((item) => item.id === params.profile_id);
  if (!profile) return failure(-32602, `no profile \`${params.profile_id}\``);
  let copy = structuredClone(profile);
  if (params.workflow_id !== undefined) {
    const workflows = embedded(copy, params.workflow_id);
    if (!workflows.length) return failure(-32602, `profile \`${profile.id}\` has no workflow \`${params.workflow_id}\``);
    copy = { ...copy, main_workflow_id: params.workflow_id, workflows };
  }
  const vars = params.vars ?? {};
  if (vars === null || typeof vars !== "object" || Array.isArray(vars)) return failure(-32602, "Invalid params");
  for (const [varName, spec] of Object.entries(vars)) {
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(varName) || !spec || typeof spec !== "object") return failure(-32005, `var \`${varName}\` is not a valid template var`);
  }
  const path = typeof params.path === "string" && params.path.trim() ? params.path.trim() : `mock/templates/${name}.toml`;
  const at = state.templates.findIndex((item) => item.name === name || item.path === path);
  if (at >= 0 && params.force !== true) return failure(-32007, `template \`${name}\` already exists at ${path}; use force to replace it`);
  const template = {
    name, description: params.description ?? "", path, requires: [],
    vars: Object.entries(vars).map(([varName, spec]) => ({ name: varName, description: spec.description ?? "", ...(spec.default !== undefined ? { default: spec.default } : {}) })),
    text: `# mock template written by template.save\n[template]\nname = "${name}"\n`, profile: copy,
  };
  if (at >= 0) state.templates[at] = template; else state.templates.push(template);
  return ok({ path });
}

/** `execution.tree`: an execution and the ones it started, at any depth, from the list. */
function executionTree(state, id, seen = new Set()) {
  const execution = state.executions.find((item) => item.id === id);
  seen.add(id);
  return { execution, children: state.executions.filter((item) => item.parent === id && !seen.has(item.id)).map((child) => executionTree(state, child.id, seen)) };
}

/** A field of type `workflow` in a node's manifest, set to a workflow of the profile: the core would start it as a child. */
function calledWorkflows(state, profile, workflow) {
  return workflow.nodes.flatMap((node) => {
    const type = state.manifests.find((manifest) => baseId(manifest.module.id) === baseId(node.module_id ?? ""))?.node?.find((item) => item.type === node.type);
    return (type?.field ?? []).filter((field) => field.type === "workflow")
      .map((field) => node.config?.[field.id])
      .filter((id) => typeof id === "string" && id !== workflow.id && profile.workflows.some((item) => item.id === id))
      .map((id) => ({ node: node.id, workflow: id }));
  });
}

/** Answers the Operate methods; `null` for any other method. */
export function operateMethod(state, method, params) {
  switch (method) {
    case "template.list": return ok(state.templates.map(({ text: _text, profile: _profile, ...summary }) => summary));
    case "template.get": {
      const template = state.templates.find((item) => item.name === params.template || item.path === params.template);
      return template ? ok({ name: template.name, path: template.path, text: template.text }) : failure(-32004, `no template \`${params.template}\``);
    }
    case "template.apply": {
      if (typeof params.template !== "string") return failure(-32602, "Invalid params");
      const template = state.templates.find((item) => item.name === params.template || item.path === params.template);
      if (!template) return failure(-32004, `no template \`${params.template}\``);
      if (template.error) return failure(-32005, `template \`${template.name}\`: ${template.error}`);
      const unmet = template.requires.find((require) => !hasModule(state, require.module));
      if (unmet) return failure(-32006, `template \`${template.name}\` requires \`${unmet.module}\` (${unmet.version}), which is not installed`);
      const rendered = render(template, params.vars ?? {});
      if (rendered.error) return rendered;
      const profile = { ...rendered.profile, id: params.id || rendered.profile.id };
      const path = `mock/profiles/${profile.id}.toml`;
      const at = state.profiles.findIndex((item) => item.id === profile.id);
      if (at >= 0 && params.force !== true) return failure(-32007, `profile \`${profile.id}\` already exists at ${path}; use force to replace it`);
      if (at >= 0) state.profiles[at] = profile; else state.profiles.push(profile);
      return ok({ profile_id: profile.id, path, diagnostics: [] });
    }
    case "template.save": return saveTemplate(state, params);
    case "execution.tree": {
      if (typeof params.execution_id !== "string") return failure(-32602, "Invalid params");
      if (!state.executions.some((item) => item.id === params.execution_id)) return failure(-32602, `no execution \`${params.execution_id}\``);
      return ok(executionTree(state, params.execution_id));
    }
    case "execution.start": {
      const profile = state.profiles.find((item) => item.id === params.profile_id);
      if (!profile) return failure(-32602, "Unknown profile");
      const workflow = profile.workflows.find((item) => item.id === (params.workflow_id ?? profile.main_workflow_id));
      if (!workflow) return failure(-32602, `profile \`${profile.id}\` has no workflow \`${params.workflow_id}\``);
      const disabled = workflow.nodes.flatMap((node) => {
        const missing = node.ports.filter((port) => port.direction === "input" && port.required &&
          !workflow.wires.some((wire) => wire.to.node_id === node.id && wire.to.port_id === port.id)).map((port) => port.id);
        return missing.length ? [{ node_id: node.id, missing }] : [];
      });
      const execution = { id: `exec-${++state.runs}`, profile_id: profile.id, workflow_id: workflow.id, project_id: profile.project_id, state: { status: "running" }, ...(disabled.length ? { disabled } : {}) };
      state.executions.push(execution);
      // A node that calls a workflow starts it as a child, as `call_workflow` does in the core.
      for (const [index, call] of calledWorkflows(state, profile, workflow).entries()) {
        state.executions.push({ id: `exec-${++state.runs}`, profile_id: profile.id, workflow_id: call.workflow, project_id: profile.project_id, state: { status: "running" }, parent: execution.id, caller: call.node, tag: `${call.node}/${index}` });
      }
      return ok(execution);
    }
    case "execution.stop": {
      const execution = state.executions.find((item) => item.id === params.id);
      if (!execution) return failure(-32602, `no execution \`${params.id}\``);
      // Stopping a parent stops its children, at any depth.
      const stop = (id) => {
        for (const item of state.executions) if (item.id === id || item.parent === id) {
          if (LIVE.has(item.state.status)) item.state = { status: "stopped" };
          if (item.id !== id) stop(item.id);
        }
      };
      stop(execution.id);
      return ok(execution);
    }
    case "event.list": {
      const since = Number(params.since ?? 0);
      const matching = state.events.filter((event) => event.seq > since &&
        (!params.execution_id || event.execution_id === params.execution_id) &&
        (!params.project_id || event.project_id === params.project_id));
      const events = params.limit ? matching.slice(0, params.limit) : matching;
      return ok({ events, next: events.length ? events[events.length - 1].seq : since, pending: 0 });
    }
    case "workflow.send": {
      const execution = state.executions.find((item) => item.id === params.execution_id);
      if (!execution || !LIVE.has(execution.state.status)) return failure(-32602, `execution \`${params.execution_id}\` is not running`);
      const workflow = workflowOf(state, execution);
      const port = (workflow?.ports ?? []).find((item) => item.id === params.port && item.direction === "input");
      if (!port) return failure(-32602, `workflow \`${execution.workflow_id}\` has no input port \`${params.port}\``);
      const seq = emit(state, execution, { node_id: "self", port_id: port.id }, port.capability, params.value);
      for (const wire of workflow.wires.filter((item) => item.from.node_id === "self" && item.from.port_id === port.id)) deliver(state, execution, seq, wire.to);
      // The canned reply: the value comes back on the workflow's first output.
      const out = workflow.ports.find((item) => item.direction === "output");
      if (out) emit(state, execution, { node_id: "self", port_id: out.id }, out.capability, params.value, 1);
      return ok({ event: seq });
    }
    case "agent.prompt": {
      if (!hasModule(state, "regulus/agent")) return null;
      const found = liveNode(state, params);
      if (found.error) return found;
      if (!takes(found.node, "agent.engine")) return failure(-32602, `node \`${found.node.id}\` is not an agent`);
      return ok({ text: `(mock) ${params.text}`, stop_reason: "end_turn" });
    }
    case "comm.history": case "comm.participants": case "comm.send": {
      if (!hasModule(state, "regulus/communication")) return null;
      const found = liveNode(state, params);
      if (found.error) return found;
      if (!takes(found.node, "participant")) return failure(-32602, `node \`${found.node.id}\` is not a channel`);
      const key = `${found.execution.id}/${found.node.id}`;
      const messages = state.messages.get(key) ?? [];
      const workflow = workflowOf(state, found.execution);
      const participants = workflow.wires.filter((wire) => wire.to.node_id === found.node.id)
        .map((wire) => workflow.nodes.find((node) => node.id === wire.from.node_id))
        .filter(Boolean).map((node) => ({ name: node.id, node: node.id, kind: node.type, state: "idle", queued: 0 }));
      if (method === "comm.history") return ok(messages);
      if (method === "comm.participants") return ok(participants);
      const from = String(params.from ?? "").trim();
      const text = String(params.text ?? "").trim();
      if (!text) return failure(-32602, "the message is empty");
      if (!from || /\s/.test(from)) return failure(-32602, `\`${from}\` is not a sender name`);
      const message = { id: messages.length + 1, at: Math.floor(Date.now() / 1000), from, to: participants.map((item) => item.name), text };
      state.messages.set(key, [...messages, message]);
      return ok(message);
    }
    case "human.list": return hasModule(state, "regulus/human") ? ok([]) : null;
    case "human.get": case "human.answer":
      return hasModule(state, "regulus/human") ? failure(-32602, `no pending decision \`${params.id}\``) : null;
    default: return null;
  }
}
