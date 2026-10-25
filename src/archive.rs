//! Archives of removed trees.
//!
//! `remove` records each worktree-backed project's HEAD, branch, index, and
//! working tree before deleting a tree, so `restore` can recreate it. Index
//! and working tree are stored as trees of a commit in the project's own
//! repository, kept reachable by `refs/repo-worktree/archive/<id>`; the
//! tree-level record lives in `.repo/repo-worktrees/.archive/<id>/`.

use crate::cli::ArchiveCommand;
use crate::discovery::discover_client;
use crate::error::{AppError, AppResult};
use crate::inspect::{checkout_git_dir, inspect_project, nested_children};
use crate::model::{ProjectMeta, TreeMeta, MODE_OVERLAY};
use crate::registry::registry_path;
use crate::util::{absolutize, canonicalize_missing, exit_reason, now, write_atomic};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub const DIR_NAME: &str = ".archive";
pub const REF_PREFIX: &str = "refs/repo-worktree/archive/";
const FORMAT_VERSION: u32 = 1;
const RECORD_FILE: &str = "archive.json";
const SNAPSHOT_FILE: &str = "snapshot.xml";
pub const DEFAULT_EXPIRE_SECS: u64 = 30 * 86_400;
pub const DEFAULT_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Age after which an archive whose capture never finished (the process was
/// killed) is deleted with its refs.
const ABANDONED_SECS: u64 = 3_600;
/// Untracked files at least this large (build outputs, downloaded SDKs) are
/// left out of archives and reported as dropped.
pub const LARGE_UNTRACKED_BYTES: u64 = 100 * 1024 * 1024;
const IN_PROGRESS: &[(&str, &str)] = &[
    ("rebase-merge", "rebase"),
    ("rebase-apply", "rebase or am"),
    ("MERGE_HEAD", "merge"),
    ("CHERRY_PICK_HEAD", "cherry-pick"),
    ("REVERT_HEAD", "revert"),
    ("BISECT_LOG", "bisect"),
];

/// The `add` options a restored tree is created with.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Options {
    pub branch: Option<String>,
    #[serde(default)]
    pub branch_projects: Option<Vec<String>>,
    #[serde(default)]
    pub submodules: bool,
    #[serde(default)]
    pub full: bool,
    #[serde(default)]
    pub full_projects: Vec<String>,
    #[serde(default)]
    pub default_modes: BTreeMap<String, String>,
    #[serde(default)]
    pub no_reflink: bool,
    #[serde(default)]
    pub no_overlay: bool,
    #[serde(default)]
    pub git_only: bool,
    #[serde(default = "crate::model::default_heavy_jobs")]
    pub heavy_jobs: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifests {
    pub git_dir: String,
    pub branch: Option<String>,
    pub commit: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Project {
    pub path: String,
    pub name: String,
    pub mode: String,
    pub git_common_dir: String,
    /// The commit the tree was created at.
    pub pinned: String,
    pub head: String,
    pub branch: Option<String>,
    /// Commit whose tree is the working tree, with parents HEAD and a commit
    /// of the index; `None` when both matched HEAD.
    pub archive: Option<String>,
    pub index: Option<String>,
    pub worktree: Option<String>,
    /// Whether the archive ref was written in the project's repository.
    #[serde(default)]
    pub referenced: bool,
    #[serde(default)]
    pub modified: usize,
    #[serde(default)]
    pub untracked: usize,
    #[serde(default)]
    pub unbranched_commits: usize,
    #[serde(default)]
    pub bytes: u64,
}

impl Project {
    pub fn tip(&self) -> &str {
        self.archive.as_deref().unwrap_or(&self.head)
    }

    pub fn has_changes(&self) -> bool {
        self.archive.is_some() || self.unbranched_commits > 0
    }

    fn needs_ref(&self) -> bool {
        self.archive.is_some() || self.unbranched_commits > 0 || self.head != self.pinned
    }
}

/// State an archive could not hold.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub project: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub format_version: u32,
    pub id: String,
    pub tree: String,
    pub path: String,
    pub source_root: String,
    pub archived_at: String,
    pub archived_unix: u64,
    pub tool_version: String,
    pub reason: String,
    #[serde(default)]
    pub keep: bool,
    /// False until every ref is written; the record is written first so a
    /// killed capture never leaves refs that nothing lists.
    #[serde(default = "complete_by_default")]
    pub complete: bool,
    pub options: Options,
    pub manifests: Option<Manifests>,
    pub projects: Vec<Project>,
    pub bytes: u64,
    #[serde(default)]
    pub dropped: Vec<Item>,
}

impl Record {
    pub fn projects_with_changes(&self) -> usize {
        self.projects
            .iter()
            .filter(|project| project.has_changes())
            .count()
    }

    pub fn ref_name(&self) -> String {
        format!("{REF_PREFIX}{}", self.id)
    }
}

fn complete_by_default() -> bool {
    true
}

pub struct Captured {
    pub record: Record,
    /// Projects with nothing to lose, which may return to the pool.
    pub clean: HashSet<String>,
}

pub struct Policy {
    pub expire_secs: u64,
    pub budget_bytes: u64,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn format_unix(seconds: u64) -> String {
    OffsetDateTime::from_unix_timestamp(seconds as i64)
        .ok()
        .and_then(|time| time.format(&Rfc3339).ok())
        .unwrap_or_default()
}

pub fn dir(registry: &Path) -> PathBuf {
    registry.join(DIR_NAME)
}

pub fn record_dir(registry: &Path, id: &str) -> PathBuf {
    dir(registry).join(id)
}

pub fn snapshot_path(registry: &Path, id: &str) -> PathBuf {
    record_dir(registry, id).join(SNAPSHOT_FILE)
}

/// Serializes changes to archive records (drop, keep, expiry).
pub fn lock(registry: &Path, quiet: bool) -> AppResult<crate::registry::Held> {
    let directory = dir(registry);
    fs::create_dir_all(&directory)
        .map_err(|error| AppError::io(format!("create {}", directory.display()), error))?;
    crate::registry::acquire(&directory.join(".lock"), "archives", quiet)
}

/// Creates the record directory under a fresh id, `<tree>-<UTC time>`.
fn reserve(registry: &Path, tree: &str) -> AppResult<(String, PathBuf)> {
    let directory = dir(registry);
    fs::create_dir_all(&directory)
        .map_err(|error| AppError::io(format!("create {}", directory.display()), error))?;
    let stamp = OffsetDateTime::now_utc()
        .format(time::macros::format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .unwrap_or_default();
    for attempt in 1.. {
        let id = if attempt == 1 {
            format!("{tree}-{stamp}")
        } else {
            format!("{tree}-{stamp}-{attempt}")
        };
        let path = directory.join(&id);
        match fs::create_dir(&path) {
            Ok(()) => return Ok((id, path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(AppError::io(format!("create {}", path.display()), error)),
        }
    }
    unreachable!("ids are unbounded")
}

/// Every readable archive whose capture finished, newest first.
pub fn load_all(registry: &Path) -> Vec<Record> {
    let mut records = load_records(registry)
        .into_iter()
        .filter(|record| record.complete)
        .collect::<Vec<_>>();
    records.sort_by(|a, b| (b.archived_unix, &b.id).cmp(&(a.archived_unix, &a.id)));
    records
}

fn load_records(registry: &Path) -> Vec<Record> {
    fs::read_dir(dir(registry))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| fs::read(entry.path().join(RECORD_FILE)).ok())
        .filter_map(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
        .filter(|record| record.format_version <= FORMAT_VERSION)
        .collect()
}

/// Deletes captures a killed process never finished: records still marked
/// incomplete, with their refs, and reserved directories that never got a
/// record. Only those older than an hour, so running captures are left alone.
fn delete_abandoned(registry: &Path, now: u64) -> Vec<String> {
    let mut deleted = Vec::new();
    for record in load_records(registry) {
        if !record.complete && now.saturating_sub(record.archived_unix) > ABANDONED_SECS {
            let _ = drop_record(registry, &record);
            deleted.push(record.id);
        }
    }
    for entry in fs::read_dir(dir(registry)).into_iter().flatten().flatten() {
        let path = entry.path();
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .is_some_and(|age| now.saturating_sub(age.as_secs()) > ABANDONED_SECS);
        if path.is_dir() && old && !path.join(RECORD_FILE).exists() {
            let _ = fs::remove_dir_all(&path);
            deleted.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    deleted
}

/// Selects an archive by id, `<tree>@{n}` (newest first), tree name, or the
/// tree's former path; names and paths pick the newest match.
pub fn select(registry: &Path, selector: &str, cwd: &Path) -> AppResult<Record> {
    let records = load_all(registry);
    let missing = || {
        AppError::operational(
            "E_NO_SUCH_ARCHIVE",
            format!("no archive matches {selector}; see `git repo-worktree archive list`"),
        )
    };
    if let Some(record) = records.iter().find(|record| record.id == selector) {
        return Ok(record.clone());
    }
    if let Some((name, index)) = selector
        .strip_suffix('}')
        .and_then(|rest| rest.rsplit_once("@{"))
    {
        let index = index.parse::<usize>().map_err(|_| missing())?;
        return records
            .into_iter()
            .filter(|record| record.tree == name)
            .nth(index)
            .ok_or_else(missing);
    }
    let looks_like_path = selector.contains('/') || selector == "." || selector == "..";
    if !looks_like_path {
        if let Some(record) = records.iter().find(|record| record.tree == selector) {
            return Ok(record.clone());
        }
    }
    let requested = canonicalize_missing(&absolutize(Path::new(selector), cwd))?;
    records
        .into_iter()
        .find(|record| {
            canonicalize_missing(Path::new(&record.path)).is_ok_and(|path| path == requested)
        })
        .ok_or_else(missing)
}

/// The newest archive with changes taken from `path`, for hints.
pub fn newest_with_changes_at(registry: &Path, path: &Path) -> Option<Record> {
    load_all(registry)
        .into_iter()
        .find(|record| Path::new(&record.path) == path && record.projects_with_changes() > 0)
}

fn git(directory: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .map_err(|error| format!("run git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!("git {}: {}", args.join(" "), exit_reason(&output)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_dir_command(git_dir: &str) -> Command {
    let mut command = Command::new("git");
    command.arg(format!("--git-dir={git_dir}"));
    command
}

fn update_ref(git_dir: &str, name: &str, value: &str) -> Result<(), String> {
    let output = git_dir_command(git_dir)
        .args(["update-ref", name, value])
        .output()
        .map_err(|error| format!("run git update-ref: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(exit_reason(&output))
    }
}

fn delete_ref(git_dir: &str, name: &str) {
    let _ = git_dir_command(git_dir)
        .args(["update-ref", "-d", name])
        .output();
}

/// Bytes of objects reachable only from `tip`: not from the commit the tree
/// started at (which the source's history already holds) and not from any
/// branch, remote-tracking ref, or tag.
fn unique_bytes(git_dir: &str, tip: &str, pinned: &str) -> u64 {
    git_dir_command(git_dir)
        .args(["rev-list", "--objects", "--disk-usage", tip, "--not"])
        .args((!pinned.is_empty()).then_some(pinned))
        .args(["--branches", "--remotes", "--tags"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().parse().ok())
        .unwrap_or(0)
}

fn commit_tree(
    checkout: &Path,
    tree: &str,
    parents: &[&str],
    message: &str,
) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(checkout)
        .env("GIT_AUTHOR_NAME", "git-repo-worktree")
        .env("GIT_AUTHOR_EMAIL", "git-repo-worktree@localhost")
        .env("GIT_COMMITTER_NAME", "git-repo-worktree")
        .env("GIT_COMMITTER_EMAIL", "git-repo-worktree@localhost")
        .args(["commit-tree", tree, "-m", message]);
    for parent in parents {
        command.args(["-p", parent]);
    }
    let output = command
        .output()
        .map_err(|error| format!("run git commit-tree: {error}"))?;
    if !output.status.success() {
        return Err(format!("git commit-tree: {}", exit_reason(&output)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

struct Changes {
    commit: String,
    index: String,
    worktree: String,
    items: Vec<Item>,
    /// Untracked files left out by policy; reported, never a reason to refuse.
    skipped: Vec<Item>,
}

/// Untracked, non-ignored files of at least `LARGE_UNTRACKED_BYTES`, with
/// their sizes, outside the project's nested projects.
fn large_untracked(checkout: &Path, children: &[String]) -> Vec<(String, u64)> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(checkout)
        .args(["ls-files", "-z", "-o", "--exclude-standard", "--", "."]);
    for child in children {
        command.arg(format!(":(exclude,literal){child}"));
    }
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| {
            let path = String::from_utf8_lossy(path).into_owned();
            let metadata = fs::symlink_metadata(checkout.join(&path)).ok()?;
            (metadata.is_file() && metadata.len() >= LARGE_UNTRACKED_BYTES)
                .then_some((path, metadata.len()))
        })
        .collect()
}

/// Writes the index and the working tree (untracked files included, ignored
/// files and nested projects excluded) as trees, and a commit holding both.
fn snapshot_changes(
    checkout: &Path,
    project: &str,
    head: &str,
    children: &[String],
    id: &str,
) -> Result<Changes, String> {
    let item = |kind: &str, detail: String| Item {
        project: project.to_string(),
        kind: kind.into(),
        detail,
    };
    let head_tree = git(checkout, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let mut items = Vec::new();
    let index = match git(checkout, &["write-tree"]) {
        Ok(tree) => tree,
        Err(_) => {
            items.push(item(
                "conflicts",
                "unmerged index entries; the working tree is archived but the index is not".into(),
            ));
            head_tree
        }
    };
    let index_path = PathBuf::from(git(
        checkout,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?);
    let temporary = index_path.with_file_name(format!(
        "index.repo-worktree-archive-{}",
        std::process::id()
    ));
    match fs::copy(&index_path, &temporary) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("copy {}: {error}", index_path.display())),
    }
    let large = large_untracked(checkout, children);
    let skipped = large
        .iter()
        .map(|(path, bytes)| {
            item(
                "large-untracked",
                format!("{path} ({}) is not archived", human_bytes(*bytes)),
            )
        })
        .collect::<Vec<_>>();
    let pathspecs = temporary.with_extension("pathspecs");
    let worktree = (|| {
        let mut specs = Vec::new();
        specs.extend_from_slice(b".\0");
        for path in children.iter().chain(large.iter().map(|(path, _)| path)) {
            specs.extend_from_slice(format!(":(exclude,literal){path}").as_bytes());
            specs.push(0);
        }
        fs::write(&pathspecs, specs)
            .map_err(|error| format!("write {}: {error}", pathspecs.display()))?;
        let output = Command::new("git")
            .arg("-C")
            .arg(checkout)
            .env("GIT_INDEX_FILE", &temporary)
            .args(["add", "-A", "--pathspec-file-nul"])
            .arg(format!("--pathspec-from-file={}", pathspecs.display()))
            .output()
            .map_err(|error| format!("run git add: {error}"))?;
        if !output.status.success() {
            return Err(format!("git add -A: {}", exit_reason(&output)));
        }
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            if let Some(path) = line.strip_prefix("warning: adding embedded git repository: ") {
                items.push(item(
                    "embedded-repository",
                    format!("{path} is a Git repository inside the project; only its commit is archived"),
                ));
            }
        }
        let output = Command::new("git")
            .arg("-C")
            .arg(checkout)
            .env("GIT_INDEX_FILE", &temporary)
            .arg("write-tree")
            .output()
            .map_err(|error| format!("run git write-tree: {error}"))?;
        if !output.status.success() {
            return Err(format!("git write-tree: {}", exit_reason(&output)));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    })();
    let _ = fs::remove_file(&temporary);
    let _ = fs::remove_file(&pathspecs);
    let worktree = worktree?;
    let index_commit = commit_tree(checkout, &index, &[head], &format!("index of {project}"))?;
    let commit = commit_tree(
        checkout,
        &worktree,
        &[head, &index_commit],
        &format!("repo-worktree archive {id} {project}"),
    )?;
    Ok(Changes {
        commit,
        index,
        worktree,
        items,
        skipped,
    })
}

fn capture_project(
    meta: &TreeMeta,
    project: &ProjectMeta,
    id: &str,
) -> (Option<Project>, Vec<Item>, Vec<Item>) {
    let root = meta.root();
    let checkout = root.join(&project.path);
    let children = nested_children(meta, project);
    let inspection = inspect_project(&root, &meta.registry_entry(), project, &children);
    let item = |kind: &str, detail: String| Item {
        project: project.path.clone(),
        kind: kind.into(),
        detail,
    };
    let mut items = Vec::new();
    let mut skipped = Vec::new();
    if let Some(error) = &inspection.error {
        let detail = match project.reason.as_deref().and_then(|reason| reason.strip_prefix("remount: ")) {
            Some(reason) if project.mode == MODE_OVERLAY => format!(
                "the overlay has changes but cannot be mounted ({reason}); pass --force to archive the rest, or --discard"
            ),
            _ => error.clone(),
        };
        items.push(item("unverifiable", detail));
        return (None, items, skipped);
    }
    let Some(head) = inspection.head.clone() else {
        return (None, items, skipped);
    };
    if let Some(git_dir) = checkout_git_dir(&checkout) {
        for (marker, operation) in IN_PROGRESS {
            if git_dir.join(marker).exists() {
                items.push(item(
                    "in-progress",
                    format!("a {operation} is in progress; finish or abort it"),
                ));
            }
        }
    }
    for submodule in &inspection.submodule_changes {
        items.push(item(
            "submodule-changes",
            format!("submodule {submodule} has uncommitted changes of its own"),
        ));
    }
    let mut archived = Project {
        path: project.path.clone(),
        name: project.name.clone(),
        mode: project.mode.clone(),
        git_common_dir: project.git_common_dir.clone(),
        pinned: project.sha.clone(),
        head: head.clone(),
        branch: inspection.branch.clone(),
        modified: inspection.modified,
        untracked: inspection.untracked,
        unbranched_commits: inspection.unbranched_commits,
        ..Default::default()
    };
    if inspection.modified > 0 || inspection.untracked > 0 {
        match snapshot_changes(&checkout, &project.path, &head, &children, id) {
            Ok(changes) => {
                archived.archive = Some(changes.commit);
                archived.index = Some(changes.index);
                archived.worktree = Some(changes.worktree);
                items.extend(changes.items);
                skipped.extend(changes.skipped);
            }
            Err(reason) => {
                items.push(item("unverifiable", reason));
                return (None, items, skipped);
            }
        }
    }
    if archived.needs_ref() {
        archived.bytes = unique_bytes(&project.git_common_dir, archived.tip(), &project.sha);
    }
    (Some(archived), items, skipped)
}

/// Projects `repo sync` added inside the tree, whose Git data lives in the
/// tree's own `.repo` and would be deleted with it.
fn unmanaged_projects(meta: &TreeMeta) -> Vec<Item> {
    let root = meta.root();
    let Ok(list) = fs::read_to_string(root.join(".repo/project.list")) else {
        return Vec::new();
    };
    let known = meta
        .projects
        .iter()
        .map(|project| project.path.as_str())
        .collect::<HashSet<_>>();
    list.lines()
        .map(str::trim)
        .filter(|path| !path.is_empty() && !known.contains(path))
        .filter(|path| fs::symlink_metadata(root.join(path).join(".git")).is_ok())
        .map(|path| Item {
            project: path.to_string(),
            kind: "unmanaged-project".into(),
            detail: "added by repo sync inside the tree; its Git data lives in the tree".into(),
        })
        .collect()
}

fn options_of(meta: &TreeMeta) -> Options {
    Options {
        branch: meta.branch.clone(),
        branch_projects: meta.branch_projects.clone(),
        submodules: meta.submodules,
        full: meta.full,
        full_projects: meta.full_projects.clone(),
        default_modes: meta.default_modes.clone(),
        no_reflink: meta.no_reflink,
        no_overlay: meta.no_overlay,
        git_only: meta.repo_bridge.is_none(),
        heavy_jobs: meta.heavy_jobs,
    }
}

fn incomplete_error(items: &[Item]) -> AppError {
    let shown = items
        .iter()
        .take(10)
        .map(|item| format!("{} ({}: {})", item.project, item.kind, item.detail))
        .collect::<Vec<_>>();
    let extra = items.len().saturating_sub(shown.len());
    AppError::operational(
        "E_ARCHIVE_INCOMPLETE",
        format!(
            "the archive cannot hold everything in this tree: {}{}; resolve it, pass --force to archive the rest and drop it, or --discard to delete without archiving",
            shown.join("; "),
            if extra > 0 {
                format!(" (+{extra} more)")
            } else {
                String::new()
            }
        ),
    )
    .with_details(json!({ "items": items }))
}

fn write_record(registry: &Path, record: &Record) -> AppResult<()> {
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|error| AppError::io("serialize archive record", error))?;
    write_atomic(
        &record_dir(registry, &record.id).join(RECORD_FILE),
        &bytes,
        0o644,
    )
}

fn manifests_of(meta: &TreeMeta) -> Option<Manifests> {
    let bridge = meta.repo_bridge.as_ref()?;
    let git_dir = bridge.manifests_git_dir.clone()?;
    let commit = git(&meta.root().join(".repo/manifests"), &["rev-parse", "HEAD"]).ok()?;
    Some(Manifests {
        git_dir,
        branch: bridge.manifests_branch.clone(),
        commit,
    })
}

fn new_record(meta: &TreeMeta, id: String, reason: &str) -> Record {
    Record {
        format_version: FORMAT_VERSION,
        id,
        tree: meta.name.clone(),
        path: meta.path.clone(),
        source_root: meta.source_root.clone(),
        archived_at: now(),
        archived_unix: unix_now(),
        tool_version: env!("CARGO_PKG_VERSION").into(),
        reason: reason.into(),
        keep: false,
        complete: false,
        options: options_of(meta),
        manifests: None,
        projects: Vec::new(),
        bytes: 0,
        dropped: Vec::new(),
    }
}

/// Records a tree's state before removal. Refuses with
/// `E_ARCHIVE_INCOMPLETE` when some state cannot be archived unless `force`,
/// which records it as dropped. Nothing is written to any repository until
/// every project has been captured.
pub fn capture(
    meta: &TreeMeta,
    registry: &Path,
    reason: &str,
    force: bool,
    jobs: usize,
) -> AppResult<Captured> {
    let root = meta.root();
    let (id, directory) = reserve(registry, &meta.name)?;
    let candidates = meta
        .projects
        .iter()
        .filter(|project| project.has_worktree() && project.state != "skipped")
        .filter(|project| root.join(&project.path).exists())
        .collect::<Vec<_>>();
    let workers = ThreadPoolBuilder::new()
        .num_threads(jobs.max(1))
        .build()
        .map_err(|error| AppError::io("create worker pool", error))?;
    let results = workers.install(|| {
        candidates
            .par_iter()
            .map(|project| capture_project(meta, project, &id))
            .collect::<Vec<_>>()
    });
    let mut projects = Vec::new();
    let mut items = Vec::new();
    let mut skipped = Vec::new();
    for (project, found, left_out) in results {
        projects.extend(project);
        items.extend(found);
        skipped.extend(left_out);
    }
    items.extend(unmanaged_projects(meta));
    items.sort_by(|a, b| a.project.cmp(&b.project));
    if !items.is_empty() && !force {
        let _ = fs::remove_dir_all(&directory);
        return Err(incomplete_error(&items));
    }
    let mut record = new_record(meta, id, reason);
    let ref_name = record.ref_name();
    for project in &mut projects {
        project.referenced = project.needs_ref();
    }
    record.bytes = projects.iter().map(|project| project.bytes).sum();
    record.projects = projects;
    items.extend(skipped);
    record.dropped = items;
    record.manifests = manifests_of(meta);
    let _ = fs::copy(
        meta.registry_entry().join(SNAPSHOT_FILE),
        directory.join(SNAPSHOT_FILE),
    );
    let abandon = |record: &Record, error: AppError| {
        delete_refs(record);
        let _ = fs::remove_dir_all(&directory);
        error
    };
    write_record(registry, &record).map_err(|error| abandon(&record, error))?;
    let mut failures = workers.install(|| {
        record
            .projects
            .par_iter()
            .filter(|project| project.referenced)
            .filter_map(|project| {
                update_ref(&project.git_common_dir, &ref_name, project.tip())
                    .err()
                    .map(|reason| format!("{}: {reason}", project.path))
            })
            .collect::<Vec<_>>()
    });
    if let Some(manifests) = &record.manifests {
        if let Err(reason) = update_ref(&manifests.git_dir, &ref_name, &manifests.commit) {
            failures.push(format!("manifest repository: {reason}"));
        }
    }
    if !failures.is_empty() {
        return Err(abandon(
            &record,
            AppError::operational(
                "E_ARCHIVE",
                format!("could not record the archive: {}", failures.join("; ")),
            ),
        ));
    }
    record.complete = true;
    write_record(registry, &record).map_err(|error| abandon(&record, error))?;
    let clean = record
        .projects
        .iter()
        .filter(|project| !project.has_changes())
        .map(|project| project.path.clone())
        .collect();
    Ok(Captured { record, clean })
}

/// The HEAD a removed checkout's worktree administrative directory still
/// records, as `(commit, branch)`.
/// The worktree administrative directory in the project's repository whose
/// `gitdir` names the tree's checkout of `project`, which may already be
/// deleted. Git may record the path relative to the administrative directory.
pub fn registration(meta: &TreeMeta, project: &ProjectMeta) -> Option<PathBuf> {
    let expected = crate::util::normalize(&meta.root().join(&project.path).join(".git"));
    fs::read_dir(Path::new(&project.git_common_dir).join("worktrees"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|admin| {
            fs::read_to_string(admin.join("gitdir")).is_ok_and(|recorded| {
                let recorded = Path::new(recorded.trim());
                let absolute = if recorded.is_absolute() {
                    recorded.to_path_buf()
                } else {
                    admin.join(recorded)
                };
                crate::util::normalize(&absolute) == expected
            })
        })
}

fn orphaned_head(meta: &TreeMeta, project: &ProjectMeta) -> Option<(String, Option<String>)> {
    let Some(admin) = registration(meta, project) else {
        if project.mode != MODE_OVERLAY {
            return None;
        }
        let checkout = meta.root().join(&project.path);
        let upper = crate::overlay::upper_dir(&meta.registry_entry(), &project.path);
        return crate::overlay::admin_head(&upper, &checkout).map(|head| (head, None));
    };
    let head = fs::read_to_string(admin.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            let output = git_dir_command(&project.git_common_dir)
                .args(["rev-parse", "--verify", "-q", reference])
                .output()
                .ok()?;
            let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (output.status.success() && !commit.is_empty()).then(|| {
                (
                    commit,
                    reference.strip_prefix("refs/heads/").map(str::to_owned),
                )
            })
        }
        None => Some((head.to_string(), None)),
    }
}

/// Records the commits and branches of a tree whose directory was deleted by
/// hand, from its projects' remaining worktree registrations.
pub fn capture_orphaned(meta: &TreeMeta, registry: &Path) -> Option<Record> {
    let (id, directory) = reserve(registry, &meta.name).ok()?;
    let mut record = new_record(meta, id, "prune");
    let ref_name = record.ref_name();
    record.projects = meta
        .projects
        .par_iter()
        .filter(|project| project.has_worktree() && project.state == "done")
        .filter_map(|project| {
            let (head, branch) = orphaned_head(meta, project)?;
            let mut archived = Project {
                path: project.path.clone(),
                name: project.name.clone(),
                mode: project.mode.clone(),
                git_common_dir: project.git_common_dir.clone(),
                pinned: project.sha.clone(),
                head,
                branch,
                ..Default::default()
            };
            archived.referenced = archived.needs_ref();
            if archived.referenced {
                archived.bytes =
                    unique_bytes(&project.git_common_dir, &archived.head, &project.sha);
            }
            Some(archived)
        })
        .collect();
    record.bytes = record.projects.iter().map(|project| project.bytes).sum();
    let _ = fs::copy(
        meta.registry_entry().join(SNAPSHOT_FILE),
        directory.join(SNAPSHOT_FILE),
    );
    let finished = write_record(registry, &record).is_ok() && {
        record.projects.par_iter_mut().for_each(|project| {
            if project.referenced {
                project.referenced =
                    update_ref(&project.git_common_dir, &ref_name, &project.head).is_ok();
            }
        });
        record.complete = true;
        write_record(registry, &record).is_ok()
    };
    if finished {
        Some(record)
    } else {
        delete_refs(&record);
        let _ = fs::remove_dir_all(&directory);
        None
    }
}

fn delete_refs(record: &Record) {
    let ref_name = record.ref_name();
    record
        .projects
        .par_iter()
        .filter(|project| project.referenced)
        .for_each(|project| delete_ref(&project.git_common_dir, &ref_name));
    if let Some(manifests) = &record.manifests {
        delete_ref(&manifests.git_dir, &ref_name);
    }
}

/// Deletes an archive's refs, then its record. Call with the archive lock held.
pub fn drop_record(registry: &Path, record: &Record) -> AppResult<()> {
    delete_refs(record);
    let directory = record_dir(registry, &record.id);
    match fs::remove_dir_all(&directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io(
            format!("remove {}", directory.display()),
            error,
        )),
    }
}

pub fn policy(registry: &Path) -> Policy {
    let defaults = crate::defaults::load(registry).unwrap_or_default();
    Policy {
        expire_secs: defaults.archive_expire_secs.unwrap_or(DEFAULT_EXPIRE_SECS),
        budget_bytes: defaults
            .archive_budget_bytes
            .unwrap_or(DEFAULT_BUDGET_BYTES),
    }
}

/// Archives the policy expires: kept archives never; others once older than
/// the expiry, or, newest first, once the bytes kept alive exceed the budget.
/// The newest archive (the one a `remove` just made) is exempt from the
/// budget and not counted against it.
fn expired<'a>(records: &'a [Record], policy: &Policy, now: u64) -> Vec<&'a Record> {
    let mut total = 0u64;
    let mut expired = Vec::new();
    for (index, record) in records
        .iter()
        .enumerate()
        .filter(|(_, record)| !record.keep)
    {
        if now.saturating_sub(record.archived_unix) > policy.expire_secs {
            expired.push(record);
            continue;
        }
        if index == 0 {
            continue;
        }
        total = total.saturating_add(record.bytes);
        if total > policy.budget_bytes {
            expired.push(record);
        }
    }
    expired
}

/// Applies the retention policy and returns the expired archives' ids.
pub fn apply_retention(registry: &Path, dry_run: bool, quiet: bool) -> AppResult<Vec<String>> {
    if !dir(registry).is_dir() {
        return Ok(Vec::new());
    }
    let _held = lock(registry, quiet)?;
    let now = unix_now();
    if !dry_run {
        delete_abandoned(registry, now);
    }
    let records = load_all(registry);
    let policy = policy(registry);
    let mut ids = Vec::new();
    for record in expired(&records, &policy, now) {
        if !dry_run {
            drop_record(registry, record)?;
        }
        ids.push(record.id.clone());
    }
    Ok(ids)
}

pub fn parse_duration(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let split = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number = number
        .parse::<u64>()
        .map_err(|_| format!("invalid duration {text:?}; use e.g. 30m, 24h, 7d, or 2w"))?;
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "" | "d" => 86_400,
        "w" => 7 * 86_400,
        _ => {
            return Err(format!(
                "invalid duration {text:?}; use e.g. 30m, 24h, 7d, or 2w"
            ))
        }
    };
    Ok(number.saturating_mul(scale))
}

pub fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let split = text
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number = number
        .parse::<f64>()
        .map_err(|_| format!("invalid size {text:?}; use e.g. 500M or 2G"))?;
    let scale = match unit
        .trim()
        .to_ascii_uppercase()
        .trim_end_matches("IB")
        .trim_end_matches('B')
    {
        "" => 1u64,
        "K" => 1 << 10,
        "M" => 1 << 20,
        "G" => 1 << 30,
        "T" => 1 << 40,
        _ => return Err(format!("invalid size {text:?}; use e.g. 500M or 2G")),
    };
    Ok((number * scale as f64) as u64)
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn human_duration(seconds: u64) -> String {
    match seconds {
        0..=119 => format!("{seconds}s"),
        120..=7_199 => format!("{}m", seconds / 60),
        7_200..=172_799 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

fn summary(record: &Record, policy: &Policy) -> Value {
    json!({
        "id": record.id,
        "tree": record.tree,
        "path": record.path,
        "archived_at": record.archived_at,
        "reason": record.reason,
        "bytes": record.bytes,
        "projects": record.projects.len(),
        "projects_with_changes": record.projects_with_changes(),
        "keep": record.keep,
        "expires_at": (!record.keep)
            .then(|| format_unix(record.archived_unix.saturating_add(policy.expire_secs))),
        "dropped": record.dropped,
    })
}

fn diff_stat(project: &Project) -> Option<String> {
    let worktree = project.worktree.as_deref()?;
    let output = git_dir_command(&project.git_common_dir)
        .args(["diff", "--stat", &project.head, worktree])
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string()
    })
}

pub fn run(
    command: &ArchiveCommand,
    cwd: &Path,
    json_output: bool,
    quiet: bool,
) -> AppResult<Value> {
    let client = discover_client(cwd, true)?;
    let registry = registry_path(&client.root);
    let policy = policy(&registry);
    match command {
        ArchiveCommand::List => {
            let records = load_all(&registry);
            if !json_output {
                let now = unix_now();
                for record in &records {
                    println!(
                        "{}  {} ago  {} changed  {}  {}{}",
                        record.id,
                        human_duration(now.saturating_sub(record.archived_unix)),
                        record.projects_with_changes(),
                        human_bytes(record.bytes),
                        record.path,
                        if record.keep { "  (kept)" } else { "" }
                    );
                }
                if records.is_empty() {
                    println!("no archives");
                }
            }
            Ok(json!({
                "archives": records.iter().map(|record| summary(record, &policy)).collect::<Vec<_>>(),
                "policy": {"expire_secs": policy.expire_secs, "budget_bytes": policy.budget_bytes},
            }))
        }
        ArchiveCommand::Show(args) => {
            let record = select(&registry, &args.archive, cwd)?;
            let projects = record
                .projects
                .iter()
                .filter(|project| {
                    project.has_changes()
                        || project.branch.is_some()
                        || project.head != project.pinned
                })
                .map(|project| {
                    let stat = diff_stat(project);
                    if !json_output {
                        let position = match &project.branch {
                            Some(branch) => format!("on {branch}"),
                            None => format!(
                                "detached at {}",
                                &project.head[..project.head.len().min(12)]
                            ),
                        };
                        println!(
                            "{}  {position}  {} modified, {} untracked, {} unbranched commit(s)",
                            project.path,
                            project.modified,
                            project.untracked,
                            project.unbranched_commits
                        );
                        if let Some(stat) = &stat {
                            for line in stat.lines() {
                                println!("    {line}");
                            }
                        }
                    }
                    json!({
                        "path": project.path,
                        "head": project.head,
                        "branch": project.branch,
                        "modified": project.modified,
                        "untracked": project.untracked,
                        "unbranched_commits": project.unbranched_commits,
                        "bytes": project.bytes,
                        "stat": stat,
                    })
                })
                .collect::<Vec<_>>();
            if !json_output {
                for item in &record.dropped {
                    println!("dropped {} ({}: {})", item.project, item.kind, item.detail);
                }
                println!(
                    "{}: {} from {} at {}; {} project(s), {} with changes, {}",
                    record.id,
                    record.tree,
                    record.path,
                    record.archived_at,
                    record.projects.len(),
                    record.projects_with_changes(),
                    human_bytes(record.bytes)
                );
            }
            let mut value = summary(&record, &policy);
            value["projects"] = Value::Array(projects);
            Ok(value)
        }
        ArchiveCommand::Drop(args) => {
            let _held = lock(&registry, quiet)?;
            let mut dropped = Vec::new();
            for selector in &args.archives {
                let record = select(&registry, selector, cwd)?;
                drop_record(&registry, &record)?;
                if !quiet {
                    eprintln!("dropped {}", record.id);
                }
                dropped.push(record.id);
            }
            Ok(json!({ "dropped": dropped }))
        }
        ArchiveCommand::Keep(args) => {
            let _held = lock(&registry, quiet)?;
            let mut record = select(&registry, &args.archive, cwd)?;
            record.keep = !args.unkeep;
            write_record(&registry, &record)?;
            if !quiet {
                eprintln!(
                    "{} {}",
                    if record.keep {
                        "keeping"
                    } else {
                        "no longer keeping"
                    },
                    record.id
                );
            }
            Ok(summary(&record, &policy))
        }
        ArchiveCommand::Prune(args) => {
            let expired = apply_retention(&registry, args.dry_run, quiet)?;
            if !quiet {
                for id in &expired {
                    eprintln!(
                        "{} {id}",
                        if args.dry_run {
                            "would expire"
                        } else {
                            "expired"
                        }
                    );
                }
            }
            Ok(json!({ "expired": expired, "dry_run": args.dry_run }))
        }
        ArchiveCommand::Policy(args) => {
            let expire = args
                .expire
                .as_deref()
                .map(parse_duration)
                .transpose()
                .map_err(AppError::usage)?;
            let budget = args
                .budget
                .as_deref()
                .map(parse_size)
                .transpose()
                .map_err(AppError::usage)?;
            let policy = if expire.is_some() || budget.is_some() {
                crate::registry::with_registry_lock(
                    &client.root,
                    crate::registry::LockMode::Exclusive,
                    quiet,
                    |registry| {
                        let mut defaults = crate::defaults::load(registry)?;
                        if expire.is_some() {
                            defaults.archive_expire_secs = expire;
                        }
                        if budget.is_some() {
                            defaults.archive_budget_bytes = budget;
                        }
                        crate::defaults::save(registry, &defaults)?;
                        Ok(self::policy(registry))
                    },
                )?
            } else {
                policy
            };
            if !json_output {
                println!(
                    "archives expire after {} or beyond {} kept alive",
                    human_duration(policy.expire_secs),
                    human_bytes(policy.budget_bytes)
                );
            }
            Ok(json!({"expire_secs": policy.expire_secs, "budget_bytes": policy.budget_bytes}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, age: u64, bytes: u64, keep: bool) -> Record {
        Record {
            format_version: FORMAT_VERSION,
            id: id.into(),
            tree: "t".into(),
            path: "/t".into(),
            source_root: "/s".into(),
            archived_at: String::new(),
            archived_unix: 1_000_000 - age,
            tool_version: String::new(),
            reason: "remove".into(),
            keep,
            complete: true,
            options: Options::default(),
            manifests: None,
            projects: Vec::new(),
            bytes,
            dropped: Vec::new(),
        }
    }

    #[test]
    fn retention_expires_old_archives_then_the_oldest_beyond_the_budget() {
        let policy = Policy {
            expire_secs: 100,
            budget_bytes: 50,
        };
        let records = vec![
            record("new", 1, 30, false),
            record("kept", 2, 1_000, true),
            record("middle", 3, 30, false),
            record("older", 4, 10, false),
            record("old", 200, 1, false),
        ];
        let ids = |records: &[Record]| {
            expired(records, &policy, 1_000_000)
                .into_iter()
                .map(|record| record.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&records), ["old"]);
        let crowded = vec![
            record("new", 1, 30, false),
            record("middle", 3, 40, false),
            record("older", 4, 20, false),
        ];
        assert_eq!(ids(&crowded), ["older"]);
    }

    #[test]
    fn a_huge_newest_archive_neither_expires_itself_nor_pushes_out_older_ones() {
        let policy = Policy {
            expire_secs: 100,
            budget_bytes: 50,
        };
        let records = vec![
            record("huge", 1, 20_000_000_000, false),
            record("small", 2, 10, false),
        ];
        assert!(expired(&records, &policy, 1_000_000).is_empty());
    }

    #[test]
    fn durations_and_sizes_parse() {
        assert_eq!(parse_duration("24h"), Ok(86_400));
        assert_eq!(parse_duration("7d"), Ok(7 * 86_400));
        assert_eq!(parse_duration("30"), Ok(30 * 86_400));
        assert!(parse_duration("soon").is_err());
        assert_eq!(parse_size("2G"), Ok(2 << 30));
        assert_eq!(parse_size("500MiB"), Ok(500 << 20));
        assert_eq!(parse_size("1.5k"), Ok(1536));
        assert!(parse_size("lots").is_err());
    }
}
