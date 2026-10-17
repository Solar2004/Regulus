//! `regulus`: runs the core, and talks to a running one like any other client. Every
//! command except `serve` goes over the socket; none of them reaches into the core.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::params::ObjectParams;
use jsonrpsee::ws_client::{HeaderMap, WsClient, WsClientBuilder};
use regulus_core::config::{default_home, Config};
use regulus_proto::{
    method, Execution, ExecutionState, HelloParams, HelloResult, ModuleSummary, Profile,
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
    #[command(subcommand)]
    command: Command,
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
    /// Start a workflow of a profile (its main workflow unless --workflow).
    Run {
        profile: String,
        #[arg(long)]
        workflow: Option<String>,
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
    match cli.command {
        Command::Serve { port, without } => serve(config, port, without).await,
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
        Command::Run { profile, workflow } => {
            let params = serde_json::json!({ "profile_id": profile, "workflow_id": workflow });
            let execution: Execution = client.request(method::EXECUTION_START, params).await?;
            print_execution(&execution);
            if let ExecutionState::Failed { reason } = execution.state {
                bail!("{reason}");
            }
            Ok(())
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
    }
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
    let (core, warnings) = regulus_core::assemble(&config, regulus_cli::modules::installed());
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

/// A connection to the core the home directory points at, made per command.
struct Client<'a> {
    config: &'a Config,
    timeout: Duration,
}

impl Client<'_> {
    /// Connects and says `hello`.
    async fn connect(&self) -> Result<WsClient> {
        let config = self.config;
        let port = std::fs::read_to_string(config.port_path()).with_context(|| {
            format!(
                "no running core: {} is missing",
                config.port_path().display()
            )
        })?;
        let token = std::fs::read_to_string(config.token_path())
            .with_context(|| format!("cannot read {}", config.token_path().display()))?;
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", format!("Bearer {}", token.trim()).parse()?);
        let url = format!("ws://{}:{}/", config.server.host, port.trim());
        let client = WsClientBuilder::default()
            .set_headers(headers)
            .request_timeout(self.timeout)
            .build(&url)
            .await
            .with_context(|| format!("cannot connect to {url}"))?;
        let hello = HelloParams {
            client: CLIENT_NAME.into(),
            proto: Some(PROTOCOL.into()),
        };
        let answer: HelloResult = client
            .request(method::HELLO, object(serde_json::to_value(hello)?)?)
            .await?;
        if !answer.compatible {
            bail!(
                "core {} speaks protocol {}, this client speaks {PROTOCOL}",
                answer.core,
                answer.proto
            );
        }
        Ok(client)
    }

    /// A call error from the core is shown as its message alone.
    async fn request<T: DeserializeOwned>(&self, name: &str, params: Value) -> Result<T> {
        match self.connect().await?.request(name, object(params)?).await {
            Ok(value) => Ok(value),
            Err(jsonrpsee::core::client::Error::Call(error)) => bail!("{}", error.message()),
            Err(error) => Err(error.into()),
        }
    }
}

/// The protocol takes named parameters only; `null` means none.
fn object(params: Value) -> Result<ObjectParams> {
    let mut out = ObjectParams::new();
    match params {
        Value::Null => {}
        Value::Object(map) => {
            for (key, value) in map {
                out.insert(&key, value)?;
            }
        }
        other => bail!("params must be a JSON object, got {other}"),
    }
    Ok(out)
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
