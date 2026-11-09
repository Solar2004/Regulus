//! `regulus talk`: a conversation from the terminal, line by line (docs/04-client.md §7,
//! docs/12-agents.md §5). A direct agent is talked to with `agent.send` and read with
//! `agent.history` from its `seq` cursor; a workflow through the binding a person chose,
//! with `workflow.send` and `event.list`. Permission requests are answered with
//! `human.answer` on the decision a Decider opened for them. Nothing new in the core.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufRead, IsTerminal, Write};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use jsonrpsee::ws_client::WsClient;
use regulus_core::Config;
use regulus_proto::{
    method, BusEventKind, ClientSettings, EventPage, Execution, Profile, WorkflowSent,
    WORKFLOW_SELF,
};
use serde_json::{json, Value};

use crate::conversation::{
    answer_for, asks_about, bus_records, choose, describe_event, history_records, merge,
    open_permissions, read_configured, Answer, Asked, Binding, BindingSource, Bindings, Choice,
    Origin, Record, Subject, AGENT, SYSTEM, USER,
};
use crate::rpc;
use crate::tui::keys::Options;

const AGENT_SEND: &str = "agent.send";
const AGENT_HISTORY: &str = "agent.history";
const AGENT_CANCEL: &str = "agent.cancel";

/// What `regulus talk` was asked.
#[derive(Debug, Clone, Default)]
pub struct Talk {
    pub execution: String,
    pub node: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    /// Accept a proposed binding without asking.
    pub yes: bool,
    /// Send this, print the reply and exit.
    pub once: Option<String>,
}

/// The numbers it runs with, from the same place the terminal UI reads them.
#[derive(Debug, Clone)]
pub struct Settings {
    /// How often to ask for more: `client.refresh_ms`.
    pub poll: Duration,
    /// `limit` of each `agent.history` or `event.list` call: `tui.event_page`.
    pub page: usize,
    /// Records kept for `/history`: `tui.conversation_keep`.
    pub keep: usize,
    /// Characters of an activity line: `tui.value_max`.
    pub value_max: usize,
    /// How long `--once` waits for the reply, and the end of input for the turn in flight.
    pub timeout: Duration,
}

impl Settings {
    pub fn new(config: &Config, options: &Options, timeout: Duration) -> Self {
        Self {
            poll: Duration::from_millis(config.client.refresh_ms.max(1)),
            page: options.tuning.event_page,
            keep: options.tuning.conversation_keep,
            value_max: options.tuning.value_max,
            timeout,
        }
    }
}

