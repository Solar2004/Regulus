//! What can be talked to and how a conversation reads, shared by `regulus talk` and the
//! terminal UI (docs/04-client.md §7, §8). Pure: no socket, no terminal. The rules are the
//! app's (`operate/conversation.ts`): a direct agent is a node with a Uses input
//! `agent.engine` and needs nothing bound; a workflow is talked to only through a binding a
//! person chose, one of its own then inputs `text@` and one of its own then outputs `text@`.
//! Nothing is ever bound on its own, not even when there is a single candidate pair.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use regulus_proto::{BusEvent, BusEventKind, Direction, Node, Port, WireKind, Workflow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Capabilities a conversation is read from, by name without version: never a module id.
pub const AGENT_ENGINE: &str = "agent.engine";
pub const TEXT: &str = "text";
pub const ACTIVITY: &str = "agent.event";
/// The file in the home where the CLI and the terminal UI keep the bindings people chose.
pub const BINDINGS_FILE: &str = "conversations.json";
/// What a workflow's own ports are called from inside it.
const SELF: &str = regulus_proto::WORKFLOW_SELF;
/// Who said an entry of an agent's history (docs/12-agents.md §5).
pub const USER: &str = "user";
pub const AGENT: &str = "agent";
pub const SYSTEM: &str = "system";

fn base(capability: &str) -> &str {
    capability.split('@').next().unwrap_or(capability)
}

// ── What can be talked to ──────────────────────────────────────────────────────────────

/// A workflow's own then ports of `text@`: where a turn goes in and where replies come out.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationPorts<'a> {
    pub inputs: Vec<&'a Port>,
    pub outputs: Vec<&'a Port>,
}

pub fn conversation_ports(workflow: &Workflow) -> ConversationPorts<'_> {
    let text = |direction: Direction| {
        workflow
            .ports
            .iter()
            .filter(move |p| {
                p.kind == WireKind::Then && p.direction == direction && base(&p.capability) == TEXT
            })
            .collect()
    };
    ConversationPorts {
        inputs: text(Direction::Input),
        outputs: text(Direction::Output),
    }
}

/// Text goes in and text comes out: it can be bound, never on its own.
pub fn is_conversable(workflow: &Workflow) -> bool {
    let ports = conversation_ports(workflow);
    !ports.inputs.is_empty() && !ports.outputs.is_empty()
}

/// An eager engine belongs to this node's session; a lazy one may be for its workers.
pub fn is_agent(node: &Node) -> bool {
    node.ports.iter().any(|p| {
        p.direction == Direction::Input
            && p.kind == WireKind::Uses
            && !p.lazy
            && base(&p.capability) == AGENT_ENGINE
    })
}

pub fn agent_nodes(workflow: &Workflow) -> Vec<&Node> {
    workflow.nodes.iter().filter(|n| is_agent(n)).collect()
}

// ── The binding a person chose ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub input: String,
    pub output: String,
}

/// One entry of `operate.conversations` in `config.client`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredBinding {
    pub profile: String,
    pub workflow: String,
    pub binding: Binding,
}

/// Where a binding in effect comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingSource {
    /// Chosen here and kept in [`BINDINGS_FILE`].
    Own,
    /// `[[client.operate.conversations]]`, read-only.
    Config,
}

pub fn binding_key(profile: &str, workflow: &str) -> String {
    format!("{profile}/{workflow}")
}

/// Whether both ports of a binding are still candidates of the workflow.
pub fn fits(binding: &Binding, workflow: &Workflow) -> bool {
    let ports = conversation_ports(workflow);
    ports.inputs.iter().any(|p| p.id == binding.input)
        && ports.outputs.iter().any(|p| p.id == binding.output)
}

/// `operate.conversations` as written, `[{ profile, workflow, input, output }]`; an entry
/// missing any of them is dropped. `client` is `config.client`'s `client` map.
pub fn read_configured(client: &serde_json::Map<String, Value>) -> Vec<ConfiguredBinding> {
    let Some(list) = client
        .get("operate")
        .and_then(|o| o.get("conversations"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|item| {
            let field = |key: &str| item.get(key).and_then(Value::as_str).map(str::to_string);
            Some(ConfiguredBinding {
                profile: field("profile")?,
                workflow: field("workflow")?,
                binding: Binding {
                    input: field("input")?,
                    output: field("output")?,
                },
            })
        })
        .collect()
}

/// The binding in effect: the one chosen here, else config's; one whose ports are gone
/// does not count.
pub fn binding_for(
    own: &BTreeMap<String, Binding>,
    configured: &[ConfiguredBinding],
    profile: &str,
    workflow: &Workflow,
) -> Option<(Binding, BindingSource)> {
    if let Some(mine) = own.get(&binding_key(profile, &workflow.id)) {
        if fits(mine, workflow) {
            return Some((mine.clone(), BindingSource::Own));
        }
    }
    configured
        .iter()
        .find(|c| c.profile == profile && c.workflow == workflow.id && fits(&c.binding, workflow))
        .map(|c| (c.binding.clone(), BindingSource::Config))
}

/// The bindings chosen in this home, `{ "<profile>/<workflow>": { input, output } }`, the
/// shape the app keeps on its device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    pub path: PathBuf,
    pub own: BTreeMap<String, Binding>,
}

