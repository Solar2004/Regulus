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
                        for profile in profiles {
                            let id = profile.id.clone();
                            match store.insert(profile) {
                                Ok(()) => {
                                    store.origins.insert(id, file.to_path_buf());
                                }
                                Err(warning) => warnings.push(warning),
                            }
                        }
                    }
                }
                Err(warning) => warnings.push(warning),
            }
        }
        (store, warnings)
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
