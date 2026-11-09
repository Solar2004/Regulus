//! `regulus`: runs the core, and talks to a running one like any other client. Every
//! command except `serve` goes over the socket; none of them reaches into the core.

use std::io::{BufRead, IsTerminal};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use jsonrpsee::ws_client::WsClient;
use regulus_cli::conversation::describe_event;
use regulus_cli::modules::TOOLS_MCP_COMMAND;
use regulus_cli::rpc;
use regulus_core::config::{default_home, Config};
use regulus_proto::{
    method, BusEvent, BusEventKind, Diagnostic, Direction, EventPage, Execution, ExecutionState,
    ExecutionTree, Manifest, ModuleSummary, Profile, ProfileDeleted, ProfileSaved, ProfileSummary,
    ProjectDetails, ProjectSummary, ProjectTrusted, Severity, TemplateApplied, TemplateSaved,
    TemplateSummary, TemplateText, WireKind, WorkflowSent, PROTOCOL, WORKFLOW_SELF,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

const CLIENT_NAME: &str = concat!("regulus-cli/", env!("CARGO_PKG_VERSION"));

#[derive(Parser)]
#[command(
    name = "regulus",
    version,
    about = "Regulus core and command-line client"
)]
struct Cli {
    /// Home directory (config, token, port, profiles). Defaults to $REGULUS_HOME or ~/.regulus.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Seconds to wait for an answer. Agent turns can take minutes.
    #[arg(long, global = true, default_value_t = 600)]
    timeout: u64,
    /// Without a command, on a terminal, opens the terminal UI.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the core and serve until Ctrl-C.
    Serve {
        /// Port to bind; 0 picks a free one. Overrides `server.port`.
        #[arg(long)]
        port: Option<u16>,
        /// Do not load this module (id with or without version). Repeatable.
        #[arg(long, value_name = "MODULE")]
        without: Vec<String>,
    },
    /// Call any method on the running core and print the JSON result.
    Call {
        method: String,
        /// Named parameters as a JSON object.
        #[arg(default_value = "{}")]
        params: String,
    },
    /// Modules known to the running core.
    Module {
        #[command(subcommand)]
        command: ModuleCommand,
    },
    /// Profiles known to the running core.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Projects: folders with a `regulus.toml` (docs/11-composition.md §13).
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Templates: profiles with holes that are applied as a copy (docs/11-composition.md §7).
    Template {
        #[command(subcommand)]
        command: TemplateCommand,
    },
    /// Print the nodes and wires of a profile.
    Graph { profile: String },
    /// Start a workflow of a profile (its main workflow unless --workflow), then print
    /// its bus events until the values emitted while starting are delivered.
    Run {
        profile: String,
        #[arg(long)]
        workflow: Option<String>,
        /// Return as soon as it starts, without waiting for the bus.
        #[arg(long)]
        detach: bool,
    },
    /// Events of the bus: values over "then" wires, deliveries, failures and cuts.
    Events {
        /// Keep printing new events until Ctrl-C.
        #[arg(long)]
        follow: bool,
        /// Only this execution.
        #[arg(long)]
        execution: Option<String>,
        /// Only this project.
        #[arg(long)]
        project: Option<String>,
    },
    /// Send a value to an input port of the workflow a running execution runs, then
    /// print the bus events it causes until they are delivered. What reaches the
    /// workflow's outputs shows up as emitted from `self.<port>`.
    Send {
        execution: String,
        port: String,
        /// The value: JSON if it parses as JSON, otherwise text (a JSON string, as
        /// `text@1` carries it).
        #[arg(required = true, num_args = 1..)]
        value: Vec<String>,
        /// Always send it as text, even if it parses as JSON.
        #[arg(long)]
        text: bool,
        /// Return as soon as it is queued, without waiting for the bus.
        #[arg(long)]
        detach: bool,
    },
    /// Stop a running execution.
    Stop { execution: String },
    /// List executions.
    Ps,
    /// Executions, one at a time.
    Exec {
        #[command(subcommand)]
        command: ExecCommand,
    },
    /// Talk to an agent node of a running execution.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Talk with a running execution: a direct agent node (`agent.send`/`history`), or the
    /// workflow through one of its own text inputs and one of its own text outputs, bound
    /// once and remembered (docs/04-client.md §7). Without `--once`, a REPL: `/help`.
    Talk {
        execution: String,
        /// The agent node to talk to. Without it: the binding chosen before, else the only
        /// agent of the workflow.
        #[arg(long, conflicts_with_all = ["input", "output"])]
        node: Option<String>,
        /// The workflow's own then input `text@` a message goes to.
        #[arg(long = "in", value_name = "PORT")]
        input: Option<String>,
        /// The workflow's own then output `text@` replies come from.
        #[arg(long = "out", value_name = "PORT")]
        output: Option<String>,
        /// Accept the binding proposed when the workflow has one text input and one text
        /// output, without asking.
        #[arg(long)]
        yes: bool,
        /// Send this, print the reply and exit; non-zero on a failure or no reply within
        /// --timeout.
        #[arg(long, value_name = "TEXT")]
        once: Option<String>,
    },
    /// Messages through a channel node of a running execution.
    Msg {
        #[command(subcommand)]
        command: MsgCommand,
    },
    /// The MCP bridge an agent starts to reach its tools. Not for people: it speaks MCP
    /// on stdio and reads its binding from the environment.
    #[command(name = "tools-mcp", hide = true)]
    ToolsMcp,
    /// The bridge an agent's CLI runs for each hook event. Not for people: it reads the
    /// event on stdin, asks the hooks wired into the agent through the core, and answers
    /// the way that CLI reads hooks (docs/13-blocks.md §4).
    #[command(name = "hook", hide = true)]
    Hook {
        #[arg(long)]
        execution: String,
        /// The agent node the hooks are wired into.
        #[arg(long)]
        node: String,
        /// Only this hook node. Without it, every hook of the agent.
        #[arg(long)]
        hook: Option<String>,
        /// The engine-neutral event name (`pre_tool`, `stop`…).
        #[arg(long)]
        event: String,
        /// How long the hooks may take. Without it, the core's `[hooks] timeout_ms`.
        #[arg(long)]
        timeout_ms: Option<u64>,
        /// What the hook declared it may answer (`veto,context`); anything else is dropped.
        #[arg(long, value_delimiter = ',')]
        returns: Vec<String>,
        /// `open`: a hook that does not answer is nothing. `closed`: it is a veto.
        #[arg(long, default_value = "open")]
        fail: String,
        /// How stdin and the answer are written: `json` (a `HookRequest` in, the combined
        /// `HookAnswer` out) or the name of an engine's hook protocol (`claude`).
        #[arg(long, default_value = HOOK_JSON)]
        format: String,
    },
    /// Secrets that workflows name as `secret:<name>`.
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },
    /// Decisions that wait for a person (Approval and Review nodes of `human`).
    Human {
        #[command(subcommand)]
        command: HumanCommand,
    },
    /// The documents of a live `ingest` or `retrieve` node (module `documents`).
    Docs {
        #[command(subcommand)]
        command: DocsCommand,
    },
    /// Workflows offered to outside systems: keys, endpoints and the request log.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Replace this program with the newest signed release of its channel (`[update]` in
    /// config.toml, docs/06-stack.md §5.4).
    #[command(args_conflicts_with_subcommands = true)]
    Update {
        #[command(subcommand)]
        command: Option<UpdateCommand>,
        /// Only say whether a newer release exists.
        #[arg(long)]
        check: bool,
        /// Follow this channel instead of `update.channel`.
        #[arg(long)]
        channel: Option<String>,
        /// Read releases from this base URL instead of `update.url`.
        #[arg(long)]
        url: Option<String>,
        /// Install without asking.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum UpdateCommand {
    /// Make a release key pair: the secret key file for `release.sh`, the public key for
    /// `[update] public_key`.
    Keygen {
        /// Where to write the secret key; the public key goes to `<out>.pub`.
        #[arg(long, default_value = "regulus-release.key")]
        out: PathBuf,
        /// Overwrite an existing key file.
        #[arg(long)]
        force: bool,
    },
    /// Sign files with the release key, into `<file>.sig` (minisign format).
    Sign {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// The secret key file. Without it, the key text is read from $REGULUS_SIGNING_KEY.
        #[arg(long)]
        key: Option<PathBuf>,
        /// The version the files belong to, recorded in the signature's trusted comment.
        #[arg(long)]
        release_version: Option<String>,
    },
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Keys that outside systems call with (`Authorization: Bearer rgk_…`).
    Key {
        #[command(subcommand)]
        command: ServiceKeyCommand,
    },
    /// Print the nodes that expose a workflow, to paste into a profile's main workflow.
    Expose {
        workflow: String,
        /// Limits for the workflow: `requests=5,window_ms=60000,cooldown_ms=…,
        /// concurrency=…,budget_requests=…,budget_ms=…`.
        #[arg(long)]
        policy: Option<String>,
        #[arg(long, default_value = "127.0.0.1:7792")]
        bind: String,
        /// How long a request waits for the answer.
        #[arg(long, default_value_t = 30000)]
        timeout_ms: u64,
    },
    /// The latest requests: who, what, when, how long, result.
    Log {
        #[arg(long, default_value_t = 20)]
        last: usize,
        /// Only this key id.
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        workflow: Option<String>,
    },
    /// Where each running HTTP engine listens.
    Endpoints,
}