impl Bindings {
    /// A missing file is no binding yet; one that does not read is an error, so it is
    /// never overwritten by accident.
    pub fn load(home: &Path) -> Result<Self, String> {
        let path = home.join(BINDINGS_FILE);
        let own = match std::fs::read_to_string(&path) {
            Ok(text) if text.trim().is_empty() => BTreeMap::new(),
            Ok(text) => {
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Self { path, own })
    }

    pub fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(&self.own).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, text + "\n").map_err(|e| format!("{}: {e}", self.path.display()))
    }

    pub fn set(&mut self, profile: &str, workflow: &str, binding: Binding) {
        self.own.insert(binding_key(profile, workflow), binding);
    }

    /// Whether there was one to forget.
    pub fn forget(&mut self, profile: &str, workflow: &str) -> bool {
        self.own.remove(&binding_key(profile, workflow)).is_some()
    }
}

/// What `regulus talk` talks to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Agent(String),
    Workflow(Binding),
}

/// Why that subject: the CLI says it, and remembers a binding given by flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Flags,
    Bound(BindingSource),
    /// The only agent of the workflow.
    OnlyAgent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Talk(Subject, Origin),
    /// The workflow's only text input and only text output: asked, never assumed.
    Propose(Binding),
    /// Nothing to talk to, or more than one thing: what to say instead.
    Refuse(String),
}

/// What the flags of `regulus talk` ask for.
#[derive(Debug, Clone, Copy, Default)]
pub struct Asked<'a> {
    pub node: Option<&'a str>,
    pub input: Option<&'a str>,
    pub output: Option<&'a str>,
}

/// Decides what to talk to: the flags; else a binding in effect; else the only agent;
/// else, with no agent, the only text pair is proposed; anything else lists candidates.
pub fn choose(
    workflow: &Workflow,
    profile: &str,
    asked: Asked<'_>,
    own: &BTreeMap<String, Binding>,
    configured: &[ConfiguredBinding],
) -> Choice {
    let ports = conversation_ports(workflow);
    let agents = agent_nodes(workflow);
    if let Some(node) = asked.node {
        if asked.input.is_some() || asked.output.is_some() {
            return Choice::Refuse(
                "--node talks to an agent; --in/--out bind the workflow: use one or the other"
                    .into(),
            );
        }
        return match workflow.node(node) {
            Some(found) if !is_agent(found) => Choice::Refuse(format!(
                "node `{node}` has no `{AGENT_ENGINE}` input: it is not an agent\n{}",
                candidates(workflow)
            )),
            // A node the workflow does not list (inside a box) is the core's to judge.
            _ => Choice::Talk(Subject::Agent(node.into()), Origin::Flags),
        };
    }
    if asked.input.is_some() || asked.output.is_some() {
        let pick = |given: Option<&str>, list: &[&Port], flag: &str| -> Result<String, String> {
            match given {
                Some(id) if list.iter().any(|p| p.id == id) => Ok(id.to_string()),
                Some(id) => Err(format!(
                    "`{id}` is not a then {flag} `{TEXT}@` of workflow `{}`\n{}",
                    workflow.id,
                    candidates(workflow)
                )),
                None if list.len() == 1 => Ok(list[0].id.clone()),
                None => Err(format!(
                    "say which {flag} with --{}\n{}",
                    if flag == "input" { "in" } else { "out" },
                    candidates(workflow)
                )),
            }
        };
        let input = pick(asked.input, &ports.inputs, "input");
        let output = pick(asked.output, &ports.outputs, "output");
        return match (input, output) {
            (Ok(input), Ok(output)) => {
                Choice::Talk(Subject::Workflow(Binding { input, output }), Origin::Flags)
            }
            (Err(e), _) | (_, Err(e)) => Choice::Refuse(e),
        };
    }
    if let Some((binding, source)) = binding_for(own, configured, profile, workflow) {
        return Choice::Talk(Subject::Workflow(binding), Origin::Bound(source));
    }
    match (
        agents.as_slice(),
        ports.inputs.as_slice(),
        ports.outputs.as_slice(),
    ) {
        ([only], _, _) => Choice::Talk(Subject::Agent(only.id.clone()), Origin::OnlyAgent),
        ([], [input], [output]) => Choice::Propose(Binding {
            input: input.id.clone(),
            output: output.id.clone(),
        }),
        ([], [], _) | ([], _, []) => Choice::Refuse(format!(
            "nothing to talk to in workflow `{}`: no agent node (a Uses input `{AGENT_ENGINE}`) \
             and no own then input and output of `{TEXT}@`",
            workflow.id
        )),
        _ => Choice::Refuse(format!(
            "more than one thing to talk to in workflow `{}`: say which\n{}",
            workflow.id,
            candidates(workflow)
        )),
    }
}