/// Runs the conversation `talk` asks for on `ws`, until `/quit`, Ctrl-C twice, the end
/// of input or, with `--once`, the reply.
pub async fn run(ws: &WsClient, config: &Config, settings: &Settings, talk: Talk) -> Result<()> {
    let executions: Vec<Execution> = rpc::call(ws, method::EXECUTION_LIST, Value::Null).await?;
    let execution = executions
        .into_iter()
        .find(|e| e.id == talk.execution)
        .with_context(|| format!("no execution `{}` (see `regulus ps`)", talk.execution))?;
    let profile: Profile = rpc::call(
        ws,
        method::PROFILE_GET,
        json!({ "id": execution.profile_id }),
    )
    .await?;
    let workflow = profile
        .workflow(&execution.workflow_id)
        .cloned()
        .with_context(|| {
            format!(
                "profile `{}` has no workflow `{}` any more",
                profile.id, execution.workflow_id
            )
        })?;
    let configured = read_configured(&client_settings(ws, execution.project_id.as_deref()).await);
    let mut bindings = Bindings::load(&config.home).map_err(anyhow::Error::msg)?;
    let asked = Asked {
        node: talk.node.as_deref(),
        input: talk.input.as_deref(),
        output: talk.output.as_deref(),
    };
    let subject = match choose(&workflow, &profile.id, asked, &bindings.own, &configured) {
        Choice::Refuse(why) => bail!("{why}"),
        Choice::Talk(subject, origin) => {
            say_subject(&subject, origin, &workflow.id);
            if let (Subject::Workflow(binding), Origin::Flags) = (&subject, origin) {
                remember(&mut bindings, &profile.id, &workflow.id, binding)?;
            }
            subject
        }
        Choice::Propose(binding) => {
            let question = format!(
                "workflow `{}` takes text on {WORKFLOW_SELF}.{} and answers on \
                 {WORKFLOW_SELF}.{}",
                workflow.id, binding.input, binding.output
            );
            if !talk.yes && !confirm(&question)? {
                bail!(
                    "{question}; nothing is bound until you say so: pass --yes, or --in {} \
                     --out {}",
                    binding.input,
                    binding.output
                );
            }
            remember(&mut bindings, &profile.id, &workflow.id, &binding)?;
            Subject::Workflow(binding)
        }
    };
    let mut session = match subject {
        Subject::Agent(node) => Session::Agent(AgentTalk {
            node,
            turns: BTreeSet::new(),
            sent: HashSet::new(),
            state: String::new(),
            session: Value::Null,
        }),
        Subject::Workflow(binding) => Session::Flow(FlowTalk {
            binding,
            sent: BTreeSet::new(),
            idle: true,
            failures: Vec::new(),
        }),
    };
    let mut talking = Talking {
        ws,
        settings,
        execution: execution.id.clone(),
        since: 0,
        records: Vec::new(),
    };
    // What was said before: the transcript for the REPL, only the cursor for `--once`.
    let earlier = talking.catch_up(&mut session).await?;
    match talk.once {
        Some(text) => once(&mut talking, &mut session, &text).await,
        None => {
            let mut printer = Printer::new(settings.value_max, &session);
            if let Session::Agent(agent) = &session {
                let s = &agent.session;
                let resume = if s["native_resume"].as_bool() == Some(true) {
                    "resumed natively"
                } else {
                    "not resumed: the history is Regulus' record"
                };
                eprintln!(
                    "session {} · {} · {resume}",
                    s["engine_session_id"].as_str().unwrap_or("-"),
                    s["state"].as_str().unwrap_or("unknown")
                );
            }
            out(&printer.print_all(&earlier, &HashSet::new(), &BTreeSet::new()));
            eprintln!("{HELP_LINE}");
            repl(&mut talking, &mut session, &mut printer).await
        }
    }
}

const HELP_LINE: &str = "type to send · /help · Ctrl-C cancels the turn, twice quits";
const HELP: &str = "\
/cancel           cancel the turn in flight
/history          print the conversation kept so far
/allow [request]  answer a permission request with its first allow option
/reject [request] answer it with its reject option
/<option> [req]   answer it with that option id (the ids follow `answer:`)
/quit             leave (also /exit, Ctrl-D, Ctrl-C twice)
//text            send `/text`";

/// `config.client` for the execution's project, falling back to the global one; nothing
/// if neither answers.
async fn client_settings(ws: &WsClient, project: Option<&str>) -> serde_json::Map<String, Value> {
    for filter in [json!({ "project_id": project }), Value::Null] {
        if let Ok(settings) = rpc::call::<ClientSettings>(ws, method::CONFIG_CLIENT, filter).await {
            return settings.client;
        }
        if project.is_none() {
            break;
        }
    }
    serde_json::Map::new()
}

fn say_subject(subject: &Subject, origin: Origin, workflow: &str) {
    match subject {
        Subject::Agent(node) => {
            let why = match origin {
                Origin::OnlyAgent => format!(" (the only agent of workflow `{workflow}`)"),
                _ => String::new(),
            };
            eprintln!("talking to agent `{node}`{why}");
        }
        Subject::Workflow(binding) => {
            let why = match origin {
                Origin::Bound(BindingSource::Own) => " (bound earlier, kept in the home)",
                Origin::Bound(BindingSource::Config) => " (bound in config.client)",
                _ => "",
            };
            eprintln!(
                "talking to workflow `{workflow}`: {WORKFLOW_SELF}.{} → {WORKFLOW_SELF}.{}{why}",
                binding.input, binding.output
            );
        }
    }
}

fn remember(
    bindings: &mut Bindings,
    profile: &str,
    workflow: &str,
    binding: &Binding,
) -> Result<()> {
    bindings.set(profile, workflow, binding.clone());
    bindings.save().map_err(anyhow::Error::msg)?;
    eprintln!(
        "bound {WORKFLOW_SELF}.{} → {WORKFLOW_SELF}.{} for {profile}/{workflow}, kept in {}",
        binding.input,
        binding.output,
        bindings.path.display()
    );
    Ok(())
}

