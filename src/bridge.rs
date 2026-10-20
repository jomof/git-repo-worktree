//! Synthesized `.repo` bridge ("Model E1") that lets repo commands run inside a tree.
//!
//! For a legacy-layout source, each tree project's `.git` becomes a relative
//! symlink to `<tree>/.repo/projects/<path>.git`, which in turn links to the
//! project's Git worktree admin directory. repo therefore reads and writes the
//! same `HEAD` and `index` as Git, and `repo sync -l` accepts the link because
//! both paths resolve to one directory.

use crate::error::AppResult;
use crate::inspect::checkout_git_dir;
use crate::model::{BridgeMeta, ProjectMeta, TreeMeta, MODE_OVERLAY};
use crate::overlay;
use crate::util::{exit_reason, relative_path, write_atomic, write_meta};
use roxmltree::{Document, Node};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const STATE_DONE: &str = "done";
pub const STATE_UNSUPPORTED: &str = "unsupported";
const STATE_FAILED: &str = "failed";

/// Shared repository entries that repo reads through the project gitdir. Git
/// itself resolves these through `commondir` and ignores the links.
const ADMIN_LINKS: &[&str] = &[
    "config",
    "objects",
    "packed-refs",
    "hooks",
    "rr-cache",
    "shallow",
    "description",
];

/// Ref namespaces Git stores per worktree; they must stay private to the tree.
const PER_WORKTREE_REFS: &[&str] = &["bisect", "worktree", "rewritten"];

pub fn new_meta() -> BridgeMeta {
    BridgeMeta {
        manifests_git_dir: None,
        manifests_branch: None,
        state: "pending".into(),
        reason: None,
    }
}

enum BridgeError {
    Unsupported(String),
    Failed(String),
}

impl From<String> for BridgeError {
    fn from(reason: String) -> Self {
        BridgeError::Failed(reason)
    }
}

/// Builds or refreshes the bridge. Failures are recorded on the tree rather
/// than returned: the tree remains a usable git-only tree.
pub fn synthesize(meta: &mut TreeMeta, quiet: bool) -> AppResult<()> {
    let Some(mut bridge) = meta.repo_bridge.clone() else {
        return Ok(());
    };
    match build(meta) {
        Ok(Built {
            manifests,
            warnings,
        }) => {
            if !quiet {
                for warning in &warnings {
                    eprintln!("warning: left out of the .repo bridge: {warning}");
                }
            }
            bridge.state = STATE_DONE.into();
            bridge.reason = None;
            if let Some(manifests) = manifests {
                bridge.manifests_git_dir = Some(manifests.git_dir);
                bridge.manifests_branch = manifests.branch;
            }
        }
        Err(BridgeError::Unsupported(reason)) => {
            if !quiet {
                eprintln!("warning: repo commands will not work in this tree: {reason}");
            }
            bridge.state = STATE_UNSUPPORTED.into();
            bridge.reason = Some(reason);
        }
        Err(BridgeError::Failed(reason)) => {
            if !quiet {
                eprintln!("warning: .repo bridge failed: {reason}");
            }
            bridge.state = STATE_FAILED.into();
            bridge.reason = Some(reason);
        }
    }
    meta.repo_bridge = Some(bridge);
    write_meta(meta)
}

struct Manifests {
    git_dir: String,
    branch: Option<String>,
}

struct Built {
    manifests: Option<Manifests>,
    /// Projects left out of the bridge because their links could not be made.
    warnings: Vec<String>,
}