/// The flags each candidate takes, one kind per line.
pub fn candidates(workflow: &Workflow) -> String {
    let ports = conversation_ports(workflow);
    let ids = |flag: &str, list: Vec<String>| -> String {
        if list.is_empty() {
            format!("  {flag:<8} (none)")
        } else {
            let shown: Vec<String> = list.iter().map(|id| format!("--{flag} {id}")).collect();
            format!("  {flag:<8} {}", shown.join(", "))
        }
    };
    let agents = agent_nodes(workflow).iter().map(|n| n.id.clone()).collect();
    let inputs = ports.inputs.iter().map(|p| p.id.clone()).collect();
    let outputs = ports.outputs.iter().map(|p| p.id.clone()).collect();
    [ids("node", agents), ids("in", inputs), ids("out", outputs)].join("\n")
}

// ── Records: an agent's history and a workflow's bus in one shape ─────────────────────

/// One thing of a conversation, in the order its `seq` gives. `event` is a
/// `SessionEvent` of `agent.engine@1` as JSON (`kind` plus its fields).
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Entry {
        seq: u64,
        role: String,
        text: String,
        turn: Option<String>,
    },
    Activity {
        seq: u64,
        turn: Option<String>,
        /// The node it came from; `None`: the agent talked to.
        node: Option<String>,
        event: Value,
    },
}

impl Record {
    pub fn seq(&self) -> u64 {
        match self {
            Record::Entry { seq, .. } | Record::Activity { seq, .. } => *seq,
        }
    }
}

fn text_of(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

/// An `agent.history` answer as records: entries and activity, one `seq` for both.
pub fn history_records(page: &Value) -> Vec<Record> {
    let mut records: Vec<Record> = Vec::new();
    for entry in page["entries"].as_array().into_iter().flatten() {
        records.push(Record::Entry {
            seq: entry["seq"].as_u64().unwrap_or_default(),
            role: text_of(entry, "role"),
            text: text_of(entry, "text"),
            turn: entry["turn"].as_str().map(str::to_string),
        });
    }
    for item in page["activity"].as_array().into_iter().flatten() {
        if item["event"]["kind"].is_string() {
            records.push(Record::Activity {
                seq: item["seq"].as_u64().unwrap_or_default(),
                turn: item["turn"].as_str().map(str::to_string),
                node: None,
                event: item["event"].clone(),
            });
        }
    }
    records.sort_by_key(Record::seq);
    records
}

/// A bus value as text: text as it is, anything else as JSON.
pub fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The bus events of a bound workflow as records: emitted from `self.<input>` is a turn of
/// the person (what `workflow.send` records), from `self.<output>` a reply, and an emitted
/// `agent.event` (an agent box's `activity`) is activity of the node it left.
pub fn bus_records(events: &[BusEvent], binding: &Binding) -> Vec<Record> {
    let mut records = Vec::new();
    for event in events {
        let BusEventKind::Emitted { value } = &event.kind else {
            continue;
        };
        let from = &event.from;
        let own = from.node_id == SELF;
        if own && from.port_id == binding.input {
            records.push(Record::Entry {
                seq: event.seq,
                role: USER.into(),
                text: value_text(value),
                turn: None,
            });
        } else if own && from.port_id == binding.output {
            records.push(Record::Entry {
                seq: event.seq,
                role: AGENT.into(),
                text: value_text(value),
                turn: None,
            });
        } else if base(&event.capability) == ACTIVITY && value["kind"].is_string() {
            records.push(Record::Activity {
                seq: event.seq,
                turn: None,
                node: Some(from.node_id.clone()),
                event: value.clone(),
            });
        }
    }
    records
}

/// Adds a page to what is kept: no duplicates, in `seq` order, the newest `keep` only
/// (0 keeps everything).
pub fn merge(current: &mut Vec<Record>, incoming: Vec<Record>, keep: usize) {
    if incoming.is_empty() {
        return;
    }
    let fresh: HashSet<u64> = incoming.iter().map(Record::seq).collect();
    current.retain(|r| !fresh.contains(&r.seq()));
    current.extend(incoming);
    current.sort_by_key(Record::seq);
    if keep > 0 && current.len() > keep {
        let extra = current.len() - keep;
        current.drain(..extra);
    }
}

// ── Permissions ────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionOption {
    pub id: String,
    pub kind: String,
    pub name: String,
}

/// A `permission_request` of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    pub seq: u64,
    pub id: String,
    pub node: Option<String>,
    pub title: String,
    pub tool: Option<String>,
    pub options: Vec<PermissionOption>,
}

