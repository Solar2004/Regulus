//! The modules this host links in. Registration is manual for now (decision #10, docs/plan.md):
//! adding a module is one line here plus its crate in `Cargo.toml`.

use regulus_core::Module;

/// Every module compiled into this binary, freshly built. The core decides which ones
/// load (`--without`, protocol compatibility); this list only says which exist.
pub fn installed() -> Vec<Box<dyn Module>> {
    vec![Box::new(regulus_module_hello::Hello::new())]
}