/// Asks on the terminal; anything but yes is no. Without a terminal there is nobody to ask.
fn confirm(question: &str) -> Result<bool> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Ok(false);
    }
    eprint!("{question}: talk to it through them? [y/N] ");
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn out(text: &str) {
    if text.is_empty() {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// A direct agent: its turns are the box's (`t-<n>`).
struct AgentTalk {
    node: String,
    /// Turns sent from here with no answer yet.
    turns: BTreeSet<String>,
    /// Every turn sent from here: its prompt is not printed again.
    sent: HashSet<String>,
    /// The session's state as the last page said it.
    state: String,
    session: Value,
}

/// A bound workflow: its turns are the `self.<input>` events.
struct FlowTalk {
    binding: Binding,
    /// `seq` of each value sent from here.
    sent: BTreeSet<u64>,
    /// The bus had nothing queued for the execution on the last page.
    idle: bool,
    /// Deliveries that failed since the last send.
    failures: Vec<String>,
}

enum Session {
    Agent(AgentTalk),
    Flow(FlowTalk),
}

struct Talking<'a> {
    ws: &'a WsClient,
    settings: &'a Settings,
    execution: String,
    since: u64,
    records: Vec<Record>,
}

impl Talking<'_> {
    /// One page after the cursor; what is new.
    async fn page(&mut self, session: &mut Session) -> Result<(Vec<Record>, bool)> {
        let since = self.since;
        let limit = self.settings.page;
        let fresh = match session {
            Session::Agent(agent) => {
                let params = json!({ "execution_id": self.execution, "node_id": agent.node,
                                     "since": since, "limit": limit });
                let page: Value = rpc::call(self.ws, AGENT_HISTORY, params).await?;
                self.since = page["next"].as_u64().unwrap_or(since).max(since);
                agent.state = page["session"]["state"]
                    .as_str()
                    .unwrap_or("unknown")
                    .into();
                agent.session = page["session"].clone();
                let records = history_records(&page);
                for record in &records {
                    if let Record::Entry {
                        role,
                        turn: Some(turn),
                        ..
                    } = record
                    {
                        if role != USER {
                            agent.turns.remove(turn);
                        }
                    }
                }
                records
            }
            Session::Flow(flow) => {
                let params = json!({ "execution_id": self.execution, "since": since,
                                     "limit": limit });
                let page: EventPage = rpc::call(self.ws, method::EVENT_LIST, params).await?;
                self.since = page.next.max(since);
                flow.idle = page.pending == 0;
                for event in &page.events {
                    if let BusEventKind::Failed { to, error } = &event.kind {
                        flow.failures.push(format!(
                            "{}.{} → {}.{} failed: {error}",
                            event.from.node_id, event.from.port_id, to.node_id, to.port_id
                        ));
                    }
                }
                bus_records(&page.events, &flow.binding)
            }
        };
        merge(&mut self.records, fresh.clone(), self.settings.keep);
        Ok((fresh, self.since == since))
    }

    /// Pages until the cursor stops: everything there is now.
    async fn catch_up(&mut self, session: &mut Session) -> Result<Vec<Record>> {
        let mut all = Vec::new();
        loop {
            let (fresh, caught_up) = self.page(session).await?;
            all.extend(fresh);
            if caught_up {
                return Ok(all);
            }
        }
    }

    async fn send(&mut self, session: &mut Session, text: &str) -> Result<()> {
        match session {
            Session::Agent(agent) => {
                let params = json!({ "execution_id": self.execution, "node_id": agent.node,
                                     "text": text });
                let sent: Value = rpc::call(self.ws, AGENT_SEND, params).await?;
                let turn = sent["turn"].as_str().unwrap_or_default().to_string();
                agent.sent.insert(turn.clone());
                agent.turns.insert(turn);
            }
            Session::Flow(flow) => {
                let params = json!({ "execution_id": self.execution,
                                     "port": flow.binding.input, "value": text });
                let sent: WorkflowSent = rpc::call(self.ws, method::WORKFLOW_SEND, params).await?;
                flow.sent.insert(sent.event);
                flow.failures.clear();
            }
        }
        Ok(())
    }

    /// Asks the agent to end its turn; in a workflow, every agent that worked on the open
    /// turn. Says what it did.
    async fn cancel(&mut self, session: &Session) -> Result<String> {
        let nodes: Vec<String> = match session {
            Session::Agent(agent) => vec![agent.node.clone()],
            Session::Flow(_) => {
                let last_turn = self
                    .records
                    .iter()
                    .rev()
                    .find(|r| matches!(r, Record::Entry { role, .. } if role == USER))
                    .map_or(0, Record::seq);
                let mut nodes: Vec<String> = Vec::new();
                for record in &self.records {
                    if let Record::Activity {
                        seq,
                        node: Some(node),
                        ..
                    } = record
                    {
                        if *seq > last_turn && !nodes.contains(node) {
                            nodes.push(node.clone());
                        }
                    }
                }
                nodes
            }
        };
        if nodes.is_empty() {
            return Ok("no agent is working on this turn".into());
        }
        for node in &nodes {
            let params = json!({ "execution_id": self.execution, "node_id": node });
            let _: Value = rpc::call(self.ws, AGENT_CANCEL, params).await?;
        }
        Ok(format!("asked {} to cancel the turn", nodes.join(", ")))
    }

    fn in_flight(&self, session: &Session) -> bool {
        match session {
            Session::Agent(agent) => !agent.turns.is_empty() || agent.state == "busy",
            Session::Flow(flow) => {
                let last = |user: bool| {
                    self.records
                        .iter()
                        .rev()
                        .find(|r| matches!(r, Record::Entry { role, .. } if (role == USER) == user))
                        .map_or(0, Record::seq)
                };
                let unseen = flow.sent.iter().any(|seq| *seq > self.since);
                unseen || last(true) > last(false)
            }
        }
    }

    /// The node a permission request without a node belongs to.
    fn node_of(&self, session: &Session, node: Option<&str>) -> Option<String> {
        match (node, session) {
            (Some(node), _) => Some(node.to_string()),
            (None, Session::Agent(agent)) => Some(agent.node.clone()),
            (None, Session::Flow(_)) => None,
        }
    }

    async fn alive(&self) -> Result<bool> {
        let executions: Vec<Execution> =
            rpc::call(self.ws, method::EXECUTION_LIST, Value::Null).await?;
        Ok(executions
            .iter()
            .any(|e| e.id == self.execution && e.state.is_live()))
    }

    /// Answers the oldest open permission request (or `request`) with `word`.
    async fn answer(
        &mut self,
        session: &Session,
        word: &str,
        request: Option<&str>,
    ) -> Result<String> {
        let open = open_permissions(&self.records);
        let Some(asking) = open.iter().find(|r| request.is_none_or(|id| r.id == id)) else {
            bail!(match request {
                Some(id) => format!("no open permission request `{id}`"),
                None => format!("unknown command `/{word}`: /help lists them"),
            });
        };
        let Some(answer) = answer_for(word, &asking.options) else {
            let ids: Vec<&str> = asking.options.iter().map(|o| o.id.as_str()).collect();
            bail!(
                "`{word}` answers nothing of request {}: its options are {}, or allow / reject",
                asking.id,
                ids.join(", ")
            );
        };
        let node = self
            .node_of(session, asking.node.as_deref())
            .context("the request does not say its node")?;
        let pending: Vec<Value> = rpc::call(self.ws, regulus_module_human::LIST, json!({})).await?;
        let Some(decision) = pending.iter().find(|p| {
            asks_about(
                p["kind"].as_str().unwrap_or_default(),
                &p["value"],
                &self.execution,
                &node,
                &asking.id,
            )
        }) else {
            bail!(
                "nobody was asked about request {}: no decision waits for it in `human list` \
                 (wire a Decider into the agent's `decisions` input)",
                asking.id
            );
        };
        let id = decision["id"].as_str().unwrap_or_default();
        let (params, said) = match &answer {
            Answer::Option(option) => (
                json!({ "id": id, "decision": "approve", "fields": { "option": option } }),
                option.clone(),
            ),
            Answer::Reject => (json!({ "id": id, "decision": "reject" }), "reject".into()),
        };
        let _: Value = rpc::call(self.ws, regulus_module_human::ANSWER, params).await?;
        Ok(format!("answered {} with {said}", asking.id))
    }
}

