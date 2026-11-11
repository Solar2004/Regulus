//! What the core answers about writing nodes (protocol 1.17): the shape of a node, syncing
//! a profile's ports with it, the source of a local module, a new Python node, and
//! starting a project's local modules again without the rest of the core.
//!
//! A reload never routes an execution to a new process silently: while live or resuming
//! executions run nodes of a module it would replace, it refuses (`MODULE_IN_USE`) unless
//! asked to stop them first. A module whose new process fails to start keeps the old one.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use regulus_proto::shape::{self, NodeShape, ProfileSynced};
use regulus_proto::{
    code, IdParams, InUseExecution, Manifest, ModuleShapeParams, ModuleState, NodeInitParams,
    NodeInitialized, NodeLocation, NodeType, NodeTypeChanges, Profile, ProfileSyncParams,
    ProjectReloadParams, ProjectReloaded, ReloadOutcome, ReloadedModule, Severity, SourceCheck,
    SourceCheckParams, SourceDiagnostic, SourceParams, SourceSaveParams, SourceSaved, SourceText,
};
use serde_json::Value;

use super::{local_spec, Core, RpcError, UNTRUSTED};
use crate::authoring::{self, Checked, InitAsk, SourceFile};
use crate::graph::{check_shapes, PORTS_STALE};
use crate::projects::Project;
use crate::registry::{base_id, Prepared};

/// How many times a reload looks again for executions that started while it prepared.
const SWAP_TRIES: usize = 3;

/// What a reload is to do with one local module.
enum Plan {
    Start(crate::process::LocalSpec),
    Remove,
    Skip(String),
}

/// One local module on its way through a reload.
struct Step {
    local_id: String,
    base: String,
    previous: Option<(Arc<Manifest>, ModuleState)>,
    plan: Plan,
}

fn invalid(message: impl Into<String>) -> RpcError {
    RpcError::invalid_params(message)
}

/// Node types added, removed or changed between two manifests.
fn node_type_changes(before: Option<&Manifest>, after: Option<&Manifest>) -> NodeTypeChanges {
    let empty = Vec::new();
    let before = before.map_or(&empty, |m| &m.nodes);
    let after = after.map_or(&empty, |m| &m.nodes);
    let find = |list: &[NodeType], t: &str| list.iter().find(|n| n.node_type == t).cloned();
    let mut changes = NodeTypeChanges::default();
    for node in after {
        match find(before, &node.node_type) {
            None => changes.added.push(node.node_type.clone()),
            Some(old) if &old != node => changes.changed.push(node.node_type.clone()),
            Some(_) => {}
        }
    }
    for node in before {
        if find(after, &node.node_type).is_none() {
            changes.removed.push(node.node_type.clone());
        }
    }
    changes
}

fn reason(state: &ModuleState) -> String {
    match state {
        ModuleState::Broken { reason } | ModuleState::Blocked { reason } => reason.clone(),
        ModuleState::Incompatible { requires, have } => {
            format!("requires proto {requires}, core has {have}")
        }
        ModuleState::Disabled { missing } => format!("missing {}", missing.join(", ")),
        ModuleState::Active { .. } | ModuleState::Idle { .. } => String::new(),
    }
}

fn diagnostic(severity: Severity, code: &str, message: String) -> SourceDiagnostic {
    SourceDiagnostic {
        severity,
        code: code.into(),
        message,
        line: None,
        column: None,
        end_line: None,
        end_column: None,
    }
}

impl Core {
    /// `module.shape`.
    pub(super) fn module_shape(&self, params: ModuleShapeParams) -> Result<NodeShape, RpcError> {
        let manifest = self
            .registry
            .manifest(&params.module_id)
            .ok_or_else(|| invalid(format!("Unknown module `{}`", params.module_id)))?;
        let node_type = manifest.node_type(&params.node_type).ok_or_else(|| {
            invalid(format!(
                "module `{}` has no node type `{}`",
                manifest.module.id, params.node_type
            ))
        })?;
        Ok(shape::shape(node_type, &params.config))
    }

