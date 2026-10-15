use crate::cli::{DetachArgs, StatusArgs};
use crate::discovery::discover_client;
use crate::error::{AppError, AppResult};
use crate::inspect::{checkout_branch, inspect_tree};
use crate::model::{TreeMeta, MODE_SHARED};
use crate::registry::{
    containing_tree, load_all, select_tree, select_tree_or_current, with_registry_lock, LockMode,
};
use crate::resolve::resolve_projects;
use crate::util::{default_jobs, exit_reason};
use rayon::prelude::*;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn status(args: &StatusArgs, cwd: &Path, json_output: bool, verbose: u8) -> AppResult<Value> {
    let client = discover_client(cwd, true)?;
    with_registry_lock(&client.root, LockMode::Shared, false, |registry| {
        let meta = select_tree_or_current(registry, args.tree.as_deref(), cwd)?;
        let projects = inspect_tree(&meta, args.jobs.unwrap_or_else(default_jobs));
        crate::tree::record_slow_inspections(&projects);
        let shared = meta
            .projects
            .iter()
            .filter(|project| project.mode == MODE_SHARED)
            .map(|project| project.path.clone())
            .collect::<Vec<_>>();
        let dirty = projects.iter().any(|project| project.dirty);
        if !json_output {
            for project in &projects {
                if !project.dirty
                    && project.branch.is_none()
                    && project.warning.is_none()
                    && verbose == 0
                {
                    continue;
                }
                let position = match (&project.branch, &project.head) {
                    (Some(branch), _) => format!("on {branch}"),
                    (None, Some(head)) if head == &project.pinned => "detached at pin".into(),
                    (None, Some(head)) => format!("detached at {}", &head[..head.len().min(12)]),
                    (None, None) => "unknown HEAD".into(),
                };
                let mut line = format!("{}  {position}", project.path);
                if project.dirty {
                    line.push_str(&format!("  [{}]", project.reasons.join(", ")));
                }
                if let Some(error) = project.error.as_ref().or(project.warning.as_ref()) {
                    line.push_str(&format!("  ({error})"));
                }
                println!("{line}");
            }
            println!(
                "{}: {} project(s), {} dirty, {} shared",
                meta.name,
                projects.len(),
                projects.iter().filter(|project| project.dirty).count(),
                shared.len()
            );
        }
        Ok(json!({
            "name": meta.name,
            "path": meta.path,
            "state": meta.state,
            "dirty": dirty,
            "repo_bridge": meta.repo_bridge,
            "projects": projects,
            "shared": shared,
        }))
    })
}

/// Branches currently checked out by a tree's own worktrees.
pub fn checked_out_branches(meta: &TreeMeta) -> Vec<Value> {
    let root = meta.root();
    meta.projects
        .iter()
        .filter(|project| project.has_worktree() && project.state != "skipped")
        .filter_map(|project| {
            checkout_branch(&root.join(&project.path))
                .map(|branch| json!({"project": project.path, "branch": branch}))
        })
        .collect()
}

struct Location {
    label: String,
    root: PathBuf,
    projects: Vec<String>,
}

