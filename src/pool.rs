//! Pools of pre-populated spare worktrees for large projects.
//!
//! A spare is an ordinary detached linked worktree kept under
//! `.repo/repo-worktrees/.pool/<project>/`. `add` moves a spare into the new
//! tree with `git worktree move` and checks out the pinned commit, which only
//! rewrites files that differ. `remove` returns clean projects to the pool
//! instead of deleting them. After either, a detached low-priority
//! `pool maintain` deletes spares no default `add` would use and refills the
//! rest up to the pool size.

use crate::bridge;
use crate::cli::PoolCommand;
use crate::defaults;
use crate::discovery::discover_client;
use crate::error::{AppError, AppResult};
use crate::model::{
    estimated_checkout_ms, ProjectMeta, TreeMeta, MODE_WORKTREE, POOL_MIN_ESTIMATED_MS,
};
use crate::registry::{acquire, load_all, registry_path, try_acquire, Held};
use crate::resolve::resolve_projects;
use crate::util::{default_jobs, exit_reason, write_atomic};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const STRATEGY: &str = "pool";
pub const DIR_NAME: &str = ".pool";
const FILLING_PREFIX: &str = ".filling-";
const MAINTAIN_LOCK: &str = ".maintain.lock";
/// Written by every maintenance run before it tries the lock, and removed by
/// the run that holds it when it starts a pass. The holder checks for it after
/// releasing the lock and runs again if a later request arrived meanwhile.
const MAINTAIN_AGAIN: &str = ".maintain.again";
/// Disables the background `pool maintain` that follows `add` and `remove`.
pub const NO_MAINTENANCE_ENV: &str = "GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE";
/// Maintenance re-plans after each round so returns and takes made while it
/// runs are seen; this bounds how long one run keeps going.
const MAINTENANCE_ROUNDS: usize = 5;
/// Background refills leave this share of the filesystem free, capped at
/// [`RESERVED_FREE_MAX_BYTES`] so large disks are not held back.
const RESERVED_FREE_PERCENT: u64 = 10;
const RESERVED_FREE_MAX_BYTES: u64 = 50_000_000_000;

fn project_dir(registry: &Path, project_path: &str) -> PathBuf {
    registry
        .join(DIR_NAME)
        .join(project_path.replace('%', "%25").replace('/', "%2F"))
}

fn facts_path(registry: &Path, project_path: &str) -> PathBuf {
    let mut name = project_dir(registry, project_path).into_os_string();
    name.push(".json");
    PathBuf::from(name)
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

/// What decides whether a project's spares serve future default adds,
/// recorded beside its pool so the decision needs no registered tree.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectFacts {
    pub est_files: u64,
    pub est_bytes: u64,
    /// The project is a fixed-depth clone, which a default `add` shares or
    /// overlays from a clean source checkout instead of pooling.
    pub shallow: bool,
}

impl ProjectFacts {
    pub fn from_meta(project: &ProjectMeta) -> Option<Self> {
        Some(Self {
            est_files: project.est_files?,
            est_bytes: project.est_bytes.unwrap_or(0),
            shallow: project.clone_depth.is_some(),
        })
    }

    /// Approximate disk one checkout occupies: content plus a block per file.
    pub fn disk_bytes(&self) -> u64 {
        self.est_bytes
            .saturating_add(self.est_files.saturating_mul(4096))
    }
}

fn read_facts(registry: &Path, project_path: &str) -> Option<ProjectFacts> {
    serde_json::from_slice(&fs::read(facts_path(registry, project_path)).ok()?).ok()
}

fn record_facts(registry: &Path, project_path: &str, facts: &ProjectFacts) {
    if let Ok(bytes) = serde_json::to_vec(facts) {
        let _ = write_atomic(&facts_path(registry, project_path), &bytes, 0o644);
    }
}