    /// `profile.sync`: never writes.
    pub(super) fn sync_profile(
        &self,
        params: ProfileSyncParams,
    ) -> Result<ProfileSynced, RpcError> {
        let profile = match (params.profile, params.profile_id) {
            (Some(profile), _) => profile,
            (None, Some(id)) => Profile::clone(&*self.profile(IdParams { id })?),
            (None, None) => return Err(invalid("give `profile` or `profile_id`")),
        };
        Ok(shape::sync_profile(
            profile,
            |id| self.registry.declared_manifest(id),
            params.workflow_id.as_deref(),
            params.node_ids.as_deref(),
        ))
    }

    /// The editable file of a local module of an open project.
    fn source_file(
        &self,
        project_id: &str,
        module: &str,
    ) -> Result<(Arc<Project>, SourceFile), RpcError> {
        let project = self.project(project_id)?;
        let file = authoring::source_of(&project, module)?;
        Ok((project, file))
    }

    /// `source.get`.
    pub(super) fn source_get(&self, params: SourceParams) -> Result<SourceText, RpcError> {
        let (project, file) = self.source_file(&params.project_id, &params.module)?;
        let max = self.local_config().max_source_bytes;
        let (text, revision) = authoring::read_source(&file, max)?.ok_or_else(|| {
            RpcError::new(
                code::SOURCE_UNAVAILABLE,
                format!(
                    "{} does not exist yet; save it to create it",
                    file.relative.display()
                ),
            )
        })?;
        Ok(SourceText {
            project_id: project.id.clone(),
            module: file.module.clone(),
            path: file.relative.display().to_string(),
            file: file.absolute.display().to_string(),
            language: authoring::PYTHON.into(),
            text,
            revision,
        })
    }

    /// `source.save`: one writer at a time; never runs or reloads the module.
    pub(super) fn source_save(&self, params: SourceSaveParams) -> Result<SourceSaved, RpcError> {
        let (project, file) = self.source_file(&params.project_id, &params.module)?;
        let max = self.local_config().max_source_bytes;
        let revision = {
            let _writing = self.writing();
            authoring::write_source(
                &file,
                &params.text,
                params.revision.as_deref(),
                params.force,
                max,
            )?
        };
        Ok(SourceSaved {
            path: file.relative.display().to_string(),
            file: file.absolute.display().to_string(),
            revision,
            check: self.check_source(&project, &file, &params.text),
        })
    }

    /// `source.check`.
    pub(super) fn source_check(&self, params: SourceCheckParams) -> Result<SourceCheck, RpcError> {
        let (project, file) = self.source_file(&params.project_id, &params.module)?;
        let text = match params.text {
            Some(text) => text,
            None => {
                let max = self.local_config().max_source_bytes;
                authoring::read_source(&file, max)?
                    .map(|(text, _)| text)
                    .ok_or_else(|| {
                        RpcError::new(
                            code::SOURCE_UNAVAILABLE,
                            format!("{} does not exist yet", file.relative.display()),
                        )
                    })?
            }
        };
        Ok(self.check_source(&project, &file, &text))
    }

    /// Reads `text`, the source of `file`, without running it: the host's checker, the
    /// canonical shape validation of what it declares, whether the project's SDK copy is
    /// this core's, and whether the loaded module already declares the same.
    fn check_source(&self, project: &Project, file: &SourceFile, text: &str) -> SourceCheck {
        let config = self.local_config();
        let cwd = self
            .project_home
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|h| h.home.clone())
            .filter(|home| home.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let mut check = SourceCheck {
            language: authoring::PYTHON.into(),
            checked: false,
            interpreter: None,
            diagnostics: Vec::new(),
            nodes: Vec::new(),
            complete: false,
            matches_loaded: None,
        };
        match authoring::check_python(&config, &cwd, text) {
            Checked::Unavailable(why) => {
                check
                    .diagnostics
                    .push(diagnostic(Severity::Warning, "check.unavailable", why))
            }
            Checked::Ran(answer) => read_answer(&answer, &mut check),
        }
        if let Some(note) = sdk_note(file, config.max_source_bytes) {
            check.diagnostics.push(note);
        }
        if check.complete {
            let base = format!("{}/{}", project.id, file.module);
            check.matches_loaded = self
                .registry
                .declared_manifest(&base)
                .filter(|m| base_id(&m.module.id) == base)
                .map(|m| m.nodes == check.nodes);
        }
        check
    }

