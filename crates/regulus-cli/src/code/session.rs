//! What a coding session is made of and what a launch does to get one
//! (docs/04-client.md §12). Pure: no socket, no terminal, no file system.
//!
//! Nothing here knows a module or a node by its name. The agent is the node with a Uses
//! input `agent.engine` (as `regulus talk` finds it); the workspace is the node that offers
//! `code.workspace` on a Uses output, as its profile or its manifest declares it; the model
//! shown is the one a node offering `llm.endpoint` is set to.

use std::collections::BTreeMap;
use std::path::Path;

use regulus_proto::{
    Diagnostic, Direction, Execution, Manifest, Node, Port, Profile, WireKind, Workflow,
};

use crate::conversation::{agent_nodes, is_agent};
use crate::tui::document::{base_module_id, slugify, unique_id};

/// Capabilities, by name without version.
pub const WORKSPACE: &str = "code.workspace";
pub const ENDPOINT: &str = "llm.endpoint";
/// The var of the template that takes the folder, and the workspace node's field it fills.
pub const ROOT: &str = "root";
/// What the profile a launch writes is called before the folder's own name.
const PROFILE_PREFIX: &str = "code";
/// Hex digits of the folder's hash in a profile id.
const HASH_DIGITS: usize = 6;

fn base(capability: &str) -> &str {
    capability.split('@').next().unwrap_or(capability)
}

/// The ports a node has: its own as the profile writes them or, if it writes none, those
/// its node type declares in the manifest of its module.
fn ports<'a>(node: &'a Node, manifests: &'a [Manifest]) -> &'a [Port] {
    if !node.ports.is_empty() {
        return &node.ports;
    }
    node.module_id
        .as_deref()
        .and_then(|id| {
            manifests
                .iter()
                .find(|m| base_module_id(&m.module.id) == base_module_id(id))
        })
        .and_then(|manifest| manifest.node_type(&node.node_type))
        .map_or(&node.ports, |node_type| &node_type.ports)
}

/// Whether a node offers `capability` (a name without version) on a Uses output.
pub fn offers(node: &Node, manifests: &[Manifest], capability: &str) -> bool {
    ports(node, manifests).iter().any(|port| {
        port.direction == Direction::Output
            && port.kind == WireKind::Uses
            && base(&port.capability) == capability
    })
}

/// The nodes of a workflow that are a coding workspace.
pub fn workspaces<'a>(workflow: &'a Workflow, manifests: &[Manifest]) -> Vec<&'a Node> {
    workflow
        .nodes
        .iter()
        .filter(|node| offers(node, manifests, WORKSPACE))
        .collect()
}

fn setting<'a>(node: &'a Node, field: &str) -> Option<&'a str> {
    node.config
        .get(field)
        .and_then(|value| value.as_str())
        .filter(|text| !text.trim().is_empty())
}

/// The folder a workspace node is set to work in.
pub fn folder_of(node: &Node) -> Option<&str> {
    setting(node, ROOT)
}

/// The folder a profile's main workflow works in: its first workspace that names one.
pub fn profile_folder<'a>(profile: &'a Profile, manifests: &[Manifest]) -> Option<&'a str> {
    let workflow = profile.workflow(&profile.main_workflow_id)?;
    workspaces(workflow, manifests)
        .into_iter()
        .find_map(folder_of)
}

/// The model a workflow talks to, as its first node offering `llm.endpoint` is set:
/// `model`, with its `preset` after it when it names one.
pub fn model_of(workflow: &Workflow, manifests: &[Manifest]) -> Option<String> {
    let node = workflow
        .nodes
        .iter()
        .find(|node| offers(node, manifests, ENDPOINT))?;
    let model = setting(node, "model");
    let preset = setting(node, "preset");
    match (model, preset) {
        (Some(model), Some(preset)) => Some(format!("{model} · {preset}")),
        (Some(one), None) | (None, Some(one)) => Some(one.to_string()),
        (None, None) => None,
    }
}

/// Who a coding session talks to and what it looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub agent: String,
    /// `None`: the workflow has no workspace node; the conversation works without it.
    pub workspace: Option<String>,
    /// What a person should know about how they were chosen.
    pub notes: Vec<String>,
}

