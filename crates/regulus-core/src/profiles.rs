//! Loading profiles from files. A file holds either one profile or `{ "profiles": [...] }`.
//!
//! The file format of `regulus.toml` is still open (docs/plan.md §3); JSON and TOML both
//! map onto the same contract types, so choosing one later changes nothing here.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use regulus_proto::{Port, Profile};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(untagged)]
enum ProfileFile {
    Many { profiles: Vec<Profile> },
    One(Profile),
}

#[derive(Debug, Default)]
pub struct ProfileStore {
    profiles: Vec<Arc<Profile>>,
    /// The file each profile was read from or written to, by profile id.
    origins: HashMap<String, PathBuf>,
}

/// Fills what a file left out, so every client gets something to show: names, and the
/// ports of a box node that declares none, which are those of its workflow.
pub(crate) fn normalize(profile: &mut Profile) {
    fn name_ports(ports: &mut [Port]) {
        for port in ports {
            if port.name.is_empty() {
                port.name = port.id.clone();
            }
        }
    }
    if profile.name.is_empty() {
        profile.name = profile.id.clone();
    }
    for workflow in &mut profile.workflows {
        if workflow.name.is_empty() {
            workflow.name = workflow.id.clone();
        }
        name_ports(&mut workflow.ports);
        for node in &mut workflow.nodes {
            if node.name.is_empty() {
                node.name = node.id.clone();
            }
            name_ports(&mut node.ports);
        }
    }
    let ports: HashMap<String, Vec<Port>> = profile
        .workflows
        .iter()
        .map(|w| (w.id.clone(), w.ports.clone()))
        .collect();
    for node in profile.workflows.iter_mut().flat_map(|w| &mut w.nodes) {
        if let (true, true, Some(inner)) = (
            node.is_box(),
            node.ports.is_empty(),
            node.internal_workflow_id
                .as_ref()
                .and_then(|id| ports.get(id)),
        ) {
            node.ports = inner.clone();
        }
    }
}

/// What a saved file holds: the profile as a client sent it, minus what [`normalize`]
/// fills in on load. A box node keeps no copy of its workflow's ports, so it follows them
/// when they change; loading the file gives back the same profile.
pub(crate) fn denormalize(profile: &mut Profile) {
    let ports: HashMap<String, Vec<Port>> = profile
        .workflows
        .iter()
        .map(|w| (w.id.clone(), w.ports.clone()))
        .collect();
    for node in profile.workflows.iter_mut().flat_map(|w| &mut w.nodes) {
        let inner = node
            .internal_workflow_id
            .as_ref()
            .and_then(|id| ports.get(id));
        if node.is_box() && inner == Some(&node.ports) {
            node.ports.clear();
        }
    }
}

/// The first line of a file written by `profile.save`: the file is rewritten whole from the
/// profile, so whatever else was in it does not survive the next save.
pub(crate) const SAVED_HEADER: &str =
    "# Saved by Regulus (profile.save). Comments and layout are not kept on the next save.\n";

/// The TOML file of a profile, as `profile.save` writes it. TOML has no null, so a null
/// anywhere in a node's config is refused with where it is.
pub(crate) fn to_toml(profile: &Profile) -> Result<String, String> {
    fn null_at(value: &serde_json::Value, path: String) -> Option<String> {
        match value {
            serde_json::Value::Null => Some(path),
            serde_json::Value::Array(items) => items
                .iter()
                .enumerate()
                .find_map(|(i, item)| null_at(item, format!("{path}[{i}]"))),
            serde_json::Value::Object(map) => map
                .iter()
                .find_map(|(key, item)| null_at(item, format!("{path}.{key}"))),
            _ => None,
        }
    }
    for workflow in &profile.workflows {
        for node in &workflow.nodes {
            if let Some(at) = node
                .config
                .iter()
                .find_map(|(key, value)| null_at(value, key.clone()))
            {
                return Err(format!(
                    "node `{}` of workflow `{}` has null at config.{at}; TOML has no null: \
                     leave the key out instead",
                    node.id, workflow.id
                ));
            }
        }
    }
    let mut file = profile.clone();
    denormalize(&mut file);
    toml::to_string(&file)
        .map(|text| format!("{SAVED_HEADER}{text}"))
        .map_err(|e| e.to_string())
}

pub(crate) fn parse(path: &Path) -> Result<Vec<Profile>, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file: ProfileFile = match path.extension().and_then(|e| e.to_str()) {
        Some("toml") => toml::from_str(&text).map_err(|e| e.to_string()),
        _ => serde_json::from_str(&text).map_err(|e| e.to_string()),
    }
    .map_err(|e| format!("invalid profile file {}: {e}", path.display()))?;
    Ok(match file {
        ProfileFile::Many { profiles } => profiles,
        ProfileFile::One(profile) => vec![profile],
    })
}