pub fn measure_facts(source: &Path, project_path: &str) -> Option<ProjectFacts> {
    let checkout = source.join(project_path);
    let (est_bytes, est_files) = crate::manifest::estimate_checkout(&checkout)?;
    let shallow = Command::new("git")
        .arg("-C")
        .arg(&checkout)
        .args(["rev-parse", "--is-shallow-repository"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "true");
    Some(ProjectFacts {
        est_files,
        est_bytes,
        shallow,
    })
}

pub fn source_is_clean(checkout: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.is_empty())
}

/// Source-client state the spare-keeping rule reads.
pub struct PoolContext {
    defaults: BTreeMap<String, String>,
    active: Option<HashSet<String>>,
    sharing_disabled: bool,
}

impl PoolContext {
    pub fn load(source: &Path, registry: &Path, sharing_disabled: bool) -> Self {
        Self {
            defaults: defaults::load(registry)
                .map(|defaults| defaults.projects)
                .unwrap_or_default(),
            active: active_projects(source)
                .ok()
                .map(|projects| projects.into_iter().collect()),
            sharing_disabled,
        }
    }
}

/// Whether a default `add` disables sharing because every active project of
/// the tree's client has a fixed clone depth.
pub fn sharing_disabled(meta: &TreeMeta) -> bool {
    let mut active = meta
        .projects
        .iter()
        .filter(|project| project.state != "skipped")
        .peekable();
    active.peek().is_some() && active.all(|project| project.clone_depth.is_some())
}

/// Why a default `add` would never take a spare of `project`, or `None` when
/// it would. `source_is_clean` is consulted only for fixed-depth projects,
/// which a default `add` pools only when their source checkout is too dirty
/// to share.
pub fn orphan_reason(
    project: &str,
    facts: Option<&ProjectFacts>,
    context: &PoolContext,
    source_is_clean: impl FnOnce() -> bool,
) -> Option<String> {
    if context
        .active
        .as_ref()
        .is_some_and(|active| !active.contains(project))
    {
        return Some("not an active project".into());
    }
    match context.defaults.get(project).map(String::as_str) {
        Some("pool") => return None,
        Some(mode) => return Some(format!("its default is {mode}")),
        None => {}
    }
    let Some(facts) = facts else {
        return Some("its source checkout is missing".into());
    };
    let estimated = estimated_checkout_ms(facts.est_files, facts.est_bytes);
    if estimated < POOL_MIN_ESTIMATED_MS {
        return Some(format!("quick to check out (about {estimated} ms)"));
    }
    if facts.shallow && !context.sharing_disabled && source_is_clean() {
        return Some("shared or overlaid from its clean source checkout".into());
    }
    None
}

struct Orphan {
    project: String,
    reason: String,
    facts: Option<ProjectFacts>,
}

/// Which projects the pool should hold spares for, and which pooled projects
/// it should not.
struct Plan {
    kept: Vec<(String, ProjectFacts)>,
    orphaned: Vec<Orphan>,
}