/// `--once`: send, wait for the reply, print it; activity goes to stderr.
async fn once(talking: &mut Talking<'_>, session: &mut Session, text: &str) -> Result<()> {
    let deadline = Instant::now() + talking.settings.timeout;
    talking.send(session, text).await?;
    let max = talking.settings.value_max;
    loop {
        let (fresh, caught_up) = talking.page(session).await?;
        for record in &fresh {
            match (record, &*session) {
                (Record::Activity { node, event, .. }, _) => {
                    let kind = event["kind"].as_str().unwrap_or_default();
                    if !matches!(kind, "text" | "thought" | "turn_started" | "state") {
                        let who = node
                            .as_deref()
                            .map(|n| format!("[{n}] "))
                            .unwrap_or_default();
                        eprintln!("  · {who}{}", cut(&describe_event(event), max));
                    }
                }
                (
                    Record::Entry {
                        role, text, turn, ..
                    },
                    Session::Agent(agent),
                ) if role != USER && turn.as_ref().is_some_and(|t| agent.sent.contains(t)) => {
                    if role == SYSTEM {
                        bail!("{text}");
                    }
                    println!("{text}");
                    return Ok(());
                }
                (
                    Record::Entry {
                        role, text, seq, ..
                    },
                    Session::Flow(flow),
                ) if role == AGENT && flow.sent.iter().any(|s| s < seq) => {
                    println!("{text}");
                    return Ok(());
                }
                _ => {}
            }
        }
        if !caught_up {
            continue;
        }
        if let Session::Flow(flow) = &*session {
            if flow.idle && !flow.failures.is_empty() {
                bail!("no reply: {}", flow.failures.join("; "));
            }
        }
        let stopped = match &*session {
            Session::Agent(agent) => agent.state == "stopped",
            Session::Flow(_) => !talking.alive().await?,
        };
        if stopped {
            bail!("execution {} is not running: no reply", talking.execution);
        }
        if Instant::now() >= deadline {
            bail!(
                "no reply within {} s (--timeout)",
                talking.settings.timeout.as_secs()
            );
        }
        tokio::time::sleep(talking.settings.poll).await;
    }
}

