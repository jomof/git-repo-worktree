//! Removed trees waiting for background deletion.
//!
//! `remove` renames a tree into `.repo/repo-worktrees/.trash/` when it is on
//! the same filesystem as the source, or next to itself under a hidden
//! marker name with a `.path` note in `.trash/` when it is not. A detached
//! `pool empty-trash` deletes them at normal priority, several at a time, and
//! finishes any deletion a crash interrupted.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DIR_NAME: &str = ".trash";
const MARKER: &str = ".git-repo-worktree-trash-";
const NOTE_EXTENSION: &str = "path";
/// `EXDEV` on Linux and macOS: the rename crossed filesystems.
const CROSS_DEVICE: i32 = 18;

fn unique_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{}", std::process::id())
}

/// Moves `root` aside for background deletion and returns where it went.
pub fn stash(registry: &Path, root: &Path) -> io::Result<PathBuf> {
    let directory = registry.join(DIR_NAME);
    fs::create_dir_all(&directory)?;
    let unique = unique_name();
    let inside = directory.join(&unique);
    match fs::rename(root, &inside) {
        Ok(()) => return Ok(inside),
        Err(error) if error.raw_os_error() == Some(CROSS_DEVICE) => {}
        Err(error) => return Err(error),
    }
    let beside = root.with_file_name(format!("{MARKER}{unique}"));
    fs::write(
        directory.join(format!("{unique}.{NOTE_EXTENSION}")),
        beside.as_os_str().as_bytes(),
    )?;
    fs::rename(root, &beside)?;
    Ok(beside)
}

/// Whether any stashed tree is waiting for deletion.
pub fn pending(registry: &Path) -> bool {
    fs::read_dir(registry.join(DIR_NAME))
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// The deletion command for `path`, which never crosses into another
/// filesystem, such as an overlay that failed to unmount.
pub fn delete_command(path: &Path) -> Command {
    let mut command = Command::new("rm");
    if cfg!(target_os = "linux") {
        command.args(["-rf", "--one-file-system"]);
    } else {
        command.args(["-rfx"]);
    }
    command
        .arg("--")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Deletes every stashed tree with `run`, which executes a deletion command
/// and reports whether it succeeded. Returns how many were deleted.
/// Trash entries still to delete: each stashed tree, and for trees stashed
/// beside themselves, the note recording where.
fn entries(registry: &Path) -> Vec<(Option<PathBuf>, Option<PathBuf>)> {
    let Ok(entries) = fs::read_dir(registry.join(DIR_NAME)) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == NOTE_EXTENSION)
            {
                let target = PathBuf::from(OsString::from_vec(fs::read(&path).ok()?));
                let marked = target
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(MARKER));
                let present = marked && fs::symlink_metadata(&target).is_ok();
                Some((present.then_some(target), Some(path)))
            } else if path.is_dir() {
                Some((Some(path), None))
            } else {
                None
            }
        })
        .collect()
}

/// Deletes every stashed tree, `workers` at a time, until the trash stays
/// empty or a pass deletes nothing. Returns the number of trees deleted.
pub fn empty_parallel(registry: &Path, workers: usize) -> usize {
    let mut deleted = 0;
    loop {
        let pending = entries(registry);
        if pending.is_empty() {
            return deleted;
        }
        let mut progress = false;
        for batch in pending.chunks(workers.max(1)) {
            let children = batch
                .iter()
                .map(|(target, note)| {
                    let child = target.as_ref().map(|target| delete_command(target).spawn());
                    (child, target, note)
                })
                .collect::<Vec<_>>();
            for (child, target, note) in children {
                let finished = match child {
                    Some(Ok(mut child)) => child.wait().is_ok_and(|status| status.success()),
                    Some(Err(_)) => false,
                    None => true,
                };
                if finished {
                    if let Some(note) = note {
                        let _ = fs::remove_file(note);
                    }
                    if target.is_some() {
                        deleted += 1;
                    }
                    progress = true;
                }
            }
        }
        if !progress {
            return deleted;
        }
    }
}

/// Starts `pool empty-trash` detached at reduced CPU priority but normal disk
/// priority, so the space comes back promptly. A deleter that is already
/// running picks up the new entry instead.
pub fn spawn_deletion(source: &Path) -> bool {
    let registry = crate::registry::registry_path(source);
    let _ = fs::write(registry.join(AGAIN_FILE), "");
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let mut command = match find_nice() {
        Some(nice) => {
            let mut command = Command::new(nice);
            command.args(["-n", "10"]).arg(executable);
            command
        }
        None => Command::new(executable),
    };
    command
        .arg("-C")
        .arg(source)
        .args(["-q", "pool", "empty-trash"])
        .current_dir(source)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .is_ok()
}

fn find_nice() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join("nice"))
        .find(|candidate| candidate.is_file())
}

/// Lock file a running deleter holds, and the marker asking it to look again.
pub const LOCK_FILE: &str = ".trash.lock";
pub const AGAIN_FILE: &str = ".trash.again";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stashed_trees_are_moved_into_the_registry_and_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let registry = temp.path().join("registry");
        let tree = temp.path().join("tree");
        fs::create_dir_all(tree.join("a/b")).unwrap();
        fs::write(tree.join("a/b/file"), "x").unwrap();
        let stashed = stash(&registry, &tree).unwrap();
        assert!(!tree.exists());
        assert!(stashed.starts_with(registry.join(DIR_NAME)));
        assert!(pending(&registry));
        let deleted = empty_parallel(&registry, 2);
        assert_eq!(deleted, 1);
        assert!(!pending(&registry));
    }

    #[test]
    fn notes_only_delete_marked_directories() {
        let temp = tempfile::tempdir().unwrap();
        let registry = temp.path().join("registry");
        fs::create_dir_all(registry.join(DIR_NAME)).unwrap();
        let unmarked = temp.path().join("precious");
        fs::create_dir(&unmarked).unwrap();
        fs::write(
            registry.join(DIR_NAME).join("1.path"),
            unmarked.as_os_str().as_bytes(),
        )
        .unwrap();
        let marked = temp.path().join(format!("{MARKER}2"));
        fs::create_dir(&marked).unwrap();
        fs::write(
            registry.join(DIR_NAME).join("2.path"),
            marked.as_os_str().as_bytes(),
        )
        .unwrap();
        let deleted = empty_parallel(&registry, 2);
        assert_eq!(deleted, 1);
        assert!(unmarked.exists());
        assert!(!marked.exists());
        assert!(!pending(&registry));
    }
}
