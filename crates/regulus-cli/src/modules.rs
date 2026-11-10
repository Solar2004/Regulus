//! The modules this host links in. Registration is manual (decision #10, docs/plan.md):
//! adding a module is one line here plus its crate in `Cargo.toml`.

use std::path::PathBuf;

use regulus_core::Module;

/// How processes that agents start reach this host: its own program, and the arguments
/// that point it at the running core (`--home <home>`). Each bridge adds its subcommand.
#[derive(Debug, Clone, Default)]
pub struct Host {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// The subcommand of the MCP bridge (`regulus tools-mcp`).
pub const TOOLS_MCP_COMMAND: &str = "tools-mcp";
/// The subcommand of the hook bridge (`regulus hook`).
pub const HOOK_COMMAND: &str = "hook";

impl Host {
    fn bridge(&self, command: &str) -> (PathBuf, Vec<String>) {
        let mut args = self.args.clone();
        args.push(command.into());
        (self.program.clone(), args)
    }
}

/// Every module compiled into this binary, freshly built, for a host that cannot start
/// bridges (tests).
pub fn installed() -> Vec<Box<dyn Module>> {
    hosted(Host::default())
}

/// Every module compiled into this binary, freshly built. The core decides which ones
/// load (`--without`, protocol compatibility); this list only says which exist. `host`
/// is how bridges started by agents reach this host.
pub fn hosted(host: Host) -> Vec<Box<dyn Module>> {
    let (program, args) = host.bridge(TOOLS_MCP_COMMAND);
    let tools = regulus_module_tools_mcp::Bridge { program, args };
    let (program, args) = host.bridge(HOOK_COMMAND);
    let hooks = regulus_module_agent::HookBridge { program, args };
    vec![
        Box::new(regulus_module_hello::Hello::new()),
        Box::new(regulus_module_flow::Flow::new()),
        Box::new(regulus_module_hooks::Hooks::new()),
        Box::new(regulus_module_agent::Agent::with_bridge(hooks)),
        Box::new(regulus_module_engine_opencode::module()),
        Box::new(regulus_module_engine_claude::module()),
        Box::new(regulus_module_providers::Providers::new()),
        Box::new(regulus_module_skills::Skills::new()),
        Box::new(regulus_module_communication::Communication::new()),
        Box::new(regulus_module_tools::Tools::new()),
        Box::new(regulus_module_tools_mcp::ToolsMcp::new(tools)),
        Box::new(regulus_module_files::Files::new()),
        Box::new(regulus_module_process::Process::new()),
        Box::new(regulus_module_guards::Guards::new()),
        Box::new(regulus_module_judge::Judge::new()),
        Box::new(regulus_module_triggers::Triggers::new()),
        Box::new(regulus_module_memory::Memory::new()),
        Box::new(regulus_module_web::Web::new()),
        Box::new(regulus_module_human::Human::new()),
        Box::new(regulus_module_service::Service::new()),
        Box::new(regulus_module_tasks::Tasks::new()),
        Box::new(regulus_module_workspace::Workspace::new()),
        Box::new(regulus_module_documents::Documents::new()),
        Box::new(regulus_module_inference::Inference::new()),
        Box::new(regulus_module_engine_native::module()),
        Box::new(regulus_module_delegation::Delegation::new()),
        Box::new(regulus_module_code::Code::new()),
    ]
}
