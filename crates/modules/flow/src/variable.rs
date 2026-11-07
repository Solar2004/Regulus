//! `variable`: a named value kept in the module's space of the store. `set` writes it;
//! any value on `get` sends it out on `value`. Per execution, it is there again when a
//! restart resumes the execution; per project, every execution of the project shares it.

use regulus_core::ModuleError;
use regulus_proto::NodeEvent;
use serde_json::{json, Value};

use crate::{lock, Here};

const SET: &str = "set";
const GET: &str = "get";
const VALUE: &str = "value";
/// The `scope` shared by every execution of the project.
const PROJECT: &str = "project";
/// Every variable key starts with this, so the module's space can hold other things.
const PREFIX: &str = "var";

/// The store key: scope, then name. Nodes with the same name in a scope share it.
fn key(here: &Here) -> (String, String) {
    let name = here.text("name").trim().to_string();
    let name = if name.is_empty() {
        here.me.node_id.clone()
    } else {
        name
    };
    let scope = if here.text("scope") == PROJECT {
        PROJECT.to_string()
    } else {
        here.me.execution_id.clone()
    };
    (format!("{PREFIX}/{scope}/{name}"), name)
}

fn write(here: &Here, key: &str, value: &Value) -> Result<(), ModuleError> {
    match here.ctx().store(Some(&here.me)) {
        Ok(store) => store.set_json(key, value),
        Err(_) => {
            lock(&here.flow.memory).insert(key.into(), value.clone());
            Ok(())
        }
    }
}

fn read(here: &Here, key: &str) -> Result<Option<Value>, ModuleError> {
    match here.ctx().store(Some(&here.me)) {
        Ok(store) => store.get_json(key),
        Err(_) => Ok(lock(&here.flow.memory).get(key).cloned()),
    }
}

pub(crate) fn event(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    let (key, name) = key(here);
    match event.port.as_str() {
        SET => {
            write(here, &key, &event.value)?;
            // The checkpoint is what makes `[store] resume = checkpoint` bring the
            // execution back; the value itself is in the store. Without a store there is
            // nothing to resume.
            let _ = here.ctx().checkpoint(&here.me, json!({ "variable": name }));
            Ok(json!({ "set": name }))
        }
        GET => {
            let value = read(here, &key)?.or_else(|| here.setting("initial").cloned());
            match value {
                Some(value) => {
                    here.emit(VALUE, value)?;
                    Ok(json!({ "sent": [VALUE] }))
                }
                None => Ok(json!({ "unset": name })),
            }
        }
        other => Err(ModuleError::InvalidParams(format!(
            "variable takes `{SET}` and `{GET}`, not `{other}`"
        ))),
    }
}
