//! The `instructions` node: a repository's own instructions for agents (AGENTS.md and
//! the like) as a `hook@1`. On its event it answers with the files that exist, as
//! context. It reads them as text through the workspace it is wired to, so they stay
//! inside the root and within its limits, and nothing of the repository runs.

use std::collections::HashSet;

use regulus_core::ModuleError;
use regulus_proto::{Hook, HookAnswer, HookEffect, HookFailure, HookRequest, NodeCall};
use serde_json::Value;

use crate::contract::{action, FileText, Look};
use crate::{field, node_ref, parse, to_json, Code, Fault, Fields, INSTRUCTIONS};

/// What stands for the file's path in `header`.
const PATH_HOLE: &str = "{{path}}";
/// What ends a text that was cut.
const CUT: &str = "\n[cut]";

/// Only an `instructions` node is a hook.
fn is_hook(call: &NodeCall, method: &str) -> Result<(), ModuleError> {
    if call.node.node_type == INSTRUCTIONS {
        Ok(())
    } else {
        Err(ModuleError::UnknownMethod(method.into()))
    }
}

impl Code {
    /// What the hook listens to: `hook`.
    pub(crate) fn describe_hook(&self, call: NodeCall) -> Result<Value, ModuleError> {
        is_hook(&call, regulus_proto::HOOK_DESCRIBE)?;
        let fields = Fields {
            manifest: &self.manifest,
            node: &call.node,
        };
        to_json(Hook {
            event: fields.text(field::EVENT).trim().to_string(),
            matcher: None,
            timeout_ms: None,
            returns: vec![HookEffect::Context],
            fail: HookFailure::Open,
        })
    }

    /// `node/hook`: on its event, the instructions as context; nothing on any other, and
    /// nothing when the repository has none.
    pub(crate) fn answer_hook(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        is_hook(&call, regulus_proto::method::NODE_HOOK)?;
        let request: HookRequest = parse(std::mem::take(&mut call.args))?;
        let fields = Fields {
            manifest: &self.manifest,
            node: &call.node,
        };
        if request.event != fields.text(field::EVENT).trim() {
            return to_json(HookAnswer::Nothing);
        }
        let backing = self.backing(&node_ref(&call))?;
        let header = fields.text(field::HEADER);
        let mut seen = HashSet::new();
        let mut parts = Vec::new();
        for file in fields.list(field::FILES) {
            let look = Look {
                action: action::READ.into(),
                path: Some(file.clone()),
                query: None,
            };
            let read: FileText = match backing.look(&look) {
                Ok(read) => parse(read)?,
                // Not there, a folder, outside the root, not text: not an instruction.
                Err(Fault::Request(_)) => continue,
                Err(Fault::Failed(why)) => {
                    eprintln!(
                        "regulus: instructions `{}`: `{file}` not read: {why}",
                        call.node.id
                    );
                    continue;
                }
            };
            // A file reached under two names (CLAUDE.md linked to AGENTS.md) is one file.
            if !seen.insert(read.path.clone()) {
                continue;
            }
            let mut text = read.text.trim_end().to_string();
            if read.truncated {
                text.push_str(CUT);
            }
            parts.push(format!("{}\n{text}", header.replace(PATH_HOLE, &read.path)));
        }
        let mut text = parts.join("\n\n");
        let max = usize::try_from(fields.count(field::MAX_CHARS)).unwrap_or(usize::MAX);
        if let Some((at, _)) = text.char_indices().nth(max) {
            text.truncate(at);
            text.push_str(CUT);
        }
        to_json(if text.is_empty() {
            HookAnswer::Nothing
        } else {
            HookAnswer::Context { text }
        })
    }
}