pub fn read_options(value: &Value) -> Vec<PermissionOption> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let id = o["id"].as_str()?.to_string();
            Some(PermissionOption {
                kind: text_of(o, "kind"),
                name: o["name"].as_str().unwrap_or(&id).to_string(),
                id,
            })
        })
        .collect()
}

fn permission_of(seq: u64, node: &Option<String>, event: &Value) -> Option<PermissionRequest> {
    let id = event["id"].as_str()?.to_string();
    Some(PermissionRequest {
        seq,
        title: event["title"].as_str().unwrap_or(&id).to_string(),
        tool: event["tool"].as_str().map(str::to_string),
        options: read_options(&event["options"]),
        node: node.clone(),
        id,
    })
}

/// The permission requests no `permission_resolved` answered yet, oldest first.
pub fn open_permissions(records: &[Record]) -> Vec<PermissionRequest> {
    let mut open: Vec<PermissionRequest> = Vec::new();
    for record in records {
        let Record::Activity {
            seq, node, event, ..
        } = record
        else {
            continue;
        };
        match event["kind"].as_str() {
            Some("permission_request") => {
                if let Some(request) = permission_of(*seq, node, event) {
                    open.retain(|r| !(r.id == request.id && r.node == request.node));
                    open.push(request);
                }
            }
            Some("permission_resolved") => {
                let id = event["id"].as_str().unwrap_or_default();
                open.retain(|r| !(r.id == id && &r.node == node));
            }
            _ => {}
        }
    }
    open
}

/// How a person answers a permission request through `human.answer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// `approve` with `fields.option`.
    Option(String),
    /// `reject`: the decider picks its reject option.
    Reject,
}

/// What a word answers: an option's id; `allow` is the first option of an `allow…` kind;
/// `reject` the first `reject…` one, or a plain rejection when there is none.
pub fn answer_for(word: &str, options: &[PermissionOption]) -> Option<Answer> {
    if let Some(option) = options.iter().find(|o| o.id == word) {
        return Some(Answer::Option(option.id.clone()));
    }
    let of_kind = |prefix: &str| options.iter().find(|o| o.kind.starts_with(prefix));
    match word {
        "allow" => of_kind("allow").map(|o| Answer::Option(o.id.clone())),
        "reject" => Some(
            of_kind("reject")
                .map(|o| Answer::Option(o.id.clone()))
                .unwrap_or(Answer::Reject),
        ),
        _ => None,
    }
}

/// Whether a pending row of `human.list` (`kind`, `value`) is the decider's question about
/// this request: a `decision` whose `value.context` names the execution, node and request.
pub fn asks_about(kind: &str, value: &Value, execution: &str, node: &str, request: &str) -> bool {
    let context = &value["context"];
    kind == "decision"
        && context["execution_id"].as_str() == Some(execution)
        && context["node_id"].as_str() == Some(node)
        && context["request_id"].as_str() == Some(request)
}

// ── Reading a session event ────────────────────────────────────────────────────────────

/// One session event (`agent.engine@1`) in a line, as `regulus agent history` prints it.
pub fn describe_event(event: &Value) -> String {
    let text = |key: &str| text_of(event, key);
    match event["kind"].as_str().unwrap_or_default() {
        "text" => format!("text {:?}", text("text")),
        "thought" => format!("thought {:?}", text("text")),
        "tool_call" => format!("tool {} {} ({})", text("id"), text("title"), text("status")),
        "tool_update" => format!("tool {} {}", text("id"), text("status")),
        "plan" => format!("plan {}", event["entries"]),
        "permission_request" => {
            let options: Vec<String> = read_options(&event["options"])
                .into_iter()
                .map(|o| o.id)
                .collect();
            format!(
                "permission {} asked: {} [{}]",
                text("id"),
                text("title"),
                options.join(" | ")
            )
        }
        "permission_resolved" => format!(
            "permission {} answered: {}",
            text("id"),
            event["option"].as_str().unwrap_or("cancelled")
        ),
        "turn_started" => "turn started".into(),
        "turn_ended" => format!("turn ended ({})", text("stop_reason")),
        "state" => format!("state {}", text("state")),
        other => format!("{other} {event}"),
    }
}

// ── Turns as cards ─────────────────────────────────────────────────────────────────────

/// How records fall into turns. `Turn`: by the box's turn id (`agent.history`); a record
/// without one stands alone. `Sequence`: each turn of the person opens one and what follows
/// joins it (a workflow's bus has no turn ids).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grouping {
    Turn,
    Sequence,
}