    /// `node.init`: writes the node, then loads it as `project.reload` does.
    pub(super) fn node_init(&self, params: NodeInitParams) -> Result<NodeInitialized, RpcError> {
        let project = self.project(&params.project_id)?;
        let python = params.python.unwrap_or_else(|| self.local_config().python);
        let done = authoring::init_python(&InitAsk {
            folder: project.root.clone(),
            id: params.id.clone(),
            python,
        })?;
        let relative = |path: &PathBuf| {
            path.strip_prefix(&done.root)
                .unwrap_or(path)
                .display()
                .to_string()
        };
        let files: Vec<String> = done.files.iter().map(relative).collect();
        let reloaded = self.reload_project(ProjectReloadParams {
            id: project.id.clone(),
            module: Some(params.id.clone()),
            stop_executions: false,
        })?;
        let module = reloaded.modules.into_iter().next().ok_or_else(|| {
            RpcError::new(
                code::MODULE_FAILED,
                format!(
                    "`{}` was written but the project does not list it",
                    params.id
                ),
            )
        })?;
        Ok(NodeInitialized {
            project_id: project.id.clone(),
            module_id: format!("{}/{}", project.id, params.id),
            node_type: params.id,
            source: files.first().cloned().unwrap_or_default(),
            files,
            rewritten: done.rewritten,
            notes: done.notes,
            module,
        })
    }

    /// The live or resuming executions with a node of any of the modules `bases`
    /// (`<project>/<id>`).
    fn in_use(&self, bases: &BTreeSet<String>) -> Vec<InUseExecution> {
        let uses = |workflow: &regulus_proto::Workflow| -> Vec<String> {
            workflow
                .nodes
                .iter()
                .filter(|n| {
                    n.module_id
                        .as_deref()
                        .is_some_and(|m| bases.contains(base_id(m)))
                })
                .map(|n| n.id.clone())
                .collect()
        };
        let recovering = self.recovering();
        let executions = self.executions();
        let resuming = recovering
            .iter()
            .map(|stored| (&stored.execution, uses(&stored.workflow)));
        let live = executions
            .iter()
            .filter(|live| live.execution.state.is_live())
            .map(|live| (&live.execution, uses(&live.workflow)));
        resuming
            .chain(live)
            .filter(|(_, nodes)| !nodes.is_empty())
            .map(|(execution, nodes)| InUseExecution {
                id: execution.id.clone(),
                profile_id: execution.profile_id.clone(),
                workflow_id: execution.workflow_id.clone(),
                nodes,
            })
            .collect()
    }

    fn in_use_error(executions: Vec<InUseExecution>) -> RpcError {
        let ids: Vec<&str> = executions.iter().map(|e| e.id.as_str()).collect();
        RpcError::new(
            code::MODULE_IN_USE,
            format!(
                "{} running nodes of this module; stop {} first, or reload with \
                 stop_executions (--stop-executions)",
                ids.join(", "),
                if ids.len() == 1 { "it" } else { "them" }
            ),
        )
        .with_data(serde_json::json!({ "executions": executions }))
    }

    /// Stops executions as `execution.stop` does; their ids.
    fn stop_all(&self, executions: &[InUseExecution]) -> Vec<String> {
        executions
            .iter()
            .filter(|e| self.stop_recovering(&e.id).is_some() || self.stop(&e.id, true).is_some())
            .map(|e| e.id.clone())
            .collect()
    }

