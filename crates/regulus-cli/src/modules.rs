//! The modules this host links in. Registration is manual (decision #10, docs/plan.md):
//! adding a module is one line here plus its crate in `Cargo.toml`.

use regulus_core::Module;
pub use regulus_module_tools_mcp::Bridge;

/// Every module compiled into this binary, freshly built, for a host that cannot start
/// bridges (tests).
pub fn installed() -> Vec<Box<dyn Module>> {
    hosted(Bridge::default())
}

/// Every module compiled into this binary, freshly built. The core decides which ones
/// load (`--without`, protocol compatibility); this list only says which exist. `bridge`
/// is how protocol bridges started by agents reach this host.
pub fn hosted(bridge: Bridge) -> Vec<Box<dyn Module>> {
    vec![
        Box::new(regulus_module_hello::Hello::new()),
        Box::new(regulus_module_agent::Agent::new()),
        Box::new(regulus_module_engine_opencode::module()),
        Box::new(regulus_module_engine_claude::module()),
        Box::new(regulus_module_providers::Providers::new()),
        Box::new(regulus_module_skills::Skills::new()),
        Box::new(regulus_module_communication::Communication::new()),
        Box::new(regulus_module_tools::Tools::new()),
        Box::new(regulus_module_tools_mcp::ToolsMcp::new(bridge)),
    ]
}