/// One card of a turn. `node` is where activity came from (`None`: the agent talked to).
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Entry {
        seq: u64,
        role: String,
        text: String,
    },
    /// Text chunks of a node joined, until the answer replaces them.
    Live {
        seq: u64,
        node: Option<String>,
        text: String,
    },
    Thought {
        seq: u64,
        node: Option<String>,
        text: String,
    },
    Tool {
        seq: u64,
        node: Option<String>,
        id: String,
        title: String,
        tool_kind: Option<String>,
        status: String,
        content: Option<Value>,
    },
    Plan {
        seq: u64,
        node: Option<String>,
        entries: Value,
    },
    Permission {
        request: PermissionRequest,
        /// `Some(None)`: cancelled.
        resolved: Option<Option<String>>,
    },
    Ended {
        seq: u64,
        node: Option<String>,
        stop_reason: String,
    },
    State {
        seq: u64,
        node: Option<String>,
        state: String,
    },
    Other {
        seq: u64,
        node: Option<String>,
        name: String,
        raw: Value,
    },
}

impl Item {
    pub fn seq(&self) -> u64 {
        match self {
            Item::Entry { seq, .. }
            | Item::Live { seq, .. }
            | Item::Thought { seq, .. }
            | Item::Tool { seq, .. }
            | Item::Plan { seq, .. }
            | Item::Ended { seq, .. }
            | Item::State { seq, .. }
            | Item::Other { seq, .. } => *seq,
            Item::Permission { request, .. } => request.seq,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnView {
    pub items: Vec<Item>,
    /// A turn of the person with no answer yet.
    pub open: bool,
}

/// Records in turns, the way `groupTurns` of the app does it.
fn group(records: &[Record], grouping: Grouping) -> Vec<Vec<&Record>> {
    let mut sorted: Vec<&Record> = records.iter().collect();
    sorted.sort_by_key(|r| r.seq());
    let mut groups: Vec<(String, Vec<&Record>)> = Vec::new();
    let mut current = "start".to_string();
    for record in sorted {
        let key = match (grouping, record) {
            (
                Grouping::Turn,
                Record::Entry { turn, seq, .. } | Record::Activity { turn, seq, .. },
            ) => turn.clone().unwrap_or_else(|| format!("seq-{seq}")),
            (Grouping::Sequence, Record::Entry { role, seq, .. }) if role == USER => {
                current = format!("u-{seq}");
                current.clone()
            }
            (Grouping::Sequence, _) => current.clone(),
        };
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(record),
            None => groups.push((key, vec![record])),
        }
    }
    groups.into_iter().map(|(_, list)| list).collect()
}

/// A turn's records as cards: text chunks join into one live bubble per node until an
/// answer arrives, consecutive thoughts join, a tool call and its updates are one card,
/// the latest plan replaces the earlier, a permission request shows how it was resolved.
fn reduce_turn(records: &[&Record]) -> TurnView {
    let mut items: Vec<Item> = Vec::new();
    let mut asked = false;
    let mut answered = false;
    for record in records {
        let (seq, node, event) = match record {
            Record::Entry {
                seq, role, text, ..
            } => {
                items.push(Item::Entry {
                    seq: *seq,
                    role: role.clone(),
                    text: text.clone(),
                });
                if role == USER {
                    asked = true;
                } else {
                    answered = true;
                }
                continue;
            }
            Record::Activity {
                seq, node, event, ..
            } => (*seq, node, event),
        };
        let text = |key: &str| text_of(event, key);
        match event["kind"].as_str().unwrap_or_default() {
            "turn_started" => {}
            "text" => {
                let live = items
                    .iter_mut()
                    .find(|i| matches!(i, Item::Live { node: n, .. } if n == node));
                match live {
                    Some(Item::Live { text: t, .. }) => t.push_str(&text("text")),
                    _ => items.push(Item::Live {
                        seq,
                        node: node.clone(),
                        text: text("text"),
                    }),
                }
            }
            "thought" => {
                let last = items
                    .iter_mut()
                    .rev()
                    .find(|i| !matches!(i, Item::Live { .. }));
                match last {
                    Some(Item::Thought {
                        node: n, text: t, ..
                    }) if n == node => t.push_str(&text("text")),
                    _ => items.push(Item::Thought {
                        seq,
                        node: node.clone(),
                        text: text("text"),
                    }),
                }
            }
            kind @ ("tool_call" | "tool_update") => {
                let id = text("id");
                let found = items.iter_mut().find(
                    |i| matches!(i, Item::Tool { node: n, id: x, .. } if n == node && *x == id),
                );
                let status = text("status");
                let content = event.get("content").filter(|c| !c.is_null()).cloned();
                match found {
                    Some(Item::Tool {
                        title,
                        tool_kind,
                        status: s,
                        content: c,
                        ..
                    }) => {
                        if kind == "tool_call" {
                            *title = event["title"].as_str().unwrap_or(&id).to_string();
                            if let Some(k) = event["tool_kind"].as_str() {
                                *tool_kind = Some(k.to_string());
                            }
                        }
                        if !status.is_empty() {
                            *s = status;
                        }
                        if content.is_some() {
                            *c = content;
                        }
                    }
                    _ => items.push(Item::Tool {
                        seq,
                        node: node.clone(),
                        title: event["title"].as_str().unwrap_or(&id).to_string(),
                        tool_kind: event["tool_kind"].as_str().map(str::to_string),
                        id,
                        status,
                        content,
                    }),
                }
            }
            "plan" => {
                let found = items
                    .iter_mut()
                    .find(|i| matches!(i, Item::Plan { node: n, .. } if n == node));
                match found {
                    Some(Item::Plan { entries, .. }) => *entries = event["entries"].clone(),
                    _ => items.push(Item::Plan {
                        seq,
                        node: node.clone(),
                        entries: event["entries"].clone(),
                    }),
                }
            }
            "permission_request" => {
                let Some(request) = permission_of(seq, node, event) else {
                    continue;
                };
                let found = items.iter_mut().find(|i| {
                    matches!(i, Item::Permission { request: r, .. } if r.node == *node && r.id == request.id)
                });
                match found {
                    Some(Item::Permission { request: r, .. }) => {
                        r.title = request.title;
                        r.options = request.options;
                        r.tool = request.tool.or(r.tool.take());
                    }
                    _ => items.push(Item::Permission {
                        request,
                        resolved: None,
                    }),
                }
            }
            "permission_resolved" => {
                let id = text("id");
                let option = event["option"].as_str().map(str::to_string);
                let found = items.iter_mut().find(|i| {
                    matches!(i, Item::Permission { request: r, .. } if r.node == *node && r.id == id)
                });
                match found {
                    Some(Item::Permission { resolved, .. }) => *resolved = Some(option),
                    _ => items.push(Item::Permission {
                        request: PermissionRequest {
                            seq,
                            title: id.clone(),
                            id,
                            node: node.clone(),
                            tool: None,
                            options: Vec::new(),
                        },
                        resolved: Some(option),
                    }),
                }
            }
            "turn_ended" => items.push(Item::Ended {
                seq,
                node: node.clone(),
                stop_reason: text("stop_reason"),
            }),
            "state" => items.push(Item::State {
                seq,
                node: node.clone(),
                state: text("state"),
            }),
            other => items.push(Item::Other {
                seq,
                node: node.clone(),
                name: other.to_string(),
                raw: event.clone(),
            }),
        }
    }
    // The answer is the text: the stream it came from goes once it is there.
    if answered {
        items.retain(|i| !matches!(i, Item::Live { .. }));
    }
    items.sort_by_key(Item::seq);
    TurnView {
        items,
        open: asked && !answered,
    }
}

pub fn reduce(records: &[Record], grouping: Grouping) -> Vec<TurnView> {
    group(records, grouping)
        .iter()
        .map(|list| reduce_turn(list))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workflow(ports: Value, nodes: Value) -> Workflow {
        serde_json::from_value(json!({
            "id": "main", "kind": "main", "ports": ports, "nodes": nodes,
        }))
        .unwrap()
    }

    fn port(id: &str, capability: &str, direction: &str, kind: &str) -> Value {
        json!({ "id": id, "capability": capability, "direction": direction, "kind": kind })
    }

    fn agent(id: &str) -> Value {
        json!({ "id": id, "type": "agent", "ports": [port("engine", "agent.engine@1", "input", "uses")] })
    }

    fn one_pair() -> Workflow {
        workflow(
            json!([
                port("ask", "text@1", "input", "then"),
                port("answer", "text@1", "output", "then"),
                port("data", "json@1", "input", "then"),
            ]),
            json!([]),
        )
    }

    #[test]
    fn a_delegate_engine_for_its_workers_is_not_a_second_chat_agent() {
        let mut workers = agent("workers");
        workers["ports"][0]["lazy"] = json!(true);
        let flow = workflow(json!([]), json!([agent("dev"), workers]));
        assert_eq!(
            choose(&flow, "p", Asked::default(), &BTreeMap::new(), &[]),
            Choice::Talk(Subject::Agent("dev".into()), Origin::OnlyAgent)
        );
    }

    #[test]
    fn a_workflow_is_proposed_never_bound_and_an_only_agent_needs_nothing() {
        let none = BTreeMap::new();
        let pair = Binding {
            input: "ask".into(),
            output: "answer".into(),
        };
        assert_eq!(
            choose(&one_pair(), "p", Asked::default(), &none, &[]),
            Choice::Propose(pair.clone())
        );
        let with_agent = workflow(json!([]), json!([agent("dev")]));
        assert_eq!(
            choose(&with_agent, "p", Asked::default(), &none, &[]),
            Choice::Talk(Subject::Agent("dev".into()), Origin::OnlyAgent)
        );
        // Bound once: the binding wins over the only agent.
        let mut both = one_pair();
        both.nodes = with_agent.nodes.clone();
        let mut own = BTreeMap::new();
        own.insert(binding_key("p", "main"), pair.clone());
        assert_eq!(
            choose(&both, "p", Asked::default(), &own, &[]),
            Choice::Talk(Subject::Workflow(pair), Origin::Bound(BindingSource::Own))
        );
    }

    #[test]
    fn more_than_one_candidate_is_refused_with_the_flags_to_use() {
        let two = workflow(
            json!([
                port("ask", "text@1", "input", "then"),
                port("topic", "text@1", "input", "then"),
                port("answer", "text@1", "output", "then"),
            ]),
            json!([]),
        );
        let Choice::Refuse(said) = choose(&two, "p", Asked::default(), &BTreeMap::new(), &[])
        else {
            panic!()
        };
        assert!(said.contains("--in ask, --in topic"), "{said}");
        assert!(said.contains("--out answer"), "{said}");
        // --in alone is enough when the output is the only one.
        let asked = Asked {
            input: Some("topic"),
            ..Asked::default()
        };
        assert_eq!(
            choose(&two, "p", asked, &BTreeMap::new(), &[]),
            Choice::Talk(
                Subject::Workflow(Binding {
                    input: "topic".into(),
                    output: "answer".into()
                }),
                Origin::Flags
            )
        );
        let wrong = Asked {
            input: Some("data"),
            output: Some("answer"),
            ..Asked::default()
        };
        assert!(matches!(
            choose(&one_pair(), "p", wrong, &BTreeMap::new(), &[]),
            Choice::Refuse(e) if e.contains("`data` is not a then input")
        ));
        let agents = workflow(json!([]), json!([agent("a"), agent("b")]));
        assert!(matches!(
            choose(&agents, "p", Asked::default(), &BTreeMap::new(), &[]),
            Choice::Refuse(e) if e.contains("--node a, --node b")
        ));
        let not_agent = Asked {
            node: Some("x"),
            ..Asked::default()
        };
        let plain = workflow(json!([]), json!([{ "id": "x", "type": "x" }]));
        assert!(matches!(
            choose(&plain, "p", not_agent, &BTreeMap::new(), &[]),
            Choice::Refuse(e) if e.contains("not an agent")
        ));
    }

    #[test]
    fn config_bindings_count_only_while_their_ports_exist() {
        let client: serde_json::Map<String, Value> = serde_json::from_value(json!({
            "operate": { "conversations": [
                { "profile": "p", "workflow": "main", "input": "ask", "output": "answer" },
                { "profile": "p", "workflow": "main", "input": "gone" },
                { "profile": "q", "workflow": "main", "input": "x", "output": "y" }
            ] }
        }))
        .unwrap();
        let configured = read_configured(&client);
        assert_eq!(configured.len(), 2);
        let found = binding_for(&BTreeMap::new(), &configured, "p", &one_pair());
        assert_eq!(found.map(|(_, s)| s), Some(BindingSource::Config));
        assert_eq!(
            binding_for(&BTreeMap::new(), &configured, "q", &one_pair()),
            None
        );
        // Own wins; an own binding whose ports are gone falls back to config.
        let mut own = BTreeMap::new();
        own.insert(
            binding_key("p", "main"),
            Binding {
                input: "nope".into(),
                output: "answer".into(),
            },
        );
        let found = binding_for(&own, &configured, "p", &one_pair());
        assert_eq!(found.map(|(_, s)| s), Some(BindingSource::Config));
    }

    #[test]
    fn bindings_are_kept_in_the_home_and_a_broken_file_is_not_overwritten() {
        let home = std::env::temp_dir().join(format!("regulus-bindings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut bindings = Bindings::load(&home).unwrap();
        assert!(bindings.own.is_empty());
        bindings.set(
            "p",
            "main",
            Binding {
                input: "ask".into(),
                output: "answer".into(),
            },
        );
        bindings.save().unwrap();
        let again = Bindings::load(&home).unwrap();
        assert_eq!(again.own["p/main"].input, "ask");
        let mut again = again;
        assert!(again.forget("p", "main"));
        assert!(!again.forget("p", "main"));
        std::fs::write(home.join(BINDINGS_FILE), "{ nope").unwrap();
        assert!(Bindings::load(&home).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    fn activity(seq: u64, turn: &str, event: Value) -> Value {
        json!({ "seq": seq, "at": 0, "turn": turn, "event": event })
    }

    fn history() -> Value {
        json!({
            "entries": [
                { "seq": 1, "role": "user", "text": "ask", "turn": "t-1" },
                { "seq": 9, "role": "agent", "text": "chose yes", "turn": "t-1" }
            ],
            "activity": [
                activity(2, "t-1", json!({ "kind": "turn_started" })),
                activity(3, "t-1", json!({ "kind": "text", "text": "let me " })),
                activity(4, "t-1", json!({ "kind": "tool_call", "id": "c1", "title": "read",
                                          "tool_kind": "read", "status": "pending" })),
                activity(5, "t-1", json!({ "kind": "tool_update", "id": "c1", "status": "completed" })),
                activity(6, "t-1", json!({ "kind": "permission_request", "id": "p1",
                    "title": "write a file", "options": [
                        { "id": "yes", "kind": "allow_once", "name": "Allow" },
                        { "id": "no", "kind": "reject_once", "name": "Reject" }] })),
                activity(7, "t-1", json!({ "kind": "permission_resolved", "id": "p1", "option": "yes" })),
                activity(8, "t-1", json!({ "kind": "text", "text": "see" })),
                activity(10, "t-1", json!({ "kind": "turn_ended", "stop_reason": "end_turn" }))
            ],
            "next": 10
        })
    }

    #[test]
    fn a_history_reduces_to_cards_and_the_answer_replaces_the_stream() {
        let records = history_records(&history());
        assert_eq!(records.len(), 10);
        let turns = reduce(&records, Grouping::Turn);
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert!(!turn.open);
        let kinds: Vec<&str> = turn
            .items
            .iter()
            .map(|i| match i {
                Item::Entry { .. } => "entry",
                Item::Live { .. } => "live",
                Item::Tool { .. } => "tool",
                Item::Permission { .. } => "permission",
                Item::Ended { .. } => "ended",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["entry", "tool", "permission", "entry", "ended"]);
        assert!(matches!(&turn.items[1], Item::Tool { status, .. } if status == "completed"));
        assert!(
            matches!(&turn.items[2], Item::Permission { resolved: Some(Some(o)), .. } if o == "yes")
        );
        // Without the answer yet: the live bubble holds the chunks and the turn is open.
        let partial: Vec<Record> = records.iter().filter(|r| r.seq() < 9).cloned().collect();
        let turn = &reduce(&partial, Grouping::Turn)[0];
        assert!(turn.open);
        assert!(turn
            .items
            .iter()
            .any(|i| matches!(i, Item::Live { text, .. } if text == "let me see")));
        assert_eq!(open_permissions(&partial).len(), 0);
        let asking: Vec<Record> = records.iter().filter(|r| r.seq() < 7).cloned().collect();
        let open = open_permissions(&asking);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "p1");
        assert_eq!(open[0].options.len(), 2);
    }

    #[test]
    fn words_answer_permissions_by_id_or_kind() {
        let options = read_options(&json!([
            { "id": "yes", "kind": "allow_once", "name": "Allow" },
            { "id": "always", "kind": "allow_always", "name": "Always" },
            { "id": "no", "kind": "reject_once", "name": "Reject" }
        ]));
        assert_eq!(
            answer_for("always", &options),
            Some(Answer::Option("always".into()))
        );
        assert_eq!(
            answer_for("allow", &options),
            Some(Answer::Option("yes".into()))
        );
        assert_eq!(
            answer_for("reject", &options),
            Some(Answer::Option("no".into()))
        );
        assert_eq!(answer_for("maybe", &options), None);
        assert_eq!(answer_for("reject", &options[..2]), Some(Answer::Reject));
        let value =
            json!({ "context": { "execution_id": "e", "node_id": "a", "request_id": "p1" } });
        assert!(asks_about("decision", &value, "e", "a", "p1"));
        assert!(!asks_about("approval", &value, "e", "a", "p1"));
        assert!(!asks_about("decision", &value, "e", "a", "p2"));
    }

    fn bus(seq: u64, node: &str, port: &str, capability: &str, value: Value) -> BusEvent {
        serde_json::from_value(json!({
            "seq": seq, "at": 0, "execution_id": "e", "project_id": null, "event": seq,
            "from": { "node_id": node, "port_id": port }, "capability": capability,
            "depth": 0, "kind": "emitted", "value": value
        }))
        .unwrap()
    }

    #[test]
    fn a_bound_workflow_reads_from_its_bus_in_sequence() {
        let binding = Binding {
            input: "ask".into(),
            output: "answer".into(),
        };
        let events = vec![
            bus(1, "self", "ask", "text@1", json!("hi")),
            bus(
                2,
                "a",
                "activity",
                "agent.event@1",
                json!({ "kind": "text", "text": "he" }),
            ),
            bus(3, "a", "reply", "text@1", json!("hello")),
            bus(4, "self", "answer", "text@1", json!("hello")),
            bus(5, "self", "ask", "text@1", json!("again")),
        ];
        let mut records = bus_records(&events, &binding);
        assert_eq!(records.len(), 4, "the inner reply is not part of it");
        let turns = reduce(&records, Grouping::Sequence);
        assert_eq!(turns.len(), 2);
        assert!(!turns[0].open);
        assert!(turns[1].open);
        // Merging a page twice keeps one of each, and `keep` the newest.
        merge(&mut records, bus_records(&events[3..], &binding), 0);
        assert_eq!(records.len(), 4);
        merge(&mut records, Vec::new(), 2);
        merge(&mut records, bus_records(&events[4..], &binding), 2);
        assert_eq!(
            records.iter().map(Record::seq).collect::<Vec<_>>(),
            vec![4, 5]
        );
    }
}
