//! Removed trees waiting for background deletion.
//!
//! `remove` renames a tree into `.repo/repo-worktrees/.trash/` when it is on
//! the same filesystem as the source, or next to itself under a hidden
//! marker name with a `.path` note in `.trash/` when it is not. Background
//! maintenance deletes them, and finishes any deletion a crash interrupted.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
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
fn delete_command(path: &Path) -> Command {
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
pub fn empty(registry: &Path, mut run: impl FnMut(Command) -> bool) -> usize {
    let directory = registry.join(DIR_NAME);
    let Ok(entries) = fs::read_dir(&directory) else {
        return 0;
    };
    let mut deleted = 0;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == NOTE_EXTENSION)
        {
            let Ok(target) = fs::read(&path) else {
                continue;
            };
            let target = PathBuf::from(OsString::from_vec(target));
            let marked = target
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(MARKER));
            if marked && fs::symlink_metadata(&target).is_ok() && !run(delete_command(&target)) {
                continue;
            }
            let _ = fs::remove_file(&path);
            deleted += usize::from(marked);
        } else if path.is_dir() && run(delete_command(&path)) {
            deleted += 1;
        }
    }
    deleted
}

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
        let deleted = empty(&registry, |mut command| {
            command.status().is_ok_and(|status| status.success())
        });
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
        let deleted = empty(&registry, |mut command| {
            command.status().is_ok_and(|status| status.success())
        });
        assert_eq!(deleted, 1);
        assert!(unmarked.exists());
        assert!(!marked.exists());
        assert!(!pending(&registry));
    }
}
