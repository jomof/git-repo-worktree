//! Copy-on-write overlay projects (Linux, `fuse-overlayfs`).
//!
//! An overlay project is a real linked Git worktree whose files are a
//! `fuse-overlayfs` view of the clean source checkout. Writes land in a
//! tool-owned upper directory, so the source checkout is never modified. The
//! worktree's `.git` lives in the upper directory and shadows the source's.

use crate::util::{exit_reason, normalize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const STRATEGY: &str = "fuse-overlayfs";

pub fn overlay_dir(entry: &Path, project_path: &str) -> PathBuf {
    entry
        .join("overlays")
        .join(project_path.replace('%', "%25").replace('/', "%2F"))
}

pub fn upper_dir(entry: &Path, project_path: &str) -> PathBuf {
    overlay_dir(entry, project_path).join("upper")
}

fn work_dir(entry: &Path, project_path: &str) -> PathBuf {
    overlay_dir(entry, project_path).join("work")
}

fn find_program(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|directory| {
        names
            .iter()
            .map(|name| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// Explains why overlay mode cannot be used on this machine, if it cannot.
pub fn unavailable_reason() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return Some("overlay mode requires Linux".into());
    }
    if find_program(&["fuse-overlayfs"]).is_none() {
        return Some("fuse-overlayfs is not on PATH".into());
    }
    if find_program(&["fusermount3", "fusermount"]).is_none() {
        return Some("fusermount3 is not on PATH".into());
    }
    if !Path::new("/dev/fuse").exists() {
        return Some("/dev/fuse is not available".into());
    }
    None
}

#[cfg(target_os = "linux")]
fn unescape_mountinfo(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&field[index + 1..index + 4], 8) {
                out.push(value);
                index += 4;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(target_os = "linux")]
pub fn is_mounted(path: &Path) -> bool {
    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    let wanted = [
        normalize(path),
        path.parent()
            .and_then(|parent| fs::canonicalize(parent).ok())
            .zip(path.file_name())
            .map(|(parent, name)| parent.join(name))
            .unwrap_or_else(|| normalize(path)),
    ];
    mountinfo.lines().any(|line| {
        line.split(' ')
            .nth(4)
            .map(|field| PathBuf::from(unescape_mountinfo(field)))
            .is_some_and(|mount_point| wanted.contains(&mount_point))
    })
}

#[cfg(not(target_os = "linux"))]
pub fn is_mounted(_path: &Path) -> bool {
    false
}

/// Whether anything besides the worktree's `.git` was ever written, created,
/// or deleted in the overlay. Answers without touching the lower directory.
pub fn upper_has_changes(upper: &Path) -> Result<bool, String> {
    match fs::read_dir(upper) {
        Ok(entries) => Ok(entries
            .filter_map(Result::ok)
            .any(|entry| entry.file_name() != ".git")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("read {}: {error}", upper.display())),
    }
}

/// Resolves the worktree admin directory from the `.git` stored in `upper`.
/// A bridge symlink is relative to the mount point, not to `upper`.
pub fn admin_dir(upper: &Path, mount_point: &Path) -> Option<PathBuf> {
    let dot_git = upper.join(".git");
    let metadata = fs::symlink_metadata(&dot_git).ok()?;
    let target = if metadata.file_type().is_symlink() {
        fs::read_link(&dot_git).ok()?
    } else {
        let contents = fs::read_to_string(&dot_git).ok()?;
        PathBuf::from(contents.strip_prefix("gitdir:")?.trim())
    };
    let absolute = if target.is_absolute() {
        target
    } else {
        normalize(&mount_point.join(target))
    };
    fs::canonicalize(absolute).ok()
}

pub fn admin_head(upper: &Path, mount_point: &Path) -> Option<String> {
    let admin = admin_dir(upper, mount_point)?;
    let output = Command::new("git")
        .arg(format!("--git-dir={}", admin.display()))
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn mount(lower: &Path, upper: &Path, work: &Path, mount_point: &Path) -> Result<(), String> {
    for path in [lower, upper, work] {
        let text = path.to_string_lossy();
        if text.contains(',') || text.contains(':') {
            return Err(format!(
                "overlay paths may not contain ',' or ':': {}",
                path.display()
            ));
        }
    }
    fs::create_dir_all(mount_point)
        .map_err(|error| format!("create {}: {error}", mount_point.display()))?;
    let output = Command::new("fuse-overlayfs")
        .arg("-o")
        .arg(format!(
            "lowerdir={},upperdir={},workdir={}",
            lower.display(),
            upper.display(),
            work.display()
        ))
        .arg(mount_point)
        .output()
        .map_err(|error| format!("run fuse-overlayfs: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("fuse-overlayfs: {}", exit_reason(&output)))
    }
}

pub fn unmount(mount_point: &Path, lazy: bool) -> Result<(), String> {
    if !is_mounted(mount_point) {
        return Ok(());
    }
    let program = find_program(&["fusermount3", "fusermount"])
        .ok_or_else(|| "fusermount3 is not on PATH".to_string())?;
    let mut command = Command::new(program);
    command
        .arg(if lazy { "-uz" } else { "-u" })
        .arg(mount_point);
    let output = command
        .output()
        .map_err(|error| format!("run fusermount: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "unmount {}: {}",
            mount_point.display(),
            exit_reason(&output)
        ))
    }
}

/// Creates (or resumes creating) an overlay project at `checkout`. The source
/// checkout must already be verified clean at `sha`.
pub fn populate(
    source_checkout: &Path,
    checkout: &Path,
    entry: &Path,
    project_path: &str,
    sha: &str,
) -> Result<(), String> {
    if let Some(reason) = unavailable_reason() {
        return Err(reason);
    }
    let upper = upper_dir(entry, project_path);
    let work = work_dir(entry, project_path);
    if is_mounted(checkout) {
        return if fs::symlink_metadata(upper.join(".git")).is_ok() {
            Ok(())
        } else {
            Err("dest-occupied".into())
        };
    }
    fs::create_dir_all(&upper).map_err(|error| format!("create {}: {error}", upper.display()))?;
    fs::create_dir_all(&work).map_err(|error| format!("create {}: {error}", work.display()))?;
    if fs::symlink_metadata(upper.join(".git")).is_err() {
        register(source_checkout, checkout, sha)?;
        let dot_git = checkout.join(".git");
        let contents = fs::read_to_string(&dot_git)
            .map_err(|error| format!("read {}: {error}", dot_git.display()))?;
        let admin = admin_from_gitfile(&contents, checkout)?;
        seed_index(source_checkout, &admin, sha)?;
        fs::write(upper.join(".git"), &contents)
            .map_err(|error| format!("write overlay .git: {error}"))?;
        fs::remove_file(&dot_git).map_err(|error| format!("move .git into overlay: {error}"))?;
    } else if checkout.join(".git").is_file() {
        fs::remove_file(checkout.join(".git"))
            .map_err(|error| format!("remove stale .git: {error}"))?;
    }
    if fs::read_dir(checkout)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
    {
        return Err("dest-occupied".into());
    }
    mount(source_checkout, &upper, &work, checkout)?;
    let finish = || -> Result<(), String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["update-index", "--no-fsmonitor", "--no-untracked-cache"])
            .output()
            .map_err(|error| format!("run git update-index: {error}"))?;
        if !output.status.success() {
            return Err(format!("prepare overlay index: {}", exit_reason(&output)));
        }
        let head = Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["rev-parse", "HEAD"])
            .output()
            .map_err(|error| format!("inspect overlay HEAD: {error}"))?;
        if String::from_utf8_lossy(&head.stdout).trim() != sha {
            return Err("overlay HEAD differs from pinned SHA".into());
        }
        Ok(())
    };
    finish().map_err(|error| {
        let _ = unmount(checkout, true);
        error
    })
}