/// Chooses the agent and the workspace of a workflow: the ones asked for, else the only
/// agent and the first workspace. More than one agent and none asked is refused with the
/// flag each takes, as `regulus talk` does.
pub fn subject(
    workflow: &Workflow,
    manifests: &[Manifest],
    agent: Option<&str>,
    workspace: Option<&str>,
) -> Result<Subject, String> {
    let agents = agent_nodes(workflow);
    let agent = match agent {
        Some(id) => match workflow.node(id) {
            Some(found) if !is_agent(found) => {
                return Err(format!(
                    "node `{id}` has no `agent.engine` input: it is not an agent"
                ))
            }
            // A node the workflow does not list (inside a box) is the core's to judge.
            _ => id.to_string(),
        },
        None => match agents.as_slice() {
            [only] => only.id.clone(),
            [] => {
                return Err(format!(
                    "workflow `{}` has no agent node (a Uses input `agent.engine`): there is \
                     nobody to code with",
                    workflow.id
                ))
            }
            several => {
                let flags: Vec<String> = several
                    .iter()
                    .map(|node| format!("--node {}", node.id))
                    .collect();
                return Err(format!(
                    "workflow `{}` has more than one agent: say which\n  {}",
                    workflow.id,
                    flags.join(", ")
                ));
            }
        },
    };
    let found = workspaces(workflow, manifests);
    let mut notes = Vec::new();
    let workspace = match workspace {
        Some(id) => match workflow.node(id) {
            Some(node) if !offers(node, manifests, WORKSPACE) => {
                return Err(format!(
                    "node `{id}` does not offer `{WORKSPACE}`: it is not a coding workspace"
                ))
            }
            _ => Some(id.to_string()),
        },
        None => {
            if let [first, _, ..] = found.as_slice() {
                let others: Vec<String> = found[1..]
                    .iter()
                    .map(|node| format!("--workspace {}", node.id))
                    .collect();
                notes.push(format!(
                    "more than one workspace: looking at `{}` ({} picks another)",
                    first.id,
                    others.join(", ")
                ));
            }
            found.first().map(|node| node.id.clone())
        }
    };
    if workspace.is_none() {
        notes.push(format!(
            "workflow `{}` has no workspace node (a Uses output `{WORKSPACE}`): files and \
             changes are not shown",
            workflow.id
        ));
    }
    Ok(Subject {
        agent,
        workspace,
        notes,
    })
}

/// An execution a person can code with: its workflow has an agent.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub execution: Execution,
    /// Its agents: a session talks to the only one, or the person says which.
    pub agents: Vec<String>,
    /// Its first workspace node, and the folder that node is set to.
    pub workspace: Option<String>,
    pub folder: Option<String>,
}

/// The executions whose workflow has an agent, in the order the core lists them. One whose
/// profile is no longer loaded cannot be read and is left out.
pub fn candidates(
    profiles: &[Profile],
    executions: &[Execution],
    manifests: &[Manifest],
) -> Vec<Candidate> {
    executions
        .iter()
        .filter_map(|execution| {
            let workflow = profiles
                .iter()
                .find(|profile| profile.id == execution.profile_id)?
                .workflow(&execution.workflow_id)?;
            let agents: Vec<String> = agent_nodes(workflow)
                .iter()
                .map(|node| node.id.clone())
                .collect();
            let workspace = workspaces(workflow, manifests).into_iter().next();
            (!agents.is_empty()).then(|| Candidate {
                execution: execution.clone(),
                agents,
                workspace: workspace.map(|node| node.id.clone()),
                folder: workspace.and_then(folder_of).map(str::to_string),
            })
        })
        .collect()
}

/// FNV-1a, 64 bits.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The id of the profile a launch writes for a folder: `code-<folder name>-<hash>`. The
/// name is slugged like every id; the hash is the first six of the sixteen hex digits of
/// the FNV-1a 64 of the folder's absolute path, so two folders with the same name do not
/// share a profile.
pub fn profile_id(root: &Path) -> String {
    let name = root
        .file_name()
        .map(|name| slugify(&name.to_string_lossy()))
        .filter(|slug| !slug.is_empty())
        .unwrap_or_else(|| "folder".to_string());
    let hash = format!("{:016x}", fnv1a(root.to_string_lossy().as_bytes()));
    format!("{PROFILE_PREFIX}-{name}-{}", &hash[..HASH_DIGITS])
}

