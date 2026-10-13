//! Pools of pre-populated spare worktrees for large projects.
//!
//! A spare is an ordinary detached linked worktree kept under
//! `.repo/repo-worktrees/.pool/<project>/`. `add` moves a spare into the new
//! tree with `git worktree move` and checks out the pinned commit, which only
//! rewrites files that differ. `remove` returns clean projects to the pool
//! instead of deleting them.

use crate::bridge;
use crate::cli::PoolCommand;
use crate::defaults;
use crate::discovery::discover_client;
use crate::error::{AppError, AppResult};
use crate::registry::{acquire, load_all, with_registry_lock, Held, LockMode};
use crate::resolve::resolve_projects;
use crate::util::{default_jobs, exit_reason};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const STRATEGY: &str = "pool";
pub const DIR_NAME: &str = ".pool";
const FILLING_PREFIX: &str = ".filling-";

fn project_dir(registry: &Path, project_path: &str) -> PathBuf {
    registry
        .join(DIR_NAME)
        .join(project_path.replace('%', "%25").replace('/', "%2F"))
}

fn slots(registry: &Path, project_path: &str) -> Vec<PathBuf> {
    let mut slots = fs::read_dir(project_dir(registry, project_path))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                .map(|entry| entry.path())
                .filter(|path| path.join(".git").is_file())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    slots.sort();
    slots
}