fn files(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let entries = fs::read_dir(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|p| {
            p.is_file()
                && matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("json" | "toml")
                )
        })
        .collect();
    files.sort();
    Ok(files)
}

impl ProfileStore {
    /// Loads every profile found in `paths`. Problems are returned as warnings: a broken
    /// file never stops the core.
    pub fn load(paths: &[PathBuf]) -> (Self, Vec<String>) {
        let mut store = ProfileStore::default();
        let warnings = store.add_from(paths, None);
        (store, warnings)
    }

    /// Adds the profiles of an open project (docs/11-composition.md §13): every one found in
    /// `paths` gets `project_id`; a file that says another one is told so, and the project
    /// wins. A path that does not exist holds none.
    pub fn load_project(&mut self, project_id: &str, paths: &[PathBuf]) -> Vec<String> {
        let existing: Vec<PathBuf> = paths.iter().filter(|p| p.exists()).cloned().collect();
        self.add_from(&existing, Some(project_id))
    }

    /// Unloads the profiles a project's `paths` gave, and returns their ids. Executions
    /// already running keep the copy they started with.
    pub fn remove_project(&mut self, project_id: &str, paths: &[PathBuf]) -> Vec<String> {
        let from_project = |origin: &Path| {
            paths
                .iter()
                .any(|p| origin == p || origin.parent() == Some(p.as_path()))
        };
        let ids: Vec<String> = self
            .profiles
            .iter()
            .filter(|p| p.project_id.as_deref() == Some(project_id))
            .filter(|p| self.origin(&p.id).is_some_and(from_project))
            .map(|p| p.id.clone())
            .collect();
        for id in &ids {
            self.remove(id);
        }
        ids
    }

    fn add_from(&mut self, paths: &[PathBuf], project_id: Option<&str>) -> Vec<String> {
        let mut warnings = Vec::new();
        for path in paths {
            let found = files(path).and_then(|files| {
                files
                    .into_iter()
                    .map(|file| parse(&file).map(|all| (file, all)))
                    .collect::<Result<Vec<_>, _>>()
            });
            match found {
                Ok(found) => {
                    for (file, profiles) in found {
                        for mut profile in profiles {
                            if let Some(project) = project_id {
                                match profile.project_id.as_deref() {
                                    Some(other) if other != project => warnings.push(format!(
                                        "profile `{}` in {} says project_id `{other}`; it \
                                         belongs to project `{project}`, which wins",
                                        profile.id,
                                        file.display()
                                    )),
                                    _ => {}
                                }
                                profile.project_id = Some(project.to_string());
                            }
                            let id = profile.id.clone();
                            match self.insert(profile) {
                                Ok(()) => {
                                    self.origins.insert(id, file.to_path_buf());
                                }
                                Err(warning) => warnings.push(warning),
                            }
                        }
                    }
                }
                Err(warning) => warnings.push(warning),
            }
        }
        warnings
    }

    pub fn insert(&mut self, mut profile: Profile) -> Result<(), String> {
        if self.get(&profile.id).is_some() {
            return Err(format!(
                "profile `{}` is defined more than once; keeping the first",
                profile.id
            ));
        }
        normalize(&mut profile);
        self.profiles.push(Arc::new(profile));
        Ok(())
    }

    /// Puts `profile`, read from or written to `origin`, in place of the one with its id,
    /// or adds it. Executions already running keep the copy they started with.
    pub fn replace(&mut self, mut profile: Profile, origin: PathBuf) {
        normalize(&mut profile);
        self.origins.insert(profile.id.clone(), origin);
        let profile = Arc::new(profile);
        match self.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => *slot = profile,
            None => self.profiles.push(profile),
        }
    }

    /// Takes a profile out, with the file it came from. Executions already running keep
    /// the copy they started with.
    pub fn remove(&mut self, id: &str) -> Option<(Arc<Profile>, Option<PathBuf>)> {
        let position = self.profiles.iter().position(|p| p.id == id)?;
        let profile = self.profiles.remove(position);
        Some((profile, self.origins.remove(id)))
    }

    pub fn get(&self, id: &str) -> Option<Arc<Profile>> {
        self.profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
    }

    /// The file a profile was read from or written to; `None` for one inserted in memory
    /// or unknown.
    pub fn origin(&self, id: &str) -> Option<&Path> {
        self.origins.get(id).map(PathBuf::as_path)
    }

    /// Profiles of one project, or of every project when `project_id` is `None`.
    pub fn list<'a>(
        &'a self,
        project_id: Option<&'a str>,
    ) -> impl Iterator<Item = &'a Profile> + 'a {
        self.profiles.iter().map(Arc::as_ref).filter(move |p| {
            project_id.is_none_or(|wanted| p.project_id.as_deref() == Some(wanted))
        })
    }
}