/// What `regulus code` was asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wanted {
    /// The folder to work in, absolute. `None` only with `execution`.
    pub root: Option<String>,
    /// The person named the folder, rather than it being where the command ran.
    pub root_given: bool,
    /// Attach to this execution and set nothing up.
    pub execution: Option<String>,
    /// The profile to run or to write. Without it: the folder's.
    pub profile: Option<String>,
    pub template: String,
    /// The template vars the person set, besides the folder.
    pub vars: BTreeMap<String, String>,
    /// Start an execution even if one is live.
    pub fresh: bool,
    /// Write the profile again from the template.
    pub reset: bool,
    pub agent: Option<String>,
    pub workspace: Option<String>,
}

/// What the core has that a plan is made from.
pub struct Known<'a> {
    pub profiles: &'a [Profile],
    pub executions: &'a [Execution],
    pub manifests: &'a [Manifest],
    /// Live executions whose workspace says it works in the wanted folder.
    pub in_folder: &'a [String],
    /// Whether a folder a profile names is the wanted one. The file system's to say: two
    /// spellings can be one folder.
    pub same_folder: &'a dyn Fn(&str) -> bool,
}

/// What a launch does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// A live session is there: talk to it.
    Attach {
        execution: String,
    },
    /// The profile is there: run it as it is.
    Start {
        profile: String,
    },
    /// Write the profile from the template, then run it. `force` replaces the one there.
    Apply {
        profile: String,
        force: bool,
    },
    Refuse(String),
}

/// Decides what a launch does. In order: the execution asked for; a live session of the
/// folder; the folder's profile as it is; a new profile from the template. A profile is
/// replaced only when asked (`reset`) and never when it belongs to another folder.
pub fn plan(wanted: &Wanted, known: &Known<'_>) -> Plan {
    if let Some(id) = &wanted.execution {
        return match known.executions.iter().find(|e| e.id == *id) {
            Some(execution) => Plan::Attach {
                execution: execution.id.clone(),
            },
            None => Plan::Refuse(format!("no execution `{id}` (see `regulus code ls`)")),
        };
    }
    let Some(root) = wanted.root.as_deref() else {
        return Plan::Refuse("say the folder to work in, or an execution to attach to".into());
    };
    let here = |profile: &&Profile| {
        profile_folder(profile, known.manifests).is_some_and(|f| (known.same_folder)(f))
    };
    let (id, existing) = match &wanted.profile {
        Some(id) => (id.clone(), known.profiles.iter().find(|p| p.id == *id)),
        None => {
            let derived = profile_id(Path::new(root));
            let own: Vec<&Profile> = known.profiles.iter().filter(here).collect();
            match own.iter().find(|p| p.id == derived).or(own.first()) {
                Some(found) => (found.id.clone(), Some(*found)),
                None => {
                    let taken = known.profiles.iter().map(|p| p.id.as_str());
                    (unique_id(&derived, taken, PROFILE_PREFIX), None)
                }
            }
        }
    };
    let Some(profile) = existing else {
        return match session_in_folder(wanted, known) {
            Some(execution) => Plan::Attach { execution },
            None => Plan::Apply {
                profile: id,
                force: false,
            },
        };
    };
    // A profile named by hand may work somewhere else.
    let elsewhere = profile_folder(profile, known.manifests).filter(|f| !(known.same_folder)(f));
    if let Some(other) = elsewhere.filter(|_| wanted.root_given || wanted.reset) {
        return Plan::Refuse(format!(
            "profile `{id}` works in {other}, not in {root}: a launch never replaces another \
             folder's profile. Leave the folder out to run it there, or name a new profile \
             with --profile"
        ));
    }
    if wanted.reset {
        return Plan::Apply {
            profile: id,
            force: true,
        };
    }
    if !wanted.vars.is_empty() {
        let names: Vec<&str> = wanted.vars.keys().map(String::as_str).collect();
        return Plan::Refuse(format!(
            "profile `{id}` is already written, so {} would not reach it: pass --reset to \
             write it again from the template (what was edited in it is lost), or leave \
             them out to run it as it is",
            names.join(", ")
        ));
    }
    if !wanted.fresh {
        let live = known
            .executions
            .iter()
            .rfind(|e| e.profile_id == id && e.state.is_live() && e.parent.is_none());
        if let Some(execution) = live {
            return Plan::Attach {
                execution: execution.id.clone(),
            };
        }
        if let Some(execution) = session_in_folder(wanted, known) {
            return Plan::Attach { execution };
        }
    }
    Plan::Start { profile: id }
}