fn build(meta: &TreeMeta) -> Result<Built, BridgeError> {
    let root = meta.root();
    let source_repo = meta.source().join(".repo");
    if !source_repo.join("repo/main.py").is_file() {
        return Err(BridgeError::Unsupported(
            "the source client has no .repo/repo checkout".into(),
        ));
    }
    let snapshot_path = meta.registry_entry().join("snapshot.xml");
    let snapshot = fs::read_to_string(&snapshot_path)
        .map_err(|error| format!("read {}: {error}", snapshot_path.display()))?;
    let document =
        Document::parse(&snapshot).map_err(|error| format!("parse snapshot: {error}"))?;
    if document
        .descendants()
        .any(|node| node.is_element() && node.tag_name().name() == "submanifest")
    {
        return Err(BridgeError::Unsupported(
            "manifests with <submanifest> are not supported".into(),
        ));
    }

    let candidates = meta
        .projects
        .iter()
        .filter(|project| project.has_worktree() && project.state != "skipped")
        .filter(|project| owns_checkout(meta, project))
        .collect::<Vec<_>>();
    let expected = meta
        .projects
        .iter()
        .filter(|project| project.has_worktree() && project.state == "done")
        .count();
    if candidates.is_empty() && expected > 0 {
        return Err(BridgeError::Failed(format!(
            "none of the tree's {expected} worktree projects has a usable .git; the .repo bridge was left unchanged"
        )));
    }
    let tree_repo = root.join(".repo");
    fs::create_dir_all(&tree_repo)
        .map_err(|error| format!("create {}: {error}", tree_repo.display()))?;
    link(&tree_repo.join("repo"), &source_repo.join("repo"))?;
    let mut included = Vec::new();
    let mut warnings = Vec::new();
    if meta.source_layout == "worktree" {
        link_object_stores(
            &tree_repo.join("worktrees"),
            &source_repo.join("worktrees"),
            &candidates,
        )?;
        included = candidates;
    } else {
        for project in candidates {
            match link_project(meta, project) {
                Ok(()) => included.push(project),
                Err(reason) => warnings.push(reason),
            }
        }
        let objects = source_repo.join("project-objects");
        if objects.is_dir() {
            link_object_stores(&tree_repo.join("project-objects"), &objects, &included)?;
        }
    }
    let manifests = ensure_manifests(&root, &source_repo, &meta.name, &meta.source_layout)?;
    let previous = BridgeFiles::read(&tree_repo);

    let paths = included
        .iter()
        .map(|project| project.path.as_str())
        .collect::<HashSet<_>>();
    let manifest = match live_manifest(&source_repo) {
        Some(live) => {
            let excluded = meta
                .projects
                .iter()
                .filter(|project| !paths.contains(project.path.as_str()))
                .collect::<Vec<_>>();
            write_local_manifests(&tree_repo, &source_repo, &excluded)?;
            live
        }
        None => generate_manifest(&document, &paths),
    };
    write_atomic(&tree_repo.join("manifest.xml"), manifest.as_bytes(), 0o644)
        .map_err(|error| error.message)?;
    let mut sorted = paths.into_iter().collect::<Vec<_>>();
    sorted.sort_unstable();
    let mut list = sorted.join("\n");
    list.push('\n');
    write_atomic(&tree_repo.join("project.list"), list.as_bytes(), 0o644)
        .map_err(|error| error.message)?;
    record_copy_link_files(&tree_repo, meta)?;
    if let Err(reason) = self_test(&root) {
        previous.restore(&tree_repo);
        return Err(BridgeError::Failed(format!(
            "repo rejected the generated .repo ({reason}); the previous files were restored"
        )));
    }
    Ok(Built {
        manifests,
        warnings,
    })
}

/// Adds the tree's created `copyfile` and `linkfile` destinations to repo's
/// tracking file, keeping entries repo wrote itself, so a directive later
/// dropped from the manifest is removed by `repo sync` as in any client.
fn record_copy_link_files(tree_repo: &Path, meta: &TreeMeta) -> Result<(), String> {
    let path = tree_repo.join("copy-link-files.json");
    let mut tracked = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    for (key, directives) in [("copyfile", &meta.copyfiles), ("linkfile", &meta.linkfiles)] {
        let mut dests = tracked
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for directive in directives
            .iter()
            .filter(|directive| directive.state == "done")
        {
            if !dests.contains(&directive.dest) {
                dests.push(directive.dest.clone());
            }
        }
        tracked[key] = serde_json::json!(dests);
    }
    let bytes = serde_json::to_vec(&tracked)
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    write_atomic(&path, &bytes, 0o644).map_err(|error| error.message)
}

