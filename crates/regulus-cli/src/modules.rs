//! The modules this host links in. Registration is manual (decision #10, docs/plan.md):
//! adding a module is one line here plus its crate in `Cargo.toml`.

use regulus_core::Module;

/// Every module compiled into this binary, freshly built. The core decides which ones
/// load (`--without`, protocol compatibility); this list only says which exist.
pub fn installed() -> Vec<Box<dyn Module>> {
    vec![
        Box::new(regulus_module_hello::Hello::new()),
        Box::new(regulus_module_agent::Agent::new()),
        Box::new(regulus_module_engine_opencode::module()),
        Box::new(regulus_module_engine_claude::module()),
    ]
}