/// The newest live session another profile runs in the wanted folder, unless the person
/// named a profile or asked for something new.
fn session_in_folder(wanted: &Wanted, known: &Known<'_>) -> Option<String> {
    if wanted.fresh || wanted.reset || !wanted.vars.is_empty() || wanted.profile.is_some() {
        return None;
    }
    known
        .executions
        .iter()
        .rfind(|e| e.state.is_live() && known.in_folder.contains(&e.id))
        .map(|e| e.id.clone())
}

/// What carries a plan out.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Attach {
        execution: String,
    },
    Start {
        profile: String,
    },
    Apply {
        template: String,
        vars: serde_json::Map<String, serde_json::Value>,
        profile: String,
        force: bool,
    },
}

/// How a session came to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    /// It was running: the conversation so far is there and the model has its context.
    Attached,
    /// Started now from a profile that was there.
    Started,
    /// Started now from a profile just written.
    Applied {
        path: String,
        diagnostics: Vec<Diagnostic>,
    },
}

/// A coding session a client is attached to.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub execution: Execution,
    /// The profile as the core has it now; the execution runs the copy it started with.
    pub profile: Profile,
    pub agent: String,
    pub workspace: Option<String>,
    pub notes: Vec<String>,
    pub origin: Origin,
}

impl Session {
    pub fn workflow(&self) -> Option<&Workflow> {
        self.profile.workflow(&self.execution.workflow_id)
    }