/// Whether the tree's checkout of `project` is a linked worktree of the
/// project's repository, checked on disk rather than from recorded state.
fn owns_checkout(meta: &TreeMeta, project: &ProjectMeta) -> bool {
    let dot_git = dot_git_path(meta, project);
    let checkout = meta.root().join(&project.path);
    let admin = match fs::symlink_metadata(&dot_git) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let projects_link = meta
                .root()
                .join(".repo/projects")
                .join(format!("{}.git", project.path));
            fs::canonicalize(&projects_link).or_else(|_| fs::canonicalize(checkout.join(".git")))
        }
        Ok(metadata) if metadata.is_file() => {
            gitfile_target(&dot_git, &checkout).map_err(io::Error::other)
        }
        _ => return false,
    };
    let common = fs::canonicalize(&project.git_common_dir);
    match (admin, common) {
        (Ok(admin), Ok(common)) => admin.parent() == Some(common.join("worktrees").as_path()),
        _ => false,
    }
}

const LOCAL_MANIFEST_PATH: &str = "local_manifests/zz-git-repo-worktree.xml";

/// The bridge files repo parses, kept so a rejected rewrite can be undone.
struct BridgeFiles(Vec<(&'static str, Option<Vec<u8>>)>);

impl BridgeFiles {
    fn read(tree_repo: &Path) -> Self {
        Self(
            ["manifest.xml", "project.list", LOCAL_MANIFEST_PATH]
                .into_iter()
                .map(|name| (name, fs::read(tree_repo.join(name)).ok()))
                .collect(),
        )
    }

    fn restore(&self, tree_repo: &Path) {
        for (name, contents) in &self.0 {
            let path = tree_repo.join(name);
            match contents {
                Some(contents) => {
                    let _ = write_atomic(&path, contents, 0o644);
                }
                None => {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }
}

/// Runs `repo list -n` in the tree, which parses the manifest exactly as every
/// repo command does. Skipped when repo is not on PATH.
fn self_test(root: &Path) -> Result<(), String> {
    let output = match Command::new("repo")
        .current_dir(root)
        .args(["list", "-n"])
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("run repo list: {error}")),
    };
    if output.status.success() {
        Ok(())
    } else {
        Err(exit_reason(&output))
    }
}

/// Links each included project's object store individually rather than the
/// whole source directory. `repo gc` treats every store in this directory that
/// the tree's manifest does not list as unused and deletes it, and `repo wipe`
/// deletes a project's store; exposing only the tree's own stores, as links,
/// keeps both from reaching the source's other projects.
fn link_object_stores(
    directory: &Path,
    source: &Path,
    included: &[&ProjectMeta],
) -> Result<(), String> {
    if fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        fs::remove_file(directory)
            .map_err(|error| format!("replace {}: {error}", directory.display()))?;
    }
    fs::create_dir_all(directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    for project in included {
        let name = format!("{}.git", project.name);
        let target = source.join(&name);
        if !target.is_dir() {
            continue;
        }
        let path = directory.join(&name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        link_if_not_real(&path, &target)?;
    }
    Ok(())
}

/// Path of a project's `.git` entry as stored on disk. Overlay projects keep it
/// in the overlay's upper directory, which is reachable even while unmounted.
pub fn dot_git_path(meta: &TreeMeta, project: &ProjectMeta) -> PathBuf {
    let checkout = meta.root().join(&project.path);
    if project.mode == MODE_OVERLAY && !overlay::is_mounted(&checkout) {
        overlay::upper_dir(&meta.registry_entry(), &project.path).join(".git")
    } else {
        checkout.join(".git")
    }
}

fn link_project(meta: &TreeMeta, project: &ProjectMeta) -> Result<(), String> {
    let root = meta.root();
    let checkout = root.join(&project.path);
    let dot_git = dot_git_path(meta, project);
    let projects_link = root
        .join(".repo/projects")
        .join(format!("{}.git", project.path));
    let admin = match fs::symlink_metadata(&dot_git) {
        Ok(metadata) if metadata.file_type().is_symlink() => fs::canonicalize(&projects_link)
            .or_else(|_| fs::canonicalize(checkout.join(".git")))
            .map_err(|error| format!("{}: resolve .git link: {error}", project.path))?,
        Ok(metadata) if metadata.is_file() => gitfile_target(&dot_git, &checkout)
            .map_err(|error| format!("{}: {error}", project.path))?,
        Ok(_) => return Err(format!("{}: .git is not a gitfile or link", project.path)),
        Err(error) => return Err(format!("{}: inspect .git: {error}", project.path)),
    };
    let common = fs::canonicalize(&project.git_common_dir)
        .map_err(|error| format!("{}: resolve common dir: {error}", project.path))?;
    if admin.parent() != Some(common.join("worktrees").as_path()) {
        return Err(format!(
            "{}: {} is not a worktree of {}",
            project.path,
            admin.display(),
            common.display()
        ));
    }
    augment_admin(&admin, &common)?;
    if let Some(parent) = projects_link.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    link(&projects_link, &admin)?;
    let target = relative_path(&checkout, &projects_link);
    replace_with_symlink(&dot_git, &target)
}

fn gitfile_target(dot_git: &Path, checkout: &Path) -> Result<PathBuf, String> {
    let contents = fs::read_to_string(dot_git)
        .map_err(|error| format!("read {}: {error}", dot_git.display()))?;
    let target = contents
        .strip_prefix("gitdir:")
        .map(str::trim)
        .ok_or_else(|| format!("{} is not a gitfile", dot_git.display()))?;
    let target = Path::new(target);
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        checkout.join(target)
    };
    fs::canonicalize(&absolute).map_err(|error| format!("resolve {}: {error}", absolute.display()))
}

/// Adds links for the shared entries repo reads directly from a gitdir.
fn augment_admin(admin: &Path, common: &Path) -> Result<(), String> {
    for name in ADMIN_LINKS {
        if *name == "packed-refs" || fs::symlink_metadata(common.join(name)).is_ok() {
            link_if_not_real(&admin.join(name), &Path::new("../..").join(name))?;
        }
    }
    if common.join("reftable").exists() {
        return Ok(());
    }
    let refs = admin.join("refs");
    match fs::symlink_metadata(&refs) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(&refs)
                .map_err(|error| format!("replace {}: {error}", refs.display()))?;
        }
        _ => {}
    }
    fs::create_dir_all(&refs).map_err(|error| format!("create {}: {error}", refs.display()))?;
    let _ = fs::create_dir_all(common.join("refs/published"));
    let entries = fs::read_dir(common.join("refs"))
        .map_err(|error| format!("read {}: {error}", common.join("refs").display()))?;
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if PER_WORKTREE_REFS.contains(&name) || !entry.path().is_dir() {
            continue;
        }
        link_if_not_real(&refs.join(name), &Path::new("../../../refs").join(name))?;
    }
    Ok(())
}

