use crate::cli::{DefaultProjectsArgs, DefaultsCommand};
use crate::discovery::discover_client;
use crate::error::{AppError, AppResult};
use crate::registry::{with_registry_lock, LockMode};
use crate::util::{path_is_safe_relative, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULTS_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectDefaults {
    pub format_version: u32,
    pub projects: BTreeMap<String, String>,
}

impl Default for ProjectDefaults {
    fn default() -> Self {
        Self {
            format_version: DEFAULTS_FORMAT_VERSION,
            projects: BTreeMap::new(),
        }
    }
}

pub fn path(registry: &Path) -> PathBuf {
    registry.join("defaults.json")
}

pub fn load(registry: &Path) -> AppResult<ProjectDefaults> {
    let defaults_path = path(registry);
    let bytes = match fs::read(&defaults_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProjectDefaults::default());
        }
        Err(error) => {
            return Err(AppError::io(
                format!("read {}", defaults_path.display()),
                error,
            ));
        }
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::io(format!("parse {}", defaults_path.display()), error))?;
    if value
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        > DEFAULTS_FORMAT_VERSION as u64
    {
        return Err(AppError::operational(
            "E_DEFAULTS_FORMAT_TOO_NEW",
            format!(
                "{} uses a newer defaults format; upgrade git-repo-worktree",
                defaults_path.display()
            ),
        ));
    }
    let defaults: ProjectDefaults = serde_json::from_value(value)
        .map_err(|error| AppError::io(format!("parse {}", defaults_path.display()), error))?;
    if let Some((project, mode)) = defaults
        .projects
        .iter()
        .find(|(_, mode)| !matches!(mode.as_str(), "symlink" | "worktree"))
    {
        return Err(AppError::operational(
            "E_DEFAULTS_INVALID",
            format!("invalid default mode {mode:?} for project {project}"),
        ));
    }
    Ok(defaults)
}

pub fn run(
    command: &DefaultsCommand,
    cwd: &Path,
    json_output: bool,
    quiet: bool,
) -> AppResult<Value> {
    let client = discover_client(cwd, true)?;
    match command {
        DefaultsCommand::List => {
            with_registry_lock(&client.root, LockMode::Shared, false, |registry| {
                let defaults = load(registry)?;
                emit(&defaults, registry, json_output);
                Ok(result(&defaults, registry))
            })
        }
        DefaultsCommand::Symlink(args) => {
            mutate(&client.root, args, Some("symlink"), json_output, quiet)
        }
        DefaultsCommand::Worktree(args) => {
            mutate(&client.root, args, Some("worktree"), json_output, quiet)
        }
        DefaultsCommand::Auto(args) => mutate(&client.root, args, None, json_output, quiet),
    }
}

fn mutate(
    source_root: &Path,
    args: &DefaultProjectsArgs,
    mode: Option<&str>,
    json_output: bool,
    quiet: bool,
) -> AppResult<Value> {
    let projects = dedupe_and_validate_paths(&args.projects)?;
    let active = if mode.is_some() {
        Some(load_active_projects(source_root)?)
    } else {
        None
    };
    if let Some(active) = &active {
        let unknown = projects
            .iter()
            .filter(|project| !active.contains(project.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Err(AppError::operational(
                "E_UNKNOWN_PROJECT",
                format!("unknown active project path(s): {}", unknown.join(", ")),
            ));
        }
    }

    with_registry_lock(source_root, LockMode::Exclusive, true, |registry| {
        let mut defaults = load(registry)?;
        for project in &projects {
            match mode {
                Some(mode) => {
                    defaults.projects.insert(project.clone(), mode.into());
                }
                None => {
                    defaults.projects.remove(project);
                }
            }
        }
        let bytes = serde_json::to_vec_pretty(&defaults)
            .map_err(|error| AppError::io("serialize defaults.json", error))?;
        write_atomic(&path(registry), &bytes, 0o644)?;
        if !quiet {
            match mode {
                Some(mode) => {
                    eprintln!("set {mode} default for {} project(s)", projects.len());
                }
                None => {
                    eprintln!("cleared default for {} project(s)", projects.len());
                }
            }
        }
        if !json_output {
            emit(&defaults, registry, false);
        }
        Ok(result(&defaults, registry))
    })
}

fn dedupe_and_validate_paths(projects: &[String]) -> AppResult<Vec<String>> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for project in projects {
        if !path_is_safe_relative(Path::new(project)) {
            return Err(AppError::operational(
                "E_BAD_PROJECT_PATH",
                format!("unsafe project path: {project}"),
            ));
        }
        if seen.insert(project.clone()) {
            output.push(project.clone());
        }
    }
    output.sort();
    Ok(output)
}

fn load_active_projects(source_root: &Path) -> AppResult<HashSet<String>> {
    let project_list = source_root.join(".repo/project.list");
    let contents = fs::read_to_string(&project_list)
        .map_err(|error| AppError::io(format!("read {}", project_list.display()), error))?;
    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

fn emit(defaults: &ProjectDefaults, registry: &Path, json_output: bool) {
    if json_output {
        return;
    }
    for (project, mode) in &defaults.projects {
        println!("{mode} {project}");
    }
    if defaults.projects.is_empty() {
        println!(
            "no project defaults configured ({})",
            path(registry).display()
        );
    }
}

fn result(defaults: &ProjectDefaults, registry: &Path) -> Value {
    json!({
        "path": path(registry),
        "format_version": defaults.format_version,
        "projects": defaults.projects,
    })
}
