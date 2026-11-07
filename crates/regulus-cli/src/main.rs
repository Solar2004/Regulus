//! `regulus`: runs the core, and talks to a running one like any other client. Every
//! command except `serve` goes over the socket; none of them reaches into the core.

use std::io::{BufRead, IsTerminal};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use jsonrpsee::ws_client::WsClient;
use regulus_cli::rpc;
use regulus_core::config::{default_home, Config};
use regulus_proto::{
    method, BusEvent, BusEventKind, EventPage, Execution, ExecutionState, ModuleSummary, Profile,
    ProfileSummary, WireKind, PROTOCOL,
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
    /// Stop a running execution.
    Stop { execution: String },
    /// List executions.
    Ps,
    /// Talk to an agent node of a running execution.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
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
    /// Secrets that workflows name as `secret:<name>`.
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
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
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// List profiles.
    Ls {
        #[arg(long)]
        project: Option<String>,
    },
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
            return regulus_cli::tui::run(config.clone(), client.timeout).await;
        }
        // Scripts and pipes get the usage, as a missing command always gave them.
        Cli::command().print_help()?;
        std::process::exit(2);
    };
    match command {
        Command::Serve { port, without } => serve(config, port, without).await,
        Command::ToolsMcp => tools_bridge(&client).await,
        Command::Call { method, params } => {
            let params: Value = serde_json::from_str(&params).context("params must be JSON")?;
            let result: Value = client.request(&method, params).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
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
                let detail: Vec<String> = detail.map(|(k, v)| format!("{k}={v}")).collect();
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
                println!("{:<24} {}", profile.id, profile.name);
            }
            Ok(())
        }
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
                events(&client, &ws, &filter, Follow::UntilIdle).await?;
            }
            Ok(())
        }
        Command::Events {
            follow,
            execution,
            project,
        } => {
            let ws = client.connect().await?;
            let filter = EventFilter { execution, project };
            let follow = if follow { Follow::Always } else { Follow::No };
            events(&client, &ws, &filter, follow).await
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

/// Prints bus events from the oldest kept, polling every `client.refresh_ms` while
/// following.
async fn events(
    client: &Client<'_>,
    ws: &WsClient,
    filter: &EventFilter,
    follow: Follow,
) -> Result<()> {
    let poll = Duration::from_millis(client.config.client.refresh_ms);
    let deadline = Instant::now() + client.timeout;
    let mut since = 0;
    loop {
        let params = serde_json::json!({
            "since": since, "execution_id": filter.execution, "project_id": filter.project,
        });
        let page: EventPage = rpc::call(ws, method::EVENT_LIST, params).await?;
        for event in &page.events {
            print_event(event);
        }
        since = page.next;
        let done = match follow {
            Follow::No => true,
            Follow::UntilIdle => page.pending == 0,
            Follow::Always => false,
        };
        if done {
            return Ok(());
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

fn print_execution(execution: &Execution) {
    let state = match &execution.state {
        ExecutionState::Starting => "starting".to_string(),
        ExecutionState::Running => "running".to_string(),
        ExecutionState::Stopped => "stopped".to_string(),
        ExecutionState::Failed { reason } => format!("failed: {reason}"),
    };
    println!("{:<12} {:<16} {state}", execution.id, execution.workflow_id);
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
    // Agents start the tools bridge as this same program, pointed at this same home.
    let bridge = regulus_cli::modules::Bridge {
        program: std::env::current_exe().context("cannot find this program's path")?,
        args: vec![
            "--home".into(),
            config.home.to_string_lossy().into_owned(),
            TOOLS_MCP_COMMAND.into(),
        ],
    };
    let (core, warnings) = regulus_core::assemble(&config, regulus_cli::modules::hosted(bridge));
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
    tokio::signal::ctrl_c().await?;
    running.stop().await;
    core.shutdown();
    Ok(())
}

const TOOLS_MCP_COMMAND: &str = "tools-mcp";

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