    /// `project.reload`.
    pub(super) fn reload_project(
        &self,
        params: ProjectReloadParams,
    ) -> Result<ProjectReloaded, RpcError> {
        // One reload at a time; the rest of the core goes on while candidates start.
        let _reloading = self.reloads.lock().unwrap_or_else(|p| p.into_inner());
        // A node written a moment ago (`node.init`, an editor) is in the file by now.
        self.refresh_projects();
        let project = self.project(&params.id)?;
        let owner = project.id.clone();
        let steps = self.plan_reload(&project, params.module.as_deref())?;

        // Asked before anything starts, so a refusal costs nothing.
        let touched: BTreeSet<String> = steps
            .iter()
            .filter(|s| s.previous.is_some() && !matches!(s.plan, Plan::Skip(_)))
            .map(|s| s.base.clone())
            .collect();
        let busy = self.in_use(&touched);
        if !busy.is_empty() && !params.stop_executions {
            return Err(Self::in_use_error(busy));
        }
        // What each module was when planned: the swap only happens if it still is.
        let planned: Vec<(String, Option<Arc<Manifest>>)> = steps
            .iter()
            .map(|s| (s.base.clone(), s.previous.as_ref().map(|(m, _)| m.clone())))
            .collect();

        // New processes, in parallel, each checked and initialized: nothing listed yet.
        let trusted = self.is_trusted(&project.root);
        let specs: Vec<&crate::process::LocalSpec> = steps
            .iter()
            .filter_map(|s| match &s.plan {
                Plan::Start(spec) => Some(spec),
                _ => None,
            })
            .collect();
        let presets: Vec<Option<ModuleState>> = specs
            .iter()
            .map(|spec| {
                let base = spec.base_id();
                let elsewhere = self.registry.is_installed(&base)
                    && self.registry.local(&owner, &base).is_none();
                if elsewhere {
                    Some(ModuleState::Broken {
                        reason: format!("duplicate id: a module `{base}` is already loaded"),
                    })
                } else if !trusted {
                    Some(ModuleState::Blocked {
                        reason: UNTRUSTED.into(),
                    })
                } else {
                    None
                }
            })
            .collect();
        let owned: Vec<crate::process::LocalSpec> = specs.into_iter().cloned().collect();
        let mut started = self.spawn_locals(&owned, presets).into_iter();
        let mut prepared: HashMap<String, Prepared> = HashMap::new();
        for step in &steps {
            if let Plan::Start(_) = step.plan {
                if let Some(local) = started.next() {
                    let ready = self.registry.prepare_local(&owner, local, &step.base);
                    prepared.insert(step.base.clone(), ready);
                }
            }
        }

        // What happens to each, from the states alone.
        let mut modules = Vec::new();
        let mut swaps = Vec::new();
        let mut removals = Vec::new();
        for step in steps {
            let previous_id = step.previous.as_ref().map(|(m, _)| m.module.id.clone());
            let previous_usable = step.previous.as_ref().is_some_and(|(_, s)| s.is_usable());
            let before = step.previous.as_ref().map(|(m, _)| m.as_ref());
            match step.plan {
                Plan::Skip(why) => modules.push(ReloadedModule {
                    local_id: step.local_id,
                    id: previous_id.clone().unwrap_or_else(|| step.base.clone()),
                    previous: previous_id,
                    outcome: ReloadOutcome::Skipped,
                    state: step.previous.map_or_else(
                        || ModuleState::Broken {
                            reason: why.clone(),
                        },
                        |(_, state)| state,
                    ),
                    error: Some(why),
                    node_types: NodeTypeChanges::default(),
                }),
                Plan::Remove => {
                    removals.push(step.base.clone());
                    modules.push(ReloadedModule {
                        local_id: step.local_id,
                        id: previous_id.clone().unwrap_or_else(|| step.base.clone()),
                        previous: previous_id,
                        outcome: ReloadOutcome::Removed,
                        state: ModuleState::Blocked {
                            reason: "no longer in regulus.toml; stopped".into(),
                        },
                        error: None,
                        node_types: node_type_changes(before, None),
                    });
                }
                Plan::Start(_) => {
                    let Some(ready) = prepared.remove(&step.base) else {
                        continue;
                    };
                    let state = ready.state();
                    let manifest = ready.manifest();
                    let (outcome, keep_old) = if !trusted {
                        (ReloadOutcome::Blocked, false)
                    } else if state.is_usable() {
                        let outcome = if previous_usable {
                            ReloadOutcome::Replaced
                        } else {
                            ReloadOutcome::Started
                        };
                        (outcome, false)
                    } else if previous_usable {
                        (ReloadOutcome::Kept, true)
                    } else {
                        (ReloadOutcome::Failed, false)
                    };
                    let error = (!state.is_usable() && trusted).then(|| reason(&state));
                    let (id, shown_state, after) = if keep_old {
                        let (old, old_state) = step
                            .previous
                            .clone()
                            .unwrap_or_else(|| (manifest.clone(), state.clone()));
                        (old.module.id.clone(), old_state, None)
                    } else {
                        (
                            manifest.module.id.clone(),
                            state.clone(),
                            Some(manifest.clone()),
                        )
                    };
                    let node_types = match &after {
                        Some(after) => node_type_changes(before, Some(after)),
                        None => NodeTypeChanges::default(),
                    };
                    if keep_old {
                        self.registry.discard(ready);
                    } else {
                        swaps.push(ready);
                    }
                    modules.push(ReloadedModule {
                        local_id: step.local_id,
                        id,
                        previous: previous_id,
                        outcome,
                        state: shown_state,
                        error,
                        node_types,
                    });
                }
            }
        }

        // Only now does anything that runs change: the executions asked to stop stop, and
        // the new modules take the place of the old ones while nothing new can start.
        let changing: BTreeSet<String> = swaps
            .iter()
            .map(|p| base_id(&p.manifest().module.id).to_string())
            .chain(removals.iter().cloned())
            .filter(|base| touched.contains(base))
            .collect();
        let mut stopped = Vec::new();
        let mut retired = Vec::new();
        let mut tries = 0;
        // Every exit of this loop leaves its guards behind: a refusal is only carried out
        // (the candidates stopped) after them, so a slow stop never holds a lock.
        let swapped: Result<(), RpcError> = loop {
            let busy = self.in_use(&changing);
            if !busy.is_empty() {
                if !params.stop_executions || tries >= SWAP_TRIES {
                    break Err(with_stopped(Self::in_use_error(busy), &stopped));
                }
                // Without any lock: stopping a node waits for its module.
                stopped.extend(self.stop_all(&busy));
                tries += 1;
                continue;
            }
            // Brief: check that nothing moved while the candidates started, then swap while
            // no execution can start (`execution.start` registers under `executions`).
            let _writing = self.writing();
            if let Err(why) = self.still_planned(&project, &planned) {
                break Err(with_stopped(
                    RpcError::new(code::MODULE_FAILED, why),
                    &stopped,
                ));
            }
            let _recovering = self.recovering();
            let executions = self.executions();
            let late = executions.iter().any(|live| {
                live.execution.state.is_live()
                    && live.workflow.nodes.iter().any(|n| {
                        n.module_id
                            .as_deref()
                            .is_some_and(|m| changing.contains(base_id(m)))
                    })
            });
            if late {
                drop(executions);
                tries += 1;
                if tries > SWAP_TRIES {
                    break Err(with_stopped(
                        RpcError::new(
                            code::MODULE_IN_USE,
                            "executions keep starting with this module; stop them and reload \
                             again",
                        ),
                        &stopped,
                    ));
                }
                continue;
            }
            for ready in swaps.drain(..) {
                retired.extend(self.registry.swap_local(ready));
            }
            for base in &removals {
                retired.extend(self.registry.take_local(&owner, base));
            }
            break Ok(());
        };
        if let Err(refused) = swapped {
            // Outside every lock: the candidates that did not take over are stopped.
            for ready in swaps {
                self.registry.discard(ready);
            }
            return Err(refused);
        }
        // Outside every lock: stopping a process waits for it.
        for old in retired {
            old.stop();
        }

        let reloaded: BTreeSet<String> = modules
            .iter()
            .map(|m| format!("{owner}/{}", m.local_id))
            .collect();
        Ok(ProjectReloaded {
            project: self.project_summary(&project),
            modules,
            stopped_executions: stopped,
            stale: self.stale_nodes(&reloaded),
        })
    }