fn source_projects(source_root: &Path) -> AppResult<Vec<String>> {
    let path = source_root.join(".repo/project.list");
    let contents = fs::read_to_string(&path)
        .map_err(|error| AppError::io(format!("read {}", path.display()), error))?;
    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

fn tree_location(meta: &TreeMeta) -> Location {
    Location {
        label: meta.name.clone(),
        root: meta.root(),
        projects: meta
            .projects
            .iter()
            .filter(|project| {
                project.has_worktree()
                    && project.state != "skipped"
                    && meta.root().join(&project.path).exists()
            })
            .map(|project| project.path.clone())
            .collect(),
    }
}

pub fn detach(args: &DetachArgs, cwd: &Path, json_output: bool, quiet: bool) -> AppResult<Value> {
    let client = discover_client(cwd, true)?;
    let source_root = client.root.clone();
    with_registry_lock(&client.root, LockMode::Shared, false, |registry| {
        let source = || -> AppResult<Location> {
            Ok(Location {
                label: "source".into(),
                root: source_root.clone(),
                projects: source_projects(&source_root)?,
            })
        };
        let locations = if args.source {
            vec![source()?]
        } else if let Some(selector) = &args.tree {
            vec![tree_location(&select_tree(registry, selector, cwd)?)]
        } else if args.branch.is_some() {
            let mut all = vec![source()?];
            all.extend(load_all(registry)?.iter().map(tree_location));
            all
        } else if let Some(tree) = containing_tree(registry, cwd)? {
            vec![tree_location(&tree)]
        } else {
            return Err(AppError::usage(
                "name a tree, pass --source, or pass --branch to search every tree",
            ));
        };

        let filter = if args.projects.is_empty() {
            None
        } else {
            let known = source_projects(&source_root)?;
            let mut roots = vec![source_root.clone()];
            roots.extend(locations.iter().map(|location| location.root.clone()));
            let resolved =
                resolve_projects(&args.projects, &known, &roots, cwd).map_err(|unknown| {
                    AppError::operational(
                        "E_UNKNOWN_PROJECT",
                        format!("unknown project path(s): {}", unknown.join(", ")),
                    )
                })?;
            Some(
                resolved
                    .into_iter()
                    .map(|resolution| resolution.project)
                    .collect::<Vec<_>>(),
            )
        };

        let candidates = locations
            .iter()
            .flat_map(|location| {
                location
                    .projects
                    .iter()
                    .filter(|project| {
                        filter
                            .as_ref()
                            .map_or(true, |filter| filter.contains(project))
                    })
                    .map(move |project| (location, project))
            })
            .collect::<Vec<_>>();
        let held = candidates
            .par_iter()
            .filter_map(|(location, project)| {
                let checkout = location.root.join(project);
                let branch = checkout_branch(&checkout)?;
                if args.branch.as_ref().is_some_and(|wanted| wanted != &branch) {
                    return None;
                }
                Some((location.label.clone(), (*project).clone(), branch, checkout))
            })
            .collect::<Vec<_>>();

        let mut detached = Vec::new();
        let mut failed = Vec::new();
        for (location, project, branch, checkout) in held {
            let record = json!({
                "location": location,
                "project": project,
                "branch": branch,
                "path": checkout,
            });
            if args.dry_run {
                detached.push(record);
                continue;
            }
            let output = Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(["checkout", "-q", "--detach"])
                .output();
            match output {
                Ok(output) if output.status.success() => detached.push(record),
                Ok(output) => failed.push(json!({
                    "location": location,
                    "project": project,
                    "branch": branch,
                    "reason": exit_reason(&output),
                })),
                Err(error) => failed.push(json!({
                    "location": location,
                    "project": project,
                    "branch": branch,
                    "reason": error.to_string(),
                })),
            }
        }
        if !json_output && !quiet {
            let verb = if args.dry_run {
                "would detach"
            } else {
                "detached"
            };
            for record in &detached {
                eprintln!(
                    "{verb} {} in {} ({})",
                    record["branch"].as_str().unwrap_or(""),
                    record["project"].as_str().unwrap_or(""),
                    record["location"].as_str().unwrap_or("")
                );
            }
            for record in &failed {
                eprintln!(
                    "warning: could not detach {} in {}: {}",
                    record["branch"].as_str().unwrap_or(""),
                    record["project"].as_str().unwrap_or(""),
                    record["reason"].as_str().unwrap_or("")
                );
            }
            if detached.is_empty() && failed.is_empty() {
                match &args.branch {
                    Some(branch) => {
                        eprintln!("{branch} is not checked out in any selected worktree")
                    }
                    None => eprintln!("no selected project has a branch checked out"),
                }
            }
        }
        crate::metrics::record("detached", json!(detached.len()));
        crate::metrics::record("failed", json!(failed.len()));
        let result = json!({"detached": detached, "failed": failed, "dry_run": args.dry_run});
        if failed.is_empty() {
            Ok(result)
        } else {
            Err(AppError::partial(result))
        }
    })
}
