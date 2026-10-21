use crate::error::{AppError, AppResult};
use crate::model::{Breadcrumb, JournalRecord, TreeMeta, FORMAT_VERSION};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Output;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub fn now() -> String {
    OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .expect("zero nanoseconds is valid")
        .format(&Rfc3339)
        .expect("RFC3339 formatting is infallible")
}

pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(32)
}

pub fn truncated_stderr(output: &Output) -> String {
    let bytes = &output.stderr[..output.stderr.len().min(500)];
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// Sets a Git config value unless it already has it. Trees share their
/// projects' and the manifest repository's config files, so operations on
/// different trees can write the same file at once; Git then fails to take the
/// file's lock, and the write is retried for a few seconds.
pub fn set_git_config(scope: &[std::ffi::OsString], key: &str, value: &str) -> Result<(), String> {
    let current = std::process::Command::new("git")
        .args(scope)
        .args(["config", "--get", key])
        .output()
        .map_err(|error| format!("run git config: {error}"))?;
    if current.status.success() && String::from_utf8_lossy(&current.stdout).trim() == value {
        return Ok(());
    }
    const ATTEMPTS: u64 = 60;
    for attempt in 1..=ATTEMPTS {
        let output = std::process::Command::new("git")
            .args(scope)
            .args(["config", key, value])
            .output()
            .map_err(|error| format!("run git config: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        let reason = exit_reason(&output);
        if attempt == ATTEMPTS || !reason.contains("could not lock config file") {
            return Err(reason);
        }
        std::thread::sleep(std::time::Duration::from_millis(10 * attempt.min(10)));
    }
    unreachable!("the last attempt returns")
}

/// Marks every descriptor above stderr close-on-exec, so the programs this
/// process starts (long-lived fuse-overlayfs daemons and background
/// maintenance included) do not inherit descriptors the caller leaked into it,
/// such as its sockets, pipes, and log files. Call once at startup.
pub fn close_inherited_descriptors_on_exec() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: close_range with CLOSE_RANGE_CLOEXEC only sets flags.
        let marked = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        };
        if marked == 0 {
            return;
        }
    }
    let descriptors = fs::read_dir("/dev/fd")
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
                .filter(|descriptor| *descriptor > 2)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for descriptor in descriptors {
        // SAFETY: fcntl on a descriptor that may already be closed fails with
        // EBADF and changes nothing.
        unsafe {
            let flags = libc::fcntl(descriptor, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

static CALLER_GIT_CONFIG_COUNT: std::sync::OnceLock<Option<std::ffi::OsString>> =
    std::sync::OnceLock::new();

fn config_count(value: Option<&std::ffi::OsString>) -> usize {
    value
        .and_then(|value| value.to_str())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// Makes every Git command this process starts ignore `core.fsmonitor`. A
/// daemon Git starts for a worktree outlives the command and stays bound to
/// the worktree's administrative directory, which the tool moves between the
/// pool and trees; a later checkout there can block on it. The setting is
/// appended to any `GIT_CONFIG_COUNT` configuration the caller passed. Call
/// once, before starting threads.
pub fn disable_fsmonitor_for_children() {
    let original = std::env::var_os("GIT_CONFIG_COUNT");
    let count = config_count(original.as_ref());
    std::env::set_var(format!("GIT_CONFIG_KEY_{count}"), "core.fsmonitor");
    std::env::set_var(format!("GIT_CONFIG_VALUE_{count}"), "false");
    std::env::set_var("GIT_CONFIG_COUNT", (count + 1).to_string());
    let _ = CALLER_GIT_CONFIG_COUNT.set(original);
}

/// Gives `command` the caller's Git configuration environment back, for
/// programs the user owns such as hooks.
pub fn with_caller_git_config(command: &mut std::process::Command) {
    let Some(original) = CALLER_GIT_CONFIG_COUNT.get() else {
        return;
    };
    let count = config_count(original.as_ref());
    command
        .env_remove(format!("GIT_CONFIG_KEY_{count}"))
        .env_remove(format!("GIT_CONFIG_VALUE_{count}"));
    match original {
        Some(value) => command.env("GIT_CONFIG_COUNT", value),
        None => command.env_remove("GIT_CONFIG_COUNT"),
    };
}

/// Stops the fsmonitor daemon serving the worktree at `checkout`, if one has
/// left its socket in the worktree's administrative directory. Call before
/// moving or deleting a worktree. Returns whether a daemon was asked to stop.
pub fn stop_fsmonitor(checkout: &Path) -> bool {
    let Some(git_dir) = crate::inspect::checkout_git_dir(checkout) else {
        return false;
    };
    if fs::symlink_metadata(git_dir.join("fsmonitor--daemon.ipc")).is_err() {
        return false;
    }
    let Ok(mut child) = std::process::Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["fsmonitor--daemon", "stop"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    true
}

/// Whether `git worktree add` failed only because another `worktree add` in
/// the same repository was half-way through registering its worktree: Git
/// reads every worktree's administrative files before adding one.
pub fn concurrent_worktree_registration(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    text.contains("failed to read") && text.contains("/worktrees/")
}

/// Runs the `git worktree add` built by `build(attempt)`, retrying while it
/// fails because of a concurrent registration. Git fails that way before
/// adding anything, except that `-b` may already have created the branch;
/// `build` sees the attempt number so a retry can check that branch out.
pub fn worktree_add(
    mut build: impl FnMut(u32) -> std::process::Command,
) -> std::io::Result<Output> {
    const ATTEMPTS: u32 = 20;
    for attempt in 0.. {
        let output = build(attempt).output()?;
        if output.status.success()
            || attempt + 1 == ATTEMPTS
            || !concurrent_worktree_registration(&output.stderr)
        {
            return Ok(output);
        }
        std::thread::sleep(std::time::Duration::from_millis(
            10 * u64::from(attempt.min(9) + 1),
        ));
    }
    unreachable!("the last attempt returns")
}

/// Whether `branch` exists in the repository `git_args` selects and points at
/// `sha`.
pub fn branch_points_at(git_args: &[std::ffi::OsString], branch: &str, sha: &str) -> bool {
    std::process::Command::new("git")
        .args(git_args)
        .args(["rev-parse", "-q", "--verify"])
        .arg(format!("refs/heads/{branch}^{{commit}}"))
        .output()
        .is_ok_and(|output| {
            output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == sha
        })
}

pub fn exit_reason(output: &Output) -> String {
    let code = output.status.code().unwrap_or(1);
    let stderr = truncated_stderr(output);
    let mut reason = format!("exit {code}: {stderr}");
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("promisor") || lower.contains("could not fetch") {
        reason.push_str(
            " (source uses partial clone; network/auth may be required — retry with 'add --continue')",
        );
    }
    reason
}

pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> AppResult<()> {
    let parent = path.parent().ok_or_else(|| {
        AppError::operational("E_IO", format!("path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| AppError::io(format!("create {}", parent.display()), error))?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("git-repo-worktree"),
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true).mode(mode);
    let mut file = options
        .open(&temp)
        .map_err(|error| AppError::io(format!("create {}", temp.display()), error))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| AppError::io(format!("write {}", temp.display()), error))?;
    fs::set_permissions(&temp, fs::Permissions::from_mode(mode))
        .map_err(|error| AppError::io(format!("chmod {}", temp.display()), error))?;
    fs::rename(&temp, path)
        .map_err(|error| AppError::io(format!("replace {}", path.display()), error))
}

pub fn write_meta(meta: &TreeMeta) -> AppResult<()> {
    let path = meta.registry_entry().join("meta.json");
    let bytes = serde_json::to_vec_pretty(meta)
        .map_err(|error| AppError::io("serialize meta.json", error))?;
    write_atomic(&path, &bytes, 0o644)
}

pub fn read_meta(path: &Path) -> AppResult<TreeMeta> {
    let bytes =
        fs::read(path).map_err(|error| AppError::io(format!("read {}", path.display()), error))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::io(format!("parse {}", path.display()), error))?;
    if value
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        > FORMAT_VERSION as u64
    {
        return Err(AppError::operational(
            "E_FORMAT_TOO_NEW",
            format!(
                "{} uses a newer metadata format; upgrade git-repo-worktree",
                path.display()
            ),
        ));
    }
    serde_json::from_value(value)
        .map_err(|error| AppError::io(format!("parse {}", path.display()), error))
}

pub fn write_breadcrumb(meta: &TreeMeta) -> AppResult<()> {
    let breadcrumb = Breadcrumb {
        format_version: FORMAT_VERSION,
        name: meta.name.clone(),
        source_root: meta.source_root.clone(),
    };
    let bytes = serde_json::to_vec(&breadcrumb)
        .map_err(|error| AppError::io("serialize breadcrumb", error))?;
    write_atomic(&meta.root().join(".repo-worktree"), &bytes, 0o644)
}

pub fn append_journal(
    entry: &Path,
    step: &str,
    status: &str,
    project: Option<&str>,
    dest: Option<&str>,
    reason: Option<&str>,
) -> AppResult<()> {
    let record = journal_record(step, status, project, dest, reason);
    append_journal_batch(entry, &[record])
}

pub fn journal_record(
    step: &str,
    status: &str,
    project: Option<&str>,
    dest: Option<&str>,
    reason: Option<&str>,
) -> JournalRecord {
    JournalRecord {
        ts: now(),
        step: step.to_string(),
        status: status.to_string(),
        project: project.map(str::to_owned),
        dest: dest.map(str::to_owned),
        reason: reason.map(str::to_owned),
    }
}

pub fn append_journal_batch(entry: &Path, records: &[JournalRecord]) -> AppResult<()> {
    if records.is_empty() {
        return Ok(());
    }
    let path = entry.join("journal.jsonl");
    let mut file = open_journal(entry)?;
    file.write_all(&encode_journal(records)?)
        .and_then(|_| file.sync_all())
        .map_err(|error| AppError::io(format!("append journal batch to {}", path.display()), error))
}

pub fn open_journal(entry: &Path) -> AppResult<fs::File> {
    let path = entry.join("journal.jsonl");
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(&path)
        .map_err(|error| AppError::io(format!("open {}", path.display()), error))
}

pub fn encode_journal(records: &[JournalRecord]) -> AppResult<Vec<u8>> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record)
            .map_err(|error| AppError::io("serialize journal record", error))?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

pub fn journal_has_done(entry: &Path, step: &str) -> AppResult<bool> {
    journal_has(entry, step, "done", None)
}

pub fn journal_has_project_status(
    entry: &Path,
    step: &str,
    status: &str,
    project: &str,
) -> AppResult<bool> {
    journal_has(entry, step, status, Some(project))
}

fn journal_has(entry: &Path, step: &str, status: &str, project: Option<&str>) -> AppResult<bool> {
    let path = entry.join("journal.jsonl");
    let mut contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(AppError::io(format!("read {}", path.display()), error)),
    };
    if !contents.is_empty() && !contents.ends_with('\n') {
        let valid_len = contents.rfind('\n').map_or(0, |index| index + 1);
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|error| AppError::io(format!("open {}", path.display()), error))?;
        file.set_len(valid_len as u64)
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                AppError::io(
                    format!("truncate incomplete journal record in {}", path.display()),
                    error,
                )
            })?;
        contents.truncate(valid_len);
    }
    for line in contents.lines() {
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| AppError::io(format!("parse {}", path.display()), error))?;
        if value.get("step").and_then(serde_json::Value::as_str) == Some(step)
            && value.get("status").and_then(serde_json::Value::as_str) == Some(status)
            && project.map_or(true, |project| {
                value.get("project").and_then(serde_json::Value::as_str) == Some(project)
            })
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn absolutize(path: &Path, cwd: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    normalize(&joined)
}

pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(value) => out.push(value),
        }
    }
    out
}