    /// The folder its workspace node is set to.
    pub fn folder(&self) -> Option<&str> {
        let workspace = self.workspace.as_deref()?;
        self.workflow()?.node(workspace).and_then(folder_of)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regulus_core::parse_manifest;
    use serde_json::{json, Value};

    fn port(id: &str, capability: &str, direction: &str, kind: &str) -> Value {
        json!({ "id": id, "capability": capability, "direction": direction, "kind": kind })
    }

    fn agent(id: &str) -> Value {
        json!({ "id": id, "type": "agent", "module_id": "regulus/agent@0.1.0",
                "ports": [port("engine", "agent.engine@1", "input", "uses")] })
    }

    fn workspace(id: &str, root: &str) -> Value {
        json!({ "id": id, "type": "workspace", "module_id": "regulus/code@0.1.0",
                "config": { "root": root },
                "ports": [port("workspace", "code.workspace@1", "output", "uses"),
                          port("tools", "tool@1", "output", "uses")] })
    }

    fn profile(id: &str, nodes: Value) -> Profile {
        serde_json::from_value(json!({
            "id": id, "main_workflow_id": "code",
            "workflows": [{ "id": "code", "kind": "main", "nodes": nodes }]
        }))
        .unwrap()
    }

    fn execution(id: &str, profile: &str, status: &str) -> Execution {
        serde_json::from_value(json!({
            "id": id, "profile_id": profile, "workflow_id": "code", "project_id": null,
            "state": { "status": status }
        }))
        .unwrap()
    }

    fn wanted(root: &str) -> Wanted {
        Wanted {
            root: Some(root.into()),
            template: "regulus-code".into(),
            ..Wanted::default()
        }
    }

    fn decide(wanted: &Wanted, profiles: &[Profile], executions: &[Execution]) -> Plan {
        let root = wanted.root.clone().unwrap_or_default();
        let same = move |folder: &str| folder == root;
        plan(
            wanted,
            &Known {
                profiles,
                executions,
                manifests: &[],
                in_folder: &[],
                same_folder: &same,
            },
        )
    }

    #[test]
    fn a_workspace_is_told_by_its_capability_in_the_profile_or_the_manifest() {
        let manifest = parse_manifest(
            "[module]\nid = \"x/space@1.0.0\"\nname = \"space\"\n[[node]]\ntype = \"ws\"\n\
             name = \"ws\"\n[[node.port]]\nid = \"out\"\ncapability = \"code.workspace@1\"\n\
             direction = \"output\"\nkind = \"uses\"\n",
        )
        .unwrap();
        let bare: Node =
            serde_json::from_value(json!({ "id": "w", "type": "ws", "module_id": "x/space" }))
                .unwrap();
        assert!(!offers(&bare, &[], WORKSPACE));
        assert!(offers(&bare, std::slice::from_ref(&manifest), WORKSPACE));
        let written: Node = serde_json::from_value(workspace("w", "/a")).unwrap();
        assert!(offers(&written, &[], WORKSPACE));
        // An input of that capability is someone who uses a workspace, not one.
        let user: Node = serde_json::from_value(json!({ "id": "u", "type": "u",
            "ports": [port("ws", "code.workspace@1", "input", "uses")] }))
        .unwrap();
        assert!(!offers(&user, &[], WORKSPACE));
    }

    #[test]
    fn the_subject_is_the_only_agent_and_the_first_workspace() {
        let both = profile("p", json!([agent("agent"), workspace("workspace", "/a")]));
        let workflow = &both.workflows[0];
        let found = subject(workflow, &[], None, None).unwrap();
        assert_eq!(found.agent, "agent");
        assert_eq!(found.workspace.as_deref(), Some("workspace"));
        assert!(found.notes.is_empty(), "{:?}", found.notes);

        let alone = profile("p", json!([agent("dev")]));
        let found = subject(&alone.workflows[0], &[], None, None).unwrap();
        assert_eq!(found.workspace, None);
        assert!(
            found.notes[0].contains("no workspace node"),
            "{:?}",
            found.notes
        );

        let two = profile("p", json!([agent("a"), agent("b"), workspace("w", "/a")]));
        let refused = subject(&two.workflows[0], &[], None, None).unwrap_err();
        assert!(refused.contains("--node a, --node b"), "{refused}");
        assert_eq!(
            subject(&two.workflows[0], &[], Some("b"), None)
                .unwrap()
                .agent,
            "b"
        );
        let wrong = subject(&two.workflows[0], &[], Some("w"), None).unwrap_err();
        assert!(wrong.contains("not an agent"), "{wrong}");
        let wrong = subject(&two.workflows[0], &[], Some("a"), Some("b")).unwrap_err();
        assert!(wrong.contains("not a coding workspace"), "{wrong}");

        let none = profile("p", json!([workspace("w", "/a")]));
        let refused = subject(&none.workflows[0], &[], None, None).unwrap_err();
        assert!(refused.contains("no agent node"), "{refused}");
    }

    #[test]
    fn a_folder_gets_an_id_of_its_own() {
        let id = profile_id(Path::new("/home/a/My App"));
        assert!(id.starts_with("code-my-app-"), "{id}");
        assert_eq!(id.len(), "code-my-app-".len() + 6);
        assert_eq!(id, profile_id(Path::new("/home/a/My App")));
        assert_ne!(id, profile_id(Path::new("/home/b/My App")));
        assert!(profile_id(Path::new("/")).starts_with("code-folder-"));
        // FNV-1a 64 of the empty input is its offset basis: the function is the standard one.
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn a_first_launch_writes_the_folders_profile_and_the_next_runs_it() {
        let ask = wanted("/work/app");
        let id = profile_id(Path::new("/work/app"));
        assert_eq!(
            decide(&ask, &[], &[]),
            Plan::Apply {
                profile: id.clone(),
                force: false
            }
        );
        let written = profile(
            &id,
            json!([agent("agent"), workspace("workspace", "/work/app")]),
        );
        let profiles = [written];
        assert_eq!(
            decide(&ask, &profiles, &[]),
            Plan::Start {
                profile: id.clone()
            }
        );
        // A live session of it is attached to; a stopped one or a worker's is not.
        let mut child = execution("exec-3", &id, "running");
        child.parent = Some("exec-2".into());
        let executions = [
            execution("exec-1", &id, "stopped"),
            execution("exec-2", &id, "running"),
            child,
        ];
        assert_eq!(
            decide(&ask, &profiles, &executions),
            Plan::Attach {
                execution: "exec-2".into()
            }
        );
        let fresh = Wanted {
            fresh: true,
            ..ask.clone()
        };
        assert_eq!(
            decide(&fresh, &profiles, &executions),
            Plan::Start { profile: id }
        );
    }

    #[test]
    fn a_profile_another_client_wrote_for_the_folder_is_found_by_its_folder() {
        let theirs = profile(
            "code-app",
            json!([agent("agent"), workspace("w", "/work/app")]),
        );
        let other = profile(
            "code-other",
            json!([agent("agent"), workspace("w", "/work/x")]),
        );
        let profiles = [other, theirs];
        assert_eq!(
            decide(&wanted("/work/app"), &profiles, &[]),
            Plan::Start {
                profile: "code-app".into()
            }
        );
        // A session whose workspace answers with the folder is attached to, whatever runs it.
        let executions = [execution("exec-9", "unrelated", "running")];
        let ask = wanted("/work/app");
        let same = |_: &str| false;
        let found = plan(
            &ask,
            &Known {
                profiles: &[],
                executions: &executions,
                manifests: &[],
                in_folder: &["exec-9".to_string()],
                same_folder: &same,
            },
        );
        assert_eq!(
            found,
            Plan::Attach {
                execution: "exec-9".into()
            }
        );
    }

    #[test]
    fn a_profile_is_replaced_only_when_asked_and_never_another_folders() {
        let id = profile_id(Path::new("/work/app"));
        let written = profile(
            &id,
            json!([agent("agent"), workspace("workspace", "/work/app")]),
        );
        let profiles = [written];
        let mut model = wanted("/work/app");
        model.vars.insert("model".into(), "big".into());
        let Plan::Refuse(said) = decide(&model, &profiles, &[]) else {
            panic!()
        };
        assert!(said.contains("pass --reset"), "{said}");
        assert!(said.contains("model"), "{said}");
        model.reset = true;
        assert_eq!(
            decide(&model, &profiles, &[]),
            Plan::Apply {
                profile: id.clone(),
                force: true
            }
        );
        // The derived id taken by another folder's profile: the next free one, untouched.
        let squatter = profile(
            &id,
            json!([agent("agent"), workspace("workspace", "/else")]),
        );
        assert_eq!(
            decide(&wanted("/work/app"), std::slice::from_ref(&squatter), &[]),
            Plan::Apply {
                profile: format!("{id}-2"),
                force: false
            }
        );
        // Named by hand: run where it works, never rewritten for another folder.
        let named = Wanted {
            profile: Some(id.clone()),
            ..wanted("/work/app")
        };
        assert_eq!(
            decide(&named, std::slice::from_ref(&squatter), &[]),
            Plan::Start {
                profile: id.clone()
            }
        );
        for ask in [
            Wanted {
                root_given: true,
                ..named.clone()
            },
            Wanted {
                reset: true,
                ..named.clone()
            },
        ] {
            let Plan::Refuse(said) = decide(&ask, std::slice::from_ref(&squatter), &[]) else {
                panic!()
            };
            assert!(said.contains("works in /else, not in /work/app"), "{said}");
        }
    }

    #[test]
    fn an_execution_asked_for_is_attached_to_or_missed() {
        let ask = Wanted {
            execution: Some("exec-7".into()),
            ..Wanted::default()
        };
        let executions = [execution("exec-7", "p", "stopped")];
        assert_eq!(
            decide(&ask, &[], &executions),
            Plan::Attach {
                execution: "exec-7".into()
            }
        );
        assert!(matches!(decide(&ask, &[], &[]), Plan::Refuse(e) if e.contains("exec-7")));
        assert!(matches!(
            decide(&Wanted::default(), &[], &[]),
            Plan::Refuse(_)
        ));
    }

    #[test]
    fn the_executions_to_code_with_are_those_with_an_agent() {
        let code = profile(
            "code-app",
            json!([agent("agent"), workspace("w", "/work/app")]),
        );
        let chat = profile("chat", json!([agent("dev")]));
        let pipe = profile("pipe", json!([workspace("w", "/work/x")]));
        let executions = [
            execution("exec-1", "code-app", "running"),
            execution("exec-2", "pipe", "running"),
            execution("exec-3", "chat", "stopped"),
            execution("exec-4", "gone", "running"),
        ];
        let found = candidates(&[code, chat, pipe], &executions, &[]);
        let ids: Vec<&str> = found.iter().map(|c| c.execution.id.as_str()).collect();
        assert_eq!(ids, vec!["exec-1", "exec-3"]);
        assert_eq!(found[0].agents, vec!["agent"]);
        assert_eq!(found[0].workspace.as_deref(), Some("w"));
        assert_eq!(found[0].folder.as_deref(), Some("/work/app"));
        assert_eq!(found[1].workspace, None);
    }

    #[test]
    fn the_model_shown_is_what_the_endpoint_node_is_set_to() {
        let with = profile(
            "p",
            json!([agent("agent"),
                { "id": "provider", "type": "provider", "config": { "model": "qwen3:4b",
                  "preset": "ollama" },
                  "ports": [port("endpoint", "llm.endpoint@1", "output", "uses")] }]),
        );
        assert_eq!(
            model_of(&with.workflows[0], &[]).as_deref(),
            Some("qwen3:4b · ollama")
        );
        let without = profile("p", json!([agent("agent")]));
        assert_eq!(model_of(&without.workflows[0], &[]), None);
    }
}
