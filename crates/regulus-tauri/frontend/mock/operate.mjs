// What the Operate and Templates views call, mirrored from the core and its modules with
// canned data (docs/composition-preview.md). Shapes follow `regulus-proto` and the
// `agent`, `communication` and `human` modules; the mock adds no method of its own.
import extra from "./operate.json" with { type: "json" };

const LIVE = new Set(["starting", "running"]);
const baseId = (id) => id.split("@")[0];
const failure = (code, message) => ({ error: [code, message] });
const ok = (result) => ({ result });

/** What `createState` adds: three modules, templates, the bus, the channels, agent conversations and decisions. */
export function operateState() {
  return {
    manifests: structuredClone(extra.manifests), templates: structuredClone(extra.templates), events: [], seq: 0, runs: 0, messages: new Map(),
    conversations: new Map(), decisions: [], decided: new Map(), decisionIds: 0,
  };
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

// ── Agent conversations (module `agent`, docs/12-agents.md §5) ──────────────────────────
// One per workflow node, like the box's `conv/<workflow>/<node>`: the next execution of the
// same node goes on in it. A turn is scripted: every `agent.history` of the node, and every
// `event.list` of its execution, moves it one step (thought, text, a tool call and its
// update, a plan, a permission request when a decider is wired, more text, the end).

const conversationKey = (execution, node) => `${execution.profile_id}/${execution.workflow_id}/${node.id}`;
function conversationOf(state, execution, node) {
  const key = conversationKey(execution, node);
  if (!state.conversations.has(key)) state.conversations.set(key, { seq: 0, turns: 0, engineSeq: 0, engineTurn: 0, entries: [], activity: [], queue: [], current: null });
  return state.conversations.get(key);
}
const outputOf = (node, id) => (node.ports ?? []).find((port) => port.direction === "output" && port.id === id);

function addEntry(conversation, role, text, turn, execution) {
  conversation.entries.push({ seq: ++conversation.seq, at: Date.now(), role, text, ...(turn ? { turn } : {}), execution_id: execution.id });
}
/** A session event: into the conversation and, if the node has `activity`, onto the bus as `agent.event@1`. */
function addActivity(state, execution, node, conversation, turn, fields) {
  const event = { seq: ++conversation.engineSeq, at: Date.now(), turn: conversation.engineTurn, ...fields };
  conversation.activity.push({ seq: ++conversation.seq, at: event.at, ...(turn ? { turn } : {}), event });
  const port = outputOf(node, "activity");
  if (port) emit(state, execution, { node_id: node.id, port_id: port.id }, port.capability, event, 1);
}
/** The decider wired to the node's `decisions`, if its module is there. */
function deciderOf(state, execution, node) {
  if (!hasModule(state, "regulus/human")) return null;
  const wire = workflowOf(state, execution)?.wires.find((item) => item.to.node_id === node.id && item.to.port_id === "decisions");
  return wire ? wire.from.node_id : null;
}
/** The node's reply: on `reply`, then on each workflow output `reply` is wired to. */
function reply(state, execution, node, text) {
  const port = outputOf(node, "reply");
  if (!port) return;
  emit(state, execution, { node_id: node.id, port_id: port.id }, port.capability, text, 1);
  const workflow = workflowOf(state, execution);
  for (const wire of workflow.wires.filter((item) => item.from.node_id === node.id && item.from.port_id === port.id && item.to.node_id === "self")) {
    const out = (workflow.ports ?? []).find((item) => item.id === wire.to.port_id && item.direction === "output");
    if (out) emit(state, execution, { node_id: "self", port_id: out.id }, out.capability, text, 2);
  }
}
const say = (text) => `(mock) You said: ${text}`;
const PERMISSION_OPTIONS = [{ id: "allow", kind: "allow_once", name: "Allow" }, { id: "reject", kind: "reject_once", name: "Reject" }];

function endTurn(state, execution, node, conversation, stopReason) {
  const { turn, text, written } = conversation.current;
  addActivity(state, execution, node, conversation, turn, { kind: "turn_ended", stop_reason: stopReason });
  const answer = stopReason === "end_turn" ? say(text) : written;
  addEntry(conversation, "agent", answer, turn, execution);
  addActivity(state, execution, node, conversation, turn, { kind: "state", state: "idle" });
  conversation.current = null;
  if (stopReason === "end_turn") reply(state, execution, node, answer);
}

/** One step of the node's turn in flight, or the next queued turn starting. */
function advance(state, execution, node, conversation) {
  if (conversation.current && conversation.current.execution_id !== execution.id) { conversation.current = null; conversation.queue = []; }
  const current = conversation.current;
  if (!current) {
    const next = conversation.queue.shift();
    if (!next) return;
    conversation.current = { ...next, execution_id: execution.id, step: 0, written: "" };
    conversation.engineTurn += 1;
    addEntry(conversation, "user", next.text, next.turn, execution);
    addActivity(state, execution, node, conversation, next.turn, { kind: "turn_started" });
    addActivity(state, execution, node, conversation, next.turn, { kind: "state", state: "busy" });
    return;
  }
  const { turn } = current;
  const activity = (fields) => addActivity(state, execution, node, conversation, turn, fields);
  const write = (chunk) => { current.written += chunk; activity({ kind: "text", text: chunk }); };
  if (current.waiting) {
    if (!state.decided.has(current.waiting)) return;
    activity({ kind: "permission_resolved", id: current.request, option: state.decided.get(current.waiting) });
    current.waiting = null;
  }
  current.step += 1;
  switch (current.step) {
    case 1: return activity({ kind: "thought", text: "(mock) Reading what was asked, then the notes." });
    case 2: return write("(mock) You said: ");
    case 3: return activity({ kind: "tool_call", id: `${turn}-read`, title: "Read notes.md", tool_kind: "read", status: "in_progress" });
    case 4: return activity({ kind: "tool_update", id: `${turn}-read`, status: "completed", content: [{ type: "content", content: { type: "text", text: "# Notes\nNothing urgent." } }] });
    case 5: return activity({ kind: "plan", entries: [{ content: "Read the notes", status: "completed", priority: "medium" }, { content: "Answer", status: "in_progress", priority: "medium" }] });
    case 6: {
      // Like `permissions = ask` with a decider: the request waits in `human.list` as a decision.
      const decider = deciderOf(state, execution, node);
      if (!decider) return write(current.text);
      const request = `${turn}-edit`;
      activity({ kind: "permission_request", id: request, tool: "edit", title: "Edit notes.md", options: PERMISSION_OPTIONS });
      const id = `h-mock-${++state.decisionIds}`;
      state.decisions.push({
        id, kind: "decision", execution_id: execution.id, node_id: decider,
        value: { options: PERMISSION_OPTIONS, context: { tool: "edit", title: "Edit notes.md", execution_id: execution.id, node_id: node.id, request_id: request } },
        prompt: `Agent \`${node.name || node.id}\` asks permission: Edit notes.md`,
        form: { type: "object", properties: { option: { type: "string", enum: PERMISSION_OPTIONS.map((option) => option.id), description: PERMISSION_OPTIONS.map((option) => `${option.id} (${option.name})`).join(", ") } }, required: ["option"], additionalProperties: false },
        created_ms: Date.now(),
      });
      current.waiting = id;
      current.request = request;
      return;
    }
    case 7: return current.written.endsWith(current.text) ? undefined : write(current.text);
    default: return endTurn(state, execution, node, conversation, "end_turn");
  }
}

/** Queues `text` as the node's next turn, as the box's `prompt` input and `agent.send` do. */
function queueTurn(conversation, text) {
  const turn = `t-${++conversation.turns}`;
  conversation.queue.push({ turn, text });
  return turn;
}

/** Every agent node of the execution moves one step: its turns run while the bus is read. */
function advanceExecution(state, execution) {
  if (!LIVE.has(execution.state.status)) return;
  for (const node of workflowOf(state, execution)?.nodes ?? []) {
    if (takes(node, "agent.engine") && state.conversations.has(conversationKey(execution, node))) advance(state, execution, node, conversationOf(state, execution, node));
  }
}

/** An agent node of an execution the core knows: live for send and cancel, any for history. */
function agentNode(state, params, { live }) {
  if (live) {
    const found = liveNode(state, params);
    if (found.error) return found;
    return takes(found.node, "agent.engine") ? found : failure(-32602, `no running agent \`${params.node_id}\` in execution \`${params.execution_id}\``);
  }
  const execution = state.executions.find((item) => item.id === params.execution_id);
  const node = execution && workflowOf(state, execution)?.nodes.find((item) => item.id === params.node_id);
  if (!node || !takes(node, "agent.engine") || (execution.disabled ?? []).some((item) => item.node_id === node.id)) {
    return failure(-32602, `no running agent \`${params.node_id}\` in execution \`${params.execution_id}\``);
  }
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
      const followed = params.execution_id && state.executions.find((item) => item.id === params.execution_id);
      if (followed) advanceExecution(state, followed);
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
      const wires = workflow.wires.filter((item) => item.from.node_id === "self" && item.from.port_id === port.id);
      for (const wire of wires) deliver(state, execution, seq, wire.to);
      // A value that reaches an agent's `prompt` is a turn of that agent: the reply comes when it ends.
      const agents = wires.map((wire) => wire.to.port_id === "prompt" && workflow.nodes.find((node) => node.id === wire.to.node_id && takes(node, "agent.engine"))).filter(Boolean);
      for (const node of agents) queueTurn(conversationOf(state, execution, node), typeof params.value === "string" ? params.value : JSON.stringify(params.value));
      // Otherwise the canned reply: the value comes back on the workflow's first output.
      const out = workflow.ports.find((item) => item.direction === "output");
      if (out && !agents.length) emit(state, execution, { node_id: "self", port_id: out.id }, out.capability, params.value, 1);
      return ok({ event: seq });
    }
    case "agent.prompt": {
      if (!hasModule(state, "regulus/agent")) return null;
      const found = liveNode(state, params);
      if (found.error) return found;
      if (!takes(found.node, "agent.engine")) return failure(-32602, `node \`${found.node.id}\` is not an agent`);
      return ok({ text: `(mock) ${params.text}`, stop_reason: "end_turn" });
    }
    case "agent.send": {
      if (!hasModule(state, "regulus/agent")) return null;
      const found = agentNode(state, params, { live: true });
      if (found.error) return found;
      if (typeof params.text !== "string") return failure(-32602, "Invalid params");
      return ok({ turn: queueTurn(conversationOf(state, found.execution, found.node), params.text) });
    }
    case "agent.history": {
      if (!hasModule(state, "regulus/agent")) return null;
      const found = agentNode(state, params, { live: false });
      if (found.error) return found;
      const live = LIVE.has(found.execution.state.status);
      const conversation = conversationOf(state, found.execution, found.node);
      if (live) advance(state, found.execution, found.node, conversation);
      const since = Number(params.since ?? 0);
      const seqs = [...conversation.entries, ...conversation.activity].map((item) => item.seq).filter((seq) => seq > since).sort((a, b) => a - b);
      const shown = params.limit ? seqs.slice(0, params.limit) : seqs;
      const last = shown.length ? shown[shown.length - 1] : since;
      const within = (item) => item.seq > since && item.seq <= last;
      return ok({
        entries: conversation.entries.filter(within), activity: conversation.activity.filter(within), next: last,
        session: { engine_session_id: `mock-${found.execution.id}-${found.node.id}`, native_resume: false, state: live ? (conversation.current ? "busy" : "idle") : "stopped" },
      });
    }
    case "agent.cancel": {
      if (!hasModule(state, "regulus/agent")) return null;
      const found = agentNode(state, params, { live: true });
      if (found.error) return found;
      const conversation = conversationOf(state, found.execution, found.node);
      const current = conversation.current;
      if (current && current.execution_id === found.execution.id) {
        // A cancel cancels the permission request waiting, as ACP asks.
        if (current.waiting && !state.decided.has(current.waiting)) {
          state.decisions = state.decisions.filter((item) => item.id !== current.waiting);
          addActivity(state, found.execution, found.node, conversation, current.turn, { kind: "permission_resolved", id: current.request, option: null });
        }
        endTurn(state, found.execution, found.node, conversation, "cancelled");
      }
      return ok(null);
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
    case "human.list": {
      if (!hasModule(state, "regulus/human")) return null;
      return ok(state.decisions.filter((item) => !params.execution_id || item.execution_id === params.execution_id));
    }
    case "human.get": case "human.answer": {
      if (!hasModule(state, "regulus/human")) return null;
      const pending = state.decisions.find((item) => item.id === params.id);
      if (!pending) return failure(-32602, `no pending decision \`${params.id}\``);
      if (method === "human.get") return ok(pending);
      if (params.decision !== "approve" && params.decision !== "reject") return failure(-32602, "Invalid params");
      // Only decider questions wait here: `option` names the answer, a rejection takes the reject option.
      const ids = pending.value.options.map((option) => option.id);
      const chosen = params.decision === "approve" ? params.fields?.option : pending.value.options.find((option) => option.kind.startsWith("reject"))?.id ?? null;
      if (params.decision === "approve" && !ids.includes(chosen)) return failure(-32602, `field \`option\` must be one of ${ids.join(", ")}`);
      state.decisions = state.decisions.filter((item) => item.id !== pending.id);
      state.decided.set(pending.id, chosen);
      return ok({ id: pending.id, decision: params.decision, execution_id: pending.execution_id, node_id: pending.node_id, port: "decisions" });
    }
    default: return null;
  }
}