fn plan(source: &Path, registry: &Path) -> AppResult<Plan> {
    let trees = load_all(registry)?;
    let pooled = pooled_projects(registry)
        .into_iter()
        .collect::<HashSet<_>>();
    let mut candidates = pooled.iter().cloned().collect::<BTreeSet<_>>();
    let mut known = HashMap::new();
    for tree in &trees {
        for project in &tree.projects {
            if project.mode == MODE_WORKTREE && tree.uses_pool(project) {
                candidates.insert(project.path.clone());
            }
            if let Some(facts) = ProjectFacts::from_meta(project) {
                known.entry(project.path.clone()).or_insert(facts);
            }
        }
    }
    let context = PoolContext::load(
        source,
        registry,
        trees
            .iter()
            .max_by(|left, right| left.created_at.cmp(&right.created_at))
            .is_some_and(sharing_disabled),
    );
    candidates.extend(
        context
            .defaults
            .iter()
            .filter(|(_, mode)| *mode == "pool")
            .map(|(project, _)| project.clone()),
    );
    let evaluated = candidates
        .par_iter()
        .map(|project| {
            let facts = read_facts(registry, project).or_else(|| {
                let facts = known
                    .get(project)
                    .copied()
                    .or_else(|| measure_facts(source, project));
                if let (Some(facts), true) = (&facts, pooled.contains(project)) {
                    record_facts(registry, project, facts);
                }
                facts
            });
            let reason = orphan_reason(project, facts.as_ref(), &context, || {
                source_is_clean(&source.join(project))
            });
            (project.clone(), facts, reason)
        })
        .collect::<Vec<_>>();
    let mut plan = Plan {
        kept: Vec::new(),
        orphaned: Vec::new(),
    };
    for (project, facts, reason) in evaluated {
        match (reason, facts) {
            (None, Some(facts)) => plan.kept.push((project, facts)),
            (Some(reason), facts) if pooled.contains(&project) => plan.orphaned.push(Orphan {
                project,
                reason,
                facts,
            }),
            _ => {}
        }
    }
    plan.kept.sort_by_key(|(_, facts)| {
        std::cmp::Reverse(estimated_checkout_ms(facts.est_files, facts.est_bytes))
    });
    Ok(plan)
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
    facts: Option<&ProjectFacts>,
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
    if let Some(facts) = facts {
        record_facts(registry, project_path, facts);
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

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn run(command: &PoolCommand, cwd: &Path, json_output: bool, quiet: bool) -> AppResult<Value> {
    let client = discover_client(cwd, true)?;
    let source = client.root.clone();
    let registry = registry_path(&source);
    match command {
        PoolCommand::List => list(&source, &registry, json_output),
        PoolCommand::Size(args) => {
            if let Some(spares) = args.spares {
                crate::registry::with_registry_lock(
                    &source,
                    crate::registry::LockMode::Exclusive,
                    true,
                    |registry| {
                        let mut defaults = defaults::load(registry)?;
                        defaults.pool_spares = Some(spares);
                        let bytes = serde_json::to_vec_pretty(&defaults)
                            .map_err(|error| AppError::io("serialize defaults.json", error))?;
                        write_atomic(&defaults::path(registry), &bytes, 0o644)
                    },
                )?;
            }
            let spares = capacity(&registry);
            if !json_output {
                println!("{spares} spare(s) per pooled project");
            }
            Ok(json!({"spares": spares, "default": DEFAULT_SPARES}))
        }
        PoolCommand::Fill(args) => {
            let projects = if args.projects.is_empty() {
                plan(&source, &registry)?
                    .kept
                    .into_iter()
                    .map(|(project, _)| project)
                    .collect()
            } else {
                select(&source, cwd, &args.projects, Vec::new())?
            };
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
                        let started = Instant::now();
                        let result = fill_one(&source, &registry, project, false, None);
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
                    Ok(Some(slot)) => filled.push(json!({"project": project, "path": slot})),
                    Ok(None) => {}
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
            let fallback = if args.orphaned {
                plan(&source, &registry)?
                    .orphaned
                    .into_iter()
                    .map(|orphan| orphan.project)
                    .collect::<Vec<_>>()
            } else {
                pooled_projects(&registry)
            };
            let projects = if args.orphaned && !args.projects.is_empty() {
                let allowed = fallback.iter().collect::<HashSet<_>>();
                select(&source, cwd, &args.projects, Vec::new())?
                    .into_iter()
                    .filter(|project| allowed.contains(project))
                    .collect()
            } else {
                select(&source, cwd, &args.projects, fallback)?
            };
            let mut cleared = Vec::new();
            for project in projects {
                for slot in clear_project(&source, &registry, &project)? {
                    cleared.push(json!({"project": project, "path": slot}));
                }
            }
            if !quiet {
                eprintln!("removed {} spare worktree(s)", cleared.len());
            }
            Ok(json!({"cleared": cleared}))
        }
        PoolCommand::Maintain => maintain(&source, &registry),
    }
}

fn list(source: &Path, registry: &Path, json_output: bool) -> AppResult<Value> {
    let plan = plan(source, registry)?;
    let capacity = capacity(registry);
    let orphaned = plan
        .orphaned
        .iter()
        .map(|orphan| orphan.project.as_str())
        .collect::<HashSet<_>>();
    let spares = pooled_projects(registry)
        .into_iter()
        .flat_map(|project| {
            slots(registry, &project)
                .into_iter()
                .map(move |slot| (project.clone(), slot))
        })
        .map(|(project, slot)| {
            json!({
                "project": project,
                "path": slot,
                "head": slot_head(&slot),
                "orphaned": orphaned.contains(project.as_str()),
            })
        })
        .collect::<Vec<_>>();
    let mut projects = Vec::new();
    for (project, facts) in &plan.kept {
        projects.push(json!({
            "project": project,
            "spares": slots(registry, project).len(),
            "bytes_per_spare": facts.disk_bytes(),
            "orphaned": null,
        }));
    }
    let mut orphaned_spares = 0;
    let mut orphaned_bytes = 0u64;
    for orphan in &plan.orphaned {
        let count = slots(registry, &orphan.project).len();
        let bytes = orphan.facts.map(|facts| facts.disk_bytes());
        orphaned_spares += count;
        orphaned_bytes =
            orphaned_bytes.saturating_add(bytes.unwrap_or(0).saturating_mul(count as u64));
        projects.push(json!({
            "project": orphan.project,
            "spares": count,
            "bytes_per_spare": bytes,
            "orphaned": orphan.reason,
        }));
    }
    let depth = plan
        .kept
        .iter()
        .map(|(project, _)| slots(registry, project).len())
        .min();
    if !json_output {
        for project in &projects {
            let count = project["spares"].as_u64().unwrap_or(0);
            let size = project["bytes_per_spare"]
                .as_u64()
                .map(|bytes| format!("  about {} each", human_bytes(bytes)))
                .unwrap_or_default();
            let orphan = project["orphaned"]
                .as_str()
                .map(|reason| format!("  orphaned: {reason}"))
                .unwrap_or_default();
            println!(
                "{}  {count} spare(s){size}{orphan}",
                project["project"].as_str().unwrap_or("")
            );
        }
        match depth {
            Some(depth) => println!(
                "depth {depth} of {capacity}: the next {depth} add(s) can take every pooled project from the pool"
            ),
            None => println!("no pooled projects"),
        }
        if orphaned_spares > 0 {
            println!(
                "{orphaned_spares} orphaned spare(s), about {}: no default add uses them; `pool clear --orphaned` deletes them",
                human_bytes(orphaned_bytes)
            );
        }
    }
    Ok(json!({
        "spares": spares,
        "projects": projects,
        "capacity": capacity,
        "depth": depth,
        "orphaned_spares": orphaned_spares,
        "orphaned_bytes": orphaned_bytes,
    }))
}

/// Deletes every spare of `project` and its pool directory.
fn clear_project(source: &Path, registry: &Path, project: &str) -> AppResult<Vec<PathBuf>> {
    let _held =
        lock_project(registry, project).map_err(|reason| AppError::locked("E_LOCKED", reason))?;
    let directory = project_dir(registry, project);
    let entries = fs::read_dir(&directory)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut cleared = Vec::new();
    match common_dir(source, project) {
        Ok(common) => {
            for slot in entries {
                remove_slot(&common, &slot)
                    .map_err(|reason| AppError::operational("E_IO", reason))?;
                cleared.push(slot);
            }
        }
        Err(_) => {
            cleared.extend(entries);
            let _ = fs::remove_dir_all(&directory);
        }
    }
    let _ = fs::remove_dir(&directory);
    let _ = fs::remove_file(facts_path(registry, project));
    Ok(cleared)
}

/// Removes spares beyond `capacity`, newest first.
fn trim_project(source: &Path, registry: &Path, project: &str, capacity: usize) -> usize {
    let Ok(_held) = lock_project(registry, project) else {
        return 0;
    };
    let current = slots(registry, project);
    if current.len() <= capacity {
        return 0;
    }
    let Ok(common) = common_dir(source, project) else {
        return 0;
    };
    current
        .iter()
        .rev()
        .take(current.len() - capacity)
        .filter(|slot| remove_slot(&common, slot).is_ok())
        .count()
}

/// Whether the filesystem holding the pool can take `bytes` more and keep
/// its reserve free.
fn has_room(directory: &Path, bytes: u64) -> bool {
    let Ok(output) = Command::new("df").arg("-Pk").arg(directory).output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(fields) = text
        .lines()
        .nth(1)
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
    else {
        return false;
    };
    let (Some(total), Some(available)) = (
        fields.get(1).and_then(|value| value.parse::<u64>().ok()),
        fields.get(3).and_then(|value| value.parse::<u64>().ok()),
    ) else {
        return false;
    };
    let reserve = std::env::var("GRW_TEST_POOL_RESERVE_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            (total.saturating_mul(1024) / 100 * RESERVED_FREE_PERCENT).min(RESERVED_FREE_MAX_BYTES)
        });
    available.saturating_mul(1024).saturating_sub(bytes) >= reserve
}

/// Whether a tree operation (`add`, `remove`, `repair`, ...) holds its tree
/// lock right now.
fn tree_operation_active(registry: &Path) -> bool {
    fs::read_dir(registry)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name() != DIR_NAME)
                .map(|entry| crate::registry::tree_lock_path(&entry.path()))
                .any(|lock| crate::registry::is_locked(&lock))
        })
        .unwrap_or(false)
}