pub fn canonicalize_missing(path: &Path) -> AppResult<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            AppError::operational("E_IO", format!("cannot resolve path {}", path.display()))
        })?;
        suffix.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| AppError::operational("E_IO", "path has no existing ancestor"))?
            .to_path_buf();
    }
    let mut resolved = fs::canonicalize(&existing)
        .map_err(|error| AppError::io(format!("resolve {}", existing.display()), error))?;
    for name in suffix.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

pub fn is_path_prefix(parent: &Path, child: &Path) -> bool {
    child.starts_with(parent)
}

pub fn sanitize_name(value: &OsStr) -> String {
    value
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub fn path_is_safe_relative(path: &Path) -> bool {
    !path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

pub fn ensure_safe_destination(tree_root: &Path, relative: &Path) -> AppResult<PathBuf> {
    if !path_is_safe_relative(relative) {
        return Err(AppError::operational(
            "E_UNSAFE_PATH",
            "unsafe relative path",
        ));
    }
    let canonical_root = canonicalize_missing(tree_root)?;
    let dest = tree_root.join(relative);
    let parent = dest.parent().unwrap_or(tree_root);

    let mut cursor = tree_root.to_path_buf();
    if let Ok(rest) = parent.strip_prefix(tree_root) {
        for component in rest.components() {
            cursor.push(component.as_os_str());
            if cursor.exists() {
                let metadata = fs::symlink_metadata(&cursor).map_err(|error| {
                    AppError::io(format!("inspect {}", cursor.display()), error)
                })?;
                if metadata.file_type().is_symlink() {
                    return Err(AppError::operational(
                        "E_UNSAFE_PATH",
                        format!("symlink in destination path: {}", cursor.display()),
                    ));
                }
            } else {
                if let Err(error) = fs::create_dir(&cursor) {
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(AppError::io(format!("create {}", cursor.display()), error));
                    }
                    let metadata = fs::symlink_metadata(&cursor).map_err(|error| {
                        AppError::io(format!("inspect {}", cursor.display()), error)
                    })?;
                    if metadata.file_type().is_symlink() {
                        return Err(AppError::operational(
                            "E_UNSAFE_PATH",
                            format!("symlink in destination path: {}", cursor.display()),
                        ));
                    }
                }
                fs::set_permissions(&cursor, fs::Permissions::from_mode(0o755))
                    .map_err(|error| AppError::io(format!("chmod {}", cursor.display()), error))?;
            }
        }
    }
    let resolved_parent = fs::canonicalize(parent)
        .map_err(|error| AppError::io(format!("resolve {}", parent.display()), error))?;
    if !resolved_parent.starts_with(&canonical_root) {
        return Err(AppError::operational(
            "E_UNSAFE_PATH",
            format!("destination escapes tree: {}", dest.display()),
        ));
    }
    Ok(dest)
}

pub fn relative_path(from_dir: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from_dir.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut result = PathBuf::new();
    for _ in common..from.len() {
        result.push("..");
    }
    for component in &to[common..] {
        result.push(component.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

pub fn copy_permissions(source: &Path, destination: &Path) -> AppResult<()> {
    let mode = fs::metadata(source)
        .map_err(|error| AppError::io(format!("stat {}", source.display()), error))?
        .permissions()
        .mode();
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))
        .map_err(|error| AppError::io(format!("chmod {}", destination.display()), error))
}

pub fn dedupe_paths<'a>(paths: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut seen = HashSet::new();
    paths
        .filter(|value| seen.insert((*value).clone()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn sanitizes_names() {
        assert_eq!(
            sanitize_name(OsStr::new("hello world/ok")),
            "hello-world-ok"
        );
        assert_eq!(sanitize_name(OsStr::new("a_b.c-1")), "a_b.c-1");
    }

    #[test]
    fn computes_relative_paths() {
        assert_eq!(
            relative_path(Path::new("/tree/out"), Path::new("/tree/p/src")),
            PathBuf::from("../p/src")
        );
        assert_eq!(
            relative_path(Path::new("/tree"), Path::new("/tree/a")),
            PathBuf::from("a")
        );
    }

    #[test]
    fn rejects_parent_and_absolute_paths() {
        assert!(!path_is_safe_relative(Path::new("../escape")));
        assert!(!path_is_safe_relative(Path::new("/escape")));
        assert!(path_is_safe_relative(Path::new("safe/file")));
    }

    #[test]
    fn rejects_symlink_intermediate_destination() {
        let tree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), tree.path().join("escape")).unwrap();
        let error = ensure_safe_destination(tree.path(), Path::new("escape/file")).unwrap_err();
        assert_eq!(error.code, "E_UNSAFE_PATH");
        assert!(!outside.path().join("file").exists());
    }

    #[test]
    fn concurrent_worktree_registrations_are_recognized() {
        assert!(concurrent_worktree_registration(
            b"Preparing worktree (detached HEAD 6385704)\nfatal: failed to read .git/worktrees/-filling-1792441865851678915-15776-0/commondir: Success\n"
        ));
        assert!(!concurrent_worktree_registration(
            b"fatal: a branch named 'topic' already exists\n"
        ));
        assert!(!concurrent_worktree_registration(
            b"fatal: '/tmp/x' already exists\n"
        ));
    }

    #[test]
    fn git_config_writes_wait_out_a_held_config_lock() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&repository)
            .status()
            .unwrap()
            .success());
        let lock = repository.join(".git/config.lock");
        fs::write(&lock, "").unwrap();
        let releaser = {
            let lock = lock.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                fs::remove_file(lock).unwrap();
            })
        };
        let scope = [
            std::ffi::OsString::from("-C"),
            repository.clone().into_os_string(),
        ];
        set_git_config(&scope, "branch.topic.merge", "refs/heads/main").unwrap();
        releaser.join().unwrap();
        let value = std::process::Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(["config", "branch.topic.merge"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&value.stdout).trim(),
            "refs/heads/main"
        );
        set_git_config(&scope, "branch.topic.merge", "refs/heads/main").unwrap();
    }

    #[test]
    fn appends_and_fsyncs_journal_records_as_one_batch() {
        let entry = tempfile::tempdir().unwrap();
        let records = vec![
            journal_record("project-add", "done", Some("a"), None, None),
            journal_record(
                "project-add",
                "failed",
                Some("b"),
                None,
                Some("checkout failed"),
            ),
        ];
        append_journal_batch(entry.path(), &records).unwrap();
        let contents = fs::read_to_string(entry.path().join("journal.jsonl")).unwrap();
        assert_eq!(contents.lines().count(), 2);
        assert!(contents.ends_with('\n'));
    }

    #[test]
    fn ignores_only_an_incomplete_trailing_journal_record() {
        let entry = tempfile::tempdir().unwrap();
        append_journal(entry.path(), "pin", "done", None, None, None).unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(entry.path().join("journal.jsonl"))
            .unwrap();
        file.write_all(br#"{"step":"project-add""#).unwrap();
        assert!(journal_has_done(entry.path(), "pin").unwrap());
        let path = entry.path().join("journal.jsonl");
        assert!(fs::read(&path).unwrap().ends_with(b"\n"));
        append_journal(entry.path(), "project-add", "done", Some("a"), None, None).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap().lines().count(), 2);
    }
}