/// Creates or corrects a symlink, leaving any real file or directory alone.
fn link_if_not_real(path: &Path, target: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if fs::read_link(path).ok().as_deref() == Some(target) {
                return Ok(());
            }
            fs::remove_file(path)
                .map_err(|error| format!("replace {}: {error}", path.display()))?;
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect {}: {error}", path.display())),
    }
    symlink(target, path).map_err(|error| format!("link {}: {error}", path.display()))
}

/// Creates or corrects a symlink, refusing to replace real data.
fn link(path: &Path, target: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if fs::read_link(path).ok().as_deref() == Some(target) {
                return Ok(());
            }
            fs::remove_file(path)
                .map_err(|error| format!("replace {}: {error}", path.display()))?;
        }
        Ok(_) => return Err(format!("{} exists and is not a link", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect {}: {error}", path.display())),
    }
    symlink(target, path).map_err(|error| format!("link {}: {error}", path.display()))
}

fn replace_with_symlink(path: &Path, target: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() && fs::read_link(path).ok().as_deref() == Some(target)
        {
            return Ok(());
        }
    }
    let temporary = path.with_file_name(format!(".git.repo-worktree-{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    symlink(target, &temporary)
        .map_err(|error| format!("link {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("replace {}: {error}", path.display())
    })
}

fn replace_with_file(path: &Path, contents: &str) -> Result<(), String> {
    let temporary = path.with_file_name(format!(".git.repo-worktree-{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    fs::write(&temporary, contents)
        .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("replace {}: {error}", path.display())
    })
}

/// Git's worktree `remove`, `move`, and `repair` require `.git` to be a
/// gitfile. Converts a bridge symlink back to one, returning the link target
/// so [`restore_link`] can reinstate it.
pub fn to_gitfile(dot_git: &Path) -> Result<Option<PathBuf>, String> {
    let Ok(metadata) = fs::symlink_metadata(dot_git) else {
        return Ok(None);
    };
    if !metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let target = fs::read_link(dot_git).map_err(|error| format!("readlink: {error}"))?;
    let admin = fs::canonicalize(dot_git).map_err(|error| format!("resolve .git: {error}"))?;
    replace_with_file(dot_git, &format!("gitdir: {}\n", admin.display()))?;
    Ok(Some(target))
}

pub fn restore_link(dot_git: &Path, target: Option<PathBuf>) -> Result<(), String> {
    match target {
        Some(target) if dot_git.is_file() => replace_with_symlink(dot_git, &target),
        _ => Ok(()),
    }
}

fn git_text(args: &[&str], directory: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !text.is_empty()).then_some(text)
}

