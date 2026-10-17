//! The ablation matrix (docs/03-modules.md §4): removing any installed module leaves every
//! other one loaded and usable. Generic over `installed()`, so a new module is covered the
//! moment it is registered.

use regulus_cli::modules::installed;
use regulus_core::Registry;

fn ids() -> Vec<String> {
    installed()
        .iter()
        .map(|m| m.manifest().module.id.clone())
        .collect()
}

#[test]
fn every_installed_module_loads() {
    let registry = Registry::load(installed(), &[]);
    for summary in registry.summaries() {
        assert!(
            summary.state.is_usable(),
            "{} is {:?}",
            summary.id,
            summary.state
        );
    }
    assert_eq!(registry.summaries().len(), ids().len());
}

#[test]
fn removing_any_module_leaves_the_rest_working() {
    for removed in ids() {
        let registry = Registry::load(installed(), std::slice::from_ref(&removed));
        assert!(!registry.is_installed(&removed));
        for summary in registry.summaries() {
            assert!(
                summary.state.is_usable(),
                "without {removed}: {} is {:?}",
                summary.id,
                summary.state
            );
        }
        assert_eq!(
            registry.summaries().len(),
            ids().len() - 1,
            "without {removed}"
        );
    }
}

#[test]
fn the_core_runs_with_no_modules_at_all() {
    let all = ids();
    let registry = Registry::load(installed(), &all);
    assert!(registry.summaries().is_empty());
    assert_eq!(registry.methods().count(), 0);
}