#[derive(Subcommand)]
enum ServiceKeyCommand {
    /// Create a key. It is shown this once; Regulus keeps only its hash.
    New {
        /// What it may call: `<project>[/<workflow>]`, `global` for a profile without
        /// project, `*` for any. Repeatable.
        #[arg(long, required = true)]
        scope: Vec<String>,
        #[arg(long, default_value = "")]
        name: String,
        /// Limits of this key everywhere, as for `expose --policy`.
        #[arg(long)]
        policy: Option<String>,
    },
    /// Every key, without its secret.
    List,
    /// Revoke one key; the others keep working.
    Revoke { id: String },
}

#[derive(Subcommand)]
enum HumanCommand {
    /// Decisions waiting in running executions, oldest first.
    List {
        /// Only this execution.
        #[arg(long)]
        execution: Option<String>,
    },
    /// One pending decision in full: what waits (raw, to edit for a review) and the form.
    Show { id: String },
    /// Decide: `approve` or `reject`. With --file or --text a review sends that text on;
    /// the decision may then be left out (approve). A `decision` (a Decider's question) is
    /// approved with the option chosen, `--field option=<id>`; `reject` picks its reject
    /// option.
    Answer {
        id: String,
        decision: Option<String>,
        /// A form field, `name=value`. Repeatable; text is read as the type the form says.
        #[arg(long = "field", value_name = "NAME=VALUE")]
        fields: Vec<String>,
        /// A review's edited text, or an approval's comment.
        #[arg(long)]
        text: Option<String>,
        /// A review's edited text, read from this file.
        #[arg(long, conflicts_with = "text")]
        file: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum DocsCommand {
    /// The sources of the node's collection: id, version, chunks, title and location.
    Ls { execution: String, node: String },
    /// Search the node's collection, best hits first.
    Query {
        execution: String,
        node: String,
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// Hits at most. Default: the node's `k`.
        #[arg(long)]
        k: Option<u64>,
        /// Characters of each hit's text shown.
        #[arg(long, default_value_t = 200)]
        chars: usize,
    },
    /// Delete a source, its chunks and its index entries.
    Rm {
        execution: String,
        node: String,
        source_id: String,
    },
}

#[derive(Subcommand)]
enum SecretCommand {
    /// Store a secret. The value is read from stdin, never from the command line, so it
    /// stays out of the shell history.
    Set { name: String },
    /// List secret names. Values are never shown.
    Ls,
    /// Delete a secret.
    Rm { name: String },
}

#[derive(Subcommand)]
enum AgentCommand {
    /// Send a prompt and print the agent's answer when its turn ends.
    Prompt {
        execution: String,
        node: String,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Queue a prompt and print its turn id at once; the answer shows in `agent history`.
    Send {
        execution: String,
        node: String,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// The conversation the agent box recorded: prompts, replies and what happened in its
    /// session (tool calls, permission requests…), oldest first.
    History {
        execution: String,
        node: String,
        /// Only what came after this cursor (the `next` a previous call printed).
        #[arg(long, default_value_t = 0)]
        since: u64,
        /// Keep printing what comes, until Ctrl-C.
        #[arg(long)]
        follow: bool,
    },
    /// Ask the agent to end the turn in flight.
    Cancel { execution: String, node: String },
}

#[derive(Subcommand)]
enum MsgCommand {
    /// Send a message. `@name` sends it to those participants; without it, to all of
    /// them. Returns once it is queued; answers show up in `msg log`.
    Send {
        execution: String,
        channel: String,
        /// Who it is from.
        #[arg(long, default_value = "user")]
        from: String,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Who is in the channel: name, what it is, whether it is free, messages waiting.
    Who { execution: String, channel: String },
    /// The channel's messages and answers, oldest first.
    Log {
        execution: String,
        channel: String,
        /// Only the last N.
        #[arg(long)]
        last: Option<usize>,
    },
}

#[derive(Subcommand)]
enum ModuleCommand {
    /// List modules and their state.
    Ls,
    /// Print a module's manifest as TOML: its node types with ports, fields, defaults and
    /// options. JSON with `regulus call module.manifest '{"id":"<id>"}'`.
    Show {
        /// The module id, with or without the version (`regulus/flow`).
        id: String,
    },
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// List the open projects.
    Ls,
    /// Print an open project's `regulus.toml` as the core resolved it.
    Show { id: String },
    /// Open a folder as a project (writes a minimal `regulus.toml` if it has none) and
    /// remember it in `config.toml`.
    Open {
        path: PathBuf,
        /// Also trust it: its local modules (`[[modules.local]]`) run code on this machine.
        #[arg(long)]
        trust: bool,
    },
    /// Trust a project folder so its local modules run, or stop trusting it (`--remove`).
    /// It need not be open; if it is, its local modules start or stop now.
    Trust {
        path: PathBuf,
        #[arg(long)]
        remove: bool,
    },
    /// Forget an open project. Nothing in its folder is deleted.
    Close { id: String },
}

#[derive(Subcommand)]
enum ExecCommand {
    /// An execution and the children its nodes started, as deep as they go: id, state,
    /// workflow, the node that started each child and its tag.
    Tree { execution: String },
}

#[derive(Subcommand)]
enum TemplateCommand {
    /// List templates: name, description and the vars that need a value.
    Ls,
    /// Print a template's file as it is.
    Show {
        /// A template name, or a path to a `.toml` file.
        template: String,
    },
    /// Render a template into a profile, write it to the profiles directory and load it.
    Apply {
        /// A template name, or a path to a `.toml` file (anything with `/` or ending in
        /// `.toml`).
        template: String,
        /// A var's value, `name=value`. Repeatable.
        #[arg(long = "set", value_name = "NAME=VALUE")]
        set: Vec<String>,
        /// The profile id, and file name, to write. Defaults to the template's.
        #[arg(long)]
        id: Option<String>,
        /// Replace a profile with that id written before.
        #[arg(long)]
        force: bool,
        /// Apply it into this open project: its templates first, written to its profiles
        /// folder with its `project_id`.
        #[arg(long)]
        project: Option<String>,
    },
    /// Write a profile, or one workflow of it with the workflows it places as boxes, as a
    /// template. Holes (`{{name}}`) already in the profile must be declared with --var.
    Save {
        /// The profile to save.
        profile: String,
        /// The template's name, and file name.
        #[arg(long)]
        name: String,
        /// Only this workflow, as the main one, with the workflows it runs.
        #[arg(long)]
        workflow: Option<String>,
        #[arg(long)]
        description: Option<String>,
        /// A var it declares: `name=description` or `name=description=default`.
        /// Repeatable.
        #[arg(long = "var", value_name = "NAME=DESCRIPTION[=DEFAULT]")]
        var: Vec<String>,
        /// Replace a template file written before.
        #[arg(long)]
        force: bool,
        /// Where to write it: a `.toml` file or a folder. Defaults to the first templates
        /// path.
        #[arg(long)]
        path: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// List profiles.
    Ls {
        #[arg(long)]
        project: Option<String>,
    },
    /// Write a whole profile to the profiles directory and load it. The core rewrites the
    /// file from the profile: comments are not kept.
    Save {
        /// A profile as TOML or JSON (`.json`), or `-` to read it from stdin.
        file: String,
        /// Replace the profile with that id, if its file holds it alone.
        #[arg(long)]
        force: bool,
    },
    /// Unload a profile and delete its file. Refused while an execution of it runs.
    Rm { id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = cli.home.unwrap_or_else(default_home);
    let config = Config::load(&home)?;
    let client = Client {
        config: &config,
        timeout: Duration::from_secs(cli.timeout),
    };
    let Some(command) = cli.command else {
        if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            let table = regulus_cli::tui::client_table(&config.client);
            return regulus_cli::tui::run_with(config.clone(), client.timeout, Some(&table)).await;
        }
        // Scripts and pipes get the usage, as a missing command always gave them.
        Cli::command().print_help()?;
        std::process::exit(2);
    };
    match command {
        Command::Serve { port, without } => serve(config, port, without).await,
        Command::ToolsMcp => tools_bridge(&client).await,
        Command::Hook {
            execution,
            node,
            hook,
            event,
            timeout_ms,
            returns,
            fail,
            format,
        } => {
            let ask = HookAsk {
                execution,
                node,
                hook,
                event,
                timeout_ms,
                returns,
                fail,
            };
            let code = hook_bridge(&client, ask, &format).await;
            std::process::exit(code);
        }
        Command::Call { method, params } => {
            let params: Value = serde_json::from_str(&params).context("params must be JSON")?;
            let result: Value = client.request(&method, params).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Command::Module {
            command: ModuleCommand::Show { id },
        } => {
            let params = serde_json::json!({ "id": id });
            let manifest: Manifest = client.request(method::MODULE_MANIFEST, params).await?;
            // TOML, as the module wrote it; JSON only if TOML cannot hold a value (null).
            match toml::to_string(&manifest) {
                Ok(text) => print!("{text}"),
                Err(_) => println!("{}", serde_json::to_string_pretty(&manifest)?),
            }
            Ok(())
        }
        Command::Module {
            command: ModuleCommand::Ls,
        } => {
            let modules: Vec<ModuleSummary> =
                client.request(method::MODULE_LIST, Value::Null).await?;
            for module in modules {
                let state = serde_json::to_value(&module.state)?;
                let status = state["status"].as_str().unwrap_or_default().to_string();
                let detail = state
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter(|(k, _)| *k != "status");
                let mut detail: Vec<String> = detail.map(|(k, v)| format!("{k}={v}")).collect();
                if let Some(project) = &module.project {
                    detail.push(format!("project={project}"));
                }
                println!("{:<32} {:<12} {}", module.id, status, detail.join(" "));
            }
            Ok(())
        }
        Command::Profile {
            command: ProfileCommand::Ls { project },
        } => {
            let filter = project.map_or(Value::Null, |id| serde_json::json!({ "project_id": id }));
            let profiles: Vec<ProfileSummary> =
                client.request(method::PROFILE_LIST, filter).await?;
            for profile in profiles {
                let project = profile
                    .project_id
                    .map_or(String::new(), |p| format!("  [project {p}]"));
                println!("{:<24} {}{project}", profile.id, profile.name);
            }
            Ok(())
        }
        Command::Profile {
            command: ProfileCommand::Save { file, force },
        } => {
            let params = serde_json::json!({ "profile": profile_arg(&file)?, "force": force });
            let saved: ProfileSaved = client.request(method::PROFILE_SAVE, params).await?;
            println!("profile {}", saved.profile_id);
            println!("wrote {}", saved.path);
            print_diagnostics(saved.diagnostics);
            Ok(())
        }
        Command::Profile {
            command: ProfileCommand::Rm { id },
        } => {
            let params = serde_json::json!({ "id": id });
            let deleted: ProfileDeleted = client.request(method::PROFILE_DELETE, params).await?;
            println!("removed {}", deleted.path);
            Ok(())
        }
        Command::Template { command } => template(&client, command).await,
        Command::Update {
            command,
            check,
            channel,
            url,
            yes,
        } => match command {
            Some(command) => update_tool(command),
            None => {
                let ask = UpdateAsk {
                    check,
                    channel,
                    url,
                    yes,
                };
                tokio::task::spawn_blocking(move || update(&config, ask)).await?
            }
        },
        Command::Project { command } => project(&client, command).await,
        Command::Graph { profile } => {
            let params = serde_json::json!({ "id": profile });
            let profile: Profile = client.request(method::PROFILE_GET, params).await?;
            print_graph(&profile);
            Ok(())
        }
        Command::Run {
            profile,
            workflow,
            detach,
        } => {
            let params = serde_json::json!({ "profile_id": profile, "workflow_id": workflow });
            let ws = client.connect().await?;
            let execution: Execution = rpc::call(&ws, method::EXECUTION_START, params).await?;
            print_execution(&execution);
            if let ExecutionState::Failed { reason } = execution.state {
                bail!("{reason}");
            }
            if !detach {
                let filter = EventFilter {
                    execution: Some(execution.id),
                    project: None,
                };
                events(&client, &ws, &filter, 0, Follow::UntilIdle).await?;
            }
            Ok(())
        }
        Command::Send {
            execution,
            port,
            value,
            text,
            detach,
        } => {
            let value = value.join(" ");
            let value = match serde_json::from_str::<Value>(&value) {
                Ok(json) if !text => json,
                _ => Value::String(value),
            };
            let params = serde_json::json!({
                "execution_id": execution, "port": port, "value": value,
            });
            let ws = client.connect().await?;
            let sent: WorkflowSent = rpc::call(&ws, method::WORKFLOW_SEND, params).await?;
            if detach {
                println!("#{} {execution} {WORKFLOW_SELF}.{port}", sent.event);
                return Ok(());
            }
            let filter = EventFilter {
                execution: Some(execution),
                project: None,
            };
            let since = sent.event.saturating_sub(1);
            events(&client, &ws, &filter, since, Follow::UntilIdle).await
        }
        Command::Events {
            follow,
            execution,
            project,
        } => {
            let ws = client.connect().await?;
            let filter = EventFilter { execution, project };
            let follow = if follow { Follow::Always } else { Follow::No };
            events(&client, &ws, &filter, 0, follow).await
        }
        Command::Stop { execution } => {
            let params = serde_json::json!({ "id": execution });
            let execution: Execution = client.request(method::EXECUTION_STOP, params).await?;
            print_execution(&execution);
            Ok(())
        }
        Command::Ps => {
            let executions: Vec<Execution> =
                client.request(method::EXECUTION_LIST, Value::Null).await?;
            for execution in executions {
                print_execution(&execution);
            }
            Ok(())
        }
        Command::Exec {
            command: ExecCommand::Tree { execution },
        } => {
            let params = serde_json::json!({ "execution_id": execution });
            let tree: ExecutionTree = client.request(method::EXECUTION_TREE, params).await?;
            print_tree(&tree, "", "");
            Ok(())
        }
        Command::Agent {
            command:
                AgentCommand::Send {
                    execution,
                    node,
                    text,
                },
        } => {
            let params = serde_json::json!({ "execution_id": execution, "node_id": node, "text": text.join(" ") });
            let sent: Value = client.request("agent.send", params).await?;
            println!("{}", sent["turn"].as_str().unwrap_or_default());
            Ok(())
        }
        Command::Agent {
            command:
                AgentCommand::History {
                    execution,
                    node,
                    since,
                    follow,
                },
        } => agent_history(&client, &execution, &node, since, follow).await,
        Command::Agent {
            command: AgentCommand::Cancel { execution, node },
        } => {
            let params = serde_json::json!({ "execution_id": execution, "node_id": node });
            let _: Value = client.request("agent.cancel", params).await?;
            Ok(())
        }
        Command::Agent {
            command:
                AgentCommand::Prompt {
                    execution,
                    node,
                    text,
                },
        } => {
            let params = serde_json::json!({ "execution_id": execution, "node_id": node, "text": text.join(" ") });
            let turn: Value = client.request("agent.prompt", params).await?;
            println!("{}", turn["text"].as_str().unwrap_or_default());
            if let Some(reason) = turn["stop_reason"].as_str().filter(|r| *r != "end_turn") {
                eprintln!("regulus: the turn ended with `{reason}`");
            }
            Ok(())
        }
        Command::Talk {
            execution,
            node,
            input,
            output,
            yes,
            once,
        } => {
            let table = regulus_cli::tui::client_table(&config.client);
            let options = regulus_cli::tui::options(&config, Some(&table))?;
            let settings = regulus_cli::talk::Settings::new(&config, &options, client.timeout);
            let talk = regulus_cli::talk::Talk {
                execution,
                node,
                input,
                output,
                yes,
                once,
            };
            let ws = client.connect().await?;
            regulus_cli::talk::run(&ws, &config, &settings, talk).await
        }
        Command::Msg { command } => match command {
            MsgCommand::Send {
                execution,
                channel,
                from,
                text,
            } => {
                let params = serde_json::json!({
                    "execution_id": execution, "node_id": channel,
                    "from": from, "text": text.join(" "),
                });
                let sent: Value = client.request("comm.send", params).await?;
                let to: Vec<&str> = sent["to"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                println!("#{} → {}", sent["id"], to.join(", "));
                Ok(())
            }
            MsgCommand::Who { execution, channel } => {
                let params = serde_json::json!({ "execution_id": execution, "node_id": channel });
                let seen: Vec<Value> = client.request("comm.participants", params).await?;
                for p in seen {
                    let kind = p["kind"].as_str().unwrap_or_default();
                    println!(
                        "@{} ({}{}) {} · {} queued",
                        p["name"].as_str().unwrap_or_default(),
                        p["node"].as_str().unwrap_or_default(),
                        if kind.is_empty() {
                            String::new()
                        } else {
                            format!(", {kind}")
                        },
                        p["state"].as_str().unwrap_or("unknown"),
                        p["queued"]
                    );
                }
                Ok(())
            }
            MsgCommand::Log {
                execution,
                channel,
                last,
            } => {
                let params = serde_json::json!({
                    "execution_id": execution, "node_id": channel, "limit": last,
                });
                let messages: Vec<Value> = client.request("comm.history", params).await?;
                for m in messages {
                    let to: Vec<&str> = m["to"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    let body = match m["error"].as_str() {
                        Some(error) => format!("(not delivered: {error})"),
                        None => m["text"].as_str().unwrap_or_default().to_string(),
                    };
                    let reply = m["reply_to"]
                        .as_u64()
                        .map(|id| format!(" re #{id}"))
                        .unwrap_or_default();
                    println!(
                        "#{} {} → {}{reply}: {body}",
                        m["id"],
                        m["from"].as_str().unwrap_or_default(),
                        to.join(", ")
                    );
                }
                Ok(())
            }
        },
        Command::Secret { command } => match command {
            SecretCommand::Set { name } => {
                let value = read_secret(&name)?;
                let params = serde_json::json!({ "name": name, "value": value });
                let _: Value = client.request(method::SECRET_SET, params).await?;
                Ok(())
            }
            SecretCommand::Ls => {
                let names: Vec<String> = client.request(method::SECRET_LIST, Value::Null).await?;
                for name in names {
                    println!("{name}");
                }
                Ok(())
            }
            SecretCommand::Rm { name } => {
                let params = serde_json::json!({ "name": name });
                let _: Value = client.request(method::SECRET_DELETE, params).await?;
                Ok(())
            }
        },
        Command::Human { command } => human(&client, command).await,
        Command::Docs { command } => docs(&client, command).await,
        Command::Service { command } => service(&client, command).await,
    }
}

/// A template argument as the core reads it: a relative path is made absolute here, since
/// the core resolves relative paths against its home, not this directory.
fn template_arg(template: String) -> Result<String> {
    let is_path = template.contains('/') || template.ends_with(".toml");
    if !is_path || std::path::Path::new(&template).is_absolute() {
        return Ok(template);
    }
    Ok(std::path::absolute(&template)
        .with_context(|| format!("cannot resolve {template}"))?
        .to_string_lossy()
        .into_owned())
}

/// `regulus template`: the `template.*` methods of the running core.
async fn template(client: &Client<'_>, command: TemplateCommand) -> Result<()> {
    match command {
        TemplateCommand::Ls => {
            let list: Vec<TemplateSummary> =
                client.request(method::TEMPLATE_LIST, Value::Null).await?;
            for t in list {
                if let Some(error) = &t.error {
                    println!("{:<20} (unusable: {error})", t.name);
                    continue;
                }
                let required: Vec<&str> = t
                    .vars
                    .iter()
                    .filter(|v| v.default.is_none())
                    .map(|v| v.name.as_str())
                    .collect();
                let required = if required.is_empty() {
                    String::new()
                } else {
                    format!("  [needs: {}]", required.join(", "))
                };
                let project = t
                    .project_id
                    .as_ref()
                    .map_or(String::new(), |p| format!("  [project {p}]"));
                println!("{:<20} {}{required}{project}", t.name, t.description);
            }
            Ok(())
        }
        TemplateCommand::Show { template } => {
            let params = serde_json::json!({ "template": template_arg(template)? });
            let t: TemplateText = client.request(method::TEMPLATE_GET, params).await?;
            print!("{}", t.text);
            Ok(())
        }
        TemplateCommand::Apply {
            template,
            set,
            id,
            force,
            project,
        } => {
            let mut vars = serde_json::Map::new();
            for pair in set {
                let (name, value) = pair
                    .split_once('=')
                    .with_context(|| format!("`{pair}` is not NAME=VALUE"))?;
                vars.insert(name.trim().into(), Value::String(value.into()));
            }
            let params = serde_json::json!({
                "template": template_arg(template)?, "vars": vars, "id": id, "force": force,
                "project_id": project,
            });
            let applied: TemplateApplied = client.request(method::TEMPLATE_APPLY, params).await?;
            println!("profile {}", applied.profile_id);
            println!("wrote {}", applied.path);
            print_diagnostics(applied.diagnostics);
            Ok(())
        }
        TemplateCommand::Save {
            profile,
            name,
            workflow,
            description,
            var,
            force,
            path,
        } => {
            let mut vars = serde_json::Map::new();
            for spec in var {
                let mut parts = spec.splitn(3, '=');
                let (Some(var_name), Some(var_description)) = (parts.next(), parts.next()) else {
                    bail!("`{spec}` is not NAME=DESCRIPTION or NAME=DESCRIPTION=DEFAULT");
                };
                let mut entry = serde_json::json!({ "description": var_description });
                if let Some(default) = parts.next() {
                    entry["default"] = Value::String(default.into());
                }
                vars.insert(var_name.trim().into(), entry);
            }
            let path = path.map(template_arg).transpose()?;
            let params = serde_json::json!({
                "name": name, "profile_id": profile, "workflow_id": workflow,
                "description": description, "vars": vars, "path": path, "force": force,
            });
            let saved: TemplateSaved = client.request(method::TEMPLATE_SAVE, params).await?;
            println!("wrote {}", saved.path);
            Ok(())
        }
    }
}

/// One line per execution of a tree, children indented under their parent with the node
/// that started them and their tag.
fn print_tree(tree: &ExecutionTree, lead: &str, child_lead: &str) {
    let execution = &tree.execution;
    let mut line = format!(
        "{lead}{}  {}  {}",
        execution.id,
        state_name(&execution.state),
        execution.workflow_id
    );
    if let Some(caller) = &execution.caller {
        line.push_str(&format!("  by {caller}"));
    }
    if let Some(tag) = &execution.tag {
        line.push_str(&format!("  [{tag}]"));
    }
    println!("{line}");
    let count = tree.children.len();
    for (i, child) in tree.children.iter().enumerate() {
        let last = i + 1 == count;
        let (branch, rest) = if last {
            ("└─ ", "   ")
        } else {
            ("├─ ", "│  ")
        };
        print_tree(
            child,
            &format!("{child_lead}{branch}"),
            &format!("{child_lead}{rest}"),
        );
    }
}

/// `regulus project`: the `project.*` methods of the running core. Warnings go to stderr.
async fn project(client: &Client<'_>, command: ProjectCommand) -> Result<()> {
    let summary = |p: ProjectSummary| {
        let failed = if p.error.is_some() {
            "  [error: not loaded]"
        } else {
            ""
        };
        println!("{:<20} {:<24} {}{failed}", p.id, p.name, p.path);
        for warning in p.warnings {
            eprintln!("warning: {}: {warning}", p.id);
        }
    };
    match command {
        ProjectCommand::Ls => {
            let list: Vec<ProjectSummary> =
                client.request(method::PROJECT_LIST, Value::Null).await?;
            list.into_iter().for_each(summary);
        }
        ProjectCommand::Show { id } => {
            let p: ProjectDetails = client
                .request(method::PROJECT_GET, serde_json::json!({ "id": id }))
                .await?;
            println!("id        {}", p.id);
            println!("name      {}", p.name);
            println!("path      {}", p.path);
            println!("file      {}", p.file);
            println!("profiles  {}", p.profiles.join(", "));
            println!("templates {}", p.templates.join(", "));
            if !p.disable.is_empty() {
                println!("disable   {}", p.disable.join(", "));
            }
            if let Some(enable) = &p.enable {
                println!("enable    {}", enable.join(", "));
            }
            if !p.local.is_empty() {
                println!("local     {}", p.local.len());
            }
            if !p.client.is_empty() {
                println!("client    {}", Value::Object(p.client));
            }
            for warning in p.warnings {
                eprintln!("warning: {warning}");
            }
        }
        ProjectCommand::Open { path, trust } => {
            let path = std::path::absolute(&path)
                .with_context(|| format!("cannot resolve {}", path.display()))?;
            let params = serde_json::json!({ "path": path, "trust": trust });
            summary(client.request(method::PROJECT_OPEN, params).await?);
        }
        ProjectCommand::Trust { path, remove } => {
            let path = std::path::absolute(&path)
                .with_context(|| format!("cannot resolve {}", path.display()))?;
            let params = serde_json::json!({ "path": path, "remove": remove });
            let done: ProjectTrusted = client.request(method::PROJECT_TRUST, params).await?;
            let what = if done.trusted {
                "trusted"
            } else {
                "not trusted"
            };
            println!("{what} {}", done.path);
        }
        ProjectCommand::Close { id } => {
            let params = serde_json::json!({ "id": id });
            let closed: ProjectSummary = client.request(method::PROJECT_CLOSE, params).await?;
            println!("closed {} ({})", closed.id, closed.path);
        }
    }
    Ok(())
}

/// Warnings and information a write returned, on stderr.
fn print_diagnostics(diagnostics: Vec<Diagnostic>) {
    for d in diagnostics {
        let severity = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        };
        let at = [d.workflow_id, d.node_id, d.wire_id]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("/");
        eprintln!("{severity}: {} ({}) {at}", d.message, d.code);
    }
}

/// The profile `regulus profile save` sends: a file (JSON if it ends in `.json`, TOML
/// otherwise) or stdin (`-`, JSON if it parses as JSON, TOML otherwise). A file with
/// several profiles or a template is refused here, where it is clearer.
fn profile_arg(file: &str) -> Result<Value> {
    let (text, json) = if file == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .context("cannot read the profile from stdin")?;
        let json = serde_json::from_str::<Value>(&text).is_ok();
        (text, json)
    } else {
        let text = std::fs::read_to_string(file).with_context(|| format!("cannot read {file}"))?;
        (text, file.ends_with(".json"))
    };
    let profile: Value = if json {
        serde_json::from_str(&text).with_context(|| format!("{file} is not valid JSON"))?
    } else {
        let table: toml::Table =
            toml::from_str(&text).with_context(|| format!("{file} is not valid TOML"))?;
        serde_json::to_value(table)?
    };
    if profile.get("profiles").is_some() {
        bail!("{file} holds several profiles; `profile save` takes one");
    }
    if profile.get("template").is_some() {
        bail!("{file} is a template; apply it with `regulus template apply`");
    }
    Ok(profile)
}

fn policy_arg(text: Option<&str>) -> Result<regulus_module_service::policy::Policy> {
    match text {
        None => Ok(Default::default()),
        Some(text) => regulus_module_service::parse_policy(text).map_err(anyhow::Error::msg),
    }
}

fn print_key(key: &Value) {
    let revoked = if key["revoked_ms"].is_null() {
        ""
    } else {
        "  revoked"
    };
    let scope: Vec<&str> = key["scope"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let policy = match &key["policy"] {
        Value::Object(map) if !map.is_empty() => format!("  policy {}", key["policy"]),
        _ => String::new(),
    };
    println!(
        "{:<12} {:<16} {}…  {}{policy}{revoked}",
        key["id"].as_str().unwrap_or_default(),
        key["name"].as_str().unwrap_or_default(),
        key["hint"].as_str().unwrap_or_default(),
        scope.join(" "),
    );
}

/// `regulus service`: the `service.*` methods of the running core; `expose` only prints.
async fn service(client: &Client<'_>, command: ServiceCommand) -> Result<()> {
    match command {
        ServiceCommand::Key { command } => match command {
            ServiceKeyCommand::New {
                scope,
                name,
                policy,
            } => {
                let policy = policy_arg(policy.as_deref())?;
                let params = serde_json::json!({ "scope": scope, "name": name, "policy": policy });
                let key: Value = client.request("service.key.new", params).await?;
                print_key(&key);
                println!("{}", key["key"].as_str().unwrap_or_default());
                eprintln!(
                    "regulus: this is the only time the key is shown; Regulus keeps its hash"
                );
                Ok(())
            }
            ServiceKeyCommand::List => {
                let keys: Vec<Value> = client.request("service.key.list", Value::Null).await?;
                keys.iter().for_each(print_key);
                Ok(())
            }
            ServiceKeyCommand::Revoke { id } => {
                let params = serde_json::json!({ "id": id });
                let key: Value = client.request("service.key.revoke", params).await?;
                print_key(&key);
                Ok(())
            }
        },
        ServiceCommand::Expose {
            workflow,
            policy,
            bind,
            timeout_ms,
        } => {
            let policy = regulus_module_service::policy_toml(&policy_arg(policy.as_deref())?);
            print!(
                "{}",
                EXPOSE_TOML
                    .replace("{workflow}", &workflow)
                    .replace("{policy}", &policy)
                    .replace("{bind}", &bind)
                    .replace("{timeout_ms}", &timeout_ms.to_string())
            );
            Ok(())
        }
        ServiceCommand::Log {
            last,
            key,
            workflow,
        } => {
            let params = serde_json::json!({ "last": last, "key": key, "workflow": workflow });
            let entries: Vec<Value> = client.request("service.log", params).await?;
            for e in entries {
                let at = e["at_ms"].as_u64().unwrap_or(0);
                println!(
                    "{}.{:03} {:<12} {:<10} {}/{:<12} {:<5} {} {:>6} ms  {}",
                    at / 1000,
                    at % 1000,
                    e["key_id"].as_str().unwrap_or("-"),
                    e["key_name"].as_str().unwrap_or("-"),
                    e["project"].as_str().unwrap_or_default(),
                    e["workflow"].as_str().unwrap_or("-"),
                    e["mode"].as_str().unwrap_or_default(),
                    e["status"],
                    e["ms"],
                    e["result"].as_str().unwrap_or_default(),
                );
            }
            Ok(())
        }
        ServiceCommand::Endpoints => {
            let list: Vec<Value> = client.request("service.endpoints", Value::Null).await?;
            for e in list {
                println!(
                    "{} {}/{}",
                    e["url"].as_str().unwrap_or_default(),
                    e["execution_id"].as_str().unwrap_or_default(),
                    e["node_id"].as_str().unwrap_or_default()
                );
            }
            Ok(())
        }
    }
}

/// What `regulus service expose` prints: an HTTP engine and an endpoint, wired.
const EXPOSE_TOML: &str = r#"# Paste into the main workflow of the profile that has `{workflow}`, then run it.
  [[workflows.nodes]]
  id = "service-http"
  name = "HTTP engine"
  type = "http"
  module_id = "regulus/service@0.1.0"
  config = { bind = "{bind}" }
  ports = [{ id = "engine", capability = "service.engine@1", direction = "output", kind = "uses" }]

  [[workflows.nodes]]
  id = "service-{workflow}"
  name = "Endpoint {workflow}"
  type = "endpoint"
  module_id = "regulus/service@0.1.0"
  config = { workflows = ["{workflow}"], timeout_ms = {timeout_ms}, policy = {policy} }
  ports = [{ id = "engine", capability = "service.engine@1", direction = "input", kind = "uses", required = true }]

  [[workflows.wires]]
  id = "service-engine-{workflow}"
  kind = "uses"
  from = { node_id = "service-http", port_id = "engine" }
  to = { node_id = "service-{workflow}", port_id = "engine" }
"#;

/// `regulus agent history`: prints the recorded conversation after `since`, page after
/// page; with `follow`, polls every `client.refresh_ms` for more until Ctrl-C.
async fn agent_history(
    client: &Client<'_>,
    execution: &str,
    node: &str,
    mut since: u64,
    follow: bool,
) -> Result<()> {
    let poll = Duration::from_millis(client.config.client.refresh_ms);
    let mut printed_session = false;
    loop {
        let params =
            serde_json::json!({ "execution_id": execution, "node_id": node, "since": since });
        let page: Value = client.request("agent.history", params).await?;
        if !printed_session {
            let session = &page["session"];
            let resume = if session["native_resume"].as_bool() == Some(true) {
                "resumed natively"
            } else {
                "not resumed: the history is Regulus' record"
            };
            eprintln!(
                "session {} · {} · {resume}",
                session["engine_session_id"].as_str().unwrap_or("-"),
                session["state"].as_str().unwrap_or("unknown"),
            );
            printed_session = true;
        }
        let mut lines: Vec<(u64, String)> = Vec::new();
        for entry in page["entries"].as_array().into_iter().flatten() {
            lines.push((
                entry["seq"].as_u64().unwrap_or_default(),
                format!(
                    "{:<6} {}: {}",
                    entry["turn"].as_str().unwrap_or("-"),
                    entry["role"].as_str().unwrap_or_default(),
                    entry["text"].as_str().unwrap_or_default()
                ),
            ));
        }
        for activity in page["activity"].as_array().into_iter().flatten() {
            lines.push((
                activity["seq"].as_u64().unwrap_or_default(),
                format!(
                    "{:<6} · {}",
                    activity["turn"].as_str().unwrap_or("-"),
                    describe_event(&activity["event"])
                ),
            ));
        }
        lines.sort_by_key(|(seq, _)| *seq);
        for (seq, line) in &lines {
            println!("#{seq:<6} {line}");
        }
        let next = page["next"].as_u64().unwrap_or(since);
        let caught_up = next == since;
        since = next;
        if caught_up {
            if !follow {
                eprintln!("next {since}");
                return Ok(());
            }
            tokio::time::sleep(poll).await;
        }
    }
}

/// `regulus human`: talks to the `human.*` methods of the running core.
async fn human(client: &Client<'_>, command: HumanCommand) -> Result<()> {
    match command {
        HumanCommand::List { execution } => {
            let params = serde_json::json!({ "execution_id": execution });
            let pending: Vec<Value> = client.request(regulus_module_human::LIST, params).await?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
            for p in pending {
                let age = now.saturating_sub(p["created_ms"].as_u64().unwrap_or(now)) / 1000;
                let deadline = p["deadline_ms"]
                    .as_u64()
                    .map(|d| format!(" · timeout in {} s", d.saturating_sub(now) / 1000))
                    .unwrap_or_default();
                println!(
                    "{:<16} {:<9} {}/{}  {age} s ago{deadline}  {}",
                    p["id"].as_str().unwrap_or_default(),
                    p["kind"].as_str().unwrap_or_default(),
                    p["execution_id"].as_str().unwrap_or_default(),
                    p["node_id"].as_str().unwrap_or_default(),
                    p["prompt"].as_str().unwrap_or_default(),
                );
            }
            Ok(())
        }
        HumanCommand::Show { id } => {
            let p: Value = client
                .request("human.get", serde_json::json!({ "id": id }))
                .await?;
            match &p["value"] {
                Value::String(text) => println!("{text}"),
                other => println!("{}", serde_json::to_string_pretty(other)?),
            }
            if !p["form"].is_null() {
                eprintln!("form: {}", p["form"]);
            }
            Ok(())
        }
        HumanCommand::Answer {
            id,
            decision,
            fields,
            text,
            file,
        } => {
            let text = match file {
                Some(path) => Some(
                    std::fs::read_to_string(&path)
                        .with_context(|| format!("cannot read {}", path.display()))?,
                ),
                None => text,
            };
            let decision = match decision {
                Some(decision) => decision,
                None if text.is_some() => "approve".into(),
                None => bail!("say `approve` or `reject`"),
            };
            let mut map = serde_json::Map::new();
            for field in fields {
                let (name, value) = field
                    .split_once('=')
                    .with_context(|| format!("`{field}` is not NAME=VALUE"))?;
                map.insert(name.trim().into(), Value::String(value.into()));
            }
            let params = serde_json::json!({
                "id": id, "decision": decision, "fields": map, "text": text,
            });
            let done: Value = client.request(regulus_module_human::ANSWER, params).await?;
            println!(
                "{} {} → {} {}.{}",
                done["id"].as_str().unwrap_or_default(),
                done["decision"].as_str().unwrap_or_default(),
                done["execution_id"].as_str().unwrap_or_default(),
                done["node_id"].as_str().unwrap_or_default(),
                done["port"].as_str().unwrap_or_default(),
            );
            Ok(())
        }
    }
}

/// `regulus docs`: talks to the `documents.*` methods of the running core.
async fn docs(client: &Client<'_>, command: DocsCommand) -> Result<()> {
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    match command {
        DocsCommand::Ls { execution, node } => {
            let params = serde_json::json!({ "execution_id": execution, "node_id": node });
            let sources: Vec<Value> = client
                .request(regulus_module_documents::LIST, params)
                .await?;
            if sources.is_empty() {
                eprintln!("no sources");
            }
            for s in sources {
                let version = text(&s["version"]);
                println!(
                    "{:<14} {:<9} {:>4} chunks {:>8} chars  {}  {}",
                    text(&s["source_id"]),
                    version.get(..8).unwrap_or(&version),
                    s["chunks"],
                    s["chars"],
                    text(&s["title"]),
                    text(&s["location"]),
                );
            }
            Ok(())
        }
        DocsCommand::Query {
            execution,
            node,
            query,
            k,
            chars,
        } => {
            let params = serde_json::json!({
                "execution_id": execution, "node_id": node, "query": query.join(" "), "k": k,
            });
            let found: Value = client
                .request(regulus_module_documents::QUERY, params)
                .await?;
            let hits = found["hits"].as_array().cloned().unwrap_or_default();
            if hits.is_empty() {
                eprintln!("no hits");
            }
            for (n, hit) in hits.iter().enumerate() {
                let score = hit["score"].as_f64().unwrap_or_default();
                println!(
                    "[{}] {score:.3}  {}  {}#{}-{}",
                    n + 1,
                    text(&hit["chunk_id"]),
                    text(&hit["location"]),
                    hit["start"],
                    hit["end"],
                );
                let body: String = text(&hit["text"]).chars().take(chars).collect();
                println!("    {}", body.replace('\n', "\n    "));
            }
            Ok(())
        }
        DocsCommand::Rm {
            execution,
            node,
            source_id,
        } => {
            let params = serde_json::json!({
                "execution_id": execution, "node_id": node, "source_id": source_id,
            });
            let done: Value = client
                .request(regulus_module_documents::DELETE, params)
                .await?;
            if done["deleted"] == true {
                println!("deleted {source_id} ({} chunks)", done["chunks"]);
            } else {
                bail!("no source `{source_id}` in that collection");
            }
            Ok(())
        }
    }
}

/// One line from stdin. On a terminal, asks for it and hides what is typed.
fn read_secret(name: &str) -> Result<String> {
    let stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    let echo = |on: bool| {
        let _ = std::process::Command::new("stty")
            .arg(if on { "echo" } else { "-echo" })
            .stdin(Stdio::inherit())
            .status();
    };
    if terminal {
        eprint!("value for `{name}`: ");
        echo(false);
    }
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    if terminal {
        echo(true);
        eprintln!();
    }
    read.context("cannot read the secret from stdin")?;
    let value = line.trim_end_matches(['\n', '\r']).to_string();
    if value.is_empty() {
        bail!("empty secret: nothing stored");
    }
    Ok(value)
}

struct EventFilter {
    execution: Option<String>,
    project: Option<String>,
}

#[derive(PartialEq)]
enum Follow {
    /// Print what is there and return.
    No,
    /// Until no delivery is queued for what the filter names.
    UntilIdle,
    /// Until Ctrl-C.
    Always,
}

/// Prints bus events after `since` (0 = from the oldest kept), page after page, polling
/// every `client.refresh_ms` once caught up while following.
async fn events(
    client: &Client<'_>,
    ws: &WsClient,
    filter: &EventFilter,
    mut since: u64,
    follow: Follow,
) -> Result<()> {
    let poll = Duration::from_millis(client.config.client.refresh_ms);
    let deadline = Instant::now() + client.timeout;
    loop {
        let params = serde_json::json!({
            "since": since, "execution_id": filter.execution, "project_id": filter.project,
        });
        let page: EventPage = rpc::call(ws, method::EVENT_LIST, params).await?;
        for event in &page.events {
            print_event(event);
        }
        // A page that does not move the cursor means there is nothing more for now.
        let caught_up = page.next == since;
        since = page.next;
        let done = match follow {
            Follow::No => caught_up,
            Follow::UntilIdle => caught_up && page.pending == 0,
            Follow::Always => false,
        };
        if done {
            return Ok(());
        }
        if !caught_up {
            continue;
        }
        if follow == Follow::UntilIdle && Instant::now() >= deadline {
            bail!(
                "the bus is still busy after {} s; follow it with `regulus events --follow`",
                client.timeout.as_secs()
            );
        }
        tokio::time::sleep(poll).await;
    }
}

fn print_event(event: &BusEvent) {
    let from = format!("{}.{}", event.from.node_id, event.from.port_id);
    let what = match &event.kind {
        BusEventKind::Emitted { value } => {
            format!(
                "emitted {} {value} (depth {})",
                event.capability, event.depth
            )
        }
        BusEventKind::Delivered { to } => format!("→ {}.{} delivered", to.node_id, to.port_id),
        BusEventKind::Failed { to, error } => {
            format!("→ {}.{} failed: {error}", to.node_id, to.port_id)
        }
        BusEventKind::Cut { max_depth } => format!("cut: deeper than {max_depth} deliveries"),
    };
    println!("#{:<6} {:<12} {from} {what}", event.seq, event.execution_id);
}

fn state_name(state: &ExecutionState) -> String {
    match state {
        ExecutionState::Starting => "starting".to_string(),
        ExecutionState::Running => "running".to_string(),
        ExecutionState::Stopped => "stopped".to_string(),
        ExecutionState::Failed { reason } => format!("failed: {reason}"),
        ExecutionState::Interrupted { reason } => format!("interrupted: {reason}"),
    }
}

fn print_execution(execution: &Execution) {
    let state = state_name(&execution.state);
    let parent = match &execution.parent {
        Some(parent) => format!("  (child of {parent})"),
        None => String::new(),
    };
    println!(
        "{:<12} {:<16} {state}{parent}",
        execution.id, execution.workflow_id
    );
    for node in &execution.disabled {
        println!(
            "{:<12} disabled {}: nothing usable wired into {}",
            "",
            node.node_id,
            node.missing.join(", ")
        );
    }
}

async fn serve(mut config: Config, port: Option<u16>, without: Vec<String>) -> Result<()> {
    let started = Instant::now();
    if let Some(port) = port {
        config.server.port = port;
    }
    config.modules.without.extend(without);
    // Agents start the bridges as this same program, pointed at this same home.
    let host = regulus_cli::modules::Host {
        program: std::env::current_exe().context("cannot find this program's path")?,
        args: vec!["--home".into(), config.home.to_string_lossy().into_owned()],
    };
    let (core, warnings) = regulus_core::assemble(&config, regulus_cli::modules::hosted(host));
    for warning in warnings {
        eprintln!("regulus: {warning}");
    }
    let running = regulus_core::serve(&config, core.clone()).await?;
    eprintln!(
        "regulus: serving ws://{} (proto {PROTOCOL}, token in {}) in {} ms",
        running.addr,
        config.token_path().display(),
        started.elapsed().as_millis()
    );
    if config.update.check_on_start {
        let update = config.update.clone();
        let path = config.config_path();
        std::thread::spawn(move || announce_update(&update, &path));
    }
    shutdown_signal().await?;
    running.stop().await;
    core.shutdown();
    Ok(())
}

/// Waits for Ctrl-C or, on Unix, SIGTERM: both stop the core cleanly, which stops the
/// processes of local modules.
async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = term.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

/// The tools box, reached through the core with the binding the agent was mounted with.
struct ToolsBackend<'a> {
    client: &'a Client<'a>,
    binding: String,
    runtime: tokio::runtime::Handle,
}

impl regulus_module_tools_mcp::Backend for ToolsBackend<'_> {
    fn list(&self) -> Result<Vec<regulus_contract_tool::Spec>, String> {
        let params = serde_json::json!({ "binding": self.binding });
        self.runtime
            .block_on(self.client.request("tools.list", params))
            .map_err(|e| e.to_string())
    }

    fn call(&self, name: &str, args: Value) -> Result<(String, bool), String> {
        let params = serde_json::json!({ "binding": self.binding, "name": name, "args": args });
        let written: Value = self
            .runtime
            .block_on(self.client.request("tools.call", params))
            .map_err(|e| e.to_string())?;
        Ok((
            written["text"].as_str().unwrap_or_default().to_string(),
            written["error"].as_bool().unwrap_or(false),
        ))
    }
}

async fn tools_bridge(client: &Client<'_>) -> Result<()> {
    let binding = std::env::var(regulus_module_tools_mcp::BINDING_ENV)
        .with_context(|| format!("{} is not set", regulus_module_tools_mcp::BINDING_ENV))?;
    let backend = ToolsBackend {
        client,
        binding,
        runtime: tokio::runtime::Handle::current(),
    };
    // stdio is blocking; the runtime keeps serving the backend's requests meanwhile.
    tokio::task::block_in_place(|| regulus_module_tools_mcp::serve(&backend, TOOLS_MCP_COMMAND))?;
    Ok(())
}

/// The neutral format of the hook bridge: a `HookRequest` on stdin, a `HookAnswer` out.
const HOOK_JSON: &str = "json";
/// What the hook bridge exits with when it cannot get an answer: a non-blocking error
/// for Claude Code, which goes on as if there were no hook.
const HOOK_FAILED: i32 = 1;
/// What the hook bridge exits with to stop the agent when it has no answer to translate.
const HOOK_VETO: i32 = 2;

struct HookAsk {
    execution: String,
    node: String,
    hook: Option<String>,
    event: String,
    timeout_ms: Option<u64>,
    returns: Vec<String>,
    fail: String,
}

/// Reads the event, asks the core and answers in `format`. Returns the exit code.
///
/// Fail-open by default: a hook that does not answer in time, fails, answers what it did
/// not declare, or cannot be reached counts as no answer and the agent goes on. With
/// `--fail closed` any of those is a veto instead.
async fn hook_bridge(client: &Client<'_>, ask: HookAsk, format: &str) -> i32 {
    let closed = match serde_json::from_value(Value::String(ask.fail.clone())) {
        Ok(regulus_proto::HookFailure::Closed) => true,
        Ok(regulus_proto::HookFailure::Open) => false,
        Err(_) => {
            eprintln!("regulus hook: unknown --fail `{}` (open, closed)", ask.fail);
            return HOOK_FAILED;
        }
    };
    match hook_answer(client, ask, format, closed).await {
        Ok((code, stdout, stderr)) => {
            print!("{stdout}");
            eprint!("{stderr}");
            code
        }
        Err(error) if closed => {
            // Not even an answer from the core: closed means the agent does not go on.
            eprint!("regulus hook: no answer, so no: {error:#}");
            HOOK_VETO
        }
        Err(error) => {
            eprintln!("regulus hook: {error:#}");
            HOOK_FAILED
        }
    }
}

async fn hook_answer(
    client: &Client<'_>,
    ask: HookAsk,
    format: &str,
    closed: bool,
) -> Result<(i32, String, String)> {
    use regulus_module_engine_claude::hook as claude;
    let mut stdin = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut stdin)
        .context("cannot read the event from stdin")?;
    let payload: Value = if stdin.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&stdin).context("the event on stdin is not JSON")?
    };
    let request = match format {
        HOOK_JSON => {
            let mut request: regulus_proto::HookRequest = serde_json::from_value(match &payload {
                Value::Null => serde_json::json!({ "event": ask.event }),
                other => other.clone(),
            })
            .context("stdin is not a hook request")?;
            if request.event.is_empty() {
                request.event = ask.event.clone();
            }
            request
        }
        claude::FORMAT => claude::request(&ask.event, payload.clone()),
        other => bail!(
            "unknown hook format `{other}` (known: {HOOK_JSON}, {})",
            claude::FORMAT
        ),
    };
    let returns = ask
        .returns
        .iter()
        .map(|name| serde_json::from_value(Value::String(name.clone())))
        .collect::<Result<Vec<regulus_proto::HookEffect>, _>>()
        .context("--returns takes veto, context")?;
    let params = regulus_proto::HookDispatchParams {
        execution_id: ask.execution,
        node_id: ask.node,
        hook_id: ask.hook,
        timeout_ms: ask.timeout_ms,
        returns,
        request,
    };
    let mut got: regulus_proto::HookDispatched = client
        .request(method::HOOK_DISPATCH, serde_json::to_value(params)?)
        .await?;
    // Hooks that failed or ran out of time count as no answer; say so where a person
    // debugging the agent's hooks looks.
    let mut notes = String::new();
    for reply in &got.replies {
        if let Some(error) = &reply.error {
            notes.push_str(&format!("regulus hook: `{}`: {error}\n", reply.node_id));
        }
    }
    if closed {
        for reply in &mut got.replies {
            if let Some(error) = &reply.error {
                reply.answer = regulus_proto::HookAnswer::Veto {
                    reason: format!("hook `{}` gave no answer, so no: {error}", reply.node_id),
                };
            }
        }
    }
    let answer = got.combined();
    Ok(match format {
        HOOK_JSON => (0, format!("{}\n", serde_json::to_string(&answer)?), notes),
        _ => {
            let out = claude::output(&ask.event, &payload, &answer);
            // Exit 2 makes stderr the reason Claude sees: nothing else may go there.
            let stderr = if out.code == 0 { notes } else { out.stderr };
            (out.code, out.stdout, stderr)
        }
    })
}

struct UpdateAsk {
    check: bool,
    channel: Option<String>,
    url: Option<String>,
    yes: bool,
}

/// `regulus update`: check, ask, download, verify, replace.
fn update(config: &Config, ask: UpdateAsk) -> Result<()> {
    use regulus_cli::update as up;
    let source = up::Source::new(&config.update, &config.config_path(), ask.url, ask.channel)?;
    let current = up::current_version();
    let offer = up::fetch_offer(&source, !ask.check)?;
    let trust = if offer.signed {
        "signed"
    } else {
        "unverified: [update] public_key is empty, so the signature was not checked"
    };
    if !offer.newer_than(&current) {
        println!(
            "regulus {current} is up to date ({} on {}, {trust})",
            offer.version, source.channel
        );
        return Ok(());
    }
    println!(
        "regulus {} is available on {} ({trust}); running {current}",
        offer.version, source.channel
    );
    if ask.check {
        return Ok(());
    }
    let platform = up::platform();
    let entry = offer.binary(&platform).with_context(|| {
        format!(
            "release {} has no program for {platform}",
            offer.manifest.version
        )
    })?;
    let exe = std::env::current_exe().context("cannot find this program's path")?;
    if !ask.yes {
        if !std::io::stdin().is_terminal() {
            bail!("pass --yes to replace {} without asking", exe.display());
        }
        eprint!(
            "Replace {} with regulus {}? [y/N] ",
            exe.display(),
            offer.version
        );
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("nothing changed");
            return Ok(());
        }
    }
    let bytes = up::download(&source, entry)?;
    up::install(&exe, &bytes, &offer.version)?;
    println!("updated {} to regulus {}", exe.display(), offer.version);
    Ok(())
}

/// `[update] check_on_start`: one line on stderr when a newer release exists.
fn announce_update(update: &regulus_core::config::UpdateConfig, path: &std::path::Path) {
    use regulus_cli::update as up;
    let offer = up::Source::new(update, path, None, None)
        .and_then(|source| Ok((up::fetch_offer(&source, false)?, source)));
    match offer {
        Ok((offer, source)) if offer.newer_than(&up::current_version()) => eprintln!(
            "regulus: {} is available on {} (`regulus update`)",
            offer.version, source.channel
        ),
        Ok(_) => {}
        Err(error) => eprintln!("regulus: update check: {error:#}"),
    }
}

/// `regulus update keygen` and `regulus update sign`: the release side.
fn update_tool(command: UpdateCommand) -> Result<()> {
    use regulus_cli::update as up;
    match command {
        UpdateCommand::Keygen { out, force } => {
            let mut public_path = out.clone().into_os_string();
            public_path.push(".pub");
            let public_path = PathBuf::from(public_path);
            if !force && (out.exists() || public_path.exists()) {
                bail!("{} exists; --force overwrites it", out.display());
            }
            let pair = up::keygen()?;
            write_private(&out, &pair.secret_file)?;
            std::fs::write(
                &public_path,
                format!(
                    "untrusted comment: minisign public key {}\n{}\n",
                    pair.key_id, pair.public_key
                ),
            )?;
            println!(
                "secret key: {} (keep it off the machines you update)",
                out.display()
            );
            println!("public key: {}", public_path.display());
            println!();
            println!("[update]");
            println!("public_key = \"{}\"", pair.public_key);
            Ok(())
        }
        UpdateCommand::Sign {
            files,
            key,
            release_version,
        } => {
            let text = match key {
                Some(path) => std::fs::read_to_string(&path)
                    .with_context(|| format!("cannot read {}", path.display()))?,
                None => std::env::var(up::KEY_ENV).with_context(|| {
                    format!("no key: pass --key <file> or set ${}", up::KEY_ENV)
                })?,
            };
            let key = up::SecretKey::parse(&text)?;
            for path in up::sign_files(&key, &files, release_version.as_deref())? {
                println!("{}", path.display());
            }
            Ok(())
        }
    }
}

/// Writes a file only its owner can read. The mode given at creation does not apply to a
/// file that exists (`keygen --force`), so it is set again before anything is written.
fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    std::io::Write::write_all(&mut file, text.as_bytes())?;
    Ok(())
}

/// A connection to the core the home directory points at, made per command.
struct Client<'a> {
    config: &'a Config,
    timeout: Duration,
}