fn unique_name() -> String {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!(
        "{nanos}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn same_device(left: &Path, right: &Path) -> bool {
    match (fs::metadata(left), fs::metadata(right)) {
        (Ok(left), Ok(right)) => left.dev() == right.dev(),
        _ => false,
    }
}

fn git_worktree_move(git_common_dir: &str, from: &Path, to: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .arg(format!("--git-dir={git_common_dir}"))
        .args(["worktree", "move"])
        .arg(from)
        .arg(to)
        .output()
        .map_err(|error| format!("run git worktree move: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(exit_reason(&output))
    }
}

pub const DEFAULT_SPARES: usize = 2;

/// Spares each pooled project keeps, from `pool size` or the default.
pub fn capacity(registry: &Path) -> usize {
    defaults::load(registry)
        .ok()
        .and_then(|defaults| defaults.pool_spares)
        .unwrap_or(DEFAULT_SPARES)
}

/// Serializes spare claims and returns for one project across processes.
fn lock_project(registry: &Path, project_path: &str) -> Result<Held, String> {
    let directory = registry.join(DIR_NAME);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let path = project_dir(registry, project_path).with_extension("lock");
    acquire(&path, &format!("the {project_path} pool"), true).map_err(|error| error.message)
}

fn branch_exists(git_common_dir: &str, branch: &str) -> bool {
    Command::new("git")
        .arg(format!("--git-dir={git_common_dir}"))
        .args([
            "rev-parse",
            "-q",
            "--verify",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Moves a spare into `checkout` and checks out `sha`, creating `branch` there
/// when given. Returns `Ok(false)` when no usable spare exists, leaving the
/// caller to populate normally.
pub fn take(
    registry: &Path,
    project_path: &str,
    git_common_dir: &str,
    checkout: &Path,
    sha: &str,
    branch: Option<&str>,
) -> Result<bool, String> {
    if branch.is_some_and(|branch| branch_exists(git_common_dir, branch)) {
        return Err("branch-exists".into());
    }
    let Some(parent) = checkout.parent() else {
        return Ok(false);
    };
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    {
        let _held = lock_project(registry, project_path)?;
        let Some(spare) = slots(registry, project_path)
            .into_iter()
            .find(|spare| same_device(spare, parent))
        else {
            return Ok(false);
        };
        if git_worktree_move(git_common_dir, &spare, checkout).is_err() {
            return Ok(false);
        }
    }
    let mut command = Command::new("git");
    command.arg("-C").arg(checkout).args([
        "-c",
        "checkout.workers=8",
        "-c",
        "checkout.thresholdForParallelism=100",
        "checkout",
        "-q",
    ]);
    match branch {
        Some(branch) => command.arg("-b").arg(branch),
        None => command.arg("--detach"),
    };
    let output = command
        .arg(sha)
        .output()
        .map_err(|error| format!("run git checkout: {error}"))?;
    if output.status.success() {
        Ok(true)
    } else {
        Err(format!("pool checkout: {}", exit_reason(&output)))
    }
}

/// Returns a clean project checkout to its pool when the pool has room.
pub fn give_back(
    registry: &Path,
    project_path: &str,
    git_common_dir: &str,
    checkout: &Path,
    capacity: usize,
) -> Result<bool, String> {
    let _held = lock_project(registry, project_path)?;
    if slots(registry, project_path).len() >= capacity {
        return Ok(false);
    }
    let directory = project_dir(registry, project_path);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    if !same_device(&directory, checkout) {
        return Ok(false);
    }
    let dot_git = checkout.join(".git");
    let link = bridge::to_gitfile(&dot_git)?;
    let _ = Command::new("git")
        .arg(format!("--git-dir={git_common_dir}"))
        .args(["worktree", "unlock"])
        .arg(checkout)
        .output();
    let slot = directory.join(unique_name());
    if let Err(error) = git_worktree_move(git_common_dir, checkout, &slot) {
        bridge::restore_link(&dot_git, link)?;
        return Err(error);
    }
    for args in [
        &["checkout", "-q", "--detach"][..],
        &["clean", "-ffdxq"][..],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(&slot)
            .args(args)
            .output()
            .map_err(|error| format!("run git {}: {error}", args[0]))?;
        if !output.status.success() {
            let _ = remove_slot(git_common_dir, &slot);
            return Err(format!("reset pooled worktree: {}", exit_reason(&output)));
        }
    }
    Ok(true)
}

fn remove_slot(git_common_dir: &str, slot: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .arg(format!("--git-dir={git_common_dir}"))
        .args(["worktree", "remove", "--force", "--force"])
        .arg(slot)
        .output()
        .map_err(|error| format!("run git worktree remove: {error}"))?;
    if slot.exists() {
        fs::remove_dir_all(slot).map_err(|error| format!("remove {}: {error}", slot.display()))?;
    }
    if !output.status.success() {
        let _ = Command::new("git")
            .arg(format!("--git-dir={git_common_dir}"))
            .args(["worktree", "prune"])
            .output();
    }
    Ok(())
}

fn common_dir(source_root: &Path, project_path: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source_root.join(project_path))
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .map_err(|error| format!("inspect {project_path}: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!("inspect {project_path}: {}", exit_reason(&output)))
    }
}

fn pooled_projects(registry: &Path) -> Vec<String> {
    let mut projects = fs::read_dir(registry.join(DIR_NAME))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_dir())
                .map(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .replace("%2F", "/")
                        .replace("%25", "%")
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    projects.sort();
    projects
}

fn slot_head(slot: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(slot)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn active_projects(source_root: &Path) -> AppResult<Vec<String>> {
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

fn select(
    source_root: &Path,
    cwd: &Path,
    inputs: &[String],
    fallback: Vec<String>,
) -> AppResult<Vec<String>> {
    if inputs.is_empty() {
        return Ok(fallback);
    }
    let known = active_projects(source_root)?;
    resolve_projects(inputs, &known, &[source_root.to_path_buf()], cwd)
        .map(|resolved| {
            resolved
                .into_iter()
                .map(|resolution| resolution.project)
                .collect()
        })
        .map_err(|unknown| {
            AppError::operational(
                "E_UNKNOWN_PROJECT",
                format!("unknown active project path(s): {}", unknown.join(", ")),
            )
        })
}

pub fn run(command: &PoolCommand, cwd: &Path, json_output: bool, quiet: bool) -> AppResult<Value> {
    let client = discover_client(cwd, true)?;
    let source = client.root.clone();
    match command {
        PoolCommand::List => with_registry_lock(&source, LockMode::Shared, false, |registry| {
            let spares = pooled_projects(registry)
                .into_iter()
                .flat_map(|project| {
                    slots(registry, &project)
                        .into_iter()
                        .map(move |slot| (project.clone(), slot))
                })
                .map(|(project, slot)| {
                    json!({"project": project, "path": slot, "head": slot_head(&slot)})
                })
                .collect::<Vec<_>>();
            if !json_output {
                for spare in &spares {
                    println!(
                        "{}  {}  {}",
                        spare["project"].as_str().unwrap_or(""),
                        spare["head"].as_str().unwrap_or("-"),
                        spare["path"].as_str().unwrap_or("")
                    );
                }
                if spares.is_empty() {
                    println!("no spare worktrees");
                }
            }
            Ok(json!({"spares": spares}))
        }),
        PoolCommand::Size(args) => {
            let registry = crate::registry::registry_path(&source);
            if let Some(spares) = args.spares {
                with_registry_lock(&source, LockMode::Exclusive, true, |registry| {
                    let mut defaults = defaults::load(registry)?;
                    defaults.pool_spares = Some(spares);
                    let bytes = serde_json::to_vec_pretty(&defaults)
                        .map_err(|error| AppError::io("serialize defaults.json", error))?;
                    crate::util::write_atomic(&defaults::path(registry), &bytes, 0o644)
                })?;
            }
            let spares = capacity(&registry);
            if !json_output {
                println!("{spares} spare(s) per pooled project");
            }
            Ok(json!({"spares": spares, "default": DEFAULT_SPARES}))
        }
        PoolCommand::Fill(args) => {
            let registry = crate::registry::registry_path(&source);
            let mut pooled = defaults::load(&registry)?
                .projects
                .into_iter()
                .filter(|(_, mode)| mode == "pool")
                .map(|(project, _)| project)
                .collect::<Vec<_>>();
            pooled.extend(pooled_projects(&registry));
            for tree in load_all(&registry)? {
                pooled.extend(
                    tree.projects
                        .iter()
                        .filter(|project| {
                            project.mode == crate::model::MODE_WORKTREE && tree.uses_pool(project)
                        })
                        .map(|project| project.path.clone()),
                );
            }
            pooled.sort();
            pooled.dedup();
            let projects = select(&source, cwd, &args.projects, pooled)?;
            if projects.is_empty() {
                return Err(AppError::operational(
                    "E_NO_POOL_PROJECTS",
                    "no pooled projects yet: create a tree first, run `defaults pool <project>`, or name projects",
                ));
            }
            let count = args.count.unwrap_or_else(|| capacity(&registry));
            let mut work = Vec::new();
            for project in &projects {
                let existing = slots(&registry, project).len();
                for _ in existing..count {
                    work.push(project.clone());
                }
            }
            let pool = ThreadPoolBuilder::new()
                .num_threads(args.jobs.unwrap_or_else(default_jobs).min(4))
                .build()
                .map_err(|error| AppError::io("create worker pool", error))?;
            let created = pool.install(|| {
                work.par_iter()
                    .map(|project| {
                        let started = std::time::Instant::now();
                        let result = fill_one(&source, &registry, project);
                        crate::metrics::push(
                            "spares",
                            json!({
                                "project": project,
                                "ms": crate::metrics::elapsed_ms(started),
                                "ok": result.is_ok(),
                            }),
                        );
                        result
                    })
                    .collect::<Vec<_>>()
            });
            let mut filled = Vec::new();
            let mut failed = Vec::new();
            for (project, result) in work.iter().zip(created) {
                match result {
                    Ok(slot) => filled.push(json!({"project": project, "path": slot})),
                    Err(reason) => {
                        if !quiet {
                            eprintln!("warning: pool fill {project} failed: {reason}");
                        }
                        failed.push(json!({"project": project, "reason": reason}));
                    }
                }
            }
            if !quiet {
                eprintln!("created {} spare worktree(s)", filled.len());
            }
            let result = json!({"filled": filled, "failed": failed});
            if failed.is_empty() {
                Ok(result)
            } else {
                Err(AppError::partial(result))
            }
        }
        PoolCommand::Clear(args) => {
            with_registry_lock(&source, LockMode::Shared, false, |registry| {
                let projects = select(&source, cwd, &args.projects, pooled_projects(registry))?;
                let mut cleared = Vec::new();
                for project in projects {
                    let _held = lock_project(registry, &project)
                        .map_err(|reason| AppError::locked("E_LOCKED", reason))?;
                    let directory = project_dir(registry, &project);
                    let Ok(common) = common_dir(&source, &project) else {
                        let _ = fs::remove_dir_all(&directory);
                        continue;
                    };
                    let entries = fs::read_dir(&directory)
                        .map(|entries| {
                            entries
                                .filter_map(Result::ok)
                                .map(|entry| entry.path())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    for slot in entries {
                        remove_slot(&common, &slot)
                            .map_err(|reason| AppError::operational("E_IO", reason))?;
                        cleared.push(json!({"project": project, "path": slot}));
                    }
                    let _ = fs::remove_dir(&directory);
                }
                if !quiet {
                    eprintln!("removed {} spare worktree(s)", cleared.len());
                }
                Ok(json!({"cleared": cleared}))
            })
        }
    }
}

/// Builds a spare outside the registry lock, then publishes it under the lock.
fn fill_one(source: &Path, registry: &Path, project: &str) -> Result<PathBuf, String> {
    let common = common_dir(source, project)?;
    let directory = project_dir(registry, project);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let name = unique_name();
    let staging = directory.join(format!("{FILLING_PREFIX}{name}"));
    let output = Command::new("git")
        .arg("-C")
        .arg(source.join(project))
        .args([
            "-c",
            "checkout.workers=8",
            "-c",
            "checkout.thresholdForParallelism=100",
            "worktree",
            "add",
            "--detach",
        ])
        .arg(&staging)
        .arg("HEAD")
        .output()
        .map_err(|error| format!("run git worktree add: {error}"))?;
    if !output.status.success() {
        let _ = remove_slot(&common, &staging);
        return Err(exit_reason(&output));
    }
    let slot = directory.join(&name);
    let published = lock_project(registry, project)
        .and_then(|_held| git_worktree_move(&common, &staging, &slot));
    match published {
        Ok(()) => Ok(slot),
        Err(error) => {
            let _ = remove_slot(&common, &staging);
            Err(error)
        }
    }
}