fn manifests_branch_name(tree_name: &str) -> String {
    let mut name = tree_name.trim_start_matches('.').replace("..", "-");
    while name.ends_with(".lock") {
        name.truncate(name.len() - ".lock".len());
    }
    if name.is_empty() {
        name.push_str("tree");
    }
    format!("repo-worktree/{name}")
}

/// Gives the tree its own manifest checkout: a linked worktree of the source
/// manifest repository on a per-tree branch that tracks the same upstream as
/// the source's manifest branch. repo reads the manifest revision from that
/// tracking configuration, and manifest updates stay inside the tree.
fn ensure_manifests(
    root: &Path,
    source_repo: &Path,
    tree_name: &str,
    source_layout: &str,
) -> Result<Option<Manifests>, String> {
    let tree_repo = root.join(".repo");
    let source_git = source_repo.join("manifests.git");
    let source_checkout = source_repo.join("manifests");
    let checkout = tree_repo.join("manifests");
    if !source_git.is_dir() {
        link(&checkout, &source_checkout)?;
        return Ok(None);
    }
    let common = fs::canonicalize(&source_git)
        .map_err(|error| format!("resolve {}: {error}", source_git.display()))?;
    let source_branch = git_text(&["symbolic-ref", "-q", "--short", "HEAD"], &source_checkout);
    let tracking = source_branch.as_ref().and_then(|branch| {
        Some((
            git_text(
                &["config", &format!("branch.{branch}.remote")],
                &source_checkout,
            )?,
            git_text(
                &["config", &format!("branch.{branch}.merge")],
                &source_checkout,
            )?,
        ))
    });
    let branch = tracking.as_ref().map(|_| manifests_branch_name(tree_name));
    if fs::symlink_metadata(&checkout).is_err() {
        let head = git_text(&["rev-parse", "HEAD"], &source_checkout)
            .ok_or_else(|| "cannot resolve the source manifest HEAD".to_string())?;
        let added = crate::util::worktree_add(|_| {
            let mut command = Command::new("git");
            command
                .arg(format!("--git-dir={}", common.display()))
                .args(["worktree", "add"]);
            match &branch {
                Some(branch) => command.arg("-B").arg(branch),
                None => command.arg("--detach"),
            };
            command.arg(&checkout).arg(&head);
            command
        })
        .map_err(|error| format!("add manifest worktree: {error}"))?;
        if !added.status.success() {
            return Err(format!("add manifest worktree: {}", exit_reason(&added)));
        }
    }
    if let (Some(branch), Some((remote, merge))) = (&branch, &tracking) {
        let scope = [std::ffi::OsString::from(format!(
            "--git-dir={}",
            common.display()
        ))];
        for (key, value) in [("remote", remote), ("merge", merge)] {
            crate::util::set_git_config(&scope, &format!("branch.{branch}.{key}"), value)
                .map_err(|reason| format!("configure manifest branch: {reason}"))?;
        }
    }
    let admin = checkout_git_dir(&checkout)
        .ok_or_else(|| format!("{} has no usable .git", checkout.display()))?;
    if admin.parent() != Some(common.join("worktrees").as_path()) {
        return Err(format!(
            "{} is not a worktree of {}",
            checkout.display(),
            common.display()
        ));
    }
    let manifests_git = tree_repo.join("manifests.git");
    if source_layout == "worktree" {
        // As in the source: repo reads the checkout's HEAD through its gitfile
        // and the shared repository through `manifests.git`.
        let dot_git = checkout.join(".git");
        if fs::symlink_metadata(&dot_git).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            to_gitfile(&dot_git)?;
        }
        link(&manifests_git, &common)?;
    } else {
        augment_admin(&admin, &common)?;
        link(&manifests_git, &admin)?;
        replace_with_symlink(&checkout.join(".git"), Path::new("../manifests.git"))?;
    }
    Ok(Some(Manifests {
        git_dir: common.to_string_lossy().into_owned(),
        branch,
    }))
}