impl Client<'_> {
    /// Connects and says `hello`.
    async fn connect(&self) -> Result<WsClient> {
        let config = self.config;
        let endpoint = regulus_core::local::endpoint(config).with_context(|| {
            format!(
                "no running core: {} is missing",
                config.port_path().display()
            )
        })?;
        Ok(rpc::connect(&endpoint, CLIENT_NAME, self.timeout).await?.0)
    }

    async fn request<T: DeserializeOwned>(&self, name: &str, params: Value) -> Result<T> {
        rpc::call(&self.connect().await?, name, params).await
    }
}

fn print_graph(profile: &Profile) {
    println!("{} ({})", profile.name, profile.id);
    for workflow in &profile.workflows {
        let main = if workflow.id == profile.main_workflow_id {
            " [main]"
        } else {
            ""
        };
        println!("\n{} ({}){main}", workflow.name, workflow.id);
        for port in &workflow.ports {
            let direction = match port.direction {
                Direction::Input => "in",
                Direction::Output => "out",
            };
            println!(
                "  port {WORKFLOW_SELF}.{:<11} {direction:<14} {}",
                port.id, port.capability
            );
        }
        for node in &workflow.nodes {
            let owner = node.module_id.as_deref().unwrap_or("-");
            println!("  node {:<16} {:<14} {owner}", node.id, node.node_type);
        }
        for wire in &workflow.wires {
            let arrow = match wire.kind {
                WireKind::Uses => "uses",
                WireKind::Then => "then",
            };
            println!(
                "  wire {}.{} -{arrow}-> {}.{}",
                wire.from.node_id, wire.from.port_id, wire.to.node_id, wire.to.port_id
            );
        }
    }
}
