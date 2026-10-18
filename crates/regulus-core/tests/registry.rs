//! The registry contains every failure of a module at its edge (docs/03-modules.md §2.2).

use regulus_core::registry::CallError;
use regulus_core::{Module, ModuleCtx, ModuleError, Registry};
use regulus_proto::{Manifest, ModuleInfo, ModuleState};
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq)]
enum Does {
    Work,
    Fail,
    Panic,
}

struct Fake {
    manifest: Manifest,
    init: Does,
    call: Does,
}

fn fake(
    id: &str,
    methods: &[&str],
    requires: Option<&str>,
    init: Does,
    call: Does,
) -> Box<dyn Module> {
    let module = ModuleInfo {
        id: id.into(),
        name: id.into(),
        requires_proto: requires.map(Into::into),
        methods: methods.iter().map(|m| m.to_string()).collect(),
        workflow_id: None,
        origin: None,
    };
    Box::new(Fake {
        manifest: Manifest {
            module,
            nodes: vec![],
        },
        init,
        call,
    })
}

fn works(id: &str, methods: &[&str]) -> Box<dyn Module> {
    fake(id, methods, None, Does::Work, Does::Work)
}

impl Module for Fake {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        match self.init {
            Does::Work => Ok(()),
            Does::Fail => Err(ModuleError::Failed("no luck".into())),
            Does::Panic => panic!("init exploded"),
        }
    }
    fn call(&self, method: &str, _: Value) -> Result<Value, ModuleError> {
        match self.call {
            Does::Work => Ok(json!({ "method": method })),
            Does::Fail => Err(ModuleError::Failed("call failed".into())),
            Does::Panic => panic!("call exploded"),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn state(registry: &Registry, id: &str) -> ModuleState {
    registry
        .summaries()
        .into_iter()
        .find(|s| s.id == id)
        .expect("module listed")
        .state
}

#[test]
fn a_fresh_module_is_idle_and_never_used_until_called() {
    let registry = Registry::load(vec![works("t/a@1.0.0", &["a.ping"])], &[]);
    assert_eq!(
        state(&registry, "t/a@1.0.0"),
        ModuleState::Idle { last_used: None }
    );
    assert_eq!(
        registry.call("a.ping", json!({})).unwrap()["method"],
        "a.ping"
    );
    assert!(matches!(
        state(&registry, "t/a@1.0.0"),
        ModuleState::Active { .. }
    ));
}

#[test]
fn without_skips_a_module_by_id_with_or_without_version() {
    let modules = || {
        vec![
            works("t/a@1.0.0", &["a.ping"]),
            works("t/b@1.0.0", &["b.ping"]),
        ]
    };
    for skip in ["t/a", "t/a@1.0.0"] {
        let registry = Registry::load(modules(), &[skip.to_string()]);
        assert!(!registry.is_installed("t/a"));
        assert!(matches!(
            registry.call("a.ping", json!({})),
            Err(CallError::NotFound)
        ));
        assert!(registry.call("b.ping", json!({})).is_ok());
    }
}

#[test]
fn init_failures_and_panics_leave_the_module_broken_and_the_rest_working() {
    let registry = Registry::load(
        vec![
            fake(
                "t/fails@1.0.0",
                &["fails.ping"],
                None,
                Does::Fail,
                Does::Work,
            ),
            fake(
                "t/panics@1.0.0",
                &["panics.ping"],
                None,
                Does::Panic,
                Does::Work,
            ),
            works("t/fine@1.0.0", &["fine.ping"]),
        ],
        &[],
    );
    assert!(
        matches!(state(&registry, "t/fails@1.0.0"), ModuleState::Broken { reason } if reason.contains("no luck"))
    );
    assert!(
        matches!(state(&registry, "t/panics@1.0.0"), ModuleState::Broken { reason } if reason.contains("init exploded"))
    );
    assert!(registry.call("fine.ping", json!({})).is_ok());
}

#[test]
fn a_panic_in_call_breaks_only_that_module() {
    let registry = Registry::load(
        vec![
            fake("t/bad@1.0.0", &["bad.ping"], None, Does::Work, Does::Panic),
            works("t/fine@1.0.0", &["fine.ping"]),
        ],
        &[],
    );
    assert!(matches!(
        registry.call("bad.ping", json!({})),
        Err(CallError::Panicked(_))
    ));
    assert!(
        matches!(state(&registry, "t/bad@1.0.0"), ModuleState::Broken { reason } if reason.contains("call exploded"))
    );
    assert!(matches!(
        registry.call("bad.ping", json!({})),
        Err(CallError::Unavailable { .. })
    ));
    assert!(registry.call("fine.ping", json!({})).is_ok());
}

#[test]
fn module_errors_pass_through_without_breaking_it() {
    let registry = Registry::load(
        vec![fake(
            "t/err@1.0.0",
            &["err.ping"],
            None,
            Does::Work,
            Does::Fail,
        )],
        &[],
    );
    assert!(matches!(
        registry.call("err.ping", json!({})),
        Err(CallError::Module(_))
    ));
    assert!(state(&registry, "t/err@1.0.0").is_usable());
}

#[test]
fn an_incompatible_protocol_requirement_is_not_loaded() {
    let registry = Registry::load(
        vec![fake(
            "t/old@1.0.0",
            &["old.ping"],
            Some("^9"),
            Does::Work,
            Does::Work,
        )],
        &[],
    );
    assert!(
        matches!(state(&registry, "t/old@1.0.0"), ModuleState::Incompatible { requires, .. } if requires == "^9")
    );
    assert!(matches!(
        registry.call("old.ping", json!({})),
        Err(CallError::NotFound)
    ));
}

#[test]
fn a_module_cannot_take_a_method_already_served() {
    let registry = Registry::load(
        vec![
            works("t/a@1.0.0", &["shared.ping"]),
            works("t/b@1.0.0", &["shared.ping"]),
            works("t/c@1.0.0", &["module.list"]),
        ],
        &[],
    );
    assert!(state(&registry, "t/a@1.0.0").is_usable());
    assert!(matches!(
        state(&registry, "t/b@1.0.0"),
        ModuleState::Broken { .. }
    ));
    assert!(matches!(
        state(&registry, "t/c@1.0.0"),
        ModuleState::Broken { .. }
    ));
}