/// Removes the tree's manifest worktree registration before the tree is deleted.
pub fn remove_manifests(meta: &TreeMeta) {
    let Some(git_dir) = meta
        .repo_bridge
        .as_ref()
        .and_then(|bridge| bridge.manifests_git_dir.as_deref())
    else {
        return;
    };
    let checkout = meta.root().join(".repo/manifests");
    if fs::symlink_metadata(&checkout).is_ok() {
        let dot_git = checkout.join(".git");
        let _ = to_gitfile(&dot_git);
        let _ = Command::new("git")
            .arg(format!("--git-dir={git_dir}"))
            .args(["worktree", "remove", "--force", "--force"])
            .arg(&checkout)
            .output();
    }
    let _ = Command::new("git")
        .arg(format!("--git-dir={git_dir}"))
        .args(["worktree", "prune"])
        .output();
    if let Some(branch) = meta
        .repo_bridge
        .as_ref()
        .and_then(|bridge| bridge.manifests_branch.as_deref())
    {
        let _ = Command::new("git")
            .arg(format!("--git-dir={git_dir}"))
            .args(["branch", "-D", branch])
            .output();
    }
}

/// Re-registers the manifest worktree after the tree moved.
pub fn repair_manifests(meta: &TreeMeta) -> Result<(), String> {
    let Some(git_dir) = meta
        .repo_bridge
        .as_ref()
        .and_then(|bridge| bridge.manifests_git_dir.as_deref())
    else {
        return Ok(());
    };
    let checkout = meta.root().join(".repo/manifests");
    if !checkout.is_dir() {
        return Ok(());
    }
    repair_worktree_links(&checkout, Path::new(git_dir))
        .map_err(|reason| format!("repair manifest worktree: {reason}"))
}