const YIELD_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Waits while any tree operation runs, so maintenance never competes with
/// one for the disk. Returns the milliseconds waited.
fn wait_for_quiet_trees(registry: &Path) -> u64 {
    let started = Instant::now();
    while tree_operation_active(registry) && !crate::INTERRUPTED.load(Ordering::SeqCst) {
        std::thread::sleep(YIELD_POLL);
    }
    crate::metrics::elapsed_ms(started)
}

fn signal_group(group: u32, signal: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(format!("-{group}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Runs `command` in its own process group, stopping the whole group while
/// any tree operation runs and continuing it afterwards: buffered writes from
/// a refill otherwise throttle the operation's writes, whatever their I/O
/// priority. Returns the exit status and the milliseconds spent stopped.
fn run_yielding(
    mut command: Command,
    registry: &Path,
) -> std::io::Result<(std::process::ExitStatus, u64)> {
    let mut child = command.process_group(0).spawn()?;
    let group = child.id();
    let mut stopped_since: Option<Instant> = None;
    let mut stopped_ms = 0;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, stopped_ms));
        }
        let active = tree_operation_active(registry) && !crate::INTERRUPTED.load(Ordering::SeqCst);
        match (active, stopped_since) {
            (true, None) => {
                signal_group(group, "STOP");
                stopped_since = Some(Instant::now());
            }
            (false, Some(since)) => {
                signal_group(group, "CONT");
                stopped_ms += crate::metrics::elapsed_ms(since);
                stopped_since = None;
            }
            _ => {}
        }
        std::thread::sleep(YIELD_POLL);
    }
}

