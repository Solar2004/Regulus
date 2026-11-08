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

/// The veto row (docs/11-composition.md §13): a project that disables a module the core
/// was started without still opens, says the module is not loaded, and the rest stays
/// usable. The core never knows the module by name.
#[test]
fn a_project_that_vetoes_a_removed_module_leaves_the_rest_working() {
    use regulus_core::projects::{Project, Projects};
    use regulus_core::service::ProjectHome;
    use regulus_core::{Core, ProfileStore};
    use regulus_proto::{method, ModuleSummary, ProjectSummary};

    let root = std::env::temp_dir().join(format!("regulus-ablation-veto-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let folder = root.join("demo");
    std::fs::create_dir_all(&folder).unwrap();
    for removed in ids() {
        std::fs::write(
            folder.join("regulus.toml"),
            format!(
                "[modules]\ndisable = [{:?}]\n",
                removed.split('@').next().unwrap()
            ),
        )
        .unwrap();
        let core = Core::build(
            installed(),
            std::slice::from_ref(&removed),
            ProfileStore::default(),
        );
        let mut projects = Projects::default();
        projects
            .insert(Project::load(&folder.canonicalize().unwrap()).unwrap())
            .unwrap();
        core.set_projects(
            projects,
            ProjectHome {
                config: root.join("config.toml"),
                home: root.clone(),
            },
        );
        let listed: Vec<ProjectSummary> = serde_json::from_value(
            core.dispatch(method::PROJECT_LIST, serde_json::Value::Null)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(listed.len(), 1, "without {removed}");
        assert!(
            listed[0]
                .warnings
                .iter()
                .any(|w| w.contains("is not loaded")),
            "without {removed}: {:?}",
            listed[0].warnings
        );
        let modules: Vec<ModuleSummary> = serde_json::from_value(
            core.dispatch(method::MODULE_LIST, serde_json::Value::Null)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(modules.len(), ids().len() - 1, "without {removed}");
        assert!(
            modules.iter().all(|m| m.state.is_usable()),
            "without {removed}"
        );
        core.shutdown();
    }
    let _ = std::fs::remove_dir_all(&root);
}
