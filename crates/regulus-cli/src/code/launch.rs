//! Carrying a launch out over the socket (docs/04-client.md §12): looking at what the core
//! has, deciding (`session::plan`), applying the template, starting, attaching. The screen
//! and the scripted commands share every step here; nothing is decided twice.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use jsonrpsee::ws_client::WsClient;
use regulus_core::Config;
use regulus_proto::{
    code, method, Execution, ExecutionState, Manifest, ModuleSummary, Profile, ProfileSummary,
    TemplateApplied, TemplateRequire, TemplateSummary, TemplateText, TemplateVar,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};

use super::inspect::{self, Ask};
use super::session::{
    candidates, plan, subject, workspaces, Candidate, Known, Origin, Plan, Session, Step, Wanted,
    ROOT,
};
use crate::rpc::{self, CallError};
use crate::talk;
use crate::tui::core::{self, Connection, METHOD_NOT_FOUND};
use crate::tui::keys::Options;

/// A failed step with the core's code, when it gave one.
pub type Failure = CallError;

fn failure(message: impl Into<String>) -> Failure {
    Failure {
        code: None,
        message: message.into(),
    }
}

async fn call<T: DeserializeOwned>(ws: &WsClient, name: &str, params: Value) -> Result<T, Failure> {
    rpc::call_coded(ws, name, params).await
}

/// What the core has that a launch is decided from.
#[derive(Debug, Clone, PartialEq)]
pub struct Survey {
    pub profiles: Vec<Profile>,
    pub executions: Vec<Execution>,
    pub modules: Vec<ModuleSummary>,
    pub manifests: Vec<Manifest>,
    /// The template a launch applies, as the core lists it; why not, if it does not.
    pub template: Result<TemplateSummary, String>,
    /// Live executions whose workspace says it works in the wanted folder.
    pub in_folder: Vec<String>,
}

/// Whether two spellings name one folder. A folder that is not there is only itself.
pub fn same_folder(one: &str, other: &str) -> bool {
    let real = |path: &str| std::fs::canonicalize(path).unwrap_or_else(|_| path.into());
    real(one) == real(other)
}

/// A template named by a path: relative ones are made absolute here, since the core
/// resolves them against its home, not this directory.
fn template_place(template: &str) -> String {
    let is_path = template.contains('/') || template.ends_with(".toml");
    if !is_path || Path::new(template).is_absolute() {
        return template.to_string();
    }
    std::path::absolute(template)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| template.to_string())
}