/// Points the linked worktree at `checkout` and its registration in `common`
/// back at each other, as `git worktree repair <checkout>` does. Git's command
/// also checks every other worktree of the repository and fails on other
/// trees' bridge links, so this touches only `checkout`.
pub fn repair_worktree_links(checkout: &Path, common: &Path) -> Result<(), String> {
    let dot_git = checkout.join(".git");
    let metadata =
        fs::symlink_metadata(&dot_git).map_err(|error| format!("inspect .git: {error}"))?;
    let admin = if metadata.file_type().is_symlink() {
        fs::canonicalize(&dot_git).map_err(|error| format!("resolve .git link: {error}"))?
    } else if metadata.is_file() {
        gitfile_target(&dot_git, checkout)?
    } else {
        return Err(".git is a directory, not a linked worktree".into());
    };
    let common = fs::canonicalize(common)
        .map_err(|error| format!("resolve {}: {error}", common.display()))?;
    if admin.parent() != Some(common.join("worktrees").as_path()) {
        return Err(format!(
            "{} is not a worktree of {}",
            admin.display(),
            common.display()
        ));
    }
    let checkout = fs::canonicalize(checkout)
        .map_err(|error| format!("resolve {}: {error}", checkout.display()))?;
    let expected = format!("{}\n", checkout.join(".git").display());
    let gitdir = admin.join("gitdir");
    if fs::read_to_string(&gitdir).ok().as_deref() != Some(expected.as_str()) {
        write_atomic(&gitdir, expected.as_bytes(), 0o644).map_err(|error| error.message)?;
    }
    Ok(())
}

/// The source client's manifest selection, so repo in the tree follows the
/// same manifest as the source: `repo sync` moves a tree to upstream exactly as
/// it would a client. The tree was still created at the source's checked-out
/// commits. Returns `None` when the source has no readable `manifest.xml`.
fn live_manifest(source_repo: &Path) -> Option<String> {
    let path = source_repo.join("manifest.xml");
    let metadata = fs::symlink_metadata(&path).ok()?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(&path).ok()?;
        let name = target
            .strip_prefix("manifests")
            .unwrap_or(&target)
            .to_string_lossy()
            .into_owned();
        return Some(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<manifest>\n  <include name=\"{}\" />\n</manifest>\n",
            escape_xml(&name)
        ));
    }
    fs::read_to_string(&path).ok()
}

const LOCAL_MANIFEST: &str = "zz-git-repo-worktree.xml";

/// Mirrors the source's local manifests into the tree and adds one, applied
/// last, that removes every project the tree does not own: shared projects
/// belong to the source, and repo would otherwise replace their `.git`.
fn write_local_manifests(
    tree_repo: &Path,
    source_repo: &Path,
    excluded: &[&ProjectMeta],
) -> Result<(), String> {
    let directory = tree_repo.join("local_manifests");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let source_directory = source_repo.join("local_manifests");
    let mut mirrored = HashSet::new();
    if let Ok(entries) = fs::read_dir(&source_directory) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            if name == LOCAL_MANIFEST || !name.to_string_lossy().ends_with(".xml") {
                continue;
            }
            link(&directory.join(&name), &entry.path())?;
            mirrored.insert(name);
        }
    }
    if let Ok(entries) = fs::read_dir(&directory) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            let is_link = entry
                .file_type()
                .map(|kind| kind.is_symlink())
                .unwrap_or(false);
            if is_link && !mirrored.contains(&name) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!-- Generated by git-repo-worktree: projects this tree does not own. -->\n");
    out.push_str("<manifest>\n");
    for project in excluded {
        out.push_str(&format!(
            "  <remove-project name=\"{}\" path=\"{}\" optional=\"true\" />\n",
            escape_xml(&project.name),
            escape_xml(&project.path)
        ));
    }
    out.push_str("</manifest>\n");
    write_atomic(&directory.join(LOCAL_MANIFEST), out.as_bytes(), 0o644)
        .map_err(|error| error.message)
}