/// Deletes orphaned spares, trims pools above the pool size, and refills the
/// rest one spare at a time, shallowest pool first. Exits at once when
/// another maintenance run holds the lock, and yields to tree operations.
fn maintain(source: &Path, registry: &Path) -> AppResult<Value> {
    let directory = registry.join(DIR_NAME);
    if !registry.is_dir() {
        return Ok(json!({"skipped": "no registry"}));
    }
    fs::create_dir_all(&directory)
        .map_err(|error| AppError::io(format!("create {}", directory.display()), error))?;
    let again = directory.join(MAINTAIN_AGAIN);
    let _ = fs::write(&again, "");
    crate::metrics::record("system_start", crate::metrics::system_load());
    let capacity = capacity(registry);
    let mut reaped = Vec::new();
    let mut trimmed = Vec::new();
    let mut filled = Vec::new();
    let mut failed = Vec::new();
    let mut deferred = BTreeSet::new();
    let mut trees_deleted = 0;
    let mut passes = 0;
    loop {
        let Some(held) = try_acquire(&directory.join(MAINTAIN_LOCK), "pool maintenance")? else {
            if passes == 0 {
                crate::metrics::record("skipped", json!("another maintenance run is active"));
                return Ok(json!({"skipped": "another maintenance run is active"}));
            }
            break;
        };
        let _ = fs::remove_file(&again);
        passes += 1;
        for _ in 0..MAINTENANCE_ROUNDS {
            if crate::INTERRUPTED.load(Ordering::SeqCst) {
                break;
            }
            let mut worked = false;
            let step = Instant::now();
            crate::metrics::phase("paused", wait_for_quiet_trees(registry));
            let deleted = crate::trash::empty(registry, |command| {
                run_yielding(command, registry).is_ok_and(|(status, stopped_ms)| {
                    crate::metrics::phase("paused", stopped_ms);
                    status.success()
                })
            });
            crate::metrics::phase("trash", crate::metrics::elapsed_ms(step));
            if deleted > 0 {
                worked = true;
                trees_deleted += deleted;
            }
            let step = Instant::now();
            let plan = plan(source, registry)?;
            crate::metrics::phase("plan", crate::metrics::elapsed_ms(step));
            let step = Instant::now();
            for orphan in &plan.orphaned {
                crate::metrics::phase("paused", wait_for_quiet_trees(registry));
                let cleared = clear_project(source, registry, &orphan.project)?;
                if !cleared.is_empty() {
                    worked = true;
                    reaped.push(json!({
                    "project": orphan.project,
                    "spares": cleared.len(),
                    "reason": orphan.reason,
                    "bytes": orphan.facts.map(|facts| facts.disk_bytes().saturating_mul(cleared.len() as u64)),
                }));
                }
            }
            for (project, _) in &plan.kept {
                let removed = trim_project(source, registry, project, capacity);
                if removed > 0 {
                    worked = true;
                    trimmed.push(json!({"project": project, "spares": removed}));
                }
            }
            crate::metrics::phase("reap", crate::metrics::elapsed_ms(step));
            let step = Instant::now();
            let mut blocked = HashSet::new();
            for level in 1..=capacity {
                for (project, facts) in &plan.kept {
                    if blocked.contains(project) || slots(registry, project).len() >= level {
                        continue;
                    }
                    if crate::INTERRUPTED.load(Ordering::SeqCst) {
                        break;
                    }
                    crate::metrics::phase("paused", wait_for_quiet_trees(registry));
                    if !has_room(&directory, facts.disk_bytes()) {
                        deferred.insert(project.clone());
                        blocked.insert(project.clone());
                        continue;
                    }
                    let started = Instant::now();
                    match fill_one(source, registry, project, true, Some(capacity)) {
                        Ok(Some(_)) => {
                            worked = true;
                            filled.push(json!({
                                "project": project,
                                "ms": crate::metrics::elapsed_ms(started),
                            }));
                        }
                        Ok(None) => {}
                        Err(reason) => {
                            blocked.insert(project.clone());
                            failed.push(json!({"project": project, "reason": reason}));
                        }
                    }
                }
            }
            crate::metrics::phase("fill", crate::metrics::elapsed_ms(step));
            if !worked {
                break;
            }
        }
        drop(held);
        if !again.exists() || crate::INTERRUPTED.load(Ordering::SeqCst) {
            break;
        }
    }
    crate::metrics::record("passes", json!(passes));
    let deferred = deferred.into_iter().collect::<Vec<_>>();
    crate::metrics::record("reaped", json!(reaped));
    crate::metrics::record("trimmed", json!(trimmed));
    crate::metrics::record("filled", json!(filled));
    crate::metrics::record("failed", json!(failed));
    crate::metrics::record("deferred", json!(deferred));
    crate::metrics::record("trees_deleted", json!(trees_deleted));
    crate::metrics::record("system_end", crate::metrics::system_load());
    Ok(json!({
        "reaped": reaped,
        "trimmed": trimmed,
        "filled": filled,
        "failed": failed,
        "deferred": deferred,
        "trees_deleted": trees_deleted,
    }))
}