/// The vars and the modules a template file declares, read from its text: `template.list`
/// only knows the templates of the configured paths, and a launch may name a file.
fn summary_of(file: &TemplateText) -> Result<TemplateSummary, String> {
    let table: toml::Table = toml::from_str(&file.text).map_err(|e| e.to_string())?;
    let template = table
        .get("template")
        .and_then(toml::Value::as_table)
        .ok_or("it has no [template] table")?;
    let text = |table: &toml::Table, key: &str| {
        table
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    };
    let vars = template
        .get("vars")
        .and_then(toml::Value::as_table)
        .map(|vars| {
            vars.iter()
                .map(|(name, var)| {
                    let var = var.as_table().cloned().unwrap_or_default();
                    TemplateVar {
                        name: name.clone(),
                        description: text(&var, "description").unwrap_or_default(),
                        default: text(&var, "default"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let requires = template
        .get("requires")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|require| {
            let require = require.as_table()?;
            Some(TemplateRequire {
                module: text(require, "module")?,
                version: text(require, "version").unwrap_or_default(),
            })
        })
        .collect();
    Ok(TemplateSummary {
        name: file.name.clone(),
        description: text(template, "description").unwrap_or_default(),
        path: file.path.clone(),
        vars,
        requires,
        error: None,
        project_id: None,
    })
}

async fn template(ws: &WsClient, wanted: &str, config: &Config) -> Result<TemplateSummary, String> {
    let place = template_place(wanted);
    if place != wanted || Path::new(&place).is_absolute() {
        let file: TemplateText = call(ws, method::TEMPLATE_GET, json!({ "template": place }))
            .await
            .map_err(|e| e.message)?;
        return summary_of(&file).map_err(|e| format!("{}: {e}", file.path));
    }
    let list: Vec<TemplateSummary> = call(ws, method::TEMPLATE_LIST, Value::Null)
        .await
        .map_err(|e| e.message)?;
    match list.into_iter().find(|t| t.name == wanted) {
        Some(found) => match &found.error {
            Some(error) => Err(format!("{}: {error}", found.path)),
            None => Ok(found),
        },
        None => {
            let places: Vec<String> = config
                .templates
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            Err(format!(
                "no template `{wanted}` in {}: put its file there (the repository has it in \
                 templates/), or name a file with --template",
                places.join(", ")
            ))
        }
    }
}

/// Every profile the core has loaded, whole.
async fn profiles(ws: &WsClient) -> Result<Vec<Profile>, Failure> {
    let listed: Vec<ProfileSummary> = call(ws, method::PROFILE_LIST, Value::Null).await?;
    let mut profiles = Vec::with_capacity(listed.len());
    for summary in listed {
        profiles.push(call(ws, method::PROFILE_GET, json!({ "id": summary.id })).await?);
    }
    Ok(profiles)
}

/// Looks at what the core has. A template that cannot be read and a workspace that does
/// not answer are part of the picture, not failures.
pub async fn survey(ws: &WsClient, wanted: &Wanted, config: &Config) -> Result<Survey, Failure> {
    let profiles = profiles(ws).await?;
    let executions: Vec<Execution> = call(ws, method::EXECUTION_LIST, Value::Null).await?;
    let modules: Vec<ModuleSummary> = call(ws, method::MODULE_LIST, Value::Null).await?;
    let manifests: Vec<Manifest> = call(ws, method::MODULE_MANIFEST, Value::Null).await?;
    let template = template(ws, &wanted.template, config).await;
    let mut in_folder = Vec::new();
    if let Some(root) = wanted
        .root
        .as_deref()
        .filter(|_| wanted.execution.is_none())
    {
        for candidate in candidates(&profiles, &executions, &manifests) {
            let live = candidate.execution.state.is_live() && candidate.execution.parent.is_none();
            let Some(workspace) = candidate.workspace.as_deref().filter(|_| live) else {
                continue;
            };
            let params = Ask::summary().params(&candidate.execution.id, workspace);
            let answer: Result<Value, Failure> = call(ws, inspect::METHOD, params).await;
            if answer.is_ok_and(|a| same_folder(&inspect::summary(&a).root, root)) {
                in_folder.push(candidate.execution.id.clone());
            }
        }
    }
    Ok(Survey {
        profiles,
        executions,
        modules,
        manifests,
        template,
        in_folder,
    })
}

/// What a launch does with what the core has.
pub fn decide(wanted: &Wanted, survey: &Survey) -> Plan {
    let same = |folder: &str| {
        wanted
            .root
            .as_deref()
            .is_some_and(|root| same_folder(folder, root))
    };
    plan(
        wanted,
        &Known {
            profiles: &survey.profiles,
            executions: &survey.executions,
            manifests: &survey.manifests,
            in_folder: &survey.in_folder,
            same_folder: &same,
        },
    )
}

/// The step that carries a plan out with what the person set and nothing else: the folder
/// and the vars of the flags. `None`: the plan refuses.
pub fn step(plan: &Plan, wanted: &Wanted) -> Option<Step> {
    match plan {
        Plan::Attach { execution } => Some(Step::Attach {
            execution: execution.clone(),
        }),
        Plan::Start { profile } => Some(Step::Start {
            profile: profile.clone(),
        }),
        Plan::Apply { profile, force } => {
            let mut vars = Map::new();
            if let Some(root) = &wanted.root {
                vars.insert(ROOT.into(), Value::from(root.clone()));
            }
            for (name, value) in &wanted.vars {
                vars.insert(name.clone(), Value::from(value.clone()));
            }
            Some(Step::Apply {
                template: wanted.template.clone(),
                vars,
                profile: profile.clone(),
                force: *force,
            })
        }
        Plan::Refuse(_) => None,
    }
}

async fn start(ws: &WsClient, profile: &str) -> Result<Execution, Failure> {
    let execution: Execution = call(
        ws,
        method::EXECUTION_START,
        json!({ "profile_id": profile }),
    )
    .await?;
    match &execution.state {
        ExecutionState::Failed { reason } => Err(failure(format!(
            "execution {} of `{profile}` failed to start: {reason}",
            execution.id
        ))),
        _ => Ok(execution),
    }
}

/// The session of an execution: its profile, and who to talk to and what to look at in the
/// workflow it runs.
pub async fn open(
    ws: &WsClient,
    execution: &str,
    wanted: &Wanted,
    origin: Origin,
) -> Result<Session, Failure> {
    let executions: Vec<Execution> = call(ws, method::EXECUTION_LIST, Value::Null).await?;
    let execution = executions
        .into_iter()
        .find(|e| e.id == execution)
        .ok_or_else(|| {
            failure(format!(
                "no execution `{execution}` (see `regulus code ls`)"
            ))
        })?;
    let params = json!({ "id": execution.profile_id });
    let profile: Profile = call(ws, method::PROFILE_GET, params).await?;
    let manifests: Vec<Manifest> = call(ws, method::MODULE_MANIFEST, Value::Null).await?;
    let workflow = profile.workflow(&execution.workflow_id).ok_or_else(|| {
        failure(format!(
            "profile `{}` has no workflow `{}` any more",
            profile.id, execution.workflow_id
        ))
    })?;
    let found = subject(
        workflow,
        &manifests,
        wanted.agent.as_deref(),
        wanted.workspace.as_deref(),
    )
    .map_err(failure)?;
    Ok(Session {
        execution,
        profile,
        agent: found.agent,
        workspace: found.workspace,
        notes: found.notes,
        origin,
    })
}

/// Carries a step out and answers with the session it leads to.
pub async fn carry_out(ws: &WsClient, step: Step, wanted: &Wanted) -> Result<Session, Failure> {
    match step {
        Step::Attach { execution } => open(ws, &execution, wanted, Origin::Attached).await,
        Step::Start { profile } => {
            let execution = start(ws, &profile).await?;
            open(ws, &execution.id, wanted, Origin::Started).await
        }
        Step::Apply {
            template,
            vars,
            profile,
            force,
        } => {
            let params = json!({
                "template": template_place(&template), "vars": vars, "id": profile,
                "force": force,
            });
            let applied: TemplateApplied = call(ws, method::TEMPLATE_APPLY, params).await?;
            let execution = start(ws, &applied.profile_id).await?;
            let origin = Origin::Applied {
                path: applied.path,
                diagnostics: applied.diagnostics,
            };
            open(ws, &execution.id, wanted, origin).await
        }
    }
}

/// How a failed step reads on a command line: the core's message, with what to do about
/// the codes that have something to do.
pub fn explain(failure: &Failure) -> String {
    let message = &failure.message;
    match failure.code {
        Some(code::TEMPLATE_UNMET) => format!(
            "{message}\nthe template needs a module this core does not have: `regulus module \
             ls` shows what is installed"
        ),
        Some(code::PROFILE_EXISTS) => format!("{message}\n--reset writes it again"),
        _ => message.clone(),
    }
}

/// Connects like the terminal UI does: to the core of this home, starting one if none
/// answers and `client.autostart` allows it.
pub async fn connect(config: &Config, timeout: Duration) -> Result<WsClient> {
    let starting = || eprintln!("regulus: no core answered, starting one");
    let (ws, connection) = core::open(config, timeout, true, &starting).await?;
    if let Connection::Connected { core, proto } = connection {
        eprintln!("regulus: connected to {core} (proto {proto})");
    }
    Ok(ws)
}

/// Finds or makes the session `wanted` asks for, without a screen: what the flags set is
/// all there is, and what the plan refuses is the error.
pub async fn launch(ws: &WsClient, wanted: &Wanted, config: &Config) -> Result<Session> {
    let survey = survey(ws, wanted, config)
        .await
        .map_err(|e| anyhow!("{}", explain(&e)))?;
    let plan = decide(wanted, &survey);
    let step = match (step(&plan, wanted), &plan) {
        (Some(step), _) => step,
        (None, Plan::Refuse(why)) => bail!("{why}"),
        (None, _) => bail!("nothing to do"),
    };
    if let (Step::Apply { .. }, Err(why)) = (&step, &survey.template) {
        bail!("{why}");
    }
    let session = carry_out(ws, step, wanted)
        .await
        .map_err(|e| anyhow!("{}", explain(&e)))?;
    Ok(session)
}

/// What a person is told about a session on stderr: how it came to be, what the core
/// noted, and what is off in it.
pub fn notes(session: &Session) -> Vec<String> {
    let id = &session.execution.id;
    let mut notes = Vec::new();
    match &session.origin {
        Origin::Attached => notes.push(format!(
            "attached to {id}: the conversation so far is the Agent box's record"
        )),
        Origin::Started => notes.push(format!(
            "started {id} from profile `{}`: a new session, the model starts without \
             the context of earlier ones",
            session.profile.id
        )),
        Origin::Applied { path, diagnostics } => {
            notes.push(format!(
                "wrote {path} and started {id}: a new session, the model starts without \
                 the context of earlier ones"
            ));
            for d in diagnostics {
                notes.push(format!(
                    "{}: {} ({})",
                    severity(d.severity),
                    d.message,
                    d.code
                ));
            }
        }
    }
    for node in &session.execution.disabled {
        notes.push(format!(
            "node `{}` is disabled: nothing usable is wired into {}",
            node.node_id,
            node.missing.join(", ")
        ));
    }
    notes.extend(session.notes.iter().cloned());
    notes
}

fn severity(severity: regulus_proto::Severity) -> &'static str {
    match severity {
        regulus_proto::Severity::Error => "error",
        regulus_proto::Severity::Warning => "warning",
        regulus_proto::Severity::Info => "info",
    }
}

/// `regulus code --detach`: the ids a script goes on with, one `name value` per line.
pub fn print_session(session: &Session) -> String {
    let mut out = format!(
        "execution {}\nprofile   {}\nagent     {}\n",
        session.execution.id, session.profile.id, session.agent
    );
    if let Some(workspace) = &session.workspace {
        out.push_str(&format!("workspace {workspace}\n"));
    }
    if let Some(folder) = session.folder() {
        out.push_str(&format!("root      {folder}\n"));
    }
    out
}

/// How a scripted launch talks after it has its session.
pub enum Talking {
    /// Print the ids and leave.
    Detach,
    /// Send this, print the reply and leave.
    Once(String),
    /// Line by line from stdin, like `regulus talk`.
    Lines,
}

/// `regulus code` without a screen: the session, then what `talking` says. The talking is
/// `regulus talk`'s, on the session's agent.
pub async fn run(
    config: &Config,
    options: &Options,
    timeout: Duration,
    wanted: &Wanted,
    talking: Talking,
) -> Result<()> {
    let ws = connect(config, timeout).await?;
    let session = launch(&ws, wanted, config).await?;
    for note in notes(&session) {
        eprintln!("regulus: {note}");
    }
    let once = match talking {
        Talking::Detach => {
            print!("{}", print_session(&session));
            return Ok(());
        }
        Talking::Once(text) => Some(text),
        Talking::Lines => None,
    };
    let settings = talk::Settings::new(config, options, timeout);
    let talk = talk::Talk {
        execution: session.execution.id.clone(),
        node: Some(session.agent.clone()),
        once,
        ..talk::Talk::default()
    };
    talk::run(&ws, config, &settings, talk).await
}

/// The workspace node `regulus code inspect` asks: the one named, else the only one of
/// the execution's workflow.
async fn workspace_of(ws: &WsClient, execution: &str, node: Option<&str>) -> Result<String> {
    if let Some(node) = node {
        return Ok(node.to_string());
    }
    let executions: Vec<Execution> = rpc::call(ws, method::EXECUTION_LIST, Value::Null).await?;
    let execution = executions
        .into_iter()
        .find(|e| e.id == execution)
        .with_context(|| format!("no execution `{execution}` (see `regulus code ls`)"))?;
    let params = json!({ "id": execution.profile_id });
    let profile: Profile = rpc::call(ws, method::PROFILE_GET, params).await?;
    let manifests: Vec<Manifest> = rpc::call(ws, method::MODULE_MANIFEST, Value::Null).await?;
    let workflow = profile.workflow(&execution.workflow_id).with_context(|| {
        format!(
            "profile `{}` has no workflow `{}` any more",
            profile.id, execution.workflow_id
        )
    })?;
    let found = workspaces(workflow, &manifests);
    match found.as_slice() {
        [only] => Ok(only.id.clone()),
        [] => bail!(
            "workflow `{}` of {} has no workspace node (a Uses output `code.workspace`); \
             name one inside a box with --node",
            workflow.id,
            execution.id
        ),
        several => {
            let flags: Vec<String> = several
                .iter()
                .map(|node| format!("--node {}", node.id))
                .collect();
            bail!(
                "workflow `{}` has more than one workspace: say which\n  {}",
                workflow.id,
                flags.join(", ")
            )
        }
    }
}

/// `regulus code inspect`: one read-only question to a workspace node, and its answer.
pub async fn inspect(
    ws: &WsClient,
    execution: &str,
    node: Option<&str>,
    ask: &Ask,
) -> Result<Value> {
    let node = workspace_of(ws, execution, node).await?;
    let answer: Result<Value, Failure> =
        call(ws, inspect::METHOD, ask.params(execution, &node)).await;
    answer.map_err(|failure| match failure.code {
        Some(METHOD_NOT_FOUND) => anyhow!(
            "no module of this core serves `{}`: the one that runs coding workspaces is \
             not installed (`regulus module ls`)",
            inspect::METHOD
        ),
        Some(code::MODULE_UNAVAILABLE) => anyhow!("unavailable: {}", failure.message),
        _ => anyhow!("{}", failure.message),
    })
}

/// `regulus code ls`: the executions a person can code with, oldest first.
pub async fn list(ws: &WsClient) -> Result<Vec<Candidate>> {
    let profiles = profiles(ws).await.map_err(|e| anyhow!("{}", e.message))?;
    let executions: Vec<Execution> = rpc::call(ws, method::EXECUTION_LIST, Value::Null).await?;
    let manifests: Vec<Manifest> = rpc::call(ws, method::MODULE_MANIFEST, Value::Null).await?;
    Ok(candidates(&profiles, &executions, &manifests))
}

/// One line of `regulus code ls`.
pub fn print_candidate(candidate: &Candidate) -> String {
    let execution = &candidate.execution;
    let state = match &execution.state {
        ExecutionState::Starting => "starting",
        ExecutionState::Running => "running",
        ExecutionState::Stopped => "stopped",
        ExecutionState::Failed { .. } => "failed",
        ExecutionState::Interrupted { .. } => "interrupted",
    };
    let mut line = format!(
        "{:<12} {state:<11} {}/{}  agent {}",
        execution.id,
        execution.profile_id,
        execution.workflow_id,
        candidate.agents.join(", ")
    );
    if let Some(workspace) = &candidate.workspace {
        line.push_str(&format!("  workspace {workspace}"));
    }
    if let Some(folder) = &candidate.folder {
        line.push_str(&format!("  {folder}"));
    }
    if let Some(parent) = &execution.parent {
        line.push_str(&format!("  (worker of {parent})"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_file_says_its_vars_and_what_it_needs() {
        let file = TemplateText {
            name: "regulus-code".into(),
            path: "/t/regulus-code.toml".into(),
            text: r#"
id = "regulus-code"
main_workflow_id = "code"

[template]
name = "regulus-code"
description = "A coding harness"
requires = [{ module = "regulus/agent", version = ">=0.1" }]
[template.vars]
root = { description = "The folder" }
model = { description = "The model", default = "qwen3:4b" }
"#
            .into(),
        };
        let summary = summary_of(&file).unwrap();
        assert_eq!(summary.description, "A coding harness");
        assert_eq!(summary.requires[0].module, "regulus/agent");
        let root = summary.vars.iter().find(|v| v.name == "root").unwrap();
        assert_eq!(root.default, None);
        let model = summary.vars.iter().find(|v| v.name == "model").unwrap();
        assert_eq!(model.default.as_deref(), Some("qwen3:4b"));
        let bare = TemplateText {
            text: "id = \"x\"\n".into(),
            ..file
        };
        assert_eq!(summary_of(&bare).unwrap_err(), "it has no [template] table");
    }

    #[test]
    fn a_step_carries_the_folder_and_what_the_flags_set() {
        let mut wanted = Wanted {
            root: Some("/work/app".into()),
            template: "regulus-code".into(),
            ..Wanted::default()
        };
        wanted.vars.insert("model".into(), "big".into());
        let plan = Plan::Apply {
            profile: "code-app".into(),
            force: true,
        };
        let Some(Step::Apply {
            template,
            vars,
            profile,
            force,
        }) = step(&plan, &wanted)
        else {
            panic!()
        };
        assert_eq!(template, "regulus-code");
        assert_eq!(profile, "code-app");
        assert!(force);
        assert_eq!(
            Value::Object(vars),
            json!({ "root": "/work/app", "model": "big" })
        );
        assert_eq!(step(&Plan::Refuse("no".into()), &wanted), None);
    }

    #[test]
    fn two_spellings_of_a_folder_are_one() {
        let dir = std::env::temp_dir().join(format!("regulus-code-same-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a")).unwrap();
        let plain = dir.join("a").display().to_string();
        let dotted = dir.join("a/../a").display().to_string();
        assert!(same_folder(&plain, &dotted));
        assert!(!same_folder(&plain, &dir.display().to_string()));
        assert!(same_folder("/nowhere/at/all", "/nowhere/at/all"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