/// Serializes the pinned snapshot for repo, keeping only the tree's own
/// worktree projects. Shared and skipped projects must be absent: repo would
/// otherwise try to "fix" their `.git`, which belongs to the source client.
fn generate_manifest(document: &Document<'_>, included: &HashSet<&str>) -> String {
    let root = document.root_element();
    let names = root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "project")
        .filter(|node| included.contains(project_path(*node)))
        .filter_map(|node| node.attribute("name"))
        .collect::<HashSet<_>>();
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!-- Generated by git-repo-worktree from the tree's pinned snapshot. -->\n");
    out.push_str("<manifest>\n");
    for node in root.children().filter(|node| node.is_element()) {
        let keep = match node.tag_name().name() {
            "project" => included.contains(project_path(node)),
            "extend-project" => node
                .attribute("name")
                .is_some_and(|name| names.contains(name)),
            "repo-hooks" => node
                .attribute("in-project")
                .is_some_and(|name| names.contains(name)),
            "superproject" | "remove-project" | "include" | "submanifest" => false,
            _ => true,
        };
        if keep {
            write_element(&mut out, node, 1);
        }
    }
    out.push_str("</manifest>\n");
    out
}

fn project_path<'a>(node: Node<'a, '_>) -> &'a str {
    node.attribute("path")
        .or_else(|| node.attribute("name"))
        .unwrap_or("")
}

fn write_element(out: &mut String, node: Node<'_, '_>, depth: usize) {
    let indent = "  ".repeat(depth);
    out.push_str(&indent);
    out.push('<');
    out.push_str(node.tag_name().name());
    for attribute in node.attributes() {
        out.push(' ');
        out.push_str(attribute.name());
        out.push_str("=\"");
        out.push_str(&escape_xml(attribute.value()));
        out.push('"');
    }
    let children = node
        .children()
        .filter(|child| child.is_element())
        .collect::<Vec<_>>();
    let text = node
        .children()
        .filter(|child| child.is_text())
        .filter_map(|child| child.text())
        .collect::<String>();
    let text = text.trim();
    if children.is_empty() && text.is_empty() {
        out.push_str("/>\n");
        return;
    }
    out.push('>');
    if !text.is_empty() {
        out.push_str(&escape_xml(text));
    }
    if !children.is_empty() {
        out.push('\n');
        for child in children {
            write_element(out, child, depth + 1);
        }
        out.push_str(&indent);
    }
    out.push_str("</");
    out.push_str(node.tag_name().name());
    out.push_str(">\n");
}

fn escape_xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_manifest_keeps_only_tree_projects_and_their_references() {
        let xml = r#"<manifest>
  <remote name="goog" fetch=".."/>
  <default remote="goog" revision="main"/>
  <superproject name="super" remote="goog"/>
  <notice>Read &amp; follow</notice>
  <project name="a" path="pa" revision="1"><linkfile src="x" dest="y"/></project>
  <project name="b" path="pb" revision="2"/>
  <repo-hooks in-project="b" enabled-list="pre-upload"/>
  <extend-project name="a"/>
</manifest>"#;
        let document = Document::parse(xml).unwrap();
        let included = ["pa"].into_iter().collect::<HashSet<_>>();
        let output = generate_manifest(&document, &included);
        assert!(output.contains(r#"<project name="a" path="pa" revision="1">"#));
        assert!(output.contains(r#"<linkfile src="x" dest="y"/>"#));
        assert!(output.contains(r#"<default remote="goog" revision="main"/>"#));
        assert!(output.contains("<notice>Read &amp; follow</notice>"));
        assert!(output.contains(r#"<extend-project name="a"/>"#));
        assert!(!output.contains("pb"));
        assert!(!output.contains("repo-hooks"));
        assert!(!output.contains("superproject"));
        Document::parse(&output).unwrap();
    }
}