fn register(source_checkout: &Path, checkout: &Path, sha: &str) -> Result<(), String> {
    if checkout.join(".git").is_file() {
        return Ok(());
    }
    if fs::read_dir(checkout)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
    {
        return Err("dest-occupied".into());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(source_checkout)
        .args(["worktree", "add", "--no-checkout", "--detach"])
        .arg(checkout)
        .arg(sha)
        .output()
        .map_err(|error| format!("run git worktree add --no-checkout: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(exit_reason(&output))
    }
}

fn admin_from_gitfile(contents: &str, checkout: &Path) -> Result<PathBuf, String> {
    let target = contents
        .strip_prefix("gitdir:")
        .map(str::trim)
        .ok_or_else(|| "worktree .git is not a gitfile".to_string())?;
    let target = Path::new(target);
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        checkout.join(target)
    };
    fs::canonicalize(&absolute).map_err(|error| format!("resolve {}: {error}", absolute.display()))
}

/// Gives the new worktree the source checkout's index. The overlay presents
/// the source files with identical stat data, so Git trusts every entry
/// without reading file contents. Split indexes cannot be copied; they fall
/// back to a stat-less index that Git refreshes on first use.
fn seed_index(source_checkout: &Path, admin: &Path, sha: &str) -> Result<(), String> {
    let split = Command::new("git")
        .arg("-C")
        .arg(source_checkout)
        .args(["config", "--bool", "core.splitIndex"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "true")
        .unwrap_or(false);
    if !split {
        let output = Command::new("git")
            .arg("-C")
            .arg(source_checkout)
            .args(["rev-parse", "--path-format=absolute", "--git-path", "index"])
            .output()
            .map_err(|error| format!("locate source index: {error}"))?;
        let index = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        if output.status.success() && index.is_file() {
            fs::copy(&index, admin.join("index"))
                .map_err(|error| format!("copy source index: {error}"))?;
            return Ok(());
        }
    }
    let output = Command::new("git")
        .arg(format!("--git-dir={}", admin.display()))
        .args(["read-tree", sha])
        .output()
        .map_err(|error| format!("initialize overlay index: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "initialize overlay index: {}",
            exit_reason(&output)
        ))
    }
}

/// Re-mounts an overlay after a reboot or a tree move.
pub fn remount(
    source_checkout: &Path,
    checkout: &Path,
    entry: &Path,
    project_path: &str,
) -> Result<(), String> {
    if is_mounted(checkout) {
        return Ok(());
    }
    if let Some(reason) = unavailable_reason() {
        return Err(reason);
    }
    let upper = upper_dir(entry, project_path);
    if fs::symlink_metadata(upper.join(".git")).is_err() {
        return Err("overlay upper directory is missing its .git".into());
    }
    mount(
        source_checkout,
        &upper,
        &work_dir(entry, project_path),
        checkout,
    )
}

/// Unmounts an overlay project and clears any worktree lock so the
/// registration can be pruned once the tree is deleted.
pub fn release(
    checkout: &Path,
    entry: &Path,
    project_path: &str,
    lazy: bool,
) -> Result<(), String> {
    unmount(checkout, lazy)?;
    if let Some(admin) = admin_dir(&upper_dir(entry, project_path), checkout) {
        let _ = fs::remove_file(admin.join("locked"));
    }
    Ok(())
}

/// Converts a mounted, Git-clean overlay into an ordinary checkout of its HEAD.
/// Ignored files that exist only in the overlay are discarded.
pub fn materialize(checkout: &Path, entry: &Path, project_path: &str) -> Result<(), String> {
    let upper = upper_dir(entry, project_path);
    if is_mounted(checkout) {
        let status = Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["status", "--porcelain", "--untracked-files=normal"])
            .output()
            .map_err(|error| format!("inspect overlay: {error}"))?;
        if !status.status.success() {
            return Err(format!("inspect overlay: {}", exit_reason(&status)));
        }
        if !status.stdout.is_empty() {
            return Err(
                "overlay has uncommitted or untracked changes; commit or discard them first".into(),
            );
        }
        unmount(checkout, false)?;
    }
    let dot_git = upper.join(".git");
    let target = checkout.join(".git");
    match fs::symlink_metadata(&dot_git) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let link = fs::read_link(&dot_git).map_err(|error| format!("readlink: {error}"))?;
            let _ = fs::remove_file(&target);
            std::os::unix::fs::symlink(link, &target)
                .map_err(|error| format!("restore .git: {error}"))?;
        }
        Ok(_) => {
            fs::copy(&dot_git, &target).map_err(|error| format!("restore .git: {error}"))?;
        }
        Err(_) if fs::symlink_metadata(&target).is_ok() => {}
        Err(error) => return Err(format!("overlay .git is missing: {error}")),
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args([
            "-c",
            "checkout.workers=8",
            "-c",
            "checkout.thresholdForParallelism=100",
            "reset",
            "--hard",
            "-q",
        ])
        .output()
        .map_err(|error| format!("run git reset --hard: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "populate materialized checkout: {}",
            exit_reason(&output)
        ));
    }
    let _ = fs::remove_dir_all(overlay_dir(entry, project_path));
    Ok(())
}