    /// Whether `project` is still open in the same folder and each module of `planned` is
    /// still the one the reload planned against (a close, a reopen or a trust change since
    /// replaces them); why not, if not.
    fn still_planned(
        &self,
        project: &Project,
        planned: &[(String, Option<Arc<Manifest>>)],
    ) -> Result<(), String> {
        let open = self.projects().get(&project.id);
        if open.is_none_or(|open| open.root != project.root) {
            return Err(format!(
                "project `{}` was closed or reopened while it reloaded; nothing changed",
                project.id
            ));
        }
        for (base, before) in planned {
            let now = self.registry.local(&project.id, base).map(|(m, _)| m);
            let same = match (before, &now) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            };
            if !same {
                return Err(format!(
                    "`{base}` changed while it reloaded (a trust change or another start); \
                     nothing changed, reload again"
                ));
            }
        }
        Ok(())
    }

    /// What a reload of `module` (or of every local module) of `project` is to do.
    fn plan_reload(&self, project: &Project, module: Option<&str>) -> Result<Vec<Step>, RpcError> {
        let owner = &project.id;
        let listed: Vec<String> = self
            .registry
            .local_ids(owner)
            .into_iter()
            .filter_map(|base| Some(base.strip_prefix(&format!("{owner}/"))?.to_string()))
            .collect();
        let entries = &project.file.modules.local;
        let in_file = |id: &str| entries.iter().any(|e| e.id.as_deref() == Some(id));
        let targets: Vec<String> = match module {
            Some(module) => {
                let id = authoring::local_id(owner, module).to_string();
                if !in_file(&id) && !listed.contains(&id) {
                    return Err(invalid(format!(
                        "project `{owner}` has no [[modules.local]] with id `{id}`"
                    )));
                }
                vec![id]
            }
            None => {
                let mut ids: Vec<String> = Vec::new();
                for id in entries
                    .iter()
                    .filter_map(|e| e.id.clone())
                    .chain(listed.clone())
                {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                ids
            }
        };
        Ok(targets
            .into_iter()
            .map(|local_id| {
                let base = format!("{owner}/{local_id}");
                let previous = self.registry.local(owner, &base);
                let entry = entries
                    .iter()
                    .find(|e| e.id.as_deref() == Some(local_id.as_str()));
                let plan = match entry {
                    None if previous.is_some() => Plan::Remove,
                    None => Plan::Skip("not in regulus.toml".into()),
                    Some(entry) => match entry.unusable() {
                        Some(why) => Plan::Skip(format!("[[modules.local]] {why}")),
                        None if self.registry.excludes(&base) => {
                            Plan::Skip(format!("left out with --without {base}"))
                        }
                        None => Plan::Start(local_spec(project, local_id.clone(), entry)),
                    },
                };
                Step {
                    local_id,
                    base,
                    previous,
                    plan,
                }
            })
            .collect())
    }

    /// The nodes of loaded profiles, of the modules `bases`, whose ports now differ from
    /// their shape.
    fn stale_nodes(&self, bases: &BTreeSet<String>) -> Vec<NodeLocation> {
        let profiles: Vec<Profile> = self.profile_store().list(None).cloned().collect();
        let mut stale = Vec::new();
        for profile in profiles {
            let ours = |node_id: &str, workflow_id: &str| {
                profile
                    .workflow(workflow_id)
                    .and_then(|w| w.node(node_id))
                    .and_then(|n| n.module_id.as_deref())
                    .is_some_and(|m| bases.contains(base_id(m)))
            };
            for found in check_shapes(&profile, |id| self.registry.declared_manifest(id)) {
                let (Some(workflow_id), Some(node_id)) = (found.workflow_id, found.node_id) else {
                    continue;
                };
                if found.code == PORTS_STALE && ours(&node_id, &workflow_id) {
                    stale.push(NodeLocation {
                        profile_id: profile.id.clone(),
                        workflow_id,
                        node_id,
                    });
                }
            }
        }
        stale
    }
}

