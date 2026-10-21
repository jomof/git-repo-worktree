//! Site hooks: executables the source client's owner places in
//! `.repo/repo-worktrees/hooks/`. They run with the tree root as working
//! directory and never change a command's outcome; results are recorded on the
//! tree and in the usage history.

use crate::discovery::discover_client;
use crate::error::AppResult;
use crate::model::{HookRecord, TreeMeta};
use crate::registry::registry_path;
use crate::util::now;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const POST_ADD: &str = "post-add";
pub const PRE_REMOVE: &str = "pre-remove";
pub const EVENTS: &[&str] = &[POST_ADD, PRE_REMOVE];
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

static DISABLED: AtomicBool = AtomicBool::new(false);

/// Disables hooks for this invocation (`--no-hooks`).
pub fn disable() {
    DISABLED.store(true, Ordering::SeqCst);
}

fn disabled() -> bool {
    DISABLED.load(Ordering::SeqCst)
        || std::env::var("GIT_REPO_WORKTREE_NO_HOOKS")
            .is_ok_and(|value| !value.is_empty() && value != "0")
}

pub fn hooks_dir(registry: &Path) -> PathBuf {
    registry.join("hooks")
}

fn timeout() -> Duration {
    std::env::var("GIT_REPO_WORKTREE_HOOK_TIMEOUT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT)
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Runs the `event` hook for `meta` if one is installed and hooks are enabled.
/// The hook's stdout is sent to stderr so `--json` output stays one document.
/// A hook that is installed but disabled is recorded as `skipped`, so a later
/// `repair` with hooks enabled still runs `post-add`.
pub fn run(event: &str, meta: &TreeMeta, quiet: bool) -> Option<HookRecord> {
    let path = hooks_dir(&meta.source().join(".repo/repo-worktrees")).join(event);
    if !path.exists() {
        return None;
    }
    let started = Instant::now();
    if disabled() {
        if !quiet {
            eprintln!("skipped {event} hook (hooks are disabled)");
        }
        return Some(HookRecord {
            status: "skipped".into(),
            exit_code: None,
            ms: 0,
            at: now(),
            reason: Some("hooks were disabled".into()),
        });
    }
    let finish = |status: &str, exit_code: Option<i32>, reason: Option<String>| {
        let record = HookRecord {
            status: status.into(),
            exit_code,
            ms: crate::metrics::elapsed_ms(started),
            at: now(),
            reason,
        };
        crate::metrics::push(
            "hooks",
            json!({"event": event, "status": record.status, "exit_code": exit_code, "ms": record.ms}),
        );
        if !quiet {
            match record.status.as_str() {
                "ok" => eprintln!("ran {event} hook ({} ms)", record.ms),
                _ => eprintln!(
                    "warning: {event} hook {}: {}",
                    record.status,
                    record.reason.as_deref().unwrap_or("")
                ),
            }
        }
        record
    };
    if !is_executable(&path) {
        return Some(finish(
            "failed",
            None,
            Some(format!(
                "{} is not executable; run chmod +x",
                path.display()
            )),
        ));
    }
    if !meta.root().is_dir() {
        return Some(finish(
            "failed",
            None,
            Some("tree directory is missing".into()),
        ));
    }
    let mut command = Command::new(&path);
    crate::util::with_caller_git_config(&mut command);
    let child = command
        .current_dir(meta.root())
        .env("GIT_REPO_WORKTREE_EVENT", event)
        .env("GIT_REPO_WORKTREE_TREE", meta.root())
        .env("GIT_REPO_WORKTREE_NAME", &meta.name)
        .env("GIT_REPO_WORKTREE_SOURCE", meta.source())
        .stdin(Stdio::null())
        .stdout(std::io::stderr())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => return Some(finish("failed", None, Some(format!("start: {error}")))),
    };
    let limit = timeout();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Some(finish("ok", Some(0), None)),
            Ok(Some(status)) => {
                return Some(finish(
                    "failed",
                    status.code(),
                    Some(match status.code() {
                        Some(code) => format!("exited with status {code}"),
                        None => "terminated by a signal".into(),
                    }),
                ))
            }
            Ok(None) if started.elapsed() >= limit => {
                kill_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                return Some(finish(
                    "timeout",
                    None,
                    Some(format!(
                        "stopped after {}s (GIT_REPO_WORKTREE_HOOK_TIMEOUT)",
                        limit.as_secs()
                    )),
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Some(finish("failed", None, Some(format!("wait: {error}")))),
        }
    }
}

/// Stops a timed-out hook and everything it started: the hook runs in its own
/// process group, and leftover children would otherwise keep the caller's
/// output pipe open.
fn kill_group(pid: u32) {
    let group = format!("-{pid}");
    let _ = Command::new("kill").args(["-TERM", "--", &group]).status();
    std::thread::sleep(Duration::from_millis(200));
    let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
}

/// Runs `post-add` unless it already succeeded for this tree, recording the
/// result on the tree. Returns whether metadata changed.
pub fn run_post_add(meta: &mut TreeMeta, quiet: bool) -> bool {
    if meta
        .hooks
        .get(POST_ADD)
        .is_some_and(|record| record.status == "ok")
    {
        return false;
    }
    match run(POST_ADD, meta, quiet) {
        Some(record) => {
            meta.hooks.insert(POST_ADD.into(), record);
            true
        }
        None => false,
    }
}

pub fn list(cwd: &Path, json_output: bool) -> AppResult<Value> {
    let client = discover_client(cwd, true)?;
    let directory = hooks_dir(&registry_path(&client.root));
    let hooks = EVENTS
        .iter()
        .map(|event| {
            let path = directory.join(event);
            json!({
                "event": event,
                "path": path,
                "installed": path.exists(),
                "executable": is_executable(&path),
            })
        })
        .collect::<Vec<_>>();
    if !json_output {
        for hook in &hooks {
            let state = match (hook["installed"].as_bool(), hook["executable"].as_bool()) {
                (Some(true), Some(true)) => "installed",
                (Some(true), _) => "installed but not executable",
                _ => "not installed",
            };
            println!(
                "{:<11} {state}  {}",
                hook["event"].as_str().unwrap_or(""),
                hook["path"].as_str().unwrap_or("")
            );
        }
    }
    Ok(json!({
        "directory": directory,
        "disabled": disabled(),
        "hooks": hooks,
    }))
}