pub fn maintenance_disabled() -> bool {
    std::env::var(NO_MAINTENANCE_ENV).is_ok_and(|value| !value.is_empty() && value != "0")
}

fn find_program(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

/// Starts `pool maintain` detached, at idle I/O and lowest CPU priority, so
/// spares are reaped and refilled after the calling command has returned.
pub fn spawn_maintenance(source: &Path) -> bool {
    if maintenance_disabled() {
        return false;
    }
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let mut argv: Vec<OsString> = Vec::new();
    if let Some(ionice) = find_program("ionice") {
        argv.extend([ionice.into(), "-c".into(), "3".into(), "-t".into()]);
    }
    if let Some(nice) = find_program("nice") {
        argv.extend([nice.into(), "-n".into(), "19".into()]);
    }
    argv.push(executable.into());
    argv.extend([
        "-C".into(),
        source.as_os_str().to_owned(),
        "-q".into(),
        "pool".into(),
        "maintain".into(),
    ]);
    Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(source)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .is_ok()
}

/// Whether `project`'s pool already holds `capacity` spares.
pub fn is_full(registry: &Path, project_path: &str, capacity: usize) -> bool {
    slots(registry, project_path).len() >= capacity
}

/// Whether any of a removed tree's pooled projects is below the pool size.
/// Orphans are left for the maintenance that follows the next `add`.
pub fn needs_refill(meta: &TreeMeta) -> bool {
    let registry = registry_path(&meta.source());
    let capacity = capacity(&registry);
    meta.projects
        .iter()
        .filter(|project| project.mode == MODE_WORKTREE && meta.uses_pool(project))
        .any(|project| !is_full(&registry, &project.path, capacity))
}

/// Whether a tree's pool may need attention once the command finishes.
pub fn maintenance_wanted(meta: &TreeMeta) -> bool {
    let registry = registry_path(&meta.source());
    registry.join(DIR_NAME).is_dir()
        || crate::trash::pending(&registry)
        || meta
            .projects
            .iter()
            .any(|project| project.mode == MODE_WORKTREE && meta.uses_pool(project))
}

/// Builds a spare outside the project lock, then publishes it under the lock.
/// With `capacity`, a spare that would overfill the pool is discarded and
/// `Ok(None)` returned. `background` checks out with fewer workers.
fn fill_one(
    source: &Path,
    registry: &Path,
    project: &str,
    background: bool,
    capacity: Option<usize>,
) -> Result<Option<PathBuf>, String> {
    let common = common_dir(source, project)?;
    let directory = project_dir(registry, project);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    if read_facts(registry, project).is_none() {
        if let Some(facts) = measure_facts(source, project) {
            record_facts(registry, project, &facts);
        }
    }
    let name = unique_name();
    let staging = directory.join(format!("{FILLING_PREFIX}{name}"));
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(source.join(project))
        .args([
            "-c",
            if background {
                "checkout.workers=2"
            } else {
                "checkout.workers=8"
            },
            "-c",
            "checkout.thresholdForParallelism=100",
            "worktree",
            "add",
            "--detach",
        ])
        .arg(&staging)
        .arg("HEAD");
    if background {
        let log = directory.join(format!("{FILLING_PREFIX}{name}.log"));
        let stderr =
            fs::File::create(&log).map_err(|error| format!("create {}: {error}", log.display()))?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        let outcome = run_yielding(command, registry);
        let message = fs::read_to_string(&log).unwrap_or_default();
        let _ = fs::remove_file(&log);
        let (status, stopped_ms) =
            outcome.map_err(|error| format!("run git worktree add: {error}"))?;
        crate::metrics::phase("paused", stopped_ms);
        if !status.success() {
            let _ = remove_slot(&common, &staging);
            return Err(format!(
                "exit {}: {}",
                status.code().unwrap_or(1),
                message.trim().chars().take(500).collect::<String>()
            ));
        }
    } else {
        let output = command
            .output()
            .map_err(|error| format!("run git worktree add: {error}"))?;
        if !output.status.success() {
            let _ = remove_slot(&common, &staging);
            return Err(exit_reason(&output));
        }
    }
    let slot = directory.join(&name);
    let published = match lock_project(registry, project) {
        Ok(_held) => {
            if capacity.is_some_and(|capacity| slots(registry, project).len() >= capacity) {
                Err(None)
            } else {
                git_worktree_move(&common, &staging, &slot).map_err(Some)
            }
        }
        Err(error) => Err(Some(error)),
    };
    match published {
        Ok(()) => Ok(Some(slot)),
        Err(error) => {
            let _ = remove_slot(&common, &staging);
            error.map_or(Ok(None), Err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(defaults: &[(&str, &str)], active: &[&str]) -> PoolContext {
        PoolContext {
            defaults: defaults
                .iter()
                .map(|(project, mode)| (project.to_string(), mode.to_string()))
                .collect(),
            active: Some(active.iter().map(|project| project.to_string()).collect()),
            sharing_disabled: false,
        }
    }

    fn facts(est_files: u64, est_bytes: u64, shallow: bool) -> ProjectFacts {
        ProjectFacts {
            est_files,
            est_bytes,
            shallow,
        }
    }

    #[test]
    fn spares_are_kept_only_for_projects_a_default_add_pools() {
        let all = [
            "tools/idea",
            "prebuilts/tools",
            "external/dexter",
            "sdk/linux",
            "small",
        ];
        let context = context(&[("small", "pool"), ("tools/base", "worktree")], &all);
        let idea = facts(274_366, 982_000_000, false);
        let prebuilt = facts(40_000, 26_800_000_000, true);
        let dexter = facts(183, 666_000_000, false);
        assert_eq!(
            orphan_reason("tools/idea", Some(&idea), &context, || true),
            None
        );
        assert_eq!(
            orphan_reason("prebuilts/tools", Some(&prebuilt), &context, || true).as_deref(),
            Some("shared or overlaid from its clean source checkout")
        );
        assert_eq!(
            orphan_reason("sdk/linux", Some(&prebuilt), &context, || false),
            None
        );
        assert!(
            orphan_reason("external/dexter", Some(&dexter), &context, || true)
                .unwrap()
                .starts_with("quick to check out")
        );
        assert_eq!(
            orphan_reason("small", Some(&facts(1, 1, true)), &context, || true),
            None
        );
        assert_eq!(
            orphan_reason("tools/base", Some(&idea), &context, || true).as_deref(),
            Some("not an active project")
        );
        let mut shallow_client = context;
        shallow_client.sharing_disabled = true;
        assert_eq!(
            orphan_reason("prebuilts/tools", Some(&prebuilt), &shallow_client, || true),
            None
        );
    }

    #[test]
    fn explicit_non_pool_defaults_orphan_spares() {
        let context = context(&[("tools/idea", "worktree")], &["tools/idea"]);
        assert_eq!(
            orphan_reason(
                "tools/idea",
                Some(&facts(274_366, 982_000_000, false)),
                &context,
                || true
            )
            .as_deref(),
            Some("its default is worktree")
        );
    }

    #[test]
    fn disk_estimate_counts_a_block_per_file() {
        assert_eq!(
            facts(274_366, 982_000_000, false).disk_bytes(),
            2_105_803_136
        );
    }
}