/// `error` with the executions a failed reload had already stopped as asked
/// (`data.stopped_executions`), so a client can say so.
fn with_stopped(error: RpcError, stopped: &[String]) -> RpcError {
    if stopped.is_empty() {
        return error;
    }
    let mut data = match error.data.clone() {
        Some(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    data.insert("stopped_executions".into(), serde_json::json!(stopped));
    error.with_data(Value::Object(data))
}

/// What the checker answered, into `check`: its diagnostics, and the node types it read
/// with the canonical shape validation of each.
fn read_answer(answer: &Value, check: &mut SourceCheck) {
    check.checked = true;
    check.interpreter = answer
        .get("interpreter")
        .and_then(Value::as_str)
        .map(str::to_string);
    check.complete = answer
        .get("complete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    for item in answer
        .get("diagnostics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match serde_json::from_value::<SourceDiagnostic>(item.clone()) {
            Ok(found) => check.diagnostics.push(found),
            Err(error) => check.diagnostics.push(diagnostic(
                Severity::Warning,
                "check.unavailable",
                format!("the checker said something this core cannot read: {error}"),
            )),
        }
    }
    for item in answer
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let line = item
            .get("line")
            .and_then(Value::as_u64)
            .and_then(|l| u32::try_from(l).ok());
        let Some(raw) = item.get("node") else {
            continue;
        };
        match serde_json::from_value::<NodeType>(raw.clone()) {
            Ok(node) => {
                for problem in shape::validate(&node) {
                    check.diagnostics.push(SourceDiagnostic {
                        line,
                        ..diagnostic(Severity::Error, "shape.invalid", problem)
                    });
                }
                check.nodes.push(node);
            }
            Err(error) => {
                check.complete = false;
                check.diagnostics.push(SourceDiagnostic {
                    line,
                    ..diagnostic(
                        Severity::Error,
                        "node.declaration",
                        format!("this node does not make a valid manifest: {error}"),
                    )
                });
            }
        }
    }
}

/// A warning if the SDK copy beside the source is not this core's.
fn sdk_note(file: &SourceFile, max: usize) -> Option<SourceDiagnostic> {
    let copy = file.absolute.parent()?.join(authoring::SDK_FILE);
    let meta = std::fs::symlink_metadata(&copy).ok()?;
    if !meta.file_type().is_file() || meta.len() > max as u64 {
        return None;
    }
    let text = std::fs::read_to_string(&copy).ok()?;
    if text == authoring::SDK {
        return None;
    }
    let theirs = authoring::sdk_version(&text).unwrap_or("an older version");
    let ours = authoring::sdk_version(authoring::SDK).unwrap_or("?");
    Some(diagnostic(
        Severity::Warning,
        "sdk.outdated",
        format!(
            "{} beside this file is SDK {theirs}, not this core's {ours}: what it does not \
             know fails when the module starts; copy the new one from a fresh `regulus node \
             init`",
            authoring::SDK_FILE
        ),
    ))
}
