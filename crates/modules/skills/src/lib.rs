//! Skills (docs/12-agents.md §3). A skill node answers `skill` with where its folder is;
//! the engine of each agent it is wired into places it in that agent's profile.

use std::path::PathBuf;

use regulus_contract_skill::{Skill, DESCRIBE, ENTRY};
use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError};
use regulus_proto::{Manifest, NodeCall};
use serde_json::Value;

const MANIFEST: &str = include_str!("../regulus-module.toml");

pub struct Skills {
    manifest: Manifest,
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

impl Skills {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("skills manifest is valid"),
        }
    }

    fn text<'a>(call: &'a NodeCall, field: &str) -> Option<&'a str> {
        call.node
            .config
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    fn describe(&self, call: &NodeCall) -> Result<Skill, ModuleError> {
        let id = &call.node.id;
        let path = Self::text(call, "path")
            .map(expand_home)
            .ok_or_else(|| ModuleError::Failed(format!("skill `{id}` has no `path` set")))?;
        if !path.is_absolute() || !path.join(ENTRY).is_file() {
            return Err(ModuleError::Failed(format!(
                "skill `{id}`: `{}` is not an absolute folder with a {ENTRY}",
                path.display()
            )));
        }
        let name = match Self::text(call, "name") {
            Some(name) => name.to_string(),
            None => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .ok_or_else(|| ModuleError::Failed(format!("skill `{id}` has no name")))?,
        };
        if name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(ModuleError::InvalidParams(format!(
                "skill `{id}`: `{name}` is not a valid folder name"
            )));
        }
        Ok(Skill { name, path })
    }
}

impl Default for Skills {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Skills {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, _ctx: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            DESCRIBE => {
                let call: NodeCall = serde_json::from_value(params)
                    .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
                serde_json::to_value(self.describe(&call)?)
                    .map_err(|e| ModuleError::Failed(e.to_string()))
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(config: Value) -> Result<Value, ModuleError> {
        let node = json!({
            "id": "review", "type": "skill", "name": "Review",
            "module_id": "regulus/skills@0.1.0", "config": config,
        });
        Skills::new().call(DESCRIBE, json!({ "execution_id": "exec-1", "node": node }))
    }

    #[test]
    fn describes_a_folder_with_a_skill_md() {
        let dir = std::env::temp_dir().join(format!("regulus-skill-{}", std::process::id()));
        let folder = dir.join("review");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(ENTRY), "---\nname: review\n---\n").unwrap();
        let path = folder.to_string_lossy();
        let skill = call(json!({ "path": path })).unwrap();
        assert_eq!(skill["name"], "review");
        assert_eq!(skill["path"], path.as_ref());
        let renamed = call(json!({ "path": path, "name": "check" })).unwrap();
        assert_eq!(renamed["name"], "check");
        assert!(call(json!({ "path": path, "name": "../x" })).is_err());
        assert!(call(json!({ "path": dir.to_string_lossy() })).is_err());
        assert!(call(json!({})).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