async fn repl(
    talking: &mut Talking<'_>,
    session: &mut Session,
    printer: &mut Printer,
) -> Result<()> {
    let (lines_tx, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut tick = tokio::time::interval(talking.settings.poll);
    let mut cancelling = false;
    // Set at the end of input: leave once the turn in flight is answered, or at this time.
    let mut leave_at: Option<Instant> = None;
    loop {
        tokio::select! {
            line = lines.recv() => {
                let Some(line) = line else {
                    if leave_at.is_none() {
                        leave_at = Some(Instant::now() + talking.settings.timeout);
                    }
                    continue;
                };
                match command(&line) {
                    Command::Nothing => {}
                    Command::Quit => return Ok(()),
                    Command::Help => eprintln!("{HELP}"),
                    Command::History => {
                        let mut fresh = Printer::new(talking.settings.value_max, session);
                        out(&printer.close());
                        out(&fresh.print_all(&talking.records, &HashSet::new(), &BTreeSet::new()));
                    }
                    Command::Cancel => match talking.cancel(session).await {
                        Ok(said) => eprintln!("{said}"),
                        Err(e) => eprintln!("regulus: {e:#}"),
                    },
                    Command::Answer(word, request) => {
                        match talking.answer(session, &word, request.as_deref()).await {
                            Ok(said) => eprintln!("{said}"),
                            Err(e) => eprintln!("regulus: {e:#}"),
                        }
                    }
                    Command::Send(text) => {
                        if let Err(e) = talking.send(session, &text).await {
                            eprintln!("regulus: {e:#}");
                        }
                    }
                }
                tick.reset_immediately();
            }
            _ = tick.tick() => {
                match talking.catch_up(session).await {
                    Ok(fresh) => {
                        let (turns, events) = match &*session {
                            Session::Agent(agent) => (agent.sent.clone(), BTreeSet::new()),
                            Session::Flow(flow) => (HashSet::new(), flow.sent.clone()),
                        };
                        out(&printer.print_all(&fresh, &turns, &events));
                    }
                    Err(e) => eprintln!("regulus: {e:#}"),
                }
                if !talking.in_flight(session) {
                    cancelling = false;
                    if leave_at.is_some() {
                        out(&printer.close());
                        return Ok(());
                    }
                }
                if leave_at.is_some_and(|at| Instant::now() >= at) {
                    out(&printer.close());
                    bail!("the input ended and the turn in flight did not finish in time");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                out(&printer.close());
                if cancelling || !talking.in_flight(session) {
                    return Ok(());
                }
                cancelling = true;
                match talking.cancel(session).await {
                    Ok(said) => eprintln!("{said}; Ctrl-C again quits"),
                    Err(e) => eprintln!("regulus: {e:#}"),
                }
            }
        }
    }
}

/// What a typed line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Nothing,
    Send(String),
    Quit,
    Help,
    History,
    Cancel,
    /// A permission answer: the word (`allow`, `reject` or an option id) and, optionally,
    /// the request it answers.
    Answer(String, Option<String>),
}

pub fn command(line: &str) -> Command {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() {
        return Command::Nothing;
    }
    if let Some(text) = line.strip_prefix("//") {
        return Command::Send(format!("/{text}"));
    }
    let Some(rest) = line.trim().strip_prefix('/') else {
        return Command::Send(line.to_string());
    };
    let mut words = rest.split_whitespace();
    let word = words.next().unwrap_or_default().to_string();
    match word.as_str() {
        "quit" | "exit" => Command::Quit,
        "help" => Command::Help,
        "history" => Command::History,
        "cancel" => Command::Cancel,
        _ => Command::Answer(word, words.next().map(str::to_string)),
    }
}

/// Characters past `max` become `…`; 0 cuts nothing.
fn cut(line: &str, max: usize) -> String {
    if max > 0 && line.chars().count() > max {
        let kept: String = line.chars().take(max).collect();
        format!("{kept}…")
    } else {
        line.to_string()
    }
}

/// Turns records into terminal lines: text chunks stream onto one line per speaker, the
/// rest are `·` lines, a permission request says how to answer it.
pub struct Printer {
    max: usize,
    /// Who speaks for the subject: the agent's node, or `self.<output>`.
    speaker: String,
    /// The key of the line being streamed, if one is open.
    live: Option<String>,
    /// What was streamed per key, and whether something else was printed in between.
    streamed: HashMap<String, (String, bool)>,
}

impl Printer {
    fn new(max: usize, session: &Session) -> Self {
        let speaker = match session {
            Session::Agent(agent) => agent.node.clone(),
            Session::Flow(flow) => format!("{WORKFLOW_SELF}.{}", flow.binding.output),
        };
        Self::for_speaker(max, speaker)
    }

    pub fn for_speaker(max: usize, speaker: String) -> Self {
        Self {
            max,
            speaker,
            live: None,
            streamed: HashMap::new(),
        }
    }

    /// Ends the streamed line, if one is open.
    pub fn close(&mut self) -> String {
        match self.live.take() {
            Some(key) => {
                if let Some(entry) = self.streamed.get_mut(&key) {
                    entry.1 = true;
                }
                "\n".into()
            }
            None => String::new(),
        }
    }

    /// `turns` and `events` are what this terminal sent: its prompts are not echoed.
    pub fn print_all(
        &mut self,
        records: &[Record],
        turns: &HashSet<String>,
        events: &BTreeSet<u64>,
    ) -> String {
        records
            .iter()
            .map(|r| self.print(r, turns, events))
            .collect()
    }

    fn print(
        &mut self,
        record: &Record,
        turns: &HashSet<String>,
        events: &BTreeSet<u64>,
    ) -> String {
        match record {
            Record::Entry {
                seq,
                role,
                text,
                turn,
            } => {
                if role == USER {
                    let own =
                        turn.as_ref().is_some_and(|t| turns.contains(t)) || events.contains(seq);
                    if own {
                        return String::new();
                    }
                    return format!("{}you › {text}\n", self.close());
                }
                if role == SYSTEM {
                    return format!("{}! {text}\n", self.close());
                }
                // The streamed line was the answer: end it instead of saying it twice.
                let key = turn.clone().unwrap_or_default();
                let whole = self
                    .streamed
                    .get(&key)
                    .is_some_and(|(said, broken)| !broken && said.trim() == text.trim());
                if role == AGENT && whole && self.live.as_deref() == Some(key.as_str()) {
                    self.live = None;
                    return "\n".into();
                }
                format!("{}{} › {text}\n", self.close(), self.speaker)
            }
            Record::Activity {
                turn, node, event, ..
            } => {
                let kind = event["kind"].as_str().unwrap_or_default();
                match kind {
                    "turn_started" | "state" => String::new(),
                    "text" => {
                        let chunk = event["text"].as_str().unwrap_or_default();
                        if chunk.is_empty() {
                            return String::new();
                        }
                        let key = match node {
                            Some(node) => format!("node:{node}"),
                            None => turn.clone().unwrap_or_default(),
                        };
                        let mut said = String::new();
                        if self.live.as_deref() != Some(key.as_str()) {
                            said.push_str(&self.close());
                            let who = node.clone().unwrap_or_else(|| self.speaker.clone());
                            said.push_str(&format!("{who} › "));
                            self.live = Some(key.clone());
                        }
                        self.streamed.entry(key).or_default().0.push_str(chunk);
                        said.push_str(chunk);
                        said
                    }
                    _ => {
                        let who = node
                            .as_deref()
                            .map(|n| format!("[{n}] "))
                            .unwrap_or_default();
                        let mut said = format!(
                            "{}  · {who}{}\n",
                            self.close(),
                            cut(&describe_event(event), self.max)
                        );
                        if kind == "permission_request" {
                            let options = crate::conversation::read_options(&event["options"]);
                            let choices: Vec<String> = options
                                .iter()
                                .map(|o| format!("/{} ({})", o.id, o.name))
                                .collect();
                            said.push_str(&format!(
                                "    answer: {} · /reject\n",
                                choices.join(" · ")
                            ));
                        }
                        said
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(seq: u64, event: Value) -> Record {
        Record::Activity {
            seq,
            turn: Some("t-1".into()),
            node: None,
            event,
        }
    }

    fn entry(seq: u64, role: &str, text: &str) -> Record {
        Record::Entry {
            seq,
            role: role.into(),
            text: text.into(),
            turn: Some("t-1".into()),
        }
    }

    #[test]
    fn lines_are_commands_or_text() {
        assert_eq!(command("hello"), Command::Send("hello".into()));
        assert_eq!(command("  "), Command::Nothing);
        assert_eq!(command("/quit"), Command::Quit);
        assert_eq!(command("/exit"), Command::Quit);
        assert_eq!(command("/cancel"), Command::Cancel);
        assert_eq!(command("/history"), Command::History);
        assert_eq!(command("//etc/hosts"), Command::Send("/etc/hosts".into()));
        assert_eq!(
            command("/yes p1"),
            Command::Answer("yes".into(), Some("p1".into()))
        );
        assert_eq!(command("/allow"), Command::Answer("allow".into(), None));
    }

    #[test]
    fn a_streamed_answer_is_not_said_twice() {
        let mut printer = Printer::for_speaker(240, "dev".into());
        let mut turns = HashSet::new();
        turns.insert("t-1".to_string());
        let records = vec![
            entry(1, USER, "hi"),
            activity(2, json!({ "kind": "turn_started" })),
            activity(3, json!({ "kind": "text", "text": "hel" })),
            activity(4, json!({ "kind": "text", "text": "lo" })),
            entry(5, AGENT, "hello"),
            activity(
                6,
                json!({ "kind": "turn_ended", "stop_reason": "end_turn" }),
            ),
        ];
        let said = printer.print_all(&records, &turns, &BTreeSet::new());
        assert_eq!(said, "dev › hello\n  · turn ended (end_turn)\n");
        // Someone else's prompt is shown; a stream broken by a tool card is said whole.
        let mut printer = Printer::for_speaker(240, "dev".into());
        let records = vec![
            entry(1, USER, "hi"),
            activity(2, json!({ "kind": "text", "text": "hel" })),
            activity(
                3,
                json!({ "kind": "tool_call", "id": "c1", "title": "read", "status": "pending" }),
            ),
            entry(4, AGENT, "hello"),
        ];
        let said = printer.print_all(&records, &HashSet::new(), &BTreeSet::new());
        assert_eq!(
            said,
            "you › hi\ndev › hel\n  · tool c1 read (pending)\ndev › hello\n"
        );
    }

    #[test]
    fn a_permission_request_says_how_to_answer_it() {
        let mut printer = Printer::for_speaker(240, "dev".into());
        let said = printer.print_all(
            &[activity(
                1,
                json!({ "kind": "permission_request", "id": "p1", "title": "write",
                        "options": [{ "id": "yes", "kind": "allow_once", "name": "Allow" }] }),
            )],
            &HashSet::new(),
            &BTreeSet::new(),
        );
        assert_eq!(
            said,
            "  · permission p1 asked: write [yes]\n    answer: /yes (Allow) · /reject\n"
        );
    }
}
